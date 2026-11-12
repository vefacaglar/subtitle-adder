//! The player controller of a main window (win-ui-main §10 `PlayerController`, mac-viewmodels §7.1 `PlayerModel`):
//! wraps a [`MediaPlayerBackend`], keeps the [`PlaybackTimeline`] (seek settling), the active line, auto-play,
//! volume/mute and playback failures, and reports watch progress events for the platform hub.
//!
//! UI-thread owned and free of threads: backend events are posted to the UI thread by the caller and fed into
//! [`PlayerController::handle_event`]; everything the view must react to is queued as [`PlayerOutput`] and drained
//! with [`PlayerController::take_outputs`]. The clock is injected so tests drive time explicitly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use sa_core::engine_types::Sentence;
use sa_player::{
    FailureKind, LoadGeneration, MediaPlayerBackend, MediaSource, MediaTracks, PictureTrackFailure, PlaybackTimeline, PlayerEvent, SubtitlePicture,
    VideoFrame,
};
use sa_viewmodels::hub::PlaybackReport;
use sa_viewmodels::shortcuts::adjusted_volume;
use uuid::Uuid;

/// Which app's player semantics apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerFlavor {
    /// win-ui-main §10: `is_playing` follows the backend on every tick; play-empty; failures go to the error bar.
    Windows,
    /// mac-viewmodels §7.1: `is_playing` follows the backend's playing-state events; failures are kept as
    /// `PlaybackFailure` until dismissed; an unsupported format tries the engine preview once.
    Mac,
}

/// macOS `PlaybackFailure`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackFailure {
    NotPlayable,
    Unknown,
    System(String),
}

impl PlaybackFailure {
    pub fn id(&self) -> String {
        match self {
            PlaybackFailure::NotPlayable => "not-playable".into(),
            PlaybackFailure::Unknown => "unknown".into(),
            PlaybackFailure::System(t) => format!("system:{t}"),
        }
    }
    pub fn text(&self) -> String {
        match self {
            PlaybackFailure::NotPlayable => sa_core::l("Bu biçim oynatılamıyor."),
            PlaybackFailure::Unknown => sa_core::l("Bilinmeyen hata"),
            PlaybackFailure::System(t) => t.clone(),
        }
    }
    /// `cause(error)`: none → not playable; blank → unknown; else the text.
    pub fn cause(message: Option<&str>) -> Self {
        match message {
            None => PlaybackFailure::NotPlayable,
            Some(m) if m.trim().is_empty() => PlaybackFailure::Unknown,
            Some(m) => PlaybackFailure::System(m.trim().to_owned()),
        }
    }
    /// Id `playback-failed:<id>`, text `Video oynatılamadı: %@` (relocalizes with the UI language).
    pub fn message(&self) -> sa_core::l10n::LocalizedMessage {
        let this = self.clone();
        sa_core::l10n::LocalizedMessage::new(format!("playback-failed:{}", self.id()), move || {
            sa_core::l!("Video oynatılamadı: %@", this.text())
        })
    }
}

/// What the view must react to.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerOutput {
    /// Time / playing / volume changed: re-sync the transport and the split button.
    Ticked,
    ActiveChanged(Option<Uuid>),
    VideoSizeChanged(f64, f64),
    /// A new frame can be taken with [`PlayerController::latest_frame`].
    FrameReady,
    /// Windows: show `Video oynatılamadı: %@` with these details.
    Failed(String),
    /// macOS: the failure state changed (`failure()`).
    FailureChanged,
    /// Windows: toggle without a loaded path (§10.6).
    PlayEmptyRequested,
    /// Watch progress for the platform hub (privacy-gated there).
    Playback(PlaybackReport),
    /// macOS: convert `source` with the engine `preview` command and call [`PlayerController::preview_ready`].
    PreparePreview { source: PathBuf, generation: u64 },
    PreparingPreviewChanged(bool),
    /// The audio / subtitle tracks of the media (or the playing audio track) changed: [`PlayerController::tracks`].
    TracksChanged,
    /// The audio track asked for could not be played (the previous one keeps playing); FFmpeg's details.
    AudioTrackFailed(String),
    /// Image subtitle track `.0` cannot be shown: one asked for that the player refused (the previous choice stays),
    /// or the one shown, given up after its decoder rejected packet after packet (it is off now; `TracksChanged`
    /// follows).
    SubtitleTrackFailed(usize, PictureTrackFailure),
    /// The bitmap subtitle picture to draw over the video changed: [`PlayerController::subtitle_picture`].
    PictureChanged,
}

/// Opens the media for the backend (Windows wraps the file in a `BufferedVideoStream`).
pub type SourceFactory = Box<dyn Fn(&Path) -> MediaSource>;

pub struct PlayerController {
    flavor: PlayerFlavor,
    backend: Box<dyn MediaPlayerBackend>,
    source_factory: SourceFactory,
    clock: Box<dyn Fn() -> Instant>,
    backend_generation: LoadGeneration,
    /// Own load counter (preview restarts bump it too).
    load_generation: u64,
    current_path: Option<PathBuf>,
    has_media: bool,
    timeline: PlaybackTimeline,
    current_time: f64,
    seek_when_ready: Option<f64>,
    auto_play_when_ready: bool,
    is_playing: bool,
    reported_playing: bool,
    volume: f64,
    muted: bool,
    video_size: (f64, f64),
    sentences: Vec<Sentence>,
    active: Option<Uuid>,
    failure: Option<PlaybackFailure>,
    failure_reported: bool,
    preparing_preview: bool,
    preview_tried: bool,
    pending_preview_position: Option<f64>,
    preview_file: Option<PathBuf>,
    disposed: bool,
    last_progress_position: f64,
    tracks: MediaTracks,
    outputs: Vec<PlayerOutput>,
}

impl PlayerController {
    pub fn new(flavor: PlayerFlavor, backend: Box<dyn MediaPlayerBackend>) -> Self {
        Self::with_parts(flavor, backend, Box::new(|p: &Path| MediaSource::File(p.to_path_buf())), Box::new(Instant::now))
    }

