//! Media fixtures and probes for tests: Rust versions of the PyAV code in the Python test suite
//! (`conftest.tone_video`, `test_audio.py`, `test_preview.py`), built with the same FFmpeg encoders through
//! [`crate::ff`]. Not part of the engine's runtime API.

use std::f64::consts::PI;
use std::ffi::c_int;
use std::path::Path;

use se_core::paths::py_path_str;

use crate::ff::{self, Frame, Input, MediaError, Output, q, sys};

pub type GenResult<T> = std::result::Result<T, String>;

fn err(e: MediaError) -> String {
    match e {
        MediaError::Ff(e) => e.to_string(),
        MediaError::Engine(e) => e.to_string(),
    }
}

fn mux_encoded(out: &mut Output, index: usize, frame: Option<Frame>) -> GenResult<()> {
    let packets = out.encode(index, frame).map_err(err)?;
    out.mux_all(packets).map_err(err)
}

fn f32_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn fltp_frame(planes: &[Vec<f32>], rate: c_int, pts: i64) -> Frame {
    let bytes: Vec<Vec<u8>> = planes.iter().map(|p| f32_bytes(p)).collect();
    let refs: Vec<&[u8]> = bytes.iter().map(|b| b.as_slice()).collect();
    let mut f = Frame::audio_from_planes("fltp", planes.len() as c_int, rate, planes[0].len() as c_int, &refs);
    f.set_pts(pts);
    f
}

fn rgb_frame(size: c_int, value: u8, pts: i64) -> Frame {
    let rgb = vec![value; (size * size * 3) as usize];
    let mut f = Frame::video_rgb24(size, size, &rgb);
    f.set_pts(pts);
    f
}

/// `conftest.tone_video`: `tone.mp4`, libx264 64x64 @10 fps (20 black frames) + AAC 44.1 kHz stereo, 2 s sine
/// 440 Hz at amplitude 0.3 in 1024-sample fltp frames (pts = sample index).
pub fn tone_video(path: &Path) -> GenResult<()> {
    let (rate, seconds) = (44_100i64, 2i64);
    let mut out = Output::create_path(&py_path_str(path), None).map_err(err)?;
    let v = out.add_stream("libx264", q(10, 1), 0, &[]).map_err(err)?;
    out.set_video_size(v, 64, 64);
    let a = out.add_stream("aac", q(rate as c_int, 1), 2, &[]).map_err(err)?;
    for i in 0..seconds * 10 {
        mux_encoded(&mut out, v, Some(rgb_frame(64, 0, i)))?;
    }
    let total = (rate * seconds) as usize;
    let samples: Vec<f32> = (0..total)
        .map(|i| {
            let t = i as f64 / rate as f64;
            ((2.0 * PI * 440.0 * t).sin() * 0.3) as f32
        })
        .collect();
    for start in (0..total).step_by(1024) {
        let block = samples[start..(start + 1024).min(total)].to_vec();
        mux_encoded(&mut out, a, Some(fltp_frame(&[block.clone(), block], rate as c_int, start as i64)))?;
    }
    mux_encoded(&mut out, v, None)?;
    mux_encoded(&mut out, a, None)?;
    out.close().map_err(err)
}

/// `test_late_audio_start_is_padded_to_container_timeline`: 4 s of video, mono AAC starting at 2 s.
pub fn late_audio_mp4(path: &Path) -> GenResult<()> {
    let rate = 44_100i64;
    let mut out = Output::create_path(&py_path_str(path), None).map_err(err)?;
    let v = out.add_stream("libx264", q(10, 1), 0, &[]).map_err(err)?;
    out.set_video_size(v, 64, 64);
    let a = out.add_stream("aac", q(rate as c_int, 1), 1, &[]).map_err(err)?;
    for i in 0..40 {
        mux_encoded(&mut out, v, Some(rgb_frame(64, 0, i)))?;
    }
    for i in (0..rate * 2).step_by(1024) {
        mux_encoded(&mut out, a, Some(fltp_frame(&[vec![0.2f32; 1024]], rate as c_int, 2 * rate + i)))?;
    }
    mux_encoded(&mut out, v, None)?;
    mux_encoded(&mut out, a, None)?;
    out.close().map_err(err)
}

