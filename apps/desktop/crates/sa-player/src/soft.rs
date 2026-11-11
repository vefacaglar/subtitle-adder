//! The CPU picture path of the software renderer (RGBA frames): deinterlacing and the YUV → R'G'B' conversion with
//! HDR tone mapping, mirroring the UI's GPU shader (`sa-app/src/main_window/video_gl.rs`), so a frame looks the same
//! whichever renderer shows it. Pure Rust, compiled without the `ffmpeg` feature; the decode thread
//! (`ffmpeg_backend`) decides which frames take it: HDR and wide-gamut frames are deinterlaced (when interlaced) and
//! converted here; interlaced SDR frames are deinterlaced here and converted by swscale (formats without a planar
//! description: swscale's RGBA, deinterlaced and scaled here); plain SDR progressive frames keep swscale alone.
//!
//! * [`deinterlace_planes`]: the shader's `deinterlaced()` on the planes, for the field due: the shown field's rows are
//!   copied; each row of the other field is woven in where it did not change since the previous frame and replaced by
//!   the average of the rows above and below where it did (`smoothstep` between 1.2 % and 4 % of the sample range of
//!   the motion measure); without a previous frame it is always the average (bob). Edge rows are mirrored like the
//!   shader's. Chroma planes with a vertical subsampling of 1 or 2 rows alternate between the fields row by row as the
//!   shader's `fetch_row` assigns them (4:2:0 chroma row `c` belongs to field `c % 2`); 4:1:0 chroma (4 rows) is copied
//!   as it is (the shader's known limit too).
//! * [`convert_rgba`]: the shader's `to_rgb` + `color_map` per output pixel: luma straight from its row (unscaled) or
//!   linearly interpolated across the sample-aspect stretch, chroma bilinear (two chroma rows interpolated once per
//!   output row, then horizontally per pixel) or, for a deinterlaced 4:2:0 frame, from the same-field chroma row the
//!   shader picks; the colour mapping through [`ToneLuts`]. Output rows are split into stripes (one per 0.25 Mpx,
//!   up to the hardware threads minus two, at most 8) run on a persistent [`StripePool`]; a panicking stripe makes
//!   that frame convert again on the calling thread instead of unwinding into the decode thread. Frames above 1440p
//!   ([`HALF_SIZE_ABOVE`]) are converted at half width and height ([`ConvertOptions::half`]: 2 × 2 luma averaged in the
//!   signal domain, chroma sampled at the output resolution, one colour mapping per output pixel), so 4K costs what
//!   1080p costs; the software renderer scales the picture to the window anyway.
//!
//! Known deviations from the shader (small, documented for the comparison of frame dumps):
//! * The motion measure is taken per sample of each plane (interleaved U and V separately), where the shader takes the
//!   largest change over Y, U and V of a pixel; a pixel whose luma is static but whose chroma moved can therefore keep
//!   woven luma here.
//! * Deinterlacing runs before the chroma upsampling (on the chroma rows), the shader after it (on the sampled
//!   colour); for moving 4:2:0 content the interpolated chroma row lies half a chroma row apart from the shader's.
//! * 8-bit samples are interpolated in f32 (the GPU filters with fixed-point weights): within one code.

use std::sync::OnceLock;

use crate::color::{ToneLuts, yuv_uniforms};
use crate::{PlanarImage, PlaneLayout};

/// Whether a frame at `previous` pts is the one right before a frame at `pts` (for motion detection): a step forward
/// of at most 100 ms (a frame or two of interlaced video), not a seek or a new source. Shared by the GPU and the CPU
/// deinterlacer.
pub fn previous_frame_follows(previous: f64, pts: f64) -> bool {
    let d = pts - previous;
    d > 0.0 && d <= 0.1
}

/// One writable output plane: rows `stride` bytes apart, each at least the source plane's `row_bytes` long.
pub struct PlaneMut<'a> {
    pub data: &'a mut [u8],
    pub stride: usize,
}

/// Motion below this share of the sample range keeps the other field's row (woven) ...
const MOTION_LO: f32 = 0.012;
/// ... and above this one replaces it by the interpolation (the shader's `smoothstep(0.012, 0.04, m)`).
const MOTION_HI: f32 = 0.04;

/// The value range a sample of `img` is normalized against (what the shader divides by).
fn sample_range(img: &PlanarImage) -> f32 {
    if img.bits <= 8 {
        255.0
    } else if img.msb_aligned {
        65535.0
    } else {
        ((1u32 << img.bits.min(16)) - 1) as f32
    }
}

trait Sample {
    const BYTES: usize;
    fn get(b: &[u8], i: usize) -> i32;
    fn put(b: &mut [u8], i: usize, v: i32);
}

impl Sample for u8 {
    const BYTES: usize = 1;
    #[inline(always)]
    fn get(b: &[u8], i: usize) -> i32 {
        b[i] as i32
    }
    #[inline(always)]
    fn put(b: &mut [u8], i: usize, v: i32) {
        b[i] = v as u8;
    }
}

impl Sample for u16 {
    const BYTES: usize = 2;
    #[inline(always)]
    fn get(b: &[u8], i: usize) -> i32 {
        u16::from_le_bytes([b[2 * i], b[2 * i + 1]]) as i32
    }
    #[inline(always)]
    fn put(b: &mut [u8], i: usize, v: i32) {
        b[2 * i..2 * i + 2].copy_from_slice(&(v as u16).to_le_bytes());
    }
}

