//! `preview.py`: a local MP4 playback copy for formats AVFoundation cannot play (spec `audio.md` §6).
//!
//! Deliberate deviations from Python on Windows only (spec §6.1): piped stdin is not seekable
//! (`GetFileType != FILE_TYPE_DISK`), and on failure the output handle is closed before the partial output is
//! deleted, so the original error is reported instead of `[WinError 32] ...`.

use std::collections::{HashMap, HashSet};
use std::ffi::c_int;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use se_core::paths::py_path_str;
use se_core::{EngineError, ErrorKind, Result, t};

use crate::ff::{self, AudioResampler, CustomIo, FfError, Input, MediaError, Output, q, sys};

/// The preview source: a file, or the already-open file on stdin (`preview -`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewSource {
    Path(PathBuf),
    Stdin,
}

fn attribute_error() -> MediaError {
    MediaError::Engine(EngineError::new(ErrorKind::Type, "'NoneType' object has no attribute 'name'"))
}

fn ff_error(e: FfError, io: Option<&mut CustomIo>) -> EngineError {
    if let Some(stashed) = io.and_then(CustomIo::take_stashed) {
        return EngineError::os(&stashed, None);
    }
    EngineError::runtime(e.to_string())
}

#[cfg(windows)]
fn stdin_file() -> std::io::Result<(File, bool)> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileType(handle: *mut std::ffi::c_void) -> u32;
    }
    let handle = std::io::stdin().as_raw_handle();
    // Borrow the process handle without ever closing it; the clone is a duplicated handle we own.
    let borrowed = std::mem::ManuallyDrop::new(unsafe { File::from_raw_handle(handle) });
    let file = borrowed.try_clone()?;
    let seekable = unsafe { GetFileType(handle as *mut _) } == 1; // FILE_TYPE_DISK
    Ok((file, seekable))
}

#[cfg(unix)]
fn stdin_file() -> std::io::Result<(File, bool)> {
    use std::os::fd::FromRawFd;
    let borrowed = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let mut file = borrowed.try_clone()?;
    let seekable = file.seek(SeekFrom::Current(0)).is_ok();
    Ok((file, seekable))
}

/// `make_preview(source, output)`: reserves `output` exclusively (`create_new`), copies compatible tracks or
/// converts them to H.264/AAC MP4. No network, no model downloads. Errors: `preview_no_streams` (localized),
/// FFmpeg/OS errors with Python's texts; a failed run removes the partial output.
pub fn make_preview(source: PreviewSource, output: &Path) -> Result<()> {
    ff::init();
    let (file, name, seekable) = match &source {
        PreviewSource::Path(p) => {
            let mut f = File::open(p).map_err(|e| EngineError::os(&e, Some(p)))?;
            let seekable = f.seek(SeekFrom::Current(0)).is_ok();
            (f, py_path_str(p), seekable)
        }
        PreviewSource::Stdin => {
            let (f, seekable) = stdin_file().map_err(|e| EngineError::os(&e, None))?;
            (f, "<stdin>".to_string(), seekable)
        }
    };
    let io = CustomIo::new(file, seekable, false);
    let mut input = match Input::open_custom(io, &name, &[("protocol_whitelist", "pipe")]) {
        Ok(i) => i,
        Err(e) => return Err(EngineError::runtime(e.to_string())),
    };
    let video = input.streams_of(sys::AVMediaType::AVMEDIA_TYPE_VIDEO).into_iter().find(|&i| unsafe {
        (*input.stream(i)).disposition & sys::AV_DISPOSITION_ATTACHED_PIC == 0
    });
    let audio = input.streams_of(sys::AVMediaType::AVMEDIA_TYPE_AUDIO).into_iter().next();
    if video.is_none() && audio.is_none() {
        return Err(EngineError::value(t!("preview_no_streams")));
    }

    let out_file = OpenOptions::new().write(true).create_new(true).open(output).map_err(|e| EngineError::os(&e, Some(output)))?;
    let result = (|| -> Result<(), MediaError> {
        let mut writer = Output::create_custom(CustomIo::new(out_file, true, true), &py_path_str(output), "mp4")?;
        let converted = convert(&mut input, &mut writer, video, audio);
        let closed = writer.close();
        drop(writer); // closes the output handle before any delete
        converted?;
        closed
    })();
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(output);
            Err(match err {
                MediaError::Ff(e) => ff_error(e, input.io.as_mut()),
                MediaError::Engine(e) => e,
            })
        }
    }
}

