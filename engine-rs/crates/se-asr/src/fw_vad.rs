//! faster-whisper's Silero VAD (`faster_whisper/vad.py`, `silero_vad_v6.onnx`; spec transcribe.md §4.3).
//! Not the same model or algorithm as onnx-asr's VAD ([`crate::onnx_asr::vad`]).

use std::sync::{Mutex, OnceLock};

use ort::session::Session;
use ort::value::Tensor;
use se_core::Result;

use crate::ort_runtime::{self, ort_err};
use crate::whisper::WhisperVadOptions;

/// The bundled model (MIT, from faster-whisper 1.2.1).
pub static SILERO_V6_ONNX: &[u8] = include_bytes!("../assets/silero_vad_v6.onnx");

const WINDOW: usize = 512;
const CONTEXT: usize = 64;
const BATCH_ROWS: usize = 10_000;

/// A speech region in samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Speech {
    pub start: i64,
    pub end: i64,
}

/// Computes per-window speech probabilities; a seam so the segment logic can be tested without a model.
pub trait SpeechProbModel {
    /// `rows`: `n x 576` model input (context ++ window); returns `n` probabilities.
    fn run(&mut self, rows: &[f32], n: usize, h: &mut Vec<f32>, c: &mut Vec<f32>) -> Result<Vec<f32>>;
}

struct OrtSilero(Session);

impl SpeechProbModel for OrtSilero {
    fn run(&mut self, rows: &[f32], n: usize, h: &mut Vec<f32>, c: &mut Vec<f32>) -> Result<Vec<f32>> {
        let input = Tensor::from_array(([n, WINDOW + CONTEXT], rows.to_vec())).map_err(ort_err)?;
        let ht = Tensor::from_array(([1usize, 1, 128], h.clone())).map_err(ort_err)?;
        let ct = Tensor::from_array(([1usize, 1, 128], c.clone())).map_err(ort_err)?;
        let outputs = self.0.run(ort::inputs!["input" => input, "h" => ht, "c" => ct]).map_err(ort_err)?;
        let (_, probs) = outputs[0].try_extract_tensor::<f32>().map_err(ort_err)?;
        let (_, hn) = outputs[1].try_extract_tensor::<f32>().map_err(ort_err)?;
        let (_, cn) = outputs[2].try_extract_tensor::<f32>().map_err(ort_err)?;
        *h = hn.to_vec();
        *c = cn.to_vec();
        Ok(probs.to_vec())
    }
}

static MODEL: OnceLock<std::result::Result<Mutex<OrtSilero>, String>> = OnceLock::new();

fn with_model<T>(f: impl FnOnce(&mut dyn SpeechProbModel) -> Result<T>) -> Result<T> {
    let model = MODEL.get_or_init(|| {
        let build = || -> Result<OrtSilero> {
            let session = ort_runtime::builder()?
                .with_inter_threads(1)
                .map_err(ort_err)?
                .with_intra_threads(1)
                .map_err(ort_err)?
                .with_log_level(ort::logging::LogLevel::Fatal)
                .map_err(ort_err)?
                .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
                .map_err(ort_err)?
                .commit_from_memory(SILERO_V6_ONNX)
                .map_err(ort_err)?;
            Ok(OrtSilero(session))
        };
        build().map(Mutex::new).map_err(|e| e.message)
    });
    match model {
        Ok(m) => {
            let mut guard = m.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut *guard)
        }
        Err(e) => Err(se_core::EngineError::runtime(e.clone())),
    }
}

/// `SileroVADModel.__call__(pad(audio))` including the view-write quirk that zeroes the last row's final 64
/// samples.
pub fn speech_probs(model: &mut dyn SpeechProbModel, audio: &[f32]) -> Result<Vec<f32>> {
    let mut padded = audio.to_vec();
    padded.resize(audio.len() + (WINDOW - audio.len() % WINDOW), 0.0);
    let n = padded.len() / WINDOW;
    // context = rows[..., -64:] is a view: `context[-1] = 0` zeroes the tail of the last row itself.
    let last = (n - 1) * WINDOW;
    for v in &mut padded[last + WINDOW - CONTEXT..last + WINDOW] {
        *v = 0.0;
    }
    let mut batched = Vec::with_capacity(n * (WINDOW + CONTEXT));
    for i in 0..n {
        if i == 0 {
            batched.extend(std::iter::repeat_n(0f32, CONTEXT));
        } else {
            batched.extend_from_slice(&padded[i * WINDOW - CONTEXT..i * WINDOW]);
        }
        batched.extend_from_slice(&padded[i * WINDOW..(i + 1) * WINDOW]);
    }
    let mut h = vec![0f32; 128];
    let mut c = vec![0f32; 128];
    let mut probs = Vec::with_capacity(n);
    let row = WINDOW + CONTEXT;
    let mut b = 0;
    while b < n {
        let rows = BATCH_ROWS.min(n - b);
        probs.extend(model.run(&batched[b * row..(b + rows) * row], rows, &mut h, &mut c)?);
        b += rows;
    }
    Ok(probs)
}

