//! `SubtitleViewModel`: the editor's controller (win-viewmodels §3–§19, mac-viewmodels §3–§6). One shared core;
//! platform differences come from [`crate::policy::PlatformPolicy`]. UI-thread owned (interior mutability: every
//! method takes `&self`, so the app can hold it in an `Rc` and call it from change handlers); engine streams run on
//! worker threads and every event is applied on the UI thread **in order with back-pressure**
//! (`UiDispatcherExt::invoke_blocking` through [`crate::uiref::UiKey`]).

mod editing;
mod files;
mod playlist;
mod runner;
pub mod session;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use sa_core::appearance::{SubtitleDisplay, TranslationEntry};
use sa_core::engine_types::{EngineModel, Sentence, SubtitleFormat};
use sa_core::l;
use sa_core::l10n::LocalizedMessage;
use sa_core::models::{ModelCatalog, ModelCatalogChange, ModelLabel, SubtitleLanguage};
use sa_core::settings::{defaults, keys};
use sa_core::subscription::Subscription;
use sa_core::undo::UndoStack;
use sa_providers::TranslationProviderProfile;
use uuid::Uuid;

use crate::jobs::{JobQueue, TranslationGate};
use crate::policy::{PlatformPolicy, SingleJobsAsListRows};
use crate::services::AppServices;
use crate::types::{
    ExportLanguage, JobKind, JobSettings, JobStatus, JobStatusKind, ListRow, PlaylistItem, PlaylistItemState, SubtitleChange,
    TimingEdge, Workspace, is_translation_label, stage_label,
};
use crate::uiref::UiKey;

pub use session::{OfflineRetry, Session, SessionId};

/// Background pre-translation needs at least this many stable lines while recognition runs.
pub const MIN_ASYNC_BATCH_SENTENCES: usize = 10;
/// Playlist audio pre-extraction lookahead.
pub const MAX_AUDIO_LOOKAHEAD: usize = 1;

/// What `open_files` decided (shared routing, UNIFIED.md §3.1 / §3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenPlan {
    /// One video (+ optional subtitle).
    Video { video: PathBuf, subtitle: Option<PathBuf> },
    /// A subtitle with its companion video, or alone.
    Subtitle { subtitle: PathBuf, companion_video: Option<PathBuf> },
    /// More than one video (or the queue tab): add to the playlist.
    Playlist(Vec<PathBuf>),
    Nothing,
}

/// Classifies dropped / externally opened paths (folders ignored): the first subtitle file is the subtitle, every
/// other file a video; more than one video (or any video on the queue tab) goes to the playlist.
pub fn plan_open(paths: &[PathBuf], playlist_tab_active: bool) -> OpenPlan {
    let files: Vec<&PathBuf> = paths.iter().filter(|p| !p.is_dir()).collect();
    let subtitle = files.iter().find(|p| files::is_subtitle_path(p)).map(|p| (*p).clone());
    let videos: Vec<PathBuf> = files.iter().filter(|p| !files::is_subtitle_path(p)).map(|p| (*p).clone()).collect();
    if videos.len() > 1 || (!videos.is_empty() && playlist_tab_active) {
        return OpenPlan::Playlist(videos);
    }
    if let Some(v) = videos.into_iter().next() {
        return OpenPlan::Video { video: v, subtitle };
    }
    match subtitle {
        Some(s) => {
            let companion = files::find_companion_video(&s);
            OpenPlan::Subtitle { subtitle: s, companion_video: companion }
        }
        None => OpenPlan::Nothing,
    }
}

/// A job waiting in (or running on) the lane.
#[derive(Debug, Clone)]
pub(crate) struct JobRequest {
    pub kind: JobKind,
    pub settings: JobSettings,
    pub previous_status: JobStatus,
}

/// macOS open confirmation (`PendingOpen`).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingOpen {
    pub stops_job: bool,
    pub loses_edits: bool,
    pub(crate) action: files::OpenAction,
}

impl PendingOpen {
    pub fn message(&self) -> String {
        match (self.stops_job, self.loses_edits) {
            (true, true) => l!("Süren iş durdurulacak ve kaydedilmemiş değişiklikler kaybolacak. Devam edilsin mi?"),
            (true, false) => l!("Süren iş durdurulacak. Devam edilsin mi?"),
            _ => l!("Kaydedilmemiş değişiklikler kaybolacak. Devam edilsin mi?"),
        }
    }
    pub fn title() -> String {
        l!("Başka bir dosya açılsın mı?")
    }
}

/// macOS playlist notice (a run that could not start).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistNotice {
    pub message: LocalizedMessage,
    pub has_retry: bool,
}

/// Snapshot of the active translation provider (from `ProviderStore`).
#[derive(Debug, Clone, Default)]
pub(crate) struct ProviderSnapshot {
    pub active: Option<TranslationProviderProfile>,
    pub usable: bool,
    pub on_local_network: bool,
}

/// All UI-thread state.
pub(crate) struct State {
    pub policy: Arc<PlatformPolicy>,
    pub language: String,
    pub model: String,
    pub translation_target: String,
    pub translation_model: String,
    pub diarize: bool,
    pub speaker_count: i64,
    pub offline_mode: bool,
    pub auto_save: bool,
    pub auto_play: bool,
    pub side_by_side: bool,
    pub hide_panel: bool,
    pub export_format: SubtitleFormat,
    pub workspace: Workspace,
    pub theater: bool,
    pub model_notice: Option<LocalizedMessage>,
    pub sessions: HashMap<SessionId, Session>,
    pub by_key: HashMap<String, SessionId>,
    pub foreground: SessionId,
    pub next_session: SessionId,
    pub queue: JobQueue<JobRequest>,
    pub claims: HashMap<Uuid, HashSet<String>>,
    pub recent_jobs: Vec<SessionId>,
    pub playlist: Vec<PlaylistItem>,
    pub playlist_running: bool,
    pub playlist_run: Option<playlist::RunHandle>,
    pub playlist_notice: Option<PlaylistNotice>,
    pub playlist_background_pending: usize,
    pub audio_ready: HashSet<Uuid>,
    pub stage2_started: HashSet<Uuid>,
    pub open_refusal: Option<LocalizedMessage>,
    pub pending_open: Option<PendingOpen>,
    pub temp_files: Vec<PathBuf>,
    pub physical_memory: u64,
    pub provider: ProviderSnapshot,
    pub mirroring_playlist: bool,
    /// Playlist stage 2 waits for these jobs to end.
    pub job_waiters: HashMap<(SessionId, Uuid), crossbeam_channel::Sender<()>>,
}

impl State {
    pub fn fg(&self) -> &Session {
        &self.sessions[&self.foreground]
    }
    pub fn fg_mut(&mut self) -> &mut Session {
        self.sessions.get_mut(&self.foreground).expect("foreground session")
    }
    pub fn session(&self, sid: SessionId) -> Option<&Session> {
        self.sessions.get(&sid)
    }
    pub fn session_mut(&mut self, sid: SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(&sid)
    }
    pub fn is_foreground(&self, sid: SessionId) -> bool {
        self.foreground == sid
    }
    pub fn key(&self, path: &Path) -> String {
        self.policy.path_key(path)
    }
    pub fn existing_session(&self, path: &Path) -> Option<SessionId> {
        self.by_key.get(&self.key(path)).copied()
    }
    pub fn new_session(&mut self, video: Option<PathBuf>) -> SessionId {
        self.next_session += 1;
        let id = self.next_session;
        let display = self.stored_display_mode_value();
        self.sessions.insert(id, Session::new(id, video, display));
        id
    }
    fn stored_display_mode_value(&self) -> SubtitleDisplay {
        // The foreground's mode is the persisted one; new sessions start with it.
        self.sessions.get(&self.foreground).map(|s| s.display_mode).unwrap_or_default()
    }
    pub fn is_external_llm_active(&self) -> bool {
        self.provider.usable
    }
    pub fn is_external_async_active(&self) -> bool {
        self.provider.usable && self.provider.active.as_ref().is_some_and(|p| p.async_translation)
    }
    /// The active provider's name when it sends text over the internet.
    pub fn active_cloud_provider_name(&self) -> Option<String> {
        if self.provider.usable && !self.provider.on_local_network {
            self.provider.active.as_ref().map(|p| p.name.clone())
        } else {
            None
        }
    }
    pub fn job_settings(&self) -> JobSettings {
        JobSettings {
            language: self.language.clone(),
            model: self.model.clone(),
            diarize: self.diarize,
            speaker_count: self.speaker_count,
            translation_target: self.translation_target.clone(),
            translation_model: self.translation_model.clone(),
            auto_save: self.auto_save,
            export_format: self.export_format,
        }
    }
    pub fn translation_source(&self) -> Option<String> {
        self.fg().translation_source(&self.language)
    }
}