    pub fn with_parts(flavor: PlayerFlavor, backend: Box<dyn MediaPlayerBackend>, source_factory: SourceFactory, clock: Box<dyn Fn() -> Instant>) -> Self {
        Self {
            flavor,
            backend,
            source_factory,
            clock,
            backend_generation: 0,
            load_generation: 0,
            current_path: None,
            has_media: false,
            timeline: PlaybackTimeline::new(),
            current_time: 0.0,
            seek_when_ready: None,
            auto_play_when_ready: false,
            is_playing: false,
            reported_playing: false,
            volume: 1.0,
            muted: false,
            video_size: (16.0, 9.0),
            sentences: Vec::new(),
            active: None,
            failure: None,
            failure_reported: false,
            preparing_preview: false,
            preview_tried: false,
            pending_preview_position: None,
            preview_file: None,
            disposed: false,
            last_progress_position: -1.0,
            tracks: MediaTracks::default(),
            outputs: Vec::new(),
        }
    }

    /// Lets the backend post its events (the caller re-posts them to the UI thread and calls `handle_event`).
    pub fn set_event_sink(&mut self, sink: Box<dyn Fn(PlayerEvent) + Send + Sync>) {
        self.backend.set_event_sink(sink);
    }

    pub fn take_outputs(&mut self) -> Vec<PlayerOutput> {
        std::mem::take(&mut self.outputs)
    }

    fn emit(&mut self, o: PlayerOutput) {
        self.outputs.push(o);
    }

    // ---- state
    pub fn current_time(&self) -> f64 {
        self.current_time
    }
    pub fn duration(&self) -> f64 {
        self.timeline.duration
    }
    pub fn is_playing(&self) -> bool {
        self.is_playing
    }
    pub fn is_muted(&self) -> bool {
        self.muted
    }
    pub fn volume(&self) -> f64 {
        self.volume
    }
    pub fn has_media(&self) -> bool {
        self.has_media
    }
    pub fn has_item(&self) -> bool {
        self.current_path.is_some()
    }
    pub fn current_path(&self) -> Option<&Path> {
        self.current_path.as_deref()
    }
    pub fn video_size(&self) -> (f64, f64) {
        self.video_size
    }
    pub fn active_id(&self) -> Option<Uuid> {
        self.active
    }
    pub fn failure(&self) -> Option<&PlaybackFailure> {
        self.failure.as_ref()
    }
    pub fn failure_message(&self) -> Option<sa_core::l10n::LocalizedMessage> {
        self.failure.as_ref().map(PlaybackFailure::message)
    }
    pub fn is_preparing_preview(&self) -> bool {
        self.preparing_preview
    }
    pub fn auto_play_when_ready(&self) -> bool {
        self.auto_play_when_ready
    }
    pub fn buffered_ratio(&self) -> f64 {
        if self.has_media && self.timeline.duration > 0.0 { self.backend.buffered_ratio() } else { 0.0 }
    }
    pub fn latest_frame(&mut self) -> Option<VideoFrame> {
        self.backend.latest_frame()
    }
    /// Planar (GPU-converted) frames instead of RGBA (`MediaPlayerBackend::set_planar_output`).
    pub fn set_planar_output(&mut self, planar: bool) {
        self.backend.set_planar_output(planar);
    }
    pub fn diagnostics(&self) -> Option<String> {
        self.backend.diagnostics()
    }
    /// The backend's own clock right now (diagnostics; `current_time` is the last tick's).
    pub fn backend_position(&self) -> f64 {
        self.backend.position()
    }
    /// The engine preview copy in use (macOS; deleted by the owner when replaced).
    pub fn preview_file(&self) -> Option<&Path> {
        self.preview_file.as_deref()
    }
    /// The audio / subtitle tracks of the current media (empty until it opened).
    pub fn tracks(&self) -> &MediaTracks {
        &self.tracks
    }
    /// The file the backend plays (the engine preview copy on macOS when one is in use): the file whose stream
    /// indices [`Self::tracks`] lists.
    pub fn playing_file(&self) -> Option<&Path> {
        self.preview_file.as_deref().or(self.current_path.as_deref())
    }
    /// Plays audio track `stream_index` from the current position (answered with `TracksChanged`, or
    /// `AudioTrackFailed`). The backend decides whether the stream is already active: the list here can be behind a
    /// switch still in flight, so A → B → A in quick succession must reach it.
    pub fn select_audio_track(&mut self, stream_index: usize) {
        if self.disposed || !self.has_media {
            return;
        }
        self.backend.select_audio_track(stream_index);
    }
    /// Shows image subtitle track `stream_index` over the video (`None`: none); answered with `TracksChanged`, or
    /// `SubtitleTrackFailed`. As for audio, the backend decides what is already active.
    pub fn select_image_subtitle_track(&mut self, stream_index: Option<usize>) {
        if self.disposed || !self.has_media {
            return;
        }
        self.backend.select_image_subtitle_track(stream_index);
    }
    /// The bitmap subtitle picture to draw over the video now (none without media).
    pub fn subtitle_picture(&self) -> Option<Arc<SubtitlePicture>> {
        if self.disposed || self.current_path.is_none() { None } else { self.backend.subtitle_picture() }
    }
    fn set_tracks(&mut self, tracks: MediaTracks) {
        if tracks != self.tracks {
            self.tracks = tracks;
            self.emit(PlayerOutput::TracksChanged);
        }
    }

    fn now(&self) -> Instant {
        (self.clock)()
    }

    fn position_report(&self) -> f64 {
        self.current_time
    }

