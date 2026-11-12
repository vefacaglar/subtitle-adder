//! Windows: D3D11VA frames read through a ring of mapped staging textures (`StagingRing`), instead of
//! `av_hwframe_transfer_data`'s copy out of FFmpeg's one staging texture into a system-memory frame.
//!
//! `av_hwframe_transfer_data` (FFmpeg's `d3d11va_transfer_data`) copies the decoder's array slice into its staging
//! texture (`CopySubresourceRegion`), maps it (`Map(READ)`: waits for the GPU, under the device lock), copies the
//! mapped bytes into the destination frame on the CPU (`av_image_copy2`, ~25 MB per 4K P010 frame) and unmaps. The ring
//! skips that CPU copy: each frame gets a staging texture of its own, which stays mapped while the frame is held, and
//! the frame's planes point straight into the mapping (Y at `pData`, chroma at `pData + RowPitch * texture height`,
//! both `RowPitch` apart: FFmpeg's `fill_texture_ptrs`, i.e. `av_image_fill_pointers` over the texture's height).
//!
//! * Every D3D call (copy, map, unmap) runs on the decode thread under FFmpeg's device lock; a frame that is dropped
//!   (on any thread) only marks its slot returned, and the decode thread unmaps returned slots before reusing them.
//!   Only when the ring itself is gone (the decoder was closed) while a frame is still held does the last holder unmap
//!   its slot, under the same lock.
//! * Up to [`SLOTS`] textures, created as needed, of the decoder texture's size and format (NV12 / P010); a new size
//!   or format starts a new ring. No free slot, an unexpected format or any D3D failure: the caller falls back to
//!   `av_hwframe_transfer_data` for that frame.
//! * `SUBTITLE_ADDER_ZEROCOPY=0` (`off` / `no` / `false`) turns the ring off: every frame goes through
//!   `av_hwframe_transfer_data` as before.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use ffmpeg_next::ffi;
use ffmpeg_next::ffi::AVPixelFormat;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11DeviceContext, ID3D11Device,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC};
use windows::core::Interface;

use super::hw::OwnedFrame;

/// Staging textures per ring: the frames that can be held at once (4 queued, the presenter's and the UI's, the GL
/// side's current and previous image) plus spares.
pub(crate) const SLOTS: usize = 10;

/// Whether the ring is used (`SUBTITLE_ADDER_ZEROCOPY=0|off|no|false` turns it off, as `hw::allowed` parses).
pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let v = std::env::var("SUBTITLE_ADDER_ZEROCOPY").unwrap_or_default().to_ascii_lowercase();
        !matches!(v.trim(), "0" | "off" | "no" | "false")
    })
}

