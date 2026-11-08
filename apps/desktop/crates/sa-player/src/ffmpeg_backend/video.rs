//! Video frame conversion: decoded frame (any pixel format) → tightly packed RGBA8 at the display size (sample
//! aspect ratio applied), rotated by the stream's display matrix. Output buffers are recycled once the UI dropped them.

use std::ffi::c_int;
use std::ptr;
use std::sync::Arc;

use ffmpeg_next::ffi;

use crate::{PlanarFormat, PlanarImage, PlaneInfo, PlaneLayout, YuvMatrix};

/// Display geometry of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Geometry {
    /// Scaled (square-pixel) size before rotation.
    pub width: u32,
    pub height: u32,
    /// Clockwise rotation in quarter turns (0..=3).
    pub quarter_turns: u8,
}

impl Geometry {
    /// Size as shown (after rotation).
    pub fn display_size(&self) -> (u32, u32) {
        if self.quarter_turns % 2 == 1 { (self.height, self.width) } else { (self.width, self.height) }
    }

    /// Coded size + sample aspect ratio + rotation (degrees, counter-clockwise as `av_display_rotation_get` returns).
    pub fn new(coded_w: u32, coded_h: u32, sar: (i32, i32), rotation_ccw_degrees: f64) -> Self {
        let (num, den) = sar;
        let mut width = coded_w.max(1);
        let height = coded_h.max(1);
        if num > 0 && den > 0 && num != den {
            let w = (coded_w as f64 * num as f64 / den as f64).round();
            if w.is_finite() && w >= 2.0 && w <= 16384.0 {
                width = (w as u32) & !1;
            }
        }
        let quarter_turns = if rotation_ccw_degrees.is_finite() {
            // Clockwise quarter turns needed to display the picture upright.
            let cw = (-rotation_ccw_degrees).rem_euclid(360.0);
            ((cw / 90.0).round() as i64).rem_euclid(4) as u8
        } else {
            0
        };
        Self { width, height, quarter_turns }
    }
}

/// What decides the display geometry of a stream's frames: its rotation and sample aspect ratios. The geometry is
/// computed per frame, so a resolution change in the middle of a stream shows at its new size (`SizeChanged`), as
/// Media Foundation's NaturalVideoSizeChanged does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StreamLayout {
    /// Degrees counter-clockwise (`av_display_rotation_get`).
    pub rotation: f64,
    /// The container's sample aspect ratio (wins when set, as in `av_guess_sample_aspect_ratio`).
    pub container_sar: (i32, i32),
    /// The codec parameters' sample aspect ratio (used when a frame carries none).
    pub codec_sar: (i32, i32),
}

impl StreamLayout {
    fn valid(r: (i32, i32)) -> bool {
        r.0 > 0 && r.1 > 0
    }

    /// `av_guess_sample_aspect_ratio`: the container's ratio, else the frame's (the codec's without a frame).
    pub fn sar(&self, frame_sar: Option<(i32, i32)>) -> (i32, i32) {
        if Self::valid(self.container_sar) {
            return self.container_sar;
        }
        let f = frame_sar.unwrap_or(self.codec_sar);
        if Self::valid(f) { f } else { (0, 1) }
    }

    /// Geometry of a frame of `width` × `height` with its own sample aspect ratio.
    pub fn geometry(&self, width: u32, height: u32, frame_sar: Option<(i32, i32)>) -> Geometry {
        Geometry::new(width, height, self.sar(frame_sar), self.rotation)
    }
}

/// Reads the display-matrix rotation of a stream (degrees counter-clockwise, 0 when absent).
pub(crate) unsafe fn stream_rotation(stream: *const ffi::AVStream) -> f64 {
    unsafe {
        let par = (*stream).codecpar;
        if par.is_null() {
            return 0.0;
        }
        let sd = ffi::av_packet_side_data_get(
            (*par).coded_side_data,
            (*par).nb_coded_side_data,
            ffi::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
        );
        if sd.is_null() || (*sd).size < 36 || (*sd).data.is_null() {
            return 0.0;
        }
        let r = ffi::av_display_rotation_get((*sd).data as *const i32);
        if r.is_finite() { r } else { 0.0 }
    }
}

struct Scaler {
    ctx: *mut ffi::SwsContext,
    key: (c_int, c_int, c_int, u32, u32, c_int, c_int),
}

impl Drop for Scaler {
    fn drop(&mut self) {
        unsafe { ffi::sws_freeContext(self.ctx) };
    }
}

/// Converts frames; owned by the video decode thread.
pub(crate) struct Converter {
    scaler: Option<Scaler>,
    pool: Vec<Arc<Vec<u8>>>,
    scratch: Vec<u8>,
}