/// State plus the foreground undo history (separate fields so undo actions get `&mut State`).
pub(crate) struct Core {
    pub st: State,
    pub undo: UndoStack<State>,
}

type Deferred = Box<dyn FnOnce(&Rc<Inner>)>;

/// Effects collected while `core` is borrowed; run after the borrow is released.
#[derive(Default)]
pub(crate) struct Fx {
    pub changes: Vec<SubtitleChange>,
    pub after: Vec<Deferred>,
}

impl Fx {
    pub fn ch(&mut self, c: SubtitleChange) {
        if !self.changes.contains(&c) {
            self.changes.push(c);
        }
    }
    pub fn all(&mut self, cs: &[SubtitleChange]) {
        for c in cs {
            self.ch(*c);
        }
    }
    pub fn then(&mut self, f: impl FnOnce(&Rc<Inner>) + 'static) {
        self.after.push(Box::new(f));
    }
}

/// External callbacks the app installs.
#[derive(Default)]
pub(crate) struct Hooks {
    pub subtitle_saved: Option<Rc<dyn Fn(&Path)>>,
    pub renders: HashMap<u64, Box<dyn FnOnce(Option<String>)>>,
    pub next_render: u64,
}

pub(crate) struct Inner {
    pub svc: AppServices,
    pub catalog: Rc<ModelCatalog>,
    pub gate: Arc<TranslationGate>,
    pub key: Cell<Option<UiKey<Inner>>>,
    core: RefCell<Core>,
    handlers: RefCell<Vec<Rc<dyn Fn(SubtitleChange)>>>,
    pending: RefCell<Vec<SubtitleChange>>,
    notifying: Cell<bool>,
    deferred: RefCell<VecDeque<Deferred>>,
    subs: RefCell<Vec<Subscription>>,
    pub hooks: RefCell<Hooks>,
}

impl Inner {
    pub fn key(&self) -> UiKey<Inner> {
        self.key.get().expect("registered view model")
    }

    /// Mutates state; notifications and deferred work run afterwards (outside the borrow).
    pub fn edit<R>(self: &Rc<Self>, f: impl FnOnce(&Inner, &mut Core, &mut Fx) -> R) -> R {
        let mut fx = Fx::default();
        let r = {
            let mut c = self.core.borrow_mut();
            f(self, &mut c, &mut fx)
        };
        self.finish(fx);
        r
    }

    /// Runs `f` now, or after the current edit when the state is borrowed (inline dispatchers).
    pub fn run_ui(self: &Rc<Self>, f: impl FnOnce(&Rc<Inner>) + 'static) {
        if self.core.try_borrow_mut().is_ok() {
            f(self);
        } else {
            self.deferred.borrow_mut().push_back(Box::new(f));
        }
    }

    pub fn read<R>(&self, f: impl FnOnce(&Core) -> R) -> R {
        f(&self.core.borrow())
    }

    fn finish(self: &Rc<Self>, fx: Fx) {
        {
            let mut p = self.pending.borrow_mut();
            for c in fx.changes {
                if !p.contains(&c) {
                    p.push(c);
                }
            }
        }
        for a in fx.after {
            a(self);
        }
        loop {
            let next = self.deferred.borrow_mut().pop_front();
            match next {
                Some(d) => d(self),
                None => break,
            }
        }
        self.flush_notifications();
    }

    fn flush_notifications(self: &Rc<Self>) {
        if self.notifying.get() {
            return;
        }
        self.notifying.set(true);
        loop {
            let batch: Vec<SubtitleChange> = std::mem::take(&mut *self.pending.borrow_mut());
            if batch.is_empty() {
                break;
            }
            let handlers: Vec<Rc<dyn Fn(SubtitleChange)>> = self.handlers.borrow().clone();
            for c in batch {
                for h in &handlers {
                    h(c);
                }
            }
        }
        self.notifying.set(false);
    }

    /// Model claims: the union of every running job's models (Settings disables deleting them).
    pub fn publish_claims(&self, st: &State) {
        let all: HashSet<String> = st.claims.values().flatten().cloned().collect();
        self.catalog.set_active_models(all);
    }

    pub fn claim_models(&self, st: &mut State, job: Uuid, ids: HashSet<String>) {
        if ids.is_empty() {
            st.claims.remove(&job);
        } else {
            st.claims.insert(job, ids);
        }
        self.publish_claims(st);
    }

    pub fn release_models(&self, st: &mut State, job: Uuid) {
        if st.claims.remove(&job).is_some() {
            self.publish_claims(st);
        }
    }

    pub fn persist_string(&self, key: &str, v: &str) {
        self.svc.settings.set_string(key, v);
    }

    /// Publishes `offline` (and the active external LLM, key read off the UI thread) to the launch state.
    pub fn publish_launch_settings(&self, st: &State) {
        self.svc.launch_state.set_offline(st.offline_mode);
        let providers = self.svc.providers.clone();
        let launch = self.svc.launch_state.clone();
        let _ = std::thread::Builder::new().name("sa-launch-settings".into()).spawn(move || {
            let llm = providers.active_external_llm().map(|a| a.llm);
            launch.set_external_llm(llm);
        });
    }

    /// Re-reads the active provider (UI-safe: memory only).
    pub fn refresh_provider_snapshot(&self, st: &mut State) {
        let active = self.svc.providers.active_provider();
        let normalized = active.as_ref().and_then(|p| sa_enginekit::external_llm::ExternalLlmServer::normalize_base_url(&p.base_url));
        let on_local = normalized
            .as_deref()
            .and_then(sa_enginekit::external_llm::ExternalLlmServer::host)
            .is_some_and(|h| sa_enginekit::external_llm::ExternalLlmServer::is_local_network_host(&h));
        st.provider = ProviderSnapshot { usable: normalized.is_some(), active, on_local_network: on_local };
    }
}

/// The view model.
pub struct SubtitleViewModel {
    pub(crate) inner: Rc<Inner>,
}

impl Drop for SubtitleViewModel {
    fn drop(&mut self) {
        if let Some(k) = self.inner.key.get() {
            k.unregister();
        }
    }
}

fn valid_language(code: &str) -> bool {
    SubtitleLanguage::from_code(code).is_some()
}

impl SubtitleViewModel {
    pub const DIARIZATION_MODEL_ID: &'static str = "speaker-diarization";
    pub const SPEAKER_COUNT_CHOICES: [i64; 6] = [0, 2, 3, 4, 5, 6];
    pub const MIN_ASYNC_BATCH_SENTENCES: usize = MIN_ASYNC_BATCH_SENTENCES;
    pub const DEFAULT_MODEL: &'static str = defaults::SELECTED_MODEL;
    pub const DEFAULT_TRANSLATION_MODEL: &'static str = defaults::TRANSLATION_MODEL;
    pub const DEFAULT_TRANSLATION_TARGET: &'static str = defaults::TRANSLATION_TARGET;