/// `AVD3D11VADeviceContext` (libavutil/hwcontext_d3d11va.h of FFmpeg 8.1; not in the generated bindings).
#[repr(C)]
struct D3D11VaDevice {
    device: *mut c_void,
    device_context: *mut c_void,
    video_device: *mut c_void,
    video_context: *mut c_void,
    lock: Option<unsafe extern "C" fn(*mut c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    lock_ctx: *mut c_void,
    bind_flags: u32,
    misc_flags: u32,
}

/// The device's immediate context and FFmpeg's lock around it (the device is process-wide and never freed).
#[derive(Clone, Copy)]
struct Context {
    device: *mut c_void,
    context: *mut c_void,
    lock: unsafe extern "C" fn(*mut c_void),
    unlock: unsafe extern "C" fn(*mut c_void),
    lock_ctx: *mut c_void,
}

// SAFETY: the pointers are FFmpeg's process-wide device; every use of the immediate context holds FFmpeg's lock.
unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl Context {
    /// The D3D11VA device of hardware frame `hw`.
    unsafe fn of(hw: *const ffi::AVFrame) -> Option<Self> {
        unsafe {
            let frames = (*hw).hw_frames_ctx;
            if frames.is_null() {
                return None;
            }
            let fc = (*frames).data as *const ffi::AVHWFramesContext;
            let dev = (*fc).device_ctx;
            if dev.is_null() || (*dev).type_ != ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA || (*dev).hwctx.is_null() {
                return None;
            }
            let d = &*((*dev).hwctx as *const D3D11VaDevice);
            if d.device.is_null() || d.device_context.is_null() {
                return None;
            }
            Some(Self { device: d.device, context: d.device_context, lock: d.lock?, unlock: d.unlock?, lock_ctx: d.lock_ctx })
        }
    }

    /// Runs `f` with the immediate context under FFmpeg's device lock.
    fn locked<R>(&self, f: impl FnOnce(&ID3D11DeviceContext) -> R) -> Option<R> {
        // SAFETY: FFmpeg's lock functions take its own lock context; the context pointer is FFmpeg's (borrowed).
        unsafe {
            let ctx = ID3D11DeviceContext::from_raw_borrowed(&self.context)?;
            (self.lock)(self.lock_ctx);
            let r = f(ctx);
            (self.unlock)(self.lock_ctx);
            Some(r)
        }
    }
}

/// A slot is with the ring (free, maybe still mapped from its last frame).
const FREE: u8 = 0;
/// A frame holds the slot (mapped).
const LENT: u8 = 1;
/// A frame holds the slot and the ring is gone: the frame unmaps it.
const ORPHANED: u8 = 2;

struct Slot {
    tex: ID3D11Texture2D,
    state: AtomicU8,
    /// Mapped since its last frame (touched only by whoever owns the slot: the ring while `FREE`, the holder while
    /// `ORPHANED`).
    mapped: AtomicBool,
    ctx: Context,
}

// SAFETY: the texture is only used under FFmpeg's device lock (see `state` for who may).
unsafe impl Send for Slot {}
unsafe impl Sync for Slot {}

impl Slot {
    fn unmap(&self) {
        if self.mapped.swap(false, Ordering::AcqRel) {
            // SAFETY: the slot's own texture, mapped by `StagingRing::download`.
            let _ = self.ctx.locked(|c| unsafe { c.Unmap(&self.tex, 0) });
        }
    }
}

/// Keeps a slot (and its mapping) for a frame; returns it on drop.
pub(crate) struct Lease(Arc<Slot>);

impl Drop for Lease {
    fn drop(&mut self) {
        if self.0.state.compare_exchange(LENT, FREE, Ordering::AcqRel, Ordering::Acquire).is_err() {
            // The ring was dropped while this frame was held.
            self.0.unmap();
        }
    }
}

/// What the textures of a ring are made like.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Spec {
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
}

/// The ring of one decode thread.
#[derive(Default)]
pub(crate) struct StagingRing {
    spec: Option<Spec>,
    slots: Vec<Arc<Slot>>,
}

impl Drop for StagingRing {
    fn drop(&mut self) {
        self.clear();
    }
}

impl StagingRing {
    /// Gives up every slot: free ones are unmapped now, held ones by their last holder.
    fn clear(&mut self) {
        for s in self.slots.drain(..) {
            if s.state.compare_exchange(LENT, ORPHANED, Ordering::AcqRel, Ordering::Acquire).is_err() {
                s.unmap();
            }
        }
        self.spec = None;
    }

    /// The planes of hardware frame `hw` in a mapped staging texture, as a frame without buffers whose `data` /
    /// `linesize` point into the mapping (valid while the returned lease lives); `None`: use
    /// `av_hwframe_transfer_data`.
    pub unsafe fn download(&mut self, hw: *const ffi::AVFrame) -> Option<(OwnedFrame, Lease)> {
        unsafe {
            let ctx = Context::of(hw)?;
            let frames = (*hw).hw_frames_ctx;
            let sw_format = (*((*frames).data as *const ffi::AVHWFramesContext)).sw_format;
            let expected = match sw_format {
                AVPixelFormat::AV_PIX_FMT_NV12 => DXGI_FORMAT_NV12,
                AVPixelFormat::AV_PIX_FMT_P010LE => DXGI_FORMAT_P010,
                _ => return None,
            };
            let src_ptr = (*hw).data[0] as *mut c_void;
            let index = (*hw).data[1] as usize as u32;
            let src = ID3D11Texture2D::from_raw_borrowed(&src_ptr)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            src.GetDesc(&mut desc);
            if desc.Format != expected || index >= desc.ArraySize.max(1) {
                return None;
            }
            let (w, h) = ((*hw).width, (*hw).height);
            if w <= 0 || h <= 0 || w as u32 > desc.Width || h as u32 > desc.Height {
                return None;
            }
            let spec = Spec { width: desc.Width, height: desc.Height, format: desc.Format };
            if self.spec != Some(spec) {
                self.clear();
                self.spec = Some(spec);
            }
            let slot = match self.slots.iter().find(|s| s.state.load(Ordering::Acquire) == FREE) {
                Some(s) => s.clone(),
                None if self.slots.len() < SLOTS => {
                    let s = Arc::new(Slot { tex: create_staging(&ctx, &spec)?, state: AtomicU8::new(FREE), mapped: AtomicBool::new(false), ctx });
                    self.slots.push(s.clone());
                    s
                }
                None => return None,
            };
            // Free and owned by the ring: unmap its previous frame's mapping, then copy and map the new one.
            slot.unmap();
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            let mapped = ctx.locked(|c| {
                c.CopySubresourceRegion(&slot.tex, 0, 0, 0, 0, src, index, None);
                c.Map(&slot.tex, 0, D3D11_MAP_READ, 0, Some(&mut map))
            })?;
            if mapped.is_err() || map.pData.is_null() || map.RowPitch == 0 {
                return None;
            }
            slot.mapped.store(true, Ordering::Release);
            slot.state.store(LENT, Ordering::Release);
            let lease = Lease(slot);
            let frame = OwnedFrame::alloc()?;
            let f = frame.0;
            let pitch = map.RowPitch as usize;
            let base = map.pData as *mut u8;
            (*f).format = sw_format as i32;
            (*f).width = w;
            (*f).height = h;
            (*f).data[0] = base;
            (*f).data[1] = base.add(pitch * desc.Height as usize);
            (*f).linesize[0] = map.RowPitch as i32;
            (*f).linesize[1] = map.RowPitch as i32;
            Some((frame, lease))
        }
    }
}