    // ---- loading
    /// `load(path, auto_play)` (win-ui-main §10.2, mac-viewmodels §7.1).
    pub fn load(&mut self, path: Option<&Path>, auto_play: bool) {
        if self.disposed {
            return;
        }
        if self.current_path.is_some() {
            let position = self.position_report();
            self.emit(PlayerOutput::Playback(PlaybackReport::Leaving { position }));
        }
        self.load_generation += 1;
        self.reported_playing = false;
        self.current_path = path.map(Path::to_path_buf);
        self.has_media = false;
        self.timeline.reset();
        self.current_time = 0.0;
        self.seek_when_ready = None;
        self.pending_preview_position = None;
        self.preview_tried = false;
        if self.preparing_preview {
            self.preparing_preview = false;
            self.emit(PlayerOutput::PreparingPreviewChanged(false));
        }
        self.preview_file = None;
        let had_failure = self.failure.take().is_some();
        self.failure_reported = false;
        if had_failure {
            self.emit(PlayerOutput::FailureChanged);
        }
        self.auto_play_when_ready = path.is_some() && auto_play;
        self.is_playing = match self.flavor {
            PlayerFlavor::Windows => self.auto_play_when_ready,
            PlayerFlavor::Mac => false,
        };
        self.last_progress_position = -1.0;
        self.emit(PlayerOutput::Ticked);
        self.video_size = (16.0, 9.0);
        self.emit(PlayerOutput::VideoSizeChanged(16.0, 9.0));
        self.set_tracks(MediaTracks::default());
        self.emit(PlayerOutput::PictureChanged);
        self.update_active();
        let source = path.map(|p| (self.source_factory)(p));
        self.backend.pause();
        self.backend_generation = self.backend.load(source, false);
        self.backend.set_volume(self.volume);
        self.backend.set_muted(self.muted);
    }

    fn install_file(&mut self, file: &Path) {
        self.has_media = false;
        self.set_tracks(MediaTracks::default());
        self.backend_generation = self.backend.load(Some(MediaSource::File(file.to_path_buf())), false);
        self.backend.set_volume(self.volume);
        self.backend.set_muted(self.muted);
    }

    /// The engine preview finished (macOS); stale generations are ignored.
    pub fn preview_ready(&mut self, generation: u64, result: Result<PathBuf, String>) {
        if self.disposed || generation != self.load_generation || !self.preparing_preview {
            return;
        }
        self.preparing_preview = false;
        self.emit(PlayerOutput::PreparingPreviewChanged(false));
        match result {
            Ok(file) => {
                self.install_file(&file);
                self.preview_file = Some(file);
            }
            Err(e) => self.report_failure(Some(&e)),
        }
    }

    // ---- backend events
    pub fn handle_event(&mut self, event: PlayerEvent) {
        if self.disposed {
            return;
        }
        match event {
            PlayerEvent::Opened { generation, duration, natural_size } => {
                if generation != self.backend_generation || self.current_path.is_none() {
                    return;
                }
                // `itemReady`: `guard failure == nil` (a dismissed notice does not keep a ready item from playing).
                if self.flavor == PlayerFlavor::Mac && self.failure.is_some() {
                    return;
                }
                self.has_media = true;
                let now = self.now();
                self.timeline.observe(self.timeline.position, duration, now);
                if let Some((w, h)) = natural_size {
                    self.set_video_size(w, h);
                }
                self.emit(PlayerOutput::Playback(PlaybackReport::Opened { duration: self.timeline.duration }));
                if let Some(p) = self.pending_preview_position.take() {
                    if p > 0.0 {
                        self.seek_when_ready = Some(p);
                    }
                }
                if let Some(target) = self.seek_when_ready.take() {
                    self.seek(target);
                }
                if self.auto_play_when_ready {
                    self.auto_play_when_ready = false;
                    self.backend.play();
                    if self.flavor == PlayerFlavor::Windows {
                        self.is_playing = true;
                    }
                }
                self.tick();
            }
            PlayerEvent::Failed { generation, message } => {
                if generation != self.backend_generation || self.current_path.is_none() {
                    return;
                }
                match self.flavor {
                    PlayerFlavor::Windows => {
                        self.has_media = false;
                        self.is_playing = false;
                        self.emit(PlayerOutput::Ticked);
                        let details = if message.trim().is_empty() { "Unknown".to_owned() } else { message.trim().to_owned() };
                        self.emit(PlayerOutput::Failed(details));
                    }
                    PlayerFlavor::Mac => self.item_failed(&message),
                }
            }
            PlayerEvent::SizeChanged { generation, width, height } => {
                if generation == self.backend_generation {
                    self.set_video_size(width, height);
                }
            }
            PlayerEvent::FrameReady { generation } => {
                if generation == self.backend_generation {
                    self.emit(PlayerOutput::FrameReady);
                }
            }
            PlayerEvent::PlayingChanged { generation, playing } => {
                if generation != self.backend_generation {
                    return;
                }
                self.set_playing_state(playing);
            }
            PlayerEvent::Ended { generation } => {
                if generation == self.backend_generation {
                    self.set_playing_state(false);
                }
            }
            PlayerEvent::TracksChanged { generation } => {
                if generation == self.backend_generation && self.current_path.is_some() {
                    let tracks = self.backend.tracks();
                    self.set_tracks(tracks);
                }
            }
            PlayerEvent::AudioTrackFailed { generation, message, .. } => {
                if generation == self.backend_generation && self.current_path.is_some() {
                    // An empty message is worded in the UI language by `media_tracks::audio_track_failure`.
                    self.emit(PlayerOutput::AudioTrackFailed(message.trim().to_owned()));
                }
            }
            PlayerEvent::SubtitleTrackFailed { generation, stream_index, reason } => {
                if generation == self.backend_generation && self.current_path.is_some() {
                    self.emit(PlayerOutput::SubtitleTrackFailed(stream_index, reason));
                }
            }
            PlayerEvent::SubtitlePictureChanged { generation } => {
                if generation == self.backend_generation && self.current_path.is_some() {
                    self.emit(PlayerOutput::PictureChanged);
                }
            }
        }
    }

    /// `is_playing` from the backend's state; reports `Started` / `Paused` once per change.
    fn set_playing_state(&mut self, playing: bool) {
        if self.is_playing != playing {
            self.is_playing = playing;
            self.emit(PlayerOutput::Ticked);
        }
        if playing && !self.reported_playing {
            self.reported_playing = true;
        } else if !playing && self.reported_playing {
            self.reported_playing = false;
            let position = self.position_report();
            self.emit(PlayerOutput::Playback(PlaybackReport::Paused { position }));
        }
    }

