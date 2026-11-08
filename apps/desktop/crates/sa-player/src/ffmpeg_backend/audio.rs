//! Audio: swresample to interleaved f32 at the device rate (divided by the playback speed) and channel count, a
//! ring of decoded chunks, and the cpal output stream that consumes it and anchors the master clock.

use std::collections::VecDeque;
use std::ffi::c_int;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use ffmpeg_next::ffi;
use parking_lot::{Condvar, Mutex};

use super::session::Control;

/// Converted audio waiting for the device.
pub(crate) struct AudioChunk {
    pub serial: u64,
    /// Media time of the first frame of `samples`.
    pub pts: f64,
    /// Media seconds per output frame (`1 / out_rate`, so speed is included).
    pub frame_dur: f64,
    pub samples: Vec<f32>,
    /// Interleaved channel count of `samples` (a chunk made for another device format is dropped).
    pub channels: u16,
    /// Read position in samples.
    pub pos: usize,
}

#[derive(Default)]
pub(crate) struct RingState {
    pub chunks: VecDeque<AudioChunk>,
    /// Frames (per channel) queued.
    pub frames: usize,
    /// The decoder delivered everything of this serial.
    pub eof_serial: Option<u64>,
}

#[derive(Default)]
pub(crate) struct AudioRing {
    pub state: Mutex<RingState>,
    pub space: Condvar,
}

impl AudioRing {
    pub fn flush(&self) {
        let mut s = self.state.lock();
        s.chunks.clear();
        s.frames = 0;
        s.eof_serial = None;
        self.space.notify_all();
    }
}

/// Device format chosen by the output thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeviceFormat {
    pub rate: u32,
    pub channels: u16,
}

impl DeviceFormat {
    /// Packed into one atomic word (`Control::device_format`); 0 = none yet.
    pub fn pack(self) -> u64 {
        ((self.rate as u64) << 16) | self.channels as u64
    }

    pub fn unpack(bits: u64) -> Option<Self> {
        let f = Self { rate: (bits >> 16) as u32, channels: (bits & 0xffff) as u16 };
        (f.rate > 0 && f.channels > 0).then_some(f)
    }
}

/// Whether a cpal stream error leaves the stream usable. Underruns, a refused real-time priority and an automatic
/// reroute to the new default device (CoreAudio) are reported but harmless; everything else (device removed, stream
/// invalidated by a default-device change on WASAPI, backend errors) needs a new stream. Media Foundation and
/// AVPlayer both follow the default output device, so the player does too.
pub(crate) fn is_recoverable_in_place(kind: cpal::ErrorKind) -> bool {
    matches!(kind, cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied)
}

/// Delay between attempts to open an output device after the previous one was lost.
const REOPEN_RETRY: Duration = Duration::from_secs(1);
/// Minimum time between two stream rebuilds (a device that fails right after opening must not spin).
const REOPEN_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// swresample wrapper (recreated on every serial change, format change or speed change).
pub(crate) struct Resampler {
    ctx: *mut ffi::SwrContext,
    key: (c_int, c_int, u64, c_int, c_int),
    pub out_rate: i32,
    out_channels: i32,
}

// SAFETY: used by the audio decode thread only.
unsafe impl Send for Resampler {}

impl Drop for Resampler {
    fn drop(&mut self) {
        unsafe { ffi::swr_free(&mut self.ctx) };
    }
}

