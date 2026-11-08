//! Test fakes for the view-model tests (ports of the C# `FakeEngineClient` / `Make` helpers and the Swift
//! `FakeEngine` / `PlatformSandbox`): a channel-fed engine, in-memory stores, a shell over a temp folder and a
//! pumped UI thread (`QueueDispatcher`).

#![allow(dead_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_core::diagnostics::{DiagnosticSink, MemoryDiagnostics};
use sa_core::dispatch::{QueueDispatcher, UiDispatcher};
use sa_core::engine_types::{EngineError, EngineModel, EngineStage, Sentence, SubtitleFormat, Word};
use sa_core::future::LocalBoxFuture;
use sa_core::l10n::{CatalogChain, LanguageState, UiLanguage};
use sa_core::models::{ModelCatalog, ModelCatalogBackend, ModelCatalogOptions};
use sa_core::platform::{Alert, AlertResponse, FileFilter, PlatformShell, QuickLocation, ThumbnailGenerator};
use sa_core::secrets::{SecretKey32, SecretString};
use sa_core::settings::{InMemorySettingsStore, SettingsStore};
use sa_core::subscription::{Subscribers, Subscription};
use sa_enginekit::client::{EngineClient, EventStream, TranscribeOptions};
use sa_enginekit::events::{EngineEvent, EngineResult};
use sa_enginekit::launch::{EngineLaunchSettings, EngineLaunchState, ExternalLlm};
use sa_media::{DirectorySnapshot, IndexedMediaFile, MediaIndex, ScanResult};
use sa_profiles::{
    FeedQuery, FingerprintMode, ProfileChange, ProfileCounts, ProfileError, ProfileStore, Ticks, UserVideoList, VideoFeedFilter,
    VideoUserStats,
};
use sa_providers::{ActiveExternalLlm, ProviderChange, ProviderKeyRead, ProviderLoadProblem, ProviderStore, TranslationProviderProfile};
use sa_sync::SyncBus;
use sa_viewmodels::policy::PlatformPolicy;
use sa_viewmodels::services::AppServices;
use sa_viewmodels::subtitle::SubtitleViewModel;
use uuid::Uuid;

pub type Item = Result<EngineEvent, EngineError>;

// ------------------------------------------------------------------------------------------------- language scope

static LANG_LOCK: Mutex<()> = Mutex::new(());

/// Serializes tests that touch the global language state and sets the chain + language for the scope.
pub struct LanguageScope {
    _g: parking_lot::MutexGuard<'static, ()>,
}

impl LanguageScope {
    pub fn new(chain: CatalogChain, lang: UiLanguage) -> Self {
        let g = LANG_LOCK.lock();
        sa_core::l10n::set_active_chain(chain);
        LanguageState::shared().set_language(lang);
        Self { _g: g }
    }
    pub fn set(&self, lang: UiLanguage) {
        LanguageState::shared().set_language(lang);
    }
}

impl Drop for LanguageScope {
    fn drop(&mut self) {
        LanguageState::shared().set_language(UiLanguage::Tr);
    }
}

// ------------------------------------------------------------------------------------------------- engine

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallKind {
    Extract,
    Transcribe,
    Translate,
}

/// What a streaming call received.
#[derive(Debug, Clone)]
pub struct CallRecord {
    pub kind: CallKind,
    pub video: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub options: Option<TranscribeOptions>,
    pub sentences: Vec<Sentence>,
    pub to: Option<String>,
    pub from: Option<String>,
    pub model: Option<String>,
    /// The launch settings at call time (offline, external LLM).
    pub launch: EngineLaunchSettings,
}

/// A live streaming call: push events with `send`, end with `finish` (dropping the sender ends the stream).
pub struct LiveCall {
    pub record: CallRecord,
    tx: Mutex<Option<crossbeam_channel::Sender<Item>>>,
    pub cancel: CancellationToken,
}

