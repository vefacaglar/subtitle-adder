//! Colour science of the GPU video path: YUV → R'G'B' uniforms, transfer functions (PQ, HLG, BT.1886), BT.2390 tone
//! mapping of HDR to SDR, and gamut conversion to BT.709.
//!
//! The UI's fragment shader ([`glsl`]) and the CPU functions here use the same constants (the GLSL source is generated
//! from them), so the unit tests below check the math the GPU runs. [`ColorParams::apply`] is the CPU mirror of the
//! shader's `color_map` for one pixel; [`ToneLuts`] is the same mapping as lookup tables, which the CPU (software
//! renderer) path runs per pixel (`crate::soft`).
//!
//! Pipeline per pixel (after the YUV matrix): SDR BT.709-ish content is left exactly as it is (mode 0). Otherwise the
//! signal is linearized (BT.1886 gamma 2.4 for SDR, the PQ EOTF in nits, or the HLG inverse OETF + OOTF for a
//! 1000-nit display), converted to BT.709 primaries, tone-mapped for HDR (BT.2390 EETF on the largest component, from
//! the content's peak to SDR reference white, 203 nits, which becomes 1.0) and encoded again with gamma 1/2.4.
//!
//! Known limits (accepted for now):
//! * Components that are negative after the gamut conversion (colours outside BT.709) are clipped to 0 before the
//!   tone curve, so saturated wide-gamut colours lose hue accuracy instead of being gamut-mapped.
//! * The content peak trusts the stream's MaxCLL (then the mastering peak): a stream that understates MaxCLL has
//!   highlights above it clipped rather than rolled off.

use crate::{PlanarImage, PlaneLayout, Primaries, Transfer};

/// SDR reference white in nits (BT.2408): HDR diffuse white lands here, and it is the SDR output's 1.0.
pub const SDR_WHITE_NITS: f32 = 203.0;
/// Content peak assumed when an HDR stream carries no mastering or content-light metadata.
pub const DEFAULT_PEAK_NITS: f32 = 1000.0;
/// Nominal peak of the display the HLG OOTF is evaluated for (system gamma 1.2).
pub const HLG_PEAK_NITS: f32 = 1000.0;
/// SDR display gamma (BT.1886 with a zero black level).
pub const SDR_GAMMA: f32 = 2.4;

// SMPTE ST 2084 (PQ).
pub const PQ_M1: f32 = 2610.0 / 16384.0;
pub const PQ_M2: f32 = 2523.0 / 4096.0 * 128.0;
pub const PQ_C1: f32 = 3424.0 / 4096.0;
pub const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
pub const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;
pub const PQ_MAX_NITS: f32 = 10000.0;

// ARIB STD-B67 (HLG).
pub const HLG_A: f32 = 0.178_832_77;
pub const HLG_B: f32 = 0.284_668_92;
pub const HLG_C: f32 = 0.559_910_7;

/// PQ EOTF: signal 0..1 → nits.
pub fn pq_eotf(e: f32) -> f32 {
    let p = e.max(0.0).powf(1.0 / PQ_M2);
    let num = (p - PQ_C1).max(0.0);
    let den = PQ_C2 - PQ_C3 * p;
    PQ_MAX_NITS * (num / den).powf(1.0 / PQ_M1)
}

/// Inverse PQ EOTF: nits → signal 0..1.
pub fn pq_inverse_eotf(nits: f32) -> f32 {
    let y = (nits / PQ_MAX_NITS).max(0.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)).powf(PQ_M2)
}

/// HLG inverse OETF: signal 0..1 → normalized scene light 0..1.
pub fn hlg_inverse_oetf(e: f32) -> f32 {
    let e = e.max(0.0);
    if e <= 0.5 { e * e / 3.0 } else { (((e - HLG_C) / HLG_A).exp() + HLG_B) / 12.0 }
}

/// HLG OOTF for a display of `HLG_PEAK_NITS`: scene light → display light in nits. `luma` = luminance coefficients
/// of the primaries.
pub fn hlg_ootf(scene: [f32; 3], luma: [f32; 3]) -> [f32; 3] {
    let ys = (scene[0] * luma[0] + scene[1] * luma[1] + scene[2] * luma[2]).max(0.0);
    let gain = HLG_PEAK_NITS * ys.max(1e-6).powf(HLG_SYSTEM_GAMMA - 1.0);
    scene.map(|e| e * gain)
}

/// HLG system gamma at `HLG_PEAK_NITS` (1.2 + 0.42 log10(Lw / 1000)).
pub const HLG_SYSTEM_GAMMA: f32 = 1.2;

/// The parameters of the BT.2390 EETF from `peak_nits` to [`SDR_WHITE_NITS`]: `(source peak as PQ, target peak
/// normalized to the source, knee start)`. A source that is not brighter than the target gets a knee of 1 (no
/// compression).
pub fn eetf_params(peak_nits: f32) -> [f32; 3] {
    let src = pq_inverse_eotf(peak_nits.max(1.0));
    let dst = pq_inverse_eotf(SDR_WHITE_NITS);
    if dst >= src {
        return [src, 1.0, 1.0];
    }
    let max_lum = dst / src;
    [src, max_lum, 1.5 * max_lum - 0.5]
}

