//! Video frame description and conversion: the planes of a decoded frame for the GPU path (format, colour, HDR
//! levels, interlacing), or swscale's RGBA8 conversion of any pixel format (at the display size and rotated for the
//! RGBA path; unscaled for the GPU path's packed images). The RGBA path tone-maps HDR / wide-gamut frames and
//! deinterlaces interlaced ones itself (`Converter::convert_soft`, `crate::soft`). Output buffers are recycled once
//! the UI dropped them.

use std::ffi::c_int;
use std::ptr;
use std::sync::Arc;

use ffmpeg_next::ffi;

use super::hw::OwnedFrame;
use crate::color::{ColorMode, ColorParams, ToneLuts};
use crate::soft;
use crate::{ColorInfo, Interlace, PlanarFormat, PlanarImage, PlaneInfo, PlaneLayout, Primaries, Transfer, YuvMatrix};

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
    /// Output buffers per length (most recently used length first; at most two lengths: the packed route alternates
    /// between the coded and the display size).
    pool: Vec<(usize, Vec<Arc<Vec<u8>>>)>,
    scratch: Vec<u8>,
    /// Worker threads of the RGBA kernel's stripes (created on first use).
    stripes: Option<soft::StripePool>,
    /// Deinterlaced copies of frames (RGBA path), reused once nothing holds them.
    deinterlaced: Vec<Arc<OwnedFrame>>,
    /// The colour tables of the RGBA path's own conversion, for the parameters they were built for.
    luts: Option<ToneLuts>,
}

// SAFETY: the swscale context is only used by the owning thread.
unsafe impl Send for Converter {}

/// What the RGBA path does with one decoded frame ([`Converter::soft_source`]).
pub(crate) struct SoftSource {
    /// The frame's planes when the RGBA path converts it itself (HDR, wide gamut, interlaced); `None`: swscale
    /// converts the frame as it is.
    pub image: Option<Arc<PlanarImage>>,
    /// Shown field by field (deinterlaced); only with `image`.
    pub interlaced: bool,
    /// Tone-mapped / gamut-converted (not plain SDR BT.709); only with `image`.
    pub tone_map: bool,
}

impl Converter {
    pub fn new() -> Self {
        Self { scaler: None, pool: Vec::new(), scratch: Vec::new(), stripes: None, deinterlaced: Vec::new(), luts: None }
    }

    /// The kernel's stripe workers: `soft::max_stripes() - 1` threads with the decode thread's priority (none on a
    /// machine with too few cores).
    fn stripe_pool(&mut self) -> Option<&soft::StripePool> {
        let workers = soft::max_stripes() - 1;
        if workers == 0 {
            return None;
        }
        let init: Arc<dyn Fn() -> Box<dyn std::any::Any> + Send + Sync> =
            Arc::new(|| Box::new(super::priority::raise(super::priority::Role::VideoDecode)) as Box<dyn std::any::Any>);
        Some(self.stripes.get_or_insert_with(|| soft::StripePool::new(workers, init)))
    }

    /// Decides how the RGBA path converts `raw` (its download `src` / `downloaded` for hardware frames): plain SDR
    /// BT.709 progressive frames through swscale as before; HDR, wide-gamut and interlaced frames described as planes
    /// (zero-copy, or swscale's packed RGBA for formats without a planar description) for [`Converter::convert_soft`].
    pub unsafe fn soft_source(
        &mut self,
        raw: *const ffi::AVFrame,
        src: *const ffi::AVFrame,
        downloaded: Option<Arc<OwnedFrame>>,
        geo: Geometry,
        hdr: &HdrLevels,
    ) -> SoftSource {
        unsafe {
            let plain = SoftSource { image: None, interlaced: false, tone_map: false };
            let interlace = frame_interlace(raw);
            let matrix = frame_matrix(raw);
            let transfer = transfer_of((*raw).color_trc);
            let primaries = primaries_of((*raw).color_primaries, transfer, matrix);
            let tone_map = ColorParams::new(transfer, primaries, hdr.peak_nits()).mode != ColorMode::None;
            if !tone_map && interlace.is_none() {
                return plain;
            }
            let image = if planar_format((*src).format).is_some() {
                let owner: Option<Box<dyn Send + Sync>> = match downloaded {
                    Some(d) => Some(Box::new(d)),
                    None => OwnedFrame::reference(raw).map(|r| Box::new(r) as Box<dyn Send + Sync>),
                };
                owner.and_then(|owner| planar_image(owner, src, raw, geo, hdr))
            } else {
                self.convert_packed(src).and_then(|(w, h, rgba)| packed_image(rgba, (w, h), raw, geo, hdr))
            };
            match image {
                Some(img) => SoftSource { interlaced: img.interlaced, tone_map, image: Some(Arc::new(img)) },
                None => plain,
            }
        }
    }

