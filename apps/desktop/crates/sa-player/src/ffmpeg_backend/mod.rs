//! FFmpeg + cpal backend (feature `ffmpeg`).
//!
//! Threads per loaded source: demux, video decode (hardware or software, see [`hw`]; then the YUV planes for the GPU
//! or swscale to RGBA), presenter, audio decode (+ swresample) and the audio output (cpal callback fed by a ring of
//! resampled samples); see [`session`]. The UI-side frame handoff is "latest frame wins" with
//! `PlayerEvent::FrameReady` coalesced (at most one pending until `latest_frame`).
//!
//! A/V sync: the audio clock (the sample being heard, from the device's playback timestamp) is the master; without
//! an audio stream or device a monotonic wall clock is used. The clock snaps to the device after every start or seek
//! and then only slews toward it, so per-callback timestamp wobble does not show up as frame jitter. Frames due within
//! 20 ms are waited for with a high-resolution sleep (condvar timeouts follow the 15.6 ms Windows tick). Video
//! frames are shown when their pts is due; late frames are dropped (before conversion when hopelessly late). Seeking
//! flushes queues and decoders, seeks to the previous keyframe and decodes forward to the exact target; the clock
//! stands at the target until its frame is on screen.
//!
//! Like Media Foundation and AVPlayer, the player follows the default output device: a lost or invalidated stream
//! hands the clock to the wall clock, a new stream is opened on the current default device and audio resumes from
//! the current position. A source that stops being readable mid-playback posts `Failed`, not `Ended`.

mod audio;
mod hw;
mod io;
pub mod perf;
mod pictures;
mod priority;
mod session;
#[cfg(windows)]
mod staging;
pub mod subtitles;
mod sync;
mod tracks;
mod video;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{FailureKind, LoadGeneration, MediaPlayerBackend, MediaSource, MediaTracks, NativeStackInfo, PlayerEvent, VideoFrame};
use session::{Control, PlayerShared};

#[doc(hidden)]
pub use pictures::{DecodedPictureEvent, decode_picture_stream};
pub use subtitles::{
    ExtractError, ExtractedSubtitle, StreamOptions, extract_text_subtitles, extract_text_subtitles_with, snapshot_interval, stream_text_subtitles,
};

/// One block a virtual output device ([`AudioTap`]) played: interleaved samples (volume applied) and the media time
/// of the first one.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct TapBlock {
    pub pts: f64,
    pub samples: Vec<f32>,
}

/// Tests: a virtual output device in a fixed format that plays in real time and records what it plays
/// ([`FfmpegPlayer::use_audio_tap`]). The audio clock is master with it exactly as with a real device.
#[doc(hidden)]
#[derive(Clone)]
pub struct AudioTap {
    rate: u32,
    channels: u16,
    blocks: Arc<parking_lot::Mutex<Vec<TapBlock>>>,
}

impl AudioTap {
    pub fn new(rate: u32, channels: u16) -> Self {
        Self { rate, channels, blocks: Arc::default() }
    }
    pub fn rate(&self) -> u32 {
        self.rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    /// Everything played so far.
    pub fn blocks(&self) -> Vec<TapBlock> {
        self.blocks.lock().clone()
    }
    pub fn clear(&self) {
        self.blocks.lock().clear();
    }
    pub(crate) fn record(&self, pts: f64, samples: &[f32]) {
        self.blocks.lock().push(TapBlock { pts, samples: samples.to_vec() });
    }
}

/// Buffered-stream progress for the Windows buffer bar: `playback_ratio → buffered_ratio`
/// (`BufferedVideoStream::buffered_ratio`).
pub type BufferProbe = Arc<dyn Fn(f64) -> f64 + Send + Sync>;

/// Construction options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerOptions {
    /// Play audio through the default output device. Off: audio is ignored and the wall clock is master (tests,
    /// machines without an output device behave the same way automatically).
    pub audio_output: bool,
    /// Decode video in hardware (D3D11VA / VideoToolbox) when the stream allows it, with a software fallback.
    /// `SUBTITLE_ADDER_HWDEC=0` turns it off process-wide.
    pub hardware_decoding: bool,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        Self { audio_output: true, hardware_decoding: true }
    }
}

