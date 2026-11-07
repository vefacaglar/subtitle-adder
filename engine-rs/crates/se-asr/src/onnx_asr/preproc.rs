//! onnx-asr NumPy preprocessors used on CPU (spec transcribe.md §6.4-§6.5): `nemo128` (Parakeet, Canary) and
//! `gigaam_v3` (GigaAM v3).

use realfft::RealFftPlanner;

static NEMO128: &[u8] = include_bytes!("../../assets/fbanks_nemo128.f32");
static GIGAAM_V3: &[u8] = include_bytes!("../../assets/fbanks_gigaam_v3.f32");
static GIGAAM_V3_WINDOW: &[u8] = include_bytes!("../../assets/fbanks_gigaam_v3_window.f32");

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

/// Batch features `[B, n_feat, T]` (row-major) and lengths.
#[derive(Clone, Debug)]
pub struct BatchFeatures {
    pub batch: usize,
    pub n_feat: usize,
    pub frames: usize,
    pub data: Vec<f32>,
    pub lens: Vec<i64>,
}

/// `spec @ fbanks` with `spec: [n_freqs]`, `fbanks: [n_freqs, n_mels]` (f32 result, f64 accumulation).
fn mel_row(spec: &[f32], fbanks: &[f32], n_mels: usize, out: &mut [f32]) {
    for (m, o) in out.iter_mut().enumerate().take(n_mels) {
        let mut acc = 0f64;
        for (f, &s) in spec.iter().enumerate() {
            acc += s as f64 * fbanks[f * n_mels + m] as f64;
        }
        *o = acc as f32;
    }
}

pub trait Preprocessor: Send {
    fn features_size(&self) -> usize;
    /// `preprocessor(waveforms [B, N], lens)`.
    fn process(&self, waveforms: &[f32], batch: usize, n: usize, lens: &[i64]) -> BatchFeatures;
}

/// `NemoPreprocessorNumpy("nemo128")`.
pub struct NemoPreprocessor {
    fbanks: Vec<f32>,
    window: Vec<f64>,
}

impl Default for NemoPreprocessor {
    fn default() -> Self {
        Self::new()
    }
}

impl NemoPreprocessor {
    const N_FFT: usize = 512;
    const WIN: usize = 400;
    const HOP: usize = 160;
    const N_MELS: usize = 128;

    pub fn new() -> Self {
        // np.hanning(400) (symmetric), zero-padded by 56 on each side.
        let mut window = vec![0f64; Self::N_FFT];
        let left = (Self::N_FFT - Self::WIN) / 2;
        for n in 0..Self::WIN {
            window[left + n] = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (Self::WIN - 1) as f64).cos();
        }
        Self { fbanks: f32s(NEMO128), window }
    }
}

impl Preprocessor for NemoPreprocessor {
    fn features_size(&self) -> usize {
        Self::N_MELS
    }

    fn process(&self, waveforms: &[f32], batch: usize, n: usize, lens: &[i64]) -> BatchFeatures {
        let pad = Self::N_FFT / 2;
        let frames = n / Self::HOP + 1;
        let n_freqs = Self::N_FFT / 2 + 1;
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(Self::N_FFT);
        let mut input = fft.make_input_vec();
        let mut output = fft.make_output_vec();
        let mut scratch = fft.make_scratch_vec();
        let preemph = 0.97f64 as f32;
        let guard = 2f64.powi(-24) as f32;
        let mut data = vec![0f32; batch * Self::N_MELS * frames];
        let mut out_lens = Vec::with_capacity(batch);
        let mut logmel = vec![0f32; frames * Self::N_MELS];
        let mut spec = vec![0f32; n_freqs];
        for b in 0..batch {
            let w = &waveforms[b * n..(b + 1) * n];
            let len = lens[b].max(0) as usize;
            let mut x = vec![0f32; n + 2 * pad];
            for t in 0..n {
                let prev = if t == 0 { 0.0 } else { w[t - 1] };
                x[pad + t] = if t >= len { 0.0 } else { w[t] - preemph * prev };
            }
            for t in 0..frames {
                let off = t * Self::HOP;
                for k in 0..Self::N_FFT {
                    input[k] = x[off + k] as f64 * self.window[k];
                }
                fft.process_with_scratch(&mut input, &mut output, &mut scratch).expect("fft sizes");
                for (f, c) in output.iter().enumerate() {
                    let mag = c.re.hypot(c.im) as f32;
                    spec[f] = mag * mag;
                }
                let row = &mut logmel[t * Self::N_MELS..(t + 1) * Self::N_MELS];
                mel_row(&spec, &self.fbanks, Self::N_MELS, row);
                for v in row.iter_mut() {
                    *v = (*v + guard).ln();
                }
            }
            let flen = (len / Self::HOP) as i64;
            out_lens.push(flen);
            let valid = (flen.max(0) as usize).min(frames);
            let mut sum = vec![0f32; Self::N_MELS];
            for t in 0..valid {
                for m in 0..Self::N_MELS {
                    sum[m] += logmel[t * Self::N_MELS + m];
                }
            }
            let mean: Vec<f32> = sum.iter().map(|s| s / flen as f32).collect();
            let mut vsum = vec![0f32; Self::N_MELS];
            for t in 0..valid {
                for m in 0..Self::N_MELS {
                    let d = logmel[t * Self::N_MELS + m] - mean[m];
                    vsum[m] += d * d;
                }
            }
            let std: Vec<f32> = vsum.iter().map(|s| (s / (flen - 1) as f32).sqrt() + 1e-5f64 as f32).collect();
            let base = b * Self::N_MELS * frames;
            for m in 0..Self::N_MELS {
                for t in 0..valid {
                    data[base + m * frames + t] = (logmel[t * Self::N_MELS + m] - mean[m]) / std[m];
                }
            }
        }
        BatchFeatures { batch, n_feat: Self::N_MELS, frames, data, lens: out_lens }
    }
}