unsafe fn create_staging(ctx: &Context, spec: &Spec) -> Option<ID3D11Texture2D> {
    unsafe {
        let device = ID3D11Device::from_raw_borrowed(&ctx.device)?;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: spec.width,
            Height: spec.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: spec.format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex = None;
        device.CreateTexture2D(&desc, None, Some(&mut tex)).ok()?;
        tex
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;
    use crate::ffmpeg_backend::hw::{self, Download};

    /// A short test-pattern clip encoded by the FFmpeg CLI (`$FFMPEG_DIR/bin`), cached in the fixtures folder of the
    /// integration tests; `None` without the CLI or the encoder.
    fn clip(name: &str, size: &str, codec: &str, pix_fmt: &str) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join("sa-player-fixtures-v1");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        if path.exists() {
            return Some(path);
        }
        let ff = PathBuf::from(std::env::var("FFMPEG_DIR").ok()?).join("bin").join("ffmpeg.exe");
        let tmp = dir.join(format!("tmp-{}-{name}", std::process::id()));
        let mut args = vec!["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"];
        let src = format!("testsrc2=size={size}:rate=30");
        args.push(&src);
        args.extend(["-frames:v", "24", "-c:v", codec, "-pix_fmt", pix_fmt, "-g", "6"]);
        if codec == "libx265" {
            args.extend(["-x265-params", "log-level=error"]);
        }
        let tmp_s = tmp.to_string_lossy().into_owned();
        args.push(&tmp_s);
        let ok = Command::new(&ff).args(&args).status().ok()?.success();
        if !ok {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
        let _ = std::fs::rename(&tmp, &path);
        Some(path)
    }

    /// The rows of the planes of software frame `f` (NV12 / P010): Y, then interleaved chroma.
    unsafe fn planes(f: *const ffi::AVFrame) -> Vec<Vec<u8>> {
        unsafe {
            let (w, h) = ((*f).width as usize, (*f).height as usize);
            let sb = if (*f).format == AVPixelFormat::AV_PIX_FMT_P010LE as i32 { 2 } else { 1 };
            let mut out = Vec::new();
            for (i, rows, row_bytes) in [(0, h, w * sb), (1, h.div_ceil(2), w.div_ceil(2) * 2 * sb)] {
                let mut bytes = Vec::with_capacity(rows * row_bytes);
                for r in 0..rows {
                    let p = (*f).data[i].add(r * (*f).linesize[i] as usize);
                    bytes.extend_from_slice(std::slice::from_raw_parts(p, row_bytes));
                }
                out.push(bytes);
            }
            out
        }
    }

    unsafe fn props(f: *const ffi::AVFrame) -> String {
        unsafe {
            format!(
                "{} {}x{} {:?} {:?} {:?} {:?} sar {}/{} flags {:x} pts {}",
                (*f).format,
                (*f).width,
                (*f).height,
                (*f).colorspace,
                (*f).color_range,
                (*f).color_trc,
                (*f).color_primaries,
                (*f).sample_aspect_ratio.num,
                (*f).sample_aspect_ratio.den,
                (*f).flags,
                (*f).pts
            )
        }
    }

    /// Decodes `path` in hardware; for every frame the ring's mapped planes must hold exactly the bytes
    /// `av_hwframe_transfer_data` copies (and the frame the same format, size and properties). Some frames are kept
    /// (several slots in use at once, the ring full), and two outlive the ring. Returns the frames compared (0: no
    /// D3D11VA decoding here).
    fn compare(path: &std::path::Path) -> usize {
        let _ = ffmpeg_next::init();
        let mut input = ffmpeg_next::format::input(path).expect("open");
        let stream = input.streams().best(ffmpeg_next::media::Type::Video).expect("video");
        let index = stream.index();
        let params = stream.parameters();
        let tb = stream.time_base();
        let (mut dec, hw_dec, _) = crate::ffmpeg_backend::session::open_video_decoder(&params, tb, true).expect("decoder");
        if !hw_dec {
            eprintln!("skipped: no hardware decoder for {}", path.display());
            return 0;
        }
        let mut ring = StagingRing::default();
        let mut copies = Download::default();
        let mut kept = Vec::new();
        let mut compared = 0;
        let mut frame = ffmpeg_next::frame::Video::empty();
        let mut check = |dec: &mut ffmpeg_next::decoder::Video, kept: &mut Vec<(OwnedFrame, Lease, Vec<Vec<u8>>)>, ring: &mut StagingRing| {
            while dec.receive_frame(&mut frame).is_ok() {
                let raw = unsafe { frame.as_ptr() };
                if !hw::is_hw_format(unsafe { (*raw).format }) {
                    continue;
                }
                let copied = unsafe { copies.transfer(raw) }.expect("transfer");
                let want = unsafe { planes(copied.frame()) };
                match unsafe { ring.download(raw) } {
                    Some((f, lease)) => {
                        assert_eq!(unsafe { props(f.0) }, unsafe { props(copied.frame()) }, "{}", path.display());
                        let got = unsafe { planes(f.0) };
                        assert!(got == want, "{}: frame {compared} differs from av_hwframe_transfer_data", path.display());
                        compared += 1;
                        kept.push((f, lease, want));
                    }
                    None => {
                        // Only a full ring may refuse a frame.
                        assert_eq!(kept.len(), SLOTS, "{}: the ring refused a frame with free slots", path.display());
                        // Mapped frames stay readable and unchanged while they are held.
                        for (f, _, bytes) in kept.iter() {
                            assert!(unsafe { planes(f.0) } == *bytes);
                        }
                        kept.drain(..SLOTS - 2);
                    }
                }
            }
        };
        for (s, packet) in input.packets() {
            if s.index() == index {
                dec.send_packet(&packet).expect("packet");
                check(&mut dec, &mut kept, &mut ring);
            }
        }
        dec.send_eof().expect("eof");
        check(&mut dec, &mut kept, &mut ring);
        // Frames held when the decoder (and its ring) goes: still readable, unmapped by their last holder.
        if let Some(s) = ring.spec {
            eprintln!("{}: frames {}x{} in decoder textures of {}x{}", path.display(), frame.width(), frame.height(), s.width, s.height);
        }
        drop(dec);
        drop(ring);
        for (f, _, bytes) in kept.iter() {
            assert!(unsafe { planes(f.0) } == *bytes, "{}: a held frame changed after the ring was dropped", path.display());
        }
        drop(kept);
        compared
    }

    #[test]
    fn a_frame_that_is_no_d3d11va_frame_is_left_to_the_transfer() {
        let _ = ffmpeg_next::init();
        let f = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::NV12, 64, 64);
        assert!(unsafe { StagingRing::default().download(f.as_ptr()) }.is_none());
    }

    #[test]
    fn ring_frames_hold_the_bytes_av_hwframe_transfer_data_copies() {
        let clips = [
            ("zc-642x362-h264.mp4", "642x362", "libx264", "yuv420p"),
            ("zc-1920x800-h264.mp4", "1920x800", "libx264", "yuv420p"),
            ("zc-642x362-hevc10.mkv", "642x362", "libx265", "yuv420p10le"),
            ("zc-1920x800-hevc10.mkv", "1920x800", "libx265", "yuv420p10le"),
            ("zc-3840x1600-hevc10.mkv", "3840x1600", "libx265", "yuv420p10le"),
        ];
        for (name, size, codec, pix) in clips {
            let Some(path) = clip(name, size, codec, pix) else {
                eprintln!("skipped {name}: no ffmpeg CLI or encoder");
                continue;
            };
            let n = compare(&path);
            eprintln!("{name}: {n} frames identical");
        }
    }
}
