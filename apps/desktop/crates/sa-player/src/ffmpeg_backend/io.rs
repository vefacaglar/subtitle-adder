//! Opening a [`MediaSource`] as an FFmpeg demuxer: the file protocol for paths, a custom `AVIOContext` over a
//! `Read + Seek` (Windows `BufferedVideoStream` over the encrypted chunk cache) otherwise. Every blocking FFmpeg call
//! is interruptible through the session's stop flag.

use std::ffi::{CString, c_int, c_void};
use std::io::{Read, Seek, SeekFrom};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ffmpeg_next::ffi;
use ffmpeg_next::format::context::Input;

use crate::{FailureKind, MediaSource, ReadSeek};

const IO_BUFFER_SIZE: usize = 256 * 1024;

struct IoOpaque {
    reader: Box<dyn ReadSeek>,
    stop: Arc<AtomicBool>,
}

/// An opened demuxer plus the custom I/O it reads through (freed after the format context).
pub(crate) struct OpenedInput {
    input: Option<Input>,
    pb: *mut ffi::AVIOContext,
    opaque: *mut IoOpaque,
    _stop: Arc<AtomicBool>,
}

// SAFETY: the format context, the AVIO context and the reader are only used by the thread that owns this value.
unsafe impl Send for OpenedInput {}

impl OpenedInput {
    pub fn input(&mut self) -> &mut Input {
        self.input.as_mut().expect("input is present until drop")
    }
}

impl Drop for OpenedInput {
    fn drop(&mut self) {
        // Close the demuxer first: it may still read through `pb` while closing.
        drop(self.input.take());
        unsafe { free_custom_io(self.pb, self.opaque) };
    }
}

unsafe fn free_custom_io(pb: *mut ffi::AVIOContext, opaque: *mut IoOpaque) {
    unsafe {
        if !pb.is_null() {
            let mut pb = pb;
            // The buffer may have been reallocated by FFmpeg: free the current one.
            ffi::av_freep(ptr::addr_of_mut!((*pb).buffer) as *mut c_void);
            ffi::avio_context_free(&mut pb);
        }
        if !opaque.is_null() {
            drop(Box::from_raw(opaque));
        }
    }
}

unsafe extern "C" fn interrupt_cb(opaque: *mut c_void) -> c_int {
    let stop = unsafe { &*(opaque as *const AtomicBool) };
    stop.load(Ordering::Relaxed) as c_int
}

unsafe extern "C" fn read_cb(opaque: *mut c_void, buf: *mut u8, size: c_int) -> c_int {
    let io = unsafe { &mut *(opaque as *mut IoOpaque) };
    if io.stop.load(Ordering::Relaxed) {
        return ffi::AVERROR_EXIT;
    }
    if size <= 0 {
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, size as usize) };
    loop {
        match io.reader.read(slice) {
            Ok(0) => return ffi::AVERROR_EOF,
            Ok(n) => return n as c_int,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return ffi::AVERROR(ffi::EIO),
        }
    }
}

unsafe extern "C" fn seek_cb(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let io = unsafe { &mut *(opaque as *mut IoOpaque) };
    let whence = whence & !(ffi::AVSEEK_FORCE);
    if whence == ffi::AVSEEK_SIZE {
        let Ok(here) = io.reader.stream_position() else { return -1 };
        let Ok(end) = io.reader.seek(SeekFrom::End(0)) else { return -1 };
        if io.reader.seek(SeekFrom::Start(here)).is_err() {
            return -1;
        }
        return end as i64;
    }
    let target = match whence {
        0 => SeekFrom::Start(offset.max(0) as u64), // SEEK_SET
        1 => SeekFrom::Current(offset),            // SEEK_CUR
        2 => SeekFrom::End(offset),                // SEEK_END
        _ => return -1,
    };
    match io.reader.seek(target) {
        Ok(p) => p as i64,
        Err(_) => -1,
    }
}

/// Human-readable FFmpeg error text.
pub(crate) fn error_text(code: c_int) -> String {
    ffmpeg_next::Error::from(code).to_string()
}

/// Why opening failed: the message for the UI and its [`FailureKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenFailure {
    pub message: String,
    pub kind: FailureKind,
}

impl OpenFailure {
    pub fn new(message: impl Into<String>, kind: FailureKind) -> Self {
        Self { message: message.into(), kind }
    }

    pub fn from_code(code: c_int) -> Self {
        Self { message: error_text(code), kind: classify(ffmpeg_next::Error::from(code)) }
    }
}

/// POSIX errno values FFmpeg wraps with `AVERROR(e)`; identical in the Windows CRT, macOS and Linux.
const EPERM: c_int = 1;
const ENOENT: c_int = 2;
const EIO: c_int = 5;
const EACCES: c_int = 13;

