//! `MainWindowController`: wires one `MainWindow` to the view models and its own player (win-ui-main §6–§18,
//! mac-ui §3–§8, §10). The view models decide; this file only maps their state onto `MainBridge` /
//! `ExplorerBridge` / `AppBridge` / `MenuBridge` (diffed, on the UI thread) and turns callbacks into VM commands.
//!
//! Composition (which stores, which engine, which settings window) is the app shell's job: it builds
//! [`MainWindowDeps`] and calls [`MainWindowController::attach`].

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use sa_core::appearance::{AppearanceSettings, OverlayBackground, SpeakerPalette, SubtitleDisplay};
use sa_core::cancel::CancellationToken;
use sa_core::dispatch::UiDispatcher;
use sa_core::engine_types::SubtitleFormat;
use sa_core::models::SubtitleLanguage;
use sa_core::platform::{FileFilter, PlatformShell};
use sa_core::settings::{SettingsStore, defaults, keys};
use sa_enginekit::editing::SentenceListExt;
use sa_player::ffmpeg_backend::{ExtractError, ExtractedSubtitle};
use sa_player::{MediaPlayerBackend, VideoFrame};
use sa_viewmodels::explorer::{ExplorerChange, FileExplorerModel};
use sa_viewmodels::media_tracks::{self, EmbeddedSubtitleReads, TrackChoice, TrackMenu};
use sa_viewmodels::hub::{HubChange, VideoPlatformModel};
use sa_viewmodels::shortcuts::{self, PlayerShortcut, ShortcutFocus, ShortcutTarget, WindowsKeyAction};
use sa_viewmodels::subtitle::subtitle_time_short;
use sa_viewmodels::types::{ExportLanguage, JobStatusKind, PlaylistItemState, SubtitleChange, TimingEdge, Workspace};
use sa_viewmodels::uiref::UiKey;
use sa_viewmodels::{PlatformPolicy, SubtitleViewModel};
use uuid::Uuid;

use super::hud::{HudFlavor, HudState};
use super::input::{self, DropBatcher, SlintKey};
use super::layout::{LayoutFlags, LayoutMode, MacEditorContent, mac_editor_content, windows_layout};
use super::menus::{self, Entry, MenuAction, RowMenuState};
use super::player::{PlayerController, PlayerFlavor, PlayerOutput, SourceFactory};
use super::rows::{fill_if_changed, sync_keyed};
use super::status::{ErrorBar, bridge_fraction, mac_playback_notice, status_kind_index};
use super::transport::{TransportInput, scrub_target, synchronize, windows_wheel_volume};
use super::video_gl::VideoSurface;
use crate::ui::{self, AppBridge, ExplorerBridge, ExplorerRowData, MainBridge, MainWindow, MenuBridge, MenuEntry, PickerItem, QueueRowData, SentenceRowData};

/// Converts the engine `preview` command into a playable copy (macOS fallback; runs on a worker thread).
pub type PreviewMaker = Arc<dyn Fn(&Path) -> Result<PathBuf, String> + Send + Sync>;

/// Reads embedded text subtitle stream `stream` of a media file as an importable document (runs on a worker thread;
/// default: `sa_player::ffmpeg_backend::extract_text_subtitles`).
pub type SubtitleReader =
    Arc<dyn Fn(&Path, usize, &CancellationToken, &mut dyn FnMut(f64)) -> Result<ExtractedSubtitle, ExtractError> + Send + Sync>;

fn default_subtitle_reader() -> SubtitleReader {
    Arc::new(|path: &Path, stream: usize, cancel: &CancellationToken, progress: &mut dyn FnMut(f64)| {
        sa_player::ffmpeg_backend::extract_text_subtitles(path, stream, cancel, progress)
    })
}

/// Everything a main window needs. `explorer` and `hub` are optional so the window also runs in previews/tests.
pub struct MainWindowDeps {
    pub vm: Rc<SubtitleViewModel>,
    pub explorer: Option<Rc<FileExplorerModel>>,
    pub hub: Option<Rc<VideoPlatformModel>>,
    pub settings: Arc<dyn SettingsStore>,
    pub appearance: Option<Arc<AppearanceSettings>>,
    pub shell: Arc<dyn PlatformShell>,
    pub ui: Arc<dyn UiDispatcher>,
    pub player: Box<dyn MediaPlayerBackend>,
    /// Windows: wraps a file in the buffered (VideoCache) stream; default = plain file.
    pub source_factory: Option<SourceFactory>,
    pub preview: Option<PreviewMaker>,
    /// Embedded subtitle tracks; `None` = FFmpeg's (tests inject one).
    pub subtitle_reader: Option<SubtitleReader>,
    pub hooks: MainWindowHooks,
}

/// Actions owned by other areas (settings window, profile UI, translation-model picker, Apple-menu roles).
#[derive(Default)]
pub struct MainWindowHooks {
    pub open_settings: Option<Rc<dyn Fn()>>,
    pub profile_clicked: Option<Rc<dyn Fn()>>,
    pub translation_model_clicked: Option<Rc<dyn Fn()>>,
    /// Real window full screen (winit `set_fullscreen`); the controller keeps the state.
    pub set_full_screen: Option<Rc<dyn Fn(bool)>>,
    pub minimize: Option<Rc<dyn Fn()>>,
    pub toggle_maximize: Option<Rc<dyn Fn()>>,
    pub drag_window: Option<Rc<dyn Fn()>>,
    pub close_window: Option<Rc<dyn Fn()>>,
    pub new_window: Option<Rc<dyn Fn()>>,
}

/// Row-list bookkeeping (diagnostics, see [`MainWindowController::row_sync_counters`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RowSyncCounters {
    /// Full `sync_rows` rebuilds.
    pub full_syncs: usize,
    /// Rows rewritten by active-line changes (two per change at most).
    pub active_row_updates: usize,
}

#[derive(Default)]
struct UiState {
    full_screen: bool,
    editing_text: bool,
    list_focused: bool,
    anchor: Option<Uuid>,
    seek_scrubbing: bool,
    scrub_ratio: f64,
    volume_scrubbing: bool,
    layout_epoch: i32,
    scroll_epoch: i32,
    run_after_picking: bool,
    row_uids: Vec<Uuid>,
    /// The line the rows currently mark as playing (`is_active`).
    active_row: Option<Uuid>,
    row_counters: RowSyncCounters,
    queue_ids: Vec<Uuid>,
    explorer_paths: Vec<PathBuf>,
    explorer_folders: Vec<bool>,
    language_codes: Vec<&'static str>,
    target_codes: Vec<&'static str>,
    /// RGBA path: the buffer `video-frame` shows, and the one shown before it (written next, so the copy never
    /// has to copy-on-write a buffer the bridge still holds).
    frame: Option<SharedPixelBuffer<Rgba8Pixel>>,
    spare_frame: Option<SharedPixelBuffer<Rgba8Pixel>>,
    /// What the player was last told about planar output (the GPU video path is up).
    planar_output: bool,
    last_layout: Option<(LayoutFlags, bool, bool)>,
    /// The ids `menu-entries` currently shows (`overflow` items map back to toolbar actions).
    overflow_items: Vec<usize>,
    last_window_size: (f32, f32),
    mac_notches: shortcuts::ScrollNotches,
    /// The player's track menu is open (it follows track and read changes; the controls stay up).
    tracks_menu_open: bool,
    /// The head of the menu id `menu-entries` was last filled for (`tracks`, `model`, ...): a track change only
    /// refills the shared entries while they are the track menu's.
    last_menu: String,
    /// macOS: a track menu failure shown in the status bar like a playback failure (dismissable, cleared with the
    /// video; the session's own status is left alone).
    track_notice: Option<sa_core::l10n::LocalizedMessage>,
}

pub struct MainWindowController {
    window: slint::Weak<MainWindow>,
    mac: bool,
    policy: Arc<PlatformPolicy>,
    vm: Rc<SubtitleViewModel>,
    explorer: Option<Rc<FileExplorerModel>>,
    hub: Option<Rc<VideoPlatformModel>>,
    settings: Arc<dyn SettingsStore>,
    appearance: Option<Arc<AppearanceSettings>>,
    shell: Arc<dyn PlatformShell>,
    ui_dispatcher: Arc<dyn UiDispatcher>,
    preview: Option<PreviewMaker>,
    subtitle_reader: SubtitleReader,
    /// The embedded subtitle track being read.
    reads: RefCell<EmbeddedSubtitleReads>,
    hooks: MainWindowHooks,
    player: RefCell<PlayerController>,
    video: Rc<VideoSurface>,
    hud: RefCell<HudState>,
    error_bar: RefCell<ErrorBar>,
    drops: RefCell<DropBatcher>,
    st: RefCell<UiState>,
    rows: Rc<VecModel<SentenceRowData>>,
    queue_rows: Rc<VecModel<QueueRowData>>,
    explorer_rows: Rc<VecModel<ExplorerRowData>>,
    language_items: Rc<VecModel<PickerItem>>,
    target_items: Rc<VecModel<PickerItem>>,
    menu_entries: Rc<VecModel<MenuEntry>>,
    timer: slint::Timer,
    /// `SUBTITLE_ADDER_PLAYER_STATS=1`: player counters on stderr once a second (diagnostics).
    stats: Option<StatsLog>,
    key: Cell<Option<UiKey<MainWindowController>>>,
    disposed: Cell<bool>,
    /// Windows: the attached native chrome and the accent watcher (`attach_native_chrome`).
    #[cfg(windows)]
    native: RefCell<Option<NativeChrome>>,
}

#[cfg(windows)]
struct NativeChrome {
    chrome: Rc<super::chrome::MainChrome>,
    _accent: Option<sa_platform::win::accent::AccentWatcher>,
}

fn s(v: impl Into<SharedString>) -> SharedString {
    v.into()
}

fn to_ui_workspace(w: Workspace) -> ui::Workspace {
    match w {
        Workspace::Editor => ui::Workspace::Editor,
        Workspace::Platform => ui::Workspace::Platform,
        Workspace::Playlist => ui::Workspace::Playlist,
    }
}

fn from_ui_workspace(w: ui::Workspace) -> Workspace {
    match w {
        ui::Workspace::Editor => Workspace::Editor,
        ui::Workspace::Platform => Workspace::Platform,
        ui::Workspace::Playlist => Workspace::Playlist,
    }
}

/// A menu builder's entry as the bridge struct (also used by the look-check demo).
pub(crate) fn menu_entry(e: Entry) -> MenuEntry {
    to_menu_entry(e)
}

fn to_menu_entry(e: Entry) -> MenuEntry {
    MenuEntry {
        id: e.id.into(),
        title: e.title.into(),
        glyph: e.glyph.into(),
        symbol: e.symbol.into(),
        tint: e.tint.map(|(r, g, b)| slint::Color::from_rgb_u8(r, g, b)).unwrap_or(slint::Color::from_argb_u8(0, 0, 0, 0)),
        checkable: e.checkable,
        checked: e.checked,
        radio: e.radio,
        enabled: e.enabled,
        separator: e.separator,
        destructive: e.destructive,
        hint: e.hint.into(),
        level: e.level,
        submenu: e.submenu,
        header: e.header,
    }
}

/// `SUBTITLE_ADDER_PLAYER_STATS=1`: the once-a-second `[player-stats]` line, written to stderr by a background thread
/// (a write to a redirected stderr can block the UI thread for milliseconds, which would show up as a late frame in
/// the very measurement the line is for).
struct StatsLog {
    last: Cell<Instant>,
    lines: std::sync::mpsc::Sender<String>,
}

impl StatsLog {
    fn from_env() -> Option<Self> {
        std::env::var_os("SUBTITLE_ADDER_PLAYER_STATS").filter(|v| v == "1")?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("player-stats".into())
            .spawn(move || {
                while let Ok(line) = rx.recv() {
                    eprintln!("{line}");
                }
            })
            .ok()?;
        Some(Self { last: Cell::new(Instant::now()), lines: tx })
    }
}

/// Copies an RGBA frame into a reusable pixel buffer.
fn frame_to_buffer(frame: &VideoFrame, reuse: Option<SharedPixelBuffer<Rgba8Pixel>>) -> SharedPixelBuffer<Rgba8Pixel> {
    let (w, h) = (frame.width, frame.height);
    let mut buf = match reuse {
        Some(b) if b.width() == w && b.height() == h => b,
        _ => SharedPixelBuffer::<Rgba8Pixel>::new(w, h),
    };
    let dst = buf.make_mut_bytes();
    let row = (w * 4) as usize;
    let stride = frame.stride.max(w * 4) as usize;
    if stride == row && frame.rgba.len() >= dst.len() {
        dst.copy_from_slice(&frame.rgba[..dst.len()]);
    } else {
        for y in 0..h as usize {
            let src = &frame.rgba[y * stride..(y * stride + row).min(frame.rgba.len())];
            dst[y * row..y * row + src.len()].copy_from_slice(src);
        }
    }
    buf
}

impl MainWindowController {
    /// Attaches to `window` and pushes the initial state. The returned `Rc` must be kept alive with the window.
    pub fn attach(window: &MainWindow, deps: MainWindowDeps) -> Rc<Self> {
        let policy = deps.vm.policy();
        let mac = policy.catalog_chain == sa_core::l10n::CatalogChain::Mac;
        let flavor = if mac { PlayerFlavor::Mac } else { PlayerFlavor::Windows };
        let player = match deps.source_factory {
            Some(f) => PlayerController::with_parts(flavor, deps.player, f, Box::new(Instant::now)),
            None => PlayerController::new(flavor, deps.player),
        };
        let this = Rc::new(Self {
            window: window.as_weak(),
            mac,
            policy,
            vm: deps.vm,
            explorer: deps.explorer,
            hub: deps.hub,
            settings: deps.settings,
            appearance: deps.appearance,
            shell: deps.shell,
            ui_dispatcher: deps.ui,
            preview: deps.preview,
            subtitle_reader: deps.subtitle_reader.unwrap_or_else(default_subtitle_reader),
            reads: RefCell::new(EmbeddedSubtitleReads::default()),
            hooks: deps.hooks,
            player: RefCell::new(player),
            video: VideoSurface::for_main_window(window),
            hud: RefCell::new(HudState::new(if mac { HudFlavor::Mac } else { HudFlavor::Windows })),
            error_bar: RefCell::new(ErrorBar::default()),
            drops: RefCell::new(DropBatcher::default()),
            st: RefCell::new(UiState::default()),
            rows: Rc::new(VecModel::default()),
            queue_rows: Rc::new(VecModel::default()),
            explorer_rows: Rc::new(VecModel::default()),
            language_items: Rc::new(VecModel::default()),
            target_items: Rc::new(VecModel::default()),
            menu_entries: Rc::new(VecModel::default()),
            timer: slint::Timer::default(),
            stats: StatsLog::from_env(),
            key: Cell::new(None),
            disposed: Cell::new(false),
            #[cfg(windows)]
            native: RefCell::new(None),
        });
        let key = UiKey::register(&this);
        this.key.set(Some(key));
        this.install_models(window);
        this.wire_player();
        this.wire_view_models();
        this.wire_callbacks(window);
        this.start_timer();
        this.refresh_all(true);
        if this.vm.video().is_some() {
            this.on_video_changed();
        }
        this
    }

