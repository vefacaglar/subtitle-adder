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
//! * The decode thread copies, maps and unmaps under FFmpeg's device lock; a frame that is dropped (on any thread)
//!   only marks its slot returned, and the decode thread unmaps returned slots before reusing them. Only when the ring
//!   gave the slot up (the decoder was closed, or the frame size changed) while a frame still holds it does the last
//!   holder unmap it and release the texture, on whatever thread drops it, under the same lock. Each ring keeps a
//!   reference to FFmpeg's device (`device_ref`), shared by its slots, so the device outlives every mapping.
//! * Up to [`SLOTS`] textures, created as needed, of the decoder texture's size and format (NV12 / P010); a new size
//!   or format starts a new ring. No free slot, an unexpected format or any D3D failure: the caller falls back to
//!   `av_hwframe_transfer_data` for that frame (a texture that could not be created is not tried again for that
//!   size).
//! * When it is used ([`enabled`] and [`allowed_for`]): `SUBTITLE_ADDER_ZEROCOPY=0` (`off` / `no` / `false`) never,
//!   `=1` (`on` / `yes` / `true`) on any GPU, otherwise on NVIDIA adapters only (vendor id 0x10DE; the read speed of
//!   mapped READ staging memory was measured there only: a driver that maps it uncached would make every read slow).
//!   It is off for a libavutil other than 60 (the `AVD3D11VADeviceContext` mirror below is that version's), and for
//!   the rest of the process once the upload guard ([`report_upload`]) trips.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use ffmpeg_next::ffi;
use ffmpeg_next::ffi::AVPixelFormat;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::Interface;

use super::hw::OwnedFrame;

/// Staging textures per ring: the frames that can be held at once (4 queued, or 8 fields of 4 interlaced frames; the
/// presenter's and the UI's; the GL side's current and previous image) plus spares.
pub(crate) const SLOTS: usize = 10;

/// What `SUBTITLE_ADDER_ZEROCOPY` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    /// NVIDIA adapters only.
    Auto,
    On,
}

fn mode_of(v: &str) -> Mode {
    match v.trim().to_ascii_lowercase().as_str() {
        "0" | "off" | "no" | "false" => Mode::Off,
        "1" | "on" | "yes" | "true" => Mode::On,
        _ => Mode::Auto,
    }
}

fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| mode_of(&std::env::var("SUBTITLE_ADDER_ZEROCOPY").unwrap_or_default()))
}

/// Says once per process why the ring is not used.
fn off(reason: &str) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if !SAID.swap(true, Ordering::AcqRel) {
        eprintln!("[video] zero-copy off: {reason}");
    }
}

/// Whether the ring may be used at all in this process (the environment switch, the libavutil version, the upload
/// guard); the adapter is checked per device ([`allowed_for`]).
pub(crate) fn enabled() -> bool {
    static ABI: OnceLock<bool> = OnceLock::new();
    if mode() == Mode::Off || GUARD.tripped.load(Ordering::Acquire) {
        return false;
    }
    *ABI.get_or_init(|| {
        let major = unsafe { ffi::avutil_version() } >> 16;
        if major != 60 {
            off(&format!("libavutil {major} (the device context layout is libavutil 60's)"));
        }
        major == 60
    })
}

/// PCI vendor id of NVIDIA.
const NVIDIA: u32 = 0x10DE;

/// Whether the ring is used for frames of `ctx`'s device: always with `SUBTITLE_ADDER_ZEROCOPY=1`, else only on an
/// NVIDIA adapter (read once: the process has one decoding device).
fn allowed_for(ctx: &Context) -> bool {
    static VENDOR: OnceLock<Option<u32>> = OnceLock::new();
    if mode() == Mode::On {
        return true;
    }
    let vendor = *VENDOR.get_or_init(|| {
        // SAFETY: FFmpeg's device (kept alive by the caller's frame); QueryInterface / GetAdapter / GetDesc only read.
        let v = unsafe {
            ID3D11Device::from_raw_borrowed(&ctx.device)
                .and_then(|d| d.cast::<IDXGIDevice>().ok())
                .and_then(|d| d.GetAdapter().ok())
                .and_then(|a| a.GetDesc().ok())
                .map(|desc| desc.VendorId)
        };
        if v != Some(NVIDIA) {
            off(&format!("adapter vendor {} (only NVIDIA by default; SUBTITLE_ADDER_ZEROCOPY=1 forces it on)", v.map_or("unknown".into(), |v| format!("0x{v:04X}"))));
        }
        v
    });
    vendor == Some(NVIDIA)
}

