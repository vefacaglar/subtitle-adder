//! onnx-asr's Silero VAD (`istupakov/silero-vad-onnx`, `onnx_asr/models/silero.py` + `vad.py`; spec §6.3).

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;
use se_core::{EngineError, Result};

use crate::ort_runtime::{self, ort_err};

const HOP: usize = 512;
const CONTEXT: usize = 64;
const SAMPLE_RATE: i64 = 16000;
const INF: i64 = 1_000_000_000_000_000;

/// VAD options the engine passes (`with_vad(vad, speech_pad_ms=50, min_silence_duration_ms=200)`); the rest are
/// onnx-asr defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OnnxVadOptions {
    pub threshold: f64,
    pub min_speech_duration_ms: f64,
    pub max_speech_duration_s: f64,
    pub min_silence_duration_ms: f64,
    pub speech_pad_ms: f64,
    pub batch_size: usize,
}

impl Default for OnnxVadOptions {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_speech_duration_ms: 250.0,
            max_speech_duration_s: 20.0,
            min_silence_duration_ms: 100.0,
            speech_pad_ms: 30.0,
            batch_size: 8,
        }
    }
}

/// Per-frame model call (a seam for tests): `frame` is 576 samples; returns the probability.
pub trait VadFrameModel {
    fn run(&mut self, frame: &[f32], state: &mut Vec<f32>) -> Result<f32>;
}

/// The Silero session (`input [1,576]`, `state [2,1,128]`, `sr [1]` → `output [1,1]`, `stateN`).
pub struct SileroSession {
    session: Session,
    sr_is_i64: bool,
}

impl SileroSession {
    pub fn load(model: &Path) -> Result<Self> {
        let session = ort_runtime::default_session(model)?;
        let sr_is_i64 = session
            .inputs()
            .iter()
            .find(|i| i.name() == "sr")
            .map(|i| format!("{:?}", i.dtype()).contains("Int64"))
            .unwrap_or(true);
        Ok(Self { session, sr_is_i64 })
    }
}

impl VadFrameModel for SileroSession {
    fn run(&mut self, frame: &[f32], state: &mut Vec<f32>) -> Result<f32> {
        let input = Tensor::from_array(([1usize, frame.len()], frame.to_vec())).map_err(ort_err)?;
        let st = Tensor::from_array(([2usize, 1, 128], state.clone())).map_err(ort_err)?;
        let outputs = if self.sr_is_i64 {
            let sr = Tensor::from_array(([1usize], vec![SAMPLE_RATE])).map_err(ort_err)?;
            self.session.run(ort::inputs!["input" => input, "state" => st, "sr" => sr]).map_err(ort_err)?
        } else {
            let sr = Tensor::from_array(([1usize], vec![SAMPLE_RATE as i32])).map_err(ort_err)?;
            self.session.run(ort::inputs!["input" => input, "state" => st, "sr" => sr]).map_err(ort_err)?
        };
        let (_, out) = outputs["output"].try_extract_tensor::<f32>().map_err(ort_err)?;
        let (_, sn) = outputs["stateN"].try_extract_tensor::<f32>().map_err(ort_err)?;
        let p = out[0];
        *state = sn.to_vec();
        Ok(p)
    }
}

/// `SileroVad._encode` for one waveform: probabilities in order.
pub fn probabilities(model: &mut dyn VadFrameModel, x: &[f32]) -> Result<Vec<f32>> {
    let n = x.len();
    if n < CONTEXT + HOP {
        return Err(EngineError::value("window shape cannot be larger than input array shape"));
    }
    let mut state = vec![0f32; 2 * 128];
    let mut probs = Vec::new();
    let mut first = vec![0f32; CONTEXT];
    first.extend_from_slice(&x[..HOP.min(n)]);
    probs.push(model.run(&first, &mut state)?);
    let mut start = HOP - CONTEXT;
    while start + CONTEXT + HOP <= n {
        probs.push(model.run(&x[start..start + CONTEXT + HOP], &mut state)?);
        start += HOP;
    }
    let r = n % HOP;
    if r != 0 {
        let mut last = x[n - r - CONTEXT..].to_vec();
        last.resize(CONTEXT + HOP, 0.0);
        probs.push(model.run(&last, &mut state)?);
    }
    Ok(probs)
}

