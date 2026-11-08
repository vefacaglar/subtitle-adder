//! Hardware video decoding: D3D11VA on Windows, VideoToolbox on macOS (nothing elsewhere), as Media Foundation and
//! AVFoundation decode on the GPU. A stream is decoded in hardware when the codec has a device-type hardware config
//! for the platform and the stream is 4:2:0 at 8 or 10 bits (what the GPU decoders handle); otherwise, when the
//! device cannot be created, or when the decoder still picks a software format (an unsupported profile, an old
//! GPU), it is decoded in software. `SUBTITLE_ADDER_HWDEC=0` turns hardware decoding off.
//!
//! Decoded GPU surfaces are downloaded once into reusable system-memory frames (NV12 / P010): one copy, no per-frame
//! buffer allocation. The UI uploads those planes and converts them to RGB on the GPU.

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
    if !allowed() || codec.is_null() || par.is_null() {
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

/// Downloads GPU frames into recycled system-memory frames.
#[derive(Default)]
pub(crate) struct Download {
    pool: Vec<Arc<OwnedFrame>>,
}

const POOL: usize = 8;

impl Download {
    /// The software copy of hardware frame `hw` (NV12 / P010); `None` when the transfer failed.
    pub unsafe fn download(&mut self, hw: *const ffi::AVFrame) -> Option<Arc<OwnedFrame>> {
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
                    let p = f.0;
                    (*p).width == w && (*p).height == h && (*p).format == sw_format && !(*p).buf[0].is_null()
                }
            });
            let dst = match reuse {
                Some(i) => self.pool.swap_remove(i),
                None => Arc::new(OwnedFrame::alloc()?),
            };
            if ffi::av_hwframe_transfer_data(dst.0, hw, 0) < 0 {
                return None;
            }
            self.pool.retain(|f| (*f.0).width == w && (*f.0).height == h);
            if self.pool.len() < POOL {
                self.pool.push(dst.clone());
            }
            Some(dst)
        }
    }
}