    fn win(&self) -> Option<MainWindow> {
        self.window.upgrade()
    }

    fn weak(self: &Rc<Self>) -> std::rc::Weak<Self> {
        Rc::downgrade(self)
    }

    fn install_models(&self, w: &MainWindow) {
        let main = w.global::<MainBridge>();
        main.set_rows(ModelRc::from(self.rows.clone()));
        main.set_queue_rows(ModelRc::from(self.queue_rows.clone()));
        main.set_language_items(ModelRc::from(self.language_items.clone()));
        main.set_target_items(ModelRc::from(self.target_items.clone()));
        main.set_menu_entries(ModelRc::from(self.menu_entries.clone()));
        w.global::<ExplorerBridge>().set_rows(ModelRc::from(self.explorer_rows.clone()));
    }

    // ------------------------------------------------------------------ wiring

    fn wire_player(self: &Rc<Self>) {
        let key = self.key.get().expect("registered");
        let ui = self.ui_dispatcher.clone();
        self.player.borrow_mut().set_event_sink(Box::new(move |ev| {
            key.post(&ui, move |c| c.on_player_event(ev));
        }));
        let weak = self.weak();
        self.video.set_trace_clock(Box::new(move || Some(weak.upgrade()?.player.try_borrow().ok()?.backend_position())));
    }

    fn wire_view_models(self: &Rc<Self>) {
        let weak = self.weak();
        self.vm.on_change(Box::new(move |change| {
            if let Some(c) = weak.upgrade() {
                c.on_vm_change(change);
            }
        }));
        if let Some(ex) = &self.explorer {
            let weak = self.weak();
            ex.on_change(Box::new(move |change| {
                if let Some(c) = weak.upgrade() {
                    c.on_explorer_change(change);
                }
            }));
        }
        if let Some(hub) = &self.hub {
            let weak = self.weak();
            hub.on_change(Box::new(move |change| {
                if let Some(c) = weak.upgrade()
                    && matches!(change, HubChange::Session | HubChange::Engagement)
                {
                    c.sync_engagement();
                    if change == HubChange::Session
                        && let Some(ex) = &c.explorer
                    {
                        ex.set_logged_in(c.hub.as_ref().is_some_and(|h| h.is_logged_in()));
                    }
                }
            }));
        }
        if let Some(app) = &self.appearance {
            let key = self.key.get().expect("registered");
            let ui = self.ui_dispatcher.clone();
            // The subscription lives as long as the window (the controller is dropped with it).
            app.subscribe(move |_| {
                key.post(&ui, |c| c.sync_overlay_style());
            })
            .detach();
        }
        let key = self.key.get().expect("registered");
        let ui = self.ui_dispatcher.clone();
        sa_core::l10n::LanguageState::shared()
            .subscribe(move |_| {
                key.post(&ui, |c| c.refresh_all(true));
            })
            .detach();
    }