/// BT.2390 EETF (ITU-R BT.2390, section 5.4) in the PQ domain, without the black-level lift: nits → nits.
pub fn eetf(nits: f32, params: [f32; 3]) -> f32 {
    let [src, max_lum, ks] = params;
    let e1 = (pq_inverse_eotf(nits) / src).clamp(0.0, 1.0);
    let e2 = if e1 < ks || ks >= 1.0 {
        e1
    } else {
        let t = (e1 - ks) / (1.0 - ks);
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * ks + (t3 - 2.0 * t2 + t) * (1.0 - ks) + (-2.0 * t3 + 3.0 * t2) * max_lum
    };
    pq_eotf(e2 * src)
}

/// Chromaticities `[R, G, B, white]` as (x, y).
pub fn chromaticities(p: Primaries) -> [(f64, f64); 4] {
    const D65: (f64, f64) = (0.3127, 0.3290);
    match p {
        Primaries::Bt709 => [(0.640, 0.330), (0.300, 0.600), (0.150, 0.060), D65],
        Primaries::Bt2020 => [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046), D65],
        Primaries::DisplayP3 => [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060), D65],
    }
}

type M3 = [[f64; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for (i, row) in r.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn invert(m: &M3) -> M3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let inv = 1.0 / det;
    [
        [(e * i - f * h) * inv, (c * h - b * i) * inv, (b * f - c * e) * inv],
        [(f * g - d * i) * inv, (a * i - c * g) * inv, (c * d - a * f) * inv],
        [(d * h - e * g) * inv, (b * g - a * h) * inv, (a * e - b * d) * inv],
    ]
}

/// Linear RGB → CIE XYZ for primaries `p` (rows X, Y, Z).
fn rgb_to_xyz(p: Primaries) -> M3 {
    let c = chromaticities(p);
    let xyz = |(x, y): (f64, f64)| [x / y, 1.0, (1.0 - x - y) / y];
    let (r, g, b, w) = (xyz(c[0]), xyz(c[1]), xyz(c[2]), xyz(c[3]));
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let inv = invert(&m);
    let s: Vec<f64> = (0..3).map(|i| (0..3).map(|k| inv[i][k] * w[k]).sum()).collect();
    let mut out = m;
    for row in out.iter_mut() {
        for (j, v) in row.iter_mut().enumerate() {
            *v *= s[j];
        }
    }
    out
}

/// Linear RGB of primaries `from` → linear RGB of `to` (row-major).
pub fn gamut_matrix(from: Primaries, to: Primaries) -> [[f32; 3]; 3] {
    let m = mul(&invert(&rgb_to_xyz(to)), &rgb_to_xyz(from));
    m.map(|r| r.map(|v| v as f32))
}

/// Luminance coefficients (the Y row of RGB → XYZ) of primaries `p`.
pub fn luminance_coefficients(p: Primaries) -> [f32; 3] {
    rgb_to_xyz(p)[1].map(|v| v as f32)
}

/// The content peak to tone-map from: MaxCLL when present, else the mastering display's peak, else
/// [`DEFAULT_PEAK_NITS`]; never below SDR white (nothing to compress) nor above the PQ maximum.
pub fn hdr_peak_nits(max_cll: Option<f32>, mastering_max: Option<f32>) -> f32 {
    let valid = |v: Option<f32>| v.filter(|v| v.is_finite() && *v >= 1.0);
    valid(max_cll).or(valid(mastering_max)).unwrap_or(DEFAULT_PEAK_NITS).clamp(SDR_WHITE_NITS, PQ_MAX_NITS)
}

/// What the shader does after the YUV matrix (`u_color`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// SDR in BT.709 (or unknown) primaries: nothing; the result is the plain matrix conversion.
    None = 0,
    /// SDR in wider primaries: gamut conversion only.
    Gamut = 1,
    Pq = 2,
    Hlg = 3,
}

/// Uniforms of the shader's `color_map`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorParams {
    pub mode: ColorMode,
    /// Source → BT.709 linear RGB, row-major (upload with `transpose = true`, or transposed).
    pub gamut: [[f32; 3]; 3],
    /// EETF `(source peak as PQ, normalized target peak, knee)` and SDR white (nits).
    pub tone: [f32; 4],
    /// Luminance coefficients of the source primaries (HLG OOTF).
    pub luma: [f32; 3],
}

impl ColorParams {
    pub fn new(transfer: Transfer, primaries: Primaries, peak_nits: f32) -> Self {
        let mode = match transfer {
            Transfer::Pq => ColorMode::Pq,
            Transfer::Hlg => ColorMode::Hlg,
            Transfer::Sdr if primaries == Primaries::Bt709 => ColorMode::None,
            Transfer::Sdr => ColorMode::Gamut,
        };
        let peak = match transfer {
            Transfer::Hlg => HLG_PEAK_NITS,
            _ => peak_nits,
        };
        let [src, max_lum, ks] = eetf_params(peak);
        Self {
            mode,
            gamut: gamut_matrix(primaries, Primaries::Bt709),
            tone: [src, max_lum, ks, SDR_WHITE_NITS],
            luma: luminance_coefficients(primaries),
        }
    }