/// `test_48khz_aac_does_not_accumulate_rounding_drift`: audio-only mp4, silent 48 kHz mono AAC.
pub fn aac_48k_mp4(path: &Path, seconds: i64) -> GenResult<()> {
    let rate = 48_000i64;
    let mut out = Output::create_path(&py_path_str(path), None).map_err(err)?;
    let a = out.add_stream("aac", q(rate as c_int, 1), 1, &[]).map_err(err)?;
    for pts in (0..rate * seconds).step_by(1024) {
        mux_encoded(&mut out, a, Some(fltp_frame(&[vec![0.0f32; 1024]], rate as c_int, pts)))?;
    }
    mux_encoded(&mut out, a, None)?;
    out.close().map_err(err)
}

/// `test_wmv_avi_mov_mkv_mp4_containers_with_corrupt_midstream_bytes`: an audio-only container (format from
/// the extension) with 4 s of a 44.1 kHz stereo tone at 128 kb/s; then 64 bytes at `len / 3` are set to 0xFF.
pub fn corrupt_tone_container(path: &Path, codec: &str) -> GenResult<()> {
    let rate = 44_100i64;
    {
        let mut out = Output::create_path(&py_path_str(path), None).map_err(err)?;
        let a = out.add_stream(codec, q(rate as c_int, 1), 2, &[]).map_err(err)?;
        out.set_bit_rate(a, 128_000);
        let t: Vec<f32> = (0..1024).map(|i| i as f32 / 44_100f32).collect();
        let left: Vec<f32> = t.iter().map(|&t| 0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()).collect();
        let right: Vec<f32> = t.iter().map(|&t| 0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).cos()).collect();
        for pts in (0..rate * 4).step_by(1024) {
            mux_encoded(&mut out, a, Some(fltp_frame(&[left.clone(), right.clone()], rate as c_int, pts)))?;
        }
        mux_encoded(&mut out, a, None)?;
        out.close().map_err(err)?;
    }
    let mut raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let offset = raw.len() / 3;
    let end = (offset + 64).min(raw.len());
    raw[offset..end].fill(0xFF);
    std::fs::write(path, raw).map_err(|e| e.to_string())
}

/// `test_previews_as_seekable_mp4` source: 64x64 @10 fps, 20 frames of gray level `i`, 48 kHz mono audio
/// (20 silent s16 frames of 4800 samples) starting 0.5 s late; for `wmv` the first audio packet at/after video
/// frame 10 is overwritten with 0xFF.
pub fn preview_source(path: &Path, video_codec: &str, audio_codec: &str, video_options: &[(&str, &str)]) -> GenResult<()> {
    let is_wmv = py_path_str(path).to_lowercase().ends_with(".wmv");
    let mut out = Output::create_path(&py_path_str(path), None).map_err(err)?;
    let v = out.add_stream(video_codec, q(10, 1), 0, video_options).map_err(err)?;
    out.set_video_size(v, 64, 64);
    let a = out.add_stream(audio_codec, q(48_000, 1), 1, &[]).map_err(err)?;
    if audio_codec == "wmav2" {
        out.set_bit_rate(a, 64_000);
    }
    let silence = vec![0u8; 4800 * 2];
    let mut damaged = false;
    for i in 0..20i64 {
        mux_encoded(&mut out, v, Some(rgb_frame(64, i as u8, i)))?;
        let mut samples = Frame::audio_from_planes("s16", 1, 48_000, 4800, &[&silence]);
        samples.set_pts(24_000 + i * 4800);
        let packets = out.encode(a, Some(samples)).map_err(err)?;
        for mut p in packets {
            if is_wmv && i >= 10 && !damaged {
                p.fill(0xFF);
                damaged = true;
            }
            out.mux(p).map_err(err)?;
        }
    }
    mux_encoded(&mut out, v, None)?;
    mux_encoded(&mut out, a, None)?;
    out.close().map_err(err)
}

/// What `test_previews_as_seekable_mp4` checks on the output.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewProbe {
    pub video_codec: String,
    pub audio_codec: String,
    /// `audio.start_time * audio.time_base`.
    pub audio_start: f64,
    /// `container.duration / AV_TIME_BASE`.
    pub duration: f64,
    pub first_video_time: f64,
    /// After `seek(1_000_000)`, the time of the last decoded video frame.
    pub last_video_time_after_seek: f64,
}