impl Resampler {
    /// A resampler for `frame` → f32 interleaved `out_channels` at `out_rate`. Reuses `old` when it matches.
    pub unsafe fn for_frame(old: Option<Resampler>, frame: *const ffi::AVFrame, out_rate: i32, out_channels: i32) -> Option<Resampler> {
        unsafe {
            let in_rate = (*frame).sample_rate;
            let in_fmt = (*frame).format;
            let in_ch = (*frame).ch_layout.nb_channels;
            if in_rate <= 0 || in_ch <= 0 || in_fmt < 0 {
                return None;
            }
            let mask = if (*frame).ch_layout.order == ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE {
                (*frame).ch_layout.u.mask
            } else {
                0
            };
            let key = (in_rate, in_fmt, mask ^ ((in_ch as u64) << 56), out_rate, out_channels);
            if let Some(old) = old {
                if old.key == key {
                    return Some(old);
                }
            }
            let mut in_layout: ffi::AVChannelLayout = std::mem::zeroed();
            if (*frame).ch_layout.order == ffi::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC {
                ffi::av_channel_layout_default(&mut in_layout, in_ch);
            } else if ffi::av_channel_layout_copy(&mut in_layout, &(*frame).ch_layout) < 0 {
                ffi::av_channel_layout_default(&mut in_layout, in_ch);
            }
            let mut out_layout: ffi::AVChannelLayout = std::mem::zeroed();
            ffi::av_channel_layout_default(&mut out_layout, out_channels);
            let mut ctx: *mut ffi::SwrContext = ptr::null_mut();
            let r = ffi::swr_alloc_set_opts2(
                &mut ctx,
                &out_layout,
                ffi::AVSampleFormat::AV_SAMPLE_FMT_FLT,
                out_rate,
                &in_layout,
                std::mem::transmute::<c_int, ffi::AVSampleFormat>(in_fmt),
                in_rate,
                0,
                ptr::null_mut(),
            );
            ffi::av_channel_layout_uninit(&mut in_layout);
            ffi::av_channel_layout_uninit(&mut out_layout);
            if r < 0 || ctx.is_null() || ffi::swr_init(ctx) < 0 {
                ffi::swr_free(&mut ctx);
                return None;
            }
            Some(Resampler { ctx, key, out_rate, out_channels })
        }
    }

    /// Converts one decoded frame; returns interleaved samples.
    pub unsafe fn convert(&mut self, frame: *const ffi::AVFrame) -> Vec<f32> {
        unsafe {
            let in_rate = (*frame).sample_rate as i64;
            let n = (*frame).nb_samples;
            let delay = ffi::swr_get_delay(self.ctx, in_rate);
            let cap = (((delay + n as i64) * self.out_rate as i64) / in_rate.max(1) + 64) as usize;
            let mut out = vec![0f32; cap * self.out_channels as usize];
            let out_ptrs = [out.as_mut_ptr() as *mut u8];
            let got = ffi::swr_convert(
                self.ctx,
                out_ptrs.as_ptr(),
                cap as c_int,
                (*frame).extended_data as *const *const u8,
                n,
            );
            if got <= 0 {
                return Vec::new();
            }
            out.truncate(got as usize * self.out_channels as usize);
            out
        }
    }
}

/// Runs the cpal output stream for one session until it stops. Reports the device format (or why there is none)
/// through `ready` before it starts consuming. When the device is lost later (unplugged, default device changed on
/// WASAPI) the clock falls back to the wall clock, a stream is opened on the current default device (retried every
/// second while there is none) and audio resumes from the current position.
pub(crate) fn output_thread(ctl: Arc<Control>, ready: crossbeam_channel::Sender<Result<DeviceFormat, String>>) {
    let host = cpal::default_host();
    let mut ready = Some(ready);
    let mut last_open: Option<Instant> = None;
    loop {
        if ctl.stopped() {
            return;
        }
        if let Some(t) = last_open {
            let since = t.elapsed();
            if since < REOPEN_MIN_INTERVAL {
                std::thread::sleep(REOPEN_MIN_INTERVAL - since);
            }
        }
        last_open = Some(Instant::now());
        ctl.audio_lost.store(false, Ordering::Release);
        let opened = open_stream(&host, &ctl);
        let (stream, format) = match opened {
            Ok(s) => s,
            Err(e) => {
                if let Some(tx) = ready.take() {
                    // No audio output at all: the session runs on the wall clock (as without an audio stream).
                    let _ = tx.send(Err(e));
                    return;
                }
                // Still no device: keep playing on the wall clock and try again.
                ctl.audio_wake.wait(REOPEN_RETRY);
                continue;
            }
        };
        ctl.device_format.store(format.pack(), Ordering::Release);
        match ready.take() {
            Some(tx) => {
                let _ = tx.send(Ok(format));
            }
            None => audio_restored(&ctl),
        }
        let mut running = false;
        // The stream runs only while playing (no silent device activity while paused).
        while !ctl.stopped() && !ctl.audio_lost.load(Ordering::Acquire) {
            let want = ctl.playing.load(Ordering::Relaxed);
            if want != running {
                let r = if want { stream.play() } else { stream.pause() };
                if r.is_ok() {
                    running = want;
                }
            }
            ctl.audio_wake.wait(Duration::from_millis(50));
        }
        let _ = stream.pause();
        drop(stream);
    }
}