    /// Reads settings, applies offline/launch state, reconciles models and translation target (win-viewmodels §3
    /// constructor order / mac-viewmodels §3.3). Must be called on the UI thread.
    pub fn new(services: AppServices, catalog: Rc<ModelCatalog>) -> Self {
        let s = &services.settings;
        let get_s = |k: &str, d: &str| s.get_string(k).unwrap_or_else(|| d.to_owned());
        let language = {
            let v = get_s(keys::SELECTED_LANGUAGE, defaults::SELECTED_LANGUAGE);
            if valid_language(&v) { v } else { String::new() }
        };
        let translation_target = {
            let v = get_s(keys::TRANSLATION_TARGET, defaults::TRANSLATION_TARGET);
            if !v.is_empty() && valid_language(&v) { v } else { defaults::TRANSLATION_TARGET.to_owned() }
        };
        let display = SubtitleDisplay::parse(s.get_string(keys::SUBTITLE_DISPLAY_MODE).as_deref()).unwrap_or_default();
        let speaker_count = s.get_int(keys::SPEAKER_COUNT).filter(|v| *v >= 2).unwrap_or(0);
        let policy = services.policy.clone();
        let physical_memory = services.shell.total_physical_memory();
        let mut st = State {
            policy,
            language,
            model: get_s(keys::SELECTED_MODEL, defaults::SELECTED_MODEL),
            translation_target,
            translation_model: get_s(keys::TRANSLATION_MODEL, defaults::TRANSLATION_MODEL),
            diarize: s.get_bool(keys::DIARIZE_SPEAKERS).unwrap_or(defaults::DIARIZE_SPEAKERS),
            speaker_count,
            offline_mode: s.get_bool(keys::OFFLINE_MODE).unwrap_or(defaults::OFFLINE_MODE),
            auto_save: s.get_bool(keys::AUTO_SAVE_SUBTITLE).unwrap_or(defaults::AUTO_SAVE_SUBTITLE),
            auto_play: s.get_bool(keys::AUTO_PLAY_VIDEO).unwrap_or(defaults::AUTO_PLAY_VIDEO),
            side_by_side: s.get_bool(keys::SIDE_BY_SIDE_LAYOUT).unwrap_or(defaults::SIDE_BY_SIDE_LAYOUT),
            hide_panel: s.get_bool(keys::HIDE_SUBTITLE_PANEL).unwrap_or(defaults::HIDE_SUBTITLE_PANEL),
            export_format: SubtitleFormat::Srt,
            workspace: Workspace::Editor,
            theater: false,
            model_notice: None,
            sessions: HashMap::new(),
            by_key: HashMap::new(),
            foreground: 0,
            next_session: 0,
            queue: JobQueue::new(),
            claims: HashMap::new(),
            recent_jobs: Vec::new(),
            playlist: Vec::new(),
            playlist_running: false,
            playlist_run: None,
            playlist_notice: None,
            playlist_background_pending: 0,
            audio_ready: HashSet::new(),
            stage2_started: HashSet::new(),
            open_refusal: None,
            pending_open: None,
            temp_files: Vec::new(),
            physical_memory,
            provider: ProviderSnapshot::default(),
            mirroring_playlist: false,
            job_waiters: HashMap::new(),
        };
        st.next_session = 1;
        st.sessions.insert(1, Session::new(1, None, display));
        st.foreground = 1;

        let inner = Rc::new(Inner {
            svc: services,
            catalog,
            gate: TranslationGate::new(),
            key: Cell::new(None),
            core: RefCell::new(Core { st, undo: UndoStack::new() }),
            handlers: RefCell::new(Vec::new()),
            pending: RefCell::new(Vec::new()),
            notifying: Cell::new(false),
            deferred: RefCell::new(VecDeque::new()),
            subs: RefCell::new(Vec::new()),
            hooks: RefCell::new(Hooks::default()),
        });
        let key = UiKey::register(&inner);
        inner.key.set(Some(key));

        // Subscriptions (posted to the UI thread).
        {
            let ui = inner.svc.ui.clone();
            let sub = inner.catalog.subscribe(move |c: &ModelCatalogChange| {
                if *c == ModelCatalogChange::Models {
                    key.post(&ui, |inner| {
                        let inner = inner.clone();
                        inner.clone().run_ui(move |_| {
                            inner.edit(|i, c, fx| {
                                i.reconcile_model_selection(&mut c.st, fx);
                                i.reconcile_translation_model_selection(&mut c.st, fx);
                                fx.all(&[SubtitleChange::Models, SubtitleChange::Settings]);
                            })
                        });
                    });
                }
            });
            inner.subs.borrow_mut().push(sub);
        }
        {
            let ui = inner.svc.ui.clone();
            let sub = inner.svc.providers.subscribe(Box::new(move |_| {
                key.post(&ui, |inner| {
                    let inner = inner.clone();
                    inner.clone().run_ui(move |_| {
                        inner.edit(|i, c, fx| {
                            i.refresh_provider_snapshot(&mut c.st);
                            i.publish_launch_settings(&c.st);
                            fx.all(&[SubtitleChange::Providers, SubtitleChange::Settings]);
                        })
                    });
                });
            }));
            inner.subs.borrow_mut().push(sub);
        }
        {
            let ui = inner.svc.ui.clone();
            let sub = sa_core::l10n::LanguageState::shared().subscribe(move |_| {
                key.post(&ui, |inner| {
                    let inner = inner.clone();
                    inner.clone().run_ui(move |_| inner.edit(|_, _, fx| fx.ch(SubtitleChange::All)));
                });
            });
            inner.subs.borrow_mut().push(sub);
        }
        {
            let ui = inner.svc.ui.clone();
            let mut undo_sub = None;
            inner.edit(|_, c, _| {
                undo_sub = Some(c.undo.subscribe(move |_| {
                    key.post(&ui, |inner| {
                        let inner = inner.clone();
                        inner.clone().run_ui(move |_| inner.edit(|_, _, fx| fx.ch(SubtitleChange::Undo)));
                    });
                }));
            });
            if let Some(s) = undo_sub {
                inner.subs.borrow_mut().push(s);
            }
        }

        inner.edit(|i, c, fx| {
            i.refresh_provider_snapshot(&mut c.st);
            i.svc.launch_state.set_offline(c.st.offline_mode);
            i.reconcile_model_selection(&mut c.st, fx);
            i.reconcile_translation_target(&mut c.st, fx);
            i.reconcile_translation_model_selection(&mut c.st, fx);
            i.publish_launch_settings(&c.st);
        });
        SubtitleViewModel { inner }
    }

    /// UI-thread change notifications.
    pub fn on_change(&self, handler: Box<dyn Fn(SubtitleChange)>) {
        self.inner.handlers.borrow_mut().push(Rc::from(handler));
    }

    /// Called after every successful subtitle write (the sidebar rescans that folder).
    pub fn on_subtitle_saved(&self, handler: Box<dyn Fn(&Path)>) {
        self.inner.hooks.borrow_mut().subtitle_saved = Some(Rc::from(handler));
    }

    fn read<R>(&self, f: impl FnOnce(&State) -> R) -> R {
        self.inner.read(|c| f(&c.st))
    }

    fn set_setting(&self, f: impl FnOnce(&Inner, &mut State, &mut Fx)) {
        self.inner.edit(|i, c, fx| {
            f(i, &mut c.st, fx);
            fx.ch(SubtitleChange::Settings);
        });
    }

    // ----- persisted settings (setters persist only on change)