    /// RGBA at `geo` for the software renderer: swscale for a plain source, else the field due (`second_field`)
    /// deinterlaced against `prev` (the previous interlaced frame as decoded, when it directly precedes this one) and
    /// converted by swscale (SDR with a planar description) or by [`soft::convert_rgba`] with tone mapping.
    pub unsafe fn convert_soft(
        &mut self,
        soft: &SoftSource,
        src: *const ffi::AVFrame,
        geo: Geometry,
        prev: Option<&PlanarImage>,
        second_field: bool,
    ) -> Option<(u32, u32, Arc<Vec<u8>>)> {
        unsafe {
            let Some(img) = soft.image.as_ref() else { return self.convert(src, geo) };
            if !soft.interlaced {
                return self.convert_kernel(img, geo, false);
            }
            let packed = img.layout == PlaneLayout::Packed;
            let av_format = if packed { ffi::AVPixelFormat::AV_PIX_FMT_RGBA as c_int } else { (*src).format };
            let out = self.deinterlaced_frame(img.coded_width, img.coded_height, av_format)?;
            let f = out.0;
            {
                let mut planes = Vec::with_capacity(3);
                for i in 0..img.plane_count() {
                    let (_, info) = img.plane(i)?;
                    let stride = (*f).linesize[i];
                    if (*f).data[i].is_null() || stride <= 0 || (stride as usize) < info.row_bytes {
                        return None;
                    }
                    let len = (info.rows as usize - 1) * stride as usize + info.row_bytes;
                    planes.push(soft::PlaneMut { data: std::slice::from_raw_parts_mut((*f).data[i], len), stride: stride as usize });
                }
                soft::deinterlace_planes(img, prev, img.shown_field_is_bottom(second_field), &mut planes);
            }
            // The deinterlaced copy holds no alpha plane (only the colour planes are written): alpha formats go through
            // the kernel, which shows them opaque like the shader.
            if !soft.tone_map && !packed && !has_alpha(av_format) {
                // SDR: swscale converts the deinterlaced copy as it converts progressive frames.
                (*f).colorspace = (*src).colorspace;
                (*f).color_range = (*src).color_range;
                (*f).color_trc = (*src).color_trc;
                (*f).color_primaries = (*src).color_primaries;
                return self.convert(f, geo);
            }
            let format = img.format();
            let planes = frame_planes(f, &format)?;
            let color = ColorInfo { matrix: img.matrix, full_range: img.full_range, transfer: img.transfer, primaries: img.primaries, peak_nits: img.peak_nits };
            let image = PlanarImage::new(format, (img.coded_width, img.coded_height), color, 0, None, planes, Box::new(out.clone()));
            self.convert_kernel(&image, geo, true)
        }
    }

    /// A writable frame of `format` at `w` × `h` for a deinterlaced copy (recycled once nothing holds it).
    unsafe fn deinterlaced_frame(&mut self, w: u32, h: u32, format: c_int) -> Option<Arc<OwnedFrame>> {
        unsafe {
            let (w, h) = (w as c_int, h as c_int);
            let same = |f: &Arc<OwnedFrame>| {
                let p = f.0;
                (*p).width == w && (*p).height == h && (*p).format == format && !(*p).buf[0].is_null()
            };
            if let Some(i) = self.deinterlaced.iter().position(|f| Arc::strong_count(f) == 1 && Arc::weak_count(f) == 0 && same(f)) {
                return Some(self.deinterlaced[i].clone());
            }
            let f = OwnedFrame::alloc()?;
            (*f.0).width = w;
            (*f.0).height = h;
            (*f.0).format = format;
            if ffi::av_frame_get_buffer(f.0, 0) < 0 {
                return None;
            }
            let f = Arc::new(f);
            self.deinterlaced.retain(|f| same(f));
            // One in use at a time (converted before the next field is deinterlaced), one spare.
            if self.deinterlaced.len() < 2 {
                self.deinterlaced.push(f.clone());
            }
            Some(f)
        }
    }

    /// [`soft::convert_rgba`] of `img` at `geo` (then rotated), with the colour tables cached per parameters. Frames
    /// above [`soft::HALF_SIZE_ABOVE`] are converted at half width and height (the returned size says so; the video's
    /// size is still `geo`'s).
    fn convert_kernel(&mut self, img: &PlanarImage, geo: Geometry, chroma_fields: bool) -> Option<(u32, u32, Arc<Vec<u8>>)> {
        if img.coded_height != geo.height || geo.width == 0 {
            return None;
        }
        let half = soft::half_size(geo.width as usize * geo.height as usize);
        let opts = soft::ConvertOptions { chroma_fields, half };
        let geo = if half {
            Geometry { width: (geo.width / 2).max(1), height: soft::output_rows(img, opts) as u32, quarter_turns: geo.quarter_turns }
        } else {
            geo
        };
        let params = ColorParams::for_image(img);
        if self.luts.as_ref().is_none_or(|l| *l.params() != params) {
            // Also when the stream's MaxCLL arrives with a later frame.
            self.luts = Some(ToneLuts::new(&params));
        }
        let (dw, dh) = (geo.width, geo.height);
        let len = dw as usize * dh as usize * 4;
        let rotated = geo.quarter_turns % 4 != 0;
        let mut out = self.buffer(len);
        self.stripe_pool();
        let pool = self.stripes.as_ref();
        let luts = self.luts.as_ref()?;
        let target: &mut Vec<u8> = if rotated {
            self.scratch.resize(len, 0);
            &mut self.scratch
        } else {
            Arc::get_mut(&mut out).expect("fresh or recycled buffer is unique")
        };
        if !soft::convert_rgba(img, dw, luts, opts, target, pool) {
            return None;
        }
        if rotated {
            rotate_rgba(&self.scratch, dw, dh, geo.quarter_turns, Arc::get_mut(&mut out).expect("unique"));
        }
        self.recycle(&out);
        let (ow, oh) = geo.display_size();
        Some((ow, oh, out))
    }