    pub fn for_image(img: &PlanarImage) -> Self {
        Self::new(img.transfer, img.primaries, img.peak_nits)
    }

    /// The gamut matrix column-major (what `glUniformMatrix3fv` takes without transposing).
    pub fn gamut_column_major(&self) -> [f32; 9] {
        let m = self.gamut;
        [m[0][0], m[1][0], m[2][0], m[0][1], m[1][1], m[2][1], m[0][2], m[1][2], m[2][2]]
    }

    /// CPU mirror of the shader's `color_map`: non-linear R'G'B' (after the YUV matrix) → display R'G'B' 0..1.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let g = &self.gamut;
        let gamut = |c: [f32; 3]| -> [f32; 3] {
            [0, 1, 2].map(|i| g[i][0] * c[0] + g[i][1] * c[1] + g[i][2] * c[2])
        };
        let encode = |c: [f32; 3]| c.map(|v| v.clamp(0.0, 1.0).powf(1.0 / SDR_GAMMA));
        let rgb = rgb.map(|v| v.clamp(0.0, 1.0));
        match self.mode {
            ColorMode::None => rgb,
            ColorMode::Gamut => encode(gamut(rgb.map(|v| v.powf(SDR_GAMMA)))),
            ColorMode::Pq | ColorMode::Hlg => {
                let nits = if self.mode == ColorMode::Pq {
                    rgb.map(pq_eotf)
                } else {
                    hlg_ootf(rgb.map(hlg_inverse_oetf), self.luma)
                };
                let lin = gamut(nits).map(|v| v.max(0.0));
                let m = lin[0].max(lin[1]).max(lin[2]);
                let scale = if m > 0.0 { eetf(m, [self.tone[0], self.tone[1], self.tone[2]]) / m } else { 0.0 };
                encode(lin.map(|v| v * scale / self.tone[3]))
            }
        }
    }
}

/// Entries (plus one) of [`ToneLuts`]' signal → linear light table, over the signal 0..1.
const DECODE_STEPS: usize = 4096;
/// Entries (plus one) of the HLG OOTF gain table, over the square root of the scene luminance 0..1.
const GAIN_STEPS: usize = 4096;
/// Entries of the EETF scale table between the knee and the content peak.
const EETF_ENTRIES: usize = 2048;
/// Entries of the output encode table, over the square root of linear light 0..1 (the square root spreads the
/// table's resolution where gamma 1/2.4 is steep, so the darkest codes are exact as well).
const ENCODE_ENTRIES: usize = 65536;

/// [`ColorParams::apply`] as lookup tables, for the CPU (software renderer) path: non-linear R'G'B' → display R'G'B'
/// as 8-bit codes, a few table reads and multiplications per pixel instead of half a dozen `powf`.
///
/// Per pixel, in `apply`'s order: (1) every channel through `decode` (signal → nits for PQ, scene light for HLG,
/// `v^2.4` for the gamut-only mode; linear interpolation); (2) HLG only: the OOTF gain from the scene luminance
/// (`gain`, indexed by its square root); (3) the gamut matrix, negative components clipped to 0; (4) HDR only: the
/// EETF as a scale factor of the largest component `m` (1 below the knee, the `eetf` table interpolated between the
/// knee and the peak, `top / m` above the peak, where the curve is flat), divided by SDR white; (5) `encode`
/// (gamma 1/2.4 to a code). The EETF cannot be a per-channel curve: its scale depends on max(R, G, B) after the
/// gamut matrix. The unit tests bound the difference to `round(apply(rgb) * 255)`.
#[derive(Debug, Clone)]
pub struct ToneLuts {
    params: ColorParams,
    // Fixed-size tables: the clamped indices need no bounds checks.
    decode: Box<[f32; DECODE_STEPS + 1]>,
    gain: Box<[f32; GAIN_STEPS + 1]>,
    /// EETF knee and peak (nits); the EETF's output at and above the peak.
    knee: f32,
    peak: f32,
    top: f32,
    eetf_steps_per_nit: f32,
    /// `eetf(m) / m / white` at `knee + i * (peak - knee) / (EETF_ENTRIES - 1)`.
    eetf: Box<[f32; EETF_ENTRIES]>,
    white_inv: f32,
    encode: Box<[u8; ENCODE_ENTRIES]>,
}