/// Counters of the current source (reset on every load).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaybackStats {
    /// Frames out of the decoder (including frames decoded only to reach a seek target).
    pub decoded: u64,
    /// Frames handed to the UI.
    pub presented: u64,
    /// Frames converted but skipped because the next one was already due.
    pub dropped_late: u64,
    /// Frames so late they were skipped before conversion.
    pub dropped_decoder: u64,
    /// Frames the hardware decoder produced.
    pub hardware: u64,
    /// Frames handed over as YUV planes.
    pub planar: u64,
    /// RGBA pictures tone-mapped (HDR / wide gamut) on the CPU (`crate::soft`; fields count one each).
    pub soft_tonemapped: u64,
    /// RGBA fields deinterlaced on the CPU.
    pub soft_deinterlaced: u64,
    /// RGBA second fields dropped as late before their conversion (fields, not frames: not in [`Self::dropped`]).
    pub dropped_fields: u64,
    /// Bitmap subtitle events decoded (clearing events included) on the shown track(s).
    pub picture_events: u64,
    /// Bitmap subtitle pictures put on screen.
    pub pictures_shown: u64,
    /// Bitmap subtitle packets the decoder rejected, or events dropped as too large, on the current track.
    pub picture_failures: u64,
    /// Bitmap subtitle events ahead of the position lost because the queue was full.
    pub picture_overflow: u64,
    /// Bitmap subtitle pictures kept on screen across a restart that keeps the position although the queue no longer
    /// knew the screen there (`PictureTimeline::seek` in place, the carry-over).
    pub pictures_carried: u64,
    /// Encoded XSUB / VobSub packets turned away because the track's packet store was full.
    pub picture_store_overflow: u64,
}

impl PlaybackStats {
    pub fn dropped(&self) -> u64 {
        self.dropped_late + self.dropped_decoder
    }
}

static NEXT_SOURCE_ID: AtomicU64 = AtomicU64::new(1);

/// The cross-platform player.
pub struct FfmpegPlayer {
    shared: Arc<PlayerShared>,
    session: Option<Arc<Control>>,
    options: PlayerOptions,
    rate: f64,
    probe: Option<BufferProbe>,
    /// A load that failed before a session existed (the demux thread could not be spawned).
    load_failure: Option<FailureKind>,
}

impl FfmpegPlayer {
    pub fn new() -> Self {
        Self::with_options(PlayerOptions::default())
    }

    pub fn with_options(options: PlayerOptions) -> Self {
        let _ = ffmpeg_next::init();
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
        let shared = Arc::new(PlayerShared::new(options.hardware_decoding));
        Self { shared, session: None, options, rate: 1.0, probe: None, load_failure: None }
    }

    /// `load` plus the buffered-stream probe that drives `buffered_ratio` (Windows VideoCache playback).
    pub fn load_with_probe(&mut self, source: Option<MediaSource>, auto_play: bool, probe: Option<BufferProbe>) -> LoadGeneration {
        let generation = self.shared.gate.begin_load();
        self.stop_session();
        self.shared.gate.clear_active_source();
        self.probe = None;
        self.load_failure = None;
        let Some(source) = source else { return generation };
        if self.options.hardware_decoding {
            // The first load creates the GPU decoding device while the demuxer opens the source (not at startup:
            // the driver costs ~80 MB that an app showing no video should not pay).
            hw::warm_up();
        }
        let source_id = NEXT_SOURCE_ID.fetch_add(1, Ordering::Relaxed);
        if !self.shared.gate.try_activate_source(generation, Some(source_id)) {
            return generation;
        }
        let ctl = Control::new(self.shared.clone(), generation, source_id, auto_play, self.rate);
        match session::spawn(ctl.clone(), source, self.options.audio_output) {
            Ok(_detached) => {
                self.probe = probe;
                self.session = Some(ctl);
            }
            Err(e) => {
                self.load_failure = Some(FailureKind::Other);
                let sink = self.shared.sink.read().clone();
                if let Some(sink) = sink {
                    sink(PlayerEvent::Failed { generation, message: e.to_string() });
                }
            }
        }
        generation
    }

    /// Playback speed (0.25…4; 1 = normal). Audio is resampled, so its pitch follows the speed. Kept across loads.
    pub fn set_rate(&mut self, rate: f64) {
        let rate = if rate.is_finite() { rate.clamp(0.25, 4.0) } else { 1.0 };
        self.rate = rate;
        if let Some(s) = &self.session {
            s.set_speed(rate);
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn volume(&self) -> f64 {
        f64::from_bits(self.shared.volume.load(Ordering::Relaxed))
    }

    pub fn is_muted(&self) -> bool {
        self.shared.muted.load(Ordering::Relaxed)
    }

    /// Whether the current source opened (Opened was posted).
    pub fn is_opened(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.info.lock().opened)
    }

    /// Whether playback reached the end of the current source.
    pub fn is_ended(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.ended.load(Ordering::Acquire))
    }