impl LiveCall {
    pub fn send(&self, ev: EngineEvent) {
        if let Some(tx) = self.tx.lock().as_ref() {
            let _ = tx.send(Ok(ev));
        }
    }
    pub fn fail(&self, e: EngineError) {
        if let Some(tx) = self.tx.lock().take() {
            let _ = tx.send(Err(e));
        }
    }
    pub fn finish(&self) {
        self.tx.lock().take();
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
    pub fn stage(&self, stage: EngineStage, fraction: Option<f64>) {
        self.send(EngineEvent::Stage { stage, fraction });
    }
    pub fn result(&self, language: &str, audio: &str, sentences: Vec<Sentence>) {
        self.send(EngineEvent::Result(EngineResult {
            language: language.into(),
            duration: 10.0,
            audio: audio.into(),
            outputs: vec![],
            sentences,
        }));
        self.finish();
    }
    pub fn translation(&self, language: &str, sentences: Vec<Sentence>) {
        self.send(EngineEvent::Translation { language: language.into(), sentences });
    }
}

type Script = Box<dyn Fn(&CallRecord) -> Option<Vec<Item>> + Send + Sync>;

/// The channel-fed engine (C# `FakeEngineClient` + `PipelinedTestEngineClient`).
pub struct FakeEngine {
    pub launch: Arc<EngineLaunchState>,
    pending: Mutex<VecDeque<Arc<LiveCall>>>,
    all: Mutex<Vec<Arc<LiveCall>>>,
    script: Mutex<Option<Script>>,
    pub auto_complete_extract: AtomicBool,
    pub renders: Mutex<Vec<(Vec<Sentence>, SubtitleFormat)>>,
    pub render_result: Mutex<Option<Result<String, EngineError>>>,
    pub render_delay: Mutex<Option<Duration>>,
    pub models: Mutex<Vec<EngineModel>>,
    pub models_calls: AtomicUsize,
    pub deletes: Mutex<Vec<String>>,
    pub peak_transcribe: AtomicUsize,
    pub peak_translate: AtomicUsize,
    pub peak_extract: AtomicUsize,
    active: Arc<Mutex<HashMap<CallKind, usize>>>,
}

impl FakeEngine {
    pub fn new(launch: Arc<EngineLaunchState>) -> Arc<Self> {
        Arc::new(Self {
            launch,
            pending: Mutex::new(VecDeque::new()),
            all: Mutex::new(Vec::new()),
            script: Mutex::new(None),
            auto_complete_extract: AtomicBool::new(true),
            renders: Mutex::new(Vec::new()),
            render_result: Mutex::new(None),
            render_delay: Mutex::new(None),
            models: Mutex::new(sa_core::models::fallback_models()),
            models_calls: AtomicUsize::new(0),
            deletes: Mutex::new(Vec::new()),
            peak_transcribe: AtomicUsize::new(0),
            peak_translate: AtomicUsize::new(0),
            peak_extract: AtomicUsize::new(0),
            active: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Answers calls automatically when `script` returns `Some(items)`.
    pub fn set_script(&self, f: impl Fn(&CallRecord) -> Option<Vec<Item>> + Send + Sync + 'static) {
        *self.script.lock() = Some(Box::new(f));
    }

    pub fn calls(&self) -> Vec<CallRecord> {
        self.all.lock().iter().map(|c| c.record.clone()).collect()
    }
    pub fn calls_of(&self, kind: CallKind) -> Vec<CallRecord> {
        self.calls().into_iter().filter(|c| c.kind == kind).collect()
    }
    pub fn live_calls(&self) -> Vec<Arc<LiveCall>> {
        self.all.lock().clone()
    }

    /// Takes the next pending (unscripted) call of `kind`, waiting while pumping the UI.
    pub fn next(&self, ui: &QueueDispatcher, kind: CallKind) -> Arc<LiveCall> {
        let mut found = None;
        let ok = ui.run_until(Duration::from_secs(10), || {
            let mut p = self.pending.lock();
            if let Some(pos) = p.iter().position(|c| c.record.kind == kind) {
                found = p.remove(pos);
                true
            } else {
                false
            }
        });
        assert!(ok, "no {kind:?} call arrived");
        found.expect("call")
    }

    pub fn has_pending(&self, kind: CallKind) -> bool {
        self.pending.lock().iter().any(|c| c.record.kind == kind)
    }

    fn stream(&self, record: CallRecord, cancel: &CancellationToken) -> EventStream {
        let (tx, rx) = crossbeam_channel::unbounded::<Item>();
        let token = cancel.child();
        let kind = record.kind;
        {
            let mut a = self.active.lock();
            let n = a.entry(kind).or_insert(0);
            *n += 1;
            let peak = match kind {
                CallKind::Transcribe => &self.peak_transcribe,
                CallKind::Translate => &self.peak_translate,
                CallKind::Extract => &self.peak_extract,
            };
            peak.fetch_max(*n, Ordering::SeqCst);
        }
        let live = Arc::new(LiveCall { record: record.clone(), tx: Mutex::new(Some(tx.clone())), cancel: token.clone() });
        self.all.lock().push(live.clone());
        let scripted = self.script.lock().as_ref().and_then(|s| s(&record));
        let auto_extract = kind == CallKind::Extract && self.auto_complete_extract.load(Ordering::SeqCst);
        match scripted {
            Some(items) => {
                for i in items {
                    let _ = tx.send(i);
                }
                live.finish();
            }
            None if auto_extract => live.finish(),
            None => self.pending.lock().push_back(live.clone()),
        }
        drop(tx);
        // A relay ends the stream with Cancelled once the token fires and tracks concurrency.
        let (otx, orx) = crossbeam_channel::unbounded::<Item>();
        let t2 = token.clone();
        let active = self.active.clone();
        std::thread::spawn(move || {
            loop {
                if t2.is_cancelled() {
                    let _ = otx.send(Err(EngineError::Cancelled));
                    break;
                }
                match rx.recv_timeout(Duration::from_millis(5)) {
                    Ok(item) => {
                        if otx.send(item).is_err() {
                            break;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
            }
            if let Some(n) = active.lock().get_mut(&kind) {
                *n = n.saturating_sub(1);
            }
        });
        EventStream::new(orx, token)
    }
}

impl EngineClient for FakeEngine {
    fn extract_audio(&self, video: &Path, out_dir: &Path, cancel: &CancellationToken) -> EventStream {
        let r = CallRecord {
            kind: CallKind::Extract,
            video: Some(video.into()),
            out_dir: Some(out_dir.into()),
            options: None,
            sentences: vec![],
            to: None,
            from: None,
            model: None,
            launch: self.launch.get(),
        };
        self.stream(r, cancel)
    }
    fn transcribe(&self, video: &Path, out_dir: &Path, options: &TranscribeOptions, cancel: &CancellationToken) -> EventStream {
        let r = CallRecord {
            kind: CallKind::Transcribe,
            video: Some(video.into()),
            out_dir: Some(out_dir.into()),
            options: Some(options.clone()),
            sentences: vec![],
            to: None,
            from: None,
            model: Some(options.model.clone()),
            launch: self.launch.get(),
        };
        self.stream(r, cancel)
    }
    fn translate(&self, sentences: &[Sentence], to: &str, from: Option<&str>, model: &str, cancel: &CancellationToken) -> EventStream {
        let r = CallRecord {
            kind: CallKind::Translate,
            video: None,
            out_dir: None,
            options: None,
            sentences: sentences.to_vec(),
            to: Some(to.into()),
            from: from.map(str::to_owned),
            model: Some(model.into()),
            launch: self.launch.get(),
        };
        self.stream(r, cancel)
    }
    fn render(&self, sentences: &[Sentence], format: SubtitleFormat, cancel: &CancellationToken) -> Result<String, EngineError> {
        self.renders.lock().push((sentences.to_vec(), format));
        let delay = *self.render_delay.lock();
        if let Some(d) = delay {
            let deadline = std::time::Instant::now() + d;
            while std::time::Instant::now() < deadline {
                if cancel.is_cancelled() {
                    return Err(EngineError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if let Some(r) = self.render_result.lock().clone() {
            return r;
        }
        Ok(sentences.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("|"))
    }
    fn models(&self, _cancel: &CancellationToken) -> Result<Vec<EngineModel>, EngineError> {
        self.models_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.models.lock().clone())
    }
    fn delete_model(&self, id: &str, _cancel: &CancellationToken) -> Result<i64, EngineError> {
        self.deletes.lock().push(id.into());
        Ok(100)
    }
}

pub struct EngineBackend(pub Arc<FakeEngine>);
impl ModelCatalogBackend for EngineBackend {
    fn models(&self, cancel: &CancellationToken) -> Result<Vec<EngineModel>, EngineError> {
        self.0.models(cancel)
    }
    fn delete_model(&self, id: &str, cancel: &CancellationToken) -> Result<i64, EngineError> {
        self.0.delete_model(id, cancel)
    }
}

// ------------------------------------------------------------------------------------------------- builders

pub fn s(id: i64, start: f64, end: f64, text: &str) -> Sentence {
    Sentence::new(id, start, end, text)
}

pub fn sp(id: i64, start: f64, end: f64, text: &str, speaker: Option<i64>) -> Sentence {
    let mut x = Sentence::new(id, start, end, text);
    x.speaker = speaker;
    x
}

pub fn with_words(mut x: Sentence) -> Sentence {
    let n = x.text.split_whitespace().count().max(1);
    let step = (x.end - x.start) / n as f64;
    x.words = x.text.split_whitespace().enumerate().map(|(i, w)| Word::new(x.start + step * i as f64, x.start + step * (i + 1) as f64, w)).collect();
    x
}

/// `Make.Model(id, kind, languages, speed, accuracy, downloaded)`.
pub fn model(id: &str, kind: &str, languages: Option<&[&str]>, speed: i64, accuracy: i64, downloaded: bool) -> EngineModel {
    EngineModel {
        id: id.into(),
        kind: kind.into(),
        backend: if kind == "translation" { "ctranslate2".into() } else { "whisper".into() },
        languages: languages.map(|l| l.iter().map(|c| (*c).to_owned()).collect()),
        size_mb: 100,
        disk_mb: downloaded.then_some(100),
        speed,
        accuracy,
        license: Some("MIT".into()),
        downloaded,
    }
}

/// Every fallback model marked downloaded.
pub fn downloaded_models() -> Vec<EngineModel> {
    sa_core::models::fallback_models()
        .into_iter()
        .map(|mut m| {
            m.downloaded = true;
            m.disk_mb = Some(m.size_mb);
            m
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------- shell

pub struct FakeShell {
    pub root: PathBuf,
    pub diagnostics: Arc<MemoryDiagnostics>,
    pub memory: AtomicU64,
    pub quick: Mutex<Vec<QuickLocation>>,
    pub unavailable: Mutex<HashSet<PathBuf>>,
    pub revealed: Mutex<Vec<PathBuf>>,
}

impl PlatformShell for FakeShell {
    fn app_data_dir(&self) -> PathBuf {
        self.root.join("appdata")
    }
    fn engine_work_dir(&self) -> PathBuf {
        self.root.join("work")
    }
    fn diagnostics(&self) -> &dyn DiagnosticSink {
        self.diagnostics.as_ref()
    }
    fn system_preferred_languages(&self) -> Vec<String> {
        vec!["tr-TR".into()]
    }
    fn apply_process_ui_language(&self, _language: Option<UiLanguage>) {}
    fn installed_font_families(&self) -> Vec<String> {
        vec![]
    }
    fn quick_locations(&self) -> Vec<QuickLocation> {
        self.quick.lock().clone()
    }
    fn storage_available(&self, path: &Path) -> bool {
        !self.unavailable.lock().iter().any(|p| path.starts_with(p))
    }
    fn reveal_in_file_manager(&self, path: &Path) {
        self.revealed.lock().push(path.into());
    }
    fn open_folder(&self, path: &Path) {
        self.revealed.lock().push(path.into());
    }
    fn total_physical_memory(&self) -> u64 {
        self.memory.load(Ordering::SeqCst)
    }
    fn other_app_instances_running(&self) -> bool {
        false
    }
    fn pick_open_file(&self, _filter: FileFilter) -> LocalBoxFuture<'static, Option<PathBuf>> {
        Box::pin(async { None })
    }
    fn pick_open_files(&self, _filter: FileFilter) -> LocalBoxFuture<'static, Vec<PathBuf>> {
        Box::pin(async { vec![] })
    }
    fn pick_save_file(&self, _suggested_name: &str, _format: SubtitleFormat) -> LocalBoxFuture<'static, Option<PathBuf>> {
        Box::pin(async { None })
    }
    fn confirm(&self, _alert: Alert) -> LocalBoxFuture<'static, AlertResponse> {
        Box::pin(async { None })
    }
}

pub struct FakeThumbs {
    pub calls: Mutex<Vec<PathBuf>>,
}
impl ThumbnailGenerator for FakeThumbs {
    fn generate(&self, path: &Path, _cancel: &CancellationToken) -> Option<Vec<u8>> {
        self.calls.lock().push(path.into());
        Some(vec![0xFF, 0xD8, 0xFF, 1, 2, 3])
    }
    fn max_concurrency(&self) -> usize {
        1
    }
}

// ------------------------------------------------------------------------------------------------- media index

/// Lists real folders; `search`/`all_files` count their calls (privacy assertions).
pub struct FakeIndex {
    pub dirs: Mutex<HashMap<String, DirectorySnapshot>>,
    pub search_calls: AtomicUsize,
    pub all_calls: AtomicUsize,
    pub scans: AtomicUsize,
    /// Folders listed without touching the index (`scan_transient`).
    pub transient_scans: Mutex<Vec<PathBuf>>,
    /// Folders refreshed in memory only (`refresh_in_memory`).
    pub memory_refreshes: Mutex<Vec<PathBuf>>,
    subs: Subscribers<()>,
}

impl FakeIndex {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            dirs: Mutex::new(HashMap::new()),
            search_calls: AtomicUsize::new(0),
            all_calls: AtomicUsize::new(0),
            scans: AtomicUsize::new(0),
            transient_scans: Mutex::new(Vec::new()),
            memory_refreshes: Mutex::new(Vec::new()),
            subs: Subscribers::new(),
        })
    }
    fn list(dir: &Path) -> Option<DirectorySnapshot> {
        let rd = std::fs::read_dir(dir).ok()?;
        let mut subs = Vec::new();
        let mut files = Vec::new();
        let mut names: Vec<(String, PathBuf, bool, u64)> = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            let md = e.metadata().ok()?;
            names.push((e.file_name().to_string_lossy().into_owned(), p, md.is_dir(), md.len()));
        }
        names.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        let all: Vec<String> = names.iter().map(|n| n.0.to_lowercase()).collect();
        for (name, p, is_dir, len) in names {
            if is_dir {
                subs.push(p.to_string_lossy().into_owned());
            } else if sa_enginekit::media_types::is_media(&p) && !sa_enginekit::media_types::is_audio(&p) {
                let stem = Path::new(&name).file_stem().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
                let side = ["srt", "vtt", "ass", "json"].iter().find(|e| all.contains(&format!("{stem}.{e}"))).map(|e| format!(".{e}"));
                files.push(IndexedMediaFile {
                    full_path: p.to_string_lossy().into_owned(),
                    file_name: name,
                    directory_path: dir.to_string_lossy().into_owned(),
                    size_bytes: len as i64,
                    last_write_time_utc: 0,
                    has_sidecar_subtitle: side.is_some(),
                    sidecar_extension: side,
                    indexed_at_utc: 0,
                });
            }
        }
        Some(DirectorySnapshot { directory_path: dir.to_string_lossy().into_owned(), subdirectories: subs, media_files: files, scanned_at_utc: 0 })
    }
    pub fn all(&self) -> Vec<IndexedMediaFile> {
        self.dirs.lock().values().flat_map(|d| d.media_files.clone()).collect()
    }
}

impl MediaIndex for FakeIndex {
    fn scan(&self, dir: &Path, _cancel: &CancellationToken) -> ScanResult {
        self.scans.fetch_add(1, Ordering::SeqCst);
        match Self::list(dir) {
            Some(s) => {
                self.dirs.lock().insert(dir.to_string_lossy().to_lowercase(), s.clone());
                self.subs.notify(&());
                ScanResult::Listed(s)
            }
            None => ScanResult::Missing,
        }
    }
    fn scan_transient(&self, dir: &Path, _cancel: &CancellationToken) -> ScanResult {
        self.transient_scans.lock().push(dir.to_path_buf());
        match Self::list(dir) {
            Some(s) => ScanResult::Listed(s),
            None => ScanResult::Missing,
        }
    }
    fn refresh_in_memory(&self, dir: &Path, _cancel: &CancellationToken) -> ScanResult {
        self.memory_refreshes.lock().push(dir.to_path_buf());
        match Self::list(dir) {
            Some(s) => {
                let key = dir.to_string_lossy().to_lowercase();
                if self.dirs.lock().contains_key(&key) {
                    self.dirs.lock().insert(key, s.clone());
                }
                ScanResult::Listed(s)
            }
            None => ScanResult::Missing,
        }
    }
    fn crawl(&self, root: &Path, _max_depth: u8, _max_folders: usize, cancel: &CancellationToken, _progress: &dyn Fn(usize)) -> usize {
        let _ = self.scan(root, cancel);
        1
    }
    fn cached_snapshot(&self, dir: &Path) -> Option<DirectorySnapshot> {
        self.dirs.lock().get(&dir.to_string_lossy().to_lowercase()).cloned()
    }
    fn total_indexed_files(&self) -> usize {
        self.all().len()
    }
    fn search(&self, query: &str, limit: usize) -> Vec<IndexedMediaFile> {
        self.search_calls.fetch_add(1, Ordering::SeqCst);
        let q = query.to_lowercase();
        self.all().into_iter().filter(|f| f.file_name.to_lowercase().contains(&q)).take(limit).collect()
    }
    fn all_files(&self, limit: usize) -> Vec<IndexedMediaFile> {
        self.all_calls.fetch_add(1, Ordering::SeqCst);
        self.all().into_iter().take(limit).collect()
    }
    fn subscribe_changes(&self, handler: Box<dyn Fn() + Send + Sync>) -> Subscription {
        self.subs.subscribe(move |_| handler())
    }
    fn invalidate_sidecar(&self, video_path: &Path) {
        if let Some(dir) = video_path.parent() {
            let k = dir.to_string_lossy().to_lowercase();
            if self.dirs.lock().contains_key(&k)
                && let Some(s) = Self::list(dir)
            {
                self.dirs.lock().insert(k, s);
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------- profiles

#[derive(Default)]
pub struct ProfState {
    pub users: Vec<(String, String)>,
    pub current: Option<String>,
    pub last: Option<String>,
    pub remember: bool,
    pub generation: u64,
    pub stats: HashMap<String, HashMap<String, VideoUserStats>>,
    pub lists: HashMap<String, Vec<UserVideoList>>,
    pub thumbs: HashMap<String, HashMap<String, Vec<u8>>>,
    pub feed_calls: usize,
    pub logged_out_io: usize,
    pub progress_writes: usize,
}

/// In-memory profile store with the rule-6 contract (logged out: no-ops, empty reads).
pub struct FakeProfiles {
    pub st: Mutex<ProfState>,
    subs: Subscribers<ProfileChange>,
}

fn k(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

impl FakeProfiles {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { st: Mutex::new(ProfState::default()), subs: Subscribers::new() })
    }
    pub fn with_user(user: &str, pw: &str, logged_in: bool) -> Arc<Self> {
        let p = Self::new();
        {
            let mut st = p.st.lock();
            st.users.push((user.into(), pw.into()));
            if logged_in {
                st.current = Some(user.into());
                st.generation = 1;
            }
        }
        p
    }
    pub fn emit(&self, c: ProfileChange) {
        self.subs.notify(&c);
    }
    fn user(&self) -> Option<String> {
        self.st.lock().current.clone()
    }
    fn entry(&self, path: &Path) -> Option<VideoUserStats> {
        let st = self.st.lock();
        let u = st.current.clone()?;
        st.stats.get(&u).and_then(|m| m.get(&k(path))).cloned()
    }
    fn upsert(&self, path: &Path, f: impl FnOnce(&mut VideoUserStats)) -> Option<VideoUserStats> {
        let mut st = self.st.lock();
        let Some(u) = st.current.clone() else {
            st.logged_out_io += 1;
            return None;
        };
        let m = st.stats.entry(u).or_default();
        let e = m.entry(k(path)).or_insert_with(|| VideoUserStats {
            content_fingerprint: None,
            video_path: path.to_string_lossy().into_owned(),
            file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            directory_path: path.parent().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            view_count: 0,
            is_liked: false,
            rating: 0,
            last_position_seconds: 0.0,
            duration_seconds: 0.0,
            last_watched_at: None,
            updated_at: Ticks::now(),
            thumbnail: None,
        });
        f(e);
        Some(e.clone())
    }
    pub fn stats_of(&self, user: &str, path: &Path) -> Option<VideoUserStats> {
        self.st.lock().stats.get(user).and_then(|m| m.get(&k(path))).cloned()
    }
}

impl ProfileStore for FakeProfiles {
    fn is_logged_in(&self) -> bool {
        self.st.lock().current.is_some()
    }
    fn current_username(&self) -> Option<String> {
        self.user()
    }
    fn registered_usernames(&self) -> Vec<String> {
        let mut v: Vec<String> = self.st.lock().users.iter().map(|u| u.0.clone()).collect();
        v.sort_by_key(|a| a.to_lowercase());
        v
    }
    fn last_username(&self) -> Option<String> {
        self.st.lock().last.clone()
    }
    fn remember_me(&self) -> bool {
        self.st.lock().remember
    }
    fn session_generation(&self) -> u64 {
        self.st.lock().generation
    }
    fn start(&self) {}
    fn register(&self, username: &str, password: &SecretString, remember: bool) -> Result<(), ProfileError> {
        if username.trim().is_empty() {
            return Err(ProfileError::EmptyUsername);
        }
        if password.expose().is_empty() {
            return Err(ProfileError::EmptyPassword);
        }
        let mut st = self.st.lock();
        if st.users.iter().any(|u| u.0.eq_ignore_ascii_case(username)) {
            return Err(ProfileError::UsernameTaken);
        }
        st.users.push((username.into(), password.expose().into()));
        st.current = Some(username.into());
        st.last = Some(username.into());
        st.remember = remember;
        st.generation += 1;
        Ok(())
    }
    fn login(&self, username: &str, password: &SecretString, remember: bool) -> Result<(), ProfileError> {
        let mut st = self.st.lock();
        let Some(u) = st.users.iter().find(|u| u.0.eq_ignore_ascii_case(username) && u.1 == password.expose()).cloned() else {
            return Err(ProfileError::InvalidCredentials);
        };
        st.current = Some(u.0.clone());
        st.last = Some(u.0);
        st.remember = remember;
        st.generation += 1;
        Ok(())
    }
    fn logout(&self) -> Result<(), ProfileError> {
        let mut st = self.st.lock();
        st.current = None;
        st.remember = false;
        st.generation += 1;
        Ok(())
    }
    fn delete_profile(&self, username: &str, password: &SecretString) -> Result<(), ProfileError> {
        let mut st = self.st.lock();
        let Some(pos) = st.users.iter().position(|u| u.0.eq_ignore_ascii_case(username) && u.1 == password.expose()) else {
            return Err(ProfileError::InvalidCredentials);
        };
        let name = st.users.remove(pos).0;
        st.stats.remove(&name);
        st.lists.remove(&name);
        if st.current.as_deref() == Some(name.as_str()) {
            st.current = None;
        }
        st.generation += 1;
        Ok(())
    }
    fn record_watch(&self, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        let r = self.upsert(path, |e| {
            e.view_count += 1;
            e.last_watched_at = Some(Ticks::now());
            if duration > 0.0 {
                e.last_position_seconds = position;
                e.duration_seconds = duration;
            }
        });
        if r.is_some() {
            self.subs.notify(&ProfileChange::Local);
        }
        r
    }
    fn update_playback_progress(&self, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        if self.is_logged_in() {
            self.st.lock().progress_writes += 1;
        }
        self.upsert(path, |e| {
            e.last_position_seconds = position;
            e.duration_seconds = duration;
            e.last_watched_at = Some(Ticks::now());
        })
    }
    fn flush_playback_progress(&self) -> bool {
        self.is_logged_in()
    }
    fn set_liked(&self, path: &Path, liked: bool) -> Option<VideoUserStats> {
        let r = self.upsert(path, |e| e.is_liked = liked);
        if r.is_some() {
            self.subs.notify(&ProfileChange::Local);
        }
        r
    }
    fn set_rating(&self, path: &Path, stars: u8) -> Option<VideoUserStats> {
        let r = self.upsert(path, |e| e.rating = i64::from(stars.min(5)));
        if r.is_some() {
            self.subs.notify(&ProfileChange::Local);
        }
        r
    }
    fn stats(&self, path: &Path, _mode: FingerprintMode) -> Option<VideoUserStats> {
        if !self.is_logged_in() {
            self.st.lock().logged_out_io += 1;
            return None;
        }
        Some(self.entry(path).unwrap_or_else(|| VideoUserStats {
            content_fingerprint: None,
            video_path: path.to_string_lossy().into_owned(),
            file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            directory_path: path.parent().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            view_count: 0,
            is_liked: false,
            rating: 0,
            last_position_seconds: 0.0,
            duration_seconds: 0.0,
            last_watched_at: None,
            updated_at: Ticks::MIN,
            thumbnail: None,
        }))
    }
    fn stats_fast_in_memory(&self, path: &Path) -> Option<VideoUserStats> {
        self.entry(path)
    }
    fn clear_watch_history(&self) -> bool {
        let mut st = self.st.lock();
        let Some(u) = st.current.clone() else { return false };
        if let Some(m) = st.stats.get_mut(&u) {
            for e in m.values_mut() {
                e.view_count = 0;
                e.last_watched_at = None;
                e.last_position_seconds = 0.0;
            }
        }
        drop(st);
        self.subs.notify(&ProfileChange::Local);
        true
    }
    fn thumbnail(&self, path: &Path) -> Option<Vec<u8>> {
        let st = self.st.lock();
        let u = st.current.clone()?;
        st.thumbs.get(&u).and_then(|m| m.get(&k(path))).cloned()
    }
    fn set_thumbnail(&self, path: &Path, bytes: &[u8]) -> bool {
        let mut st = self.st.lock();
        let Some(u) = st.current.clone() else {
            st.logged_out_io += 1;
            return false;
        };
        st.thumbs.entry(u).or_default().insert(k(path), bytes.to_vec());
        true
    }
    fn lists(&self) -> Vec<UserVideoList> {
        let st = self.st.lock();
        st.current.as_ref().and_then(|u| st.lists.get(u)).cloned().unwrap_or_default()
    }
    fn create_list(&self, name: &str) -> Result<Option<UserVideoList>, ProfileError> {
        let mut st = self.st.lock();
        let Some(u) = st.current.clone() else { return Ok(None) };
        let n = name.trim();
        if n.is_empty() {
            return Err(ProfileError::EmptyListName);
        }
        let lists = st.lists.entry(u).or_default();
        if lists.iter().any(|l| l.name.to_lowercase() == n.to_lowercase()) {
            return Err(ProfileError::DuplicateListName);
        }
        let l = UserVideoList {
            id: Uuid::new_v4().simple().to_string(),
            name: n.into(),
            video_paths: vec![],
            created_at: Ticks::now(),
            updated_at: Ticks::now(),
        };
        lists.push(l.clone());
        drop(st);
        self.subs.notify(&ProfileChange::Local);
        Ok(Some(l))
    }
    fn rename_list(&self, id: &str, name: &str) -> Result<bool, ProfileError> {
        let mut st = self.st.lock();
        let Some(u) = st.current.clone() else { return Ok(false) };
        let Some(l) = st.lists.entry(u).or_default().iter_mut().find(|l| l.id == id) else { return Err(ProfileError::ListNotFound) };
        l.name = name.trim().into();
        Ok(true)
    }
    fn delete_list(&self, id: &str) -> bool {
        let mut st = self.st.lock();
        let Some(u) = st.current.clone() else { return false };
        let lists = st.lists.entry(u).or_default();
        let before = lists.len();
        lists.retain(|l| l.id != id);
        lists.len() != before
    }
    fn set_in_list(&self, id: &str, path: &Path, is_in: bool) -> bool {
        let mut st = self.st.lock();
        let Some(u) = st.current.clone() else { return false };
        let Some(l) = st.lists.entry(u).or_default().iter_mut().find(|l| l.id == id) else { return false };
        let pk = path.to_string_lossy().into_owned();
        l.video_paths.retain(|p| p.to_lowercase() != pk.to_lowercase());
        if is_in {
            l.video_paths.push(pk);
        }
        true
    }
    fn toggle_in_list(&self, id: &str, path: &Path) -> bool {
        let now_in = !self.lists_containing(path).iter().any(|i| i == id);
        self.set_in_list(id, path, now_in);
        now_in
    }
    fn lists_containing(&self, path: &Path) -> Vec<String> {
        let pk = k(path);
        self.lists().into_iter().filter(|l| l.video_paths.iter().any(|p| p.to_lowercase() == pk)).map(|l| l.id).collect()
    }
    fn feed(&self, query: &FeedQuery, _should_continue: &dyn Fn() -> bool) -> Vec<VideoUserStats> {
        let mut st = self.st.lock();
        st.feed_calls += 1;
        let Some(u) = st.current.clone() else { return vec![] };
        let mut items: Vec<VideoUserStats> = st.stats.get(&u).map(|m| m.values().cloned().collect()).unwrap_or_default();
        drop(st);
        if let VideoFeedFilter::CustomList(Some(id)) = &query.filter {
            for l in self.lists().into_iter().filter(|l| &l.id == id) {
                for p in l.video_paths {
                    if !items.iter().any(|i| i.video_path.to_lowercase() == p.to_lowercase()) {
                        items.push(self.stats(Path::new(&p), FingerprintMode::Compute).expect("stats"));
                    }
                }
            }
        }
        if query.filter == VideoFeedFilter::All {
            for d in &query.discovered {
                if !items.iter().any(|i| i.video_path.to_lowercase() == k(d)) {
                    items.push(self.stats(d, FingerprintMode::Compute).expect("stats"));
                }
            }
        }
        items.retain(|i| match &query.filter {
            VideoFeedFilter::All => true,
            VideoFeedFilter::History => i.has_watched(),
            VideoFeedFilter::Liked => i.is_liked,
            VideoFeedFilter::TopRated => i.rating > 0,
            VideoFeedFilter::MostWatched => i.view_count > 0,
            VideoFeedFilter::CustomList(id) => {
                self.lists().iter().any(|l| Some(&l.id) == id.as_ref() && l.video_paths.iter().any(|p| p.eq_ignore_ascii_case(&i.video_path)))
            }
        });
        if let Some(q) = &query.search {
            let toks: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
            items.retain(|i| toks.iter().all(|t| i.file_name.to_lowercase().contains(t)));
        }
        items.sort_by(|a, b| a.video_path.cmp(&b.video_path));
        items
    }
    fn totals(&self) -> ProfileCounts {
        let st = self.st.lock();
        let Some(u) = st.current.clone() else { return ProfileCounts::default() };
        let mut c = ProfileCounts::default();
        for e in st.stats.get(&u).map(|m| m.values().cloned().collect::<Vec<_>>()).unwrap_or_default() {
            if e.view_count > 0 {
                c.watched += 1;
            }
            c.views += e.view_count;
            if e.is_liked {
                c.liked += 1;
            }
            if e.rating > 0 {
                c.rated += 1;
            }
        }
        c
    }
    fn persist_pending_migrations(&self) -> bool {
        false
    }
    fn subscribe_changes(&self, handler: Box<dyn Fn(ProfileChange) + Send + Sync>) -> Subscription {
        self.subs.subscribe(move |c| handler(*c))
    }
    fn active_vault_key(&self) -> Option<(String, SecretKey32)> {
        self.user().map(|u| (u, SecretKey32::new([7; 32])))
    }
}

// ------------------------------------------------------------------------------------------------- providers

#[derive(Default)]
pub struct ProvState {
    pub providers: Vec<TranslationProviderProfile>,
    pub active: Option<Uuid>,
    pub keys: HashMap<Uuid, String>,
    pub unreadable: HashSet<Uuid>,
    pub refuse_writes: bool,
    pub key_reads: usize,
}

pub struct FakeProviders {
    pub st: Mutex<ProvState>,
    subs: Subscribers<ProviderChange>,
}

impl FakeProviders {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { st: Mutex::new(ProvState::default()), subs: Subscribers::new() })
    }
    fn changed(&self, c: ProviderChange) {
        self.subs.notify(&c);
    }
}

impl ProviderStore for FakeProviders {
    fn providers(&self) -> Vec<TranslationProviderProfile> {
        self.st.lock().providers.clone()
    }
    fn active_provider_id(&self) -> Option<Uuid> {
        self.st.lock().active
    }
    fn load_problem(&self) -> Option<ProviderLoadProblem> {
        None
    }
    fn add(&self, profile: TranslationProviderProfile, key: Option<&str>, set_active: bool) -> TranslationProviderProfile {
        {
            let mut st = self.st.lock();
            st.providers.retain(|p| p.id != profile.id);
            st.providers.push(profile.clone());
            if let Some(k) = key.map(str::trim).filter(|k| !k.is_empty()) {
                st.keys.insert(profile.id, k.into());
            }
            if set_active {
                st.active = Some(profile.id);
            }
        }
        self.changed(ProviderChange::Providers);
        profile
    }
    fn update(&self, profile: TranslationProviderProfile, key: Option<&str>) -> bool {
        {
            let mut st = self.st.lock();
            let Some(p) = st.providers.iter_mut().find(|p| p.id == profile.id) else { return false };
            *p = profile.clone();
            match key.map(str::trim) {
                Some("") => {
                    st.keys.remove(&profile.id);
                }
                Some(k) => {
                    st.keys.insert(profile.id, k.into());
                }
                None => {}
            }
        }
        self.changed(ProviderChange::Providers);
        true
    }
    fn remove(&self, id: Uuid) {
        {
            let mut st = self.st.lock();
            st.providers.retain(|p| p.id != id);
            st.keys.remove(&id);
            if st.active == Some(id) {
                st.active = None;
            }
        }
        self.changed(ProviderChange::Providers);
    }
    fn set_active(&self, id: Option<Uuid>) {
        {
            let mut st = self.st.lock();
            st.active = id.filter(|i| st.providers.iter().any(|p| p.id == *i));
        }
        self.changed(ProviderChange::Active);
    }
    fn update_model(&self, id: Uuid, model: &str) {
        {
            let mut st = self.st.lock();
            if let Some(p) = st.providers.iter_mut().find(|p| p.id == id) {
                p.model = model.into();
            }
        }
        self.changed(ProviderChange::Providers);
    }
    fn set_discovered_models(&self, id: Uuid, models: &[String]) {
        {
            let mut st = self.st.lock();
            if let Some(p) = st.providers.iter_mut().find(|p| p.id == id) {
                p.discovered_models = models.to_vec();
            }
        }
        self.changed(ProviderChange::Providers);
    }
    fn set_key(&self, id: Uuid, key: &str) -> bool {
        let mut st = self.st.lock();
        if st.refuse_writes || !st.providers.iter().any(|p| p.id == id) {
            return false;
        }
        let t = key.trim();
        if t.is_empty() {
            st.keys.remove(&id);
        } else {
            st.keys.insert(id, t.into());
        }
        st.unreadable.remove(&id);
        drop(st);
        self.changed(ProviderChange::Keys);
        true
    }
    fn read_key(&self, id: Uuid) -> ProviderKeyRead {
        let mut st = self.st.lock();
        st.key_reads += 1;
        if st.unreadable.contains(&id) {
            return ProviderKeyRead::Unavailable;
        }
        match st.keys.get(&id) {
            Some(k) => ProviderKeyRead::Found(SecretString::new(k.clone())),
            None => ProviderKeyRead::Absent,
        }
    }
    fn active_external_llm(&self) -> Option<ActiveExternalLlm> {
        let st = self.st.lock();
        let id = st.active?;
        let p = st.providers.iter().find(|p| p.id == id)?;
        sa_enginekit::external_llm::ExternalLlmServer::normalize_base_url(&p.base_url)?;
        if st.unreadable.contains(&id) {
            return None;
        }
        Some(ActiveExternalLlm {
            llm: ExternalLlm { url: p.base_url.trim().into(), model: p.model.trim().into(), api_key: st.keys.get(&id).cloned().unwrap_or_default() },
            async_translation: p.async_translation,
        })
    }
    fn subscribe(&self, handler: Box<dyn Fn(ProviderChange) + Send + Sync>) -> Subscription {
        self.subs.subscribe(move |c| handler(*c))
    }
}

pub fn provider(preset: &str, name: &str, url: &str, model: &str, async_translation: bool) -> TranslationProviderProfile {
    TranslationProviderProfile {
        id: Uuid::new_v4(),
        name: name.into(),
        preset_id: preset.into(),
        base_url: url.into(),
        model: model.into(),
        async_translation,
        discovered_models: vec![],
    }
}

// ------------------------------------------------------------------------------------------------- env

pub struct Env {
    pub ui: Arc<QueueDispatcher>,
    pub settings: Arc<InMemorySettingsStore>,
    pub launch: Arc<EngineLaunchState>,
    pub engine: Arc<FakeEngine>,
    pub shell: Arc<FakeShell>,
    pub profiles: Arc<FakeProfiles>,
    pub index: Arc<FakeIndex>,
    pub providers: Arc<FakeProviders>,
    pub bus: Arc<dyn SyncBus>,
    pub diagnostics: Arc<MemoryDiagnostics>,
    pub thumbs: Arc<FakeThumbs>,
    pub policy: PlatformPolicy,
    pub with_engine: bool,
    pub dir: tempfile::TempDir,
}

impl Env {
    pub fn new(policy: PlatformPolicy) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let diagnostics = MemoryDiagnostics::new();
        let launch = EngineLaunchState::new(EngineLaunchSettings::default());
        let shell = Arc::new(FakeShell {
            root: dir.path().to_path_buf(),
            diagnostics: diagnostics.clone(),
            memory: AtomicU64::new(64 * 1024 * 1024 * 1024),
            quick: Mutex::new(vec![]),
            unavailable: Mutex::new(HashSet::new()),
            revealed: Mutex::new(vec![]),
        });
        std::fs::create_dir_all(shell.engine_work_dir()).ok();
        let settings = InMemorySettingsStore::new();
        // Job tests run online unless a test turns offline mode on (C# `Make.ViewModel`).
        settings.set_bool(sa_core::settings::keys::OFFLINE_MODE, false);
        Self {
            ui: QueueDispatcher::new(),
            settings,
            engine: FakeEngine::new(launch.clone()),
            launch,
            shell,
            profiles: FakeProfiles::new(),
            index: FakeIndex::new(),
            providers: FakeProviders::new(),
            bus: sa_sync::InProcessBus::new(),
            diagnostics,
            thumbs: Arc::new(FakeThumbs { calls: Mutex::new(vec![]) }),
            policy,
            with_engine: true,
            dir,
        }
    }

    pub fn win() -> Self {
        Self::new(PlatformPolicy::windows())
    }
    pub fn mac() -> Self {
        Self::new(PlatformPolicy::mac())
    }

    /// `rel` with `/` separators, joined with the platform separator.
    pub fn path(&self, rel: &str) -> PathBuf {
        let mut p = self.dir.path().to_path_buf();
        for part in rel.split('/') {
            p.push(part);
        }
        p
    }

    /// Creates a file (and its folder).
    pub fn touch(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.path(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).ok();
        }
        std::fs::write(&p, content).expect("write");
        p
    }

    pub fn services(&self) -> AppServices {
        let ui: Arc<dyn UiDispatcher> = self.ui.clone();
        AppServices {
            policy: Arc::new(self.policy.clone()),
            settings: self.settings.clone(),
            ui,
            engine: self.with_engine.then(|| self.engine.clone() as Arc<dyn EngineClient>),
            launch_state: self.launch.clone(),
            profiles: self.profiles.clone(),
            media_index: self.index.clone(),
            providers: self.providers.clone(),
            shell: self.shell.clone(),
            bus: self.bus.clone(),
            diagnostics: self.diagnostics.clone(),
            thumbnails: self.thumbs.clone(),
        }
    }

    pub fn catalog(&self) -> Rc<ModelCatalog> {
        let ui: Arc<dyn UiDispatcher> = self.ui.clone();
        let backend: Option<Arc<dyn ModelCatalogBackend>> =
            self.with_engine.then(|| Arc::new(EngineBackend(self.engine.clone())) as Arc<dyn ModelCatalogBackend>);
        let options = if self.policy.catalog_chain == CatalogChain::Windows { ModelCatalogOptions::WINDOWS } else { ModelCatalogOptions::MAC };
        Rc::new(ModelCatalog::with_options(backend, ui, options))
    }

    /// A view model with a loaded catalog (the engine's models).
    pub fn vm(&self) -> (SubtitleViewModel, Rc<ModelCatalog>) {
        let catalog = self.catalog();
        if self.with_engine {
            catalog.load();
            let c = catalog.clone();
            assert!(self.ui.run_until(Duration::from_secs(5), || c.has_loaded()), "catalog loaded");
        }
        let vm = SubtitleViewModel::new(self.services(), catalog.clone());
        self.pump();
        (vm, catalog)
    }

    pub fn pump(&self) {
        self.ui.run_until(Duration::from_millis(30), || false);
    }

    /// `Make.Eventually` (pumps the UI thread).
    pub fn eventually(&self, what: &str, cond: impl FnMut() -> bool) {
        assert!(self.ui.run_until(Duration::from_secs(10), cond), "timed out waiting for: {what}");
    }

    pub fn next(&self, kind: CallKind) -> Arc<LiveCall> {
        self.engine.next(&self.ui, kind)
    }
}

/// The ids of a sentence list.
pub fn ids(v: &[Sentence]) -> Vec<i64> {
    v.iter().map(|x| x.id).collect()
}

pub fn texts(v: &[Sentence]) -> Vec<String> {
    v.iter().map(|x| x.text.clone()).collect()
}