impl ToneLuts {
    pub fn new(p: &ColorParams) -> Self {
        let decode = match p.mode {
            ColorMode::None => Vec::new(),
            ColorMode::Gamut => table(DECODE_STEPS + 1, |i| (i as f32 / DECODE_STEPS as f32).powf(SDR_GAMMA)),
            ColorMode::Pq => table(DECODE_STEPS + 1, |i| pq_eotf(i as f32 / DECODE_STEPS as f32)),
            ColorMode::Hlg => table(DECODE_STEPS + 1, |i| hlg_inverse_oetf(i as f32 / DECODE_STEPS as f32)),
        };
        let gain = if p.mode == ColorMode::Hlg {
            table(GAIN_STEPS + 1, |i| {
                let s = i as f32 / GAIN_STEPS as f32;
                HLG_PEAK_NITS * (s * s).max(1e-6).powf(HLG_SYSTEM_GAMMA - 1.0)
            })
        } else {
            Vec::new()
        };
        let [src, max_lum, ks] = [p.tone[0], p.tone[1], p.tone[2]];
        let white = p.tone[3];
        let hdr = matches!(p.mode, ColorMode::Pq | ColorMode::Hlg);
        let (knee, peak, top) = if ks < 1.0 {
            (pq_eotf(ks * src), pq_eotf(src), pq_eotf(max_lum * src))
        } else {
            // No compression: the EETF is the identity up to the peak, flat above it.
            let peak = pq_eotf(src);
            (peak, peak, peak)
        };
        let compress = hdr && peak > knee;
        let span = (peak - knee).max(f32::MIN_POSITIVE);
        let eetf = if compress {
            table(EETF_ENTRIES, |i| {
                let m = if i + 1 == EETF_ENTRIES { peak } else { knee + span * i as f32 / (EETF_ENTRIES - 1) as f32 };
                eetf(m, [src, max_lum, ks]) / m / white
            })
        } else {
            Vec::new()
        };
        let encode = if p.mode == ColorMode::None {
            Vec::new()
        } else {
            (0..ENCODE_ENTRIES)
                .map(|i| {
                    let s = i as f64 / (ENCODE_ENTRIES - 1) as f64;
                    ((s * s).powf(1.0 / SDR_GAMMA as f64) * 255.0).round() as u8
                })
                .collect()
        };
        Self {
            params: *p,
            decode: fixed(decode),
            gain: fixed(gain),
            knee,
            peak,
            top,
            eetf_steps_per_nit: if compress { (EETF_ENTRIES - 1) as f32 / span } else { 0.0 },
            eetf: fixed(eetf),
            white_inv: 1.0 / white,
            encode: fixed(encode),
        }
    }

    /// The parameters the tables were built for.
    pub fn params(&self) -> &ColorParams {
        &self.params
    }

    #[inline(always)]
    fn decode(&self, v: f32) -> f32 {
        let x = v.clamp(0.0, 1.0) * DECODE_STEPS as f32;
        let i = (x as usize).min(DECODE_STEPS - 1);
        let (a, b) = (self.decode[i], self.decode[i + 1]);
        a + (b - a) * (x - i as f32)
    }

    #[inline(always)]
    fn encode(&self, v: f32) -> u8 {
        // 65536 entries: every u16 is an index.
        self.encode[(v.clamp(0.0, 1.0).sqrt() * (ENCODE_ENTRIES - 1) as f32 + 0.5) as u16 as usize]
    }

    #[inline(always)]
    fn gamut(&self, c: [f32; 3]) -> [f32; 3] {
        let g = &self.params.gamut;
        [
            g[0][0] * c[0] + g[0][1] * c[1] + g[0][2] * c[2],
            g[1][0] * c[0] + g[1][1] * c[1] + g[1][2] * c[2],
            g[2][0] * c[0] + g[2][1] * c[1] + g[2][2] * c[2],
        ]
    }

    /// The EETF scale (divided by SDR white) for a largest component of `m` nits.
    #[inline(always)]
    fn scale(&self, m: f32) -> f32 {
        if m < self.knee {
            self.white_inv
        } else if m >= self.peak {
            if m > 0.0 { self.top / m * self.white_inv } else { 0.0 }
        } else {
            let x = (m - self.knee) * self.eetf_steps_per_nit;
            let i = (x as usize).min(EETF_ENTRIES - 2);
            let (a, b) = (self.eetf[i], self.eetf[i + 1]);
            a + (b - a) * (x - i as f32)
        }
    }

    /// Non-linear source R'G'B' (after the YUV matrix) → display R'G'B' codes.
    #[inline(always)]
    pub fn map(&self, rgb: [f32; 3]) -> [u8; 3] {
        match self.params.mode {
            ColorMode::None => rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8),
            ColorMode::Gamut => {
                let lin = self.gamut(rgb.map(|v| self.decode(v)));
                lin.map(|v| self.encode(v))
            }
            ColorMode::Pq | ColorMode::Hlg => {
                let mut light = rgb.map(|v| self.decode(v));
                if self.params.mode == ColorMode::Hlg {
                    let l = self.params.luma;
                    let ys = (light[0] * l[0] + light[1] * l[1] + light[2] * l[2]).clamp(0.0, 1.0);
                    let x = ys.sqrt() * GAIN_STEPS as f32;
                    let i = (x as usize).min(GAIN_STEPS - 1);
                    let (a, b) = (self.gain[i], self.gain[i + 1]);
                    let gain = a + (b - a) * (x - i as f32);
                    light = light.map(|v| v * gain);
                }
                let lin = self.gamut(light).map(|v| v.max(0.0));
                let m = lin[0].max(lin[1]).max(lin[2]);
                if m <= 0.0 {
                    return [0; 3];
                }
                let s = self.scale(m);
                lin.map(|v| self.encode(v * s))
            }
        }
    }
}