/// `get_speech_timestamps` segment search and padding on precomputed probabilities.
pub fn timestamps_from_probs(probs: &[f32], audio_len: usize, opts: &WhisperVadOptions) -> Vec<Speech> {
    let sr = 16000.0f64;
    let w = WINDOW as i64;
    let threshold = opts.threshold;
    let neg_threshold = opts.neg_threshold;
    let min_speech_samples = sr * opts.min_speech_duration_ms as f64 / 1000.0;
    let speech_pad_samples = sr * opts.speech_pad_ms as f64 / 1000.0;
    let max_speech_samples = sr * opts.max_speech_duration_s as f64 - WINDOW as f64 - 2.0 * speech_pad_samples;
    let min_silence_samples = sr * opts.min_silence_duration_ms as f64 / 1000.0;
    let min_silence_at_max = sr * 98.0 / 1000.0;
    let audio_len = audio_len as i64;

    let mut triggered = false;
    let mut speeches: Vec<Speech> = Vec::new();
    let mut cur: Option<i64> = None; // current_speech["start"]
    let mut temp_end: i64 = 0;
    let mut prev_end: i64 = 0;
    let mut next_start: i64 = 0;

    for (i, &p) in probs.iter().enumerate() {
        let wi = w * i as i64;
        if p >= threshold && temp_end != 0 {
            temp_end = 0;
            if next_start < prev_end {
                next_start = wi;
            }
        }
        if p >= threshold && !triggered {
            triggered = true;
            cur = Some(wi);
            continue;
        }
        if triggered && (wi - cur.unwrap_or(0)) as f64 > max_speech_samples {
            if prev_end != 0 {
                speeches.push(Speech { start: cur.unwrap_or(0), end: prev_end });
                cur = None;
                if next_start < prev_end {
                    triggered = false;
                } else {
                    cur = Some(next_start);
                }
                prev_end = 0;
                next_start = 0;
                temp_end = 0;
            } else {
                speeches.push(Speech { start: cur.unwrap_or(0), end: wi });
                cur = None;
                prev_end = 0;
                next_start = 0;
                temp_end = 0;
                triggered = false;
                continue;
            }
        }
        if p < neg_threshold && triggered {
            if temp_end == 0 {
                temp_end = wi;
            }
            if (wi - temp_end) as f64 > min_silence_at_max {
                prev_end = temp_end;
            }
            if ((wi - temp_end) as f64) < min_silence_samples {
                continue;
            }
            let start = cur.unwrap_or(0);
            if ((temp_end - start) as f64) > min_speech_samples {
                speeches.push(Speech { start, end: temp_end });
            }
            cur = None;
            prev_end = 0;
            next_start = 0;
            temp_end = 0;
            triggered = false;
            continue;
        }
    }
    if let Some(start) = cur {
        if ((audio_len - start) as f64) > min_speech_samples {
            speeches.push(Speech { start, end: audio_len });
        }
    }

    let last = speeches.len().saturating_sub(1);
    for i in 0..speeches.len() {
        if i == 0 {
            speeches[0].start = (0f64.max(speeches[0].start as f64 - speech_pad_samples)) as i64;
        }
        if i != last {
            let silence = speeches[i + 1].start - speeches[i].end;
            if (silence as f64) < 2.0 * speech_pad_samples {
                speeches[i].end += silence.div_euclid(2);
                speeches[i + 1].start = 0.max(speeches[i + 1].start - silence.div_euclid(2));
            } else {
                speeches[i].end = (audio_len as f64).min(speeches[i].end as f64 + speech_pad_samples) as i64;
                speeches[i + 1].start = (0f64.max(speeches[i + 1].start as f64 - speech_pad_samples)) as i64;
            }
        } else {
            speeches[i].end = (audio_len as f64).min(speeches[i].end as f64 + speech_pad_samples) as i64;
        }
    }
    speeches
}

/// `get_speech_timestamps(audio, VadOptions(**opts))` with the bundled model.
pub fn get_speech_timestamps(audio: &[f32], opts: &WhisperVadOptions) -> Result<Vec<Speech>> {
    let probs = with_model(|m| speech_probs(m, audio))?;
    Ok(timestamps_from_probs(&probs, audio.len(), opts))
}

/// `collect_chunks` with `max_duration=inf`: the concatenation of all speech regions.
pub fn collect_chunks(audio: &[f32], speeches: &[Speech]) -> Vec<f32> {
    let mut out = Vec::new();
    for s in speeches {
        let a = (s.start.max(0) as usize).min(audio.len());
        let b = (s.end.max(0) as usize).min(audio.len());
        if b > a {
            out.extend_from_slice(&audio[a..b]);
        }
    }
    out
}