/// One row of the other field: `own` (the row itself), `a` / `b` (the shown rows above and below), `prev` the same
/// three rows of the previous frame. `lo2` / `hi2`: twice the motion thresholds in sample units.
fn blend_row<S: Sample>(own: &[u8], a: &[u8], b: &[u8], prev: Option<(&[u8], &[u8], &[u8])>, out: &mut [u8], lo2: f32, hi2: f32) {
    let n = own.len() / S::BYTES;
    match prev {
        None => {
            for i in 0..n {
                S::put(out, i, (S::get(a, i) + S::get(b, i) + 1) >> 1);
            }
        }
        Some((po, pa, pb)) => {
            let inv = 1.0 / (hi2 - lo2);
            for i in 0..n {
                let (o, x, y) = (S::get(own, i), S::get(a, i), S::get(b, i));
                // 2 × max(|own − prev own|, ½ (|a − prev a| + |b − prev b|)).
                let m2 = (2 * (o - S::get(po, i)).abs()).max((x - S::get(pa, i)).abs() + (y - S::get(pb, i)).abs()) as f32;
                let v = if m2 <= lo2 {
                    o
                } else if m2 >= hi2 {
                    (x + y + 1) >> 1
                } else {
                    let t = (m2 - lo2) * inv;
                    let t = t * t * (3.0 - 2.0 * t);
                    let of = o as f32;
                    (of + (0.5 * (x + y) as f32 - of) * t + 0.5) as i32
                };
                S::put(out, i, v);
            }
        }
    }
}

/// Deinterlaces `cur` for the field shown (`bottom`: the odd rows) into `out` (one plane per plane of `cur`, same
/// rows and row lengths), against `prev` (the previous frame as decoded, not deinterlaced; ignored unless it has the
/// same format and size). See the module docs.
pub fn deinterlace_planes(cur: &PlanarImage, prev: Option<&PlanarImage>, bottom: bool, out: &mut [PlaneMut<'_>]) {
    let prev = prev.filter(|p| p.format() == cur.format() && (p.coded_width, p.coded_height) == (cur.coded_width, cur.coded_height));
    let field = bottom as usize;
    let range = sample_range(cur);
    let (lo2, hi2) = (2.0 * MOTION_LO * range, 2.0 * MOTION_HI * range);
    let wide = cur.sample_bytes() == 2;
    for (i, o) in out.iter_mut().enumerate().take(cur.plane_count()) {
        let Some((data, info)) = cur.plane(i) else { continue };
        let pp = prev.and_then(|p| p.plane(i)).filter(|(_, pi)| pi.rows == info.rows && pi.row_bytes == info.row_bytes);
        let rows = info.rows as usize;
        let rb = info.row_bytes;
        // Luma always; chroma unless it spans four luma rows (4:1:0).
        let by_field = i == 0 || cur.chroma_shift.1 <= 1;
        for r in 0..rows {
            let start = r * o.stride;
            let dst = &mut o.data[start..start + rb];
            fn row(d: &[u8], stride: usize, k: usize, len: usize) -> &[u8] {
                &d[k * stride..k * stride + len]
            }
            let row = |d, stride, k| row(d, stride, k, rb);
            let own = row(data, info.stride, r);
            if !by_field || rows < 2 || r % 2 == field {
                dst.copy_from_slice(own);
                continue;
            }
            let up = if r == 0 { r + 1 } else { r - 1 };
            let dn = if r + 1 >= rows { r - 1 } else { r + 1 };
            let (a, b) = (row(data, info.stride, up), row(data, info.stride, dn));
            let p = pp.map(|(pd, pi)| (row(pd, pi.stride, r), row(pd, pi.stride, up), row(pd, pi.stride, dn)));
            if wide {
                blend_row::<u16>(own, a, b, p, dst, lo2, hi2);
            } else {
                blend_row::<u8>(own, a, b, p, dst, lo2, hi2);
            }
        }
    }
}


/// Frames above this many pixels are converted at half width and half height on the kernel route
/// ([`ConvertOptions::half`]): 2560 × 1440. At the measured ~6.5 ns per pixel for PQ on four threads (HLG ~10), a
/// 1440p frame takes ~24 ms (HLG ~37 ms), which still fits 24-30 fps; a 4K frame would take ~54 ms (HLG ~83), more than
/// a 24 fps frame lasts. The software renderer scales the picture to the window anyway, which at 4K is nearly always
/// smaller than the video.
pub const HALF_SIZE_ABOVE: usize = 2560 * 1440;

/// Whether a frame of `pixels` (display size) is converted at half size on the kernel route.
pub fn half_size(pixels: usize) -> bool {
    pixels > HALF_SIZE_ABOVE
}

/// The most stripes a conversion is split into on this machine: the hardware threads minus two (the decoder's
/// threads and the UI), 1..=8.
pub fn max_stripes() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| (std::thread::available_parallelism().map_or(1, |n| n.get()).saturating_sub(2)).clamp(1, 8))
}

/// How many stripes [`convert_rgba`] splits a picture of `pixels` output pixels into: one per 0.25 Mpx, at most
/// [`max_stripes`] (one below 0.5 Mpx).
pub fn stripes_for(pixels: usize) -> usize {
    (pixels / 250_000).clamp(1, max_stripes())
}

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Persistent worker threads for the stripes of [`convert_rgba`] (no thread is spawned per frame). `init` runs once on
/// every worker before its first job and what it returns lives as long as the worker (a thread-priority guard).
pub struct StripePool {
    jobs: Option<crossbeam_channel::Sender<Job>>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl StripePool {
    pub fn new(workers: usize, init: std::sync::Arc<dyn Fn() -> Box<dyn std::any::Any> + Send + Sync>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Job>();
        let workers = (0..workers)
            .filter_map(|i| {
                let rx = rx.clone();
                let init = init.clone();
                std::thread::Builder::new()
                    .name(format!("player-stripe-{i}"))
                    .spawn(move || {
                        let _state = init();
                        while let Ok(job) = rx.recv() {
                            job();
                        }
                    })
                    .ok()
            })
            .collect();
        Self { jobs: Some(tx), workers }
    }

    /// Worker threads (a run uses them plus the calling thread).
    pub fn workers(&self) -> usize {
        self.workers.len()
    }

    /// Runs `task(i)` for every `i` in `0..n`: `i ≥ 1` on the workers, 0 on this thread, and waits for all of them.
    /// False when a task panicked (the panic is caught; the other tasks still ran to completion).
    pub fn run(&self, n: usize, task: &(dyn Fn(usize) + Sync)) -> bool {
        let (done_tx, done_rx) = crossbeam_channel::bounded::<bool>(n.max(1));
        let mut sent = 0;
        let mut ok = true;
        for i in 1..n {
            // SAFETY: the borrow's lifetime is erased for the worker. This function does not return before every job
            // it sent has run (each reports through `done`, also when the task panics: `catch_unwind`), or has been
            // dropped unrun (its `done` sender is dropped with it, ending the wait below).
            let task_static: &'static (dyn Fn(usize) + Sync) = unsafe { std::mem::transmute(task) };
            let done = done_tx.clone();
            let job: Job = Box::new(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task_static(i))).is_ok();
                let _ = done.send(r);
            });
            match self.jobs.as_ref().map(|j| j.send(job)) {
                Some(Ok(())) => sent += 1,
                _ => ok &= std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(i))).is_ok(),
            }
        }
        drop(done_tx);
        ok &= std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(0))).is_ok();
        for _ in 0..sent {
            ok &= done_rx.recv().unwrap_or(false);
        }
        ok
    }
}