    pub fn language(&self) -> String {
        self.read(|s| s.language.clone())
    }
    pub fn set_language(&self, code: &str) {
        let code = if valid_language(code) { code.to_owned() } else { String::new() };
        self.set_setting(|i, st, fx| {
            if st.language == code {
                return;
            }
            st.language = code.clone();
            i.persist_string(keys::SELECTED_LANGUAGE, &code);
            st.fg_mut().lines_source_language = None;
            st.model_notice = None;
            i.reconcile_model_selection(st, fx);
            i.reconcile_translation_target(st, fx);
            fx.all(&[SubtitleChange::Models, SubtitleChange::Status]);
        });
    }
    pub fn model(&self) -> String {
        self.read(|s| s.model.clone())
    }
    pub fn set_model(&self, id: &str) {
        self.set_setting(|i, st, fx| {
            if st.model == id {
                return;
            }
            st.model = id.to_owned();
            st.model_notice = None;
            i.persist_string(keys::SELECTED_MODEL, id);
            fx.ch(SubtitleChange::Models);
        });
    }
    pub fn translation_target(&self) -> String {
        self.read(|s| s.translation_target.clone())
    }
    pub fn set_translation_target(&self, code: &str) {
        self.set_setting(|i, st, _| i.set_target(st, code));
    }
    pub fn translation_model(&self) -> String {
        self.read(|s| s.translation_model.clone())
    }
    pub fn set_translation_model(&self, id: &str) {
        self.set_setting(|i, st, _| {
            if st.translation_model != id {
                st.translation_model = id.to_owned();
                i.persist_string(keys::TRANSLATION_MODEL, id);
            }
        });
    }
    pub fn diarize(&self) -> bool {
        self.read(|s| s.diarize)
    }
    pub fn set_diarize(&self, v: bool) {
        self.set_setting(|i, st, _| {
            if st.diarize != v {
                st.diarize = v;
                i.svc.settings.set_bool(keys::DIARIZE_SPEAKERS, v);
            }
        });
    }
    pub fn speaker_count(&self) -> i64 {
        self.read(|s| s.speaker_count)
    }
    pub fn set_speaker_count(&self, v: i64) {
        self.set_setting(|i, st, _| {
            if st.speaker_count != v {
                st.speaker_count = v;
                i.svc.settings.set_int(keys::SPEAKER_COUNT, v);
            }
        });
    }
    pub fn offline_mode(&self) -> bool {
        self.read(|s| s.offline_mode)
    }
    pub fn set_offline_mode(&self, v: bool) {
        self.set_setting(|i, st, _| {
            if st.offline_mode != v {
                st.offline_mode = v;
                i.svc.settings.set_bool(keys::OFFLINE_MODE, v);
                i.publish_launch_settings(st);
            }
        });
    }
    pub fn auto_save_subtitle(&self) -> bool {
        self.read(|s| s.auto_save)
    }
    pub fn set_auto_save_subtitle(&self, v: bool) {
        self.set_setting(|i, st, _| {
            if st.auto_save != v {
                st.auto_save = v;
                i.svc.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, v);
            }
        });
    }
    pub fn auto_play_video(&self) -> bool {
        self.read(|s| s.auto_play)
    }
    pub fn set_auto_play_video(&self, v: bool) {
        self.set_setting(|i, st, _| {
            if st.auto_play != v {
                st.auto_play = v;
                i.svc.settings.set_bool(keys::AUTO_PLAY_VIDEO, v);
            }
        });
    }
    pub fn side_by_side_layout(&self) -> bool {
        self.read(|s| s.side_by_side)
    }
    pub fn set_side_by_side_layout(&self, v: bool) {
        self.inner.edit(|i, c, fx| {
            if c.st.side_by_side != v {
                c.st.side_by_side = v;
                i.svc.settings.set_bool(keys::SIDE_BY_SIDE_LAYOUT, v);
                fx.ch(SubtitleChange::Layout);
            }
        });
    }
    pub fn hide_subtitle_panel(&self) -> bool {
        self.read(|s| s.hide_panel)
    }
    pub fn set_hide_subtitle_panel(&self, v: bool) {
        self.inner.edit(|i, c, fx| {
            if c.st.hide_panel != v {
                c.st.hide_panel = v;
                i.svc.settings.set_bool(keys::HIDE_SUBTITLE_PANEL, v);
                fx.ch(SubtitleChange::Layout);
            }
        });
    }
    pub fn toggle_subtitle_panel(&self) {
        let v = self.hide_subtitle_panel();
        self.set_hide_subtitle_panel(!v);
    }
    /// The foreground's display mode (persisted as `subtitleDisplayMode`).
    pub fn display_mode(&self) -> SubtitleDisplay {
        self.read(|s| s.fg().display_mode)
    }
    pub fn effective_display_mode(&self) -> SubtitleDisplay {
        self.read(|s| s.fg().effective_display_mode())
    }
    pub fn set_display_mode(&self, v: SubtitleDisplay) {
        self.inner.edit(|i, c, fx| {
            if c.st.fg_mut().set_display_mode(v) {
                i.persist_string(keys::SUBTITLE_DISPLAY_MODE, v.raw());
                fx.all(&[SubtitleChange::Settings, SubtitleChange::Translations]);
            }
        });
    }

    // ----- state

    pub fn policy(&self) -> Arc<PlatformPolicy> {
        self.inner.svc.policy.clone()
    }
    pub fn workspace(&self) -> Workspace {
        self.read(|s| s.workspace)
    }
    pub fn set_workspace(&self, w: Workspace) {
        self.inner.edit(|_, c, fx| {
            if c.st.workspace != w {
                c.st.workspace = w;
                fx.all(&[SubtitleChange::Workspace, SubtitleChange::Status]);
            }
        });
    }
    pub fn is_theater_mode(&self) -> bool {
        self.read(|s| s.theater)
    }
    pub fn set_theater_mode(&self, v: bool) {
        self.inner.edit(|_, c, fx| {
            if c.st.theater != v {
                c.st.theater = v;
                fx.ch(SubtitleChange::Layout);
            }
        });
    }
    pub fn video(&self) -> Option<PathBuf> {
        self.read(|s| s.fg().video.clone())
    }
    pub fn sentences(&self) -> Vec<Sentence> {
        self.read(|s| s.fg().sentences.clone())
    }
    pub fn sentence_count(&self) -> usize {
        self.read(|s| s.fg().sentences.len())
    }
    pub fn has_sentences(&self) -> bool {
        self.read(|s| !s.fg().sentences.is_empty())
    }
    pub fn translations(&self) -> HashMap<Uuid, TranslationEntry> {
        self.read(|s| s.fg().translations.clone())
    }
    pub fn has_translations(&self) -> bool {
        self.read(|s| s.fg().has_translations())
    }
    pub fn translation_for(&self, uid: Uuid) -> Option<TranslationEntry> {
        self.read(|s| s.fg().translations.get(&uid).cloned())
    }
    pub fn is_translation_stale(&self, uid: Uuid) -> bool {
        self.read(|s| s.fg().sentences.iter().find(|x| x.uid == uid).is_some_and(|x| s.fg().is_translation_stale(x)))
    }
    /// The review marker of a line (macOS also shows an imported JSON line's own flag).
    pub fn needs_review(&self, uid: Uuid) -> bool {
        self.read(|s| {
            let fg = s.fg();
            match fg.sentences.iter().find(|x| x.uid == uid) {
                None => false,
                Some(x) if s.policy.import_json_review_entries => fg.needs_review(x),
                Some(x) => fg.translations.get(&x.uid).is_some_and(|e| e.needs_review),
            }
        })
    }
    pub fn translation_language(&self) -> Option<String> {
        self.read(|s| s.fg().translation_language.clone())
    }
    pub fn detected_language(&self) -> Option<String> {
        self.read(|s| s.fg().detected_language.clone())
    }
    pub fn translation_source(&self) -> Option<String> {
        self.read(|s| s.translation_source())
    }
    pub fn is_translation_target_enabled(&self, code: &str) -> bool {
        self.read(|s| s.translation_source().as_deref() != Some(code))
    }
    pub fn translation_target_help(&self) -> String {
        if self.translation_source().is_none() {
            l!("Çeviri hedef dili. Kaynak dil bilinmiyor; NLLB için önce konuşma dilini seç.")
        } else {
            l!("Çeviri hedef dili; kaynak dille aynı olan seçilemez.")
        }
    }
    pub fn status(&self) -> JobStatus {
        self.read(|s| s.fg().status.clone())
    }
    pub fn loaded_subtitle(&self) -> Option<PathBuf> {
        self.read(|s| s.fg().loaded_subtitle.clone())
    }
    pub fn saved_subtitle_name(&self) -> Option<String> {
        self.read(|s| s.fg().saved_subtitle_name.clone())
    }
    pub fn has_unsaved_edits(&self) -> bool {
        self.read(|s| s.fg().has_unsaved_edits)
    }
    pub fn is_async_translating(&self) -> bool {
        self.read(|s| s.fg().is_async_translating)
    }
    pub fn async_translation_fraction(&self) -> Option<f64> {
        self.read(|s| s.fg().async_fraction)
    }
    pub fn is_translation_job_running(&self) -> bool {
        self.read(|s| s.fg().is_translation_job_running)
    }
    pub fn is_playlist_running(&self) -> bool {
        self.read(|s| s.playlist_running)
    }
    pub fn status_kind(&self) -> JobStatusKind {
        self.read(|s| {
            let fg = s.fg();
            match &fg.status {
                JobStatus::Working { .. } => JobStatusKind::Working,
                _ if fg.is_async_translating => JobStatusKind::Working,
                other => other.kind(),
            }
        })
    }
    pub fn finished_summary(&self) -> String {
        self.read(|s| s.fg().finished_summary(s.policy.summary_language_placeholder))
    }
    fn async_label_text(fraction: Option<f64>) -> String {
        stage_label(sa_core::engine_types::EngineStage::Translating, fraction, crate::types::JobPurpose::Translation).text()
    }
    /// win-viewmodels §4.2 `StatusText` / mac status texts.
    pub fn status_text(&self) -> String {
        self.read(|s| {
            let fg = s.fg();
            let win = s.policy.single_jobs_as_list_rows == SingleJobsAsListRows::PlaylistRows;
            match &fg.status {
                JobStatus::Working { label, .. } => {
                    if fg.is_async_translating && !is_translation_label(label) && (win || !fg.asyncs.shown_in_status) {
                        format!("{} · {}", label.text(), Self::async_label_text(fg.async_fraction))
                    } else {
                        label.text()
                    }
                }
                _ if fg.is_async_translating => Self::async_label_text(fg.async_fraction),
                JobStatus::Finished => fg.finished_summary(s.policy.summary_language_placeholder),
                JobStatus::Failed { message } => message.text(),
                JobStatus::Idle => {
                    let ready = fg.video.is_none() || !fg.sentences.is_empty() || (!win && s.workspace != Workspace::Editor);
                    if ready {
                        l!("Hazır")
                    } else if win {
                        l!("Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (Ctrl+Enter)")
                    } else {
                        l!("Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (⌘↩)")
                    }
                }
            }
        })
    }
    pub fn status_fraction(&self) -> Option<f64> {
        self.read(|s| {
            let fg = s.fg();
            match &fg.status {
                JobStatus::Working { fraction, .. } => *fraction,
                _ if fg.is_async_translating => fg.async_fraction,
                _ => None,
            }
        })
    }
    pub fn is_indeterminate(&self) -> bool {
        self.read(|s| {
            let fg = s.fg();
            match &fg.status {
                JobStatus::Working { fraction, .. } => fraction.is_none(),
                _ => fg.is_async_translating && fg.async_fraction.is_none(),
            }
        })
    }
    /// macOS: the background-translation detail next to a Working label.
    pub fn async_status_detail(&self) -> Option<String> {
        self.read(|s| {
            let fg = s.fg();
            (fg.is_async_translating && !fg.asyncs.shown_in_status).then(|| Self::async_label_text(fg.async_fraction))
        })
    }
    pub fn is_working(&self) -> bool {
        self.read(|s| {
            let win = s.policy.single_jobs_as_list_rows == SingleJobsAsListRows::PlaylistRows;
            s.fg().is_working() || (win && s.playlist_running)
        })
    }
    /// Quit only clears the engine scratch folder when false.
    pub fn has_active_work(&self) -> bool {
        self.read(|s| s.playlist_running || s.queue.is_busy() || s.sessions.values().any(|x| x.is_working()))
    }
    pub fn is_translation_in_progress(&self) -> bool {
        self.read(|s| {
            let fg = s.fg();
            fg.is_async_translating
                || fg.is_translation_job_running
                || fg.is_playlist_translating
                || matches!(&fg.status, JobStatus::Working { label, .. } if is_translation_label(label))
        })
    }
    /// Editor header badge text (`""` when no translation runs).
    pub fn translation_badge_text(&self) -> String {
        self.read(|s| {
            let fg = s.fg();
            if let JobStatus::Working { label, .. } = &fg.status
                && (is_translation_label(label) || fg.is_translation_job_running)
            {
                return label.text();
            }
            if fg.is_async_translating || fg.is_playlist_translating {
                return Self::async_label_text(fg.async_fraction);
            }
            String::new()
        })
    }
    pub fn model_notice(&self) -> Option<LocalizedMessage> {
        self.read(|s| s.model_notice.clone())
    }
    pub fn model_notice_help() -> String {
        l!("Seçili model bu dil için listede yoktu; en doğru uygun model seçildi.")
    }
    pub fn available_models(&self) -> Vec<EngineModel> {
        let lang = self.language();
        self.inner.catalog.models_for(&lang)
    }
    pub fn model_name(&self) -> String {
        ModelLabel::name(&self.model())
    }
    pub fn is_external_llm_active(&self) -> bool {
        self.read(|s| s.is_external_llm_active())
    }
    pub fn is_external_async_active(&self) -> bool {
        self.read(|s| s.is_external_async_active())
    }
    pub fn active_cloud_provider_name(&self) -> Option<String> {
        self.read(|s| s.active_cloud_provider_name())
    }
    /// Toolbar translation-model title.
    pub fn translation_model_name(&self) -> String {
        self.read(|s| {
            let Some(p) = s.provider.active.as_ref().filter(|_| s.provider.usable) else {
                return ModelLabel::name(&s.translation_model);
            };
            let model = p.model.trim();
            if p.is_lan() {
                let shown = if model.is_empty() { l!("Harici LLM") } else { model.to_owned() };
                if p.async_translation { l!("Harici: %@ (Asenkron)", shown) } else { l!("Harici: %@", shown) }
            } else if !model.is_empty() {
                format!("{}: {model}", p.name)
            } else if s.policy.provider_title_auto_model {
                format!("{}: {}", p.name, l!("Otomatik"))
            } else {
                p.name.clone()
            }
        })
    }
    pub fn window_title(&self) -> String {
        self.read(|s| {
            s.fg()
                .video
                .as_ref()
                .and_then(|v| v.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Subtitle Adder".to_owned())
        })
    }
    pub fn export_format(&self) -> SubtitleFormat {
        self.read(|s| s.export_format)
    }
    pub fn set_export_format(&self, f: SubtitleFormat) {
        self.inner.edit(|_, c, fx| {
            if c.st.export_format != f {
                c.st.export_format = f;
                fx.ch(SubtitleChange::Settings);
            }
        });
    }
    pub fn export_language(&self) -> ExportLanguage {
        self.read(|s| s.fg().export_language)
    }
    pub fn set_export_language(&self, v: ExportLanguage) {
        self.inner.edit(|i, c, fx| {
            let before = c.st.fg().display_mode;
            if c.st.fg_mut().set_export_language(v) {
                let after = c.st.fg().display_mode;
                if before != after {
                    i.persist_string(keys::SUBTITLE_DISPLAY_MODE, after.raw());
                }
                fx.all(&[SubtitleChange::Settings, SubtitleChange::Translations]);
            }
        });
    }
    pub fn exports_translation(&self) -> bool {
        self.read(|s| s.fg().export_language == ExportLanguage::Translated && s.fg().has_translations())
    }
    /// `Çeviri (%@)` / `Çeviri`.
    pub fn translation_title(&self) -> String {
        match self.translation_language() {
            Some(l) => l!("Çeviri (%@)", l),
            None => l!("Çeviri"),
        }
    }
    /// The save-panel default name (`{stem}{.lang}.{fmt}`).
    pub fn export_file_name(&self) -> String {
        self.read(|s| {
            let fg = s.fg();
            let stem = fg
                .base_path()
                .and_then(|p| p.file_stem())
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_else(|| "subtitles".to_owned());
            let translated = fg.export_language == ExportLanguage::Translated && fg.has_translations();
            let suffix = if translated {
                format!(".{}", fg.translation_language.clone().unwrap_or_else(|| "translated".to_owned()))
            } else {
                String::new()
            };
            format!("{stem}{suffix}.{}", s.export_format.raw())
        })
    }
    /// `<video stem>.<fmt>` next to the video (or the loaded subtitle).
    pub fn video_subtitle_path(&self) -> Option<PathBuf> {
        self.read(|s| s.fg().base_path().map(|p| p.with_extension(s.export_format.raw())))
    }
    pub fn current_job_settings(&self) -> JobSettings {
        self.read(|s| s.job_settings())
    }
    /// Distinct speakers, ascending.
    pub fn speakers(&self) -> Vec<i64> {
        self.read(|s| {
            let mut v: Vec<i64> = s.fg().sentences.iter().filter_map(|x| x.speaker).collect();
            v.sort_unstable();
            v.dedup();
            v
        })
    }
    pub fn is_player_shown(&self) -> bool {
        self.read(|s| s.workspace == Workspace::Editor && s.fg().video.is_some())
    }
    /// `!(is_player_shown && (hide_subtitle_panel || is_full_screen))`.
    pub fn shows_subtitle_panel(&self, is_full_screen: bool) -> bool {
        !(self.is_player_shown() && (self.hide_subtitle_panel() || is_full_screen))
    }
    pub fn open_refusal(&self) -> Option<LocalizedMessage> {
        self.read(|s| s.open_refusal.clone())
    }
    pub fn clear_open_refusal(&self) {
        self.inner.edit(|_, c, fx| {
            c.st.open_refusal = None;
            fx.ch(SubtitleChange::Notice);
        });
    }
    pub fn pending_open(&self) -> Option<PendingOpen> {
        self.read(|s| s.pending_open.clone())
    }
    pub fn playlist_notice(&self) -> Option<PlaylistNotice> {
        self.read(|s| s.playlist_notice.clone())
    }
    pub fn playlist_background_pending(&self) -> usize {
        self.read(|s| s.playlist_background_pending)
    }
    /// Test hook / app: machine RAM for the playlist overlap rule.
    pub fn set_physical_memory(&self, bytes: u64) {
        self.inner.edit(|_, c, _| c.st.physical_memory = bytes);
    }
    /// The translation gate shared by every translation of this view model.
    pub fn translation_gate(&self) -> Arc<TranslationGate> {
        self.inner.gate.clone()
    }

    // ----- commands (CanExecute in can_*)

    pub fn can_generate(&self) -> bool {
        self.read(|s| s.fg().video.is_some()) && !self.is_working()
    }
    pub fn generate(&self) {
        self.inner.edit(|i, c, fx| {
            let fg = c.st.foreground;
            let settings = c.st.job_settings();
            i.request(c, fx, JobKind::Generate { then_translate: false }, fg, settings);
        });
    }
    pub fn can_generate_and_translate(&self) -> bool {
        !self.is_working() && self.read(|s| s.fg().video.is_some() || !s.fg().sentences.is_empty())
    }
    /// "Komple çevir".
    pub fn generate_and_translate(&self) {
        self.inner.edit(|i, c, fx| {
            i.commit_draft(c, fx);
            let fg = c.st.foreground;
            let settings = c.st.job_settings();
            if c.st.fg().video.is_none() && !c.st.fg().sentences.is_empty() {
                i.request(c, fx, JobKind::Translate { reuse_existing: false, only: None }, fg, settings);
            } else {
                i.request(c, fx, JobKind::Generate { then_translate: true }, fg, settings);
            }
        });
    }
    pub fn can_translate(&self) -> bool {
        self.has_sentences() && !self.is_working()
    }
    pub fn translate(&self) {
        self.translate_with(false, None);
    }
    /// `translate(reuse_existing, only)`.
    pub fn translate_with(&self, reuse_existing: bool, only: Option<HashSet<Uuid>>) {
        self.inner.edit(|i, c, fx| {
            let fg = c.st.foreground;
            let settings = c.st.job_settings();
            i.request(c, fx, JobKind::Translate { reuse_existing, only }, fg, settings);
        });
    }
    pub fn can_translate_selected(&self) -> bool {
        self.read(|s| !s.fg().selected_uids.is_empty()) && !self.is_working()
    }
    pub fn translate_selected(&self) {
        if !self.can_translate_selected() {
            return;
        }
        let only = self.read(|s| s.fg().selected_uids.clone());
        self.translate_with(false, Some(only));
    }
    pub fn can_cancel(&self) -> bool {
        self.read(|s| if s.workspace == Workspace::Playlist { s.playlist_running } else { false }) || self.is_working()
    }
    /// Stop / Esc.
    pub fn cancel(&self) {
        self.inner.edit(|i, c, fx| i.cancel(c, fx));
    }
    /// List tab row Stop.
    pub fn cancel_job_of(&self, video: &Path) {
        let video = video.to_path_buf();
        self.inner.edit(move |i, c, fx| i.cancel_job_of(c, fx, &video));
    }
    /// "Sıradakileri temizle".
    pub fn clear_queue(&self) {
        self.inner.edit(|i, c, fx| i.clear_queue(c, fx));
    }
    pub fn queued_job_count(&self) -> usize {
        self.read(|s| s.queue.waiting().len())
    }
    /// 1-based queue position of the foreground video.
    pub fn queue_position(&self) -> Option<usize> {
        self.read(|s| s.queue.position(s.foreground).map(|p| p + 1))
    }
    pub fn is_queued(&self) -> bool {
        self.read(|s| s.queue.is_queued(s.foreground))
    }
    pub fn can_go_online_and_retry(&self) -> bool {
        self.read(|s| matches!(s.fg().status, JobStatus::Failed { .. }) && s.fg().offline_retry.is_some())
    }
    pub fn offline_retry_title(&self) -> String {
        self.read(|s| match &s.fg().status {
            JobStatus::Failed { message } if message.id().starts_with("offline-cloud:") => l!("Çevrimdışı modu kapat ve çevir"),
            _ => l!("Çevrimdışı modu kapat ve indir"),
        })
    }
    /// "Çevrimdışı modu kapat ve indir" / status-bar retry.
    pub fn go_online_and_retry(&self) {
        self.inner.edit(|i, c, fx| i.go_online_and_retry(c, fx));
    }
    pub fn go_online_and_retry_playlist(&self) {
        self.inner.edit(|i, c, fx| {
            let Some(n) = c.st.playlist_notice.take() else { return };
            if !n.has_retry {
                c.st.playlist_notice = Some(n);
                return;
            }
            if c.st.offline_mode {
                c.st.offline_mode = false;
                i.svc.settings.set_bool(keys::OFFLINE_MODE, false);
                i.publish_launch_settings(&c.st);
            }
            fx.all(&[SubtitleChange::Settings, SubtitleChange::Playlist]);
            fx.then(|inner| inner.start_playlist());
        });
    }

    // ----- opening

    /// Opens a video (or none) in the editor (win-viewmodels §8.1, mac-viewmodels §5.13).
    pub fn open_video(&self, path: Option<PathBuf>) {
        self.inner.edit(|i, c, fx| i.open_video(c, fx, path));
    }
    /// Opens a video asking first when work would be lost (macOS policy); immediately otherwise.
    pub fn request_open_video(&self, path: PathBuf) {
        self.inner.edit(|i, c, fx| i.request_open_video(c, fx, path));
    }
    pub fn request_open_subtitle(&self, path: PathBuf) {
        self.inner.edit(|i, c, fx| i.request_open_subtitle(c, fx, path));
    }
    pub fn confirm_pending_open(&self) {
        self.inner.edit(|i, c, fx| i.confirm_pending_open(c, fx));
    }
    pub fn cancel_pending_open(&self) {
        self.inner.edit(|_, c, fx| {
            c.st.pending_open = None;
            fx.ch(SubtitleChange::Notice);
        });
    }
    /// Imports a subtitle file into the foreground. Parsing happens synchronously here only for small files the UI
    /// already picked; failures show in the status.
    pub fn load_subtitle_file(&self, path: &Path) -> bool {
        let path = path.to_path_buf();
        self.inner.edit(move |i, c, fx| {
            let fg = c.st.foreground;
            i.load_subtitle_file(c, fx, &path, fg)
        })
    }
    /// Window drop (UNIFIED.md §3.3 FIX-W: a single dropped video switches to the editor).
    pub fn open_files(&self, paths: Vec<PathBuf>) {
        self.inner.edit(|i, c, fx| i.route_files(c, fx, paths, false));
    }
    /// Command line / pipe / Apple Events (only existing regular files).
    pub fn open_external_files(&self, paths: Vec<PathBuf>) {
        let files: Vec<PathBuf> = paths.into_iter().filter(|p| p.is_file()).collect();
        if files.is_empty() {
            return;
        }
        self.inner.edit(|i, c, fx| i.route_files(c, fx, files, true));
    }
    /// Shows a video in the editor (list rows, cards, explorer).
    pub fn show_in_editor(&self, video: PathBuf) {
        self.inner.edit(|i, c, fx| {
            c.st.workspace = Workspace::Editor;
            fx.ch(SubtitleChange::Workspace);
            i.open_video(c, fx, Some(video));
        });
    }
    pub fn is_current_video(&self, path: &Path) -> bool {
        self.read(|s| s.fg().video.as_ref().is_some_and(|v| s.key(v) == s.key(path)))
    }

    // ----- editing (each public op = one undo group)

    pub fn selection(&self) -> Option<Uuid> {
        self.read(|s| s.fg().selection)
    }
    pub fn selected_uids(&self) -> HashSet<Uuid> {
        self.read(|s| s.fg().selected_uids.clone())
    }
    pub fn has_single_selection(&self) -> bool {
        self.read(|s| s.fg().has_single_selection())
    }
    pub fn set_selection(&self, uid: Option<Uuid>) {
        self.inner.edit(|_, c, fx| {
            c.st.fg_mut().set_selection(uid);
            fx.ch(SubtitleChange::Selection);
        });
    }
    /// win-viewmodels §4.4.
    pub fn set_selected_lines(&self, uids: HashSet<Uuid>, active: Option<Uuid>) {
        self.inner.edit(|_, c, fx| {
            c.st.fg_mut().set_selected_lines(uids, active);
            fx.ch(SubtitleChange::Selection);
        });
    }
    /// macOS list selection change (the farthest newly added line becomes active).
    pub fn update_selection(&self, uids: HashSet<Uuid>) {
        self.inner.edit(|_, c, fx| {
            c.st.fg_mut().update_selection(uids);
            fx.ch(SubtitleChange::Selection);
        });
    }
    pub fn select_row(&self, uid: Uuid) {
        self.inner.edit(|_, c, fx| {
            c.st.fg_mut().select_row(uid);
            fx.ch(SubtitleChange::Selection);
        });
    }
    pub fn draft_text(&self, uid: Uuid) -> String {
        self.read(|s| {
            let fg = s.fg();
            match &fg.draft {
                Some(d) if d.uid == uid => d.text.clone(),
                _ => fg.sentences.iter().find(|x| x.uid == uid).map(|x| x.text.clone()).unwrap_or_default(),
            }
        })
    }
    pub fn translation_draft_text(&self, uid: Uuid) -> String {
        self.read(|s| {
            let fg = s.fg();
            match &fg.translation_draft {
                Some(d) if d.uid == uid => d.text.clone(),
                _ => fg.translations.get(&uid).map(|e| e.text.clone()).unwrap_or_default(),
            }
        })
    }
    pub fn begin_editing(&self, uid: Uuid) {
        self.inner.edit(|i, c, fx| i.begin_editing(c, fx, uid));
    }
    /// win-viewmodels §15.2 `UpdateDraft`.
    pub fn set_draft(&self, uid: Uuid, text: &str) {
        let text = text.to_owned();
        self.inner.edit(move |i, c, fx| i.update_draft(c, fx, uid, text));
    }
    pub fn commit_draft(&self) {
        self.inner.edit(|i, c, fx| i.commit_draft(c, fx));
    }
    pub fn begin_translation_editing(&self, uid: Uuid) {
        self.inner.edit(|i, c, fx| i.begin_translation_editing(c, fx, uid));
    }
    pub fn set_translation_draft(&self, uid: Uuid, text: &str) {
        let text = text.to_owned();
        self.inner.edit(move |i, c, fx| i.update_translation_draft(c, fx, uid, text));
    }
    pub fn commit_translation_draft(&self) {
        self.inner.edit(|i, c, fx| i.commit_translation_draft(c, fx));
    }
    /// `FinishEditing()` = commit both drafts.
    pub fn finish_editing(&self) {
        self.inner.edit(|i, c, fx| i.finish_editing(c, fx));
    }
    pub fn set_text(&self, uid: Uuid, text: &str) {
        let text = text.to_owned();
        self.inner.edit(move |i, c, fx| {
            i.set_text(c, fx, uid, &text);
        });
    }
    pub fn set_translation_text(&self, uid: Uuid, text: &str) {
        let text = text.to_owned();
        self.inner.edit(move |i, c, fx| {
            i.set_translation_text(c, fx, uid, &text);
        });
    }
    pub fn set_speaker(&self, uid: Uuid, speaker: Option<i64>) {
        self.inner.edit(|i, c, fx| i.set_speaker(c, fx, uid, speaker));
    }
    pub fn assign_new_speaker(&self, uid: Uuid) {
        self.inner.edit(|i, c, fx| i.assign_new_speaker(c, fx, uid));
    }
    pub fn nudge(&self, edge: TimingEdge, delta: f64) {
        self.inner.edit(|i, c, fx| i.nudge(c, fx, edge, delta));
    }
    pub fn set_edge_to_now(&self, edge: TimingEdge, now: f64) {
        self.inner.edit(|i, c, fx| i.set_edge(c, fx, edge, now));
    }
    pub fn can_edit_timing(&self) -> bool {
        self.read(|s| {
            let fg = s.fg();
            fg.selection.is_some() && (!s.policy.split_merge_timing_need_single_selection || fg.has_single_selection())
        })
    }
    pub fn can_split_selected(&self, at: f64) -> bool {
        self.inner.read(|c| editing::can_split_selected(&c.st, at))
    }
    pub fn split_selected(&self, at: f64) {
        self.inner.edit(|i, c, fx| i.split_selected(c, fx, at));
    }
    pub fn can_merge_selected(&self) -> bool {
        self.inner.read(|c| editing::can_merge_selected(&c.st))
    }
    pub fn merge_selected_with_next(&self) {
        self.inner.edit(|i, c, fx| i.merge_selected(c, fx));
    }
    pub fn can_delete_selected(&self) -> bool {
        self.read(|s| s.fg().selection.is_some() || !s.fg().selected_uids.is_empty())
    }
    /// Deletes every selected line (UNIFIED.md §3.5 FIX-W), one undo group.
    pub fn delete_selected(&self) {
        self.inner.edit(|i, c, fx| i.delete_selected(c, fx));
    }
    /// `MM:SS.ss` of the selected line's edge, or the placeholder.
    pub fn selected_edge_text(&self, edge: TimingEdge) -> String {
        self.read(|s| {
            let fg = s.fg();
            fg.selection
                .and_then(|u| fg.sentences.iter().find(|x| x.uid == u))
                .map(|x| subtitle_time_short(if edge == TimingEdge::Start { x.start } else { x.end }))
                .unwrap_or_else(|| sa_core::text::SUBTITLE_TIME_PLACEHOLDER.to_owned())
        })
    }
    pub fn undo(&self) {
        self.inner.edit(|i, c, fx| i.undo(c, fx));
    }
    pub fn redo(&self) {
        self.inner.edit(|i, c, fx| i.redo(c, fx));
    }
    pub fn can_undo(&self) -> bool {
        self.inner.read(|c| c.undo.can_undo())
    }
    pub fn can_redo(&self) -> bool {
        self.inner.read(|c| c.undo.can_redo())
    }
    pub fn undo_action_key(&self) -> Option<String> {
        self.inner.read(|c| c.undo.undo_action_key().map(str::to_owned))
    }
    pub fn redo_action_key(&self) -> Option<String> {
        self.inner.read(|c| c.undo.redo_action_key().map(str::to_owned))
    }
    /// Windows tooltips `%@ İşlemini Geri Al` / `Geri Al`.
    pub fn undo_title(&self) -> String {
        sa_core::undo::undo_title(self.undo_action_key().as_deref())
    }
    pub fn redo_title(&self) -> String {
        sa_core::undo::redo_title(self.redo_action_key().as_deref())
    }

    // ----- export / save (rendering through the engine)

    pub fn can_export(&self) -> bool {
        self.read(|s| !s.fg().sentences.is_empty() && !s.fg().status.is_working())
    }
    /// Renders the foreground (`ExportFormat`, `ExportsTranslation`) and writes `target` (UTF-8, no BOM).
    pub fn export_to(&self, target: PathBuf) {
        self.inner.edit(|i, c, fx| i.export_to(c, fx, target));
    }
    /// Renders the foreground; `done(Some(text))` on the UI thread (the macOS export panel writes it).
    pub fn render_export(&self, done: Box<dyn FnOnce(Option<String>)>) {
        self.inner.edit(|i, c, fx| i.render_foreground(c, fx, done));
    }
    pub fn can_save_subtitle(&self) -> bool {
        self.read(|s| files::can_save(s.fg()))
    }
    /// `<video stem>.<fmt>` next to the video, atomic.
    pub fn save_subtitle_next_to_video(&self) {
        self.inner.edit(|i, c, fx| {
            i.finish_editing(c, fx);
            let fg = c.st.foreground;
            let format = c.st.export_format;
            i.save_subtitle_of(c, fx, fg, None, format);
        });
    }
    /// macOS export panel result: success marks the edits saved.
    pub fn export_finished(&self, error: Option<String>) {
        self.inner.edit(|_, c, fx| {
            let fg = c.st.fg_mut();
            match error {
                None => {
                    fg.has_unsaved_edits = false;
                    if !fg.is_working() && matches!(&fg.status, JobStatus::Failed { message } if message.id().starts_with("save-failed:")) {
                        fg.set_status(JobStatus::Finished);
                    }
                }
                Some(e) => {
                    fg.set_status(JobStatus::Failed { message: files::save_failure(&e) });
                }
            }
            fx.ch(SubtitleChange::Status);
        });
    }

    // ----- playlist

    pub fn playlist(&self) -> Vec<PlaylistItem> {
        self.read(|s| s.playlist.clone())
    }
    pub fn list_rows(&self) -> Vec<ListRow> {
        self.read(playlist::list_rows)
    }
    pub fn add_playlist_videos(&self, paths: Vec<PathBuf>) {
        self.inner.edit(|i, c, fx| i.add_to_playlist(c, fx, paths, None));
    }
    pub fn add_playlist_videos_with_language(&self, paths: Vec<PathBuf>, source_language: &str) {
        let src = source_language.to_owned();
        self.inner.edit(move |i, c, fx| i.add_to_playlist(c, fx, paths, Some(src)));
    }
    pub fn remove_playlist_item(&self, id: Uuid) {
        self.inner.edit(|i, c, fx| i.remove_from_playlist(c, fx, id));
    }
    pub fn set_playlist_item_language(&self, id: Uuid, code: &str) {
        let code = if valid_language(code) { code.to_owned() } else { String::new() };
        self.inner.edit(move |_, c, fx| {
            if let Some(item) = c.st.playlist.iter_mut().find(|p| p.id == id)
                && item.state != PlaylistItemState::Working
                && item.source_language != code
            {
                item.source_language = code;
                fx.ch(SubtitleChange::Playlist);
            }
        });
    }
    pub fn set_all_playlist_languages(&self, code: &str) {
        let code = if valid_language(code) { code.to_owned() } else { String::new() };
        self.inner.edit(move |_, c, fx| {
            for item in c.st.playlist.iter_mut().filter(|p| p.state != PlaylistItemState::Working) {
                item.source_language = code.clone();
            }
            fx.ch(SubtitleChange::Playlist);
        });
    }
    pub fn clear_playlist(&self) {
        self.inner.edit(|i, c, fx| i.clear_playlist(c, fx));
    }
    pub fn can_start_playlist(&self) -> bool {
        self.read(|s| {
            !s.playlist_running
                && !(s.policy.single_jobs_as_list_rows == SingleJobsAsListRows::PlaylistRows && s.fg().is_working())
                && s.playlist.iter().any(|p| matches!(p.state, PlaylistItemState::Pending | PlaylistItemState::Failed))
        })
    }
    pub fn start_playlist(&self) {
        self.inner.start_playlist();
    }
    pub fn playlist_summary_text(&self) -> String {
        self.read(|s| {
            let rows = playlist::list_rows(s);
            if rows.is_empty() {
                l!("Listede henüz video yok.")
            } else {
                let done = rows.iter().filter(|r| r.state == PlaylistItemState::Completed).count();
                l!("%lld / %lld tamamlandı", done as i64, rows.len() as i64)
            }
        })
    }
    /// `playlist_out_directory` of a video (macOS per-item folder).
    pub fn playlist_out_directory(&self, video: &Path) -> PathBuf {
        let work = self.inner.svc.shell.engine_work_dir();
        playlist::playlist_out_directory(&work, video)
    }
    pub fn open_list_row(&self, row: &ListRow) {
        self.show_in_editor(row.video_path.clone());
    }
    pub fn remove_list_row(&self, row: &ListRow) {
        let row = row.clone();
        self.inner.edit(move |i, c, fx| i.remove_list_row(c, fx, &row));
    }

    // ----- lifecycle

    /// Language switch: re-push every formatted string.
    pub fn refresh_localization(&self) {
        self.inner.catalog.refresh_localization();
        self.inner.edit(|_, _, fx| fx.ch(SubtitleChange::All));
    }

    /// Cancels jobs, deletes the session temp files (Windows) and forgets subscriptions (exit).
    pub fn shutdown(&self) {
        self.inner.edit(|i, c, fx| {
            i.cancel_everything(c, fx);
            for f in std::mem::take(&mut c.st.temp_files) {
                let _ = std::fs::remove_file(&f);
            }
        });
        self.inner.subs.borrow_mut().clear();
    }
}

