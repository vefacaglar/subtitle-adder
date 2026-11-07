//! Whisper log-mel features (`faster_whisper.feature_extractor.FeatureExtractor`; spec transcribe.md §4.4).

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

pub const SAMPLING_RATE: usize = 16000;
pub const HOP_LENGTH: usize = 160;
pub const N_FFT: usize = 400;
pub const N_FREQS: usize = N_FFT / 2 + 1;
/// `nb_max_frames` (30 s windows).
pub const N_FRAMES: usize = 3000;

/// numpy `linspace(start, stop, num)` (endpoint included).
fn linspace(start: f64, stop: f64, num: usize) -> Vec<f64> {
    if num == 0 {
        return Vec::new();
    }
    if num == 1 {
        return vec![start];
    }
    let div = (num - 1) as f64;
    let step = (stop - start) / div;
    let mut y: Vec<f64> = (0..num).map(|i| i as f64 * step + start).collect();
    y[num - 1] = stop;
    y
}

/// Slaney mel filters (librosa-compatible), `[n_mels][201]` as f32.
pub fn mel_filters(sr: usize, n_fft: usize, n_mels: usize) -> Vec<f32> {
    let n_freqs = n_fft / 2 + 1;
    let d = 1.0 / sr as f64;
    let val = 1.0 / (n_fft as f64 * d);
    let fftfreqs: Vec<f64> = (0..n_freqs).map(|j| j as f64 * val).collect();
    let mels = linspace(0.0, 45.245640471924965, n_mels + 2);
    let f_min = 0.0;
    let f_sp = 200.0 / 3.0;
    let mut freqs: Vec<f64> = mels.iter().map(|m| f_min + f_sp * m).collect();
    let min_log_hz = 1000.0;
    let min_log_mel = (min_log_hz - f_min) / f_sp;
    let logstep = 6.4f64.ln() / 27.0;
    for (f, m) in freqs.iter_mut().zip(&mels) {
        if *m >= min_log_mel {
            *f = min_log_hz * (logstep * (m - min_log_mel)).exp();
        }
    }
    let fdiff: Vec<f64> = freqs.windows(2).map(|w| w[1] - w[0]).collect();
    let mut out = vec![0f32; n_mels * n_freqs];
    for m in 0..n_mels {
        let enorm = 2.0 / (freqs[m + 2] - freqs[m]);
        for j in 0..n_freqs {
            let lower = -(freqs[m] - fftfreqs[j]) / fdiff[m];
            let upper = (freqs[m + 2] - fftfreqs[j]) / fdiff[m + 1];
            let w = 0f64.max(lower.min(upper));
            out[m * n_freqs + j] = (w * enorm) as f32;
        }
    }
    out
}

/// Index into a numpy `reflect`-padded array.
fn reflect_index(i: isize, n: usize) -> usize {
    if n == 1 {
        return 0;
    }
    let period = 2 * (n as isize - 1);
    let mut k = i.rem_euclid(period);
    if k >= n as isize {
        k = period - k;
    }
    k as usize
}

/// Log-mel spectrogram `[n_mels, T]` (row-major) with `T = len(audio) // 160 + 1`.
pub struct Features {
    pub n_mels: usize,
    pub frames: usize,
    pub data: Vec<f32>,
}

impl Features {
    /// `content_frames = T - 1`.
    pub fn content_frames(&self) -> usize {
        self.frames - 1
    }

    /// `pad_or_trim(features[:, start : start + len], 3000)` as a flat `[n_mels, 3000]` buffer.
    pub fn window(&self, start: usize, len: usize) -> Vec<f32> {
        let mut out = vec![0f32; self.n_mels * N_FRAMES];
        let end = (start + len).min(self.frames);
        let take = end.saturating_sub(start).min(N_FRAMES);
        for m in 0..self.n_mels {
            let row = &self.data[m * self.frames..(m + 1) * self.frames];
            out[m * N_FRAMES..m * N_FRAMES + take].copy_from_slice(&row[start..start + take]);
        }
        out
    }
}

pub struct FeatureExtractor {
    pub n_mels: usize,
    filters: Vec<f32>,
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f64>>,
}

impl FeatureExtractor {
    pub fn new(n_mels: usize) -> Self {
        let window: Vec<f32> = (0..N_FFT)
            .map(|n| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / N_FFT as f64).cos()) as f32)
            .collect();
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(N_FFT);
        Self { n_mels, filters: mel_filters(SAMPLING_RATE, N_FFT, n_mels), window, fft }
    }

    /// `FeatureExtractor.__call__(waveform, padding=160)`.
    pub fn compute(&self, audio: &[f32]) -> Features {
        let mut x: Vec<f32> = Vec::with_capacity(audio.len() + HOP_LENGTH);
        x.extend_from_slice(audio);
        x.resize(audio.len() + HOP_LENGTH, 0.0);
        let pad = N_FFT / 2;
        let n = x.len();
        let padded: Vec<f32> =
            (0..n + 2 * pad).map(|i| x[reflect_index(i as isize - pad as isize, n)]).collect();
        let n_frames = 1 + (padded.len() - N_FFT) / HOP_LENGTH;
        let frames = n_frames - 1;

        let mut mags = vec![0f32; frames * N_FREQS];
        let mut input = self.fft.make_input_vec();
        let mut output = self.fft.make_output_vec();
        let mut scratch = self.fft.make_scratch_vec();
        for t in 0..frames {
            let off = t * HOP_LENGTH;
            for k in 0..N_FFT {
                input[k] = (padded[off + k] * self.window[k]) as f64;
            }
            self.fft.process_with_scratch(&mut input, &mut output, &mut scratch).expect("fft sizes");
            for (f, c) in output.iter().enumerate() {
                let re = c.re as f32;
                let im = c.im as f32;
                let mag = re.hypot(im);
                mags[t * N_FREQS + f] = mag * mag;
            }
        }

        let mut data = vec![0f32; self.n_mels * frames];
        let clip = 1e-10f64 as f32;
        let mut max = f32::NEG_INFINITY;
        for m in 0..self.n_mels {
            let filt = &self.filters[m * N_FREQS..(m + 1) * N_FREQS];
            for t in 0..frames {
                let mag = &mags[t * N_FREQS..(t + 1) * N_FREQS];
                let mut acc = 0f64;
                for f in 0..N_FREQS {
                    acc += filt[f] as f64 * mag[f] as f64;
                }
                let v = (acc as f32).max(clip);
                let l = (v as f64).log10() as f32;
                if l > max {
                    max = l;
                }
                data[m * frames + t] = l;
            }
        }
        let floor = max - 8.0f32;
        for v in data.iter_mut() {
            *v = (v.max(floor) + 4.0f32) / 4.0f32;
        }
        Features { n_mels: self.n_mels, frames, data }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mel_filter_row_matches_librosa() {
        let f = mel_filters(16000, 400, 80);
        let row1 = &f[N_FREQS..N_FREQS + 6];
        let expected = [0.0, 0.00199082, 0.02287177, 0.0, 0.0, 0.0];
        for (a, b) in row1.iter().zip(expected) {
            assert!((*a as f64 - b).abs() < 1e-7, "{row1:?}");
        }
    }

    #[test]
    fn shapes_and_empty_audio() {
        let fe = FeatureExtractor::new(80);
        let feats = fe.compute(&vec![0.1f32; 16000]);
        assert_eq!((feats.n_mels, feats.frames), (80, 101));
        let empty = fe.compute(&[]);
        assert_eq!(empty.frames, 1);
        assert!(empty.data.iter().all(|&v| v == -1.5));
    }
}