/// `_find_segments` (threshold, neg = threshold - 0.15; f32 comparisons).
pub fn find_segments(probs: &[f32], threshold: f64) -> Vec<(i64, i64)> {
    let thr = threshold as f32;
    let neg = (threshold - 0.15) as f32;
    let mut out = Vec::new();
    let mut state = 0;
    let mut start = 0i64;
    for (i, &p) in probs.iter().chain(std::iter::once(&0f32)).enumerate() {
        if state == 0 && p >= thr {
            state = 1;
            start = (i * HOP) as i64;
        } else if state == 1 && p < neg {
            state = 0;
            out.push((start, (i * HOP) as i64));
        }
    }
    out
}

/// `BaseVad._merge_segments`.
pub fn merge_segments(segments: &[(i64, i64)], waveform_len: i64, opts: &OnnxVadOptions) -> Vec<(i64, i64)> {
    let sr = SAMPLE_RATE as f64;
    let pad = (opts.speech_pad_ms * sr / 1000.0).floor() as i64;
    let min_speech = (opts.min_speech_duration_ms * sr / 1000.0).floor() as i64 - 2 * pad;
    let max_speech = (opts.max_speech_duration_s * sr) as i64 - 2 * pad;
    let min_silence = (opts.min_silence_duration_ms * sr / 1000.0).floor() as i64 + 2 * pad;
    let mut out = Vec::new();
    let (mut cur_s, mut cur_e) = (-INF, -INF);
    let tail = [(waveform_len, waveform_len), (INF, INF)];
    for &(s0, e) in segments.iter().chain(tail.iter()) {
        let mut s = s0;
        if s - cur_e < min_silence && e - cur_s < max_speech {
            cur_e = e;
        } else {
            if cur_s < waveform_len && cur_e > cur_s && cur_e - cur_s > min_speech {
                out.push(((cur_s - pad).max(0), (cur_e + pad).min(waveform_len)));
            }
            while e - s > max_speech {
                out.push(((s - pad).max(0), s + max_speech + pad));
                s += max_speech;
            }
            cur_s = s;
            cur_e = e;
        }
    }
    out
}

/// All merged speech segments of a waveform (samples).
pub fn segment(model: &mut dyn VadFrameModel, x: &[f32], opts: &OnnxVadOptions) -> Result<Vec<(i64, i64)>> {
    let probs = probabilities(model, x)?;
    Ok(merge_segments(&find_segments(&probs, opts.threshold), x.len() as i64, opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Vec<usize>);
    impl VadFrameModel for Fake {
        fn run(&mut self, frame: &[f32], _s: &mut Vec<f32>) -> Result<f32> {
            self.0.push(frame.len());
            Ok(0.0)
        }
    }

    #[test]
    fn framing_follows_sliding_window_view() {
        let mut f = Fake(Vec::new());
        let p = probabilities(&mut f, &vec![0.0; 1600]).unwrap();
        // first + windows at 448, 960 (+576 <= 1600) + last partial (1600 % 512 = 64)
        assert_eq!(p.len(), 4);
        assert!(f.0.iter().all(|&l| l == 576));
        assert!(probabilities(&mut f, &vec![0.0; 575]).is_err());
    }

    #[test]
    fn merge_pads_and_drops_short_segments() {
        let opts = OnnxVadOptions { speech_pad_ms: 50.0, min_silence_duration_ms: 200.0, ..Default::default() };
        let segs = merge_segments(&[(16000, 32000), (34000, 40000), (60000, 60512)], 80000, &opts);
        assert_eq!(segs, vec![(15200, 40800)]);
        assert_eq!(find_segments(&[0.6, 0.6, 0.3, 0.9], 0.5), vec![(0, 1024), (1536, 2048)]);
    }
}