/// `SubtitleTime.Short(s)` = `MM:SS.ss` (`65.25` → `01:05.25`), truncating minutes, invariant digits.
pub fn subtitle_time_short(seconds: f64) -> String {
    let s = if seconds.is_finite() { seconds } else { 0.0 };
    let minutes = s.trunc() as i64 / 60;
    let rest = s % 60.0;
    format!("{minutes:02}:{rest:05.2}")
}

impl Inner {
    fn set_target(&self, st: &mut State, code: &str) {
        if st.translation_target != code {
            st.translation_target = code.to_owned();
            self.persist_string(keys::TRANSLATION_TARGET, code);
        }
    }

    /// `ReconcileModelSelection()` (first wins ties).
    pub(crate) fn reconcile_model_selection(&self, st: &mut State, fx: &mut Fx) {
        let available = self.catalog.models_for(&st.language);
        if available.is_empty() || available.iter().any(|m| m.id == st.model) {
            return;
        }
        let Some(best) = most_accurate(&available) else { return };
        let id = best.id.clone();
        st.model = id.clone();
        self.persist_string(keys::SELECTED_MODEL, &id);
        if let Some(lang) = SubtitleLanguage::from_code(&st.language) {
            let code = st.language.clone();
            let mid = id.clone();
            st.model_notice = Some(LocalizedMessage::new(format!("notice:{code}:{id}"), move || {
                l!("%@ için %@ seçildi.", lang.short_name(), ModelLabel::name(&mid))
            }));
        }
        fx.all(&[SubtitleChange::Models, SubtitleChange::Settings]);
    }