fn convert(input: &mut Input, writer: &mut Output, video: Option<usize>, audio: Option<usize>) -> Result<(), MediaError> {
    let mut copied: HashMap<usize, usize> = HashMap::new();
    let mut encoders: Vec<(usize, usize)> = Vec::new();
    let mut resampler: Option<AudioResampler> = None;
    let mut decoded: HashSet<usize> = HashSet::new();
    let mut decode_errors: Vec<(usize, FfError)> = Vec::new();
    let is_windows_media = input.format_name() == "asf";
    let mut video_size = (0 as c_int, 0 as c_int);

    if let Some(v) = video {
        let dec = input.decoders[v].as_ref().ok_or_else(attribute_error)?;
        let (name, pix_fmt, has_b_frames, width, height) = unsafe {
            let c = dec.ctx;
            (dec.name(), ff::pix_fmt_name((*c).pix_fmt as c_int), (*c).has_b_frames, (*c).width, (*c).height)
        };
        let pix_ok = matches!(pix_fmt.as_deref(), Some("yuv420p") | Some("yuvj420p"));
        if name == "h264" && pix_ok && has_b_frames == 0 {
            copied.insert(v, writer.add_stream_from_template(input, v)?);
        } else {
            let avg = unsafe { (*input.stream(v)).avg_frame_rate };
            let rate = if avg.num != 0 && avg.den != 0 { avg } else { q(30, 1) };
            let out = writer.add_stream("libx264", rate, 0, &[("preset", "ultrafast"), ("crf", "21")])?;
            if width == 0 || height == 0 {
                return Err(MediaError::Engine(ff::plain_error("division by zero")));
            }
            let scale = 1.0f64.min(1920.0 / width as f64).min(1080.0 / height as f64);
            let even = |x: c_int| (((x as f64 * scale).trunc() as i64).div_euclid(2) * 2).max(2) as c_int;
            video_size = (even(width), even(height));
            writer.set_video_size(out, video_size.0, video_size.1);
            writer.set_time_base(out, q(1, 90_000));
            encoders.push((v, out));
        }
    }
    if let Some(a) = audio {
        let dec = input.decoders[a].as_ref().ok_or_else(attribute_error)?;
        let channels = unsafe { (*dec.ctx).ch_layout.nb_channels };
        if dec.name() == "aac" && (channels == 1 || channels == 2) {
            copied.insert(a, writer.add_stream_from_template(input, a)?);
        } else {
            let out = writer.add_stream("aac", q(48_000, 1), 2, &[])?;
            encoders.push((a, out));
            resampler = Some(AudioResampler::new("fltp", 2, 48_000, 0));
        }
    }
    let encoder_of = |index: usize| encoders.iter().find(|(i, _)| *i == index).map(|(_, o)| *o);
    let selected: Vec<usize> = [video, audio].into_iter().flatten().collect();

    let mut handle = |input: &mut Input, writer: &mut Output, mut packet: ff::Packet| -> Result<(), MediaError> {
        let index = packet.stream_index() as usize;
        if let Some(&out) = copied.get(&index) {
            if packet.size() == 0 {
                return Ok(());
            }
            unsafe {
                if (*packet.ptr).dts == ff::NOPTS {
                    (*packet.ptr).dts = (*packet.ptr).pts;
                }
                (*packet.ptr).stream_index = out as c_int;
            }
            return writer.mux(packet);
        }
        let out = encoder_of(index).expect("selected streams are copied or encoded");
        let frames = match input.decoders[index].as_mut() {
            None => Err(FfError::new(sys::AVERROR_DECODER_NOT_FOUND, None)),
            Some(d) => d.decode(&packet),
        };
        let frames = match frames {
            Ok(f) => f,
            Err(e) if e.is_invalid_data() && is_windows_media => {
                if !decode_errors.iter().any(|(i, _)| *i == index) {
                    decode_errors.push((index, e));
                }
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        if !frames.is_empty() {
            decoded.insert(index);
        }
        let stream_tb = input.stream_tb(index);
        for frame in frames {
            if Some(index) == video {
                let mut converted = ff::reformat(&frame, video_size.0, video_size.1, sys::AVPixelFormat::AV_PIX_FMT_YUV420P as c_int)?;
                converted.tb = if frame.tb.num != 0 { frame.tb } else { stream_tb };
                let packets = writer.encode(out, Some(converted))?;
                writer.mux_all(packets)?;
            } else {
                let r = resampler.as_mut().expect("audio encoder has a resampler");
                for c in r.resample(Some(&frame))? {
                    let packets = writer.encode(out, Some(c))?;
                    writer.mux_all(packets)?;
                }
            }
        }
        let _ = &mut packet;
        Ok(())
    };

    loop {
        match input.read_packet()? {
            None => break,
            Some(p) => {
                if selected.contains(&(p.stream_index() as usize)) {
                    handle(input, writer, p)?;
                }
            }
        }
    }
    for i in 0..input.nb_streams() {
        if selected.contains(&i) {
            let p = input.flush_packet(i);
            handle(input, writer, p)?;
        }
    }
    drop(handle);

    for (index, error) in decode_errors {
        if !decoded.contains(&index) {
            return Err(error.into());
        }
    }
    if let (Some(r), Some(a)) = (resampler.as_mut(), audio) {
        let out = encoder_of(a).expect("audio encoder");
        for f in r.resample(None)? {
            let packets = writer.encode(out, Some(f))?;
            writer.mux_all(packets)?;
        }
    }
    for &(_, out) in &encoders {
        let packets = writer.encode(out, None)?;
        writer.mux_all(packets)?;
    }
    Ok(())
}