impl Drop for StripePool {
    fn drop(&mut self) {
        self.jobs = None;
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

/// A horizontal interpolation tap: samples `i0` and `i1` (edge-clamped), weight `f` of `i1`.
#[derive(Debug, Clone, Copy)]
struct Tap {
    i0: u32,
    i1: u32,
    f: f32,
}

/// Taps of an output row of `out` pixels over a plane row of `src` samples: output pixel centres mapped onto the
/// plane as the shader's normalized coordinates are (`(x + 0.5) / out × src − 0.5`), clamped to the edge.
fn taps(src: usize, out: usize) -> Vec<Tap> {
    let last = src.max(1) as i64 - 1;
    (0..out)
        .map(|x| {
            let p = (x as f64 + 0.5) * src as f64 / out as f64 - 0.5;
            let i = p.floor();
            let f = (p - i) as f32;
            let i = i as i64;
            Tap { i0: i.clamp(0, last) as u32, i1: (i + 1).clamp(0, last) as u32, f }
        })
        .collect()
}

#[inline(always)]
fn lerp(a: f32, b: f32, f: f32) -> f32 {
    a + (b - a) * f
}

struct Kernel<'a> {
    layout: PlaneLayout,
    luts: &'a ToneLuts,
    y: (&'a [u8], usize),
    /// Planar: U and V planes; semi-planar: the interleaved plane (twice).
    c: [(&'a [u8], usize); 2],
    /// Source luma width (samples per row; packed: pixels).
    w: usize,
    /// Output size.
    oh: usize,
    ow: usize,
    cw: usize,
    ch: usize,
    /// Half size: output row `y` is the average of luma rows `2y` and `2y + 1`, interpolated across the row like a
    /// scaled one (two samples per output pixel: their average).
    half: bool,
    luma_x: Option<Vec<Tap>>,
    chroma_x: Vec<Tap>,
    /// Normalized luma = sample × ya − yb, chroma = sample × ca − cb (`to_rgb`'s range step folded in).
    ya: f32,
    yb: f32,
    ca: f32,
    cb: f32,
    k: [f32; 4],
    swap: bool,
    chroma_fields: bool,
}

impl Kernel<'_> {
    /// The chroma (normalized U, V per chroma column) for output row `y`. At half size the same bilinear mapping
    /// lands 4:2:0 chroma rows exactly on the output rows and averages the two rows of 4:2:2 / 4:4:4 chroma.
    fn chroma_row<S: Sample>(&self, y: usize, buf: &mut [f32]) {
        let (c0, c1, fy) = if self.chroma_fields {
            // The shader's `fetch_row`: the nearest chroma row of the same field.
            let i = y / 2;
            let crow = (2 * (i / 2) + y % 2).min(self.ch - 1);
            (crow, crow, 0.0)
        } else {
            let p = (y as f64 + 0.5) * self.ch as f64 / self.oh as f64 - 0.5;
            let i = p.floor();
            let last = self.ch as i64 - 1;
            ((i as i64).clamp(0, last) as usize, (i as i64 + 1).clamp(0, last) as usize, (p - i) as f32)
        };
        let (ua, ub, va, vb, step, uo, vo);
        if self.layout == PlaneLayout::SemiPlanar {
            let (d, s) = self.c[0];
            (ua, ub) = (&d[c0 * s..], &d[c1 * s..]);
            (va, vb) = (ua, ub);
            step = 2;
            (uo, vo) = if self.swap { (1, 0) } else { (0, 1) };
        } else {
            let ((du, su), (dv, sv)) = (self.c[0], self.c[1]);
            (ua, ub) = (&du[c0 * su..], &du[c1 * su..]);
            (va, vb) = (&dv[c0 * sv..], &dv[c1 * sv..]);
            step = 1;
            (uo, vo) = (0, 0);
        }
        for cx in 0..self.cw {
            let (iu, iv) = (cx * step + uo, cx * step + vo);
            let u = lerp(S::get(ua, iu) as f32, S::get(ub, iu) as f32, fy);
            let v = lerp(S::get(va, iv) as f32, S::get(vb, iv) as f32, fy);
            buf[2 * cx] = u * self.ca - self.cb;
            buf[2 * cx + 1] = v * self.ca - self.cb;
        }
    }

    fn rows<S: Sample>(&self, y0: usize, out: &mut [u8]) {
        let ow = self.ow;
        let has_chroma = matches!(self.layout, PlaneLayout::Planar | PlaneLayout::SemiPlanar);
        let packed = self.layout == PlaneLayout::Packed;
        let comps = if packed { 4 } else { 1 };
        let mut cbuf = vec![0f32; self.cw * 2];
        // Half size: the sum of the two source rows (signal domain, exact integer sums), halved with the scale.
        let mut sums = if self.half { vec![0f32; self.w * comps] } else { Vec::new() };
        let [k0, k1, k2, k3] = self.k;
        let (yd, ys) = self.y;
        for (j, o) in out.chunks_exact_mut(ow * 4).enumerate() {
            let y = y0 + j;
            let yrow: &[u8] = if self.half { &yd[2 * y * ys..] } else { &yd[y * ys..] };
            if self.half {
                let next = &yd[(2 * y + 1) * ys..];
                for (i, s) in sums.iter_mut().enumerate() {
                    *s = if packed { (yrow[i] as u32 + next[i] as u32) as f32 } else { (S::get(yrow, i) + S::get(next, i)) as f32 };
                }
            }
            if packed {
                for x in 0..ow {
                    let c = match (&self.luma_x, self.half) {
                        (None, _) => [0, 1, 2].map(|k| yrow[4 * x + k] as f32),
                        (Some(t), false) => {
                            let t = t[x];
                            let (a, b) = (4 * t.i0 as usize, 4 * t.i1 as usize);
                            [0, 1, 2].map(|k| lerp(yrow[a + k] as f32, yrow[b + k] as f32, t.f))
                        }
                        (Some(t), true) => {
                            let t = t[x];
                            let (a, b) = (4 * t.i0 as usize, 4 * t.i1 as usize);
                            [0, 1, 2].map(|k| 0.5 * lerp(sums[a + k], sums[b + k], t.f))
                        }
                    };
                    let px = self.luts.map(c.map(|v| v * (1.0 / 255.0)));
                    o[4 * x..4 * x + 4].copy_from_slice(&[px[0], px[1], px[2], 255]);
                }
                continue;
            }
            if has_chroma {
                self.chroma_row::<S>(y, &mut cbuf);
            }
            for x in 0..ow {
                let yv = match (&self.luma_x, self.half) {
                    (None, _) => S::get(yrow, x) as f32,
                    (Some(t), false) => {
                        let t = t[x];
                        lerp(S::get(yrow, t.i0 as usize) as f32, S::get(yrow, t.i1 as usize) as f32, t.f)
                    }
                    (Some(t), true) => {
                        let t = t[x];
                        0.5 * lerp(sums[t.i0 as usize], sums[t.i1 as usize], t.f)
                    }
                };
                let l = yv * self.ya - self.yb;
                let (u, v) = if has_chroma {
                    let t = self.chroma_x[x];
                    let (a, b) = (2 * t.i0 as usize, 2 * t.i1 as usize);
                    (lerp(cbuf[a], cbuf[b], t.f), lerp(cbuf[a + 1], cbuf[b + 1], t.f))
                } else {
                    (0.0, 0.0)
                };
                let px = self.luts.map([l + k0 * v, l - k1 * u - k2 * v, l + k3 * u]);
                o[4 * x..4 * x + 4].copy_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
    }
}

/// How [`convert_rgba`] samples the picture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConvertOptions {
    /// The image is a deinterlaced 4:2:0 frame, whose chroma rows belong to alternating fields (sampled as the shader
    /// samples them while deinterlacing). Ignored at half size.
    pub chroma_fields: bool,
    /// Half size ([`half_size`]): `out_w` is half the display width and the output has half the coded rows (rounded
    /// down); each output pixel averages a 2 × 2 block of luma (across the sample-aspect stretch, if any), chroma is
    /// sampled at the output resolution, and the colour mapping runs once per output pixel.
    pub half: bool,
}

/// The output height of [`convert_rgba`] for `img`.
pub fn output_rows(img: &PlanarImage, opts: ConvertOptions) -> usize {
    let h = img.coded_height as usize;
    if opts.half { (h / 2).max(1) } else { h }
}

/// Converts `img` (unrotated) to RGBA8 at `out_w` × [`output_rows`] (the display size before rotation: only the
/// width carries the sample aspect ratio) into `dst` (tightly packed), with the colour mapping `luts` (built for
/// `ColorParams::for_image(img)`). Rows are split into [`stripes_for`] stripes run on `pool` (and this thread). A
/// stripe that panics makes the frame convert again on this thread alone; false when that panics too, when `dst` is
/// too small or a plane is missing.
pub fn convert_rgba(img: &PlanarImage, out_w: u32, luts: &ToneLuts, opts: ConvertOptions, dst: &mut [u8], pool: Option<&StripePool>) -> bool {
    let rows = output_rows(img, opts);
    convert_striped(img, out_w, luts, opts, dst, pool, stripes_for(out_w as usize * rows))
}

/// [`convert_rgba`] in `stripes` stripes (at most the pool's workers + 1).
fn convert_striped(img: &PlanarImage, out_w: u32, luts: &ToneLuts, opts: ConvertOptions, dst: &mut [u8], pool: Option<&StripePool>, stripes: usize) -> bool {
    let (w, h, ow) = (img.coded_width as usize, img.coded_height as usize, out_w as usize);
    let half = opts.half && h >= 2 && w >= 2;
    let oh = if half { h / 2 } else { h };
    if w == 0 || h == 0 || ow == 0 || dst.len() < ow * oh * 4 {
        return false;
    }
    let Some((yd, yi)) = img.plane(0) else { return false };
    let (range, k, scale) = yuv_uniforms(img);
    let packed = img.layout == PlaneLayout::Packed;
    let unit = if img.bits <= 8 { 1.0 / 255.0 } else { scale };
    let (mut cw, mut ch) = (1usize, 1usize);
    let mut c = [(yd, yi.stride); 2];
    match img.layout {
        PlaneLayout::Planar => {
            let (Some((ud, ui)), Some((vd, vi))) = (img.plane(1), img.plane(2)) else { return false };
            c = [(ud, ui.stride), (vd, vi.stride)];
            (cw, ch) = (ui.width as usize, ui.rows as usize);
        }
        PlaneLayout::SemiPlanar => {
            let Some((cd, ci)) = img.plane(1) else { return false };
            c = [(cd, ci.stride), (cd, ci.stride)];
            (cw, ch) = (ci.width as usize, ci.rows as usize);
        }
        PlaneLayout::Gray | PlaneLayout::Packed => {}
    }
    let kernel = Kernel {
        layout: img.layout,
        luts,
        y: (yd, yi.stride),
        c,
        w,
        oh,
        ow,
        cw: cw.max(1),
        ch: ch.max(1),
        half,
        luma_x: (ow != w || half).then(|| taps(w, ow)),
        chroma_x: taps(cw.max(1), ow),
        ya: if packed { 1.0 } else { unit * range[1] },
        yb: if packed { 0.0 } else { range[0] * range[1] },
        ca: unit * range[3],
        cb: range[2] * range[3],
        k,
        swap: img.swap_uv,
        chroma_fields: opts.chroma_fields && !half && matches!(img.layout, PlaneLayout::Planar | PlaneLayout::SemiPlanar) && img.chroma_shift.1 == 1,
    };
    let wide = img.bits > 8 && !packed;
    let run = |y0: usize, out: &mut [u8]| {
        if wide { kernel.rows::<u16>(y0, out) } else { kernel.rows::<u8>(y0, out) }
    };
    let dst = &mut dst[..ow * oh * 4];
    let stripes = pool.map_or(1, |p| stripes.min(p.workers() + 1)).clamp(1, oh);
    if stripes > 1
        && let Some(pool) = pool
    {
        let rows_per = oh.div_ceil(stripes);
        let chunks: Vec<std::sync::Mutex<&mut [u8]>> = dst.chunks_mut(rows_per * ow * 4).map(std::sync::Mutex::new).collect();
        let task = |i: usize| {
            let mut chunk = chunks[i].lock().unwrap_or_else(|e| e.into_inner());
            run(i * rows_per, &mut chunk);
        };
        if pool.run(chunks.len(), &task) {
            return true;
        }
        drop(chunks);
        eprintln!("[player] a conversion stripe panicked; converting the frame on one thread");
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(0, dst))).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{ColorParams, yuv_to_rgb_reference};
    use crate::{ColorInfo, Interlace, PlanarFormat, PlaneInfo, Primaries, Transfer, YuvMatrix};

    /// An in-memory image: `planes` = (bytes, stride, width in samples, rows); the vectors are owned by the image.
    fn image(format: PlanarFormat, size: (u32, u32), color: ColorInfo, interlaced: bool, planes: Vec<(Vec<u8>, usize, u32, u32)>) -> PlanarImage {
        let sb = if format.bits > 8 { 2 } else { 1 };
        let mut ptrs: [Option<(*const u8, PlaneInfo)>; 3] = [None; 3];
        for (i, (data, stride, width, rows)) in planes.iter().enumerate() {
            let row_bytes = *width as usize * format.components(i) * sb;
            assert!(data.len() >= (*rows as usize - 1) * stride + row_bytes);
            ptrs[i] = Some((data.as_ptr(), PlaneInfo { width: *width, rows: *rows, stride: *stride, row_bytes }));
        }
        let interlace = interlaced.then_some(Interlace { top_field_first: true });
        // SAFETY: the vectors move into the owner box; their heap buffers stay where they are.
        unsafe { PlanarImage::new(format, size, color, 0, interlace, ptrs, Box::new(planes)) }
    }

    fn gray8(w: u32, h: u32, px: impl Fn(usize, usize) -> u8) -> PlanarImage {
        let stride = w as usize + 3;
        let mut d = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                d[y * stride + x] = px(x, y);
            }
        }
        let f = PlanarFormat { layout: PlaneLayout::Gray, bits: 8, msb_aligned: false, chroma_shift: (0, 0), swap_uv: false };
        image(f, (w, h), ColorInfo::default(), true, vec![(d, stride, w, h)])
    }

    /// Output buffers shaped like `img`'s planes (tight rows).
    fn outputs(img: &PlanarImage) -> Vec<(Vec<u8>, usize)> {
        (0..img.plane_count())
            .map(|i| {
                let (_, info) = img.plane(i).unwrap();
                (vec![0xAAu8; info.row_bytes * info.rows as usize], info.row_bytes)
            })
            .collect()
    }

    fn run_deint(cur: &PlanarImage, prev: Option<&PlanarImage>, bottom: bool) -> Vec<(Vec<u8>, usize)> {
        let mut out = outputs(cur);
        let mut planes: Vec<PlaneMut> = out.iter_mut().map(|(d, s)| PlaneMut { data: d.as_mut_slice(), stride: *s }).collect();
        deinterlace_planes(cur, prev, bottom, &mut planes);
        out
    }

    /// Rows of the top field 0..100, of the bottom field 200..: a comb of two different pictures.
    fn combed(x: usize, y: usize) -> u8 {
        if y % 2 == 0 { (x * 3 + y) as u8 } else { 200 + (x + y) as u8 % 50 }
    }

    #[test]
    fn a_static_picture_is_woven_exactly() {
        let cur = gray8(16, 8, combed);
        let prev = gray8(16, 8, combed);
        for bottom in [false, true] {
            let out = run_deint(&cur, Some(&prev), bottom);
            for y in 0..8 {
                for x in 0..16 {
                    assert_eq!(out[0].0[y * 16 + x], combed(x, y), "bottom={bottom} ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn without_a_previous_frame_the_other_field_is_the_average_of_its_neighbours() {
        let cur = gray8(16, 8, combed);
        let out = run_deint(&cur, None, false);
        for y in 0..8 {
            for x in 0..16 {
                let want = if y % 2 == 0 {
                    combed(x, y) as u32
                } else {
                    // The last row mirrors: both neighbours are the row above.
                    let dn = if y + 1 < 8 { y + 1 } else { y - 1 };
                    (combed(x, y - 1) as u32 + combed(x, dn) as u32).div_ceil(2)
                };
                assert_eq!(out[0].0[y * 16 + x] as u32, want, "({x}, {y})");
            }
        }
        // Showing the bottom field: row 0 mirrors to row 1 on both sides.
        let out = run_deint(&cur, None, true);
        assert_eq!(out[0].0[3], combed(3, 1));
        assert_eq!(out[0].0[16 + 3], combed(3, 1));
    }

    #[test]
    fn motion_interpolates_and_small_changes_stay_woven() {
        let cur = gray8(16, 8, combed);
        // The other field moved a lot: interpolated.
        let moved = gray8(16, 8, |x, y| if y % 2 == 1 { 20 } else { combed(x, y) });
        let out = run_deint(&cur, Some(&moved), false);
        let avg = |x: usize, y: usize| (combed(x, y - 1) as u32 + combed(x, y + 1) as u32).div_ceil(2) as u8;
        assert_eq!(out[0].0[16 + 5], avg(5, 1));
        // A change of 2 codes (below 1.2 % of 255): woven.
        let noise = gray8(16, 8, |x, y| combed(x, y).wrapping_add(if y % 2 == 1 { 2 } else { 0 }));
        let out = run_deint(&cur, Some(&noise), false);
        assert_eq!(out[0].0[16 + 5], combed(5, 1));
        // In between (6 codes: m between 3.06 and 10.2): a blend strictly between the two.
        let some = gray8(16, 8, |x, y| combed(x, y).wrapping_add(if y % 2 == 1 { 6 } else { 0 }));
        let v = run_deint(&cur, Some(&some), false)[0].0[16 + 5];
        let (own, interp) = (combed(5, 1), avg(5, 1));
        assert!(v > own.min(interp) && v < own.max(interp), "{v} strictly between {own} and {interp}");
        // A previous frame of another size is not used (bob).
        let other = gray8(16, 6, combed);
        assert_eq!(run_deint(&cur, Some(&other), false)[0].0[16 + 5], avg(5, 1));
    }

    /// P010-like 16-bit samples with the value in the high bits, and NV12-like interleaved chroma (4:2:0).
    fn semi_planar(bits: u8, msb: bool, w: u32, h: u32, luma: impl Fn(usize, usize) -> u16, chroma: impl Fn(usize, usize, usize) -> u16) -> PlanarImage {
        let sb = if bits > 8 { 2 } else { 1 };
        let put = |d: &mut Vec<u8>, o: usize, v: u16| {
            if sb == 2 {
                d[o..o + 2].copy_from_slice(&v.to_le_bytes());
            } else {
                d[o] = v as u8;
            }
        };
        let ys = w as usize * sb + 6;
        let mut y = vec![0u8; ys * h as usize];
        for r in 0..h as usize {
            for x in 0..w as usize {
                put(&mut y, r * ys + x * sb, luma(x, r));
            }
        }
        let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
        let cs = cw * 2 * sb + 2;
        let mut c = vec![0u8; cs * ch];
        for r in 0..ch {
            for x in 0..cw {
                for k in 0..2 {
                    put(&mut c, r * cs + (2 * x + k) * sb, chroma(x, r, k));
                }
            }
        }
        let f = PlanarFormat { layout: PlaneLayout::SemiPlanar, bits, msb_aligned: msb, chroma_shift: (1, 1), swap_uv: false };
        image(f, (w, h), ColorInfo::default(), true, vec![(y, ys, w, h), (c, cs, cw as u32, ch as u32)])
    }

    fn get16(d: &[u8], stride: usize, x: usize, y: usize) -> u16 {
        u16::from_le_bytes([d[y * stride + 2 * x], d[y * stride + 2 * x + 1]])
    }

    #[test]
    fn sixteen_bit_msb_aligned_samples_and_interleaved_chroma_follow_the_fields() {
        let luma = |x: usize, y: usize| ((if y % 2 == 0 { 100 } else { 900 }) + x as u16) << 6;
        let chroma = |x: usize, y: usize, k: usize| ((if y % 2 == 0 { 300 } else { 700 }) + (x * 2 + k) as u16) << 6;
        let cur = semi_planar(10, true, 8, 8, luma, chroma);
        // Bob: other-field luma rows are the average of their neighbours, in the 16-bit domain.
        let out = run_deint(&cur, None, false);
        let (ys, cs) = (out[0].1, out[1].1);
        assert_eq!(get16(&out[0].0, ys, 3, 0), luma(3, 0));
        assert_eq!(get16(&out[0].0, ys, 3, 3), ((luma(3, 2) as u32 + luma(3, 4) as u32).div_ceil(2)) as u16);
        // 4:2:0 chroma: even chroma rows belong to the top field (copied), odd ones to the bottom field.
        for x in 0..8 {
            assert_eq!(get16(&out[1].0, cs, x, 0), chroma(x / 2, 0, x % 2));
            assert_eq!(get16(&out[1].0, cs, x, 2), chroma(x / 2, 2, x % 2));
            let want = ((chroma(x / 2, 0, x % 2) as u32 + chroma(x / 2, 2, x % 2) as u32).div_ceil(2)) as u16;
            assert_eq!(get16(&out[1].0, cs, x, 1), want, "U and V are interpolated separately");
        }
        // The bottom field shown: odd chroma rows kept, even ones interpolated (row 0 mirrors to row 1).
        let out = run_deint(&cur, None, true);
        assert_eq!(get16(&out[1].0, cs, 1, 1), chroma(0, 1, 1));
        assert_eq!(get16(&out[1].0, cs, 1, 0), chroma(0, 1, 1));
        // Static: woven exactly, chroma included.
        let prev = semi_planar(10, true, 8, 8, luma, chroma);
        let out = run_deint(&cur, Some(&prev), true);
        assert_eq!(get16(&out[0].0, ys, 5, 2), luma(5, 2));
        assert_eq!(get16(&out[1].0, cs, 5, 2), chroma(2, 2, 1));
        // 8-bit NV12 the same way.
        let cur8 = semi_planar(8, false, 8, 8, |x, y| if y % 2 == 0 { 10 + x as u16 } else { 200 }, |_, y, k| if y % 2 == 0 { 50 + k as u16 } else { 150 });
        let out = run_deint(&cur8, None, false);
        assert_eq!(out[1].0[out[1].1 + 1], 51, "odd chroma row of the hidden field: average of rows 0 and 2");
        assert_eq!(out[0].0[out[0].1 + 2], 12);
    }

    #[test]
    fn chroma_spanning_four_rows_is_copied() {
        let f = PlanarFormat { layout: PlaneLayout::Planar, bits: 8, msb_aligned: false, chroma_shift: (2, 2), swap_uv: false };
        let y: Vec<u8> = (0..64).map(|i| if (i / 8) % 2 == 0 { 10 } else { 240 }).collect();
        let u: Vec<u8> = vec![1, 2, 3, 4];
        let v: Vec<u8> = vec![5, 6, 7, 8];
        let img = image(f, (8, 8), ColorInfo::default(), true, vec![(y, 8, 8, 8), (u, 2, 2, 2), (v, 2, 2, 2)]);
        let out = run_deint(&img, None, false);
        assert_eq!(out[1].0, vec![1, 2, 3, 4]);
        assert_eq!(out[2].0, vec![5, 6, 7, 8]);
        assert_eq!(out[0].0[8], 10, "luma is deinterlaced");
    }

    const HDR10: ColorInfo = ColorInfo { matrix: YuvMatrix::Bt2020, full_range: false, transfer: Transfer::Pq, primaries: Primaries::Bt2020, peak_nits: 1000.0 };

    /// 10-bit limited-range (Y, U, V) code values of the test blocks: PQ white, 203-nit grey, black, a highlight above
    /// the knee, saturated reds and greens, a dark blue and a skin tone.
    const BLOCKS: [[u16; 3]; 8] = [[940, 512, 512], [592, 512, 512], [64, 512, 512], [800, 450, 600], [400, 400, 800], [600, 300, 250], [200, 700, 480], [520, 470, 560]];

    /// A 64 × 32 picture of 16 × 16 flat blocks (`BLOCKS`), as yuv420p10le (`msb` false) or P010.
    fn blocks(msb: bool, color: ColorInfo) -> PlanarImage {
        let (w, h) = (64usize, 32usize);
        let block = |x: usize, y: usize| BLOCKS[(y / 16) * 4 + x / 16];
        let shift = if msb { 6 } else { 0 };
        let mut yp = vec![0u8; w * 2 * h];
        for y in 0..h {
            for x in 0..w {
                yp[y * w * 2 + x * 2..][..2].copy_from_slice(&(block(x, y)[0] << shift).to_le_bytes());
            }
        }
        let (cw, ch) = (w / 2, h / 2);
        if msb {
            let mut c = vec![0u8; cw * 4 * ch];
            for y in 0..ch {
                for x in 0..cw {
                    let b = block(x * 2, y * 2);
                    c[y * cw * 4 + x * 4..][..2].copy_from_slice(&(b[1] << 6).to_le_bytes());
                    c[y * cw * 4 + x * 4 + 2..][..2].copy_from_slice(&(b[2] << 6).to_le_bytes());
                }
            }
            let f = PlanarFormat { layout: PlaneLayout::SemiPlanar, bits: 10, msb_aligned: true, chroma_shift: (1, 1), swap_uv: false };
            image(f, (w as u32, h as u32), color, false, vec![(yp, w * 2, w as u32, h as u32), (c, cw * 4, cw as u32, ch as u32)])
        } else {
            let plane = |k: usize| {
                let mut c = vec![0u8; cw * 2 * ch];
                for y in 0..ch {
                    for x in 0..cw {
                        c[y * cw * 2 + x * 2..][..2].copy_from_slice(&block(x * 2, y * 2)[k].to_le_bytes());
                    }
                }
                c
            };
            let f = PlanarFormat { layout: PlaneLayout::Planar, bits: 10, msb_aligned: false, chroma_shift: (1, 1), swap_uv: false };
            image(f, (w as u32, h as u32), color, false, vec![(yp, w * 2, w as u32, h as u32), (plane(1), cw * 2, cw as u32, ch as u32), (plane(2), cw * 2, cw as u32, ch as u32)])
        }
    }

    /// Worst channel difference inside the blocks (2 pixels away from their edges) between the kernel's output
    /// (`out_w` wide) and `apply(yuv_to_rgb_reference)`.
    fn block_error(img: &PlanarImage, out_w: u32, stripes: usize) -> u8 {
        let params = ColorParams::for_image(img);
        let luts = ToneLuts::new(&params);
        let (w, h) = (img.coded_width as usize, img.coded_height as usize);
        let ow = out_w as usize;
        let mut dst = vec![0u8; ow * h * 4];
        let pool = test_pool();
        assert!(convert_striped(img, out_w, &luts, ConvertOptions::default(), &mut dst, Some(&pool), stripes));
        let mut worst = 0u8;
        for y in 0..h {
            for x in 0..ow {
                // The left one of the two luma samples the output pixel lies between.
                let sx = ((x as f64 + 0.5) * w as f64 / ow as f64 - 0.5).floor().max(0.0) as usize;
                if !(2..13).contains(&(sx % 16)) || !(2..14).contains(&(y % 16)) {
                    continue;
                }
                let want = params.apply(yuv_to_rgb_reference(img, sx, y)).map(|v| (v * 255.0).round() as u8);
                let o = (y * ow + x) * 4;
                for k in 0..3 {
                    worst = worst.max(dst[o + k].abs_diff(want[k]));
                }
                assert_eq!(dst[o + 3], 255);
            }
        }
        worst
    }

    #[test]
    fn the_kernel_tone_maps_flat_blocks_like_apply() {
        let hlg = ColorInfo { transfer: Transfer::Hlg, ..HDR10 };
        for (name, color) in [("PQ", HDR10), ("HLG", hlg), ("gamut", ColorInfo { transfer: Transfer::Sdr, ..HDR10 })] {
            for msb in [false, true] {
                let img = blocks(msb, color);
                assert_ne!(ColorParams::for_image(&img).mode, crate::color::ColorMode::None);
                let worst = block_error(&img, 64, 1);
                assert!(worst <= 1, "{name} msb={msb}: worst {worst}");
                // Stretched horizontally (sample aspect ratio), on several threads.
                let worst = block_error(&img, 96, 3);
                assert!(worst <= 1, "{name} msb={msb} stretched: worst {worst}");
            }
        }
    }

    #[test]
    fn stripes_give_the_same_picture_as_one_thread() {
        let img = blocks(true, HDR10);
        let luts = ToneLuts::new(&ColorParams::for_image(&img));
        let pool = test_pool();
        let opts = ConvertOptions::default();
        for half in [false, true] {
            let opts = ConvertOptions { half, ..opts };
            let mut one = vec![0u8; 80 * 32 * 4];
            let mut four = vec![1u8; 80 * 32 * 4];
            assert!(convert_rgba(&img, 80, &luts, opts, &mut one, None));
            assert!(convert_striped(&img, 80, &luts, opts, &mut four, Some(&pool), 4));
            let n = 80 * output_rows(&img, opts) * 4;
            assert_eq!(one[..n], four[..n], "half={half}");
        }
        let mut small = vec![0u8; 100];
        assert!(!convert_rgba(&img, 80, &luts, opts, &mut small, None), "too small a buffer");
        assert_eq!(stripes_for(1000), 1);
        assert!((1..=8).contains(&stripes_for(1920 * 1080)) && stripes_for(1920 * 1080) <= max_stripes());
        assert!(stripes_for(3840 * 2160) >= stripes_for(1280 * 720));
    }

    #[test]
    fn deinterlaced_420_chroma_comes_from_the_rows_of_the_same_field() {
        // 8-bit full-range 4:2:0, luma 128; chroma rows 0, 1, 2, 3 = U 60, 200, 90, 160 (V neutral).
        let f = PlanarFormat { layout: PlaneLayout::Planar, bits: 8, msb_aligned: false, chroma_shift: (1, 1), swap_uv: false };
        let color = ColorInfo { full_range: true, ..ColorInfo::default() };
        let us = [60u8, 200, 90, 160];
        let u: Vec<u8> = us.iter().flat_map(|v| [*v; 2]).collect();
        let img = image(f, (4, 8), color, false, vec![(vec![128; 32], 4, 4, 8), (u, 2, 2, 4), (vec![128; 8], 2, 2, 4)]);
        let luts = ToneLuts::new(&ColorParams::for_image(&img));
        let mut dst = vec![0u8; 4 * 8 * 4];
        assert!(convert_rgba(&img, 4, &luts, ConvertOptions { chroma_fields: true, half: false }, &mut dst, None));
        // Blue grows with U: luma rows 0..8 take chroma rows 0, 1, 0, 1, 2, 3, 2, 3.
        let blue: Vec<u8> = (0..8).map(|y| dst[y * 16 + 2]).collect();
        let want_row = [0, 1, 0, 1, 2, 3, 2, 3];
        for y in 0..8 {
            let same: Vec<usize> = (0..8).filter(|r| want_row[*r] == want_row[y]).collect();
            assert!(same.iter().all(|r| blue[*r] == blue[y]), "{blue:?}");
        }
        assert!(blue[0] < blue[2 + 2] && blue[1] > blue[5], "{blue:?}");
        // Progressive (bilinear) sampling differs.
        let mut prog = vec![0u8; dst.len()];
        assert!(convert_rgba(&img, 4, &luts, ConvertOptions::default(), &mut prog, None));
        assert_ne!(prog, dst);
    }

    fn test_pool() -> StripePool {
        StripePool::new(3, std::sync::Arc::new(|| Box::new(()) as Box<dyn std::any::Any>))
    }

    #[test]
    fn half_size_averages_two_by_two_blocks_and_keeps_flat_colours() {
        let hlg = ColorInfo { transfer: Transfer::Hlg, ..HDR10 };
        for color in [HDR10, hlg] {
            for msb in [false, true] {
                let img = blocks(msb, color);
                let params = ColorParams::for_image(&img);
                let luts = ToneLuts::new(&params);
                let opts = ConvertOptions { half: true, chroma_fields: true };
                assert_eq!(output_rows(&img, opts), 16);
                let mut dst = vec![0u8; 32 * 16 * 4];
                assert!(convert_rgba(&img, 32, &luts, opts, &mut dst, None));
                // Every 8 x 8 output block is its 16 x 16 source block (inside, away from the chroma edges).
                for y in (1..7).chain(9..15) {
                    for x in (1..7).chain(9..15).chain(17..23).chain(25..31) {
                        let want = params.apply(yuv_to_rgb_reference(&img, x * 2, y * 2)).map(|v| (v * 255.0).round() as u8);
                        let o = (y * 32 + x) * 4;
                        for k in 0..3 {
                            assert!(dst[o + k].abs_diff(want[k]) <= 1, "msb={msb} ({x}, {y}): {:?} vs {want:?}", &dst[o..o + 3]);
                        }
                    }
                }
            }
        }
        // A luma checkerboard averages to its mean (8-bit full-range gray: exact).
        let f = PlanarFormat { layout: PlaneLayout::Gray, bits: 8, msb_aligned: false, chroma_shift: (0, 0), swap_uv: false };
        let px: Vec<u8> = (0..64).map(|i| if (i % 8 + i / 8) % 2 == 0 { 100 } else { 200 }).collect();
        let img = image(f, (8, 8), ColorInfo { full_range: true, ..ColorInfo::default() }, false, vec![(px, 8, 8, 8)]);
        let luts = ToneLuts::new(&ColorParams::for_image(&img));
        let mut dst = vec![0u8; 4 * 4 * 4];
        assert!(convert_rgba(&img, 4, &luts, ConvertOptions { half: true, ..Default::default() }, &mut dst, None));
        assert!(dst.chunks(4).all(|p| p == [150, 150, 150, 255]), "{dst:?}");
        assert!(half_size(3840 * 2160) && !half_size(2560 * 1440) && !half_size(1920 * 1080));
    }

    #[test]
    fn a_panicking_stripe_is_reported_after_every_stripe_ran() {
        let pool = test_pool();
        let ran = std::sync::atomic::AtomicUsize::new(0);
        let ok = pool.run(4, &|i| {
            ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if i == 2 {
                panic!("stripe {i}");
            }
        });
        assert!(!ok);
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 4);
        // The pool keeps working.
        assert!(pool.run(4, &|_| ()));
        // A panic on the calling thread (stripe 0) is caught as well.
        assert!(!pool.run(2, &|i| assert_ne!(i, 0)));
    }

    #[test]
    fn only_the_frame_right_before_is_used_for_motion_detection() {
        assert!(previous_frame_follows(1.0, 1.04));
        assert!(previous_frame_follows(1.0, 1.08), "one dropped frame");
        assert!(!previous_frame_follows(1.0, 1.0));
        assert!(!previous_frame_follows(1.0, 0.96), "a seek back");
        assert!(!previous_frame_follows(1.0, 1.5), "a seek forward");
    }
}