fn table(n: usize, f: impl Fn(usize) -> f32) -> Vec<f32> {
    (0..n).map(f).collect()
}

/// `v` as a fixed-size table (zeros when `v` is empty: a table the mode does not use).
fn fixed<T: Copy + Default, const N: usize>(mut v: Vec<T>) -> Box<[T; N]> {
    v.resize(N, T::default());
    v.into_boxed_slice().try_into().unwrap_or_else(|_| unreachable!("resized to N"))
}

/// The R'G'B' (0..1, clamped) of pixel (`x`, `y`) of the coded picture of `img`, converted the way the UI's shader
/// converts it before `color_map`, with the nearest chroma sample (no chroma interpolation). A reference for tests of
/// the GPU and the CPU conversion.
pub fn yuv_to_rgb_reference(img: &PlanarImage, x: usize, y: usize) -> [f32; 3] {
    let (range, k, scale) = yuv_uniforms(img);
    let wide = img.bits > 8;
    let sb = img.sample_bytes();
    let sample = |plane: usize, px: usize, py: usize, comp: usize, comps: usize| -> f32 {
        let (d, info) = img.plane(plane).expect("plane");
        let o = py * info.stride + (px * comps + comp) * sb;
        if wide { u16::from_le_bytes([d[o], d[o + 1]]) as f32 * scale } else { d[o] as f32 / 255.0 }
    };
    if img.layout == PlaneLayout::Packed {
        return [0, 1, 2].map(|c| sample(0, x, y, c, 4));
    }
    let (sx, sy) = img.chroma_shift;
    let (cx, cy) = (x >> sx, y >> sy);
    let luma = sample(0, x, y, 0, 1);
    let (u, v) = match img.layout {
        PlaneLayout::SemiPlanar if img.swap_uv => (sample(1, cx, cy, 1, 2), sample(1, cx, cy, 0, 2)),
        PlaneLayout::SemiPlanar => (sample(1, cx, cy, 0, 2), sample(1, cx, cy, 1, 2)),
        PlaneLayout::Planar => (sample(1, cx, cy, 0, 1), sample(2, cx, cy, 0, 1)),
        _ => (range[2], range[2]),
    };
    let yv = (luma - range[0]) * range[1];
    let (u, v) = ((u - range[2]) * range[3], (v - range[2]) * range[3]);
    [yv + k[0] * v, yv - k[1] * u - k[2] * v, yv + k[3] * u].map(|c| c.clamp(0.0, 1.0))
}

/// `(range, coefficients, sample scale)` uniforms of the YUV → R'G'B' step: normalized sample = bytes × scale (16-bit)
/// or the texel (8-bit); `(v - offset) × gain` per limited/full range (`range` = y offset, y gain, chroma offset,
/// chroma gain); RGB from the matrix's `(kr, kb)` (`coefficients` = r.v, g.u, g.v, b.u).
pub fn yuv_uniforms(img: &PlanarImage) -> ([f32; 4], [f32; 4], f32) {
    let bits = img.bits.clamp(8, 16) as u32;
    // The value range the normalized sample covers.
    let (max, scale) = if img.bits <= 8 {
        (255.0f32, 1.0)
    } else if img.msb_aligned {
        // The value sits in the high bits: normalized against the 16-bit range, same ratios.
        (65535.0, 1.0 / 65535.0)
    } else {
        let m = ((1u32 << bits) - 1) as f32;
        (m, 1.0 / m)
    };
    let range = if img.full_range || img.layout == PlaneLayout::Packed {
        [0.0, 1.0, 0.5 * (max + 1.0) / max, 1.0]
    } else {
        // 16..235 / 16..240 at 8 bits, shifted up for deeper samples.
        let unit = (max + 1.0) / 256.0;
        [16.0 * unit / max, max / (219.0 * unit), 128.0 * unit / max, max / (224.0 * unit)]
    };
    let (kr, kb) = img.matrix.coefficients();
    let kg = 1.0 - kr - kb;
    let rv = 2.0 * (1.0 - kr);
    let bu = 2.0 * (1.0 - kb);
    let gu = bu * kb / kg;
    let gv = rv * kr / kg;
    (range, [rv, gu, gv, bu], scale)
}

/// GLSL float literal (always with a decimal point or exponent: GLSL ES has no implicit int → float conversion).
fn lit(v: f32) -> String {
    let s = format!("{v:?}");
    if s.contains(['.', 'e', 'E']) { s } else { format!("{s}.0") }
}

