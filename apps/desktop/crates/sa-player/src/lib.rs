//! `sa-player`: the video player of both front ends (ARCHITECTURE.md §7).
//!
//! * Pure, always compiled: [`MediaPlayerBackend`] contract, [`VideoFrame`] (RGBA8 buffers handed to the UI),
//!   [`PlaybackTimeline`] and [`PlayerSessionGate`] (win-media §4; shared by both platforms).
//! * Feature `ffmpeg` (default): [`ffmpeg_backend::FfmpegPlayer`] — demux/decode with `ffmpeg-next` 8.1, video
//!   converted to RGBA by swscale, audio resampled by swresample to the output format and played through `cpal`
//!   (WASAPI / CoreAudio); the audio clock is the master clock (video frames are shown when their pts is due, late
//!   frames dropped). View models depend on this crate **without** the feature.
//!
//! This replaces both IMFMediaEngine (Windows) and AVPlayer (macOS) with one decoder. Windows feeds the stream through
//! `sa_media::win::BufferedVideoStream` (custom AVIO context), so the encrypted SSD chunk cache keeps working.

use std::io::{Read, Seek};
use std::path::PathBuf;
use std::sync::Arc;

pub mod gate;
pub mod timeline;

#[cfg(feature = "ffmpeg")]
pub mod ffmpeg_backend;

pub use gate::{PlayerEventToken, PlayerSessionGate, SourceId};
pub use timeline::PlaybackTimeline;

/// A decoded video frame, RGBA8, tightly packed (`stride == width * 4`) unless stated.
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// Presentation time (s).
    pub pts: f64,
    pub rgba: Arc<Vec<u8>>,
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame").field("width", &self.width).field("height", &self.height).field("pts", &self.pts).finish()
    }
}

/// A `Read + Seek` source (Windows `BufferedVideoStream`).
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// What to play.
pub enum MediaSource {
    File(PathBuf),
    /// Custom AVIO over a reader; `path` is kept for diagnostics and format probing by extension.
    Reader { path: PathBuf, open: Box<dyn FnOnce() -> std::io::Result<Box<dyn ReadSeek>> + Send> },
}

/// Monotonic load counter value (`PlayerSessionGate::begin_load`).
pub type LoadGeneration = i32;

/// Events posted by a backend (gated by generation before they reach the UI).
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    Opened { generation: LoadGeneration, duration: f64, natural_size: Option<(u32, u32)> },
    /// The source could not be opened, or stopped being readable while playing (then instead of `Ended`). Playback
    /// stops; `play()` does nothing until the next load.
    Failed { generation: LoadGeneration, message: String },
    SizeChanged { generation: LoadGeneration, width: u32, height: u32 },
    /// A new frame can be fetched with `latest_frame` (coalesced: at most one pending).
    FrameReady { generation: LoadGeneration },
    Ended { generation: LoadGeneration },
    /// Reports only real changes (alternating). An auto-play load reports `playing: true` right after `Opened`; a
    /// failure or the end reports `playing: false`.
    PlayingChanged { generation: LoadGeneration, playing: bool },
}

/// Why the current load failed (`MediaPlayerBackend::failure_kind`). The macOS player model uses it for its
/// engine `preview` fallback decision (mac-viewmodels §7.1 `can_try_preview`: only unsupported formats are retried).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// Unknown container, invalid data, no stream, or no decoder for a track: converting may help.
    UnsupportedFormat,
    /// The source could not be read (missing file, no permission, I/O or cache stream error).
    Io,
    Other,
}

/// The player contract (UI-thread owned; decoding, audio and teardown run on their own threads).
pub trait MediaPlayerBackend {
    /// `None` empties the player.
    fn load(&mut self, source: Option<MediaSource>, auto_play: bool) -> LoadGeneration;
    fn play(&mut self);
    fn pause(&mut self);
    fn seek(&mut self, seconds: f64);
    /// 0…1 (clamped; non-finite is ignored). Volume and mute belong to the player and survive loads.
    fn set_volume(&mut self, volume: f64);
    fn set_muted(&mut self, muted: bool);
    fn position(&self) -> f64;
    fn duration(&self) -> f64;
    fn is_playing(&self) -> bool;
    fn natural_size(&self) -> Option<(u32, u32)>;
    /// Windows buffer bar (from the buffered stream); 0 when unknown.
    fn buffered_ratio(&self) -> f64;
    /// The newest decoded frame not yet taken.
    fn latest_frame(&mut self) -> Option<VideoFrame>;
    /// `Send` sink; the app re-posts to the Slint event loop.
    fn set_event_sink(&mut self, sink: Box<dyn Fn(PlayerEvent) + Send + Sync>);
    /// Stops threads; teardown off the UI thread.
    fn dispose(&mut self);
    /// Why the current load failed (after `PlayerEvent::Failed`); `None` while it did not fail.
    fn failure_kind(&self) -> Option<FailureKind> {
        None
    }
}

/// What the native stack reports (startup diagnostics and the build proof).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeStackInfo {
    pub ffmpeg_avformat_version: String,
    /// `avcodec_license()` of the libraries this process loaded: `LGPL version 3 or later` for the build the app
    /// ships (the BtbN LGPL build is configured with `--enable-version3`, so the LGPLv3 terms apply, not 2.1's);
    /// the engine keeps its own GPL build (`GPL version 3 or later`) in its own folder.
    pub ffmpeg_license: String,
    pub audio_host: String,
    pub audio_output_device: Option<String>,
}