/// The upload guard: the UI's upload of frames whose planes are a staging mapping ([`report_upload`]). Uploads from
/// ordinary memory run at ~12 GB/s here (4K P010, 24.9 MB in ~2.0 ms; 4K NV12 12.4 MB in ~0.9 ms); a driver that maps
/// READ staging memory uncached or write-combined reads it an order of magnitude slower. After [`GUARD_SKIP`] frames
/// (start-up), the next [`GUARD_FRAMES`] are averaged: below [`GUARD_MIN_BYTES_PER_NS`] (1 GB/s, i.e. ~25 ms for a
/// 4K P010 frame, 12× slower than measured) the ring is turned off for the rest of the process.
struct Guard {
    frames: AtomicU64,
    bytes: AtomicU64,
    ns: AtomicU64,
    tripped: AtomicBool,
}

static GUARD: Guard = Guard { frames: AtomicU64::new(0), bytes: AtomicU64::new(0), ns: AtomicU64::new(0), tripped: AtomicBool::new(false) };
const GUARD_SKIP: u64 = 16;
const GUARD_FRAMES: u64 = 60;
const GUARD_MIN_BYTES_PER_NS: f64 = 1.0;

/// Records one upload of a frame read from a staging mapping: `bytes` of planes in `wall_ns` (UI thread).
pub(crate) fn report_upload(wall_ns: u64, bytes: u64) {
    let n = GUARD.frames.fetch_add(1, Ordering::AcqRel);
    if n < GUARD_SKIP || n >= GUARD_SKIP + GUARD_FRAMES {
        return;
    }
    let total_bytes = GUARD.bytes.fetch_add(bytes, Ordering::AcqRel) + bytes;
    let total_ns = GUARD.ns.fetch_add(wall_ns, Ordering::AcqRel) + wall_ns;
    if n + 1 == GUARD_SKIP + GUARD_FRAMES {
        let rate = total_bytes as f64 / total_ns.max(1) as f64;
        if rate < GUARD_MIN_BYTES_PER_NS {
            GUARD.tripped.store(true, Ordering::Release);
            off(&format!(
                "uploads from mapped staging memory ran at {rate:.2} GB/s ({:.1} ms per frame over {GUARD_FRAMES} frames)",
                total_ns as f64 / GUARD_FRAMES as f64 / 1e6
            ));
        }
    }
}

/// `AVD3D11VADeviceContext` (libavutil/hwcontext_d3d11va.h of FFmpeg 8.1 = libavutil 60; not in the generated
/// bindings).
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

/// A reference to FFmpeg's device (`av_buffer_ref` of the frames context's `device_ref`): the device, its immediate
/// context and its lock stay valid while one lives.
struct DeviceRef(*mut ffi::AVBufferRef);

// SAFETY: AVBufferRef reference counting is thread-safe; the reference is only unref'd once, on drop.
unsafe impl Send for DeviceRef {}
unsafe impl Sync for DeviceRef {}

impl Drop for DeviceRef {
    fn drop(&mut self) {
        unsafe { ffi::av_buffer_unref(&mut self.0) };
    }
}

/// The device's immediate context and FFmpeg's lock around it (valid while the ring's [`DeviceRef`] lives).
#[derive(Clone, Copy)]
struct Context {
    device: *mut c_void,
    context: *mut c_void,
    lock: unsafe extern "C" fn(*mut c_void),
    unlock: unsafe extern "C" fn(*mut c_void),
    lock_ctx: *mut c_void,
}

