//! Hardware video decoding: D3D11VA on Windows, VideoToolbox on macOS (nothing elsewhere), as Media Foundation and
//! AVFoundation decode on the GPU. A stream is decoded in hardware when it is 4:2:0 at 8 or 10 bits (what the GPU
//! decoders handle) and a native decoder of its codec has a device-type hardware config for the platform
//! ([`decoder_for`]): the codec's default decoder when it has one, else another native decoder of the same codec
//! (AV1: the default `libdav1d` has none, the native `av1` decoder has one). Wrappers of external libraries and
//! vendor decoders are never used. Otherwise, and when the device cannot be created, the stream is decoded in software.
//!
//! The decode thread (`session::video_main`) falls back to the software (default) decoder, restarting from the
//! keyframe before the current position, when the hardware decoder:
//! * hands over a software frame (the GPU cannot decode this profile; FFmpeg's default decoders then decode in
//!   software, single-threaded);
//! * fails on its first packet while it is a hardware-only alternative decoder (AV1's native decoder has no software
//!   path: a GPU without AV1 decoding fails with ENOSYS), which is also remembered for the rest of the process, so
//!   later loads of that codec go straight to software;
//! * fails three times in a row without a frame in between (a single damaged packet, such as the start of a TS
//!   capture in the middle of a GOP, does not end hardware decoding);
//! * takes many packets (at least 60, or 2 s of stream) after a flush without handing over any frame;
//! * hands over a frame that cannot be downloaded.
//!
//! `SUBTITLE_ADDER_HWDEC=0` turns hardware decoding off. `SUBTITLE_ADDER_HWDEC_FAIL=1` (diagnostics) makes every
//! hardware decoder fail as a GPU without support for the codec would (each packet errors with ENOSYS), to exercise
//! the fall-back; such simulated failures are not remembered. Tests use `FfmpegPlayer::simulate_hardware_failure`.
//!
//! Decoded GPU surfaces are downloaded once (NV12 / P010), with no per-frame buffer allocation: on Windows into a
//! ring of staging textures that stay mapped while the frame is held (`staging`: the GPU copies, the CPU reads the
//! mapping in place), otherwise (`SUBTITLE_ADDER_ZEROCOPY=0`, no free slot, a failure; macOS) by
//! `av_hwframe_transfer_data` into reusable system-memory frames. The UI uploads those planes and converts them to
//! RGB on the GPU.

use std::ffi::c_int;
use std::ptr;
use std::sync::{Arc, OnceLock};

use ffmpeg_next::ffi;
use ffmpeg_next::ffi::AVPixelFormat;

/// An owned `AVFrame` (freed on drop). Shared read-only once filled.
pub(crate) struct OwnedFrame(pub *mut ffi::AVFrame);

// SAFETY: the frame is only written by the thread that fills it, before it is shared; afterwards it is read-only.
unsafe impl Send for OwnedFrame {}
unsafe impl Sync for OwnedFrame {}

impl OwnedFrame {
    pub fn alloc() -> Option<Self> {
        let f = unsafe { ffi::av_frame_alloc() };
        (!f.is_null()).then_some(Self(f))
    }

    /// A new reference to `src`'s buffers (no pixel copy).
    pub unsafe fn reference(src: *const ffi::AVFrame) -> Option<Self> {
        let f = Self::alloc()?;
        if unsafe { ffi::av_frame_ref(f.0, src) } < 0 {
            return None;
        }
        Some(f)
    }
}

impl Drop for OwnedFrame {
    fn drop(&mut self) {
        unsafe { ffi::av_frame_free(&mut self.0) };
    }
}

struct Device(*mut ffi::AVBufferRef);

// SAFETY: FFmpeg device contexts are thread-safe to reference (D3D11VA and VideoToolbox lock internally).
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

#[cfg(windows)]
const PLATFORM: Option<(ffi::AVHWDeviceType, AVPixelFormat)> =
    Some((ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA, AVPixelFormat::AV_PIX_FMT_D3D11));