/// A float that remembers whether Python would hold it as a `numpy.float64` (numpy rounding rules) or a
/// Python float (correctly rounded `round`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PyF {
    pub v: f64,
    pub np: bool,
}

impl PyF {
    pub fn py(v: f64) -> Self {
        Self { v, np: false }
    }

    pub fn np(v: f64) -> Self {
        Self { v, np: true }
    }

    /// Python `round(x, n)` on this value.
    pub fn round(self, n: u32) -> PyF {
        if self.np { PyF::np(np_round(self.v, n)) } else { PyF::py(se_core::pyfmt::round_ndigits(self.v, n)) }
    }
}

/// `np.round(x, n)`: `rint(x * 10**n) / 10**n`.
pub fn np_round(x: f64, n: u32) -> f64 {
    let f = 10f64.powi(n as i32);
    (x * f).round_ties_even() / f
}

/// `SpeechTimestampsMap(speeches, 16000, time_precision=2)`.
pub struct SpeechTimestampsMap {
    chunk_end_sample: Vec<i64>,
    total_silence_before: Vec<f64>,
}

impl SpeechTimestampsMap {
    pub fn new(speeches: &[Speech]) -> Self {
        let mut prev_end = 0i64;
        let mut silent = 0i64;
        let mut chunk_end_sample = Vec::new();
        let mut total_silence_before = Vec::new();
        for c in speeches {
            silent += c.start - prev_end;
            prev_end = c.end;
            chunk_end_sample.push(c.end - silent);
            total_silence_before.push(silent as f64 / 16000.0);
        }
        Self { chunk_end_sample, total_silence_before }
    }

    pub fn get_chunk_index(&self, time: f64, is_end: bool) -> usize {
        let sample = (time * 16000.0) as i64;
        if is_end {
            if let Some(i) = self.chunk_end_sample.iter().position(|&s| s == sample) {
                return i;
            }
        }
        let bisect = self.chunk_end_sample.partition_point(|&s| s <= sample);
        bisect.min(self.chunk_end_sample.len() - 1)
    }

    pub fn get_original_time(&self, time: PyF, chunk_index: Option<usize>, is_end: bool) -> PyF {
        let idx = chunk_index.unwrap_or_else(|| self.get_chunk_index(time.v, is_end));
        PyF { v: self.total_silence_before[idx] + time.v, np: time.np }.round(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whisper::WHISPER_VAD_OPTIONS;

    struct Recorder(Vec<Vec<f32>>);
    impl SpeechProbModel for Recorder {
        fn run(&mut self, rows: &[f32], n: usize, _h: &mut Vec<f32>, _c: &mut Vec<f32>) -> Result<Vec<f32>> {
            self.0.push(rows.to_vec());
            Ok(vec![0.0; n])
        }
    }

    #[test]
    fn model_input_zeroes_the_last_rows_tail() {
        let audio: Vec<f32> = (0..600).map(|i| i as f32).collect();
        let mut rec = Recorder(Vec::new());
        let probs = speech_probs(&mut rec, &audio).unwrap();
        assert_eq!(probs.len(), 2);
        let rows = &rec.0[0];
        assert_eq!(rows.len(), 2 * 576);
        assert!(rows[..64].iter().all(|&v| v == 0.0));
        assert_eq!(rows[64], 0.0);
        assert_eq!(rows[64 + 511], 511.0);
        // Row 1: context = last 64 of row 0, then row 1 (88 real samples) with its own tail zeroed.
        assert_eq!(rows[576], 448.0);
        assert_eq!(rows[576 + 64], 512.0);
        assert_eq!(rows[576 + 64 + 87], 599.0);
        assert!(rows[576 + 64 + 448..].iter().all(|&v| v == 0.0));
    }

    #[test]
    fn segments_and_padding() {
        // 3 s: speech in windows 10..40 then silence.
        let mut probs = vec![0.0f32; 94];
        for p in &mut probs[10..40] {
            *p = 0.9;
        }
        let speeches = timestamps_from_probs(&probs, 48000, &WHISPER_VAD_OPTIONS);
        assert_eq!(speeches, vec![Speech { start: 5120 - 3200, end: 40 * 512 + 3200 }]);
        let map = SpeechTimestampsMap::new(&speeches);
        assert_eq!(map.get_original_time(PyF::np(0.5), None, false).v, 0.62);
    }

    #[test]
    fn rounding_rules() {
        assert_eq!(np_round(2.675, 2), 2.68);
        assert_eq!(se_core::pyfmt::round_ndigits(2.675, 2), 2.67);
    }
}
