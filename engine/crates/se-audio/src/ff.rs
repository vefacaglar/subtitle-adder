//! Thin FFmpeg layer that reproduces the PyAV 18.1 behaviors the Python engine relies on (spec `audio.md` §8):
//! containers with `AVFMT_FLAG_GENPTS`, lazily opened decoders with slice threading, PyAV's decode loop,
//! `AudioResampler` (abuffer -> aformat -> abuffersink, passthrough), `VideoReformatter` (new swscale API),
//! encoders with PyAV's defaults and `OutputContainer.mux` (start_encoding, time-base rebasing).
//!
//! Everything here is raw FFI over `ffmpeg_next::ffi`; the wrappers own their pointers and free them on drop.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ptr;
use std::sync::Once;

pub use ffmpeg_next::ffi as sys;
use sys::{AVChannelLayout, AVRational};

use se_core::pyfmt::repr_str;
use se_core::{EngineError, ErrorKind};

pub const NOPTS: i64 = sys::AV_NOPTS_VALUE;
pub const EAGAIN: c_int = -(sys::EAGAIN as c_int); // AVERROR(EAGAIN): -11 on Windows/Linux, -35 on macOS/BSD

// ---------------------------------------------------------------------------------------------------------------
// Errors

/// PyAV `FFmpegError`: `[Errno {errno}] {strerror}: {filename!r}` (`ArgumentError` format for EINVAL).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfError {
    /// The negative FFmpeg return value.
    pub code: c_int,
    pub filename: Option<String>,
}

impl FfError {
    pub fn new(code: c_int, filename: Option<&str>) -> Self {
        Self { code, filename: filename.map(str::to_string) }
    }
    /// The positive `errno` attribute (`-ret`).
    pub fn errno(&self) -> c_int {
        self.code.wrapping_neg()
    }
    pub fn strerror(&self) -> String {
        let mut buf = [0 as c_char; 256];
        unsafe {
            sys::av_strerror(self.code, buf.as_mut_ptr(), buf.len());
            let text = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
            if text.is_empty() { format!("Unknown error {}", self.errno()) } else { text }
        }
    }
    pub fn is_invalid_data(&self) -> bool {
        self.code == sys::AVERROR_INVALIDDATA
    }
}

impl fmt::Display for FfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let filename = self.filename.as_deref().filter(|s| !s.is_empty());
        if self.errno() == 22 {
            // av.error.ArgumentError
            write!(f, "{}", self.strerror())?;
            if let Some(name) = filename {
                write!(f, ": {}", repr_str(name))?;
            }
            return write!(f, " returned {}", self.errno());
        }
        write!(f, "[Errno {}] {}", self.errno(), self.strerror())?;
        if let Some(name) = filename {
            write!(f, ": {}", repr_str(name))?;
        }
        Ok(())
    }
}

/// Errors inside the media code: an FFmpeg error (callers decide whether to catch it, like `except
/// av.FFmpegError`) or any other engine error (propagates unchanged).
#[derive(Debug)]
pub enum MediaError {
    Ff(FfError),
    Engine(EngineError),
}

impl From<FfError> for MediaError {
    fn from(e: FfError) -> Self {
        MediaError::Ff(e)
    }
}
impl From<EngineError> for MediaError {
    fn from(e: EngineError) -> Self {
        MediaError::Engine(e)
    }
}
impl MediaError {
    pub fn into_engine(self) -> EngineError {
        match self {
            MediaError::Ff(e) => EngineError::runtime(e.to_string()),
            MediaError::Engine(e) => e,
        }
    }
}

/// `ValueError("Frame does not match AudioResampler setup.")`.
pub fn resampler_mismatch() -> MediaError {
    MediaError::Engine(EngineError::value("Frame does not match AudioResampler setup."))
}

pub fn check(ret: c_int, filename: Option<&str>) -> Result<c_int, FfError> {
    if ret < 0 { Err(FfError::new(ret, filename)) } else { Ok(ret) }
}

/// PyAV silences FFmpeg's log output; do the same once per process.
pub fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe { sys::av_log_set_level(sys::AV_LOG_QUIET) });
}

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).expect("no interior NUL")
}

pub fn q(num: c_int, den: c_int) -> AVRational {
    AVRational { num, den }
}

/// Python `str(Fraction(num, den))`: reduced, `"n"` when the denominator is 1.
pub fn fraction_str(r: AVRational) -> String {
    fn gcd(a: i64, b: i64) -> i64 {
        if b == 0 { a.abs() } else { gcd(b, a % b) }
    }
    let (mut n, mut d) = (r.num as i64, r.den as i64);
    if d < 0 {
        n = -n;
        d = -d;
    }
    let g = gcd(n, d).max(1);
    let (n, d) = (n / g, d / g);
    if d == 1 { n.to_string() } else { format!("{n}/{d}") }
}

// ---------------------------------------------------------------------------------------------------------------
// Dictionaries

pub struct Dict(pub *mut sys::AVDictionary);

impl Dict {
    pub fn new(entries: &[(&str, &str)]) -> Self {
        let mut d = Dict(ptr::null_mut());
        for (k, v) in entries {
            d.set(k, v);
        }
        d
    }
    pub fn set(&mut self, k: &str, v: &str) {
        let (k, v) = (cstring(k), cstring(v));
        unsafe { sys::av_dict_set(&mut self.0, k.as_ptr(), v.as_ptr(), 0) };
    }
    pub fn copy(&self) -> Dict {
        let mut d = Dict(ptr::null_mut());
        unsafe { sys::av_dict_copy(&mut d.0, self.0, 0) };
        d
    }
}