// SAFETY: the pointers are FFmpeg's device, kept alive by a `DeviceRef`; every use of the immediate context holds
// FFmpeg's lock.
unsafe impl Send for Context {}
unsafe impl Sync for Context {}

/// Unlocks FFmpeg's device lock when dropped (also on unwinding).
struct Unlock<'a>(&'a Context);

impl Drop for Unlock<'_> {
    fn drop(&mut self) {
        unsafe { (self.0.unlock)(self.0.lock_ctx) };
    }
}

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
            let _unlock = Unlock(self);
            Some(f(ctx))
        }
    }
}

/// The DXGI format of the decoder textures for frames of `sw_format` the ring takes (NV12, P010).
fn dxgi_format(sw_format: AVPixelFormat) -> Option<DXGI_FORMAT> {
    match sw_format {
        AVPixelFormat::AV_PIX_FMT_NV12 => Some(DXGI_FORMAT_NV12),
        AVPixelFormat::AV_PIX_FMT_P010LE => Some(DXGI_FORMAT_P010),
        _ => None,
    }
}

/// A slot is with the ring (free, maybe still mapped from its last frame).
const FREE: u8 = 0;
/// A frame holds the slot (mapped).
const LENT: u8 = 1;
/// A frame holds the slot and the ring gave it up: the frame unmaps it.
const ORPHANED: u8 = 2;