/// Opens an output stream on the current default device in its default format.
fn open_stream(host: &cpal::Host, ctl: &Arc<Control>) -> Result<(cpal::Stream, DeviceFormat), String> {
    let device = host.default_output_device().ok_or_else(|| "no audio output device".to_owned())?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let format = DeviceFormat { rate: config.sample_rate(), channels: config.channels().max(1) };
    let stream_config = config.config();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, stream_config, ctl, format),
        cpal::SampleFormat::I16 => build::<i16>(&device, stream_config, ctl, format),
        cpal::SampleFormat::U16 => build::<u16>(&device, stream_config, ctl, format),
        cpal::SampleFormat::I32 => build::<i32>(&device, stream_config, ctl, format),
        cpal::SampleFormat::U32 => build::<u32>(&device, stream_config, ctl, format),
        cpal::SampleFormat::F64 => build::<f64>(&device, stream_config, ctl, format),
        cpal::SampleFormat::I8 => build::<i8>(&device, stream_config, ctl, format),
        cpal::SampleFormat::U8 => build::<u8>(&device, stream_config, ctl, format),
        other => Err(format!("unsupported sample format {other:?}")),
    }?;
    Ok((stream, format))
}

/// The output stream is gone: run the clock on the wall clock until a new stream is open.
pub(crate) fn audio_lost(ctl: &Control) {
    if ctl.audio_lost.swap(true, Ordering::AcqRel) {
        return;
    }
    ctl.audio_failed.store(true, Ordering::Release);
    ctl.clock.audio_finished();
    ctl.audio_ring.space.notify_all();
    ctl.audio_wake.notify();
    ctl.wake.notify();
}

/// A new output stream is open (its format is in `Control::device_format`): the audio clock takes over again from
/// the current position. The seek drops audio converted for the old device and re-decodes from here.
pub(crate) fn audio_restored(ctl: &Control) {
    ctl.audio_failed.store(false, Ordering::Release);
    if ctl.stopped() || !ctl.audio_active.load(Ordering::Acquire) || ctl.ended.load(Ordering::Acquire) || !ctl.info.lock().opened {
        // Not playing anything right now: the next start or seek re-enables the audio clock.
        return;
    }
    let p = ctl.position();
    ctl.request_seek(p);
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    ctl: &Arc<Control>,
    format: DeviceFormat,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let data_ctl = ctl.clone();
    let err_ctl = ctl.clone();
    let channels = format.channels as usize;
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                let ts = info.timestamp();
                let latency = ts.playback.duration_since(ts.callback);
                scratch.clear();
                scratch.resize(data.len(), 0.0);
                fill(&data_ctl, &mut scratch, channels, Instant::now() + latency);
                for (d, s) in data.iter_mut().zip(scratch.iter()) {
                    *d = T::from_sample(*s);
                }
            },
            move |e: cpal::Error| {
                if is_recoverable_in_place(e.kind()) {
                    if e.kind() != cpal::ErrorKind::Xrun {
                        eprintln!("[player] audio output: {e}");
                    }
                    return;
                }
                eprintln!("[player] audio output lost: {e}");
                audio_lost(&err_ctl);
            },
            None,
        )
        .map_err(|e| e.to_string())
}

