//! `audio.py`: video → 16 kHz mono s16 WAV with FFmpeg (spec `audio.md` §4).

use std::path::{Path, PathBuf};

use se_core::paths::py_path_str;
use se_core::pyfmt::round_half_even;
use se_core::{EngineError, ProgressFn, Result, t};

use crate::ff::{self, AudioResampler, FfError, Input, MediaError, sys};
use crate::wav::WavWriter;

/// Output sample rate.
pub const SAMPLE_RATE: u32 = 16_000;
/// Drifts under 50 ms are not padded.
pub const GAP_TOLERANCE: i64 = SAMPLE_RATE as i64 / 20;
/// Ignore bogus PTS spikes > 60 s in damaged AVI/WMV files.
pub const MAX_MIDSTREAM_GAP_SAMPLES: i64 = 60 * SAMPLE_RATE as i64;
/// Allow up to 10 min initial audio delay.
pub const MAX_INITIAL_DELAY_SAMPLES: i64 = 600 * SAMPLE_RATE as i64;
/// Give up after this many consecutive demux errors.
pub const MAX_CONSECUTIVE_DEMUX_ERRORS: u32 = 512;

/// `extract_audio(video, out_wav, on_progress)`: writes `out_wav` and returns it.
///
/// Errors (`ErrorKind::AudioExtraction`, localized): `video_not_found` (`video` as given, when it is not a
/// file), `no_audio_track`, `audio_read_failed`; FFmpeg errors keep PyAV's text (see spec §9.1).
/// Progress: fractions at least 1% apart, ending at 1.0; none when the container reports no duration.
pub fn extract_audio(video: &Path, out_wav: &Path, on_progress: Option<ProgressFn<'_>>) -> Result<PathBuf> {
    extract_audio_impl(video, out_wav, on_progress, &[])
}

/// Test hook (port of `test_corrupt_packet_mid_stream_is_skipped_without_failing_or_drifting`): the demuxed
/// packets of the selected stream whose 0-based positions are in `failing_packets` raise
/// `InvalidDataError(1094995529, ..., "avcodec_send_packet()")` from `decode()` instead of being decoded.
#[doc(hidden)]
pub fn extract_audio_with_failing_packets(
    video: &Path,
    out_wav: &Path,
    on_progress: Option<ProgressFn<'_>>,
    failing_packets: &[usize],
) -> Result<PathBuf> {
    extract_audio_impl(video, out_wav, on_progress, failing_packets)
}

fn extract_audio_impl(
    video: &Path,
    out_wav: &Path,
    mut on_progress: Option<ProgressFn<'_>>,
    failing_packets: &[usize],
) -> Result<PathBuf> {
    if !video.is_file() {
        return Err(EngineError::audio(t!("video_not_found", video = py_path_str(video))));
    }
    if let Some(parent) = out_wav.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| EngineError::win(&e, Some(parent)))?;
    }
    match extract_container(video, out_wav, &mut on_progress, failing_packets) {
        Ok(()) => Ok(out_wav.to_path_buf()),
        Err(MediaError::Ff(e)) => Err(EngineError::audio(t!("audio_read_failed", error = e))),
        Err(MediaError::Engine(e)) => Err(e),
    }
}

fn extract_container(
    video: &Path,
    out_wav: &Path,
    on_progress: &mut Option<ProgressFn<'_>>,
    failing_packets: &[usize],
) -> Result<(), MediaError> {
    let mut input = Input::open_path(&py_path_str(video))?;
    let audio = input.streams_of(sys::AVMediaType::AVMEDIA_TYPE_AUDIO);
    if audio.is_empty() {
        return Err(EngineError::audio(t!("no_audio_track")).into());
    }
    let best = input.best_audio().unwrap_or(audio[0]);
    let mut candidates = vec![best];
    candidates.extend(audio.iter().copied().filter(|&i| i != best));
    let last = candidates.len() - 1;
    for (idx, &stream) in candidates.iter().enumerate() {
        let attempt = (|| -> Result<usize, MediaError> {
            if idx > 0 {
                input.seek(0)?;
            }
            extract_stream_to_wav(&mut input, stream, out_wav, on_progress, failing_packets)
        })();
        match attempt {
            Ok(count) => {
                if count > 0 || idx == last {
                    break;
                }
            }
            Err(MediaError::Ff(e)) => {
                if idx == last {
                    return Err(MediaError::Ff(e));
                }
            }
            Err(other) => return Err(other),
        }
    }
    Ok(())
}