    fn item_failed(&mut self, message: &str) {
        if self.failure_reported || self.preparing_preview {
            return;
        }
        let try_preview = !self.preview_tried && matches!(self.backend.failure_kind(), None | Some(FailureKind::UnsupportedFormat));
        if try_preview && let Some(source) = self.current_path.clone() {
            self.preview_tried = true;
            self.pending_preview_position = Some(self.current_time);
            self.auto_play_when_ready |= self.is_playing;
            self.has_media = false;
            self.backend.load(None, false);
            self.set_tracks(MediaTracks::default());
            self.load_generation += 1;
            self.preparing_preview = true;
            self.emit(PlayerOutput::PreparingPreviewChanged(true));
            let generation = self.load_generation;
            self.emit(PlayerOutput::PreparePreview { source, generation });
            return;
        }
        self.report_failure(Some(message));
    }

    fn report_failure(&mut self, message: Option<&str>) {
        if self.failure_reported {
            return;
        }
        self.failure_reported = true;
        self.failure = Some(PlaybackFailure::cause(message));
        self.auto_play_when_ready = false;
        self.backend.pause();
        self.set_playing_state(false);
        self.emit(PlayerOutput::FailureChanged);
    }

    /// `dismiss_failure()`: no later failure of this load shows.
    pub fn dismiss_failure(&mut self) {
        self.failure = None;
        self.failure_reported = true;
        self.emit(PlayerOutput::FailureChanged);
    }

    fn set_video_size(&mut self, w: u32, h: u32) {
        if w > 1 && h > 1 {
            let size = (w as f64, h as f64);
            if size != self.video_size {
                self.video_size = size;
                self.emit(PlayerOutput::VideoSizeChanged(size.0, size.1));
            }
        }
    }

    // ---- ticks and seeking
    /// The 100 ms timer.
    pub fn tick(&mut self) {
        if self.disposed || !self.has_media {
            return;
        }
        let now = self.now();
        self.timeline.observe(self.backend.position(), self.backend.duration(), now);
        self.current_time = self.timeline.position;
        if self.flavor == PlayerFlavor::Windows {
            self.is_playing = self.backend.is_playing();
        } else {
            let playing = self.backend.is_playing();
            self.set_playing_state(playing);
        }
        self.update_active();
        self.emit(PlayerOutput::Ticked);
        self.report_progress();
    }

    fn report_progress(&mut self) {
        let duration = self.timeline.duration;
        if !self.is_playing || duration <= 0.0 {
            return;
        }
        let due = match self.flavor {
            // win-ui-main §10.8: every 5 s of playback position.
            PlayerFlavor::Windows => (self.current_time - self.last_progress_position).abs() >= 5.0,
            // The hub keeps its own 5 s clock (mac-ui §9.7).
            PlayerFlavor::Mac => true,
        };
        if due {
            self.last_progress_position = self.current_time;
            let position = self.current_time;
            self.emit(PlayerOutput::Playback(PlaybackReport::Progress { position, duration }));
        }
    }

    /// `seek(to:)`.
    pub fn seek(&mut self, seconds: f64) {
        if self.disposed {
            return;
        }
        if self.preparing_preview {
            self.pending_preview_position = Some(seconds.max(0.0));
            return;
        }
        let now = self.now();
        if self.has_media {
            self.timeline.observe(self.backend.position(), self.backend.duration(), now);
        }
        let target = self.timeline.seek(seconds, now);
        self.current_time = target;
        if self.has_media {
            self.backend.seek(target);
        } else {
            self.seek_when_ready = Some(target);
        }
        self.update_active();
        self.emit(PlayerOutput::Ticked);
        if self.current_path.is_some() {
            self.emit(PlayerOutput::Playback(PlaybackReport::Seeked { position: target }));
        }
    }

    /// `seek(by:)` (current time is the requested target while settling, so held arrows accumulate).
    pub fn seek_by(&mut self, delta: f64) {
        // macOS `guard player.currentItem != nil`: there is no item while the engine preview is prepared.
        if self.current_path.is_none() || self.preparing_preview {
            return;
        }
        self.seek(self.current_time + delta);
    }

    pub fn play(&mut self) {
        if self.preparing_preview {
            self.auto_play_when_ready = true;
            return;
        }
        if self.current_path.is_none() {
            return;
        }
        if !self.has_media {
            self.auto_play_when_ready = true;
            if self.flavor == PlayerFlavor::Windows {
                self.is_playing = true;
                self.emit(PlayerOutput::Ticked);
            }
            return;
        }
        self.auto_play_when_ready = false;
        self.backend.play();
        if self.flavor == PlayerFlavor::Windows {
            self.is_playing = true;
            self.emit(PlayerOutput::Ticked);
        }
    }

    pub fn pause(&mut self) {
        self.auto_play_when_ready = false;
        self.backend.pause();
        if self.flavor == PlayerFlavor::Windows {
            self.is_playing = false;
            self.emit(PlayerOutput::Ticked);
        }
    }

    /// `toggle_playback()`.
    pub fn toggle_playback(&mut self) {
        if self.disposed {
            return;
        }
        if self.preparing_preview {
            self.auto_play_when_ready = !self.auto_play_when_ready;
            return;
        }
        if !self.has_media {
            if self.current_path.is_some() {
                self.auto_play_when_ready = !self.auto_play_when_ready;
                if self.flavor == PlayerFlavor::Windows {
                    self.is_playing = self.auto_play_when_ready;
                }
                self.emit(PlayerOutput::Ticked);
            } else if self.flavor == PlayerFlavor::Windows {
                self.emit(PlayerOutput::PlayEmptyRequested);
            }
            return;
        }
        let playing = match self.flavor {
            PlayerFlavor::Windows => self.backend.is_playing(),
            PlayerFlavor::Mac => self.is_playing,
        };
        if !playing {
            let d = self.timeline.duration;
            if d > 0.0 && self.current_time >= d - 0.05 {
                self.seek(0.0);
            }
            self.play();
        } else {
            self.pause();
        }
        self.emit(PlayerOutput::Ticked);
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        self.backend.set_muted(self.muted);
        self.emit(PlayerOutput::Ticked);
    }