    /// Buffers kept per length: every frame that can be in flight with double-rate fields (the queue of
    /// `2 × FRAME_QUEUE` items, the frame the UI holds and the one being converted).
    const POOL_PER_LENGTH: usize = 2 * super::session::FRAME_QUEUE + 2;

    fn buffer(&mut self, len: usize) -> Arc<Vec<u8>> {
        // Reuse a buffer nobody else holds any more.
        if let Some((_, bufs)) = self.pool.iter_mut().find(|(l, _)| *l == len)
            && let Some(i) = bufs.iter().position(|b| Arc::strong_count(b) == 1 && Arc::weak_count(b) == 0)
        {
            return bufs.swap_remove(i);
        }
        Arc::new(vec![0u8; len])
    }

    fn recycle(&mut self, buf: &Arc<Vec<u8>>) {
        let len = buf.len();
        let i = match self.pool.iter().position(|(l, _)| *l == len) {
            Some(i) => i,
            None => {
                self.pool.push((len, Vec::new()));
                self.pool.len() - 1
            }
        };
        // Most recently used length first; a third length drops the oldest.
        let entry = self.pool.remove(i);
        self.pool.insert(0, entry);
        self.pool.truncate(2);
        let bufs = &mut self.pool[0].1;
        if bufs.len() < Self::POOL_PER_LENGTH {
            bufs.push(buf.clone());
        }
    }