/// `_stream_duration_seconds(container, stream)`.
fn stream_duration_seconds(input: &Input, stream: usize) -> Option<f64> {
    unsafe {
        let st = input.stream(stream);
        let tb = (*st).time_base;
        if (*st).duration != ff::NOPTS && tb.den != 0 {
            let dur = ff::rational_seconds((*st).duration, tb);
            if dur > 0.0 {
                return Some(dur);
            }
        }
    }
    match input.duration() {
        Some(d) if d > 0 => Some(d as f64 / 1_000_000.0),
        _ => None,
    }
}

/// `_iter_audio_frames(container, stream)` as a pull iterator with Python's generator semantics: the first
/// demux error ends demuxing for good (no flush packet), decode errors are skipped (the 2nd and further
/// consecutive ones flush the decoder), and the first error is raised only when nothing was decoded.
struct AudioFrames<'a> {
    input: &'a mut Input,
    stream: usize,
    failing: &'a [usize],
    packet_no: usize,
    demux_done: bool,
    flushed: bool,
    decoded_any: bool,
    first_error: Option<FfError>,
    demux_errors: u32,
    decode_errors: u32,
    pending: std::collections::VecDeque<ff::Frame>,
}

impl AudioFrames<'_> {
    fn next_frame(&mut self) -> Result<Option<ff::Frame>, FfError> {
        loop {
            if let Some(f) = self.pending.pop_front() {
                return Ok(Some(f));
            }
            let packet = if self.demux_done {
                None
            } else {
                loop {
                    match self.input.read_packet() {
                        Ok(None) => break None,
                        Ok(Some(p)) => {
                            if p.stream_index() as usize == self.stream {
                                break Some(p);
                            }
                        }
                        Err(e) => {
                            // A raising generator is finished: no further packets, no flush packet.
                            self.first_error.get_or_insert(e);
                            self.demux_errors += 1;
                            self.demux_done = true;
                            self.flushed = true;
                            break None;
                        }
                    }
                }
            };
            let packet = match packet {
                Some(p) => p,
                None => {
                    self.demux_done = true;
                    if self.flushed {
                        if !self.decoded_any
                            && let Some(e) = self.first_error.take()
                        {
                            return Err(e);
                        }
                        return Ok(None);
                    }
                    self.flushed = true;
                    self.input.flush_packet(self.stream)
                }
            };
            self.demux_errors = 0;
            let packet_no = self.packet_no;
            self.packet_no += 1;
            // The Python test wrapper hides `is_corrupt` (getattr default False).
            if self.failing.is_empty() && packet.is_corrupt() {
                continue;
            }
            let decoded = if self.failing.contains(&packet_no) {
                Err(FfError::new(sys::AVERROR_INVALIDDATA, Some("avcodec_send_packet()")))
            } else {
                match self.input.decoders[self.stream].as_mut() {
                    None => Err(FfError::new(sys::AVERROR_DECODER_NOT_FOUND, None)),
                    Some(d) => d.decode(&packet),
                }
            };
            match decoded {
                Err(e) => {
                    self.first_error.get_or_insert(e);
                    self.decode_errors += 1;
                    if self.decode_errors >= 2
                        && let Some(d) = self.input.decoders[self.stream].as_mut()
                    {
                        d.flush();
                    }
                }
                Ok(frames) => {
                    self.decode_errors = 0;
                    if !frames.is_empty() {
                        self.decoded_any = true;
                    }
                    self.pending.extend(frames);
                }
            }
            let _ = MAX_CONSECUTIVE_DEMUX_ERRORS; // unreachable cap: demuxing ends at the first error
        }
    }
}