#[cfg(target_os = "macos")]
const PLATFORM: Option<(ffi::AVHWDeviceType, AVPixelFormat)> =
    Some((ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX, AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX));
#[cfg(not(any(windows, target_os = "macos")))]
const PLATFORM: Option<(ffi::AVHWDeviceType, AVPixelFormat)> = None;

/// Whether hardware decoding is allowed in this process (`SUBTITLE_ADDER_HWDEC=0|off|no|false` turns it off).
pub(crate) fn allowed() -> bool {
    static ALLOWED: OnceLock<bool> = OnceLock::new();
    *ALLOWED.get_or_init(|| {
        let v = std::env::var("SUBTITLE_ADDER_HWDEC").unwrap_or_default().to_ascii_lowercase();
        !matches!(v.trim(), "0" | "off" | "no" | "false")
    })
}

/// `SUBTITLE_ADDER_HWDEC_FAIL=1`: simulate a failing hardware decoder (see the module docs).
pub(crate) fn simulate_failure_from_env() -> bool {
    static FAIL: OnceLock<bool> = OnceLock::new();
    *FAIL.get_or_init(|| {
        let v = std::env::var("SUBTITLE_ADDER_HWDEC_FAIL").unwrap_or_default().to_ascii_lowercase();
        matches!(v.trim(), "1" | "on" | "yes" | "true")
    })
}

/// Codecs whose hardware-only alternative decoder failed in this process.
static FAILED_HARDWARE_ONLY: std::sync::Mutex<Vec<ffi::AVCodecID>> = std::sync::Mutex::new(Vec::new());

/// Remembers that the hardware-only decoder of `id` failed: later loads decode `id` in software at once.
pub(crate) fn remember_hardware_only_failure(id: ffi::AVCodecID) {
    let mut f = FAILED_HARDWARE_ONLY.lock().unwrap_or_else(|e| e.into_inner());
    if !f.contains(&id) {
        f.push(id);
    }
}

fn hardware_only_failed(id: ffi::AVCodecID) -> bool {
    FAILED_HARDWARE_ONLY.lock().unwrap_or_else(|e| e.into_inner()).contains(&id)
}

/// Creates the device on a background thread (it takes 0.2–1 s on first use: driver and GPU setup), so the first
/// load opens its source meanwhile instead of waiting for it first.
pub(crate) fn warm_up() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    if PLATFORM.is_some() && allowed() {
        STARTED.call_once(|| {
            let _ = std::thread::Builder::new().name("player-hw-init".into()).spawn(|| {
                let _ = device();
            });
        });
    }
}

/// The process-wide device (created once; `None` when the platform has none or creation failed).
fn device() -> Option<&'static Device> {
    static DEVICE: OnceLock<Option<Device>> = OnceLock::new();
    DEVICE
        .get_or_init(|| {
            let (ty, _) = PLATFORM?;
            let mut dev: *mut ffi::AVBufferRef = ptr::null_mut();
            let r = unsafe { ffi::av_hwdevice_ctx_create(&mut dev, ty, ptr::null(), ptr::null_mut(), 0) };
            if r < 0 || dev.is_null() {
                eprintln!("[player] hardware decoding unavailable (device error {r})");
                return None;
            }
            Some(Device(dev))
        })
        .as_ref()
}

/// Whether the platform's hardware frames are what `format` is.
pub(crate) fn is_hw_format(format: c_int) -> bool {
    PLATFORM.is_some_and(|(_, f)| f as c_int == format)
}