    /// `set_volume(v)`: clamp; a positive volume unmutes.
    pub fn set_volume(&mut self, v: f64) {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 };
        self.volume = v;
        self.backend.set_volume(v);
        if v > 0.0 && self.muted {
            self.muted = false;
            self.backend.set_muted(false);
        }
        self.emit(PlayerOutput::Ticked);
    }

    /// `adjust_volume(step)`: whole 5 % steps counted from silence while muted.
    pub fn adjust_volume(&mut self, step: f64) {
        let (v, muted) = adjusted_volume(self.volume, self.muted, step);
        self.volume = v;
        self.backend.set_volume(v);
        if muted != self.muted {
            self.muted = muted;
            self.backend.set_muted(muted);
        }
        self.emit(PlayerOutput::Ticked);
    }

    // ---- lines
    pub fn set_sentences(&mut self, sentences: Vec<Sentence>) {
        self.sentences = sentences;
        self.update_active();
    }

    fn update_active(&mut self) {
        let t = self.current_time;
        let id = self.sentences.iter().find(|s| s.start <= t && t < s.end).map(|s| s.uid);
        if id != self.active {
            self.active = id;
            self.emit(PlayerOutput::ActiveChanged(id));
        }
    }

    /// Window closing: reports `Leaving`, pauses and stops the backend (teardown runs off the UI thread there).
    pub fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        if self.current_path.is_some() {
            let position = self.position_report();
            self.emit(PlayerOutput::Playback(PlaybackReport::Leaving { position }));
        }
        self.disposed = true;
        self.has_media = false;
        self.tracks = MediaTracks::default();
        self.backend.pause();
        self.backend.dispose();
        self.is_playing = false;
        self.muted = false;
        self.volume = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::Duration;

    /// Backend fake: records calls; position/duration/playing are set by the test.
    #[derive(Default)]
    struct FakeState {
        loads: Vec<Option<PathBuf>>,
        generation: i32,
        playing: bool,
        position: f64,
        duration: f64,
        seeks: Vec<f64>,
        plays: usize,
        pauses: usize,
        volume: f64,
        muted: bool,
        failure: Option<FailureKind>,
        tracks: MediaTracks,
        selected: Vec<usize>,
        images: Vec<Option<usize>>,
        picture: Option<Arc<SubtitlePicture>>,
    }

    struct Fake(Rc<RefCell<FakeState>>);

    impl MediaPlayerBackend for Fake {
        fn load(&mut self, source: Option<MediaSource>, _auto_play: bool) -> LoadGeneration {
            let mut s = self.0.borrow_mut();
            s.generation += 1;
            s.loads.push(source.map(|m| match m {
                MediaSource::File(p) => p,
                MediaSource::Reader { path, .. } => path,
            }));
            s.playing = false;
            s.position = 0.0;
            s.generation
        }
        fn play(&mut self) {
            let mut s = self.0.borrow_mut();
            s.plays += 1;
            s.playing = true;
        }
        fn pause(&mut self) {
            let mut s = self.0.borrow_mut();
            s.pauses += 1;
            s.playing = false;
        }
        fn seek(&mut self, seconds: f64) {
            self.0.borrow_mut().seeks.push(seconds);
        }
        fn set_volume(&mut self, volume: f64) {
            self.0.borrow_mut().volume = volume;
        }
        fn set_muted(&mut self, muted: bool) {
            self.0.borrow_mut().muted = muted;
        }
        fn position(&self) -> f64 {
            self.0.borrow().position
        }
        fn duration(&self) -> f64 {
            self.0.borrow().duration
        }
        fn is_playing(&self) -> bool {
            self.0.borrow().playing
        }
        fn natural_size(&self) -> Option<(u32, u32)> {
            None
        }
        fn buffered_ratio(&self) -> f64 {
            0.0
        }
        fn latest_frame(&mut self) -> Option<VideoFrame> {
            None
        }
        fn set_event_sink(&mut self, _sink: Box<dyn Fn(PlayerEvent) + Send + Sync>) {}
        fn dispose(&mut self) {}
        fn failure_kind(&self) -> Option<FailureKind> {
            self.0.borrow().failure
        }
        fn tracks(&self) -> MediaTracks {
            self.0.borrow().tracks.clone()
        }
        fn select_audio_track(&mut self, stream_index: usize) {
            self.0.borrow_mut().selected.push(stream_index);
        }
        fn select_image_subtitle_track(&mut self, stream_index: Option<usize>) {
            self.0.borrow_mut().images.push(stream_index);
        }
        fn subtitle_picture(&self) -> Option<Arc<SubtitlePicture>> {
            self.0.borrow().picture.clone()
        }
    }

    struct Rig {
        state: Rc<RefCell<FakeState>>,
        now: Rc<Cell<Instant>>,
        player: PlayerController,
    }

    fn rig(flavor: PlayerFlavor) -> Rig {
        let state = Rc::new(RefCell::new(FakeState { volume: 1.0, ..Default::default() }));
        let now = Rc::new(Cell::new(Instant::now()));
        let n = now.clone();
        let player = PlayerController::with_parts(
            flavor,
            Box::new(Fake(state.clone())),
            Box::new(|p: &Path| MediaSource::File(p.to_path_buf())),
            Box::new(move || n.get()),
        );
        Rig { state, now, player }
    }

    impl Rig {
        fn advance(&self, ms: u64) {
            self.now.set(self.now.get() + Duration::from_millis(ms));
        }
        fn open(&mut self, duration: f64) {
            let generation = self.state.borrow().generation;
            self.state.borrow_mut().duration = duration;
            self.player.handle_event(PlayerEvent::Opened { generation, duration, natural_size: Some((1920, 1080)) });
        }
        fn report(&mut self, position: f64) {
            self.state.borrow_mut().position = position;
            self.player.tick();
        }
    }

    fn line(id: i64, start: f64, end: f64) -> Sentence {
        Sentence::new(id, start, end, format!("line {id}"))
    }

    #[test]
    fn seek_holds_the_requested_position_while_the_player_still_reports_the_old_time() {
        let mut r = rig(PlayerFlavor::Mac);
        r.player.load(Some(Path::new("/v.mp4")), false);
        r.open(300.0);
        r.report(10.0);
        r.player.seek(138.0);
        assert_eq!(r.player.current_time(), 138.0);
        r.report(10.0);
        assert_eq!(r.player.current_time(), 138.0);
        r.report(138.0);
        r.report(139.0);
        assert_eq!(r.player.current_time(), 139.0);
    }

    #[test]
    fn tolerance_is_inclusive_at_a_quarter_second_and_given_up_after_two_seconds() {
        let mut r = rig(PlayerFlavor::Mac);
        r.player.load(Some(Path::new("/v.mp4")), false);
        r.open(300.0);
        r.player.seek(20.0);
        r.report(19.75);
        assert_eq!(r.player.current_time(), 19.75);
        r.player.seek(138.0);
        r.advance(3000);
        r.report(136.0);
        assert_eq!(r.player.current_time(), 136.0);
    }

    #[test]
    fn a_newer_seek_replaces_the_pending_one() {
        let mut r = rig(PlayerFlavor::Windows);
        r.player.load(Some(Path::new("C:/v.mp4")), false);
        r.open(300.0);
        r.player.seek(30.0);
        r.player.seek(180.0);
        r.report(30.0);
        assert_eq!(r.player.current_time(), 180.0);
        r.report(180.0);
        assert_eq!(r.player.current_time(), 180.0);
    }

    #[test]
    fn loading_another_video_drops_a_pending_seek_and_the_duration() {
        let mut r = rig(PlayerFlavor::Mac);
        r.player.load(Some(Path::new("/a.mp4")), false);
        r.open(300.0);
        r.player.seek(100.0);
        r.player.load(Some(Path::new("/b.mp4")), false);
        assert_eq!(r.player.duration(), 0.0);
        assert_eq!(r.player.current_time(), 0.0);
        r.open(30.0);
        r.report(0.0);
        assert_eq!(r.player.current_time(), 0.0);
        r.player.seek(100.0);
        assert_eq!(r.player.current_time(), 30.0);
    }

    #[test]
    fn a_seek_before_the_media_opened_is_applied_when_it_is_ready() {
        let mut r = rig(PlayerFlavor::Windows);
        r.player.load(Some(Path::new("C:/v.mp4")), false);
        r.player.seek(12.0);
        assert!(r.state.borrow().seeks.is_empty());
        r.open(60.0);
        assert_eq!(r.state.borrow().seeks, vec![12.0]);
    }

    #[test]
    fn seeking_without_a_video_does_nothing() {
        let mut r = rig(PlayerFlavor::Mac);
        r.player.seek_by(5.0);
        assert_eq!(r.player.current_time(), 0.0);
        assert!(r.state.borrow().seeks.is_empty());
    }

    #[test]
    fn auto_play_starts_when_the_item_is_ready_and_not_for_a_stale_load() {
        let mut r = rig(PlayerFlavor::Mac);
        r.player.load(Some(Path::new("/a.mp4")), true);
        let stale = r.state.borrow().generation;
        r.player.load(Some(Path::new("/b.mp4")), false);
        r.player.handle_event(PlayerEvent::Opened { generation: stale, duration: 10.0, natural_size: None });
        assert_eq!(r.state.borrow().plays, 0);
        r.open(10.0);
        assert_eq!(r.state.borrow().plays, 0, "no auto play when not asked");
        r.player.load(Some(Path::new("/c.mp4")), true);
        r.open(10.0);
        assert_eq!(r.state.borrow().plays, 1);
    }

    #[test]
    fn pausing_before_the_item_is_ready_cancels_auto_play_and_failure_does_too() {
        let mut r = rig(PlayerFlavor::Mac);
        r.player.load(Some(Path::new("/a.mp4")), true);
        r.player.pause();
        r.open(10.0);
        assert_eq!(r.state.borrow().plays, 0);
        r.state.borrow_mut().failure = Some(FailureKind::Io);
        r.player.load(Some(Path::new("/b.mp4")), true);
        let g = r.state.borrow().generation;
        r.player.handle_event(PlayerEvent::Failed { generation: g, message: "disk".into() });
        assert!(!r.player.auto_play_when_ready());
        assert_eq!(r.player.failure(), Some(&PlaybackFailure::System("disk".into())));
    }

    #[test]
    fn is_playing_never_outlives_a_paused_player_and_reports_alternate() {
        let mut r = rig(PlayerFlavor::Mac);
        r.player.load(Some(Path::new("/a.mp4")), false);
        r.open(10.0);
        r.player.take_outputs();
        let g = r.state.borrow().generation;
        r.player.handle_event(PlayerEvent::PlayingChanged { generation: g, playing: true });
        r.player.handle_event(PlayerEvent::PlayingChanged { generation: g, playing: true });
        r.player.handle_event(PlayerEvent::PlayingChanged { generation: g, playing: false });
        r.player.handle_event(PlayerEvent::PlayingChanged { generation: g, playing: false });
        assert!(!r.player.is_playing());
        let pauses = r.player.take_outputs().into_iter().filter(|o| matches!(o, PlayerOutput::Playback(PlaybackReport::Paused { .. }))).count();
        assert_eq!(pauses, 1);
        // A new load only reports leaving.
        r.player.load(Some(Path::new("/b.mp4")), false);
        let outs = r.player.take_outputs();
        assert!(outs.iter().any(|o| matches!(o, PlayerOutput::Playback(PlaybackReport::Leaving { .. }))));
        assert!(!outs.iter().any(|o| matches!(o, PlayerOutput::Playback(PlaybackReport::Paused { .. }))));
    }

    #[test]
    fn volume_moves_in_whole_five_percent_steps_and_counts_from_silence_while_muted() {
        let mut r = rig(PlayerFlavor::Mac);
        r.player.set_volume(0.8);
        r.player.adjust_volume(-0.05);
        assert_eq!(r.player.volume(), 0.75);
        r.player.set_volume(0.98);
        r.player.adjust_volume(0.05);
        assert_eq!(r.player.volume(), 1.0);
        r.player.toggle_mute();
        r.player.adjust_volume(0.05);
        assert_eq!(r.player.volume(), 0.05);
        assert!(!r.player.is_muted());
    }

    #[test]
    fn volume_and_mute_live_on_the_player_and_survive_a_new_video() {
        let mut r = rig(PlayerFlavor::Mac);
        r.player.set_volume(0.4);
        r.player.toggle_mute();
        r.player.load(Some(Path::new("/a.mp4")), false);
        r.player.load(Some(Path::new("/b.mp4")), false);
        assert_eq!(r.player.volume(), 0.4);
        assert!(r.player.is_muted());
        assert_eq!(r.state.borrow().volume, 0.4);
        assert!(r.state.borrow().muted);
    }

    #[test]
    fn toggle_restarts_at_the_end_and_play_empty_is_windows_only() {
        let mut r = rig(PlayerFlavor::Windows);
        r.player.toggle_playback();
        assert!(r.player.take_outputs().contains(&PlayerOutput::PlayEmptyRequested));
        r.player.load(Some(Path::new("C:/v.mp4")), false);
        r.open(10.0);
        r.report(9.97);
        r.player.toggle_playback();
        assert_eq!(r.state.borrow().seeks.last(), Some(&0.0));
        assert!(r.player.is_playing());
        r.player.toggle_playback();
        assert!(!r.player.is_playing());

        let mut m = rig(PlayerFlavor::Mac);
        m.player.toggle_playback();
        assert!(!m.player.take_outputs().contains(&PlayerOutput::PlayEmptyRequested));
    }

    #[test]
    fn toggling_before_the_media_opened_flips_auto_play() {
        let mut r = rig(PlayerFlavor::Windows);
        r.player.load(Some(Path::new("C:/v.mp4")), false);
        r.player.toggle_playback();
        assert!(r.player.auto_play_when_ready());
        assert!(r.player.is_playing());
        r.open(5.0);
        assert_eq!(r.state.borrow().plays, 1);
    }

    #[test]
    fn the_active_line_follows_time_and_sentences() {
        let mut r = rig(PlayerFlavor::Windows);
        let lines = vec![line(1, 0.0, 2.0), line(2, 2.0, 4.0)];
        let (a, b) = (lines[0].uid, lines[1].uid);
        r.player.set_sentences(lines);
        assert_eq!(r.player.active_id(), Some(a));
        r.player.load(Some(Path::new("C:/v.mp4")), false);
        r.open(10.0);
        r.report(2.5);
        assert_eq!(r.player.active_id(), Some(b));
        r.report(5.0);
        assert_eq!(r.player.active_id(), None);
    }

    #[test]
    fn windows_failure_reports_details_and_ignores_stale_loads() {
        let mut r = rig(PlayerFlavor::Windows);
        r.player.load(Some(Path::new("C:/a.mp4")), true);
        let stale = r.state.borrow().generation;
        r.player.load(Some(Path::new("C:/b.mp4")), true);
        r.player.take_outputs();
        r.player.handle_event(PlayerEvent::Failed { generation: stale, message: "old".into() });
        assert!(r.player.take_outputs().is_empty());
        let g = r.state.borrow().generation;
        r.player.handle_event(PlayerEvent::Failed { generation: g, message: "  bad codec ".into() });
        let outs = r.player.take_outputs();
        assert!(outs.contains(&PlayerOutput::Failed("bad codec".into())));
        assert!(!r.player.is_playing());
    }

    #[test]
    fn mac_unsupported_format_tries_the_preview_once_then_reports() {
        let mut r = rig(PlayerFlavor::Mac);
        r.state.borrow_mut().failure = Some(FailureKind::UnsupportedFormat);
        r.player.load(Some(Path::new("/clip.wmv")), true);
        let g = r.state.borrow().generation;
        r.player.take_outputs();
        r.player.handle_event(PlayerEvent::Failed { generation: g, message: "unknown container".into() });
        let outs = r.player.take_outputs();
        let Some(PlayerOutput::PreparePreview { source, generation }) = outs.iter().find(|o| matches!(o, PlayerOutput::PreparePreview { .. })).cloned() else {
            panic!("no preview requested: {outs:?}");
        };
        assert_eq!(source, PathBuf::from("/clip.wmv"));
        assert!(r.player.is_preparing_preview());
        // Seeks and toggles while preparing are remembered.
        r.player.seek(7.0);
        r.player.preview_ready(generation, Ok(PathBuf::from("/tmp/preview.mp4")));
        assert!(!r.player.is_preparing_preview());
        assert_eq!(r.state.borrow().loads.last().cloned().flatten(), Some(PathBuf::from("/tmp/preview.mp4")));
        r.open(20.0);
        assert_eq!(r.state.borrow().seeks.last(), Some(&7.0));
        assert_eq!(r.state.borrow().plays, 1);
        // A failure of the preview is reported (no second preview).
        let g = r.state.borrow().generation;
        r.player.handle_event(PlayerEvent::Failed { generation: g, message: "".into() });
        assert_eq!(r.player.failure(), Some(&PlaybackFailure::Unknown));
    }

    #[test]
    fn mac_arrow_seeks_do_nothing_while_the_preview_is_prepared_and_a_dismissed_notice_does_not_block_playback() {
        let mut r = rig(PlayerFlavor::Mac);
        r.state.borrow_mut().failure = Some(FailureKind::UnsupportedFormat);
        r.player.load(Some(Path::new("/clip.wmv")), false);
        let g = r.state.borrow().generation;
        r.player.handle_event(PlayerEvent::Failed { generation: g, message: "x".into() });
        let generation = r
            .player
            .take_outputs()
            .into_iter()
            .find_map(|o| if let PlayerOutput::PreparePreview { generation, .. } = o { Some(generation) } else { None })
            .expect("preview requested");
        // `seek(by:)` has no item to act on (Swift `guard player.currentItem != nil`).
        r.player.seek_by(5.0);
        r.player.preview_ready(generation, Ok(PathBuf::from("/tmp/p.mp4")));
        r.open(20.0);
        assert!(r.state.borrow().seeks.is_empty(), "no seek was queued while preparing");

        // A failure the user dismissed does not keep a later ready item from starting (`guard failure == nil`).
        let mut r = rig(PlayerFlavor::Mac);
        r.state.borrow_mut().failure = Some(FailureKind::Io);
        r.player.load(Some(Path::new("/a.mp4")), true);
        let g = r.state.borrow().generation;
        r.player.handle_event(PlayerEvent::Failed { generation: g, message: "x".into() });
        r.player.dismiss_failure();
        r.player.play();
        r.open(10.0);
        assert!(r.player.has_media());
        assert_eq!(r.state.borrow().plays, 1);
    }

    #[test]
    fn dismissing_the_failure_hides_it_for_the_rest_of_the_load() {
        let mut r = rig(PlayerFlavor::Mac);
        r.state.borrow_mut().failure = Some(FailureKind::Io);
        r.player.load(Some(Path::new("/a.mp4")), false);
        let g = r.state.borrow().generation;
        r.player.handle_event(PlayerEvent::Failed { generation: g, message: "x".into() });
        assert!(r.player.failure().is_some());
        r.player.dismiss_failure();
        r.player.handle_event(PlayerEvent::Failed { generation: g, message: "y".into() });
        assert!(r.player.failure().is_none());
        r.player.load(Some(Path::new("/b.mp4")), false);
        let g = r.state.borrow().generation;
        r.player.handle_event(PlayerEvent::Failed { generation: g, message: "z".into() });
        assert_eq!(r.player.failure().map(PlaybackFailure::id), Some("system:z".into()));
    }

    #[test]
    fn failure_texts_and_ids() {
        assert_eq!(PlaybackFailure::cause(None), PlaybackFailure::NotPlayable);
        assert_eq!(PlaybackFailure::cause(Some("  ")), PlaybackFailure::Unknown);
        assert_eq!(PlaybackFailure::NotPlayable.message().id(), "playback-failed:not-playable");
        assert_eq!(PlaybackFailure::System("e".into()).message().id(), "playback-failed:system:e");
    }

    #[test]
    fn tracks_follow_the_backend_for_the_current_load_and_switches_pass_through() {
        let mut r = rig(PlayerFlavor::Windows);
        let two = MediaTracks {
            audio: vec![
                sa_player::AudioTrack { stream_index: 1, ..Default::default() },
                sa_player::AudioTrack { stream_index: 2, ..Default::default() },
            ],
            subtitles: vec![],
            active_audio: Some(1),
            active_image_subtitle: None,
        };
        r.state.borrow_mut().tracks = two.clone();
        r.player.select_audio_track(2);
        assert!(r.state.borrow().selected.is_empty(), "nothing to switch before the media opened");
        r.player.load(Some(Path::new("C:/v.mkv")), false);
        r.open(10.0);
        let g = r.state.borrow().generation;
        r.player.take_outputs();
        r.player.handle_event(PlayerEvent::TracksChanged { generation: g - 1 });
        assert!(r.player.tracks().audio.is_empty(), "a stale load is ignored");
        r.player.handle_event(PlayerEvent::TracksChanged { generation: g });
        assert_eq!(r.player.tracks(), &two);
        assert!(r.player.take_outputs().contains(&PlayerOutput::TracksChanged));
        // The backend decides what is already active (this list can be behind a switch in flight: A → B → A).
        r.player.select_audio_track(2);
        r.player.select_audio_track(1);
        assert_eq!(r.state.borrow().selected, vec![2, 1]);
        r.player.handle_event(PlayerEvent::AudioTrackFailed { generation: g, stream_index: 2, message: " no decoder ".into() });
        assert!(r.player.take_outputs().contains(&PlayerOutput::AudioTrackFailed("no decoder".into())));
        // A new load clears them at once.
        r.player.load(Some(Path::new("C:/w.mkv")), false);
        assert!(r.player.tracks().audio.is_empty());
        assert!(r.player.take_outputs().contains(&PlayerOutput::TracksChanged));
        assert_eq!(r.player.playing_file(), Some(Path::new("C:/w.mkv")));
    }

    #[test]
    fn image_subtitle_choices_pass_through_and_picture_changes_of_the_current_load_are_reported() {
        let mut r = rig(PlayerFlavor::Windows);
        r.player.select_image_subtitle_track(Some(4));
        assert!(r.state.borrow().images.is_empty(), "nothing to show before the media opened");
        r.player.load(Some(Path::new("C:/v.mkv")), false);
        assert!(r.player.take_outputs().contains(&PlayerOutput::PictureChanged), "a new video clears the picture");
        r.open(10.0);
        r.player.select_image_subtitle_track(Some(4));
        r.player.select_image_subtitle_track(None);
        assert_eq!(r.state.borrow().images, vec![Some(4), None]);
        let g = r.state.borrow().generation;
        r.player.take_outputs();
        r.player.handle_event(PlayerEvent::SubtitlePictureChanged { generation: g - 1 });
        assert!(r.player.take_outputs().is_empty(), "a stale load is ignored");
        r.player.handle_event(PlayerEvent::SubtitlePictureChanged { generation: g });
        assert_eq!(r.player.take_outputs(), vec![PlayerOutput::PictureChanged]);
        let picture = Arc::new(SubtitlePicture { id: 7, stream_index: 4, canvas: (720, 480), video_size: None, start: 1.0, end: None, rects: vec![] });
        r.state.borrow_mut().picture = Some(picture.clone());
        assert_eq!(r.player.subtitle_picture(), Some(picture));
        let reason = PictureTrackFailure::Ffmpeg("not a picture track".into());
        r.player.handle_event(PlayerEvent::SubtitleTrackFailed { generation: g, stream_index: 3, reason: reason.clone() });
        assert_eq!(r.player.take_outputs(), vec![PlayerOutput::SubtitleTrackFailed(3, reason)]);
        r.player.dispose();
        assert!(r.player.subtitle_picture().is_none());
    }

    #[test]
    fn windows_progress_is_reported_every_five_seconds_of_playback() {
        let mut r = rig(PlayerFlavor::Windows);
        r.player.load(Some(Path::new("C:/v.mp4")), true);
        r.open(100.0);
        r.player.take_outputs();
        let mut reported = Vec::new();
        for t in [0.5, 1.0, 4.0, 5.6, 6.0, 10.7] {
            r.report(t);
            for o in r.player.take_outputs() {
                if let PlayerOutput::Playback(PlaybackReport::Progress { position, .. }) = o {
                    reported.push(position);
                }
            }
        }
        // `last_recorded` starts at -1 on every video change.
        assert_eq!(reported, vec![4.0, 10.7]);
    }
}