fn write_skipped(wav: &mut WavWriter, pcm: &[u8], skip_samples: &mut i64) -> Result<()> {
    let mut pcm = pcm;
    if *skip_samples > 0 {
        let drop_bytes = (pcm.len() as i64).min(*skip_samples * 2) as usize;
        pcm = &pcm[drop_bytes..];
        *skip_samples -= (drop_bytes / 2) as i64;
    }
    if !pcm.is_empty() {
        wav.write(pcm)?;
    }
    Ok(())
}

/// `_extract_stream_to_wav(container, stream, out_wav, on_progress)` → number of decoded frames.
fn extract_stream_to_wav(
    input: &mut Input,
    stream: usize,
    out_wav: &Path,
    on_progress: &mut Option<ProgressFn<'_>>,
    failing: &[usize],
) -> Result<usize, MediaError> {
    let origin = match input.start_time() {
        Some(s) if (s as i128).abs() < 1_000_000_000_000_000 => s as f64 / 1_000_000.0,
        _ => 0.0,
    };
    let total_duration = stream_duration_seconds(input, stream);
    let rate = SAMPLE_RATE as f64;
    let mut last_reported = -1.0f64;
    let mut cursor = 0.0f64;
    let mut skip_samples: i64 = 0;
    let mut frame_count: usize = 0;
    let mut resampler = AudioResampler::new("s16", 1, SAMPLE_RATE as i32, 0);
    let mut wav = WavWriter::create(out_wav, SAMPLE_RATE)?;
    let mut frames = AudioFrames {
        input,
        stream,
        failing,
        packet_no: 0,
        demux_done: false,
        flushed: false,
        decoded_any: false,
        first_error: None,
        demux_errors: 0,
        decode_errors: 0,
        pending: Default::default(),
    };
    let body = (|| -> Result<(), MediaError> {
        while let Some(frame) = frames.next_frame()? {
            if let Some(time) = frame.time() {
                let target = (time - origin) * rate;
                let diff = target - cursor;
                let max_pad = if frame_count == 0 { MAX_INITIAL_DELAY_SAMPLES } else { MAX_MIDSTREAM_GAP_SAMPLES } as f64;
                let tol = GAP_TOLERANCE as f64;
                let mid = MAX_MIDSTREAM_GAP_SAMPLES as f64;
                if tol < diff && diff <= max_pad {
                    let pad = round_half_even(diff);
                    wav.write_silence(pad as u64)?;
                    cursor += pad;
                } else if tol < -diff && -diff <= mid {
                    let drop = round_half_even(-diff);
                    skip_samples += drop as i64;
                    cursor -= drop;
                } else if diff.abs() > max_pad && frame_count > 0 {
                    cursor = target;
                }
            }
            frame_count += 1;
            cursor += (frame.nb_samples() as i64 * SAMPLE_RATE as i64) as f64 / frame.sample_rate() as f64;
            if let (Some(cb), Some(total)) = (on_progress.as_deref_mut(), total_duration)
                && total > 0.0
            {
                let fraction = (cursor / (total * rate)).max(0.0).min(1.0);
                if fraction - last_reported >= 0.01 || fraction >= 1.0 {
                    last_reported = fraction;
                    cb(fraction);
                }
            }
            for chunk in resampler.resample(Some(&frame))? {
                write_skipped(&mut wav, &chunk.audio_bytes(), &mut skip_samples)?;
            }
        }
        for chunk in resampler.resample(None)? {
            write_skipped(&mut wav, &chunk.audio_bytes(), &mut skip_samples)?;
        }
        Ok(())
    })();
    let closed = wav.close();
    body?;
    closed?;
    Ok(frame_count)
}