fn decode_video_frames(input: &mut Input, video: usize, limit: Option<usize>) -> GenResult<Vec<f64>> {
    let mut times = Vec::new();
    let mut flushed = false;
    loop {
        let packet = match input.read_packet().map_err(|e| e.to_string())? {
            Some(p) if p.stream_index() as usize == video => p,
            Some(_) => continue,
            None if !flushed => {
                flushed = true;
                input.flush_packet(video)
            }
            None => return Ok(times),
        };
        let dec = input.decoders[video].as_mut().ok_or("no decoder")?;
        for f in dec.decode(&packet).map_err(|e| e.to_string())? {
            times.push(f.time().unwrap_or(f64::NAN));
            if limit.is_some_and(|l| times.len() >= l) {
                return Ok(times);
            }
        }
    }
}

pub fn probe_preview(path: &Path) -> GenResult<PreviewProbe> {
    let mut input = Input::open_path(&py_path_str(path)).map_err(|e| e.to_string())?;
    let video = *input.streams_of(sys::AVMediaType::AVMEDIA_TYPE_VIDEO).first().ok_or("no video stream")?;
    let audio = *input.streams_of(sys::AVMediaType::AVMEDIA_TYPE_AUDIO).first().ok_or("no audio stream")?;
    let name = |i: usize| input.decoders[i].as_ref().map(|d| d.name()).unwrap_or_default();
    let (video_codec, audio_codec) = (name(video), name(audio));
    let audio_start = unsafe {
        let st = input.stream(audio);
        ff::rational_seconds((*st).start_time, (*st).time_base)
    };
    let duration = input.duration().unwrap_or(0) as f64 / 1_000_000.0;
    let first_video_time = *decode_video_frames(&mut input, video, Some(1))?.first().ok_or("no video frame")?;
    input.seek(1_000_000).map_err(|e| e.to_string())?;
    let all = decode_video_frames(&mut input, video, None)?;
    let last_video_time_after_seek = *all.last().ok_or("no video frame after seek")?;
    Ok(PreviewProbe { video_codec, audio_codec, audio_start, duration, first_video_time, last_video_time_after_seek })
}

/// Names of the decoders of the first video and audio streams (for quick checks).
pub fn stream_codecs(path: &Path) -> GenResult<Vec<String>> {
    let input = Input::open_path(&py_path_str(path)).map_err(|e| e.to_string())?;
    Ok((0..input.nb_streams()).map(|i| input.decoders[i].as_ref().map(|d| d.name()).unwrap_or_default()).collect())
}

/// A video-only Matroska file (libx264, 10 frames).
pub fn video_only(path: &Path) -> GenResult<()> {
    let mut out = Output::create_path(&py_path_str(path), None).map_err(err)?;
    let v = out.add_stream("libx264", q(10, 1), 0, &[]).map_err(err)?;
    out.set_video_size(v, 64, 64);
    for i in 0..10 {
        mux_encoded(&mut out, v, Some(rgb_frame(64, 0, i)))?;
    }
    mux_encoded(&mut out, v, None)?;
    out.close().map_err(err)
}

/// A `pcm_s16le` WAV (FFmpeg's wav muxer) holding `samples` (interleaved) in 1000-sample frames.
pub fn pcm_wav(path: &Path, rate: c_int, channels: c_int, samples: &[i16]) -> GenResult<()> {
    let mut out = Output::create_path(&py_path_str(path), None).map_err(err)?;
    let a = out.add_stream("pcm_s16le", q(rate, 1), channels, &[]).map_err(err)?;
    let per = 1000 * channels as usize;
    for (n, chunk) in samples.chunks(per).enumerate() {
        let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut f = Frame::audio_from_planes("s16", channels, rate, (chunk.len() / channels as usize) as c_int, &[&bytes]);
        f.set_pts((n * 1000) as i64);
        mux_encoded(&mut out, a, Some(f))?;
    }
    mux_encoded(&mut out, a, None)?;
    out.close().map_err(err)
}