    fn start_timer(self: &Rc<Self>) {
        let weak = self.weak();
        // win-ui-main §10.5: 100 ms on the UI thread (also drives the HUD timer and the drop batch).
        self.timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
            if let Some(c) = weak.upgrade() {
                c.on_timer();
            }
        });
    }

    fn on_timer(&self) {
        if self.disposed.get() {
            return;
        }
        self.player.borrow_mut().tick();
        self.drain_player();
        if let Some(stats) = &self.stats {
            if stats.last.get().elapsed() >= Duration::from_secs(1) {
                stats.last.set(Instant::now());
                let p = self.player.borrow();
                if let Some(line) = p.diagnostics() {
                    let (uploads, renders, gpu) = (self.video.uploads(), self.video.renders(), self.video.available());
                    // The track state as the backend last reported it, and what the subtitle list holds.
                    let t = p.tracks();
                    let active = t.active_audio.map_or_else(|| "-".to_owned(), |i| i.to_string());
                    let tracks = format!(
                        "audio_tracks={} subtitle_tracks={} active_audio={active} playing={} pos={:.2} lines={}",
                        t.audio.len(),
                        t.subtitles.len(),
                        p.is_playing(),
                        p.current_time(),
                        self.vm.sentences().len()
                    );
                    let unchanged = self.video.window_unchanged().map(|n| format!(" window_unchanged={n}")).unwrap_or_default();
                    let _ = stats.lines.send(format!("[player-stats] {line} gpu={gpu} uploaded={uploads} renders={renders}{unchanged} {tracks}"));
                }
            }
        }
        if self.hud.borrow_mut().poll(Instant::now()) {
            self.sync_hud();
        }
        let batch = self.drops.borrow_mut().take_if_settled(Instant::now());
        if let Some(paths) = batch {
            self.handle_drop(paths);
        }
    }

    // ------------------------------------------------------------------ view-model changes

    fn on_vm_change(&self, change: SubtitleChange) {
        if self.disposed.get() {
            return;
        }
        match change {
            SubtitleChange::Video => self.on_video_changed(),
            SubtitleChange::Sentences => {
                self.player.borrow_mut().set_sentences(self.vm.sentences());
                self.drain_player();
                self.sync_content();
                self.sync_rows();
                self.sync_editor_state();
                self.sync_overlay();
                self.sync_toolbar();
                // mac-ui §7.1 / win-ui-main §6.5: the first or last line switches drop zone / line panel / player.
                self.sync_layout(false);
            }
            SubtitleChange::Translations => {
                self.sync_rows();
                self.sync_overlay();
                self.sync_pickers(false);
                self.sync_display();
            }
            SubtitleChange::Selection => {
                self.sync_rows();
                self.sync_editor_state();
                // win-ui-main §9.1 / mac-ui §7.6: a selected line seeks the player to its start.
                if let Some(uid) = self.vm.selection()
                    && let Some(sentence) = self.vm.sentences().into_iter().find(|s| s.uid == uid)
                {
                    self.player.borrow_mut().seek(sentence.start);
                    self.drain_player();
                }
            }
            SubtitleChange::Drafts => {}
            SubtitleChange::Status | SubtitleChange::Notice => {
                self.sync_status();
                self.sync_toolbar();
                self.sync_editor_state();
            }
            SubtitleChange::Undo => self.sync_editor_state(),
            SubtitleChange::Workspace => {
                self.sync_workspace();
                self.sync_toolbar();
                self.sync_status();
            }
            SubtitleChange::Layout => {
                self.sync_layout(true);
            }
            SubtitleChange::Playlist => {
                self.sync_queue();
                self.sync_toolbar();
            }
            SubtitleChange::Settings | SubtitleChange::Models | SubtitleChange::Providers => {
                self.sync_pickers(false);
                self.sync_toolbar();
                self.sync_display();
                self.sync_status();
            }
            SubtitleChange::All => self.refresh_all(true),
        }
    }

    fn on_explorer_change(&self, _change: ExplorerChange) {
        self.sync_explorer();
    }

    /// win-ui-main §9.2 `on_video_changed()`.
    fn on_video_changed(&self) {
        let video = self.vm.video();
        self.error_bar.borrow_mut().video_changed();
        // A track of the previous video being read is of no use any more; its menu (if open) goes with the button.
        self.reads.borrow_mut().cancel();
        {
            let mut st = self.st.borrow_mut();
            st.track_notice = None;
            if st.tracks_menu_open {
                st.tracks_menu_open = false;
                let interacting = st.seek_scrubbing || st.volume_scrubbing;
                drop(st);
                self.hud.borrow_mut().set_interacting(interacting, Instant::now());
            }
        }
        let auto_play = self.vm.auto_play_video();
        {
            let mut p = self.player.borrow_mut();
            p.load(video.as_deref(), auto_play);
            p.set_sentences(self.vm.sentences());
        }
        self.drain_player();
        {
            let mut st = self.st.borrow_mut();
            st.frame = None;
            st.spare_frame = None;
        }
        self.video.clear();
        if let Some(w) = self.win() {
            w.global::<MainBridge>().set_video_frame(slint::Image::default());
        }
        if let Some(ex) = &self.explorer {
            ex.sync_to_video(video.as_deref());
        }
        if let Some(hub) = &self.hub {
            // Playlist runs never count as an opened video (privacy-gated in the hub).
            hub.player_video_changed(video.as_deref(), !self.vm.is_playlist_running());
        }
        self.refresh_all(false);
    }

    // ------------------------------------------------------------------ player

    fn on_player_event(&self, ev: sa_player::PlayerEvent) {
        if self.disposed.get() {
            return;
        }
        self.player.borrow_mut().handle_event(ev);
        self.drain_player();
    }

    fn drain_player(&self) {
        let outputs = self.player.borrow_mut().take_outputs();
        if outputs.is_empty() {
            return;
        }
        let mut ticked = false;
        for o in outputs {
            match o {
                PlayerOutput::Ticked => ticked = true,
                PlayerOutput::ActiveChanged(_) => self.on_active_changed(),
                PlayerOutput::VideoSizeChanged(w, h) => {
                    if let Some(win) = self.win() {
                        win.global::<MainBridge>().set_video_aspect((w / h.max(1.0)) as f32);
                    }
                }
                PlayerOutput::FrameReady => self.present_frame(),
                PlayerOutput::Failed(details) => {
                    let make: Rc<dyn Fn() -> String> = Rc::new(move || sa_core::l!("Video oynatılamadı: %@", details.clone()));
                    self.error_bar.borrow_mut().show_save_error(make);
                    self.sync_error_bar();
                }
                PlayerOutput::FailureChanged => self.sync_status(),
                PlayerOutput::PlayEmptyRequested => self.play_empty(),
                PlayerOutput::Playback(report) => {
                    if let Some(hub) = &self.hub {
                        hub.report_playback(report);
                    }
                }
                PlayerOutput::PreparePreview { source, generation } => self.start_preview(source, generation),
                PlayerOutput::PreparingPreviewChanged(v) => {
                    if let Some(w) = self.win() {
                        w.global::<MainBridge>().set_preparing_preview(v);
                    }
                }
                PlayerOutput::TracksChanged => self.sync_tracks(),
                PlayerOutput::AudioTrackFailed(details) => self.show_track_failure(media_tracks::audio_track_failure(&details)),
            }
        }
        if ticked {
            self.sync_transport();
        }
    }

    fn start_preview(&self, source: PathBuf, generation: u64) {
        let Some(maker) = self.preview.clone() else {
            self.player.borrow_mut().preview_ready(generation, Err(String::new()));
            return;
        };
        let key = self.key.get().expect("registered");
        let ui = self.ui_dispatcher.clone();
        std::thread::spawn(move || {
            let result = maker(&source);
            key.post(&ui, move |c| {
                c.player.borrow_mut().preview_ready(generation, result);
                c.drain_player();
            });
        });
    }

    /// Shows the newest frame. Planar frames go to the GPU path (uploaded and converted at render time; nothing is
    /// copied here); RGBA frames (no GPU path: the software renderer, tests) are copied into a pixel buffer.
    fn present_frame(&self) {
        let gpu = self.video.available();
        self.sync_planar_output(gpu);
        let frame = self.player.borrow_mut().latest_frame();
        let Some(frame) = frame else { return };
        if frame.planar.is_some() {
            if gpu {
                {
                    let mut st = self.st.borrow_mut();
                    st.frame = None;
                    st.spare_frame = None;
                }
                self.video.submit(frame);
            }
            // Otherwise the GPU path just went away: the next frames arrive as RGBA.
            return;
        }
        let Some(w) = self.win() else { return };
        self.video.clear();
        let reuse = self.st.borrow_mut().spare_frame.take();
        let buf = frame_to_buffer(&frame, reuse);
        w.global::<MainBridge>().set_video_frame(slint::Image::from_rgba8(buf.clone()));
        let mut st = self.st.borrow_mut();
        st.spare_frame = st.frame.replace(buf);
    }

    /// Asks the player for planar frames exactly while the GPU path is up.
    fn sync_planar_output(&self, gpu: bool) {
        let changed = {
            let mut st = self.st.borrow_mut();
            std::mem::replace(&mut st.planar_output, gpu) != gpu
        };
        if changed {
            self.player.borrow_mut().set_planar_output(gpu);
        }
    }

    /// win-ui-main §10.6: toggle without a loaded video plays the first candidate.
    fn play_empty(&self) {
        let candidate = self
            .vm
            .video()
            .or_else(|| self.explorer.as_ref().and_then(|e| e.rows().into_iter().find(|r| r.is_active || !r.is_folder).map(|r| r.path)))
            .or_else(|| self.vm.playlist().first().map(|p| p.video_path.clone()))
            .or_else(|| self.hub.as_ref().and_then(|h| h.cards().first().map(|c| c.path.clone())));
        let Some(c) = candidate.filter(|p| p.is_file()) else { return };
        if !self.vm.video().is_some_and(|v| self.policy.same_path(&v, &c)) {
            self.vm.open_video(Some(c.clone()));
        }
        self.player.borrow_mut().load(Some(&c), true);
        self.drain_player();
    }

    fn on_active_changed(&self) {
        self.sync_active_row();
        self.sync_overlay();
        let (playing, active) = {
            let p = self.player.borrow();
            (p.is_playing(), p.active_id())
        };
        let (editing, index) = {
            // Read the state in its own scope: a `let` chain would keep this borrow alive in the body below.
            let st = self.st.borrow();
            (st.editing_text, active.and_then(|uid| st.row_uids.iter().position(|u| *u == uid)))
        };
        if playing
            && !editing
            && let Some(index) = index
            && let Some(w) = self.win()
        {
            let main = w.global::<MainBridge>();
            main.set_scroll_row(index as i32);
            let mut st = self.st.borrow_mut();
            st.scroll_epoch += 1;
            main.set_scroll_epoch(st.scroll_epoch);
        }
    }

    // ------------------------------------------------------------------ sync (VM → bridge)

    /// `refresh_all()` (win-ui-main §9.1).
    pub fn refresh_all(&self, force_pickers: bool) {
        if self.disposed.get() {
            return;
        }
        self.sync_title();
        self.sync_workspace();
        self.sync_content();
        self.sync_pickers(force_pickers);
        self.sync_toolbar();
        self.sync_status();
        {
            let failed = self.failed_message();
            self.error_bar.borrow_mut().relocalize(failed.as_ref());
        }
        self.sync_error_bar();
        self.sync_editor_state();
        self.sync_rows();
        self.sync_queue();
        self.sync_explorer();
        self.sync_layout(false);
        self.sync_display();
        self.sync_overlay_style();
        self.sync_overlay();
        self.sync_transport();
        self.sync_tracks();
        self.sync_engagement();
        self.sync_menu_bar();
    }

    fn sync_title(&self) {
        let Some(w) = self.win() else { return };
        let app = w.global::<AppBridge>();
        let title = self.vm.window_title();
        let file = self.vm.video().and_then(|v| v.file_name().map(|n| n.to_string_lossy().into_owned()));
        app.set_window_title(s(title));
        app.set_title_file(s(file.map(|n| format!("— {n}")).unwrap_or_default()));
    }

    fn sync_workspace(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        main.set_workspace(to_ui_workspace(self.vm.workspace()));
        let count = if self.mac { self.vm.list_rows().len() } else { self.vm.playlist().len() };
        main.set_playlist_count(count as i32);
        if let Some(hub) = &self.hub {
            hub.set_hub_active(self.vm.workspace() == Workspace::Platform);
        }
    }

    fn sync_content(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        main.set_has_video(self.vm.video().is_some());
        main.set_has_sentences(self.vm.has_sentences());
    }

    /// Pickers `fill` rule (win-ui-main §9.5).
    fn sync_pickers(&self, force: bool) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        let langs: Vec<PickerItem> = SubtitleLanguage::ALL.iter().map(|l| PickerItem { title: s(l.title()), enabled: true }).collect();
        fill_if_changed(&self.language_items, langs, force);
        let code = self.vm.language();
        main.set_language_index(SubtitleLanguage::ALL.iter().position(|l| l.code == code).map(|i| i as i32).unwrap_or(-1));
        let targets = SubtitleLanguage::translation_targets();
        let items: Vec<PickerItem> =
            targets.iter().map(|l| PickerItem { title: s(l.target_name()), enabled: self.vm.is_translation_target_enabled(l.code) }).collect();
        fill_if_changed(&self.target_items, items, force);
        let target = self.vm.translation_target();
        main.set_target_index(targets.iter().position(|l| l.code == target).map(|i| i as i32).unwrap_or(-1));
        main.set_target_help(s(self.vm.translation_target_help()));
        let mut st = self.st.borrow_mut();
        st.language_codes = SubtitleLanguage::ALL.iter().map(|l| l.code).collect();
        st.target_codes = targets.iter().map(|l| l.code).collect();
    }

    fn translation_model_kind(&self) -> &'static str {
        if !self.vm.is_external_llm_active() {
            ""
        } else if self.vm.active_cloud_provider_name().is_some() {
            "cloud"
        } else {
            "network"
        }
    }

    /// macOS `TranslationModelPickerButton.helpText` (title + newline + where it runs).
    fn translation_model_help(&self) -> String {
        let title = self.vm.translation_model_name();
        if !self.mac {
            return title;
        }
        let detail = match (self.vm.is_external_llm_active(), self.vm.active_cloud_provider_name()) {
            (false, _) => sa_core::l("Çeviri motoru (Gemma 4, Qwen 3.5, MADLAD, NLLB); ilk kullanımda indirilir."),
            (true, Some(name)) => sa_core::l!("Çeviri %@ sağlayıcısında yapılır: çevrilecek metin ona gönderilir, ses gönderilmez.", name),
            (true, None) => sa_core::l("Çeviriyi yerel ağdaki başka bir bilgisayara veya Mac'e (LM Studio, Ollama, llama-server) yaptırır."),
        };
        format!("{title}\n{detail}")
    }

    fn sync_toolbar(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        let vm = &self.vm;
        let working = vm.is_working();
        main.set_is_working(working);
        main.set_model_title(s(vm.model_name()));
        main.set_diarize(vm.diarize());
        main.set_translation_model_title(s(vm.translation_model_name()));
        main.set_translation_model_help(s(self.translation_model_help()));
        main.set_translation_model_kind(s(self.translation_model_kind()));
        main.set_can_translate(vm.can_translate());
        main.set_can_generate(vm.can_generate());
        main.set_can_generate_and_translate(if self.mac { vm.can_generate_and_translate() } else { !working });
        main.set_can_cancel(self.can_stop());
        main.set_can_pick_video(vm.workspace() == Workspace::Editor);
        main.set_can_pick_subtitle(!working);
        main.set_auto_save(vm.auto_save_subtitle());
        main.set_can_save_subtitle(vm.can_save_subtitle());
        main.set_can_export(vm.can_export());
        let explorer_open = self.explorer_open();
        main.set_show_file_explorer(explorer_open);
        self.sync_menu_bar();
    }

    /// Stop has something to stop. Windows: `CancelCommand` (= `IsWorking`, which includes a playlist run). macOS
    /// `canStop`: the playlist run in the list tab, otherwise the job of the video on screen.
    fn can_stop(&self) -> bool {
        if self.mac {
            if self.vm.workspace() == Workspace::Playlist { self.vm.is_playlist_running() } else { self.vm.is_working() }
        } else {
            self.vm.can_cancel()
        }
    }

    fn explorer_open(&self) -> bool {
        match &self.explorer {
            Some(e) => e.is_sidebar_visible(),
            None => self.settings.get_bool(keys::SHOW_FILE_EXPLORER).unwrap_or(defaults::SHOW_FILE_EXPLORER),
        }
    }

    fn set_explorer_open(&self, open: bool) {
        // macOS §3.3: while full screen a change is not a choice and is ignored.
        if self.mac && !super::layout::mac::sidebar_change_is_stored(self.st.borrow().full_screen) {
            return;
        }
        match &self.explorer {
            Some(e) => e.set_sidebar_visible(open),
            None => self.settings.set_bool(keys::SHOW_FILE_EXPLORER, open),
        }
        self.sync_toolbar();
        self.sync_layout(true);
    }

    fn failed_message(&self) -> Option<sa_core::l10n::LocalizedMessage> {
        match self.vm.status() {
            sa_viewmodels::types::JobStatus::Failed { message } => Some(message),
            _ => None,
        }
    }

    fn sync_status(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        let vm = &self.vm;
        let kind = vm.status_kind();
        main.set_status_kind(status_kind_index(kind));
        main.set_status_text(s(vm.status_text()));
        main.set_status_fraction(if vm.is_indeterminate() { -1.0 } else { bridge_fraction(vm.status_fraction()) });
        main.set_status_detail(s(if self.mac { vm.async_status_detail().map(|d| format!("· {d}")).unwrap_or_default() } else { String::new() }));
        let notice = vm.model_notice().map(|m| m.text()).unwrap_or_default();
        main.set_model_notice(s(notice));
        main.set_model_notice_help(s(SubtitleViewModel::model_notice_help()));
        main.set_format_index(SubtitleFormat::ALL.iter().position(|f| *f == vm.export_format()).map(|i| i as i32).unwrap_or(0));
        main.set_export_language_index(if vm.export_language() == ExportLanguage::Translated { 1 } else { 0 });
        main.set_translation_title(s(vm.translation_title()));
        let retry = vm.can_go_online_and_retry();
        main.set_retry_visible(self.mac && retry && kind == JobStatusKind::Failed);
        main.set_retry_title(s(vm.offline_retry_title()));
        main.set_badge_text(s(vm.translation_badge_text()));
        main.set_empty_title(s(self.empty_title()));
        main.set_empty_detail(s(if vm.is_working() || (self.mac && vm.is_queued()) { String::new() } else { sa_core::l("Araç çubuğundan “Altyazı üret”e bas.") }));
        main.set_empty_symbol(s(if self.mac && vm.is_queued() && !vm.has_sentences() { "clock" } else { "captions.bubble" }));
        let notice = if self.mac {
            let failure = self.player.borrow().failure_message().or_else(|| self.st.borrow().track_notice.clone());
            mac_playback_notice(failure, vm.workspace(), vm.status().kind(), vm.async_translation_fraction().is_some() || vm.is_async_translating())
                .map(|m| m.text())
                .unwrap_or_default()
        } else {
            String::new()
        };
        main.set_playback_notice(s(notice));
        if !self.mac {
            let failed = self.failed_message();
            self.error_bar.borrow_mut().update(failed.as_ref(), retry);
            self.sync_error_bar();
        }
    }

    fn empty_title(&self) -> String {
        let vm = &self.vm;
        if self.mac && vm.is_queued() && !vm.has_sentences() {
            return match vm.queue_position() {
                Some(p) => sa_core::l!("Sırada: %lld.", p as i64),
                None => sa_core::l("Sırada"),
            };
        }
        if vm.is_working() { sa_core::l("Altyazı üretiliyor…") } else { sa_core::l("Henüz altyazı yok") }
    }

    fn sync_error_bar(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        let bar = self.error_bar.borrow();
        main.set_error_open(bar.open);
        main.set_error_text(s(bar.message.clone()));
        main.set_go_online_visible(bar.go_online_visible);
    }

    fn sync_editor_state(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        let vm = &self.vm;
        let now = self.player.borrow().current_time();
        let single = !self.policy.split_merge_timing_need_single_selection || vm.has_single_selection();
        main.set_selected_start_text(s(vm.selected_edge_text(TimingEdge::Start)));
        main.set_selected_end_text(s(vm.selected_edge_text(TimingEdge::End)));
        main.set_timing_enabled(vm.selection().is_some() && single && (!self.mac || !vm.is_working()));
        main.set_can_split(vm.can_split_selected(now));
        main.set_can_merge(vm.can_merge_selected());
        main.set_can_delete(vm.can_delete_selected());
        main.set_can_retranslate(vm.can_translate_selected());
        main.set_can_undo(vm.can_undo());
        main.set_can_redo(vm.can_redo());
        main.set_undo_title(s(vm.undo_title()));
        main.set_redo_title(s(vm.redo_title()));
        main.set_has_translations(vm.has_translations());
        self.sync_menu_bar();
    }

    /// Rows (keyed by `Sentence.uid`, win-ui-main §9.3).
    fn sync_rows(&self) {
        let Some(w) = self.win() else { return };
        let dark = w.global::<ui::Palette>().get_color_scheme() == slint::language::ColorScheme::Dark;
        // One state read for every line (per-line getters would scan the lines once per row).
        let lines = self.vm.line_rows();
        let active = self.player.borrow().active_id();
        let shows_speakers = lines.iter().any(|l| l.speaker.is_some());
        // Speaker titles repeat; format each once.
        let mut speaker_titles: std::collections::HashMap<Option<i64>, SharedString> = std::collections::HashMap::new();
        let mut uids = Vec::with_capacity(lines.len());
        let rows: Vec<SentenceRowData> = lines
            .into_iter()
            .map(|line| {
                uids.push(line.uid);
                let speaker_text = speaker_titles
                    .entry(line.speaker)
                    .or_insert_with(|| {
                        s(match (line.speaker, self.mac) {
                            (Some(sp), true) => menus::speaker_title(sp),
                            (None, true) => sa_core::l("Konuşmacı yok"),
                            (sp, false) => menus::compact_speaker_title(sp),
                        })
                    })
                    .clone();
                let color = line.speaker.map(|sp| {
                    let c = SpeakerPalette::color(sp, dark);
                    slint::Color::from_rgb_u8(c.r, c.g, c.b)
                });
                SentenceRowData {
                    uid: s(line.uid.to_string()),
                    number: s(line.id.to_string()),
                    time_text: s(format!("{} → {}", subtitle_time_short(line.start), subtitle_time_short(line.end))),
                    accessible_label: s(format!("{}. {}", line.id, line.text)),
                    text: s(line.text),
                    has_speaker: line.speaker.is_some(),
                    speaker_text,
                    speaker_color: color.unwrap_or(slint::Color::from_argb_u8(0, 0, 0, 0)),
                    has_translation: line.translation.is_some(),
                    translation_text: s(line.translation.unwrap_or_default()),
                    is_stale: line.is_stale,
                    needs_review: line.needs_review,
                    is_active: active == Some(line.uid),
                    is_selected: line.is_selected,
                }
            })
            .collect();
        sync_keyed(&self.rows, rows, |r| r.uid.clone());
        w.global::<MainBridge>().set_shows_speakers(shows_speakers);
        let mut st = self.st.borrow_mut();
        st.row_uids = uids;
        st.active_row = active;
        st.row_counters.full_syncs += 1;
    }

    /// Only the playing line changed: flips `is_active` on the row that had it and the row that gets it, with no
    /// rebuild (a full `sync_rows` formats every row, which stalls the UI thread at every line change on long
    /// subtitles). A row whose key does not match the last full sync is left to the next one.
    fn sync_active_row(&self) {
        let active = self.player.borrow().active_id();
        let targets = {
            let mut st = self.st.borrow_mut();
            let previous = std::mem::replace(&mut st.active_row, active);
            if previous == active {
                return;
            }
            let find = |uid: Option<Uuid>| uid.and_then(|u| st.row_uids.iter().position(|x| *x == u).map(|i| (u, i)));
            [(find(previous), false), (find(active), true)]
        };
        let mut touched = 0;
        for (target, flag) in targets {
            let Some((uid, index)) = target else { continue };
            let Some(mut row) = slint::Model::row_data(&*self.rows, index) else { continue };
            if row.is_active == flag || row.uid.as_str() != uid.to_string() {
                continue;
            }
            row.is_active = flag;
            slint::Model::set_row_data(&*self.rows, index, row);
            touched += 1;
        }
        self.st.borrow_mut().row_counters.active_row_updates += touched;
    }

    /// Diagnostics (tests): how often the line list was rebuilt and how many rows active-line changes rewrote.
    #[doc(hidden)]
    pub fn row_sync_counters(&self) -> RowSyncCounters {
        self.st.borrow().row_counters
    }

    fn sync_queue(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        let vm = &self.vm;
        let running = vm.is_playlist_running();
        let mut ids = Vec::new();
        let rows: Vec<QueueRowData> = if self.mac {
            vm.list_rows()
                .into_iter()
                .map(|r| {
                    ids.push(r.id);
                    let is_item = matches!(r.origin, sa_viewmodels::types::ListRowOrigin::Playlist(_));
                    let source = SubtitleLanguage::from_code(&r.source_language).map(|l| l.title()).unwrap_or_else(|| sa_core::l("Otomatik"));
                    QueueRowData {
                        id: s(r.id.to_string()),
                        file_name: s(r.title.clone()),
                        folder_path: s(r.video_path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()),
                        source_language_title: s(source.clone()),
                        status_text: s(r.status_text.clone()),
                        state: state_index(r.state),
                        audio_ready: r.is_audio_ready,
                        indeterminate: r.state == PlaylistItemState::Working && r.fraction.is_none(),
                        progress: r.fraction.filter(|f| f.is_finite()).unwrap_or(0.0).clamp(0.0, 1.0) as f32,
                        can_edit: r.state != PlaylistItemState::Working && !running,
                        can_stop: r.can_stop,
                        is_playlist_item: is_item,
                        accessible_label: s(format!("{} ({source}) — {}", r.title, r.status_text)),
                    }
                })
                .collect()
        } else {
            vm.playlist()
                .into_iter()
                .map(|item| {
                    ids.push(item.id);
                    let source = SubtitleLanguage::from_code(&item.source_language).map(|l| l.title()).unwrap_or_else(|| sa_core::l("Otomatik"));
                    let status = item.status_text();
                    QueueRowData {
                        id: s(item.id.to_string()),
                        file_name: s(item.file_name()),
                        folder_path: s(item.folder_path()),
                        source_language_title: s(source.clone()),
                        status_text: s(status.clone()),
                        state: state_index(item.state),
                        audio_ready: item.is_audio_ready(),
                        indeterminate: item.state == PlaylistItemState::Working && item.fraction.is_none(),
                        progress: item.fraction.filter(|f| f.is_finite()).unwrap_or(0.0).clamp(0.0, 1.0) as f32,
                        can_edit: item.is_editable() && !running,
                        can_stop: false,
                        is_playlist_item: true,
                        accessible_label: s(format!("{} ({source}) — {status}", item.file_name())),
                    }
                })
                .collect()
        };
        let has_items = !vm.playlist().is_empty();
        sync_keyed(&self.queue_rows, rows, |r| r.id.clone());
        main.set_has_playlist_items(has_items);
        main.set_has_list_rows(!ids.is_empty());
        main.set_playlist_summary(s(vm.playlist_summary_text()));
        main.set_can_start_playlist(vm.can_start_playlist());
        main.set_is_playlist_running(running);
        main.set_can_clear_queue(vm.queued_job_count() > 0);
        let count = if self.mac { ids.len() } else { vm.playlist().len() };
        main.set_playlist_count(count as i32);
        match vm.playlist_notice() {
            Some(n) => {
                main.set_playlist_notice(s(n.message.text()));
                main.set_playlist_notice_retry(n.has_retry);
                main.set_playlist_notice_retry_title(s(vm.offline_retry_title()));
            }
            None => {
                main.set_playlist_notice(SharedString::new());
                main.set_playlist_notice_retry(false);
            }
        }
        self.st.borrow_mut().queue_ids = ids;
    }

    fn sync_explorer(&self) {
        let Some(w) = self.win() else { return };
        let ex_bridge = w.global::<ExplorerBridge>();
        let Some(ex) = &self.explorer else { return };
        let folder = ex.current_folder();
        ex_bridge.set_current_folder(s(ex.current_folder_name()));
        ex_bridge.set_current_path(s(folder.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()));
        ex_bridge.set_is_scanning(ex.is_scanning());
        ex_bridge.set_can_go_back(ex.can_go_back());
        ex_bridge.set_can_go_up(ex.can_go_up());
        ex_bridge.set_can_open_folder(folder.is_some());
        ex_bridge.set_status_text(s(ex.status_text()));
        ex_bridge.set_search_text(s(ex.search_query()));
        let searching = !ex.search_query().trim().is_empty();
        let (empty, symbol) = match (searching, ex.availability()) {
            (true, _) => (sa_core::l("Eşleşen medya dosyası bulunamadı"), "magnifyingglass"),
            (false, sa_viewmodels::explorer::FolderAvailability::Available) => (sa_core::l("Bu klasörde medya dosyası bulunamadı"), "folder"),
            (false, sa_viewmodels::explorer::FolderAvailability::NotFound) => (sa_core::l("Klasör bulunamadı"), "questionmark.folder"),
            (false, sa_viewmodels::explorer::FolderAvailability::Unreadable) => (sa_core::l("Klasör okunamadı"), "lock"),
            #[allow(unreachable_patterns)]
            (false, _) => (sa_core::l("Bu birim bağlı değil"), "externaldrive.badge.xmark"),
        };
        ex_bridge.set_empty_text(s(if self.mac { empty } else { sa_core::l("Bu klasörde medya dosyası bulunamadı") }));
        ex_bridge.set_empty_symbol(s(symbol));
        let current_lower = folder.as_ref().map(|f| f.to_string_lossy().to_lowercase());
        let mut paths = Vec::new();
        let mut folders = Vec::new();
        let rows: Vec<ExplorerRowData> = ex
            .rows()
            .into_iter()
            .map(|r| {
                paths.push(r.path.clone());
                folders.push(r.is_folder);
                let parent_differs = r.path.parent().map(|p| p.to_string_lossy().to_lowercase()) != current_lower;
                let mut detail = r.size_text.clone();
                if !r.is_folder && parent_differs
                    && let Some(dir) = r.path.parent().and_then(|p| p.file_name())
                {
                    detail = format!("{detail} · {}", dir.to_string_lossy());
                }
                let is_audio = r.path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    sa_core::media_types::AUDIO_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(e))
                });
                ExplorerRowData {
                    path: s(r.path.to_string_lossy().into_owned()),
                    name: s(r.name.clone()),
                    is_folder: r.is_folder,
                    is_audio,
                    detail: s(detail.clone()),
                    badge: s(r.sidecar_badge.clone().unwrap_or_default()),
                    is_active: r.is_active,
                    accessible_label: s(if r.is_folder { r.name.clone() } else if self.mac { r.name.clone() } else { format!("{} ({})", r.name, r.size_text) }),
                }
            })
            .collect();
        sync_keyed(&self.explorer_rows, rows, |r| r.path.clone());
        let mut st = self.st.borrow_mut();
        st.explorer_paths = paths;
        st.explorer_folders = folders;
    }

    fn layout_flags(&self) -> LayoutFlags {
        LayoutFlags {
            explorer_open: self.explorer_open(),
            editor_hidden: self.vm.hide_subtitle_panel(),
            side_by_side: self.vm.side_by_side_layout(),
            theater: self.vm.is_theater_mode(),
            full_screen: self.st.borrow().full_screen,
        }
    }

    /// `apply_workspace_layout()` (win-ui-main §8) / mac-ui §7.1 content selection. Every re-apply resets the
    /// splitter proportions (`layout-epoch`).
    fn sync_layout(&self, reset_splitter: bool) {
        let Some(w) = self.win() else { return };
        let f = self.layout_flags();
        let has_video = self.vm.video().is_some();
        let has_lines = self.vm.has_sentences();
        let main = w.global::<MainBridge>();
        let app = w.global::<AppBridge>();
        let mode = if self.mac {
            let content = mac_editor_content(has_video, has_lines, f);
            main.set_editor_hidden(f.editor_hidden);
            content.layout_mode()
        } else {
            let l = windows_layout(f);
            main.set_editor_hidden(f.editor_hidden);
            l.mode
        };
        let _ = MacEditorContent::DropZone;
        main.set_layout_mode(mode as i32);
        main.set_side_by_side(f.side_by_side);
        main.set_show_file_explorer(f.explorer_open);
        app.set_is_full_screen(f.full_screen);
        app.set_is_theater_mode(f.theater);
        let changed = self.st.borrow().last_layout != Some((f, has_video, has_lines));
        if reset_splitter || changed {
            let mut st = self.st.borrow_mut();
            st.last_layout = Some((f, has_video, has_lines));
            st.layout_epoch += 1;
            main.set_layout_epoch(st.layout_epoch);
        }
        let _ = LayoutMode::Stacked;
        self.sync_menu_bar();
    }

    fn sync_display(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        let has = self.vm.has_translations();
        let mode = if has { self.vm.effective_display_mode() } else { SubtitleDisplay::Original };
        main.set_display_mode(match mode {
            SubtitleDisplay::Original => 0,
            SubtitleDisplay::Translation => 1,
            SubtitleDisplay::Both => 2,
        });
        main.set_has_translations(has);
        main.set_translation_title(s(self.vm.translation_title()));
    }

    fn sync_overlay_style(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        let style = self.appearance.as_ref().map(|a| a.style()).unwrap_or_else(sa_core::appearance::OverlayStyle::standard);
        main.set_overlay_font_family(s(style.font_family.clone()));
        main.set_overlay_size_fraction(style.size_fraction as f32);
        let c = style.text_color();
        main.set_overlay_color(slint::Color::from_rgb_u8(c.r, c.g, c.b));
        main.set_overlay_background(match style.background {
            OverlayBackground::Box => 0,
            OverlayBackground::Shadow => 1,
            OverlayBackground::Outline => 2,
        });
        main.set_overlay_opacity(style.background_opacity as f32);
        if let Some(a) = &self.appearance {
            w.global::<AppBridge>().set_ui_font_family(s(a.ui_font_family()));
        }
    }

    fn sync_overlay(&self) {
        let Some(w) = self.win() else { return };
        let active = self.player.borrow().active_id();
        // Composed inside the view model: no copy of every line per active-line change.
        let text = active.and_then(|uid| self.vm.overlay_text(uid));
        w.global::<MainBridge>().set_overlay_text(s(text.unwrap_or_default()));
    }

    /// The track control (shown while the file has a choice; busy while a track is read) and, while it is open, its
    /// menu (it follows switches, failures and the read's progress).
    fn sync_tracks(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        main.set_tracks_available(media_tracks::has_choices(self.player.borrow().tracks()));
        main.set_tracks_busy(self.reads.borrow().is_reading());
        let showing = {
            let st = self.st.borrow();
            st.tracks_menu_open && st.last_menu == "tracks"
        };
        if showing {
            let entries = self.track_menu_entries();
            self.menu_entries.set_vec(entries.into_iter().map(to_menu_entry).collect::<Vec<_>>());
        }
    }

    fn track_menu_entries(&self) -> Vec<Entry> {
        let loaded = self.loaded_embedded_track();
        let p = self.player.borrow();
        let menu = TrackMenu::build(p.tracks(), self.reads.borrow().reading(), loaded);
        menus::track_menu(&menu, self.mac)
    }

    fn tracks_menu_opened(&self, open: bool) {
        self.st.borrow_mut().tracks_menu_open = open;
        let interacting = {
            let st = self.st.borrow();
            open || st.seek_scrubbing || st.volume_scrubbing
        };
        self.hud.borrow_mut().set_interacting(interacting, Instant::now());
        self.sync_hud();
    }

    fn choose_track(self: &Rc<Self>, choice: TrackChoice) {
        match choice {
            TrackChoice::Audio(stream) => {
                self.player.borrow_mut().select_audio_track(stream);
                self.drain_player();
            }
            TrackChoice::Subtitle(stream) => {
                // The track whose lines the list already holds: nothing to do.
                if self.loaded_embedded_track() == Some(stream) {
                    return;
                }
                self.read_embedded_subtitle(stream)
            }
        }
    }

    /// The embedded track whose lines the list holds: the subtitle file imported last is that track's document.
    fn loaded_embedded_track(&self) -> Option<usize> {
        let p = self.player.borrow();
        let (file, loaded) = (p.playing_file()?, self.vm.loaded_subtitle()?);
        let work = self.shell.engine_work_dir();
        p.tracks().subtitles.iter().map(|t| t.stream_index).find(|i| media_tracks::embedded_subtitle_path(&work, file, *i) == loaded)
    }

    /// Reads embedded text track `stream` on a worker thread (its own demuxer: playback is not touched), writes it
    /// as a JSON sentence file into the work folder and imports that file exactly like a picked subtitle file, unless
    /// the session changed while it was read.
    fn read_embedded_subtitle(self: &Rc<Self>, stream: usize) {
        let (file, is_text) = {
            let p = self.player.borrow();
            (p.playing_file().map(Path::to_path_buf), p.tracks().subtitles.iter().any(|t| t.stream_index == stream && t.is_text()))
        };
        let Some(file) = file.filter(|_| is_text) else { return };
        let Some(ticket) = self.reads.borrow_mut().start(&file, stream, self.vm.session_mark()) else { return };
        self.sync_tracks();
        let target = media_tracks::embedded_subtitle_path(&self.shell.engine_work_dir(), &file, stream);
        let reader = self.subtitle_reader.clone();
        let key = self.key.get().expect("registered");
        let ui = self.ui_dispatcher.clone();
        let spawned = std::thread::Builder::new().name("sa-embedded-subtitles".into()).spawn(move || {
            let id = ticket.id;
            let mut last = -1.0f64;
            let progress_ui = ui.clone();
            let mut progress = |f: f64| {
                // At most every 2 %: each step re-renders the open menu.
                if f - last >= 0.02 || f >= 1.0 {
                    last = f;
                    key.post(&progress_ui, move |c| c.embedded_read_progress(id, f));
                }
            };
            let result = reader(&file, stream, &ticket.token, &mut progress).and_then(|doc| {
                let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
                std::fs::create_dir_all(&dir)
                    .and_then(|()| std::fs::write(&target, doc.text.as_bytes()))
                    .map(|()| target)
                    .map_err(|e| ExtractError::Failed(e.to_string()))
            });
            key.post(&ui, move |c| c.embedded_read_done(id, result));
        });
        if let Err(e) = spawned {
            self.embedded_read_done(ticket.id, Err(ExtractError::Failed(e.to_string())));
        }
    }

    fn embedded_read_progress(&self, id: u64, fraction: f64) {
        if !self.disposed.get() && self.reads.borrow_mut().progress(id, fraction) {
            self.sync_tracks();
        }
    }

    fn embedded_read_done(&self, id: u64, result: Result<PathBuf, ExtractError>) {
        if self.disposed.get() {
            return;
        }
        let current = self.reads.borrow_mut().finish(id);
        self.sync_tracks();
        if let Ok(path) = &result {
            // Deleted with the session's other scratch files on exit.
            self.vm.register_temp_file(path.clone());
        }
        // Cancelled or replaced meanwhile (another track, another video): dropped.
        let Some(read) = current else { return };
        match result {
            // Importing replaces the lines, clears undo and stops a job: not behind the user's back when they edited
            // or a job started or ended during the read.
            Ok(_) if !read.still_applies(self.vm.session_mark()) => self.show_track_failure(media_tracks::embedded_read_outdated()),
            Ok(path) => self.import_subtitle_file(path),
            Err(e) => {
                if let Some(message) = media_tracks::embedded_read_failure(&e) {
                    self.show_track_failure(message);
                }
            }
        }
    }

    /// A failure of the track menu: the error bar on Windows (like a playback or save error); on macOS a
    /// dismissable notice in the status bar like a playback failure (the session's status and summary stay).
    fn show_track_failure(&self, message: sa_core::l10n::LocalizedMessage) {
        if self.mac {
            self.st.borrow_mut().track_notice = Some(message);
            self.sync_status();
        } else {
            let make: Rc<dyn Fn() -> String> = Rc::new(move || message.text());
            self.error_bar.borrow_mut().show_save_error(make);
            self.sync_error_bar();
        }
    }

    /// Imports a subtitle file the way the open-subtitle command does (macOS asks first when work would be lost;
    /// Windows opens a companion video first when none is open, then loads).
    fn import_subtitle_file(&self, path: PathBuf) {
        if self.mac {
            self.vm.request_open_subtitle(path);
        } else {
            // win-ui-main §18.2: a companion video opens first when no video is open.
            if self.vm.video().is_none()
                && let Some(v) = sa_core::subtitle_import::find_companion_video_windows(&path)
            {
                self.vm.open_video(Some(v));
            }
            self.vm.load_subtitle_file(&path);
        }
    }

    fn sync_transport(&self) {
        let Some(w) = self.win() else { return };
        let main = w.global::<MainBridge>();
        let (input, playing) = {
            let p = self.player.borrow();
            let st = self.st.borrow();
            (
                TransportInput {
                    position: p.current_time(),
                    duration: p.duration(),
                    is_playing: p.is_playing(),
                    is_muted: p.is_muted(),
                    volume: p.volume(),
                    buffered_ratio: p.buffered_ratio(),
                    seek_scrubbing: st.seek_scrubbing,
                    scrub_ratio: st.scrub_ratio,
                    volume_scrubbing: st.volume_scrubbing,
                },
                p.is_playing(),
            )
        };
        let v = synchronize(input, self.mac);
        main.set_seek_interactive(v.interactive);
        if let Some(r) = v.seek_ratio {
            main.set_seek_ratio(r as f32);
        }
        let old = main.get_buffered_ratio() as f64;
        if (old - v.buffered_ratio).abs() >= super::transport::BUFFERED_EPSILON || v.buffered_ratio == 0.0 {
            main.set_buffered_ratio(v.buffered_ratio as f32);
        }
        main.set_elapsed_text(s(v.elapsed));
        main.set_total_text(s(v.total));
        main.set_remaining_text(s(v.remaining));
        main.set_is_playing(playing);
        main.set_is_muted(input.is_muted);
        if let Some(vr) = v.volume_ratio {
            main.set_volume(vr as f32);
        }
        // Split follows the playback position (win-ui-main §6.5.5).
        main.set_can_split(self.vm.can_split_selected(input.position));
        self.hud.borrow_mut().set_playing(playing);
        self.sync_hud();
    }

    fn sync_hud(&self) {
        let Some(w) = self.win() else { return };
        let v = self.hud.borrow().visibility();
        let main = w.global::<MainBridge>();
        main.set_hud_visible(v.transport);
        main.set_picker_visible(v.pickers);
    }

    fn sync_engagement(&self) {
        let Some(w) = self.win() else { return };
        // Same rule as `HubBridge.engagement-visible` (logged in and a video open in the player; rule 6).
        let visible = self.hub.as_ref().is_some_and(|h| h.is_logged_in() && h.engagement().visible);
        w.global::<MainBridge>().set_engagement_visible(visible);
    }

    /// macOS menu bar state (mac-ui §4).
    fn sync_menu_bar(&self) {
        if !self.mac {
            return;
        }
        let Some(w) = self.win() else { return };
        let m = w.global::<MenuBridge>();
        let vm = &self.vm;
        let editor = vm.workspace() == Workspace::Editor;
        m.set_can_open_video(editor);
        m.set_can_generate(vm.video().is_some() && !vm.is_working() && editor);
        m.set_can_stop(self.can_stop());
        m.set_theater_mode(vm.is_theater_mode());
        m.set_side_by_side(vm.side_by_side_layout());
        m.set_panel_hidden(vm.hide_subtitle_panel());
        m.set_can_toggle_panel(vm.is_player_shown());
        m.set_can_undo(vm.can_undo());
        m.set_can_redo(vm.can_redo());
        let title = |base: &str, with: &str, key: Option<String>| match key {
            Some(k) => sa_core::l!(with, sa_core::l(&k)),
            None => sa_core::l(base),
        };
        m.set_undo_title(s(title("Geri Al", "Geri Al: %@", vm.undo_action_key())));
        m.set_redo_title(s(title("Yinele", "Yinele: %@", vm.redo_action_key())));
    }

    // ------------------------------------------------------------------ callbacks (UI → VM)

    fn wire_callbacks(self: &Rc<Self>, w: &MainWindow) {
        let main = w.global::<MainBridge>();
        let app = w.global::<AppBridge>();
        macro_rules! on {
            ($target:expr, $setter:ident, |$c:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = self.weak();
                $target.$setter(move |$($arg),*| {
                    if let Some($c) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        macro_rules! on_ret {
            ($target:expr, $setter:ident, $default:expr, |$c:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = self.weak();
                $target.$setter(move |$($arg),*| match weak.upgrade() {
                    Some($c) => $body,
                    None => $default,
                });
            }};
        }

        // Window / chrome.
        on!(app, on_open_settings, |c| c.open_settings());
        on!(app, on_minimize_requested, |c| c.minimize_window());
        on!(app, on_maximize_toggle_requested, |c| c.toggle_maximize_window());
        on!(app, on_drag_requested, |c| c.drag_window());
        on!(app, on_title_double_clicked, |c| c.toggle_maximize_window());
        on!(app, on_close_requested, |c| match &c.hooks.close_window {
            Some(h) => h(),
            None => {
                if let Some(w) = c.win() {
                    let _ = w.hide();
                }
            }
        });

        // Toolbar.
        on!(main, on_pick_video, |c| c.choose_video(false));
        on!(main, on_pick_subtitle, |c| c.choose_subtitle());
        on!(main, on_generate, |c| {
            c.vm.finish_editing();
            c.vm.generate();
        });
        on!(main, on_generate_and_translate, |c| c.generate_and_translate());
        on!(main, on_cancel, |c| c.vm.cancel());
        on!(main, on_translate, |c| {
            c.vm.finish_editing();
            c.vm.translate();
        });
        on!(main, on_save_subtitle, |c| {
            c.vm.save_subtitle_next_to_video();
        });
        on!(main, on_export_subtitle, |c| c.export());
        on!(main, on_workspace_selected, |c, w| c.vm.set_workspace(from_ui_workspace(w)));
        on!(main, on_file_explorer_toggled, |c, v| c.set_explorer_open(v));
        on!(main, on_language_selected, |c, i| {
            let code = c.st.borrow().language_codes.get(i as usize).copied();
            if let Some(code) = code {
                c.vm.set_language(code);
            }
        });
        on!(main, on_target_selected, |c, i| {
            let code = c.st.borrow().target_codes.get(i as usize).copied();
            if let Some(code) = code
                && c.vm.is_translation_target_enabled(code)
            {
                c.vm.set_translation_target(code);
            }
            c.sync_pickers(false);
        });
        on!(main, on_auto_save_toggled, |c, v| c.vm.set_auto_save_subtitle(v));
        on!(main, on_translation_model_clicked, |c| if let Some(h) = &c.hooks.translation_model_clicked { h() });
        on!(main, on_profile_clicked, |c| if let Some(h) = &c.hooks.profile_clicked { h() });

        // Menus.
        on!(main, on_menu_requested, |c, id| c.open_menu(id.as_str()));
        on!(main, on_menu_activated, |c, id| c.menu_activated(id.as_str()));

        // Error bar.
        on!(main, on_error_closed, |c| {
            c.error_bar.borrow_mut().close();
            c.sync_error_bar();
        });
        on!(main, on_go_online, |c| c.vm.go_online_and_retry());

        // Layout toggles.
        on!(main, on_toggle_subtitle_panel, |c| c.toggle_panel());
        on!(main, on_toggle_side_by_side, |c| c.vm.set_side_by_side_layout(!c.vm.side_by_side_layout()));
        on!(main, on_toggle_theater, |c| c.vm.set_theater_mode(!c.vm.is_theater_mode()));
        on!(main, on_toggle_full_screen, |c| c.toggle_full_screen());

        // Player surface.
        on!(main, on_player_pointer, |c, inside| {
            {
                let mut h = c.hud.borrow_mut();
                if inside { h.pointer_entered(Instant::now()) } else { h.pointer_exited() }
            }
            c.sync_hud();
        });
        on!(main, on_player_pointer_moved, |c| {
            c.hud.borrow_mut().pointer_moved(Instant::now());
            c.sync_hud();
        });
        on!(main, on_picker_pointer, |c, inside| {
            {
                let mut h = c.hud.borrow_mut();
                if inside { h.picker_entered(Instant::now()) } else { h.picker_exited(Instant::now()) }
            }
            c.sync_hud();
        });
        on!(main, on_video_clicked, |c, count| {
            for a in video_click_delta(count.max(0) as u32) {
                c.perform_shortcut(a, false);
            }
        });
        on!(main, on_video_wheel, |c, delta, precise| c.video_wheel(delta as f64, precise));

        // Transport.
        on!(main, on_play_pause, |c| {
            c.player.borrow_mut().toggle_playback();
            c.drain_player();
        });
        on!(main, on_seek_scrub, |c, scrubbing, ratio| c.seek_scrub(scrubbing, ratio as f64));
        on!(main, on_toggle_mute, |c| {
            c.player.borrow_mut().toggle_mute();
            c.drain_player();
        });
        on!(main, on_volume_scrub, |c, scrubbing, ratio| {
            {
                let mut st = c.st.borrow_mut();
                st.volume_scrubbing = scrubbing;
            }
            let interacting = scrubbing || c.st.borrow().seek_scrubbing || c.st.borrow().tracks_menu_open;
            c.hud.borrow_mut().set_interacting(interacting, Instant::now());
            c.player.borrow_mut().set_volume(ratio as f64);
            c.drain_player();
        });
        on!(main, on_tracks_menu_open, |c, open| c.tracks_menu_opened(open));
        on!(main, on_volume_wheel, |c, delta| {
            let (v, m) = {
                let p = c.player.borrow();
                (p.volume(), p.is_muted())
            };
            if let Some(volume) = windows_wheel_volume(v, m, delta as f64) {
                c.player.borrow_mut().set_volume(volume);
                c.drain_player();
            }
        });

        // Display picker.
        on!(main, on_display_mode_selected, |c, i| {
            let mode = match i {
                1 => SubtitleDisplay::Translation,
                2 => SubtitleDisplay::Both,
                _ => SubtitleDisplay::Original,
            };
            if mode == SubtitleDisplay::Original || c.vm.has_translations() {
                c.vm.set_display_mode(mode);
            }
            c.sync_display();
            c.sync_overlay();
        });

        // Line editor.
        on!(main, on_row_pressed, |c, index, toggle, range| c.row_pressed(index, toggle, range));
        on!(main, on_row_context, |c, index| c.select_row_at(index));
        on!(main, on_speaker_clicked, |c, index| c.select_row_at(index));
        on!(main, on_text_focus, |c, index, translation, focused| c.text_focus(index, translation, focused));
        on!(main, on_text_edited, |c, index, translation, text| {
            if let Some(uid) = c.uid_at(index) {
                if translation { c.vm.set_translation_draft(uid, text.as_str()) } else { c.vm.set_draft(uid, text.as_str()) }
            }
        });
        on!(main, on_text_commit, |c, _index, translation| {
            if translation { c.vm.commit_translation_draft() } else { c.vm.commit_draft() }
        });
        on!(main, on_nudge, |c, edge, delta| {
            c.vm.nudge(if edge == 0 { TimingEdge::Start } else { TimingEdge::End }, delta as f64);
        });
        on!(main, on_set_now, |c, edge| {
            let now = c.player.borrow().current_time();
            c.vm.set_edge_to_now(if edge == 0 { TimingEdge::Start } else { TimingEdge::End }, now);
        });
        on!(main, on_split, |c| {
            c.vm.finish_editing();
            let now = c.player.borrow().current_time();
            c.vm.split_selected(now);
        });
        on!(main, on_merge, |c| {
            c.vm.finish_editing();
            c.vm.merge_selected_with_next();
        });
        on!(main, on_delete_lines, |c| {
            c.vm.finish_editing();
            c.vm.delete_selected();
        });
        on!(main, on_retranslate, |c| {
            c.vm.finish_editing();
            c.vm.translate_selected();
        });
        on!(main, on_undo, |c| {
            c.vm.finish_editing();
            c.vm.undo();
        });
        on!(main, on_redo, |c| {
            c.vm.finish_editing();
            c.vm.redo();
        });
        on_ret!(main, on_list_key, false, |c, key, shift| c.list_key(key.as_str(), shift));
        on!(main, on_editing_changed, |c, editing| c.st.borrow_mut().editing_text = editing);
        on!(main, on_list_focus_changed, |c, focused| c.st.borrow_mut().list_focused = focused);

        // Status bar.
        on!(main, on_format_selected, |c, i| {
            if let Some(f) = SubtitleFormat::ALL.get(i as usize) {
                c.vm.set_export_format(*f);
            }
        });
        on!(main, on_export_language_selected, |c, i| {
            if i == 0 || c.vm.has_translations() {
                c.vm.set_export_language(if i == 1 { ExportLanguage::Translated } else { ExportLanguage::Original });
            }
            c.sync_status();
        });
        on!(main, on_retry, |c| c.vm.go_online_and_retry());
        on!(main, on_dismiss_playback_notice, |c| {
            c.st.borrow_mut().track_notice = None;
            c.player.borrow_mut().dismiss_failure();
            c.drain_player();
            c.sync_status();
        });

        // Playlist.
        on!(main, on_add_playlist_videos, |c| c.add_playlist_videos());
        on!(main, on_clear_playlist, |c| c.vm.clear_playlist());
        on!(main, on_clear_queue, |c| c.vm.clear_queue());
        on!(main, on_start_playlist, |c| {
            c.vm.finish_editing();
            c.vm.start_playlist();
        });
        on!(main, on_queue_row_clicked, |c, i| c.queue_row_clicked(i));
        on!(main, on_queue_row_remove, |c, i| c.queue_row_remove(i));
        on!(main, on_playlist_notice_closed, |c| {
            c.sync_queue();
        });
        on!(main, on_playlist_notice_retried, |c| c.vm.go_online_and_retry_playlist());

        // Keyboard (tunneling root handler).
        on_ret!(main, on_key_pressed, false, |c, text, shift, control, alt, meta, repeat| {
            c.key_pressed(SlintKey { text: text.to_string(), shift, control, alt, meta, repeat })
        });

        // Explorer.
        let ex = w.global::<ExplorerBridge>();
        on!(ex, on_row_clicked, |c, i| c.explorer_row_clicked(i as usize, false));
        on!(ex, on_row_context, |_c, _i| {});
        on!(ex, on_go_back, |c| if let Some(e) = &c.explorer { e.go_back() });
        on!(ex, on_go_up, |c| if let Some(e) = &c.explorer { e.go_up() });
        on!(ex, on_refresh, |c| if let Some(e) = &c.explorer { e.refresh() });
        on!(ex, on_open_folder, |c| {
            if let Some(f) = c.explorer.as_ref().and_then(|e| e.current_folder()) {
                c.shell.open_folder(&f);
            }
        });
        on!(ex, on_search_edited, |c, q| {
            let logged_in = c.hub.as_ref().is_some_and(|h| h.is_logged_in());
            if let Some(e) = &c.explorer {
                e.set_search(q.as_str(), logged_in);
            }
        });

        // macOS menu bar.
        if self.mac {
            let m = w.global::<MenuBridge>();
            on!(m, on_new_window, |c| if let Some(h) = &c.hooks.new_window { h() });
            on!(m, on_open_video, |c| c.choose_video(false));
            on!(m, on_generate, |c| {
                if c.vm.video().is_some() && !c.vm.is_working() && c.vm.workspace() == Workspace::Editor {
                    c.vm.finish_editing();
                    c.vm.generate();
                }
            });
            on!(m, on_stop, |c| c.vm.cancel());
            on!(m, on_undo, |c| {
                if !c.st.borrow().editing_text {
                    c.vm.finish_editing();
                    c.vm.undo();
                }
            });
            on!(m, on_redo, |c| {
                if !c.st.borrow().editing_text {
                    c.vm.finish_editing();
                    c.vm.redo();
                }
            });
            on!(m, on_toggle_sidebar, |c| c.set_explorer_open(!c.explorer_open()));
            on!(m, on_toggle_full_screen, |c| c.toggle_full_screen());
            on!(m, on_toggle_theater, |c| c.vm.set_theater_mode(!c.vm.is_theater_mode()));
            on!(m, on_toggle_side_by_side, |c| {
                if !c.vm.hide_subtitle_panel() {
                    c.vm.set_side_by_side_layout(!c.vm.side_by_side_layout());
                }
            });
            on!(m, on_toggle_panel, |c| {
                if c.vm.is_player_shown() {
                    c.vm.toggle_subtitle_panel();
                }
            });
            on!(m, on_delete, |c| {
                if !c.st.borrow().editing_text && c.st.borrow().list_focused {
                    c.vm.finish_editing();
                    c.vm.delete_selected();
                }
            });
            on!(m, on_close_window, |c| match &c.hooks.close_window {
                Some(h) => h(),
                None => {
                    if let Some(w) = c.win() {
                        let _ = w.hide();
                    }
                }
            });
            on!(m, on_minimize, |c| c.minimize_window());
            on!(m, on_zoom, |c| c.toggle_maximize_window());
        }

        self.install_window_events(w);
    }

    /// Winit window events the main window needs: file drops (batched), size (responsive rules) and focus.
    fn install_window_events(self: &Rc<Self>, w: &MainWindow) {
        use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
        let weak = self.weak();
        w.window().on_winit_window_event(move |window, event| {
            let Some(c) = weak.upgrade() else { return EventResult::Propagate };
            match event {
                winit::event::WindowEvent::DroppedFile(path) => {
                    c.drops.borrow_mut().push(path.clone(), Instant::now());
                }
                winit::event::WindowEvent::Resized(_) | winit::event::WindowEvent::ScaleFactorChanged { .. } => {
                    let size = window.size().to_logical(window.scale_factor());
                    c.window_resized(size.width, size.height);
                    // Slint updates its own size after this filter: re-report the caption layout on the next turn.
                    let weak = Rc::downgrade(&c);
                    slint::Timer::single_shot(Duration::ZERO, move || {
                        if let Some(c) = weak.upgrade() {
                            c.sync_native_chrome();
                        }
                    });
                    let full = window.with_winit_window(|ww| ww.fullscreen().is_some()).unwrap_or(false);
                    let maximized = window.is_maximized();
                    if let Some(win) = c.win() {
                        win.global::<AppBridge>().set_is_maximized(maximized);
                    }
                    if full != c.st.borrow().full_screen {
                        c.st.borrow_mut().full_screen = full;
                        c.sync_layout(true);
                        c.sync_toolbar();
                    }
                }
                winit::event::WindowEvent::ThemeChanged(_) => c.sync_native_chrome(),
                winit::event::WindowEvent::Focused(f) => {
                    if let Some(win) = c.win() {
                        win.global::<AppBridge>().set_is_active(*f);
                    }
                }
                _ => {}
            }
            EventResult::Propagate
        });
    }

    /// Pushes the logical window size (responsive rules read `MainBridge.window-width`).
    pub fn window_resized(&self, width: f32, height: f32) {
        let changed = {
            let mut st = self.st.borrow_mut();
            let changed = st.last_window_size != (width, height);
            st.last_window_size = (width, height);
            changed
        };
        if changed && let Some(w) = self.win() {
            w.global::<MainBridge>().set_window_width(width);
        }
    }

    // ------------------------------------------------------------------ commands

    fn open_settings(&self) {
        if let Some(h) = &self.hooks.open_settings {
            h();
        }
    }

    /// Windows `Ctrl+O` / macOS `⌘O`: the video picker (editor tab only on macOS).
    fn choose_video(self: &Rc<Self>, run_after_picking: bool) {
        if self.mac && self.vm.workspace() != Workspace::Editor {
            return;
        }
        self.st.borrow_mut().run_after_picking = run_after_picking;
        let fut = self.shell.pick_open_file(FileFilter::Videos);
        let weak = self.weak();
        let _ = slint::spawn_local(async move {
            let picked = fut.await;
            let Some(c) = weak.upgrade() else { return };
            let then_translate = std::mem::take(&mut c.st.borrow_mut().run_after_picking);
            let Some(path) = picked else { return };
            if c.mac {
                c.vm.request_open_video(path);
            } else {
                c.vm.open_video(Some(path));
            }
            if then_translate && (c.vm.video().is_some() || c.vm.has_sentences()) {
                c.vm.generate_and_translate();
            }
        });
    }

    fn choose_subtitle(self: &Rc<Self>) {
        let fut = self.shell.pick_open_file(FileFilter::Subtitles);
        let weak = self.weak();
        let _ = slint::spawn_local(async move {
            let Some(path) = fut.await else { return };
            let Some(c) = weak.upgrade() else { return };
            c.import_subtitle_file(path);
        });
    }

    fn generate_and_translate(self: &Rc<Self>) {
        self.vm.finish_editing();
        if self.vm.video().is_none() && !self.vm.has_sentences() {
            self.choose_video(true);
            return;
        }
        self.vm.generate_and_translate();
    }

    fn add_playlist_videos(self: &Rc<Self>) {
        let fut = self.shell.pick_open_files(FileFilter::Videos);
        let weak = self.weak();
        let _ = slint::spawn_local(async move {
            let paths = fut.await;
            let Some(c) = weak.upgrade() else { return };
            if !paths.is_empty() {
                c.vm.add_playlist_videos(paths);
                c.vm.set_workspace(Workspace::Playlist);
            }
        });
    }

    /// Export (win-ui-main §18.4, mac-ui §6.3): render, then the save dialog, then write UTF-8 without BOM.
    fn export(self: &Rc<Self>) {
        self.vm.finish_editing();
        let name = self.vm.export_file_name();
        let format = self.vm.export_format();
        let weak = self.weak();
        self.vm.render_export(Box::new(move |text| {
            let Some(text) = text else { return };
            let Some(c) = weak.upgrade() else { return };
            let suggested = if c.mac { name.clone() } else { Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name.clone()) };
            let fut = c.shell.pick_save_file(&suggested, format);
            let weak = Rc::downgrade(&c);
            let _ = slint::spawn_local(async move {
                let picked = fut.await;
                let Some(c) = weak.upgrade() else { return };
                let Some(mut path) = picked else { return };
                if !c.mac && path.extension().is_none() {
                    path.set_extension(format.raw());
                }
                // UTF-8 without BOM (win-ui-main §18.4, mac-ui §6.3).
                let result = std::fs::write(&path, text.as_bytes()).map_err(|e| e.to_string());
                if c.mac {
                    c.vm.export_finished(result.err());
                } else if let Err(e) = result {
                    let make: Rc<dyn Fn() -> String> = Rc::new(move || sa_core::l!("Dosya kaydedilemedi: %@", e.clone()));
                    c.error_bar.borrow_mut().show_save_error(make);
                    c.sync_error_bar();
                }
            });
        }));
    }

    fn toggle_panel(&self) {
        // macOS disables the panel button in full screen; WinUI's SubtitlePanelButton / E key still toggle there.
        if self.mac && self.st.borrow().full_screen {
            return;
        }
        self.vm.toggle_subtitle_panel();
    }

    fn toggle_full_screen(&self) {
        let on = !self.st.borrow().full_screen;
        self.set_full_screen(on);
    }

    /// Attaches the native chrome once the window is shown (Windows: caption hit-testing with Snap Layouts, saved
    /// size, frame theme, UISettings accent; macOS: nothing, the unified title bar is set through
    /// [`super::chrome::winit_attributes`] before the window exists). No-op without a native window.
    pub fn attach_native_chrome(self: &Rc<Self>) {
        #[cfg(windows)]
        {
            let Some(w) = self.win() else { return };
            if self.native.borrow().is_some() {
                return;
            }
            // The video surface owns the rendering notifier; whether installing it worked says GPU or software.
            let Some(chrome) = super::chrome::attach(&w, &*self.settings, Some(self.video.installed())) else { return };
            self.apply_accent(sa_platform::win::accent::AccentPalette::current());
            let key = self.key.get().expect("registered");
            let ui = self.ui_dispatcher.clone();
            let watcher = sa_platform::win::accent::AccentWatcher::start(Box::new(move |p| {
                key.post(&ui, move |c| c.apply_accent(p));
            }));
            *self.native.borrow_mut() = Some(NativeChrome { chrome, _accent: watcher });
        }
    }

    /// Pushes the UISettings accent shades (win-ui-main §5) into `AppBridge` (the Fluent `Theme` derives its tokens).
    #[cfg(windows)]
    fn apply_accent(&self, p: sa_platform::win::accent::AccentPalette) {
        let Some(w) = self.win() else { return };
        let app = w.global::<AppBridge>();
        let c = |x: sa_platform::win::accent::Rgba| slint::Color::from_argb_u8(x.a, x.r, x.g, x.b);
        app.set_accent_dark1(c(p.dark1));
        app.set_accent_dark2(c(p.dark2));
        app.set_accent_light2(c(p.light2));
        app.set_accent_light3(c(p.light3));
    }

    /// Re-reports caption layout, maximized state and frame theme to the native chrome (resize, full screen, theme).
    fn sync_native_chrome(&self) {
        #[cfg(windows)]
        if let Some(w) = self.win()
            && let Some(n) = self.native.borrow().as_ref()
        {
            n.chrome.sync(&w);
            n.chrome.sync_theme(&w);
        }
    }

    /// Caption minimize / macOS Window ▸ Minimize (⌘M).
    fn minimize_window(&self) {
        if let Some(h) = &self.hooks.minimize {
            return h();
        }
        #[cfg(windows)]
        if let Some(n) = self.native.borrow().as_ref() {
            return n.chrome.minimize();
        }
        if let Some(w) = self.win() {
            w.window().set_minimized(true);
        }
    }

    /// Caption maximize/restore, title-bar double click / macOS Window ▸ Zoom.
    fn toggle_maximize_window(&self) {
        if let Some(h) = &self.hooks.toggle_maximize {
            return h();
        }
        #[cfg(windows)]
        if let Some(n) = self.native.borrow().as_ref() {
            return n.chrome.toggle_maximize();
        }
        if let Some(w) = self.win() {
            let max = w.window().is_maximized();
            w.window().set_maximized(!max);
            w.global::<AppBridge>().set_is_maximized(!max);
        }
    }

    /// Dragging the window from empty title-bar / toolbar space (the Windows chrome answers HTCAPTION itself).
    fn drag_window(&self) {
        if let Some(h) = &self.hooks.drag_window {
            return h();
        }
        use slint::winit_030::WinitWindowAccessor;
        if let Some(w) = self.win() {
            let _ = w.window().with_winit_window(|ww| ww.drag_window());
        }
    }

    fn set_full_screen(&self, on: bool) {
        let tmpdbg0 = Instant::now();
        self.st.borrow_mut().full_screen = on;
        match &self.hooks.set_full_screen {
            Some(h) => h(on),
            None => {
                if let Some(w) = self.win() {
                    w.window().set_fullscreen(on);
                }
            }
        }
        let tmpdbg1 = Instant::now();
        self.sync_layout(true);
        self.sync_toolbar();
        self.sync_native_chrome();
        eprintln!("[player] TMPDBG fullscreen {on}: set {:?} sync {:?}", tmpdbg1 - tmpdbg0, tmpdbg1.elapsed());
    }

    fn leave_full_screen_or_theater(&self) {
        if self.st.borrow().full_screen {
            self.set_full_screen(false);
        } else if self.vm.is_theater_mode() {
            self.vm.set_theater_mode(false);
        }
    }

    fn seek_scrub(&self, scrubbing: bool, ratio: f64) {
        {
            let mut st = self.st.borrow_mut();
            st.seek_scrubbing = scrubbing;
            st.scrub_ratio = ratio;
        }
        let interacting = scrubbing || self.st.borrow().volume_scrubbing || self.st.borrow().tracks_menu_open;
        self.hud.borrow_mut().set_interacting(interacting, Instant::now());
        let duration = self.player.borrow().duration();
        if scrubbing && let Some(t) = scrub_target(ratio, duration) {
            self.player.borrow_mut().seek(t);
        }
        self.drain_player();
        self.sync_transport();
    }

    fn video_wheel(&self, delta: f64, precise: bool) {
        if self.mac {
            let n = self.st.borrow_mut().mac_notches.feed(delta, precise, false);
            if n != 0 {
                self.player.borrow_mut().adjust_volume(n as f64 * shortcuts::VOLUME_STEP);
            }
        } else {
            let (v, m) = {
                let p = self.player.borrow();
                (p.volume(), p.is_muted())
            };
            // PlayerGrid_PointerWheelChanged: a zero delta returns before anything (no reveal either).
            let Some(volume) = windows_wheel_volume(v, m, delta) else { return };
            self.player.borrow_mut().set_volume(volume);
            self.hud.borrow_mut().reveal(Instant::now());
        }
        self.drain_player();
        self.sync_hud();
    }

    // ---- rows

    fn uid_at(&self, index: i32) -> Option<Uuid> {
        usize::try_from(index).ok().and_then(|i| self.st.borrow().row_uids.get(i).copied())
    }

    /// `select_row(row)` (win-ui-main §9.3): inside the selection keeps it and moves the primary line.
    fn select_row_at(&self, index: i32) {
        if let Some(uid) = self.uid_at(index) {
            self.vm.select_row(uid);
            self.st.borrow_mut().anchor = Some(uid);
        }
    }

    fn row_pressed(&self, index: i32, toggle: bool, range: bool) {
        let Some(i) = usize::try_from(index).ok() else { return };
        let (order, anchor) = {
            let st = self.st.borrow();
            (st.row_uids.clone(), st.anchor)
        };
        if !toggle && !range {
            if let Some(uid) = order.get(i).copied() {
                self.vm.set_selection(Some(uid));
                self.st.borrow_mut().anchor = Some(uid);
            }
            return;
        }
        let selected = self.vm.selected_uids();
        let (set, primary) = super::rows::press_selection(&order, &selected, anchor.as_ref(), i, toggle, range);
        if !range {
            self.st.borrow_mut().anchor = primary;
        }
        self.vm.set_selected_lines(set, primary);
    }

    fn text_focus(&self, index: i32, translation: bool, focused: bool) {
        let Some(uid) = self.uid_at(index) else { return };
        if focused {
            self.vm.select_row(uid);
            if translation { self.vm.begin_translation_editing(uid) } else { self.vm.begin_editing(uid) }
        } else if translation {
            self.vm.commit_translation_draft();
        } else {
            self.vm.commit_draft();
        }
    }

    /// Keys inside the line list that are not text editing (win-ui-main §15 step 5, mac-ui §7.6).
    fn list_key(&self, key: &str, shift: bool) -> bool {
        if self.st.borrow().editing_text {
            return false;
        }
        match key {
            "up" | "down" => {
                let (order, anchor) = {
                    let st = self.st.borrow();
                    (st.row_uids.clone(), st.anchor)
                };
                let primary = self.vm.selection();
                match super::rows::arrow_selection(&order, primary.as_ref(), anchor.as_ref(), key == "down", shift) {
                    Some((set, p)) => {
                        if !shift {
                            self.st.borrow_mut().anchor = Some(p);
                        }
                        self.vm.set_selected_lines(set, Some(p));
                        if let Some(i) = order.iter().position(|u| *u == p)
                            && let Some(w) = self.win()
                        {
                            let main = w.global::<MainBridge>();
                            main.set_scroll_row(i as i32);
                            let mut st = self.st.borrow_mut();
                            st.scroll_epoch += 1;
                            main.set_scroll_epoch(st.scroll_epoch);
                        }
                        true
                    }
                    None => false,
                }
            }
            "delete" => {
                self.vm.finish_editing();
                self.vm.delete_selected();
                true
            }
            // Windows: Space on the list toggles playback; macOS leaves it to the window chrome.
            // Lines_PreviewKeyDown: `_player.TogglePlayback()` alone (no HUD reveal, unlike the root handler).
            "space" if !self.mac => {
                self.player.borrow_mut().toggle_playback();
                self.drain_player();
                true
            }
            _ => false,
        }
    }

    // ---- queue

    fn queue_row_clicked(&self, index: i32) {
        let Some(id) = usize::try_from(index).ok().and_then(|i| self.st.borrow().queue_ids.get(i).copied()) else { return };
        if self.mac {
            if let Some(row) = self.vm.list_rows().into_iter().find(|r| r.id == id) {
                self.vm.open_list_row(&row);
            }
        } else if let Some(item) = self.vm.playlist().into_iter().find(|p| p.id == id)
            && !item.video_path.as_os_str().is_empty()
        {
            self.vm.set_workspace(Workspace::Editor);
            self.vm.open_video(Some(item.video_path));
        }
    }

    fn queue_row_remove(&self, index: i32) {
        let Some(id) = usize::try_from(index).ok().and_then(|i| self.st.borrow().queue_ids.get(i).copied()) else { return };
        if self.mac {
            if let Some(row) = self.vm.list_rows().into_iter().find(|r| r.id == id) {
                self.vm.remove_list_row(&row);
            }
        } else {
            self.vm.remove_playlist_item(id);
        }
    }

    // ---- explorer

    fn explorer_row_clicked(&self, index: usize, start_playing: bool) {
        let (path, is_folder) = {
            let st = self.st.borrow();
            match (st.explorer_paths.get(index), st.explorer_folders.get(index)) {
                (Some(p), Some(f)) => (p.clone(), *f),
                _ => return,
            }
        };
        if is_folder {
            if let Some(e) = &self.explorer {
                e.navigate(&path);
            }
            return;
        }
        self.show_video(path, start_playing);
    }

    /// A hub card's play action: Windows `VideoSelected` (editor tab + `OpenVideo`), macOS `play(url)`
    /// (`showVideo(url, startPlaying: true)`).
    pub fn play_from_hub(&self, path: PathBuf) {
        self.show_video(path, self.mac);
    }

    /// `showVideo(url, startPlaying)` (mac-ui §6.3) / `FileSelected(path)` (Windows).
    fn show_video(&self, path: PathBuf, start_playing: bool) {
        let already = self.vm.video().is_some_and(|v| self.policy.same_path(&v, &path));
        self.vm.set_workspace(Workspace::Editor);
        if already {
            if start_playing {
                self.player.borrow_mut().play();
                self.drain_player();
            }
            return;
        }
        if self.mac {
            self.vm.request_open_video(path);
        } else {
            self.vm.open_video(Some(path));
        }
        if start_playing {
            self.player.borrow_mut().play();
            self.drain_player();
        }
    }

    // ---- menus

    fn open_menu(&self, id: &str) {
        let Some(w) = self.win() else { return };
        let dark = w.global::<ui::Palette>().get_color_scheme() == slint::language::ColorScheme::Dark;
        let now = self.player.borrow().current_time();
        let (head, rest) = id.split_once(':').unwrap_or((id, ""));
        let entries: Vec<Entry> = match head {
            "model" => menus::model_menu(&self.vm.available_models(), &self.vm.model(), self.mac),
            "speakers" => menus::speakers_menu(self.vm.diarize(), self.vm.speaker_count()),
            "speaker" | "row" => {
                let Ok(row) = rest.parse::<usize>() else { return };
                let Some(uid) = self.uid_at(row as i32) else { return };
                // WinUI `SpeakerRow_Click` / `RowMenu_Opening` call `SelectRow(row)` before building the items, so
                // the enabled states below are those of the clicked row. A SwiftUI context menu leaves the selection
                // alone and its actions target the row under the pointer (`rowTargets`).
                if !self.mac {
                    self.select_row_at(row as i32);
                }
                let sentences = self.vm.sentences();
                let Some(index) = sentences.iter().position(|s| s.uid == uid) else { return };
                let speakers = menus::speaker_items(row, &self.vm.speakers(), sentences[index].speaker, dark, self.mac);
                if head == "speaker" {
                    speakers
                } else {
                    let working = self.vm.is_working();
                    let st = if self.mac {
                        let selected = self.vm.selected_uids();
                        let single = !selected.contains(&uid) || selected.len() == 1;
                        RowMenuState {
                            // `.disabled(vm.isWorking)`: the row joins the selection when it is chosen.
                            can_retranslate: true,
                            can_split: !working && single && sentences.can_split(index, now),
                            can_merge: !working && single && index + 1 < sentences.len(),
                            can_delete: !working,
                            shows_speakers: sentences.iter().any(|s| s.speaker.is_some()),
                            is_working: working,
                        }
                    } else {
                        RowMenuState {
                            can_retranslate: self.vm.can_translate_selected(),
                            can_split: self.vm.can_split_selected(now),
                            can_merge: self.vm.can_merge_selected(),
                            can_delete: true,
                            shows_speakers: sentences.iter().any(|s| s.speaker.is_some()),
                            is_working: working,
                        }
                    };
                    menus::row_menu(row, st, speakers, self.mac)
                }
            }
            "bulk-language" => menus::language_menu("lang-all:", None),
            "queue-language" => {
                let Ok(row) = rest.parse::<usize>() else { return };
                let Some(id) = self.st.borrow().queue_ids.get(row).copied() else { return };
                let current = self.vm.playlist().into_iter().find(|p| p.id == id).map(|p| p.source_language).unwrap_or_default();
                menus::language_menu(&format!("row-lang:{row}:"), Some(&current))
            }
            "display" => {
                let i = w.global::<MainBridge>().get_display_mode();
                menus::display_menu(i, &self.vm.translation_title(), self.vm.has_translations())
            }
            "explorer-quick" => self
                .explorer
                .as_ref()
                .map(|e| {
                    e.quick_locations()
                        .into_iter()
                        .enumerate()
                        .map(|(i, q)| {
                            let mut en = Entry::item(format!("quick:{i}"), q.title);
                            en.symbol = quick_symbol(q.kind).to_owned();
                            en.glyph = quick_glyph(q.kind).to_string();
                            en
                        })
                        .collect()
                })
                .unwrap_or_default(),
            "explorer-row" => {
                let Ok(row) = rest.parse::<usize>() else { return };
                let Some(is_folder) = self.st.borrow().explorer_folders.get(row).copied() else { return };
                menus::explorer_row_menu(row, is_folder, self.mac)
            }
            "overflow" => {
                let first: usize = rest.parse().unwrap_or(0);
                self.overflow_menu(first)
            }
            "tracks" => self.track_menu_entries(),
            _ => Vec::new(),
        };
        self.st.borrow_mut().last_menu = head.to_owned();
        self.menu_entries.set_vec(entries.into_iter().map(to_menu_entry).collect::<Vec<_>>());
    }

    /// macOS toolbar overflow `»`: the titles of the items from `first` on that are present (mac-ui §6.2).
    fn overflow_menu(&self, first: usize) -> Vec<Entry> {
        let editor = self.vm.workspace() == Workspace::Editor;
        let not_platform = self.vm.workspace() != Workspace::Platform;
        let can_cancel = self.vm.can_cancel();
        let items: Vec<(usize, bool, String, bool)> = vec![
            (0, editor, sa_core::l("Video seç"), true),
            (1, editor, sa_core::l("Altyazı aç"), !self.vm.is_working()),
            (6, editor && self.vm.has_sentences(), sa_core::l("Çevir…"), !self.vm.is_working()),
            (7, not_platform, sa_core::l("Çeviri modeli"), !self.vm.is_working()),
            (8, editor, sa_core::l("Otomatik kaydet"), true),
            (9, can_cancel, sa_core::l("Durdur"), true),
            (91, !can_cancel && editor, sa_core::l("Komple çevir"), self.vm.can_generate_and_translate()),
            (92, !can_cancel && editor, sa_core::l("Altyazı üret"), self.vm.video().is_some()),
            (10, true, sa_core::l("Profil"), true),
        ];
        let mut out = Vec::new();
        let mut ids = Vec::new();
        for (i, visible, title, enabled) in items {
            let order = if i >= 90 { 9 } else { i };
            if visible && order >= first {
                let mut e = Entry::item(format!("ovf:{i}"), title);
                e.enabled = enabled;
                if i == 8 {
                    e.checkable = true;
                    e.checked = self.vm.auto_save_subtitle();
                }
                out.push(e);
                ids.push(i);
            }
        }
        self.st.borrow_mut().overflow_items = ids;
        out
    }

    fn menu_activated(self: &Rc<Self>, id: &str) {
        let Some(action) = MenuAction::parse(id) else { return };
        match action {
            MenuAction::Model(m) => self.vm.set_model(&m),
            MenuAction::ToggleDiarize => self.vm.set_diarize(!self.vm.diarize()),
            MenuAction::SpeakerCount(n) => self.vm.set_speaker_count(n),
            MenuAction::SetSpeaker(row, sp) => {
                if let Some(uid) = self.uid_at(row as i32) {
                    self.vm.set_speaker(uid, sp);
                }
            }
            MenuAction::NewSpeaker(row) => {
                if let Some(uid) = self.uid_at(row as i32) {
                    self.vm.assign_new_speaker(uid);
                }
            }
            MenuAction::RowRetranslate(row) => {
                self.select_row_at(row as i32);
                self.vm.finish_editing();
                self.vm.translate_selected();
            }
            MenuAction::RowSplit(row) => {
                self.select_row_at(row as i32);
                self.vm.finish_editing();
                let now = self.player.borrow().current_time();
                self.vm.split_selected(now);
            }
            MenuAction::RowMerge(row) => {
                self.select_row_at(row as i32);
                self.vm.finish_editing();
                self.vm.merge_selected_with_next();
            }
            MenuAction::RowDelete(row) => {
                self.select_row_at(row as i32);
                self.vm.finish_editing();
                self.vm.delete_selected();
            }
            MenuAction::AllPlaylistLanguages(code) => self.vm.set_all_playlist_languages(&code),
            MenuAction::QueueRowLanguage(row, code) => {
                let id = self.st.borrow().queue_ids.get(row).copied();
                if let Some(id) = id {
                    self.vm.set_playlist_item_language(id, &code);
                }
            }
            MenuAction::Display(i) => {
                if let Some(w) = self.win() {
                    w.global::<MainBridge>().invoke_display_mode_selected(i);
                }
            }
            MenuAction::QuickLocation(i) => {
                if let Some(e) = &self.explorer
                    && let Some(q) = e.quick_locations().get(i)
                {
                    e.navigate(&q.path);
                }
            }
            MenuAction::ExplorerPlay(i) => self.explorer_row_clicked(i, true),
            MenuAction::ExplorerAddToPlaylist(i) => {
                let path = self.st.borrow().explorer_paths.get(i).cloned();
                if let Some(p) = path {
                    self.vm.add_playlist_videos(vec![p]);
                    self.vm.set_workspace(Workspace::Playlist);
                }
            }
            MenuAction::ExplorerReveal(i) => {
                let path = self.st.borrow().explorer_paths.get(i).cloned();
                if let Some(p) = path {
                    self.shell.reveal_in_file_manager(&p);
                }
            }
            MenuAction::ExplorerOpenFolder(i) => {
                let path = self.st.borrow().explorer_paths.get(i).cloned();
                if let Some(p) = path {
                    self.shell.open_folder(&p);
                }
            }
            MenuAction::Overflow(i) => match i {
                0 => self.choose_video(false),
                1 => self.choose_subtitle(),
                6 => self.vm.translate(),
                7 => {
                    if let Some(h) = &self.hooks.translation_model_clicked {
                        h();
                    }
                }
                8 => self.vm.set_auto_save_subtitle(!self.vm.auto_save_subtitle()),
                9 => self.vm.cancel(),
                91 => self.generate_and_translate(),
                92 => self.vm.generate(),
                10 => {
                    if let Some(h) = &self.hooks.profile_clicked {
                        h();
                    }
                }
                _ => {}
            },
            MenuAction::Track(choice) => self.choose_track(choice),
        }
    }

    // ---- keyboard

    fn focus(&self) -> ShortcutFocus {
        let st = self.st.borrow();
        ShortcutFocus {
            is_editing_text: st.editing_text,
            // WinUI: the root handler leaves Space on a focused ListView/ListViewItem to `Lines_PreviewKeyDown`
            // (which toggles playback without revealing the HUD). Focused buttons are not tracked (Slint buttons
            // here are not keyboard-focusable).
            is_on_button: !self.mac && st.list_focused,
            is_in_list: st.list_focused,
            is_full_screen: st.full_screen,
            panel_hidden: self.vm.hide_subtitle_panel(),
            is_theater: self.vm.is_theater_mode(),
        }
    }

    /// The root (tunneling) key handler: `true` = consumed.
    fn key_pressed(self: &Rc<Self>, key: SlintKey) -> bool {
        let input = input::to_key_input(&key, self.mac);
        let focus = self.focus();
        if self.mac {
            if let Some(cmd) = input::mac_command_key(&input) {
                // On a real Mac the NSMenu owns these key equivalents; previews and Slint builds without a menu
                // handle them here.
                return self.mac_command(cmd);
            }
            if input::is_plain_escape(&input) {
                if focus.is_editing_text {
                    return false;
                }
                if focus.is_full_screen {
                    self.set_full_screen(false);
                    return true;
                }
                if self.vm.is_theater_mode() {
                    self.vm.set_theater_mode(false);
                    return true;
                }
                return false;
            }
            return match shortcuts::resolve_mac(&input, &focus) {
                Some(sc) => self.perform_shortcut(sc, input.is_repeat),
                None => false,
            };
        }
        match shortcuts::resolve_windows_key(&input, &focus) {
            Some(WindowsKeyAction::Player(sc)) => {
                // Windows root handler: every listed key is consumed (§15), even without a player on screen.
                self.perform_shortcut(sc, input.is_repeat);
                true
            }
            Some(WindowsKeyAction::Undo) => {
                self.vm.undo();
                true
            }
            Some(WindowsKeyAction::Redo) => {
                self.vm.redo();
                true
            }
            Some(WindowsKeyAction::PickVideo) => {
                self.choose_video(false);
                true
            }
            Some(WindowsKeyAction::ToggleExplorer) => {
                self.set_explorer_open(!self.explorer_open());
                true
            }
            Some(WindowsKeyAction::Generate) => {
                self.vm.finish_editing();
                if self.vm.can_generate() {
                    self.vm.generate();
                }
                true
            }
            Some(WindowsKeyAction::OpenSettings) => {
                self.open_settings();
                true
            }
            Some(WindowsKeyAction::DeleteSelectedLines) => {
                self.vm.finish_editing();
                self.vm.delete_selected();
                true
            }
            None => false,
        }
    }

    fn mac_command(self: &Rc<Self>, cmd: input::MacCommandKey) -> bool {
        use input::MacCommandKey as K;
        match cmd {
            K::OpenVideo => self.choose_video(false),
            K::Generate => {
                if self.vm.video().is_some() && !self.vm.is_working() && self.vm.workspace() == Workspace::Editor {
                    self.vm.finish_editing();
                    self.vm.generate();
                }
            }
            K::Stop => {
                if self.can_stop() {
                    self.vm.cancel();
                }
            }
            K::ToggleSidebar => self.set_explorer_open(!self.explorer_open()),
            K::ToggleFullScreen => self.toggle_full_screen(),
            K::Settings => self.open_settings(),
            K::Undo | K::Redo => {
                if self.st.borrow().editing_text {
                    return false;
                }
                self.vm.finish_editing();
                if cmd == K::Undo { self.vm.undo() } else { self.vm.redo() }
            }
        }
        true
    }

    /// `PlayerShortcutActions.perform` through the shared performer.
    fn perform_shortcut(&self, sc: PlayerShortcut, repeat: bool) -> bool {
        let reveal = matches!(sc, PlayerShortcut::TogglePlayback | PlayerShortcut::Seek(_) | PlayerShortcut::AdjustVolume(_) | PlayerShortcut::ToggleMute);
        let mut target = Target { c: self };
        // Windows acts on every KeyDown, auto-repeat included (MainWindow.Root_PreviewKeyDown); macOS lets only the
        // repeating shortcuts repeat (mac-ui §5.2).
        let repeat = self.mac && repeat;
        let done = shortcuts::perform(sc, repeat, &mut target);
        self.drain_player();
        if reveal {
            self.hud.borrow_mut().reveal(Instant::now());
            self.sync_hud();
        }
        done
    }

    // ---- drops / external files

    /// A finished drop batch (win-ui-main §17.1, mac-ui §13.1; FIX-W: a single video switches to the Player tab).
    pub fn handle_drop(&self, paths: Vec<PathBuf>) {
        let split = input::split_drop(&paths, |p| p.is_dir());
        if split.videos.is_empty() && split.subtitle.is_none() {
            return;
        }
        let playlist_tab = self.vm.workspace() == Workspace::Playlist;
        if split.videos.len() > 1 || (playlist_tab && !split.videos.is_empty()) {
            self.vm.add_playlist_videos(split.videos);
            self.vm.set_workspace(Workspace::Playlist);
            return;
        }
        let files: Vec<PathBuf> = split.videos.into_iter().chain(split.subtitle).collect();
        self.vm.open_files(files);
    }

    /// Command line, second instance, "Open with", Finder (win-ui-main §17.2, mac-ui §13.2).
    pub fn open_external_files(&self, paths: Vec<PathBuf>) {
        self.vm.open_external_files(paths);
    }

    /// Window closing: pauses the player, reports where it was, stops the timer.
    pub fn detach(&self) {
        if self.disposed.replace(true) {
            return;
        }
        self.timer.stop();
        // win-ui-main §4.3: the restored size is saved on close.
        #[cfg(windows)]
        if let Some(n) = self.native.borrow_mut().take() {
            n.chrome.save_size(&*self.settings);
        }
        self.vm.finish_editing();
        self.reads.borrow_mut().cancel();
        self.player.borrow_mut().dispose();
        let outputs = self.player.borrow_mut().take_outputs();
        for o in outputs {
            if let (PlayerOutput::Playback(r), Some(hub)) = (o, &self.hub) {
                hub.report_playback(r);
            }
        }
        if let Some(k) = self.key.take() {
            k.unregister();
        }
    }

    /// The player (tests, the app shell's quit path).
    pub fn player(&self) -> std::cell::Ref<'_, PlayerController> {
        self.player.borrow()
    }

    pub fn is_full_screen(&self) -> bool {
        self.st.borrow().full_screen
    }

    /// The selection anchor used by Shift+click (tests).
    pub fn selected_uids(&self) -> HashSet<Uuid> {
        self.vm.selected_uids()
    }
}