/// Maps an FFmpeg error to the decision the macOS player makes (engine preview fallback or not).
pub(crate) fn classify(e: ffmpeg_next::Error) -> FailureKind {
    use ffmpeg_next::Error as E;
    match e {
        E::InvalidData
        | E::DemuxerNotFound
        | E::DecoderNotFound
        | E::StreamNotFound
        | E::PatchWelcome
        | E::BsfNotFound
        | E::Experimental => FailureKind::UnsupportedFormat,
        E::Other { errno } if matches!(errno, EPERM | ENOENT | EIO | EACCES) => FailureKind::Io,
        E::Eof => FailureKind::Io,
        _ => FailureKind::Other,
    }
}

/// Opens and probes `source`. Blocking; aborts with an error once `stop` is set.
pub(crate) fn open_input(source: MediaSource, stop: Arc<AtomicBool>) -> Result<OpenedInput, OpenFailure> {
    let (url, reader) = match source {
        MediaSource::File(path) => (path.to_string_lossy().into_owned(), None),
        MediaSource::Reader { path, open } => {
            let reader = open().map_err(|e| OpenFailure::new(e.to_string(), FailureKind::Io))?;
            (path.to_string_lossy().into_owned(), Some(reader))
        }
    };
    let url = CString::new(url).map_err(|_| OpenFailure::new("invalid path", FailureKind::Io))?;
    unsafe {
        let mut ctx = ffi::avformat_alloc_context();
        if ctx.is_null() {
            return Err(OpenFailure::from_code(ffi::AVERROR(ffi::ENOMEM)));
        }
        (*ctx).interrupt_callback =
            ffi::AVIOInterruptCB { callback: Some(interrupt_cb), opaque: Arc::as_ptr(&stop) as *mut c_void };
        let mut pb: *mut ffi::AVIOContext = ptr::null_mut();
        let mut opaque: *mut IoOpaque = ptr::null_mut();
        if let Some(reader) = reader {
            opaque = Box::into_raw(Box::new(IoOpaque { reader, stop: stop.clone() }));
            let buffer = ffi::av_malloc(IO_BUFFER_SIZE) as *mut u8;
            if buffer.is_null() {
                ffi::avformat_free_context(ctx);
                free_custom_io(ptr::null_mut(), opaque);
                return Err(OpenFailure::from_code(ffi::AVERROR(ffi::ENOMEM)));
            }
            pb = ffi::avio_alloc_context(
                buffer,
                IO_BUFFER_SIZE as c_int,
                0,
                opaque as *mut c_void,
                Some(read_cb),
                None,
                Some(seek_cb),
            );
            if pb.is_null() {
                ffi::av_free(buffer as *mut c_void);
                ffi::avformat_free_context(ctx);
                free_custom_io(ptr::null_mut(), opaque);
                return Err(OpenFailure::from_code(ffi::AVERROR(ffi::ENOMEM)));
            }
            (*ctx).pb = pb;
            (*ctx).flags |= ffi::AVFMT_FLAG_CUSTOM_IO;
        }
        // On failure avformat_open_input frees `ctx` (but never a custom pb).
        let r = ffi::avformat_open_input(&mut ctx, url.as_ptr(), ptr::null(), ptr::null_mut());
        if r < 0 {
            free_custom_io(pb, opaque);
            return Err(OpenFailure::from_code(r));
        }
        let r = ffi::avformat_find_stream_info(ctx, ptr::null_mut());
        if r < 0 {
            ffi::avformat_close_input(&mut ctx);
            free_custom_io(pb, opaque);
            return Err(OpenFailure::from_code(r));
        }
        Ok(OpenedInput { input: Some(Input::wrap(ctx)), pb, opaque, _stop: stop })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_classified_for_the_preview_fallback() {
        assert_eq!(classify(ffmpeg_next::Error::InvalidData), FailureKind::UnsupportedFormat);
        assert_eq!(classify(ffmpeg_next::Error::DecoderNotFound), FailureKind::UnsupportedFormat);
        assert_eq!(classify(ffmpeg_next::Error::DemuxerNotFound), FailureKind::UnsupportedFormat);
        assert_eq!(classify(ffmpeg_next::Error::Other { errno: ENOENT }), FailureKind::Io);
        assert_eq!(classify(ffmpeg_next::Error::Other { errno: EACCES }), FailureKind::Io);
        assert_eq!(classify(ffmpeg_next::Error::Other { errno: ffi::ENOMEM }), FailureKind::Other);
        assert_eq!(OpenFailure::from_code(ffi::AVERROR_INVALIDDATA).kind, FailureKind::UnsupportedFormat);
    }
}