/// `GigaamPreprocessorNumpy("gigaam_v3")`.
pub struct GigaamV3Preprocessor {
    fbanks: Vec<f32>,
    window: Vec<f32>,
}

impl Default for GigaamV3Preprocessor {
    fn default() -> Self {
        Self::new()
    }
}

impl GigaamV3Preprocessor {
    const N_FFT: usize = 320;
    const HOP: usize = 160;
    const N_MELS: usize = 64;

    pub fn new() -> Self {
        Self { fbanks: f32s(GIGAAM_V3), window: f32s(GIGAAM_V3_WINDOW) }
    }
}

impl Preprocessor for GigaamV3Preprocessor {
    fn features_size(&self) -> usize {
        Self::N_MELS
    }

    fn process(&self, waveforms: &[f32], batch: usize, n: usize, lens: &[i64]) -> BatchFeatures {
        let frames = if n >= Self::N_FFT { (n - Self::N_FFT) / Self::HOP + 1 } else { 0 };
        let n_freqs = Self::N_FFT / 2 + 1;
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(Self::N_FFT);
        let mut input = fft.make_input_vec();
        let mut output = fft.make_output_vec();
        let mut scratch = fft.make_scratch_vec();
        let mut data = vec![0f32; batch * Self::N_MELS * frames];
        let mut spec = vec![0f32; n_freqs];
        let mut mel = vec![0f32; Self::N_MELS];
        let (lo, hi) = (1e-9f64 as f32, 1e9f64 as f32);
        for b in 0..batch {
            let w = &waveforms[b * n..(b + 1) * n];
            for t in 0..frames {
                let off = t * Self::HOP;
                for k in 0..Self::N_FFT {
                    input[k] = (w[off + k] * self.window[k]) as f64;
                }
                fft.process_with_scratch(&mut input, &mut output, &mut scratch).expect("fft sizes");
                for (f, c) in output.iter().enumerate() {
                    let mag = (c.re as f32).hypot(c.im as f32);
                    spec[f] = mag * mag;
                }
                mel_row(&spec, &self.fbanks, Self::N_MELS, &mut mel);
                let base = b * Self::N_MELS * frames;
                for m in 0..Self::N_MELS {
                    data[base + m * frames + t] = mel[m].clamp(lo, hi).ln();
                }
            }
        }
        let out_lens = lens.iter().map(|&l| (l - Self::N_FFT as i64).div_euclid(Self::HOP as i64) + 1).collect();
        BatchFeatures { batch, n_feat: Self::N_MELS, frames, data, lens: out_lens }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_have_the_npz_shapes() {
        assert_eq!(f32s(NEMO128).len(), 257 * 128);
        assert_eq!(f32s(GIGAAM_V3).len(), 161 * 64);
        assert_eq!(f32s(GIGAAM_V3_WINDOW).len(), 320);
    }

    #[test]
    fn nemo_shapes_and_mask() {
        let p = NemoPreprocessor::new();
        let wave: Vec<f32> = (0..16000).map(|i| ((i as f32) * 0.01).sin() * 0.1).collect();
        let mut batch = wave.clone();
        batch.extend(std::iter::repeat_n(0.0, 16000));
        batch[16000..24000].copy_from_slice(&wave[..8000]);
        let f = p.process(&batch, 2, 16000, &[16000, 8000]);
        assert_eq!((f.frames, f.lens.clone()), (101, vec![100, 50]));
        // Masked frames are 0.
        assert_eq!(f.data[128 * 101 + 60], 0.0);
    }
}