impl Drop for MainWindowController {
    fn drop(&mut self) {
        self.timer.stop();
    }
}

struct Target<'a> {
    c: &'a MainWindowController,
}

impl ShortcutTarget for Target<'_> {
    fn is_player_shown(&self) -> bool {
        // Only macOS gates the player shortcuts on a visible player; the Windows root handler always acts (§15).
        !self.c.mac || self.c.vm.is_player_shown()
    }
    fn is_full_screen(&self) -> bool {
        // Only consulted for TogglePanel: macOS keeps the panel setting in full screen (mac-ui §5.2); the WinUI E key
        // calls ToggleSubtitlePanel() unconditionally.
        self.c.mac && self.c.st.borrow().full_screen
    }
    fn toggle_playback(&mut self) {
        self.c.player.borrow_mut().toggle_playback();
    }
    fn seek_by(&mut self, seconds: f64) {
        self.c.player.borrow_mut().seek_by(seconds);
    }
    fn adjust_volume(&mut self, step: f64) {
        self.c.player.borrow_mut().adjust_volume(step);
    }
    fn toggle_mute(&mut self) {
        self.c.player.borrow_mut().toggle_mute();
    }
    fn toggle_full_screen(&mut self) {
        self.c.toggle_full_screen();
    }
    fn toggle_theater(&mut self) {
        self.c.vm.set_theater_mode(!self.c.vm.is_theater_mode());
    }
    fn toggle_subtitle_panel(&mut self) {
        self.c.vm.toggle_subtitle_panel();
    }
    fn leave_full_screen_or_theater(&mut self) {
        self.c.leave_full_screen_or_theater();
    }
}