/// Whether a stream of `codec` with codec parameters `par` should be decoded in hardware.
pub(crate) unsafe fn suitable(codec: *const ffi::AVCodec, par: *const ffi::AVCodecParameters) -> bool {
    let Some((ty, hw_fmt)) = PLATFORM else { return false };
    if codec.is_null() || par.is_null() {
        return false;
    }
    unsafe {
        // GPU decoders handle 4:2:0 at 8 and 10 bits; anything else (4:2:2, 4:4:4, 12-bit) would fall back inside
        // the decoder after it was opened single-threaded.
        let sw_ok = matches!(
            std::mem::transmute::<c_int, AVPixelFormat>((*par).format),
            AVPixelFormat::AV_PIX_FMT_NONE
                | AVPixelFormat::AV_PIX_FMT_YUV420P
                | AVPixelFormat::AV_PIX_FMT_YUVJ420P
                | AVPixelFormat::AV_PIX_FMT_YUV420P10LE
                | AVPixelFormat::AV_PIX_FMT_NV12
                | AVPixelFormat::AV_PIX_FMT_P010LE
        );
        if !sw_ok {
            return false;
        }
        let mut i = 0;
        loop {
            let cfg = ffi::avcodec_get_hw_config(codec, i);
            if cfg.is_null() {
                return false;
            }
            if (*cfg).methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int != 0
                && (*cfg).device_type == ty
                && (*cfg).pix_fmt == hw_fmt
            {
                return true;
            }
            i += 1;
        }
    }
}

/// The decoder to decode a stream with codec parameters `par` in hardware: the codec's default decoder when it suits
/// ([`suitable`]), else the first other native decoder of the same codec that does (AV1: the default `libdav1d` has
/// no hardware config, the native `av1` decoder has one). Wrappers of external libraries and vendor decoders
/// (`*_cuvid`, `*_qsv`, `*_amf`, ...) are never picked. `None`: decode in software (with the default decoder). The
/// alternative decoder is skipped for a codec whose hardware-only decoder already failed in this process.
pub(crate) unsafe fn decoder_for(par: *const ffi::AVCodecParameters) -> Option<HwDecoder> {
    unsafe { pick_decoder(par, allowed(), hardware_only_failed) }
}

/// The decoder [`decoder_for`] picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HwDecoder {
    pub codec: *const ffi::AVCodec,
    /// Not the codec's default decoder: one that decodes only on the GPU (it cannot fall back inside itself).
    pub hardware_only: bool,
}

/// [`decoder_for`] with the process state passed in (tests).
unsafe fn pick_decoder(par: *const ffi::AVCodecParameters, allowed: bool, failed: impl Fn(ffi::AVCodecID) -> bool) -> Option<HwDecoder> {
    if PLATFORM.is_none() || !allowed || par.is_null() {
        return None;
    }
    unsafe {
        let id = (*par).codec_id;
        let default = ffi::avcodec_find_decoder(id);
        if suitable(default, par) {
            return Some(HwDecoder { codec: default, hardware_only: false });
        }
        if failed(id) {
            return None;
        }
        let mut opaque: *mut std::ffi::c_void = ptr::null_mut();
        loop {
            let c = ffi::av_codec_iterate(&mut opaque);
            if c.is_null() {
                return None;
            }
            if c != default && (*c).id == id && ffi::av_codec_is_decoder(c) != 0 && (*c).wrapper_name.is_null() && suitable(c, par) {
                return Some(HwDecoder { codec: c, hardware_only: true });
            }
        }
    }
}

/// Attaches the device to a decoder context that has not been opened yet. False when there is no device.
pub(crate) unsafe fn attach(ctx: *mut ffi::AVCodecContext) -> bool {
    let Some(dev) = device() else { return false };
    unsafe {
        let r = ffi::av_buffer_ref(dev.0);
        if r.is_null() {
            return false;
        }
        (*ctx).hw_device_ctx = r;
        (*ctx).get_format = Some(get_format);
        // A few spare surfaces: frames queued for the presenter are downloaded first, so they hold none.
        (*ctx).extra_hw_frames = 2;
    }
    true
}