/// The device callback: copies queued samples (volume applied) and anchors the clock.
pub(crate) fn fill(ctl: &Control, out: &mut [f32], channels: usize, heard_at: Instant) {
    if ctl.stopped() || !ctl.clock.is_running() || !ctl.clock.is_audio_master() {
        return;
    }
    let serial = ctl.serial.load(Ordering::Acquire);
    let gain = if ctl.player.muted.load(Ordering::Relaxed) { 0.0 } else { f64::from_bits(ctl.player.volume.load(Ordering::Relaxed)) as f32 };
    let frames_wanted = out.len() / channels;
    // First callback since a start or seek of a source with a picture: start at the clock (`Clock::audio_start_at`).
    let skip_to = ctl.clock.audio_start_at(heard_at);
    let mut skipped = 0usize;
    let mut written = 0usize;
    let mut first_pts: Option<f64> = None;
    let mut end_pts = 0.0;
    let mut finished = false;
    {
        let mut ring = ctl.audio_ring.state.lock();
        while written < frames_wanted {
            let Some(chunk) = ring.chunks.front_mut() else { break };
            if chunk.serial != serial || chunk.channels as usize != channels {
                // Stale (seek) or converted for a device that is gone.
                let left = (chunk.samples.len() - chunk.pos) / (chunk.channels.max(1) as usize);
                ring.frames = ring.frames.saturating_sub(left);
                ring.chunks.pop_front();
                continue;
            }
            if let Some(to) = skip_to
                && written == 0
                && chunk.frame_dur > 0.0
            {
                let at = chunk.pts + (chunk.pos / channels) as f64 * chunk.frame_dur;
                if at < to {
                    let avail = (chunk.samples.len() - chunk.pos) / channels;
                    let n = (((to - at) / chunk.frame_dur).ceil() as usize).min(avail);
                    chunk.pos += n * channels;
                    skipped += n;
                    if chunk.pos >= chunk.samples.len() {
                        ring.chunks.pop_front();
                        continue;
                    }
                }
            }
            let avail = (chunk.samples.len() - chunk.pos) / channels;
            let take = avail.min(frames_wanted - written);
            if first_pts.is_none() {
                first_pts = Some(chunk.pts + (chunk.pos / channels) as f64 * chunk.frame_dur);
            }
            let src = &chunk.samples[chunk.pos..chunk.pos + take * channels];
            let dst = &mut out[written * channels..(written + take) * channels];
            for (d, s) in dst.iter_mut().zip(src) {
                *d = s * gain;
            }
            chunk.pos += take * channels;
            written += take;
            end_pts = chunk.pts + (chunk.pos / channels) as f64 * chunk.frame_dur;
            if chunk.pos >= chunk.samples.len() {
                ring.chunks.pop_front();
            }
        }
        ring.frames = ring.frames.saturating_sub(written + skipped);
        if ring.chunks.is_empty() && ring.eof_serial == Some(serial) {
            finished = true;
        }
    }
    ctl.audio_ring.space.notify_all();
    if let Some(p) = first_pts {
        ctl.clock.audio_anchor(p, heard_at, end_pts);
    }
    if finished && !ctl.audio_drained.swap(true, Ordering::AcqRel) {
        // Let the remaining video run on the wall clock.
        ctl.clock.audio_finished();
        ctl.wake.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg_backend::session::PlayerShared;

    fn running_control() -> Arc<Control> {
        let player = Arc::new(PlayerShared::new(false));
        let g = player.gate.begin_load();
        player.gate.try_activate_source(g, Some(1));
        let ctl = Control::new(player, g, 1, true, 1.0);
        ctl.clock.set_ready(true, true);
        ctl.clock.release();
        ctl
    }

    fn push(ctl: &Control, serial: u64, pts: f64, samples: Vec<f32>) {
        let mut ring = ctl.audio_ring.state.lock();
        ring.frames += samples.len() / 2;
        ring.chunks.push_back(AudioChunk { serial, pts, frame_dur: 1.0 / 48_000.0, samples, channels: 2, pos: 0 });
    }

    #[test]
    fn fill_applies_volume_and_mute_and_anchors_the_clock() {
        let ctl = running_control();
        let serial = ctl.serial.load(Ordering::Acquire);
        push(&ctl, serial, 2.0, vec![0.5; 8]);
        ctl.player.volume.store(0.5f64.to_bits(), Ordering::Relaxed);
        let mut out = [9.0f32; 4];
        fill(&ctl, &mut out, 2, Instant::now());
        assert_eq!(out, [0.25; 4]);
        // Anchored to the first sample written; capped at the end of what was written.
        let now = ctl.clock.now();
        assert!((2.0..=2.0 + 2.0 / 48_000.0 + 1e-9).contains(&now), "{now}");
        ctl.player.muted.store(true, Ordering::Relaxed);
        let mut out = [9.0f32; 4];
        fill(&ctl, &mut out, 2, Instant::now());
        assert_eq!(out, [0.0; 4]);
        // Ring drained: the rest is silence and the frame count is back to zero.
        let mut out = [9.0f32; 4];
        fill(&ctl, &mut out, 2, Instant::now());
        assert_eq!(out, [9.0; 4], "untouched (the caller zeroes the scratch buffer)");
        assert_eq!(ctl.audio_ring.state.lock().frames, 0);
    }

    #[test]
    fn fill_skips_audio_of_an_older_serial_and_reports_the_drained_end() {
        let ctl = running_control();
        let serial = ctl.serial.load(Ordering::Acquire);
        push(&ctl, serial - 1, 0.0, vec![1.0; 8]);
        push(&ctl, serial, 5.0, vec![0.25; 4]);
        ctl.audio_ring.state.lock().eof_serial = Some(serial);
        let mut out = [0.0f32; 8];
        fill(&ctl, &mut out, 2, Instant::now());
        assert_eq!(out, [0.25, 0.25, 0.25, 0.25, 0.0, 0.0, 0.0, 0.0]);
        assert!(ctl.audio_drained.load(Ordering::Acquire));
        // The audio ran out: the clock runs on the wall clock from here.
        assert!(!ctl.clock.is_audio_master());
    }

    #[test]
    fn fill_outputs_nothing_while_the_clock_is_held() {
        let ctl = running_control();
        let serial = ctl.serial.load(Ordering::Acquire);
        ctl.clock.jump(1.0, true);
        push(&ctl, serial, 1.0, vec![0.5; 4]);
        let mut out = [0.0f32; 4];
        fill(&ctl, &mut out, 2, Instant::now());
        assert_eq!(out, [0.0; 4]);
        assert_eq!(ctl.audio_ring.state.lock().frames, 2, "kept for when the first frame is on screen");
    }

    #[test]
    fn the_first_fill_after_a_start_skips_what_the_picture_has_already_passed() {
        let ctl = running_control();
        let serial = ctl.serial.load(Ordering::Acquire);
        // The clock runs from 0; the first samples are heard 10 ms (480 frames) after the callback.
        push(&ctl, serial, 0.0, (0..2000).map(|i| (i / 2) as f32).collect());
        let mut out = [0.0f32; 8];
        let heard = Instant::now() + Duration::from_millis(10);
        fill(&ctl, &mut out, 2, heard);
        let first = out[0] as usize;
        assert!((480..600).contains(&first), "starts near 10 ms: frame {first}");
        assert_eq!(out[2] as usize, first + 1, "contiguous from there");
        assert_eq!(ctl.audio_ring.state.lock().frames, 1000 - first - 4);
        // Synced now: the next callback continues without skipping.
        let mut out = [0.0f32; 2];
        fill(&ctl, &mut out, 2, Instant::now() + Duration::from_millis(30));
        assert_eq!(out[0] as usize, first + 4);
    }

    #[test]
    fn device_format_packs_into_one_word() {
        let f = DeviceFormat { rate: 48_000, channels: 6 };
        assert_eq!(DeviceFormat::unpack(f.pack()), Some(f));
        assert_eq!(DeviceFormat::unpack(0), None);
    }

    #[test]
    fn only_underruns_reroutes_and_priority_refusals_keep_the_stream() {
        use cpal::ErrorKind as K;
        for k in [K::Xrun, K::DeviceChanged, K::RealtimeDenied] {
            assert!(is_recoverable_in_place(k), "{k:?}");
        }
        for k in [K::DeviceNotAvailable, K::StreamInvalidated, K::BackendError, K::DeviceBusy, K::Other] {
            assert!(!is_recoverable_in_place(k), "{k:?}");
        }
    }

    #[test]
    fn fill_drops_audio_converted_for_another_device_format() {
        let ctl = running_control();
        let serial = ctl.serial.load(Ordering::Acquire);
        {
            let mut ring = ctl.audio_ring.state.lock();
            ring.frames += 2;
            // Six-channel audio from the device that is gone.
            ring.chunks.push_back(AudioChunk { serial, pts: 0.0, frame_dur: 1.0 / 48_000.0, samples: vec![1.0; 12], channels: 6, pos: 0 });
        }
        push(&ctl, serial, 0.5, vec![0.5; 4]);
        let mut out = [0.0f32; 4];
        fill(&ctl, &mut out, 2, Instant::now());
        assert_eq!(out, [0.5; 4]);
        assert_eq!(ctl.audio_ring.state.lock().frames, 0);
    }

    #[test]
    fn a_lost_device_falls_back_to_the_wall_clock_and_a_new_one_resumes_from_the_position() {
        let ctl = running_control();
        ctl.audio_active.store(true, Ordering::Release);
        {
            let mut i = ctl.info.lock();
            i.opened = true;
            i.has_video = false;
            i.duration = 10.0;
        }
        ctl.clock.audio_anchor(3.0, Instant::now(), 4.0);
        assert!(ctl.clock.is_audio_master());
        audio_lost(&ctl);
        assert!(ctl.audio_failed.load(Ordering::Acquire) && ctl.audio_lost.load(Ordering::Acquire));
        assert!(!ctl.clock.is_audio_master(), "wall clock while there is no device");
        std::thread::sleep(Duration::from_millis(30));
        assert!(ctl.clock.now() > 3.0, "video keeps running");
        // The output thread opened a new stream.
        ctl.audio_lost.store(false, Ordering::Release);
        let serial = ctl.serial.load(Ordering::Acquire);
        let at = ctl.position();
        audio_restored(&ctl);
        assert!(!ctl.audio_failed.load(Ordering::Acquire));
        assert_eq!(ctl.serial.load(Ordering::Acquire), serial + 1, "re-decodes from the current position");
        assert!(ctl.clock.is_audio_master());
        assert!((ctl.position() - at).abs() < 0.05, "{} vs {at}", ctl.position());
    }

    #[test]
    fn a_new_device_after_the_end_waits_for_the_next_start() {
        let ctl = running_control();
        ctl.audio_active.store(true, Ordering::Release);
        ctl.info.lock().opened = true;
        audio_lost(&ctl);
        ctl.ended.store(true, Ordering::Release);
        let serial = ctl.serial.load(Ordering::Acquire);
        audio_restored(&ctl);
        assert!(!ctl.audio_failed.load(Ordering::Acquire));
        assert_eq!(ctl.serial.load(Ordering::Acquire), serial, "no seek away from the end");
    }
}