impl Drop for Dict {
    fn drop(&mut self) {
        unsafe { sys::av_dict_free(&mut self.0) };
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Channel layouts

pub struct Layout(pub AVChannelLayout);

impl Layout {
    pub fn default_for(channels: c_int) -> Self {
        unsafe {
            let mut l: AVChannelLayout = std::mem::zeroed();
            sys::av_channel_layout_default(&mut l, channels);
            Layout(l)
        }
    }
    pub fn copy_from(src: *const AVChannelLayout) -> Self {
        unsafe {
            let mut l: AVChannelLayout = std::mem::zeroed();
            sys::av_channel_layout_copy(&mut l, src);
            Layout(l)
        }
    }
    pub fn describe(&self) -> String {
        describe_layout(&self.0)
    }
    pub fn eq_ptr(&self, other: *const AVChannelLayout) -> bool {
        unsafe { sys::av_channel_layout_compare(&self.0, other) == 0 }
    }
}

impl Drop for Layout {
    fn drop(&mut self) {
        unsafe { sys::av_channel_layout_uninit(&mut self.0) };
    }
}

pub fn describe_layout(l: *const AVChannelLayout) -> String {
    let mut buf = [0 as c_char; 256];
    unsafe {
        sys::av_channel_layout_describe(l, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

pub fn sample_fmt_name(fmt: c_int) -> String {
    unsafe {
        let p = sys::av_get_sample_fmt_name(std::mem::transmute::<c_int, sys::AVSampleFormat>(fmt));
        if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

pub fn pix_fmt_name(fmt: c_int) -> Option<String> {
    if fmt < 0 {
        return None;
    }
    unsafe {
        let p = sys::av_get_pix_fmt_name(std::mem::transmute::<c_int, sys::AVPixelFormat>(fmt));
        if p.is_null() { None } else { Some(CStr::from_ptr(p).to_string_lossy().into_owned()) }
    }
}

pub fn sample_fmt_from_name(name: &str) -> c_int {
    let c = cstring(name);
    unsafe { sys::av_get_sample_fmt(c.as_ptr()) as c_int }
}

pub fn codec_name(codec: *const sys::AVCodec) -> String {
    unsafe {
        if codec.is_null() || (*codec).name.is_null() {
            String::new()
        } else {
            CStr::from_ptr((*codec).name).to_string_lossy().into_owned()
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Frames and packets

/// An `AVFrame` plus PyAV's separately tracked `_time_base` (`num == 0` means None).
pub struct Frame {
    pub ptr: *mut sys::AVFrame,
    pub tb: AVRational,
}

impl Frame {
    pub fn alloc() -> Self {
        let ptr = unsafe { sys::av_frame_alloc() };
        assert!(!ptr.is_null(), "av_frame_alloc");
        Frame { ptr, tb: q(0, 1) }
    }
    /// A new reference to the same data (PyAV returns the very same frame object; data is never mutated).
    pub fn clone_ref(&self) -> Self {
        let ptr = unsafe { sys::av_frame_clone(self.ptr) };
        assert!(!ptr.is_null(), "av_frame_clone");
        Frame { ptr, tb: self.tb }
    }
    pub fn pts(&self) -> i64 {
        unsafe { (*self.ptr).pts }
    }
    pub fn set_pts(&mut self, pts: i64) {
        unsafe { (*self.ptr).pts = pts };
    }
    /// PyAV `frame.time`: `float(pts) * tb.num / tb.den` (None for NOPTS).
    pub fn time(&self) -> Option<f64> {
        let pts = self.pts();
        if pts == NOPTS {
            return None;
        }
        Some(pts as f64 * self.tb.num as f64 / self.tb.den as f64)
    }
    pub fn nb_samples(&self) -> c_int {
        unsafe { (*self.ptr).nb_samples }
    }
    pub fn sample_rate(&self) -> c_int {
        unsafe { (*self.ptr).sample_rate }
    }
    pub fn format(&self) -> c_int {
        unsafe { (*self.ptr).format }
    }
    pub fn layout_ptr(&self) -> *const AVChannelLayout {
        unsafe { &(*self.ptr).ch_layout }
    }
    pub fn channels(&self) -> c_int {
        unsafe { (*self.ptr).ch_layout.nb_channels }
    }
    pub fn width(&self) -> c_int {
        unsafe { (*self.ptr).width }
    }
    pub fn height(&self) -> c_int {
        unsafe { (*self.ptr).height }
    }
    pub fn is_video(&self) -> bool {
        unsafe { (*self.ptr).width > 0 && (*self.ptr).nb_samples == 0 }
    }

    /// PyAV `AudioFrame.to_ndarray().tobytes()`: packed formats give `data[0]` (all channels interleaved),
    /// planar formats the planes one after another.
    pub fn audio_bytes(&self) -> Vec<u8> {
        unsafe {
            let fmt = std::mem::transmute::<c_int, sys::AVSampleFormat>(self.format());
            let bps = sys::av_get_bytes_per_sample(fmt) as usize;
            let planar = sys::av_sample_fmt_is_planar(fmt) != 0;
            let n = self.nb_samples().max(0) as usize;
            let ch = self.channels().max(0) as usize;
            let ext = (*self.ptr).extended_data;
            let mut out = Vec::new();
            if planar {
                for c in 0..ch {
                    let p = *ext.add(c);
                    out.extend_from_slice(std::slice::from_raw_parts(p, n * bps));
                }
            } else if ch > 0 {
                let p = *ext;
                out.extend_from_slice(std::slice::from_raw_parts(p, n * bps * ch));
            }
            out
        }
    }

    /// `AudioFrame.from_ndarray(planes, format, layout)` (+ `sample_rate`): one plane for packed formats, one
    /// plane per channel for planar ones. Time base None, pts NOPTS.
    pub fn audio_from_planes(fmt: &str, channels: c_int, sample_rate: c_int, nb_samples: c_int, planes: &[&[u8]]) -> Self {
        let f = Frame::alloc();
        unsafe {
            let p = f.ptr;
            (*p).format = sample_fmt_from_name(fmt);
            (*p).nb_samples = nb_samples;
            (*p).sample_rate = sample_rate;
            sys::av_channel_layout_default(&mut (*p).ch_layout, channels);
            check(sys::av_frame_get_buffer(p, 0), None).expect("av_frame_get_buffer");
            for (i, plane) in planes.iter().enumerate() {
                let dst = *(*p).extended_data.add(i);
                ptr::copy_nonoverlapping(plane.as_ptr(), dst, plane.len());
            }
            (*p).pts = NOPTS;
        }
        f
    }

    /// `VideoFrame.from_ndarray(rgb, format="rgb24")`.
    pub fn video_rgb24(width: c_int, height: c_int, rgb: &[u8]) -> Self {
        let f = Frame::alloc();
        unsafe {
            let p = f.ptr;
            (*p).format = sys::AVPixelFormat::AV_PIX_FMT_RGB24 as c_int;
            (*p).width = width;
            (*p).height = height;
            check(sys::av_frame_get_buffer(p, 16), None).expect("av_frame_get_buffer");
            let ls = (*p).linesize[0] as usize;
            let row = width as usize * 3;
            for y in 0..height as usize {
                ptr::copy_nonoverlapping(rgb.as_ptr().add(y * row), (*p).data[0].add(y * ls), row);
            }
            (*p).pts = NOPTS;
        }
        f
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { sys::av_frame_free(&mut self.ptr) };
    }
}

pub struct Packet {
    pub ptr: *mut sys::AVPacket,
}

impl Packet {
    pub fn alloc() -> Self {
        let ptr = unsafe { sys::av_packet_alloc() };
        assert!(!ptr.is_null(), "av_packet_alloc");
        Packet { ptr }
    }
    pub fn size(&self) -> c_int {
        unsafe { (*self.ptr).size }
    }
    pub fn stream_index(&self) -> c_int {
        unsafe { (*self.ptr).stream_index }
    }
    pub fn is_corrupt(&self) -> bool {
        unsafe { (*self.ptr).flags & sys::AV_PKT_FLAG_CORRUPT != 0 }
    }
    pub fn tb(&self) -> AVRational {
        unsafe { (*self.ptr).time_base }
    }
    pub fn set_tb(&mut self, tb: AVRational) {
        unsafe { (*self.ptr).time_base = tb };
    }
    /// PyAV `packet.update(b"\xff" * packet.size)`.
    pub fn fill(&mut self, byte: u8) {
        unsafe {
            if sys::av_packet_make_writable(self.ptr) >= 0 && !(*self.ptr).data.is_null() {
                ptr::write_bytes((*self.ptr).data, byte, (*self.ptr).size as usize);
            }
        }
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        unsafe { sys::av_packet_free(&mut self.ptr) };
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Decoders

/// A PyAV decoder `CodecContext` (opened lazily on the first decode).
pub struct Decoder {
    pub ctx: *mut sys::AVCodecContext,
    pub codec: *const sys::AVCodec,
    pub open: bool,
}

impl Decoder {
    unsafe fn for_stream(st: *mut sys::AVStream) -> Option<Self> {
        unsafe {
            let codec = sys::avcodec_find_decoder((*(*st).codecpar).codec_id);
            if codec.is_null() {
                return None;
            }
            let ctx = sys::avcodec_alloc_context3(codec);
            if ctx.is_null() {
                return None;
            }
            sys::avcodec_parameters_to_context(ctx, (*st).codecpar);
            (*ctx).pkt_timebase = (*st).time_base;
            (*ctx).thread_count = 0;
            (*ctx).thread_type = sys::FF_THREAD_SLICE;
            Some(Decoder { ctx, codec, open: false })
        }
    }

    pub fn name(&self) -> String {
        codec_name(self.codec)
    }

    fn open(&mut self) -> Result<(), FfError> {
        if self.open {
            return Ok(());
        }
        let mut opts = Dict(ptr::null_mut());
        let ret = unsafe { sys::avcodec_open2(self.ctx, self.codec, &mut opts.0) };
        check(ret, Some(&format!("avcodec_open2(\"{}\", {{}})", self.name())))?;
        self.open = true;
        Ok(())
    }

    /// PyAV `CodecContext.decode(packet)`; the frames get the packet's time base.
    pub fn decode(&mut self, pkt: &Packet) -> Result<Vec<Frame>, FfError> {
        self.open()?;
        check(unsafe { sys::avcodec_send_packet(self.ctx, pkt.ptr) }, Some("avcodec_send_packet()"))?;
        let mut out = Vec::new();
        loop {
            let frame = Frame::alloc();
            let res = unsafe { sys::avcodec_receive_frame(self.ctx, frame.ptr) };
            if res == EAGAIN || res == sys::AVERROR_EOF {
                break;
            }
            if res < 0 {
                if res == sys::AVERROR_INVALIDDATA && !out.is_empty() {
                    break;
                }
                return Err(FfError::new(res, Some("avcodec_receive_frame()")));
            }
            let mut frame = frame;
            frame.tb = pkt.tb();
            out.push(frame);
        }
        Ok(out)
    }

    /// `flush_buffers()`: only when open.
    pub fn flush(&mut self) {
        if self.open {
            unsafe { sys::avcodec_flush_buffers(self.ctx) };
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { sys::avcodec_free_context(&mut self.ctx) };
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Custom IO (PyAV `PyIOFile`)

/// The file behind a custom AVIOContext.
pub struct IoFile {
    pub file: File,
    pub seekable: bool,
    /// An I/O error raised inside a callback (PyAV "stashes" the Python exception and re-raises it).
    pub stashed: Option<io::Error>,
}

unsafe extern "C" fn io_read(opaque: *mut c_void, buf: *mut u8, size: c_int) -> c_int {
    let io = unsafe { &mut *(opaque as *mut IoFile) };
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, size.max(0) as usize) };
    loop {
        match io.file.read(slice) {
            Ok(0) => return sys::AVERROR_EOF,
            Ok(n) => return n as c_int,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                io.stashed = Some(e);
                return -5; // AVERROR(EIO)
            }
        }
    }
}

unsafe extern "C" fn io_write(opaque: *mut c_void, buf: *const u8, size: c_int) -> c_int {
    let io = unsafe { &mut *(opaque as *mut IoFile) };
    let slice = unsafe { std::slice::from_raw_parts(buf, size.max(0) as usize) };
    match io.file.write_all(slice) {
        Ok(()) => size,
        Err(e) => {
            io.stashed = Some(e);
            -5
        }
    }
}

unsafe extern "C" fn io_seek(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    if whence == sys::AVSEEK_SIZE {
        return -1;
    }
    let io = unsafe { &mut *(opaque as *mut IoFile) };
    let pos = match whence & !sys::AVSEEK_FORCE {
        0 => SeekFrom::Start(offset.max(0) as u64),
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => return -22,
    };
    match io.file.seek(pos) {
        Ok(p) => p as i64,
        Err(e) => {
            io.stashed = Some(e);
            -5
        }
    }
}

const IO_BUFFER_SIZE: c_int = 32768;

/// An AVIOContext over a boxed [`IoFile`].
pub struct CustomIo {
    pub pb: *mut sys::AVIOContext,
    pub file: Box<IoFile>,
}

impl CustomIo {
    pub fn new(file: File, seekable: bool, write: bool) -> Self {
        let mut boxed = Box::new(IoFile { file, seekable, stashed: None });
        unsafe {
            let buffer = sys::av_malloc(IO_BUFFER_SIZE as usize) as *mut u8;
            assert!(!buffer.is_null(), "av_malloc");
            let opaque = &mut *boxed as *mut IoFile as *mut c_void;
            let pb = sys::avio_alloc_context(
                buffer,
                IO_BUFFER_SIZE,
                write as c_int,
                opaque,
                Some(io_read),
                Some(io_write),
                if seekable { Some(io_seek) } else { None },
            );
            assert!(!pb.is_null(), "avio_alloc_context");
            if seekable {
                (*pb).seekable = sys::AVIO_SEEKABLE_NORMAL;
            }
            (*pb).max_packet_size = IO_BUFFER_SIZE;
            CustomIo { pb, file: boxed }
        }
    }

    pub fn take_stashed(&mut self) -> Option<io::Error> {
        self.file.stashed.take()
    }
}

impl Drop for CustomIo {
    fn drop(&mut self) {
        unsafe {
            if !self.pb.is_null() {
                sys::av_freep(&mut (*self.pb).buffer as *mut *mut u8 as *mut c_void);
                sys::avio_context_free(&mut self.pb);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Input containers

pub struct Input {
    pub ctx: *mut sys::AVFormatContext,
    pub decoders: Vec<Option<Decoder>>,
    /// PyAV `container.name` (used as the filename of open/demux errors).
    pub name: String,
    pub io: Option<CustomIo>,
}

impl Input {
    /// `av.open(path)` (FFmpeg's own `file` protocol, no options).
    pub fn open_path(path_utf8: &str) -> Result<Input, FfError> {
        Self::open_impl(path_utf8, None, &[]).map_err(|(e, _)| e)
    }

    /// `av.open(file_object, options=...)`: custom IO; `name` is the file object's name. An I/O error raised
    /// by a read/seek callback while probing wins over the FFmpeg error (PyAV re-raises the stashed exception
    /// in `err_check`).
    pub fn open_custom(io: CustomIo, name: &str, options: &[(&str, &str)]) -> Result<Input, MediaError> {
        Self::open_impl(name, Some(io), options).map_err(|(e, stashed)| match stashed {
            Some(io_err) => MediaError::Engine(EngineError::os(&io_err, None)),
            None => MediaError::Ff(e),
        })
    }

    fn open_impl(name: &str, mut io: Option<CustomIo>, options: &[(&str, &str)]) -> Result<Input, (FfError, Option<io::Error>)> {
        init();
        let cname = cstring(name);
        unsafe {
            let mut ctx = sys::avformat_alloc_context();
            assert!(!ctx.is_null(), "avformat_alloc_context");
            (*ctx).flags |= sys::AVFMT_FLAG_GENPTS;
            if let Some(io) = &io {
                (*ctx).pb = io.pb;
            }
            let base = Dict::new(options);
            let mut opts = base.copy();
            let ret = sys::avformat_open_input(&mut ctx, cname.as_ptr(), ptr::null(), &mut opts.0);
            if ret < 0 {
                // avformat_open_input freed the context (not our AVIOContext).
                let stashed = io.as_mut().and_then(CustomIo::take_stashed);
                return Err((FfError::new(ret, Some(name)), stashed));
            }
            let mut input = Input { ctx, decoders: Vec::new(), name: name.to_string(), io };
            let before = (*ctx).nb_streams as usize;
            let ret = if !options.is_empty() && before > 0 {
                let mut per: Vec<Dict> = (0..before).map(|_| base.copy()).collect();
                let mut raw: Vec<*mut sys::AVDictionary> = per.iter().map(|d| d.0).collect();
                let r = sys::avformat_find_stream_info(ctx, raw.as_mut_ptr());
                for (d, p) in per.iter_mut().zip(raw) {
                    d.0 = p;
                }
                r
            } else {
                sys::avformat_find_stream_info(ctx, ptr::null_mut())
            };
            if let Err(e) = check(ret, Some(name)) {
                let stashed = input.io.as_mut().and_then(CustomIo::take_stashed);
                return Err((e, stashed));
            }
            for i in 0..(*ctx).nb_streams as usize {
                let st = *(*ctx).streams.add(i);
                input.decoders.push(Decoder::for_stream(st));
            }
            Ok(input)
        }
    }

    pub fn nb_streams(&self) -> usize {
        unsafe { (*self.ctx).nb_streams as usize }
    }
    pub fn stream(&self, i: usize) -> *mut sys::AVStream {
        unsafe { *(*self.ctx).streams.add(i) }
    }
    pub fn media_type(&self, i: usize) -> sys::AVMediaType {
        unsafe { (*(*self.stream(i)).codecpar).codec_type }
    }
    pub fn streams_of(&self, t: sys::AVMediaType) -> Vec<usize> {
        (0..self.nb_streams()).filter(|&i| self.media_type(i) == t).collect()
    }
    pub fn stream_tb(&self, i: usize) -> AVRational {
        unsafe { (*self.stream(i)).time_base }
    }
    pub fn format_name(&self) -> String {
        unsafe {
            let f = (*self.ctx).iformat;
            if f.is_null() || (*f).name.is_null() {
                String::new()
            } else {
                CStr::from_ptr((*f).name).to_string_lossy().into_owned()
            }
        }
    }
    /// `container.start_time` / `container.duration` (AV_TIME_BASE units, None for NOPTS).
    pub fn start_time(&self) -> Option<i64> {
        let v = unsafe { (*self.ctx).start_time };
        (v != NOPTS).then_some(v)
    }
    pub fn duration(&self) -> Option<i64> {
        let v = unsafe { (*self.ctx).duration };
        (v != NOPTS).then_some(v)
    }

    /// `av_find_best_stream(AUDIO)`.
    pub fn best_audio(&self) -> Option<usize> {
        let r = unsafe {
            sys::av_find_best_stream(self.ctx, sys::AVMediaType::AVMEDIA_TYPE_AUDIO, -1, -1, ptr::null_mut(), 0)
        };
        (r >= 0).then_some(r as usize)
    }

    /// One `av_read_frame`: `Ok(None)` at EOF; the packet gets its stream's time base. Errors carry the
    /// container name (PyAV `self.err_check`).
    pub fn read_packet(&mut self) -> Result<Option<Packet>, FfError> {
        let pkt = Packet::alloc();
        let ret = unsafe { sys::av_read_frame(self.ctx, pkt.ptr) };
        if ret == sys::AVERROR_EOF {
            return Ok(None);
        }
        check(ret, Some(&self.name))?;
        let mut pkt = pkt;
        let idx = pkt.stream_index();
        if idx >= 0 && (idx as usize) < self.nb_streams() {
            pkt.set_tb(self.stream_tb(idx as usize));
        }
        Ok(Some(pkt))
    }

    /// The empty flush packet PyAV yields for `stream` after EOF.
    pub fn flush_packet(&self, stream: usize) -> Packet {
        let mut pkt = Packet::alloc();
        unsafe { (*pkt.ptr).stream_index = stream as c_int };
        pkt.set_tb(self.stream_tb(stream));
        pkt
    }

    /// `container.seek(offset)` (backward, no stream) + `flush_buffers` of every open decoder.
    pub fn seek(&mut self, offset: i64) -> Result<(), FfError> {
        let ret = unsafe { sys::av_seek_frame(self.ctx, -1, offset, sys::AVSEEK_FLAG_BACKWARD) };
        check(ret, None)?;
        for d in self.decoders.iter_mut().flatten() {
            d.flush();
        }
        Ok(())
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.decoders.clear();
        unsafe { sys::avformat_close_input(&mut self.ctx) };
        // `io` (custom AVIOContext) is dropped afterwards by field order.
    }
}

// ---------------------------------------------------------------------------------------------------------------
// AudioResampler (PyAV)

struct Graph {
    ptr: *mut sys::AVFilterGraph,
    src: *mut sys::AVFilterContext,
    sink: *mut sys::AVFilterContext,
}

impl Drop for Graph {
    fn drop(&mut self) {
        unsafe { sys::avfilter_graph_free(&mut self.ptr) };
    }
}

unsafe fn add_filter(graph: *mut sys::AVFilterGraph, name: &str, args: &[(&str, String)]) -> Result<*mut sys::AVFilterContext, FfError> {
    unsafe {
        let cname = cstring(name);
        let filt = sys::avfilter_get_by_name(cname.as_ptr());
        if filt.is_null() {
            return Err(FfError::new(sys::AVERROR_FILTER_NOT_FOUND, None));
        }
        let ctx = sys::avfilter_graph_alloc_filter(graph, filt, cname.as_ptr());
        assert!(!ctx.is_null(), "Could not allocate AVFilterContext");
        let ret = if args.is_empty() {
            sys::avfilter_init_str(ctx, ptr::null())
        } else {
            let mut d = Dict(ptr::null_mut());
            for (k, v) in args {
                d.set(k, v);
            }
            sys::avfilter_init_dict(ctx, &mut d.0)
        };
        check(ret, None)?;
        Ok(ctx)
    }
}

/// PyAV `AudioResampler(format, layout, rate, frame_size)`.
pub struct AudioResampler {
    format: c_int,
    layout: Layout,
    rate: c_int,
    frame_size: c_int,
    graph: Option<Graph>,
    passthrough: bool,
    template: Option<(c_int, Layout, c_int)>,
}

impl AudioResampler {
    pub fn new(format: &str, channels: c_int, rate: c_int, frame_size: c_int) -> Self {
        Self::with_layout(sample_fmt_from_name(format), Layout::default_for(channels), rate, frame_size)
    }

    pub fn with_layout(format: c_int, layout: Layout, rate: c_int, frame_size: c_int) -> Self {
        AudioResampler { format, layout, rate, frame_size, graph: None, passthrough: false, template: None }
    }

    pub fn resample(&mut self, frame: Option<&Frame>) -> Result<Vec<Frame>, MediaError> {
        if self.graph.is_none() && frame.is_none() {
            return Ok(Vec::new());
        }
        if self.passthrough {
            return Ok(frame.map(|f| vec![f.clone_ref()]).unwrap_or_default());
        }
        if self.graph.is_none() {
            let f = frame.expect("checked above");
            self.template = Some((f.format(), Layout::copy_from(f.layout_ptr()), f.sample_rate()));
            if f.format() == self.format && self.layout.eq_ptr(f.layout_ptr()) && f.sample_rate() == self.rate && self.frame_size == 0 {
                self.passthrough = true;
                return Ok(vec![f.clone_ref()]);
            }
            unsafe {
                let gptr = sys::avfilter_graph_alloc();
                assert!(!gptr.is_null(), "avfilter_graph_alloc");
                let mut graph = Graph { ptr: gptr, src: ptr::null_mut(), sink: ptr::null_mut() };
                let mut args = vec![
                    ("sample_rate", f.sample_rate().to_string()),
                    ("sample_fmt", sample_fmt_name(f.format())),
                    ("channel_layout", describe_layout(f.layout_ptr())),
                ];
                if f.tb.num != 0 {
                    args.push(("time_base", fraction_str(f.tb)));
                }
                graph.src = add_filter(gptr, "abuffer", &args)?;
                let aformat = add_filter(
                    gptr,
                    "aformat",
                    &[
                        ("sample_rates", self.rate.to_string()),
                        ("sample_fmts", sample_fmt_name(self.format)),
                        ("channel_layouts", self.layout.describe()),
                    ],
                )?;
                graph.sink = add_filter(gptr, "abuffersink", &[])?;
                check(sys::avfilter_link(graph.src, 0, aformat, 0), None)?;
                check(sys::avfilter_link(aformat, 0, graph.sink, 0), None)?;
                check(sys::avfilter_graph_config(gptr, ptr::null_mut()), None)?;
                if self.frame_size > 0 {
                    sys::av_buffersink_set_frame_size(graph.sink, self.frame_size as u32);
                }
                self.graph = Some(graph);
            }
        }
        if let Some(f) = frame {
            let (fmt, layout, rate) = self.template.as_ref().expect("template set with the graph");
            if f.format() != *fmt || !layout.eq_ptr(f.layout_ptr()) || f.sample_rate() != *rate {
                return Err(resampler_mismatch());
            }
        }
        let graph = self.graph.as_ref().expect("graph built");
        unsafe {
            let ret = sys::av_buffersrc_write_frame(graph.src, frame.map_or(ptr::null(), |f| f.ptr as *const _));
            check(ret, None)?;
            let mut out = Vec::new();
            loop {
                let mut pulled = Frame::alloc();
                let ret = sys::av_buffersink_get_frame(graph.sink, pulled.ptr);
                if ret == sys::AVERROR_EOF || ret == EAGAIN {
                    break;
                }
                check(ret, None)?;
                pulled.tb = sys::av_buffersink_get_time_base(graph.sink);
                out.push(pulled);
            }
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// VideoReformatter (PyAV 18.1, new swscale API)

fn set_frame_colorspace(frame: *mut sys::AVFrame, sws_cs: c_int) {
    // SWS_CS_* -> AVColorSpace (range argument is always 0 = "do not touch").
    let target = match sws_cs {
        1 => Some(1), // ITU709 -> BT709
        4 => Some(4), // FCC
        5 => Some(6), // ITU601/SMPTE170M/DEFAULT -> SMPTE170M
        7 => Some(7), // SMPTE240M
        9 => Some(9), // BT2020 -> BT2020_NCL
        _ => None,
    };
    if let Some(v) = target {
        unsafe { (*frame).colorspace = std::mem::transmute::<u32, sys::AVColorSpace>(v) };
    }
}

/// `frame.reformat(width, height, format)` with PyAV's defaults (bilinear, threads 0, colorspace kept,
/// trc/primaries neutralized for the scale). Returns the source itself (a new reference) when it is a no-op.
pub fn reformat(src: &Frame, width: c_int, height: c_int, dst_format: c_int) -> Result<Frame, FfError> {
    unsafe {
        let sp = src.ptr;
        let dst = Frame::alloc();
        let dp = dst.ptr;
        let mut dst = dst;
        dst.tb = src.tb;
        sys::av_frame_copy_props(dp, sp);
        (*dp).format = dst_format;
        (*dp).width = width;
        (*dp).height = height;
        let src_trc = (*sp).color_trc;
        let src_pri = (*sp).color_primaries;
        (*sp).color_trc = sys::AVColorTransferCharacteristic::AVCOL_TRC_UNSPECIFIED;
        (*dp).color_trc = sys::AVColorTransferCharacteristic::AVCOL_TRC_UNSPECIFIED;
        (*sp).color_primaries = sys::AVColorPrimaries::AVCOL_PRI_UNSPECIFIED;
        (*dp).color_primaries = sys::AVColorPrimaries::AVCOL_PRI_UNSPECIFIED;
        let src_cs = (*sp).colorspace;
        let src_range = (*sp).color_range;
        let cs_value = src_cs as c_int;
        set_frame_colorspace(sp, cs_value);
        set_frame_colorspace(dp, cs_value);
        let restore = |sp: *mut sys::AVFrame| {
            (*sp).colorspace = src_cs;
            (*sp).color_range = src_range;
            (*sp).color_trc = src_trc;
            (*sp).color_primaries = src_pri;
        };
        if sys::sws_is_noop(dp, sp) != 0 {
            restore(sp);
            return Ok(src.clone_ref());
        }
        let mut sws = sys::sws_alloc_context();
        assert!(!sws.is_null(), "Could not allocate SwsContext");
        (*sws).threads = 0;
        (*sws).flags = sys::SwsFlags::SWS_BILINEAR as u32;
        let mut ret = 0;
        if width != 0 && height != 0 {
            ret = sys::av_frame_get_buffer(dp, 16);
        }
        if ret == 0 {
            ret = sys::sws_scale_frame(sws, dp, sp);
        }
        restore(sp);
        (*dp).color_trc = src_trc;
        (*dp).color_primaries = src_pri;
        sys::sws_free_context(&mut sws);
        check(ret, None)?;
        Ok(dst)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Output containers and encoders (PyAV OutputContainer / CodecContext.encode)

pub struct OutStream {
    pub st: *mut sys::AVStream,
    pub ctx: *mut sys::AVCodecContext,
    pub codec: *const sys::AVCodec,
    pub options: Vec<(String, String)>,
    pub open: bool,
    pub template_initialized: bool,
    pub is_video: bool,
    resampler: Option<AudioResampler>,
}

impl Drop for OutStream {
    fn drop(&mut self) {
        unsafe { sys::avcodec_free_context(&mut self.ctx) };
    }
}

pub struct Output {
    pub ctx: *mut sys::AVFormatContext,
    pub streams: Vec<OutStream>,
    pub name: String,
    started: bool,
    done: bool,
    owns_file_io: bool,
    pub io: Option<CustomIo>,
}

fn options_repr(options: &[(String, String)]) -> String {
    let items: Vec<String> = options.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr_str(v))).collect();
    format!("{{{}}}", items.join(", "))
}

impl Output {
    /// `av.open(path, "w", format=...)`: FFmpeg opens the file itself at `start_encoding`.
    pub fn create_path(path_utf8: &str, format: Option<&str>) -> Result<Output, MediaError> {
        Self::create(path_utf8, format, None)
    }

    /// `av.open(file_object, "w", format=...)` with custom write IO.
    pub fn create_custom(io: CustomIo, name: &str, format: &str) -> Result<Output, MediaError> {
        Self::create(name, Some(format), Some(io))
    }

    fn create(name: &str, format: Option<&str>, io: Option<CustomIo>) -> Result<Output, MediaError> {
        init();
        let cname = cstring(name);
        unsafe {
            let ofmt = match format {
                Some(f) => {
                    let cf = cstring(f);
                    sys::av_guess_format(cf.as_ptr(), ptr::null(), ptr::null())
                }
                None => sys::av_guess_format(ptr::null(), cname.as_ptr(), ptr::null()),
            };
            if ofmt.is_null() {
                return Err(EngineError::value("Could not determine output format").into());
            }
            let mut ctx = ptr::null_mut();
            check(sys::avformat_alloc_output_context2(&mut ctx, ofmt, ptr::null(), cname.as_ptr()), Some(name))?;
            (*ctx).flags |= sys::AVFMT_FLAG_GENPTS;
            let owns_file_io = io.is_none();
            if let Some(io) = &io {
                (*ctx).pb = io.pb;
            }
            Ok(Output { ctx, streams: Vec::new(), name: name.to_string(), started: false, done: false, owns_file_io, io })
        }
    }

    fn global_header(&self) -> bool {
        unsafe { (*(*self.ctx).oformat).flags & sys::AVFMT_GLOBALHEADER != 0 }
    }

    /// `add_stream(codec_name, rate, options)`; returns the stream index. Video: `rate` = frame rate; audio:
    /// `rate` = sample rate, `channels` = layout.
    pub fn add_stream(&mut self, codec_name: &str, rate: AVRational, channels: c_int, options: &[(&str, &str)]) -> Result<usize, MediaError> {
        unsafe {
            let cn = cstring(codec_name);
            let mut codec = sys::avcodec_find_encoder_by_name(cn.as_ptr());
            if codec.is_null() {
                let desc = sys::avcodec_descriptor_get_by_name(cn.as_ptr());
                if !desc.is_null() {
                    codec = sys::avcodec_find_encoder((*desc).id);
                }
            }
            if codec.is_null() {
                return Err(EngineError::value(format!("unknown encoder {}", repr_str(codec_name))).into());
            }
            let st = sys::avformat_new_stream(self.ctx, codec);
            let ctx = sys::avcodec_alloc_context3(codec);
            assert!(!st.is_null() && !ctx.is_null(), "avformat_new_stream");
            let is_video = (*codec).type_ == sys::AVMediaType::AVMEDIA_TYPE_VIDEO;
            if is_video {
                (*ctx).pix_fmt = sys::AVPixelFormat::AV_PIX_FMT_YUV420P;
                (*ctx).width = 640;
                (*ctx).height = 480;
                (*ctx).bit_rate = 0;
                (*ctx).bit_rate_tolerance = 128000;
                (*ctx).framerate = rate;
                (*st).avg_frame_rate = (*ctx).framerate;
                (*st).time_base = (*ctx).time_base;
            } else {
                let mut out: *const c_void = ptr::null();
                sys::avcodec_get_supported_config(
                    ptr::null(),
                    codec,
                    sys::AVCodecConfig::AV_CODEC_CONFIG_SAMPLE_FORMAT,
                    0,
                    &mut out,
                    ptr::null_mut(),
                );
                if !out.is_null() {
                    (*ctx).sample_fmt = *(out as *const sys::AVSampleFormat);
                }
                (*ctx).bit_rate = 0;
                (*ctx).bit_rate_tolerance = 32000;
                (*ctx).sample_rate = rate.num;
                (*st).time_base = (*ctx).time_base;
                sys::av_channel_layout_default(&mut (*ctx).ch_layout, 2);
                if channels != 2 {
                    sys::av_channel_layout_uninit(&mut (*ctx).ch_layout);
                    sys::av_channel_layout_default(&mut (*ctx).ch_layout, channels);
                }
            }
            if self.global_header() {
                (*ctx).flags |= sys::AV_CODEC_FLAG_GLOBAL_HEADER as c_int;
            }
            check(sys::avcodec_parameters_from_context((*st).codecpar, ctx), None)?;
            (*ctx).thread_count = 0;
            (*ctx).thread_type = sys::FF_THREAD_SLICE;
            self.streams.push(OutStream {
                st,
                ctx,
                codec,
                options: options.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                open: false,
                template_initialized: false,
                is_video,
                resampler: None,
            });
            Ok(self.streams.len() - 1)
        }
    }

    /// `add_stream_from_template(template)` (stream copy).
    pub fn add_stream_from_template(&mut self, input: &Input, index: usize) -> Result<usize, MediaError> {
        unsafe {
            let in_st = input.stream(index);
            let dec = input.decoders[index].as_ref();
            let is_video = (*(*in_st).codecpar).codec_type == sys::AVMediaType::AVMEDIA_TYPE_VIDEO;
            let codec: *const sys::AVCodec = match dec {
                None => ptr::null(),
                Some(d) if !is_video => d.codec,
                Some(d) => {
                    let n = cstring(&d.name());
                    let mut c = sys::avcodec_find_encoder_by_name(n.as_ptr());
                    if c.is_null() {
                        c = sys::avcodec_find_encoder((*d.codec).id);
                    }
                    c
                }
            };
            let st = sys::avformat_new_stream(self.ctx, codec);
            let ctx = sys::avcodec_alloc_context3(codec);
            assert!(!st.is_null() && !ctx.is_null(), "avformat_new_stream");
            check(sys::avcodec_parameters_to_context(ctx, (*in_st).codecpar), None)?;
            (*ctx).codec_tag = 0;
            (*st).time_base = (*in_st).time_base;
            (*ctx).time_base = (*in_st).time_base;
            if self.global_header() {
                (*ctx).flags |= sys::AV_CODEC_FLAG_GLOBAL_HEADER as c_int;
            }
            if is_video && let Some(d) = dec {
                (*ctx).flags = (*d.ctx).flags;
            }
            check(sys::avcodec_parameters_from_context((*st).codecpar, ctx), None)?;
            (*ctx).thread_count = 0;
            (*ctx).thread_type = sys::FF_THREAD_SLICE;
            self.streams.push(OutStream {
                st,
                ctx,
                codec,
                options: Vec::new(),
                open: false,
                template_initialized: true,
                is_video,
                resampler: None,
            });
            Ok(self.streams.len() - 1)
        }
    }

    /// `CodecContext.open(strict=False)` for an encoder.
    pub fn open_encoder(&mut self, index: usize) -> Result<(), FfError> {
        let s = &mut self.streams[index];
        if s.open {
            return Ok(());
        }
        unsafe {
            let ctx = s.ctx;
            if (*ctx).time_base.num == 0 {
                if s.is_video {
                    (*ctx).time_base.num = if (*ctx).framerate.den != 0 { (*ctx).framerate.den } else { 1 };
                    (*ctx).time_base.den = if (*ctx).framerate.num != 0 { (*ctx).framerate.num } else { sys::AV_TIME_BASE as c_int };
                } else {
                    (*ctx).time_base = q(1, (*ctx).sample_rate);
                }
            }
            let mut opts = Dict(ptr::null_mut());
            for (k, v) in &s.options {
                opts.set(k, v);
            }
            let ret = sys::avcodec_open2(ctx, s.codec, &mut opts.0);
            let filename = format!("avcodec_open2(\"{}\", {})", codec_name(s.codec), options_repr(&s.options));
            check(ret, Some(&filename))?;
            s.open = true;
        }
        Ok(())
    }

    pub fn set_video_size(&mut self, index: usize, width: c_int, height: c_int) {
        unsafe {
            (*self.streams[index].ctx).width = width;
            (*self.streams[index].ctx).height = height;
        }
    }
    pub fn set_time_base(&mut self, index: usize, tb: AVRational) {
        unsafe {
            (*self.streams[index].st).time_base = tb;
            (*self.streams[index].ctx).time_base = tb;
        }
    }
    pub fn set_bit_rate(&mut self, index: usize, bit_rate: i64) {
        unsafe { (*self.streams[index].ctx).bit_rate = bit_rate };
    }

    /// `Stream.encode(frame)` (PyAV): open, prepare (audio: internal resampler to the encoder's frame size;
    /// video: reformat to the encoder's size/format, pts from `frame_num`), rebase to the codec time base,
    /// send and receive. `None` flushes.
    pub fn encode(&mut self, index: usize, frame: Option<Frame>) -> Result<Vec<Packet>, MediaError> {
        self.open_encoder(index)?;
        let s = &mut self.streams[index];
        let ctx = s.ctx;
        let mut frames: Vec<Option<Frame>> = Vec::new();
        unsafe {
            if s.is_video {
                match frame {
                    None => frames.push(None),
                    Some(f) => {
                        let mut f = if f.format() != (*ctx).pix_fmt as c_int || f.width() != (*ctx).width || f.height() != (*ctx).height {
                            reformat(&f, (*ctx).width, (*ctx).height, (*ctx).pix_fmt as c_int)?
                        } else {
                            f
                        };
                        if f.pts() == NOPTS {
                            f.set_pts((*ctx).frame_num);
                        }
                        frames.push(Some(f));
                    }
                }
            } else {
                if s.resampler.is_none() {
                    let var = (*s.codec).capabilities & sys::AV_CODEC_CAP_VARIABLE_FRAME_SIZE as c_int != 0;
                    s.resampler = Some(AudioResampler::with_layout(
                        (*ctx).sample_fmt as c_int,
                        Layout::copy_from(&(*ctx).ch_layout),
                        (*ctx).sample_rate,
                        if var { 0 } else { (*ctx).frame_size },
                    ));
                }
                let r = s.resampler.as_mut().expect("just set");
                let is_flush = frame.is_none();
                for f in r.resample(frame.as_ref())? {
                    frames.push(Some(f));
                }
                if is_flush {
                    frames.push(None);
                }
            }
            let tb = (*ctx).time_base;
            let mut packets = Vec::new();
            for f in frames.iter_mut() {
                if let Some(f) = f {
                    rebase_frame(f, tb);
                }
                let ret = sys::avcodec_send_frame(ctx, f.as_ref().map_or(ptr::null(), |f| f.ptr as *const _));
                check(ret, Some("avcodec_send_frame()"))?;
                loop {
                    let mut pkt = Packet::alloc();
                    let ret = sys::avcodec_receive_packet(ctx, pkt.ptr);
                    if ret == EAGAIN || ret == sys::AVERROR_EOF {
                        break;
                    }
                    check(ret, Some("avcodec_receive_packet()"))?;
                    pkt.set_tb(tb);
                    (*pkt.ptr).stream_index = (*s.st).index;
                    packets.push(pkt);
                }
            }
            Ok(packets)
        }
    }

    fn start_encoding(&mut self) -> Result<(), MediaError> {
        if self.started {
            return Ok(());
        }
        for i in 0..self.streams.len() {
            if !self.streams[i].open && !self.streams[i].template_initialized {
                self.open_encoder(i)?;
            }
            let s = &self.streams[i];
            unsafe {
                if (*s.st).time_base.num == 0 {
                    (*s.st).time_base = (*s.ctx).time_base;
                }
                check(sys::avcodec_parameters_from_context((*s.st).codecpar, s.ctx), None)?;
                if s.is_video {
                    (*s.st).avg_frame_rate = (*s.ctx).framerate;
                }
            }
        }
        unsafe {
            if self.owns_file_io && (*self.ctx).pb.is_null() && (*(*self.ctx).oformat).flags & sys::AVFMT_NOFILE == 0 {
                let cname = cstring(&self.name);
                check(sys::avio_open(&mut (*self.ctx).pb, cname.as_ptr(), sys::AVIO_FLAG_WRITE), None)?;
            }
            let mut opts = Dict(ptr::null_mut());
            let ret = sys::avformat_write_header(self.ctx, &mut opts.0);
            self.check_io(ret)?;
        }
        self.started = true;
        Ok(())
    }

    fn check_io(&mut self, ret: c_int) -> Result<c_int, MediaError> {
        if ret < 0 {
            if let Some(e) = self.io.as_mut().and_then(CustomIo::take_stashed) {
                return Err(EngineError::os(&e, None).into());
            }
            return Err(FfError::new(ret, Some(&self.name)).into());
        }
        Ok(ret)
    }

    /// `OutputContainer.mux_one(packet)` (the packet's `stream_index` names the output stream).
    pub fn mux(&mut self, mut pkt: Packet) -> Result<(), MediaError> {
        self.start_encoding()?;
        let idx = pkt.stream_index();
        if idx < 0 || idx as usize >= self.streams.len() {
            return Err(EngineError::value("Bad Packet stream_index.").into());
        }
        unsafe {
            let st_tb = (*self.streams[idx as usize].st).time_base;
            let tb = pkt.tb();
            if tb.num == 0 {
                pkt.set_tb(st_tb);
            } else if tb.num != st_tb.num || tb.den != st_tb.den {
                sys::av_packet_rescale_ts(pkt.ptr, tb, st_tb);
                pkt.set_tb(st_tb);
            }
            let ret = sys::av_interleaved_write_frame(self.ctx, pkt.ptr);
            self.check_io(ret)?;
        }
        Ok(())
    }

    pub fn mux_all(&mut self, packets: Vec<Packet>) -> Result<(), MediaError> {
        for p in packets {
            self.mux(p)?;
        }
        Ok(())
    }

    /// `close_output`: the trailer once if the header was written, then close FFmpeg's own file.
    pub fn close(&mut self) -> Result<(), MediaError> {
        if self.started && !self.done {
            self.done = true;
            let ret = unsafe { sys::av_write_trailer(self.ctx) };
            let res = self.check_io(ret).map(|_| ());
            if self.owns_file_io {
                unsafe { sys::avio_closep(&mut (*self.ctx).pb) };
            }
            return res;
        }
        Ok(())
    }

    pub fn header_written(&self) -> bool {
        self.started
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.close();
        self.streams.clear();
        unsafe {
            if self.owns_file_io && !(*self.ctx).pb.is_null() {
                sys::avio_closep(&mut (*self.ctx).pb);
            }
            if !self.owns_file_io {
                (*self.ctx).pb = ptr::null_mut();
            }
            sys::avformat_free_context(self.ctx);
        }
    }
}

/// PyAV `Frame._rebase_time(dst)`.
pub fn rebase_frame(f: &mut Frame, dst: AVRational) {
    if f.tb.num == 0 {
        f.tb = dst;
        return;
    }
    if f.tb.num == dst.num && f.tb.den == dst.den {
        return;
    }
    unsafe {
        if (*f.ptr).pts != NOPTS {
            (*f.ptr).pts = sys::av_rescale_q((*f.ptr).pts, f.tb, dst);
        }
        if (*f.ptr).duration != 0 {
            (*f.ptr).duration = sys::av_rescale_q((*f.ptr).duration, f.tb, dst);
        }
    }
    f.tb = dst;
}

/// Exact conversion of `duration * time_base` to f64 (Python `float(Fraction)`), exact while the product fits.
pub fn rational_seconds(value: i64, tb: AVRational) -> f64 {
    let num = value as i128 * tb.num as i128;
    num as f64 / tb.den as f64
}

/// The error kind used for messages that are plain Python exceptions without a dedicated class.
pub fn plain_error(message: impl Into<String>) -> EngineError {
    EngineError::new(ErrorKind::Other, message)
}