unsafe extern "C" fn get_format(ctx: *mut ffi::AVCodecContext, fmts: *const AVPixelFormat) -> AVPixelFormat {
    unsafe {
        if let Some((_, hw)) = PLATFORM {
            let mut p = fmts;
            while *p != AVPixelFormat::AV_PIX_FMT_NONE {
                if *p == hw {
                    return hw;
                }
                p = p.add(1);
            }
        }
        // Not offered (or its setup failed and the decoder asks again without it): software.
        ffi::avcodec_default_get_format(ctx, fmts)
    }
}

/// A hardware frame in system memory: a recycled frame `av_hwframe_transfer_data` copied it into, or (Windows) a
/// mapped staging texture of the ring (`staging`), which stays mapped and reserved until this is dropped. Either way
/// [`Downloaded::frame`] is a software frame (NV12 / P010) whose planes stay valid and unchanged while it lives.
pub(crate) struct Downloaded {
    frame: OwnedFrame,
    /// Dropped after `frame` (field order): the mapping its planes point into.
    #[cfg(windows)]
    _lease: Option<super::staging::Lease>,
}

impl Downloaded {
    fn copied(frame: OwnedFrame) -> Self {
        Self {
            frame,
            #[cfg(windows)]
            _lease: None,
        }
    }

    /// The frame (format, size, `data` / `linesize`; no colour properties, as `av_hwframe_transfer_data` leaves them).
    pub fn frame(&self) -> *mut ffi::AVFrame {
        self.frame.0
    }
}

/// Downloads GPU frames: on Windows into the mapped staging ring when it is on and has a free slot, otherwise into
/// recycled system-memory frames.
#[derive(Default)]
pub(crate) struct Download {
    pool: Vec<Arc<Downloaded>>,
    #[cfg(windows)]
    ring: super::staging::StagingRing,
}

const POOL: usize = 8;

impl Download {
    /// The software copy of hardware frame `hw` (NV12 / P010); `None` when the transfer failed.
    pub unsafe fn download(&mut self, hw: *const ffi::AVFrame) -> Option<Arc<Downloaded>> {
        unsafe {
            #[cfg(windows)]
            if super::staging::enabled()
                && let Some((frame, lease)) = self.ring.download(hw)
            {
                super::perf::count_staged(true);
                return Some(Arc::new(Downloaded { frame, _lease: Some(lease) }));
            }
            super::perf::count_staged(false);
            self.transfer(hw)
        }
    }