/// The actions of a `video-clicked(count)` report. Slint's `TouchArea` invokes `clicked` for every release, the
/// second release of a double click included, and only then `double-clicked`; so for `count >= 2` the actions the
/// single click already performed are left out. A double click therefore ends as `VideoClick` (WinUI `Tapped` +
/// `DoubleTapped`, AppKit `clickCount`) defines it: playback toggled twice (no net change) and full screen toggled.
pub(crate) fn video_click_delta(count: u32) -> Vec<PlayerShortcut> {
    let mut actions = shortcuts::video_click_actions(count);
    if count >= 2 {
        for done in shortcuts::video_click_actions(1) {
            if let Some(i) = actions.iter().position(|a| *a == done) {
                actions.remove(i);
            }
        }
    }
    actions
}

fn state_index(s: PlaylistItemState) -> i32 {
    match s {
        PlaylistItemState::Pending => 0,
        PlaylistItemState::Working => 1,
        PlaylistItemState::Completed => 2,
        PlaylistItemState::Failed => 3,
    }
}

fn quick_symbol(k: sa_core::platform::QuickLocationKind) -> &'static str {
    use sa_core::platform::QuickLocationKind as Q;
    match k {
        Q::Videos => "film",
        Q::Downloads => "arrow.down.circle",
        Q::Desktop => "menubar.dock.rectangle",
        Q::Home => "house",
        Q::Drive | Q::Volume => "externaldrive",
    }
}

