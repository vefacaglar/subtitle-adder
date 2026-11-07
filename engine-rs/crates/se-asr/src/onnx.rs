//! `OnnxAsrTranscriber` (onnx-asr port on ONNX Runtime; spec transcribe.md §5-§6).

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use se_core::{EngineError, ProgressFn, Result, Transcript, Word, WordsFn};

use crate::Transcriber;
use crate::onnx_asr::models::{self, AsrModel};
use crate::onnx_asr::vad::{self as asr_vad, OnnxVadOptions, SileroSession, VadFrameModel};
use crate::wave::{self, WaveReader};

/// `WORD_MARKERS`: word-start markers in onnx-asr tokens (space and U+2581).
pub const WORD_MARKERS: [char; 2] = [' ', '\u{2581}'];

/// onnx-asr VAD options (`VAD_OPTIONS`): speech pad 50 ms, min silence 200 ms.
pub const VAD_SPEECH_PAD_MS: u32 = 50;
pub const VAD_MIN_SILENCE_DURATION_MS: u32 = 200;

/// The engine's `with_vad(vad, **VAD_OPTIONS)` options on top of onnx-asr's defaults.
pub fn vad_options() -> OnnxVadOptions {
    OnnxVadOptions {
        speech_pad_ms: VAD_SPEECH_PAD_MS as f64,
        min_silence_duration_ms: VAD_MIN_SILENCE_DURATION_MS as f64,
        ..OnnxVadOptions::default()
    }
}

/// `recognize(audio, **options)` keyword arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecognizeOptions {
    pub language: Option<String>,
    pub target_language: Option<String>,
}

/// A `TimestampedSegmentResult` (the fields the engine reads).
#[derive(Clone, Debug, PartialEq)]
pub struct SegmentResult {
    pub start: f64,
    pub end: f64,
    pub tokens: Option<Vec<String>>,
    pub timestamps: Option<Vec<f64>>,
}

/// A loaded VAD (`onnx_asr.load_vad("silero")`).
pub trait OnnxVad {
    /// Merged speech segments (samples) of a mono 16 kHz waveform.
    fn segments(&mut self, waveform: &[f32], options: &OnnxVadOptions) -> Result<Vec<(i64, i64)>>;
}

/// A loaded model (`onnx_asr.load_model(...)`) driven through `with_vad(...).with_timestamps().recognize(...)`.
pub trait OnnxAsrModel {
    /// Calls `on_segment` for every recognized VAD segment, in order.
    fn recognize(
        &mut self,
        vad: &mut dyn OnnxVad,
        vad_options: &OnnxVadOptions,
        audio: &Path,
        options: &RecognizeOptions,
        on_segment: &mut dyn FnMut(SegmentResult) -> Result<()>,
    ) -> Result<()>;
}

/// Loader hooks (Python tests replace `onnx_asr.load_model` / `load_vad`).
pub struct OnnxHooks {
    /// `(model_name, quantization, providers)`
    pub load_model: Box<dyn Fn(&str, Option<&str>, &[&str]) -> Result<Box<dyn OnnxAsrModel>>>,
    /// `(vad_name, providers)`
    pub load_vad: Box<dyn Fn(&str, &[&str]) -> Result<Box<dyn OnnxVad>>>,
}

impl Default for OnnxHooks {
    fn default() -> Self {
        Self {
            load_model: Box::new(|name, quantization, _providers| {
                Ok(Box::new(RealModel(models::load_model(name, quantization)?)) as Box<dyn OnnxAsrModel>)
            }),
            load_vad: Box::new(|name, _providers| Ok(Box::new(SileroVad::load(name)?) as Box<dyn OnnxVad>)),
        }
    }
}

const PROVIDERS: [&str; 1] = ["CPUExecutionProvider"];
const SUPPORTED_RATES: [u32; 8] = [8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000];

/// onnx-asr's Silero VAD loaded from the HF cache.
pub struct SileroVad(SileroSession);

impl SileroVad {
    pub fn load(name: &str) -> Result<Self> {
        crate::ort_runtime::init()?;
        let dir = crate::onnx_asr::model_dir(name)?;
        let file = crate::onnx_asr::find_file(&dir, "silero_vad.onnx")?;
        Ok(Self(SileroSession::load(&file)?))
    }
}

impl OnnxVad for SileroVad {
    fn segments(&mut self, waveform: &[f32], options: &OnnxVadOptions) -> Result<Vec<(i64, i64)>> {
        asr_vad::segment(&mut self.0 as &mut dyn VadFrameModel, waveform, options)
    }
}