    /// Converts `frame` to RGBA at its coded size, unrotated (the GPU path scales and rotates it); `None` when swscale
    /// cannot handle it.
    pub unsafe fn convert_packed(&mut self, frame: *const ffi::AVFrame) -> Option<(u32, u32, Arc<Vec<u8>>)> {
        unsafe {
            let (w, h) = ((*frame).width, (*frame).height);
            if w <= 0 || h <= 0 {
                return None;
            }
            self.convert(frame, Geometry { width: w as u32, height: h as u32, quarter_turns: 0 })
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

/// The planar layout of a decoder pixel format the UI can convert on the GPU (`None`: swscale converts it to RGBA).
/// The second value says whether the format implies full range (the deprecated `yuvj*` formats).
///
/// Read from the format's descriptor: little-endian integer YUV or gray at 8–16 bits, with any chroma subsampling up
/// to 4:1:0, as three planes (an alpha plane after them is ignored), as luma + interleaved chroma (either order), or
/// as luma only. RGB, palette, Bayer, float, big-endian, bitstream and packed formats are not planar here.
pub(crate) fn planar_format(format: c_int) -> Option<(PlanarFormat, bool)> {
    use ffi::AVPixelFormat as F;
    if format < 0 {
        return None;
    }
    let f: F = unsafe { std::mem::transmute::<c_int, F>(format) };
    let desc = unsafe { ffi::av_pix_fmt_desc_get(f) };
    if desc.is_null() {
        return None;
    }
    let d = unsafe { &*desc };
    let excluded = (ffi::AV_PIX_FMT_FLAG_BE
        | ffi::AV_PIX_FMT_FLAG_PAL
        | ffi::AV_PIX_FMT_FLAG_BITSTREAM
        | ffi::AV_PIX_FMT_FLAG_HWACCEL
        | ffi::AV_PIX_FMT_FLAG_RGB
        | ffi::AV_PIX_FMT_FLAG_BAYER
        | ffi::AV_PIX_FMT_FLAG_FLOAT
        | ffi::AV_PIX_FMT_FLAG_XYZ) as u64;
    // Gray formats are one plane without the planar flag; everything else must be planar.
    if d.flags & excluded != 0 || (d.flags & ffi::AV_PIX_FMT_FLAG_PLANAR as u64 == 0 && d.nb_components > 2) {
        return None;
    }
    let (sx, sy) = (d.log2_chroma_w, d.log2_chroma_h);
    if sx > 2 || sy > 2 {
        return None;
    }
    let y = d.comp[0];
    let depth = y.depth;
    if !(8..=16).contains(&depth) || y.plane != 0 || y.offset != 0 {
        return None;
    }
    let bytes = if depth > 8 { 2 } else { 1 };
    // The value sits either in the low bits of its element or right at the top (`p010`: 10 bits shifted by 6).
    let msb = match y.shift {
        0 => false,
        s if depth > 8 && s + depth == 16 => true,
        _ => return None,
    };
    let comps = d.nb_components;
    let has_alpha = d.flags & ffi::AV_PIX_FMT_FLAG_ALPHA as u64 != 0;
    let color_comps = if has_alpha { comps - 1 } else { comps };
    let same = |c: ffi::AVComponentDescriptor| c.depth == depth && c.shift == y.shift;
    let layout = match color_comps {
        1 if y.step == bytes => (PlaneLayout::Gray, false),
        3 => {
            let (u, v) = (d.comp[1], d.comp[2]);
            if !same(u) || !same(v) || y.step != bytes {
                return None;
            }
            match (u.plane, v.plane) {
                (1, 2) if u.step == bytes && v.step == bytes && u.offset == 0 && v.offset == 0 => (PlaneLayout::Planar, false),
                (1, 1) if u.step == 2 * bytes && v.step == 2 * bytes && u.offset + v.offset == bytes => (PlaneLayout::SemiPlanar, u.offset > v.offset),
                _ => return None,
            }
        }
        _ => return None,
    };
    let full = matches!(f, F::AV_PIX_FMT_YUVJ420P | F::AV_PIX_FMT_YUVJ422P | F::AV_PIX_FMT_YUVJ444P | F::AV_PIX_FMT_YUVJ440P | F::AV_PIX_FMT_YUVJ411P);
    let chroma_shift = if layout.0 == PlaneLayout::Gray { (0, 0) } else { (sx, sy) };
    Some((PlanarFormat { layout: layout.0, bits: depth as u8, msb_aligned: msb, chroma_shift, swap_uv: layout.1 }, full))
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

/// The transfer characteristic of a frame (only PQ and HLG are treated differently from SDR).
pub(crate) fn transfer_of(trc: ffi::AVColorTransferCharacteristic) -> Transfer {
    match trc {
        ffi::AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084 => Transfer::Pq,
        ffi::AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67 => Transfer::Hlg,
        _ => Transfer::Sdr,
    }
}

/// The primaries of a frame. Untagged frames are BT.709, except that PQ/HLG frames default to BT.2020 (what HDR
/// is graded in) and BT.2020-matrix frames follow their matrix.
pub(crate) fn primaries_of(pri: ffi::AVColorPrimaries, transfer: Transfer, matrix: YuvMatrix) -> Primaries {
    match pri {
        ffi::AVColorPrimaries::AVCOL_PRI_BT2020 => Primaries::Bt2020,
        ffi::AVColorPrimaries::AVCOL_PRI_SMPTE432 => Primaries::DisplayP3,
        ffi::AVColorPrimaries::AVCOL_PRI_UNSPECIFIED | ffi::AVColorPrimaries::AVCOL_PRI_RESERVED0 | ffi::AVColorPrimaries::AVCOL_PRI_RESERVED
            if transfer != Transfer::Sdr || matrix == YuvMatrix::Bt2020 =>
        {
            Primaries::Bt2020
        }
        _ => Primaries::Bt709,
    }
}

/// `AVMasteringDisplayMetadata` (libavutil/mastering_display_metadata.h; not in the generated bindings).
#[repr(C)]
struct MasteringDisplay {
    display_primaries: [[ffi::AVRational; 2]; 3],
    white_point: [ffi::AVRational; 2],
    min_luminance: ffi::AVRational,
    max_luminance: ffi::AVRational,
    has_primaries: c_int,
    has_luminance: c_int,
}

/// `AVContentLightMetadata`.
#[repr(C)]
struct ContentLight {
    max_cll: std::ffi::c_uint,
    max_fall: std::ffi::c_uint,
}

/// HDR light levels of a stream (nits), from the stream's side data and refreshed by the frames that carry them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct HdrLevels {
    pub max_cll: Option<f32>,
    pub mastering_max: Option<f32>,
}

impl HdrLevels {
    unsafe fn parse(mastering: Option<(*const u8, usize)>, light: Option<(*const u8, usize)>) -> Self {
        unsafe {
            let mut out = Self::default();
            if let Some((p, n)) = mastering
                && !p.is_null()
                && n >= std::mem::size_of::<MasteringDisplay>()
            {
                let m = std::ptr::read_unaligned(p as *const MasteringDisplay);
                if m.has_luminance != 0 && m.max_luminance.den > 0 && m.max_luminance.num > 0 {
                    out.mastering_max = Some(m.max_luminance.num as f32 / m.max_luminance.den as f32);
                }
            }
            if let Some((p, n)) = light
                && !p.is_null()
                && n >= std::mem::size_of::<ContentLight>()
            {
                let l = std::ptr::read_unaligned(p as *const ContentLight);
                if l.max_cll > 0 {
                    out.max_cll = Some(l.max_cll as f32);
                }
            }
            out
        }
    }

    /// The levels a stream declares (codec parameters' side data).
    pub unsafe fn of_stream(stream: *const ffi::AVStream) -> Self {
        unsafe {
            let par = (*stream).codecpar;
            if par.is_null() {
                return Self::default();
            }
            let get = |t| {
                let sd = ffi::av_packet_side_data_get((*par).coded_side_data, (*par).nb_coded_side_data, t);
                (!sd.is_null()).then(|| ((*sd).data as *const u8, (*sd).size))
            };
            Self::parse(
                get(ffi::AVPacketSideDataType::AV_PKT_DATA_MASTERING_DISPLAY_METADATA),
                get(ffi::AVPacketSideDataType::AV_PKT_DATA_CONTENT_LIGHT_LEVEL),
            )
        }
    }

    /// Takes over the levels `frame` carries (decoders attach them to keyframes).
    pub unsafe fn update(&mut self, frame: *const ffi::AVFrame) {
        unsafe {
            let get = |t| {
                let sd = ffi::av_frame_get_side_data(frame, t);
                (!sd.is_null()).then(|| ((*sd).data as *const u8, (*sd).size))
            };
            let f = Self::parse(
                get(ffi::AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA),
                get(ffi::AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL),
            );
            if f.max_cll.is_some() {
                self.max_cll = f.max_cll;
            }
            if f.mastering_max.is_some() {
                self.mastering_max = f.mastering_max;
            }
        }
    }

    pub fn peak_nits(&self) -> f32 {
        crate::color::hdr_peak_nits(self.max_cll, self.mastering_max)
    }
}

/// The colour description of `props` (the decoded frame; the hardware frame for a download).
pub(crate) unsafe fn frame_color(props: *const ffi::AVFrame, full_range: bool, hdr: &HdrLevels) -> ColorInfo {
    unsafe {
        let matrix = frame_matrix(props);
        let transfer = transfer_of((*props).color_trc);
        ColorInfo { matrix, full_range, transfer, primaries: primaries_of((*props).color_primaries, transfer, matrix), peak_nits: hdr.peak_nits() }
    }
}

/// Whether `props` holds two fields captured at different times, and their order. `SUBTITLE_ADDER_DEINTERLACE=0`
/// reports every frame as progressive (shown woven; for comparisons).
pub(crate) unsafe fn frame_interlace(props: *const ffi::AVFrame) -> Option<Interlace> {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| {
        let v = std::env::var("SUBTITLE_ADDER_DEINTERLACE").unwrap_or_default().to_ascii_lowercase();
        !matches!(v.trim(), "0" | "off" | "no" | "false")
    });
    if !enabled {
        return None;
    }
    unsafe {
        let flags = (*props).flags;
        (flags & ffi::AV_FRAME_FLAG_INTERLACED != 0).then_some(Interlace { top_field_first: flags & ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST != 0 })
    }
}

/// Whether pixel format `format` carries an alpha component.
fn has_alpha(format: c_int) -> bool {
    if format < 0 {
        return false;
    }
    let desc = unsafe { ffi::av_pix_fmt_desc_get(std::mem::transmute::<c_int, ffi::AVPixelFormat>(format)) };
    !desc.is_null() && unsafe { (*desc).flags } & ffi::AV_PIX_FMT_FLAG_ALPHA as u64 != 0
}

/// The planes of `frame` as `format` lays them out (`None`: a plane is missing, or a stride is negative or shorter
/// than its row).
unsafe fn frame_planes(frame: *const ffi::AVFrame, format: &PlanarFormat) -> Option<[Option<(*const u8, PlaneInfo)>; 3]> {
    unsafe {
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
        for (i, plane) in planes.iter_mut().enumerate().take(format.plane_count()) {
            let data = (*frame).data[i];
            let stride = (*frame).linesize[i];
            if data.is_null() || stride <= 0 {
                return None;
            }
            let (pw, rows) = if i == 0 { (w, h) } else { (cw, ch) };
            let row_bytes = pw as usize * format.components(i) * sb;
            if (stride as usize) < row_bytes {
                return None;
            }
            *plane = Some((data as *const u8, PlaneInfo { width: pw, rows, stride: stride as usize, row_bytes }));
        }
        Some(planes)
    }
}

/// Describes the planes of `owner` (a software frame) for the GPU, or `None` when its format or layout is not one
/// the UI converts (negative strides, unknown formats). `props` is the frame the color properties are read from
/// (the hardware frame when `owner` is its download).
pub(crate) unsafe fn planar_image(
    owner: Box<dyn Send + Sync>,
    frame: *const ffi::AVFrame,
    props: *const ffi::AVFrame,
    geo: Geometry,
    hdr: &HdrLevels,
) -> Option<PlanarImage> {
    unsafe {
        let (format, forced_full) = planar_format((*frame).format)?;
        let (w, h) = ((*frame).width, (*frame).height);
        if w <= 0 || h <= 0 {
            return None;
        }
        let (w, h) = (w as u32, h as u32);
        let planes = frame_planes(frame, &format)?;
        // Untagged gray has the range the RGBA path gives it (measured, see the test
        // `untagged_gray_has_the_range_the_rgba_path_gives_it`): swscale never applies a range conversion to 8-bit
        // gray (the `full_range = 0` passed to `sws_setColorspaceDetails` for untagged frames has no effect on it), so
        // untagged 8-bit gray is full range, while untagged 9–16-bit gray is limited. A frame tagged limited is shown
        // limited here even at 8 bits, where swscale (the RGBA path) still shows it full range.
        let full_range = match (*props).color_range {
            ffi::AVColorRange::AVCOL_RANGE_JPEG => true,
            ffi::AVColorRange::AVCOL_RANGE_MPEG => forced_full,
            _ => forced_full || (format.layout == PlaneLayout::Gray && format.bits == 8),
        };
        let color = frame_color(props, full_range, hdr);
        Some(PlanarImage::new(format, (w, h), color, geo.quarter_turns % 4, frame_interlace(props), planes, owner))
    }
}

/// A packed RGBA image (swscale's conversion of a frame the planar path cannot describe) for the GPU: coded size,
/// rotation and interlacing as the frame, colour as `props` says (the R'G'B' signal keeps its transfer function).
pub(crate) unsafe fn packed_image(rgba: Arc<Vec<u8>>, size: (u32, u32), props: *const ffi::AVFrame, geo: Geometry, hdr: &HdrLevels) -> Option<PlanarImage> {
    unsafe {
        let (w, h) = size;
        let row = w as usize * 4;
        if w == 0 || h == 0 || rgba.len() < row * h as usize {
            return None;
        }
        let format = PlanarFormat { layout: PlaneLayout::Packed, bits: 8, msb_aligned: false, chroma_shift: (0, 0), swap_uv: false };
        let planes = [Some((rgba.as_ptr(), PlaneInfo { width: w, rows: h, stride: row, row_bytes: row })), None, None];
        let color = frame_color(props, true, hdr);
        Some(PlanarImage::new(format, size, color, geo.quarter_turns % 4, frame_interlace(props), planes, Box::new(rgba)))
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
    fn planar_formats_come_from_the_pixel_format_descriptors() {
        use PlaneLayout::{Gray, Planar, SemiPlanar};
        use ffi::AVPixelFormat as F;
        let f = |p: F| planar_format(p as c_int).map(|(f, full)| (f.layout, f.bits, f.msb_aligned, f.chroma_shift, f.swap_uv, full));
        assert_eq!(f(F::AV_PIX_FMT_YUV420P), Some((Planar, 8, false, (1, 1), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_YUVJ420P), Some((Planar, 8, false, (1, 1), false, true)));
        assert_eq!(f(F::AV_PIX_FMT_YUVJ440P), Some((Planar, 8, false, (0, 1), false, true)));
        assert_eq!(f(F::AV_PIX_FMT_YUV410P), Some((Planar, 8, false, (2, 2), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_YUV411P), Some((Planar, 8, false, (2, 0), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_YUV420P9LE), Some((Planar, 9, false, (1, 1), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_YUV422P10LE), Some((Planar, 10, false, (1, 0), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_YUV444P14LE), Some((Planar, 14, false, (0, 0), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_YUV444P16LE), Some((Planar, 16, false, (0, 0), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_YUVA444P10LE), Some((Planar, 10, false, (0, 0), false, false)), "alpha is ignored");
        assert_eq!(f(F::AV_PIX_FMT_YUVA420P), Some((Planar, 8, false, (1, 1), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_NV12), Some((SemiPlanar, 8, false, (1, 1), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_NV21), Some((SemiPlanar, 8, false, (1, 1), true, false)));
        assert_eq!(f(F::AV_PIX_FMT_NV16), Some((SemiPlanar, 8, false, (1, 0), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_P010LE), Some((SemiPlanar, 10, true, (1, 1), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_P210LE), Some((SemiPlanar, 10, true, (1, 0), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_P016LE), Some((SemiPlanar, 16, false, (1, 1), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_GRAY8), Some((Gray, 8, false, (0, 0), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_GRAY10LE), Some((Gray, 10, false, (0, 0), false, false)));
        assert_eq!(f(F::AV_PIX_FMT_GRAY16LE), Some((Gray, 16, false, (0, 0), false, false)));
        for other in [
            F::AV_PIX_FMT_RGB24,
            F::AV_PIX_FMT_RGBA,
            F::AV_PIX_FMT_GBRP,
            F::AV_PIX_FMT_GBRP10LE,
            F::AV_PIX_FMT_PAL8,
            F::AV_PIX_FMT_YUV420P10BE,
            F::AV_PIX_FMT_GRAY16BE,
            F::AV_PIX_FMT_YUYV422,
            F::AV_PIX_FMT_UYVY422,
            F::AV_PIX_FMT_YA8,
            F::AV_PIX_FMT_BAYER_RGGB8,
            F::AV_PIX_FMT_GRAYF32LE,
            F::AV_PIX_FMT_MONOBLACK,
            F::AV_PIX_FMT_D3D11,
            F::AV_PIX_FMT_VIDEOTOOLBOX,
            F::AV_PIX_FMT_XYZ12LE,
        ] {
            assert_eq!(f(other), None, "{other:?}");
        }
        assert_eq!(planar_format(-1).map(|f| f.0), None);
    }

    #[test]
    fn hdr_streams_default_to_bt2020_primaries_and_sdr_ones_to_bt709() {
        use ffi::AVColorPrimaries as P;
        assert_eq!(primaries_of(P::AVCOL_PRI_UNSPECIFIED, Transfer::Pq, YuvMatrix::Bt709), Primaries::Bt2020);
        assert_eq!(primaries_of(P::AVCOL_PRI_UNSPECIFIED, Transfer::Sdr, YuvMatrix::Bt709), Primaries::Bt709);
        assert_eq!(primaries_of(P::AVCOL_PRI_UNSPECIFIED, Transfer::Sdr, YuvMatrix::Bt2020), Primaries::Bt2020);
        assert_eq!(primaries_of(P::AVCOL_PRI_BT709, Transfer::Pq, YuvMatrix::Bt2020), Primaries::Bt709);
        assert_eq!(primaries_of(P::AVCOL_PRI_SMPTE170M, Transfer::Sdr, YuvMatrix::Bt601), Primaries::Bt709);
        assert_eq!(primaries_of(P::AVCOL_PRI_SMPTE432, Transfer::Sdr, YuvMatrix::Bt709), Primaries::DisplayP3);
        use ffi::AVColorTransferCharacteristic as T;
        assert_eq!(transfer_of(T::AVCOL_TRC_SMPTE2084), Transfer::Pq);
        assert_eq!(transfer_of(T::AVCOL_TRC_ARIB_STD_B67), Transfer::Hlg);
        assert_eq!(transfer_of(T::AVCOL_TRC_BT709), Transfer::Sdr);
        assert_eq!(transfer_of(T::AVCOL_TRC_UNSPECIFIED), Transfer::Sdr);
    }

    #[test]
    fn hdr_levels_are_read_from_side_data_structs() {
        let m = MasteringDisplay {
            display_primaries: [[ffi::AVRational { num: 0, den: 1 }; 2]; 3],
            white_point: [ffi::AVRational { num: 0, den: 1 }; 2],
            min_luminance: ffi::AVRational { num: 1, den: 10000 },
            max_luminance: ffi::AVRational { num: 4000, den: 1 },
            has_primaries: 0,
            has_luminance: 1,
        };
        let l = ContentLight { max_cll: 1200, max_fall: 400 };
        let levels = unsafe {
            HdrLevels::parse(
                Some((&m as *const MasteringDisplay as *const u8, std::mem::size_of::<MasteringDisplay>())),
                Some((&l as *const ContentLight as *const u8, std::mem::size_of::<ContentLight>())),
            )
        };
        assert_eq!(levels, HdrLevels { max_cll: Some(1200.0), mastering_max: Some(4000.0) });
        assert_eq!(levels.peak_nits(), 1200.0);
        // Truncated side data is ignored.
        let short = unsafe { HdrLevels::parse(Some((&m as *const MasteringDisplay as *const u8, 8)), None) };
        assert_eq!(short, HdrLevels::default());
        assert_eq!(short.peak_nits(), crate::color::DEFAULT_PEAK_NITS);
    }

    /// A `w`×1 gray frame of `fmt` holding `values` (in the format's own scale), with `range`.
    fn gray_frame(fmt: ffmpeg_next::format::Pixel, values: &[u16], range: ffi::AVColorRange) -> ffmpeg_next::frame::Video {
        let mut f = ffmpeg_next::frame::Video::new(fmt, values.len() as u32, 2);
        let wide = planar_format(unsafe { (*f.as_ptr()).format }).is_some_and(|(p, _)| p.bits > 8);
        let stride = f.stride(0);
        let data = f.data_mut(0);
        for row in 0..2 {
            for (i, v) in values.iter().enumerate() {
                if wide {
                    data[row * stride + i * 2..row * stride + i * 2 + 2].copy_from_slice(&v.to_le_bytes());
                } else {
                    data[row * stride + i] = *v as u8;
                }
            }
        }
        unsafe { (*f.as_mut_ptr()).color_range = range };
        f
    }

    /// What swscale's RGBA path makes of untagged gray, and the planar path's range for the same frame.
    #[test]
    fn untagged_gray_has_the_range_the_rgba_path_gives_it() {
        use ffmpeg_next::format::Pixel;
        let _ = ffmpeg_next::init();
        for (fmt, scale) in [(Pixel::GRAY8, 1u16), (Pixel::GRAY10LE, 4), (Pixel::GRAY16LE, 257)] {
            for range in [ffi::AVColorRange::AVCOL_RANGE_UNSPECIFIED, ffi::AVColorRange::AVCOL_RANGE_MPEG, ffi::AVColorRange::AVCOL_RANGE_JPEG] {
                let values: Vec<u16> = [0u16, 16, 128, 235, 255].iter().map(|v| v * scale).collect();
                let f = gray_frame(fmt, &values, range);
                let mut conv = Converter::new();
                let (w, _, rgba) = unsafe { conv.convert(f.as_ptr(), Geometry { width: values.len() as u32, height: 2, quarter_turns: 0 }) }.expect("swscale");
                let reds: Vec<u8> = (0..w as usize).map(|i| rgba[i * 4]).collect();
                let owner = unsafe { crate::ffmpeg_backend::hw::OwnedFrame::reference(f.as_ptr()) }.expect("ref");
                let img = unsafe { planar_image(Box::new(owner), f.as_ptr(), f.as_ptr(), Geometry { width: w, height: 2, quarter_turns: 0 }, &HdrLevels::default()) }
                    .expect("planar");
                // Limited range maps 16 to black and 235 to white; full range keeps 16 dark gray.
                let sws_full = reds[1] > 8;
                if fmt == Pixel::GRAY8 && range == ffi::AVColorRange::AVCOL_RANGE_MPEG {
                    // swscale ignores the range of 8-bit gray (always full); the planar path follows the tag.
                    assert!(sws_full && !img.full_range, "swscale gave {reds:?}");
                } else {
                    assert_eq!(img.full_range, sws_full, "{fmt:?} {range:?}: swscale gave {reds:?}");
                }
            }
        }
    }

    /// An interlaced SDR frame with alpha (transparent here) is deinterlaced and shown opaque, as the shader shows it:
    /// the deinterlaced copy has no alpha plane for swscale to copy.
    #[test]
    fn interlaced_alpha_frames_are_shown_opaque() {
        use ffmpeg_next::format::Pixel;
        let _ = ffmpeg_next::init();
        for fmt in [Pixel::YUVA420P, Pixel::YUVA444P10LE] {
            let (w, h) = (16u32, 8u32);
            let mut f = ffmpeg_next::frame::Video::new(fmt, w, h);
            let wide = fmt == Pixel::YUVA444P10LE;
            for i in 0..4 {
                let stride = f.stride(i);
                let rows = if i == 1 || i == 2 { f.plane_height(i) } else { h } as usize;
                let value: u16 = match (i, wide) {
                    (3, _) => 0,
                    (_, false) => 128,
                    (_, true) => 512,
                };
                let data = f.data_mut(i);
                for r in 0..rows {
                    for x in 0..stride / if wide { 2 } else { 1 } {
                        if wide {
                            data[r * stride + 2 * x..r * stride + 2 * x + 2].copy_from_slice(&value.to_le_bytes());
                        } else {
                            data[r * stride + x] = value as u8;
                        }
                    }
                }
            }
            unsafe { (*f.as_mut_ptr()).flags |= ffi::AV_FRAME_FLAG_INTERLACED | ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST };
            let geo = Geometry { width: w, height: h, quarter_turns: 0 };
            let mut conv = Converter::new();
            let soft = unsafe { conv.soft_source(f.as_ptr(), f.as_ptr(), None, geo, &HdrLevels::default()) };
            assert!(soft.interlaced && !soft.tone_map && soft.image.is_some(), "{fmt:?}");
            for second in [false, true] {
                let (ow, oh, rgba) = unsafe { conv.convert_soft(&soft, f.as_ptr(), geo, None, second) }.expect("converted");
                assert_eq!((ow, oh), (w, h));
                assert!(rgba.chunks(4).all(|p| p[3] == 255), "{fmt:?}: alpha {:?}", rgba.chunks(4).map(|p| p[3]).collect::<Vec<_>>());
                // Mid gray.
                assert!(rgba.chunks(4).all(|p| (120..=136).contains(&p[1])), "{fmt:?}");
            }
        }
    }

    #[test]
    fn buffers_are_pooled_per_length() {
        let mut conv = Converter::new();
        let a = conv.buffer(16);
        conv.recycle(&a);
        let b = conv.buffer(32);
        conv.recycle(&b);
        drop((a, b));
        // Both lengths are still pooled after alternating.
        let a2 = conv.buffer(16);
        assert_eq!(a2.len(), 16);
        conv.recycle(&a2);
        let b2 = conv.buffer(32);
        conv.recycle(&b2);
        assert_eq!(conv.pool.len(), 2);
        assert!(conv.pool.iter().all(|(_, bufs)| bufs.len() == 1), "reused, not reallocated");
        // A third length drops the least recently used one.
        let c = conv.buffer(64);
        conv.recycle(&c);
        assert_eq!(conv.pool.iter().map(|(l, _)| *l).collect::<Vec<_>>(), [64, 32]);
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