fn quick_glyph(k: sa_core::platform::QuickLocationKind) -> char {
    use sa_core::platform::QuickLocationKind as Q;
    match k {
        Q::Videos => '\u{E8B2}',
        Q::Downloads => '\u{E896}',
        Q::Desktop => '\u{E7F4}',
        Q::Home => '\u{E80F}',
        Q::Drive | Q::Volume => '\u{EDA2}',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real call site, not a vocabulary scan: every quick location's macOS symbol has a vector stand-in.
    #[test]
    fn every_quick_location_symbol_has_a_vector_stand_in() {
        use sa_core::platform::QuickLocationKind as Q;
        for kind in [Q::Videos, Q::Downloads, Q::Desktop, Q::Home, Q::Drive, Q::Volume] {
            // Exhaustive: a new kind fails to compile here until it is added to the list above.
            match kind {
                Q::Videos | Q::Downloads | Q::Desktop | Q::Home | Q::Drive | Q::Volume => {}
            }
            let name = quick_symbol(kind);
            assert!(crate::symbols::svg::body(name).is_some(), "{kind:?}: no stand-in for {name}");
        }
    }

    #[test]
    fn a_double_click_toggles_playback_twice_in_total_and_full_screen_once() {
        // Slint: release 1 → clicked; release 2 → clicked, then double-clicked.
        let all: Vec<PlayerShortcut> = [1, 1, 2].into_iter().flat_map(video_click_delta).collect();
        assert_eq!(all, vec![PlayerShortcut::TogglePlayback, PlayerShortcut::TogglePlayback, PlayerShortcut::ToggleFullScreen]);
        assert!(video_click_delta(0).is_empty());
    }

    #[test]
    fn frames_are_copied_row_by_row_when_strided() {
        let frame = VideoFrame::from_rgba(2, 2, 12, 0.0, Arc::new((0..24).collect()));
        let buf = frame_to_buffer(&frame, None);
        assert_eq!(buf.as_bytes(), &[0, 1, 2, 3, 4, 5, 6, 7, 12, 13, 14, 15, 16, 17, 18, 19]);
        let tight = VideoFrame::from_rgba(2, 1, 8, 0.0, Arc::new((0..8).collect()));
        let reused = frame_to_buffer(&tight, Some(SharedPixelBuffer::new(2, 1)));
        assert_eq!(reused.as_bytes(), &[0, 1, 2, 3, 4, 5, 6, 7]);
    }
}