    /// Whether the current source plays its audio through a device (the audio clock is master).
    pub fn has_audio_output(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.info.lock().has_audio_output)
    }

    /// Tests (debug builds only): `gate` runs inside every seek of the current source between moving the bitmap
    /// subtitle timeline to the seek's serial and moving the clock (`None`: no gate), so a test can let the presenter
    /// run in that window.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn set_seek_gate(&self, gate: Option<Box<dyn Fn() + Send + Sync>>) {
        if let Some(s) = &self.session {
            *s.seek_gate.lock() = gate;
        }
    }

    /// Counters of the current source.
    pub fn stats(&self) -> PlaybackStats {
        let Some(s) = &self.session else { return PlaybackStats::default() };
        PlaybackStats {
            decoded: s.stats.decoded.load(Ordering::Relaxed),
            presented: s.stats.presented.load(Ordering::Relaxed),
            dropped_late: s.stats.dropped_late.load(Ordering::Relaxed),
            dropped_decoder: s.stats.dropped_decoder.load(Ordering::Relaxed),
            hardware: s.stats.hardware.load(Ordering::Relaxed),
            planar: s.stats.planar.load(Ordering::Relaxed),
            soft_tonemapped: s.stats.soft_tonemapped.load(Ordering::Relaxed),
            soft_deinterlaced: s.stats.soft_deinterlaced.load(Ordering::Relaxed),
            dropped_fields: s.stats.dropped_fields.load(Ordering::Relaxed),
            picture_events: s.stats.picture_events.load(Ordering::Relaxed),
            pictures_shown: s.stats.pictures_shown.load(Ordering::Relaxed),
            picture_failures: s.stats.picture_failures.load(Ordering::Relaxed),
            picture_overflow: s.stats.picture_overflow.load(Ordering::Relaxed),
            pictures_carried: s.stats.pictures_carried.load(Ordering::Relaxed),
            picture_store_overflow: s.stats.picture_store_overflow.load(Ordering::Relaxed),
        }
    }

    /// Tests: from the next load on, every hardware decoder of this player fails as on a GPU that cannot decode the
    /// codec (each packet errors with ENOSYS), so the software fall-back runs. Process-wide:
    /// `SUBTITLE_ADDER_HWDEC_FAIL=1`. Simulated failures are not remembered (the `hw` module docs).
    #[doc(hidden)]
    pub fn simulate_hardware_failure(&mut self, on: bool) {
        self.shared.simulate_hw_failure.store(on, Ordering::Relaxed);
    }

    /// Tests: from the next load on, the decoder of audio stream `stream_index` does not open (`None`: every decoder
    /// opens), so the fall-back of a failed track switch runs.
    #[doc(hidden)]
    pub fn simulate_audio_decoder_failure(&mut self, stream_index: Option<usize>) {
        *self.shared.simulate_audio_failure.lock() = stream_index;
    }

    /// Tests: from the next load on, audio plays into `tap` (a virtual device) instead of the default output device,
    /// whatever `PlayerOptions::audio_output` says. `None` returns to the options.
    #[doc(hidden)]
    pub fn use_audio_tap(&mut self, tap: Option<AudioTap>) {
        *self.shared.audio_tap.lock() = tap;
    }

    fn stop_session(&mut self) {
        if let Some(s) = self.session.take() {
            // The threads exit on their own; the demux thread joins the others and frees FFmpeg state.
            s.shutdown();
        }
    }
}

impl Default for FfmpegPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FfmpegPlayer {
    fn drop(&mut self) {
        self.dispose();
    }
}

impl MediaPlayerBackend for FfmpegPlayer {
    fn load(&mut self, source: Option<MediaSource>, auto_play: bool) -> LoadGeneration {
        self.load_with_probe(source, auto_play, None)
    }

    fn play(&mut self) {
        if let Some(s) = &self.session {
            s.set_playing(true);
        }
    }

    fn pause(&mut self) {
        if let Some(s) = &self.session {
            s.set_playing(false);
        }
    }

    fn seek(&mut self, seconds: f64) {
        if let Some(s) = &self.session {
            s.request_seek(seconds);
        }
    }

    fn set_volume(&mut self, volume: f64) {
        // Stored as f64 (MediaPlayer.Volume is a double), so the value reads back exactly. A non-finite request keeps
        // the current volume instead of jumping to full loudness.
        if !volume.is_finite() {
            return;
        }
        self.shared.volume.store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    fn set_muted(&mut self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }

    fn position(&self) -> f64 {
        self.session.as_ref().map_or(0.0, |s| s.position())
    }

    fn duration(&self) -> f64 {
        self.session.as_ref().map_or(0.0, |s| s.info.lock().duration)
    }

    fn is_playing(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.playing.load(Ordering::Acquire))
    }