    pub(crate) fn reconcile_translation_model_selection(&self, st: &mut State, fx: &mut Fx) {
        let models = self.catalog.translation_models();
        if models.is_empty() || models.iter().any(|m| m.id == st.translation_model) {
            return;
        }
        if let Some(best) = most_accurate(&models) {
            st.translation_model = best.id.clone();
            self.persist_string(keys::TRANSLATION_MODEL, &best.id);
            fx.ch(SubtitleChange::Settings);
        }
    }

    /// Moves the target off the source language.
    pub(crate) fn reconcile_translation_target(&self, st: &mut State, fx: &mut Fx) {
        let source = st.translation_source();
        let next = reconciled_target(&st.translation_target, source.as_deref());
        if next != st.translation_target {
            self.set_target(st, &next);
            fx.ch(SubtitleChange::Settings);
        }
    }
}

/// Max by `(accuracy, speed)`, the first wins ties (never `Iterator::max_by`).
pub fn most_accurate(models: &[EngineModel]) -> Option<&EngineModel> {
    let mut best: Option<&EngineModel> = None;
    for m in models {
        if best.is_none_or(|b| (m.accuracy, m.speed) > (b.accuracy, b.speed)) {
            best = Some(m);
        }
    }
    best
}

/// The first translation target other than `source` when `source == target`.
pub fn reconciled_target(target: &str, source: Option<&str>) -> String {
    if source == Some(target) {
        SubtitleLanguage::translation_targets()
            .iter()
            .find(|l| Some(l.code) != source)
            .map(|l| l.code.to_owned())
            .unwrap_or_else(|| target.to_owned())
    } else {
        target.to_owned()
    }
}

impl crate::providers_ui::PickerHost for SubtitleViewModel {
    fn translation_model(&self) -> String {
        SubtitleViewModel::translation_model(self)
    }
    fn set_translation_model(&self, id: &str) {
        SubtitleViewModel::set_translation_model(self, id);
    }
}
