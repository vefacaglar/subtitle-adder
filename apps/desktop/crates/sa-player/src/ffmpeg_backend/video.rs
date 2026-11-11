//! Video frame description and conversion: the planes of a decoded frame for the GPU path (format, colour, HDR
//! levels, interlacing), or swscale's RGBA8 conversion of any pixel format (at the display size and rotated for the
//! RGBA path; unscaled for the GPU path's packed images). Output buffers are recycled once the UI dropped them.

use std::ffi::c_int;
use std::ptr;
use std::sync::Arc;

use ffmpeg_next::ffi;

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
        // Untagged 8-bit gray is full range (as swscale, and so the RGBA path, treats it; deeper gray is limited).
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