    /// [`Download::download`] through `av_hwframe_transfer_data` only.
    pub unsafe fn transfer(&mut self, hw: *const ffi::AVFrame) -> Option<Arc<Downloaded>> {
        unsafe {
            let (w, h) = ((*hw).width, (*hw).height);
            let frames = (*hw).hw_frames_ctx;
            if frames.is_null() {
                return None;
            }
            let sw_format = (*((*frames).data as *const ffi::AVHWFramesContext)).sw_format as c_int;
            // Reuse a frame nobody else holds any more (same size and format: the transfer writes into its buffers).
            let reuse = self.pool.iter().position(|f| {
                Arc::strong_count(f) == 1 && Arc::weak_count(f) == 0 && {
                    let p = f.frame();
                    (*p).width == w && (*p).height == h && (*p).format == sw_format && !(*p).buf[0].is_null()
                }
            });
            let dst = match reuse {
                Some(i) => self.pool.swap_remove(i),
                None => Arc::new(Downloaded::copied(OwnedFrame::alloc()?)),
            };
            if ffi::av_hwframe_transfer_data(dst.frame(), hw, 0) < 0 {
                return None;
            }
            self.pool.retain(|f| (*f.frame()).width == w && (*f.frame()).height == h);
            if self.pool.len() < POOL {
                self.pool.push(dst.clone());
            }
            Some(dst)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffi::AVCodecID as Id;

    struct Params(*mut ffi::AVCodecParameters);

    impl Params {
        fn new(id: Id, format: AVPixelFormat) -> Self {
            let p = unsafe { ffi::avcodec_parameters_alloc() };
            assert!(!p.is_null());
            unsafe {
                (*p).codec_type = ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
                (*p).codec_id = id;
                (*p).format = format as c_int;
                (*p).width = 1920;
                (*p).height = 1080;
            }
            Self(p)
        }
    }

    impl Drop for Params {
        fn drop(&mut self) {
            unsafe { ffi::avcodec_parameters_free(&mut self.0) };
        }
    }

    fn name(c: *const ffi::AVCodec) -> String {
        unsafe { std::ffi::CStr::from_ptr((*c).name).to_string_lossy().into_owned() }
    }

    const IDS: [Id; 8] = [
        Id::AV_CODEC_ID_H264,
        Id::AV_CODEC_ID_HEVC,
        Id::AV_CODEC_ID_AV1,
        Id::AV_CODEC_ID_VP9,
        Id::AV_CODEC_ID_VP8,
        Id::AV_CODEC_ID_MPEG2VIDEO,
        Id::AV_CODEC_ID_VC1,
        Id::AV_CODEC_ID_MPEG4,
    ];

    #[test]
    fn the_hardware_decoder_is_never_a_wrapper_and_av1_uses_the_native_decoder() {
        let _ = ffmpeg_next::init();
        for id in IDS {
            for fmt in [AVPixelFormat::AV_PIX_FMT_YUV420P, AVPixelFormat::AV_PIX_FMT_YUV420P10LE] {
                let par = Params::new(id, fmt);
                if let Some(d) = unsafe { pick_decoder(par.0, true, |_| false) } {
                    assert!(unsafe { (*d.codec).wrapper_name.is_null() }, "{id:?}: {}", name(d.codec));
                    assert_eq!(unsafe { (*d.codec).id }, id);
                    let default = unsafe { ffi::avcodec_find_decoder(id) };
                    assert_eq!(d.hardware_only, d.codec != default, "{id:?}");
                }
                // Hardware decoding off: the default (software) decoder.
                assert_eq!(unsafe { pick_decoder(par.0, false, |_| false) }, None, "{id:?}");
            }
        }
        // AV1: when the default decoder is a wrapper (libdav1d) and the native `av1` decoder has a hardware config for
        // this platform, the native one is picked, unless it failed before in this process.
        let par = Params::new(Id::AV_CODEC_ID_AV1, AVPixelFormat::AV_PIX_FMT_YUV420P10LE);
        let default = unsafe { ffi::avcodec_find_decoder(Id::AV_CODEC_ID_AV1) };
        let native = unsafe { ffi::avcodec_find_decoder_by_name(c"av1".as_ptr()) };
        let default_is_wrapper = !default.is_null() && unsafe { !(*default).wrapper_name.is_null() };
        let native_hw = !native.is_null() && unsafe { suitable(native, par.0) };
        let picked = unsafe { pick_decoder(par.0, true, |_| false) };
        eprintln!("AV1 default {:?}, native hardware config {native_hw}: picked {:?}", (!default.is_null()).then(|| name(default)), picked.map(|d| name(d.codec)));
        if default_is_wrapper && native_hw {
            assert_eq!(picked.map(|d| (name(d.codec), d.hardware_only)), Some(("av1".to_owned(), true)));
            assert_eq!(unsafe { pick_decoder(par.0, true, |id| id == Id::AV_CODEC_ID_AV1) }, None, "a remembered failure means software");
            assert!(unsafe { pick_decoder(par.0, true, |id| id == Id::AV_CODEC_ID_HEVC) }.is_some());
        }
        if PLATFORM.is_none() {
            assert_eq!(picked, None);
        }
    }

    #[test]
    fn a_hardware_only_failure_is_remembered_per_codec() {
        // A codec id no test decodes in hardware.
        let id = Id::AV_CODEC_ID_VC1;
        assert!(!hardware_only_failed(id));
        remember_hardware_only_failure(id);
        remember_hardware_only_failure(id);
        assert!(hardware_only_failed(id));
        assert!(!hardware_only_failed(Id::AV_CODEC_ID_H264));
    }
}