/// onnx-asr `read_wav` + mono selection (`x = int / 2**(8w-1)`, u8 shifted) and the sample-rate check.
pub fn read_wav_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut wav = WaveReader::open(path)?;
    let n = wav.nframes();
    let data = wav.readframes(n)?;
    let width = wav.sampwidth() as usize;
    let channels = wav.nchannels() as usize;
    let samples: Vec<f32> = match width {
        1 => data.iter().map(|&b| b as f32 / 128.0 - 1.0).collect(),
        2 => data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect(),
        3 => data
            .chunks_exact(3)
            .map(|b| i32::from_le_bytes([0, b[0], b[1], b[2]]) as f32 / 2147483648.0)
            .collect(),
        4 => data
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2147483648.0)
            .collect(),
        _ => return Err(EngineError::value(format!("unsupported sample width {width}"))),
    };
    if channels != 1 {
        return Err(EngineError::value(
            "Supported only mono audio. Use the 'channel' parameter to select a channel or 'mean' to average channels.",
        ));
    }
    let rate = wav.framerate();
    if !SUPPORTED_RATES.contains(&rate) {
        return Err(EngineError::value(
            "Supported only (8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000) sample rates.",
        ));
    }
    if rate != 16000 {
        return Err(EngineError::value(format!("resampling {rate} Hz audio to 16000 Hz is not supported")));
    }
    Ok((samples, rate))
}

/// The real model behind the adapter chain.
struct RealModel(Box<dyn AsrModel>);

impl OnnxAsrModel for RealModel {
    fn recognize(
        &mut self,
        vad: &mut dyn OnnxVad,
        vad_options: &OnnxVadOptions,
        audio: &Path,
        options: &RecognizeOptions,
        on_segment: &mut dyn FnMut(SegmentResult) -> Result<()>,
    ) -> Result<()> {
        let (x, _rate) = read_wav_mono(audio)?;
        let segments = vad.segments(&x, vad_options)?;
        let n = x.len() as i64;
        for batch in segments.chunks(vad_options.batch_size.max(1)) {
            let slices: Vec<&[f32]> = batch
                .iter()
                .map(|&(s, e)| {
                    let a = s.clamp(0, n) as usize;
                    let b = e.clamp(0, n) as usize;
                    &x[a..b.max(a)]
                })
                .collect();
            let max_len = slices.iter().map(|s| s.len()).max().unwrap_or(0);
            let mut padded = vec![0f32; slices.len() * max_len];
            let mut lens = Vec::with_capacity(slices.len());
            for (i, s) in slices.iter().enumerate() {
                padded[i * max_len..i * max_len + s.len()].copy_from_slice(s);
                lens.push(s.len() as i64);
            }
            let results = self.0.recognize_batch(
                &padded,
                slices.len(),
                max_len,
                &lens,
                options.language.as_deref(),
                options.target_language.as_deref(),
            )?;
            for (res, &(s, e)) in results.into_iter().zip(batch) {
                on_segment(SegmentResult {
                    start: s as f64 / 16000.0,
                    end: e as f64 / 16000.0,
                    tokens: Some(res.tokens),
                    timestamps: res.timestamps,
                })?;
            }
        }
        Ok(())
    }
}

/// Parakeet / Canary / GigaAM on CPU int8 with onnx-asr's silero VAD.
pub struct OnnxAsrTranscriber {
    model_name: String,
    quantization: Option<String>,
    canary: bool,
    default_language: Option<String>,
    model: Box<dyn OnnxAsrModel>,
    vad: Box<dyn OnnxVad>,
}

impl OnnxAsrTranscriber {
    /// `OnnxAsrTranscriber(model_name, quantization)`: loads the model files from the HF cache
    /// (`nemo-parakeet-tdt-0.6b-v3`, `nemo-canary-1b-v2`, `gigaam-v3-e2e-rnnt`) and the shared silero VAD.
    /// Requires [`crate::ort_runtime::init`].
    pub fn new(model_name: &str, quantization: Option<&str>) -> Result<Self> {
        Self::with_hooks(model_name, quantization, &OnnxHooks::default())
    }

    /// [`OnnxAsrTranscriber::new`] with injected loaders (tests). Python's default quantization is `"int8"`;
    /// callers that want it pass `Some("int8")`.
    pub fn with_hooks(model_name: &str, quantization: Option<&str>, hooks: &OnnxHooks) -> Result<Self> {
        let canary = model_name.starts_with("canary") || model_name.starts_with("nemo-canary");
        let default_language = model_name.starts_with("gigaam").then(|| "ru".to_string());
        let model = (hooks.load_model)(model_name, quantization, &PROVIDERS)?;
        let vad = (hooks.load_vad)("silero", &PROVIDERS)?;
        Ok(Self {
            model_name: model_name.to_string(),
            quantization: quantization.map(str::to_string),
            canary,
            default_language,
            model,
            vad,
        })
    }

    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    pub fn quantization(&self) -> Option<&str> {
        self.quantization.as_deref()
    }
}