    fn natural_size(&self) -> Option<(u32, u32)> {
        self.session.as_ref().and_then(|s| s.info.lock().natural)
    }

    fn buffered_ratio(&self) -> f64 {
        let (Some(probe), Some(_)) = (&self.probe, &self.session) else { return 0.0 };
        let d = self.duration();
        if d <= 0.0 {
            return 0.0;
        }
        let r = probe((self.position() / d).clamp(0.0, 1.0));
        if r.is_finite() { r.clamp(0.0, 1.0) } else { 0.0 }
    }

    fn latest_frame(&mut self) -> Option<VideoFrame> {
        self.session.as_ref().and_then(|s| s.take_latest())
    }

    fn set_event_sink(&mut self, sink: Box<dyn Fn(PlayerEvent) + Send + Sync>) {
        *self.shared.sink.write() = Some(Arc::from(sink));
    }

    fn failure_kind(&self) -> Option<FailureKind> {
        self.session.as_ref().and_then(|s| s.info.lock().failure).or(self.load_failure)
    }

    fn set_planar_output(&mut self, planar: bool) {
        self.shared.planar.store(planar, Ordering::Relaxed);
    }

    fn diagnostics(&self) -> Option<String> {
        self.session.as_ref()?;
        let s = self.stats();
        // The bitmap subtitle track shown (`-` none), its events decoded, pictures shown, failures, and the size of
        // the picture on screen now (bounding box on its canvas; 0x0 when none) with the canvas.
        let image = self.tracks().active_image_subtitle.map_or_else(|| "-".to_owned(), |i| i.to_string());
        let (size, canvas) = self
            .subtitle_picture()
            .map(|p| {
                let (_, _, w, h) = p.bounds();
                (format!("{w}x{h}"), format!("{}x{}", p.canvas.0, p.canvas.1))
            })
            .unwrap_or_else(|| ("0x0".to_owned(), "-".to_owned()));
        Some(format!(
            "decoded={} hardware={} planar={} presented={} dropped_late={} dropped_decoder={} dropped_fields={} soft_tonemapped={} soft_deinterlaced={} \
             image_subtitle={image} picture_events={} pictures_shown={} picture_failures={} picture_overflow={} picture_store_overflow={} picture_size={size} picture_canvas={canvas}",
            s.decoded,
            s.hardware,
            s.planar,
            s.presented,
            s.dropped_late,
            s.dropped_decoder,
            s.dropped_fields,
            s.soft_tonemapped,
            s.soft_deinterlaced,
            s.picture_events,
            s.pictures_shown,
            s.picture_failures,
            s.picture_overflow,
            s.picture_store_overflow
        ))
    }

    fn tracks(&self) -> MediaTracks {
        self.session.as_ref().map(|s| s.tracks.lock().clone()).unwrap_or_default()
    }

    fn select_audio_track(&mut self, stream_index: usize) {
        if let Some(s) = &self.session {
            s.request_audio_track(stream_index);
        }
    }

    fn select_image_subtitle_track(&mut self, stream_index: Option<usize>) {
        if let Some(s) = &self.session {
            s.request_image_subtitle(stream_index);
        }
    }

    fn subtitle_picture(&self) -> Option<Arc<crate::SubtitlePicture>> {
        self.session.as_ref().and_then(|s| s.pictures.lock().shown.clone())
    }

    fn dispose(&mut self) {
        self.shared.gate.begin_load();
        self.shared.gate.clear_active_source();
        self.stop_session();
        self.probe = None;
    }
}

/// The UI uploaded the planes of a frame read in place from a staging texture ([`crate::PlanarImage::is_staged`]):
/// `bytes` in `wall`. Feeds the staging ring's read-speed guard (`staging.rs`); nothing on other platforms.
pub fn report_staged_upload(wall: std::time::Duration, bytes: usize) {
    #[cfg(windows)]
    staging::report_upload(wall.as_nanos() as u64, bytes as u64);
    #[cfg(not(windows))]
    let _ = (wall, bytes);
}

/// Initializes FFmpeg and queries the audio host (proves the native stack links and runs).
pub fn probe() -> NativeStackInfo {
    use cpal::traits::{DeviceTrait, HostTrait};
    let _ = ffmpeg_next::init();
    let v = ffmpeg_next::format::version();
    let host = cpal::default_host();
    let device = host.default_output_device().and_then(|d| d.description().ok().map(|desc| desc.name().to_owned()));
    NativeStackInfo {
        ffmpeg_avformat_version: format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff),
        ffmpeg_license: ffmpeg_next::codec::license().to_owned(),
        audio_host: format!("{:?}", host.id()),
        audio_output_device: device,
    }
}