/// The shader functions of this module: uniforms `u_color` (a [`ColorMode`]), `u_gamut`, `u_tone`, `u_luma` and
/// `vec3 color_map(vec3 rgb)` (non-linear source R'G'B' → display R'G'B'), with the constants above.
pub fn glsl() -> String {
    format!(
        r#"
uniform int u_color;
uniform mat3 u_gamut;
uniform vec4 u_tone;   // EETF source peak (PQ), normalized target peak, knee; SDR white (nits)
uniform vec3 u_luma;

float pq_eotf(float e) {{
    float p = pow(max(e, 0.0), 1.0 / {m2});
    return {max_nits} * pow(max(p - {c1}, 0.0) / ({c2} - {c3} * p), 1.0 / {m1});
}}

float pq_inverse_eotf(float nits) {{
    float y = pow(max(nits / {max_nits}, 0.0), {m1});
    return pow(({c1} + {c2} * y) / (1.0 + {c3} * y), {m2});
}}

float hlg_inverse_oetf(float e) {{
    e = max(e, 0.0);
    return e <= 0.5 ? e * e / 3.0 : (exp((e - {hc}) / {ha}) + {hb}) / 12.0;
}}

float eetf(float nits) {{
    float src = u_tone.x;
    float max_lum = u_tone.y;
    float ks = u_tone.z;
    float e1 = clamp(pq_inverse_eotf(nits) / src, 0.0, 1.0);
    float e2 = e1;
    if (e1 >= ks && ks < 1.0) {{
        float t = (e1 - ks) / (1.0 - ks);
        float t2 = t * t;
        float t3 = t2 * t;
        e2 = (2.0 * t3 - 3.0 * t2 + 1.0) * ks + (t3 - 2.0 * t2 + t) * (1.0 - ks) + (-2.0 * t3 + 3.0 * t2) * max_lum;
    }}
    return pq_eotf(e2 * src);
}}

vec3 encode_sdr(vec3 c) {{
    return pow(clamp(c, 0.0, 1.0), vec3(1.0 / {gamma}));
}}

vec3 color_map(vec3 rgb) {{
    rgb = clamp(rgb, 0.0, 1.0);
    if (u_color == 1) {{
        return encode_sdr(u_gamut * pow(rgb, vec3({gamma})));
    }}
    vec3 nits;
    if (u_color == 2) {{
        nits = vec3(pq_eotf(rgb.r), pq_eotf(rgb.g), pq_eotf(rgb.b));
    }} else {{
        vec3 scene = vec3(hlg_inverse_oetf(rgb.r), hlg_inverse_oetf(rgb.g), hlg_inverse_oetf(rgb.b));
        float ys = max(dot(scene, u_luma), 0.0);
        nits = scene * ({hlg_peak} * pow(max(ys, 1e-6), {hlg_gamma} - 1.0));
    }}
    vec3 lin = max(u_gamut * nits, 0.0);
    float m = max(lin.r, max(lin.g, lin.b));
    float scale = m > 0.0 ? eetf(m) / m : 0.0;
    return encode_sdr(lin * (scale / u_tone.w));
}}
"#,
        m1 = lit(PQ_M1),
        m2 = lit(PQ_M2),
        c1 = lit(PQ_C1),
        c2 = lit(PQ_C2),
        c3 = lit(PQ_C3),
        max_nits = lit(PQ_MAX_NITS),
        ha = lit(HLG_A),
        hb = lit(HLG_B),
        hc = lit(HLG_C),
        gamma = lit(SDR_GAMMA),
        hlg_peak = lit(HLG_PEAK_NITS),
        hlg_gamma = lit(HLG_SYSTEM_GAMMA),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn pq_reference_points() {
        // BT.2408: 203 nits (HDR reference white) is PQ 0.58; 100 nits 0.508; 1000 nits 0.752; 10000 nits 1.0.
        assert!(close(pq_eotf(0.58), 203.0, 2.5), "{}", pq_eotf(0.58));
        assert!(close(pq_inverse_eotf(203.0), 0.5806, 0.001), "{}", pq_inverse_eotf(203.0));
        assert!(close(pq_inverse_eotf(100.0), 0.5081, 0.001));
        assert!(close(pq_inverse_eotf(1000.0), 0.7518, 0.001));
        assert!(close(pq_eotf(1.0), 10000.0, 0.5));
        assert_eq!(pq_eotf(0.0), 0.0);
        for n in [0.01f32, 1.0, 48.0, 203.0, 600.0, 4000.0] {
            assert!(close(pq_eotf(pq_inverse_eotf(n)), n, n * 1e-3), "{n}");
        }
    }

    #[test]
    fn hlg_reference_points() {
        // BT.2100 inverse OETF: 0.5 → 1/12; 1.0 → 1.0 (continuous at 0.5).
        assert!(close(hlg_inverse_oetf(0.5), 1.0 / 12.0, 1e-6));
        assert!(close(hlg_inverse_oetf(0.500_001), 1.0 / 12.0, 1e-5));
        assert!(close(hlg_inverse_oetf(1.0), 1.0, 1e-4));
        // BT.2408: 75 % HLG is reference white, 203 nits on a 1000-nit display.
        let luma = luminance_coefficients(Primaries::Bt2020);
        let w = hlg_inverse_oetf(0.75);
        let d = hlg_ootf([w, w, w], luma);
        assert!(close(d[1], 203.0, 1.5), "{}", d[1]);
        let peak = hlg_ootf([1.0; 3], luma);
        assert!(close(peak[0], 1000.0, 0.5));
    }

    #[test]
    fn gamut_matrices_match_bt2087() {
        let m = gamut_matrix(Primaries::Bt2020, Primaries::Bt709);
        let want = [[1.6605, -0.5876, -0.0728], [-0.1246, 1.1329, -0.0083], [-0.0182, -0.1006, 1.1187]];
        for i in 0..3 {
            for j in 0..3 {
                assert!(close(m[i][j], want[i][j], 2e-4), "{i}{j}: {}", m[i][j]);
            }
            // White stays white.
            assert!(close(m[i].iter().sum(), 1.0, 1e-5));
        }
        let id = gamut_matrix(Primaries::Bt709, Primaries::Bt709);
        for i in 0..3 {
            for j in 0..3 {
                assert!(close(id[i][j], if i == j { 1.0 } else { 0.0 }, 1e-6));
            }
        }
        let l = luminance_coefficients(Primaries::Bt709);
        assert!(close(l[0], 0.2126, 1e-4) && close(l[1], 0.7152, 1e-4) && close(l[2], 0.0722, 1e-4));
        let l = luminance_coefficients(Primaries::Bt2020);
        assert!(close(l[0], 0.2627, 1e-4) && close(l[1], 0.6780, 1e-4) && close(l[2], 0.0593, 1e-4));
    }

    #[test]
    fn the_eetf_keeps_darks_and_compresses_highlights_monotonically() {
        let p = eetf_params(1000.0);
        // Below the knee (~95 nits for 1000 → 203) nothing changes.
        for n in [0.0f32, 1.0, 20.0, 80.0] {
            assert!(close(eetf(n, p), n, n * 2e-3 + 1e-4), "{n} → {}", eetf(n, p));
        }
        assert!(close(eetf(1000.0, p), SDR_WHITE_NITS, 0.5));
        assert!(close(eetf(5000.0, p), SDR_WHITE_NITS, 0.5), "above the peak clips at the target");
        let mut prev = 0.0;
        for i in 0..=200 {
            let v = eetf(i as f32 * 5.0, p);
            assert!(v + 1e-3 >= prev, "monotonic at {}", i * 5);
            assert!(v <= SDR_WHITE_NITS + 0.5);
            prev = v;
        }
        // Diffuse white of a 1000-nit grade stays clearly brighter than mid grey.
        assert!(eetf(203.0, p) > 140.0);
        // A source no brighter than SDR white is not compressed.
        let flat = eetf_params(150.0);
        assert_eq!(flat[2], 1.0);
        assert!(close(eetf(120.0, flat), 120.0, 0.2));
    }

    #[test]
    fn the_peak_comes_from_max_cll_then_mastering_then_the_default() {
        assert_eq!(hdr_peak_nits(Some(1000.0), Some(4000.0)), 1000.0);
        assert_eq!(hdr_peak_nits(None, Some(4000.0)), 4000.0);
        assert_eq!(hdr_peak_nits(Some(0.0), Some(4000.0)), 4000.0);
        assert_eq!(hdr_peak_nits(None, None), DEFAULT_PEAK_NITS);
        assert_eq!(hdr_peak_nits(Some(50.0), None), SDR_WHITE_NITS);
        assert_eq!(hdr_peak_nits(Some(f32::NAN), Some(20000.0)), PQ_MAX_NITS);
    }

    #[test]
    fn sdr_bt709_is_left_alone_and_wide_sdr_gets_only_the_gamut() {
        let p = ColorParams::new(Transfer::Sdr, Primaries::Bt709, DEFAULT_PEAK_NITS);
        assert_eq!(p.mode, ColorMode::None);
        assert_eq!(p.apply([0.25, 0.5, 0.75]), [0.25, 0.5, 0.75]);
        let g = ColorParams::new(Transfer::Sdr, Primaries::Bt2020, DEFAULT_PEAK_NITS);
        assert_eq!(g.mode, ColorMode::Gamut);
        // Grey and white are unchanged by a gamut conversion between D65 spaces.
        for v in [0.0f32, 0.3, 1.0] {
            let out = g.apply([v; 3]);
            assert!(out.iter().all(|c| close(*c, v, 1e-4)), "{out:?}");
        }
        // A BT.2020 pure green is outside BT.709: it clips to a 709 green (red and blue at 0).
        let out = g.apply([0.0, 1.0, 0.0]);
        assert!(out[0] == 0.0 && out[1] > 0.95 && out[2] == 0.0, "{out:?}");
    }

    #[test]
    fn hdr10_tone_mapping_lands_reference_white_bright_and_keeps_hue() {
        let p = ColorParams::new(Transfer::Pq, Primaries::Bt2020, 1000.0);
        let white = p.apply([0.58; 3]);
        // 203 nits → about 160 nits after the EETF → ~0.9 after the SDR encode: bright, not grey.
        assert!(white.iter().all(|c| *c > 0.85 && *c < 0.95), "{white:?}");
        let peak = p.apply([0.7518; 3]);
        assert!(peak.iter().all(|c| close(*c, 1.0, 2e-3)), "{peak:?}");
        // 75 % bars of a PQ test pattern in 2020 primaries: hue survives (red stays the largest channel, etc.).
        let red = p.apply([0.6, 0.3, 0.2]);
        assert!(red[0] > red[1] && red[1] >= red[2], "{red:?}");
        // Black stays black.
        assert_eq!(p.apply([0.0; 3]), [0.0; 3]);
    }

    #[test]
    fn hlg_reference_white_maps_like_pq_reference_white() {
        let hlg = ColorParams::new(Transfer::Hlg, Primaries::Bt2020, 0.0).apply([0.75; 3]);
        let pq = ColorParams::new(Transfer::Pq, Primaries::Bt2020, 1000.0).apply([0.58; 3]);
        for k in 0..3 {
            assert!(close(hlg[k], pq[k], 0.01), "{hlg:?} vs {pq:?}");
        }
    }

    #[test]
    fn the_glsl_uses_the_rust_constants_as_float_literals() {
        let src = glsl();
        assert!(src.contains(&lit(PQ_M1)) && src.contains(&lit(PQ_C3)) && src.contains(&lit(HLG_A)));
        assert!(src.contains("10000.0"), "integer-valued constants keep a decimal point");
        assert_eq!(lit(2.0), "2.0");
        assert_eq!(lit(1e-7), "1e-7");
        assert!(!src.contains("{{"));
    }

    /// Deterministic pseudo-random numbers in 0..1 (xorshift64*).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40) as f32 / (1u64 << 24) as f32
        }
    }

    /// `ToneLuts::map` against `round(apply(rgb) * 255)` over a 33³ grid and 10 000 pseudo-random points, for every
    /// mode the CPU path uses: (worst difference, worst difference where the reference code is above 2, mean).
    fn lut_error(p: &ColorParams) -> (u8, u8, f64) {
        let luts = ToneLuts::new(p);
        let mut points = Vec::new();
        for r in 0..33 {
            for g in 0..33 {
                for b in 0..33 {
                    points.push([r, g, b].map(|v| v as f32 / 32.0));
                }
            }
        }
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..10_000 {
            points.push([rng.next(), rng.next(), rng.next()]);
        }
        let (mut worst, mut worst_lit, mut sum, mut n) = (0u8, 0u8, 0u64, 0u64);
        for rgb in points {
            let want = p.apply(rgb).map(|v| (v * 255.0).round() as u8);
            let got = luts.map(rgb);
            for k in 0..3 {
                let d = got[k].abs_diff(want[k]);
                worst = worst.max(d);
                if want[k] > 2 {
                    worst_lit = worst_lit.max(d);
                }
                sum += d as u64;
                n += 1;
            }
        }
        (worst, worst_lit, sum as f64 / n as f64)
    }

    #[test]
    fn the_lookup_tables_match_apply_to_within_one_code() {
        let mut cases = Vec::new();
        for peak in [150.0f32, 203.0, 1000.0, 1200.0, 4000.0, 10000.0] {
            // `hdr_peak_nits` never gives less than SDR white; 150 checks the uncompressed case all the same.
            cases.push((format!("PQ {peak}"), ColorParams::new(Transfer::Pq, Primaries::Bt2020, peak)));
        }
        cases.push(("HLG".into(), ColorParams::new(Transfer::Hlg, Primaries::Bt2020, 0.0)));
        cases.push(("PQ P3".into(), ColorParams::new(Transfer::Pq, Primaries::DisplayP3, 1000.0)));
        cases.push(("gamut BT.2020".into(), ColorParams::new(Transfer::Sdr, Primaries::Bt2020, DEFAULT_PEAK_NITS)));
        cases.push(("gamut P3".into(), ColorParams::new(Transfer::Sdr, Primaries::DisplayP3, DEFAULT_PEAK_NITS)));
        cases.push(("none".into(), ColorParams::new(Transfer::Sdr, Primaries::Bt709, DEFAULT_PEAK_NITS)));
        let mut report = Vec::new();
        for (name, p) in &cases {
            let (worst, worst_lit, mean) = lut_error(p);
            eprintln!("{name}: worst {worst}, worst above code 2 {worst_lit}, mean {mean:.4}");
            report.push((name.clone(), worst, worst_lit, mean));
        }
        for (name, worst, worst_lit, mean) in report {
            // Measured: worst 1 everywhere (dark codes included), mean at most 0.0012.
            assert!(worst_lit <= 1 && worst <= 1 && mean <= 0.01, "{name}: worst {worst} ({worst_lit} above code 2), mean {mean}");
        }
    }

    #[test]
    fn the_lookup_tables_are_rebuilt_only_for_other_parameters() {
        let a = ColorParams::new(Transfer::Pq, Primaries::Bt2020, 1000.0);
        let b = ColorParams::new(Transfer::Pq, Primaries::Bt2020, 1200.0);
        assert_eq!(ToneLuts::new(&a).params(), &a);
        assert_ne!(&a, &b, "a later MaxCLL changes the parameters (the cache key)");
        assert_eq!(ToneLuts::new(&a).map([0.0; 3]), [0; 3]);
        assert_eq!(ToneLuts::new(&a).map([1.0; 3]), [255; 3]);
    }

    #[test]
    fn the_gamut_uniform_is_column_major() {
        let p = ColorParams::new(Transfer::Pq, Primaries::Bt2020, 1000.0);
        let c = p.gamut_column_major();
        assert_eq!((c[1], c[3]), (p.gamut[1][0], p.gamut[0][1]));
    }
}