impl Transcriber for OnnxAsrTranscriber {
    fn transcribe(
        &mut self,
        audio: &Path,
        language: Option<&str>,
        on_progress: Option<ProgressFn<'_>>,
        on_words: Option<WordsFn<'_>>,
    ) -> Result<Transcript> {
        let duration = wave::wav_duration(audio)?;
        // `language or self._default_language`
        let language: Option<String> = match language {
            Some(l) if !l.is_empty() => Some(l.to_string()),
            _ => self.default_language.clone(),
        };
        let mut options = RecognizeOptions::default();
        if let Some(l) = &language {
            options.language = Some(l.clone());
            if self.canary {
                options.target_language = Some(l.clone());
            }
        }
        let label = language.clone().unwrap_or_else(|| "?".to_string());
        let mut words: Vec<Word> = Vec::new();
        let mut on_progress = on_progress;
        let mut on_words = on_words;
        let vad_opts = vad_options();
        let mut handle = |seg: SegmentResult| -> Result<()> {
            let seg_words = words_from_tokens(seg.tokens.as_deref(), seg.timestamps.as_deref(), seg.start, seg.end);
            if !seg_words.is_empty() {
                words.extend(seg_words);
                if let Some(cb) = on_words.as_mut() {
                    cb(&words, &label);
                }
            }
            if let Some(cb) = on_progress.as_mut() {
                if duration > 0.0 {
                    cb((seg.end / duration).min(1.0));
                }
            }
            Ok(())
        };
        self.model.recognize(self.vad.as_mut(), &vad_opts, audio, &options, &mut handle)?;
        Ok(Transcript { language: label, duration, words })
    }
}

fn special_token() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^<[^<>]+>$").expect("valid regex"))
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    // max(low, min(value, high)) with Python's first-wins semantics.
    let m = if hi < v { hi } else { v };
    if m > lo { m } else { lo }
}

/// `_estimated_timestamps(tokens, duration)`.
fn estimated_timestamps(tokens: &[String], duration: f64) -> Vec<f64> {
    let weights: Vec<i64> =
        tokens.iter().map(|t| (se_core::pyfmt::char_len(se_core::pyfmt::strip(t)) as i64).max(1)).collect();
    let total: i64 = weights.iter().sum();
    let mut out = Vec::with_capacity(tokens.len());
    let mut consumed = 0i64;
    for w in weights {
        out.push(duration * consumed as f64 / total as f64);
        consumed += w;
    }
    out
}

/// `words_from_tokens(tokens, timestamps, segment_start, segment_end)`: joins tokens into words at
/// [`WORD_MARKERS`], drops special tokens (`^<[^<>]+>$`), estimates timestamps from token lengths when
/// `timestamps` is `None` (Canary) and clamps times into the segment.
pub fn words_from_tokens(
    tokens: Option<&[String]>,
    timestamps: Option<&[f64]>,
    segment_start: f64,
    segment_end: f64,
) -> Vec<Word> {
    let Some(tokens) = tokens.filter(|t| !t.is_empty()) else {
        return Vec::new();
    };
    let times: Vec<f64> = match timestamps {
        Some(t) => t.to_vec(),
        None => estimated_timestamps(tokens, (segment_end - segment_start).max(0.0)),
    };
    let mut groups: Vec<Vec<(String, f64)>> = Vec::new();
    let mut current: Vec<(String, f64)> = Vec::new();
    for (token, &at) in tokens.iter().zip(times.iter()) {
        let stripped = se_core::pyfmt::strip(token);
        if !stripped.is_empty() && special_token().is_match(stripped) {
            continue;
        }
        let starts_word = token.starts_with(WORD_MARKERS);
        let content = token.trim_start_matches(WORD_MARKERS);
        if starts_word && !current.is_empty() {
            groups.push(std::mem::take(&mut current));
        }
        if !content.is_empty() {
            current.push((content.to_string(), at));
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }
    let mut out = Vec::with_capacity(groups.len());
    for (i, g) in groups.iter().enumerate() {
        let start = clamp(segment_start + g[0].1, segment_start, segment_end);
        let next_start = if i + 1 < groups.len() {
            clamp(segment_start + groups[i + 1][0].1, segment_start, segment_end)
        } else {
            segment_end
        };
        let end = if start > next_start { start } else { next_start };
        let text: String = g.iter().map(|(t, _)| t.as_str()).collect();
        out.push(Word::new(
            se_core::pyfmt::round_ndigits(start, 3),
            se_core::pyfmt::round_ndigits(end, 3),
            text,
        ));
    }
    out
}