struct Slot {
    tex: ID3D11Texture2D,
    state: AtomicU8,
    /// Mapped since its last frame (touched only by whoever owns the slot: the ring while `FREE`, the holder while
    /// `ORPHANED`).
    mapped: AtomicBool,
    ctx: Context,
    /// Dropped after `tex` (field order): the device the texture and `ctx` belong to.
    _device: Arc<DeviceRef>,
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
            // The ring gave the slot up while this frame was held.
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
    device: Option<Arc<DeviceRef>>,
    slots: Vec<Arc<Slot>>,
    /// Creating a texture of `spec` failed: not tried again until the spec changes.
    create_failed: bool,
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
        self.device = None;
        self.create_failed = false;
    }

    /// The planes of hardware frame `hw` in a mapped staging texture, as a frame without buffers whose `data` /
    /// `linesize` point into the mapping (valid while the returned lease lives); `None`: use
    /// `av_hwframe_transfer_data`.
    pub unsafe fn download(&mut self, hw: *const ffi::AVFrame) -> Option<(OwnedFrame, Lease)> {
        unsafe {
            let ctx = Context::of(hw)?;
            if !allowed_for(&ctx) {
                return None;
            }
            let frames = (*hw).hw_frames_ctx;
            let fc = (*frames).data as *const ffi::AVHWFramesContext;
            let sw_format = (*fc).sw_format;
            let expected = dxgi_format(sw_format)?;
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
                let r = ffi::av_buffer_ref((*fc).device_ref);
                if r.is_null() {
                    return None;
                }
                self.device = Some(Arc::new(DeviceRef(r)));
                self.spec = Some(spec);
            }
            let device = self.device.clone()?;
            let slot = match self.slots.iter().find(|s| s.state.load(Ordering::Acquire) == FREE) {
                Some(s) => s.clone(),
                None if self.slots.len() < SLOTS && !self.create_failed => {
                    let Some(tex) = create_staging(&ctx, &spec) else {
                        self.create_failed = true;
                        return None;
                    };
                    let s = Arc::new(Slot { tex, state: AtomicU8::new(FREE), mapped: AtomicBool::new(false), ctx, _device: device });
                    self.slots.push(s.clone());
                    s
                }
                None => return None,
            };
            // Free and owned by the ring: unmap its previous frame's mapping, then copy and map the new one.
            slot.unmap();
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            // NV12 / P010: the luma rows, then half as many chroma rows, `RowPitch` apart.
            let needed = map_rows(spec.height);
            let ok = ctx.locked(|c| {
                c.CopySubresourceRegion(&slot.tex, 0, 0, 0, 0, src, index, None);
                if c.Map(&slot.tex, 0, D3D11_MAP_READ, 0, Some(&mut map)).is_err() {
                    return false;
                }
                let valid = !map.pData.is_null() && map.RowPitch != 0 && map.DepthPitch as u64 >= map.RowPitch as u64 * needed;
                if !valid {
                    c.Unmap(&slot.tex, 0);
                }
                valid
            })?;
            if !ok {
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

/// Rows of a mapped NV12 / P010 texture of `height` luma rows: the luma rows and half as many chroma rows.
fn map_rows(height: u32) -> u64 {
    height as u64 + (height as u64).div_ceil(2)
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

    /// A frame the ring handed out, its lease and the bytes it must keep holding.
    type Kept = (OwnedFrame, Lease, Vec<Vec<u8>>);

    /// Decodes `path` in hardware through `ring`; for every frame the ring's mapped planes must hold exactly the bytes
    /// `av_hwframe_transfer_data` copies (and the frame the same format and size). Every frame is kept in `kept`, so
    /// several slots are in use at once and the ring fills up (then it must refuse, and the frames held must be
    /// unchanged). Returns the frames compared (0: no D3D11VA decoding, or the ring is not used on this adapter).
    fn compare(path: &std::path::Path, ring: &mut StagingRing, kept: &mut Vec<Kept>) -> usize {
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
        let mut copies = Download::default();
        let mut compared = 0;
        let mut unused = false;
        let mut frame = ffmpeg_next::frame::Video::empty();
        let mut check = |dec: &mut ffmpeg_next::decoder::Video, kept: &mut Vec<Kept>, ring: &mut StagingRing| {
            while !unused && dec.receive_frame(&mut frame).is_ok() {
                let raw = unsafe { frame.as_ptr() };
                if !hw::is_hw_format(unsafe { (*raw).format }) {
                    continue;
                }
                let copied = unsafe { copies.transfer(raw) }.expect("transfer");
                let want = unsafe { planes(copied.frame()) };
                match unsafe { ring.download(raw) } {
                    Some((f, lease)) => {
                        let shape = |f: *const ffi::AVFrame| unsafe { ((*f).format, (*f).width, (*f).height) };
                        assert_eq!(shape(f.0), shape(copied.frame()), "{}", path.display());
                        let got = unsafe { planes(f.0) };
                        assert!(got == want, "{}: frame {compared} differs from av_hwframe_transfer_data", path.display());
                        compared += 1;
                        kept.push((f, lease, want));
                    }
                    None if compared == 0 && kept.is_empty() => {
                        eprintln!("skipped: the ring is not used for this adapter ({})", path.display());
                        unused = true;
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
                check(&mut dec, kept, ring);
            }
        }
        dec.send_eof().expect("eof");
        check(&mut dec, kept, ring);
        if let Some(s) = ring.spec {
            eprintln!("{}: decoder textures of {}x{}", path.display(), s.width, s.height);
        }
        compared
    }

    const H264: (&str, &str) = ("libx264", "yuv420p");
    const HEVC10: (&str, &str) = ("libx265", "yuv420p10le");

    fn clip_of(name: &str, size: &str, (codec, pix): (&str, &str)) -> Option<PathBuf> {
        let c = clip(name, size, codec, pix);
        if c.is_none() {
            eprintln!("skipped {name}: no ffmpeg CLI or encoder");
        }
        c
    }

    #[test]
    fn a_frame_that_is_no_d3d11va_frame_is_left_to_the_transfer() {
        let _ = ffmpeg_next::init();
        let f = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::NV12, 64, 64);
        assert!(unsafe { StagingRing::default().download(f.as_ptr()) }.is_none());
    }

    #[test]
    fn only_nv12_and_p010_decoder_textures_are_taken() {
        use AVPixelFormat as F;
        assert_eq!(dxgi_format(F::AV_PIX_FMT_NV12), Some(DXGI_FORMAT_NV12));
        assert_eq!(dxgi_format(F::AV_PIX_FMT_P010LE), Some(DXGI_FORMAT_P010));
        for other in [F::AV_PIX_FMT_P016LE, F::AV_PIX_FMT_NV16, F::AV_PIX_FMT_P210LE, F::AV_PIX_FMT_YUV420P, F::AV_PIX_FMT_Y210LE, F::AV_PIX_FMT_VUYX] {
            assert_eq!(dxgi_format(other), None, "{other:?}");
        }
    }

    #[test]
    fn the_environment_switch_parses_like_the_hardware_switch_plus_a_force_on() {
        for v in ["0", "off", " No ", "FALSE"] {
            assert_eq!(mode_of(v), Mode::Off, "{v}");
        }
        for v in ["1", "on", "yes", "True"] {
            assert_eq!(mode_of(v), Mode::On, "{v}");
        }
        for v in ["", "2", "auto"] {
            assert_eq!(mode_of(v), Mode::Auto, "{v}");
        }
        assert_eq!(map_rows(1088), 1088 + 544);
        assert_eq!(map_rows(363), 363 + 182);
    }

    #[test]
    fn ring_frames_hold_the_bytes_av_hwframe_transfer_data_copies() {
        let clips = [
            ("zc-642x362-h264.mp4", "642x362", H264),
            ("zc-1920x800-h264.mp4", "1920x800", H264),
            ("zc-1920x1080-h264.mp4", "1920x1080", H264),
            ("zc-642x362-hevc10.mkv", "642x362", HEVC10),
            ("zc-1920x800-hevc10.mkv", "1920x800", HEVC10),
            ("zc-3840x1600-hevc10.mkv", "3840x1600", HEVC10),
        ];
        for (name, size, enc) in clips {
            let Some(path) = clip_of(name, size, enc) else { continue };
            let mut ring = StagingRing::default();
            let mut kept = Vec::new();
            let n = compare(&path, &mut ring, &mut kept);
            // Frames held when the decoder's ring goes: still readable, unmapped by their last holder.
            drop(ring);
            for (f, _, bytes) in kept.iter() {
                assert!(unsafe { planes(f.0) } == *bytes, "{name}: a held frame changed after the ring was dropped");
            }
            eprintln!("{name}: {n} frames identical");
        }
    }

    /// A new frame size in the same ring (a mid-stream resolution change): the ring gives its slots up while frames of
    /// the old size are still held; those are dropped on another thread while the decode thread maps the new ones.
    #[test]
    fn a_size_change_orphans_held_slots_which_another_thread_unmaps() {
        let (Some(small), Some(large)) = (clip_of("zc-642x362-h264.mp4", "642x362", H264), clip_of("zc-1920x1080-h264.mp4", "1920x1080", H264))
        else {
            return;
        };
        let mut ring = StagingRing::default();
        let mut old = Vec::new();
        if compare(&small, &mut ring, &mut old) == 0 {
            return;
        }
        let old_spec = ring.spec;
        assert!(!old.is_empty());
        let dropper = std::thread::spawn(move || {
            for (f, lease, bytes) in old.drain(..) {
                std::thread::sleep(std::time::Duration::from_millis(15));
                // Readable until dropped, even after the ring gave the slot up.
                assert!(unsafe { planes(f.0) } == bytes);
                drop((f, lease));
            }
        });
        let mut new = Vec::new();
        let n = compare(&large, &mut ring, &mut new);
        assert!(n > 0);
        assert!(ring.spec != old_spec, "a new spec");
        dropper.join().expect("dropper thread");
        for (f, _, bytes) in new.iter() {
            assert!(unsafe { planes(f.0) } == *bytes);
        }
    }

    #[test]
    fn the_upload_guard_trips_only_far_below_the_measured_copy_speed() {
        // 4K P010 in 2.0 ms (measured from ordinary memory): fine; 30 ms (an uncached mapping): off.
        let frame = 24_883_200f64;
        assert!(frame / 2.0e6 >= GUARD_MIN_BYTES_PER_NS);
        assert!(frame / 30.0e6 < GUARD_MIN_BYTES_PER_NS);
    }
}