// SAFETY: the swscale context is only used by the owning thread.
unsafe impl Send for Converter {}

impl Converter {
    pub fn new() -> Self {
        Self { scaler: None, pool: Vec::new(), scratch: Vec::new() }
    }

    fn buffer(&mut self, len: usize) -> Arc<Vec<u8>> {
        // Reuse a buffer nobody else holds any more.
        if let Some(i) = self.pool.iter().position(|b| Arc::strong_count(b) == 1 && Arc::weak_count(b) == 0 && b.len() == len) {
            return self.pool.swap_remove(i);
        }
        Arc::new(vec![0u8; len])
    }

    fn recycle(&mut self, buf: &Arc<Vec<u8>>) {
        self.pool.retain(|b| b.len() == buf.len());
        if self.pool.len() < 8 {
            self.pool.push(buf.clone());
        }
    }

    /// Converts `frame` to RGBA at `geo`; `None` when swscale cannot handle it.
    pub unsafe fn convert(&mut self, frame: *const ffi::AVFrame, geo: Geometry) -> Option<(u32, u32, Arc<Vec<u8>>)> {
        unsafe {
            let src_w = (*frame).width;
            let src_h = (*frame).height;
            let fmt = (*frame).format;
            if src_w <= 0 || src_h <= 0 || fmt < 0 {
                return None;
            }
            let (dw, dh) = (geo.width, geo.height);
            let colorspace = (*frame).colorspace as c_int;
            let full_range = ((*frame).color_range == ffi::AVColorRange::AVCOL_RANGE_JPEG) as c_int;
            let key = (src_w, src_h, fmt, dw, dh, colorspace, full_range);
            if self.scaler.as_ref().map(|s| s.key) != Some(key) {
                self.scaler = None;
                let src_fmt: ffi::AVPixelFormat = std::mem::transmute::<c_int, ffi::AVPixelFormat>(fmt);
                let flags = if (dw as c_int, dh as c_int) == (src_w, src_h) {
                    ffi::SwsFlags::SWS_POINT as c_int
                } else {
                    ffi::SwsFlags::SWS_BILINEAR as c_int
                };
                let ctx = ffi::sws_getContext(
                    src_w,
                    src_h,
                    src_fmt,
                    dw as c_int,
                    dh as c_int,
                    ffi::AVPixelFormat::AV_PIX_FMT_RGBA,
                    flags,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                );
                if ctx.is_null() {
                    return None;
                }
                // YUV → RGB matrix: the frame's colorspace, else BT.709 for HD and BT.601 for SD.
                let cs = match (*frame).colorspace {
                    ffi::AVColorSpace::AVCOL_SPC_BT709 => ffi::SWS_CS_ITU709,
                    ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL | ffi::AVColorSpace::AVCOL_SPC_BT2020_CL => ffi::SWS_CS_BT2020,
                    ffi::AVColorSpace::AVCOL_SPC_BT470BG
                    | ffi::AVColorSpace::AVCOL_SPC_SMPTE170M
                    | ffi::AVColorSpace::AVCOL_SPC_FCC => ffi::SWS_CS_ITU601,
                    _ if src_h >= 720 => ffi::SWS_CS_ITU709,
                    _ => ffi::SWS_CS_ITU601,
                };
                let table = ffi::sws_getCoefficients(cs);
                // Ignored (returns an error) for RGB sources, which is fine.
                ffi::sws_setColorspaceDetails(ctx, table, full_range, ffi::sws_getCoefficients(ffi::SWS_CS_DEFAULT), 1, 0, 1 << 16, 1 << 16);
                self.scaler = Some(Scaler { ctx, key });
            }
            let sws = self.scaler.as_ref()?.ctx;
            let len = dw as usize * dh as usize * 4;
            let rotated = geo.quarter_turns % 4 != 0;
            let mut out = self.buffer(len);
            {
                let target: &mut Vec<u8> = if rotated {
                    self.scratch.resize(len, 0);
                    &mut self.scratch
                } else {
                    Arc::get_mut(&mut out).expect("fresh or recycled buffer is unique")
                };
                let dst = [target.as_mut_ptr(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut()];
                let dst_stride = [(dw * 4) as c_int, 0, 0, 0];
                let r = ffi::sws_scale(
                    sws,
                    (*frame).data.as_ptr() as *const *const u8,
                    (*frame).linesize.as_ptr(),
                    0,
                    src_h,
                    dst.as_ptr(),
                    dst_stride.as_ptr(),
                );
                if r <= 0 {
                    return None;
                }
            }
            let (ow, oh) = geo.display_size();
            if rotated {
                let dst = Arc::get_mut(&mut out).expect("unique");
                rotate_rgba(&self.scratch, dw, dh, geo.quarter_turns, dst);
            }
            self.recycle(&out);
            Some((ow, oh, out))
        }
    }
}

/// The planar layout of a decoder pixel format the UI can convert on the GPU (`None`: convert to RGBA here).
/// The second value says whether the format implies full range (the deprecated `yuvj*` formats).
pub(crate) fn planar_format(format: c_int) -> Option<(PlanarFormat, bool)> {
    use ffi::AVPixelFormat as F;
    let planar = |bits: u8, shift: (u8, u8)| PlanarFormat { layout: PlaneLayout::Planar, bits, msb_aligned: false, chroma_shift: shift };
    let semi = |bits: u8, msb: bool| PlanarFormat { layout: PlaneLayout::SemiPlanar, bits, msb_aligned: msb, chroma_shift: (1, 1) };
    if format < 0 {
        return None;
    }
    let f: F = unsafe { std::mem::transmute::<c_int, F>(format) };
    Some(match f {
        F::AV_PIX_FMT_YUV420P => (planar(8, (1, 1)), false),
        F::AV_PIX_FMT_YUVJ420P => (planar(8, (1, 1)), true),
        F::AV_PIX_FMT_YUV422P => (planar(8, (1, 0)), false),
        F::AV_PIX_FMT_YUVJ422P => (planar(8, (1, 0)), true),
        F::AV_PIX_FMT_YUV444P => (planar(8, (0, 0)), false),
        F::AV_PIX_FMT_YUVJ444P => (planar(8, (0, 0)), true),
        F::AV_PIX_FMT_YUV420P10LE => (planar(10, (1, 1)), false),
        F::AV_PIX_FMT_YUV422P10LE => (planar(10, (1, 0)), false),
        F::AV_PIX_FMT_YUV444P10LE => (planar(10, (0, 0)), false),
        F::AV_PIX_FMT_YUV420P12LE => (planar(12, (1, 1)), false),
        F::AV_PIX_FMT_YUV422P12LE => (planar(12, (1, 0)), false),
        F::AV_PIX_FMT_YUV444P12LE => (planar(12, (0, 0)), false),
        F::AV_PIX_FMT_NV12 => (semi(8, false), false),
        F::AV_PIX_FMT_P010LE => (semi(10, true), false),
        F::AV_PIX_FMT_P012LE => (semi(12, true), false),
        F::AV_PIX_FMT_P016LE => (semi(16, true), false),
        _ => return None,
    })
}

/// The YUV → RGB matrix of a frame: its colorspace, else BT.709 for HD and BT.601 for SD (as the RGBA path).
pub(crate) unsafe fn frame_matrix(frame: *const ffi::AVFrame) -> YuvMatrix {
    unsafe {
        match (*frame).colorspace {
            ffi::AVColorSpace::AVCOL_SPC_BT709 => YuvMatrix::Bt709,
            ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL | ffi::AVColorSpace::AVCOL_SPC_BT2020_CL => YuvMatrix::Bt2020,
            ffi::AVColorSpace::AVCOL_SPC_BT470BG | ffi::AVColorSpace::AVCOL_SPC_SMPTE170M | ffi::AVColorSpace::AVCOL_SPC_FCC => {
                YuvMatrix::Bt601
            }
            _ if (*frame).height >= 720 => YuvMatrix::Bt709,
            _ => YuvMatrix::Bt601,
        }
    }
}

/// Describes the planes of `owner` (a software frame) for the GPU, or `None` when its format or layout is not one
/// the UI converts (negative strides, unknown formats). `props` is the frame the color properties are read from
/// (the hardware frame when `owner` is its download).
pub(crate) unsafe fn planar_image(owner: Box<dyn Send + Sync>, frame: *const ffi::AVFrame, props: *const ffi::AVFrame, geo: Geometry) -> Option<PlanarImage> {
    unsafe {
        let (format, forced_full) = planar_format((*frame).format)?;
        let (w, h) = ((*frame).width, (*frame).height);
        if w <= 0 || h <= 0 {
            return None;
        }
        let (w, h) = (w as u32, h as u32);
        let sb = if format.bits > 8 { 2usize } else { 1 };
        let (sx, sy) = format.chroma_shift;
        let cw = (w + (1 << sx) - 1) >> sx;
        let ch = (h + (1 << sy) - 1) >> sy;
        let mut planes: [Option<(*const u8, PlaneInfo)>; 3] = [None; 3];
        let count = if format.layout == PlaneLayout::Planar { 3 } else { 2 };
        for (i, plane) in planes.iter_mut().enumerate().take(count) {
            let data = (*frame).data[i];
            let stride = (*frame).linesize[i];
            if data.is_null() || stride <= 0 {
                return None;
            }
            let (pw, rows, comps) = match (i, format.layout) {
                (0, _) => (w, h, 1),
                (_, PlaneLayout::Planar) => (cw, ch, 1),
                (_, PlaneLayout::SemiPlanar) => (cw, ch, 2),
            };
            let row_bytes = pw as usize * comps * sb;
            if (stride as usize) < row_bytes {
                return None;
            }
            *plane = Some((data as *const u8, PlaneInfo { width: pw, rows, stride: stride as usize, row_bytes }));
        }
        let full_range = forced_full || (*props).color_range == ffi::AVColorRange::AVCOL_RANGE_JPEG;
        Some(PlanarImage::new(format, (w, h), frame_matrix(props), full_range, geo.quarter_turns % 4, planes, owner))
    }
}

/// Rotates a packed RGBA image clockwise by `quarter_turns` × 90°.
pub(crate) fn rotate_rgba(src: &[u8], w: u32, h: u32, quarter_turns: u8, dst: &mut [u8]) {
    let (w, h) = (w as usize, h as usize);
    let px = |buf: &[u8], i: usize| -> [u8; 4] { [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]] };
    match quarter_turns % 4 {
        0 => dst.copy_from_slice(src),
        1 => {
            // (x, y) → (h - 1 - y, x) in a h×w image.
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let d = (x * h + (h - 1 - y)) * 4;
                    dst[d..d + 4].copy_from_slice(&px(src, s));
                }
            }
        }
        2 => {
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let d = ((h - 1 - y) * w + (w - 1 - x)) * 4;
                    dst[d..d + 4].copy_from_slice(&px(src, s));
                }
            }
        }
        _ => {
            // (x, y) → (y, w - 1 - x)
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let d = ((w - 1 - x) * h + y) * 4;
                    dst[d..d + 4].copy_from_slice(&px(src, s));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_applies_sample_aspect_and_rotation() {
        let g = Geometry::new(1440, 1080, (4, 3), 0.0);
        assert_eq!(g.display_size(), (1920, 1080));
        // Phone video: display matrix -90° (clockwise) → shown portrait.
        let g = Geometry::new(1920, 1080, (1, 1), -90.0);
        assert_eq!((g.quarter_turns, g.display_size()), (1, (1080, 1920)));
        let g = Geometry::new(1920, 1080, (0, 1), 90.0);
        assert_eq!((g.quarter_turns, g.display_size()), (3, (1080, 1920)));
        assert_eq!(Geometry::new(640, 480, (1, 1), 180.0).quarter_turns, 2);
        assert_eq!(Geometry::new(640, 480, (1, 1), f64::NAN).quarter_turns, 0);
    }

    #[test]
    fn the_container_aspect_ratio_wins_then_the_frame_then_the_codec() {
        let l = StreamLayout { rotation: 0.0, container_sar: (0, 1), codec_sar: (4, 3) };
        assert_eq!(l.sar(None), (4, 3));
        assert_eq!(l.sar(Some((0, 1))), (0, 1), "a frame without a ratio is square");
        assert_eq!(l.geometry(1440, 1080, None).display_size(), (1920, 1080));
        // A mid-stream resolution change keeps the rotation.
        let l = StreamLayout { rotation: -90.0, container_sar: (1, 1), codec_sar: (4, 3) };
        assert_eq!(l.geometry(1280, 720, Some((4, 3))).display_size(), (720, 1280));
        assert_eq!(l.geometry(640, 360, Some((4, 3))).display_size(), (360, 640));
    }

    #[test]
    fn rotation_moves_pixels_clockwise() {
        // 2×1 image: red, green.
        let src = [255, 0, 0, 255, 0, 255, 0, 255];
        let mut dst = [0u8; 8];
        rotate_rgba(&src, 2, 1, 1, &mut dst);
        // 1×2: red on top, green below.
        assert_eq!(dst, [255, 0, 0, 255, 0, 255, 0, 255]);
        rotate_rgba(&src, 2, 1, 2, &mut dst);
        assert_eq!(dst, [0, 255, 0, 255, 255, 0, 0, 255]);
        rotate_rgba(&src, 2, 1, 3, &mut dst);
        assert_eq!(dst, [0, 255, 0, 255, 255, 0, 0, 255]);
    }
}
