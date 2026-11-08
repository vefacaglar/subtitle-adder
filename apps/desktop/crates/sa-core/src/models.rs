//! Model catalog, labels, language list and selection helpers (win-providers §10, mac-viewmodels §8,
//! mac-ui §12, UNIFIED.md §3.14). Shared by the toolbar pickers, Settings ▸ Models and the jobs.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;

use crate::cancel::CancellationToken;
use crate::dispatch::UiDispatcher;
use crate::engine_types::{EngineError, EngineModel};
use crate::l10n::{LanguageState, LocalizedMessage, UiLanguage};
use crate::subscription::{Subscribers, Subscription};

/// The engine operations the catalog needs (implemented by `sa_enginekit::EngineClient`; tests use a fake).
/// Blocking; called on worker threads only.
pub trait ModelCatalogBackend: Send + Sync {
    fn models(&self, cancel: &CancellationToken) -> Result<Vec<EngineModel>, EngineError>;
    /// MB freed.
    fn delete_model(&self, id: &str, cancel: &CancellationToken) -> Result<i64, EngineError>;
}

/// EU25 language list of Parakeet/Canary (fallback catalog).
pub const EU25: [&str; 25] = [
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl",
    "es", "sv", "ru", "uk",
];
/// Translation languages.
pub const TR9: [&str; 9] = ["tr", "en", "pt", "es", "ru", "de", "fr", "ja", "zh"];

/// Ids.
pub const DIARIZATION_MODEL_ID: &str = "speaker-diarization";
pub const DEFAULT_MODEL: &str = "large-v3";
pub const DEFAULT_TRANSLATION_MODEL: &str = "madlad-3b";

/// The fallback list shown before `models --json` answered (engine catalog order; all not downloaded).
pub fn fallback_models() -> Vec<EngineModel> {
    fn m(id: &str, kind: &str, backend: &str, languages: Option<&[&str]>, size_mb: i64, speed: i64, accuracy: i64, license: &str) -> EngineModel {
        EngineModel {
            id: id.to_owned(),
            kind: kind.to_owned(),
            backend: backend.to_owned(),
            languages: languages.map(|l| l.iter().map(|c| (*c).to_owned()).collect()),
            size_mb,
            disk_mb: None,
            speed,
            accuracy,
            license: Some(license.to_owned()),
            downloaded: false,
        }
    }
    const T: &str = "transcription";
    vec![
        m("base", T, "whisper", None, 145, 5, 2, "MIT"),
        m("gigaam-v3", T, "onnx", Some(&["ru"]), 216, 5, 4, "MIT"),
        m("small", T, "whisper", None, 465, 4, 3, "MIT"),
        m("parakeet-v3", T, "onnx", Some(&EU25), 640, 5, 4, "CC-BY-4.0"),
        m("canary-1b-v2", T, "onnx", Some(&EU25), 983, 2, 4, "CC-BY-4.0"),
        m("medium", T, "whisper", None, 1500, 3, 4, "MIT"),
        m("large-v3-turbo", T, "whisper", None, 1600, 2, 4, "MIT"),
        m("large-v3", T, "whisper", None, 3000, 1, 5, "MIT"),
        m(DIARIZATION_MODEL_ID, "diarization", "sherpa", None, 29, 4, 3, "MIT AND Apache-2.0"),
        m("nllb-1.3b", "translation", "ctranslate2", Some(&TR9), 1336, 3, 4, "CC-BY-NC-4.0"),
        m("qwen3.5-4b", "translation", "llama", Some(&TR9), 2614, 3, 4, "Apache-2.0"),
        m("madlad-3b", "translation", "ctranslate2", Some(&TR9), 2839, 2, 5, "Apache-2.0"),
        m("gemma-4-e4b", "translation", "llama", Some(&TR9), 4747, 2, 5, "Apache-2.0"),
        m("qwen3.5-9b", "translation", "llama", Some(&TR9), 5417, 2, 5, "Apache-2.0"),
        m("gemma-4-12b", "translation", "llama", Some(&TR9), 6792, 1, 5, "Apache-2.0"),
    ]
}

/// A row of Settings ▸ Models (texts computed at construction in the then-current language).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    pub id: String,
    pub name: String,
    pub language_summary: String,
    pub storage_status: String,
    pub license_text: String,
    pub is_non_commercial: bool,
    pub error_text: Option<String>,
    pub shows_delete_button: bool,
    pub can_delete: bool,
    pub delete_help: String,
    pub delete_confirm_title: String,
    pub delete_confirm_message: String,
    /// macOS `Hız %lld/5 · Doğruluk %lld/5` (None when a rating is unknown).
    pub rating_text: Option<String>,
    /// The model the row shows.
    pub model: EngineModel,
}

impl ModelRow {
    /// C# `ModelRow(model, is_deleting, is_active, error)`.
    pub fn new(model: &EngineModel, is_deleting: bool, is_active: bool, error: Option<&LocalizedMessage>) -> Self {
        let name = ModelLabel::name(&model.id);
        Self {
            id: model.id.clone(),
            delete_confirm_title: crate::l!("%@ modeli silinsin mi?", name.as_str()),
            name,
            language_summary: ModelLabel::language_summary(model),
            storage_status: ModelLabel::storage_status(model),
            license_text: ModelLabel::license_text(model),
            is_non_commercial: model.has_non_commercial_license(),
            error_text: error.map(LocalizedMessage::text),
            shows_delete_button: model.downloaded && !is_deleting,
            can_delete: model.downloaded && !is_deleting && !is_active,
            delete_help: if is_active {
                crate::l10n::l("Süren bir iş bu modeli kullanıyor; iş bitince silebilirsin.")
            } else {
                crate::l10n::l("Modeli diskten sil; tekrar kullanırsan yeniden indirilir.")
            },
            delete_confirm_message: crate::l!(
                "%@ boşalır. Tekrar kullanırsan yeniden indirilir.",
                ModelLabel::size(model.disk_mb.unwrap_or(model.size_mb))
            ),
            rating_text: ModelLabel::rating_text(model),
            model: model.clone(),
        }
    }
}

/// What changed on the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelCatalogChange {
    Models,
    IsLoading,
    LoadError,
    Deleting,
    DeleteErrors,
    ActiveModels,
    Rows,
    /// macOS: the "… boşaltıldı." notice appeared or went away.
    DeleteNotice,
}

/// Per-platform knobs of the shared catalog (UNIFIED.md §3.14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCatalogOptions {
    /// macOS lists llama translation models first, then the classic ones; Windows keeps catalog order.
    pub llama_translation_first: bool,
    /// macOS hides the delete notice after 8 s; `None` keeps it until dismissed (Windows shows its own closable
    /// InfoBar from the freed MB and ignores the notice).
    pub notice_duration: Option<Duration>,
}

impl ModelCatalogOptions {
    pub const WINDOWS: Self = Self { llama_translation_first: false, notice_duration: None };
    pub const MAC: Self = Self { llama_translation_first: true, notice_duration: Some(Duration::from_secs(8)) };

    /// The options of the platform this binary is built for.
    pub const fn native() -> Self {
        if cfg!(target_os = "macos") { Self::MAC } else { Self::WINDOWS }
    }
}

struct CatalogState {
    models: Vec<EngineModel>,
    is_loading: bool,
    has_loaded: bool,
    load_error: Option<LocalizedMessage>,
    deleting: HashSet<String>,
    delete_errors: HashMap<String, LocalizedMessage>,
    active_models: HashSet<String>,
    delete_notice: Option<LocalizedMessage>,
    notice_epoch: u64,
}

struct CatalogInner {
    state: Mutex<CatalogState>,
    backend: Option<Arc<dyn ModelCatalogBackend>>,
    ui: Arc<dyn UiDispatcher>,
    changed: Subscribers<ModelCatalogChange>,
    options: ModelCatalogOptions,
    /// Cancelled when the catalog is dropped (the window closed: no error, retried on the next open).
    cancel: CancellationToken,
}

impl CatalogInner {
    fn notify(&self, changes: &[ModelCatalogChange]) {
        for c in changes {
            self.changed.notify(c);
        }
    }

    /// Runs `f` on the UI thread with the inner state, unless the catalog is gone.
    fn post(weak: Weak<CatalogInner>, ui: &Arc<dyn UiDispatcher>, f: impl FnOnce(&CatalogInner) + Send + 'static) {
        ui.post(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                f(&inner);
            }
        }));
    }
}

/// Single source of truth for models (UI thread; work on workers, results posted through the dispatcher).
///
/// Every notification is raised on the UI thread (the dispatcher's thread), also for cancelled loads.
pub struct ModelCatalog {
    inner: Arc<CatalogInner>,
}

impl Drop for ModelCatalog {
    fn drop(&mut self) {
        self.inner.cancel.cancel();
    }
}

impl ModelCatalog {
    /// `backend = None` when no engine was located (load reports NotFound). Native platform options.
    pub fn new(backend: Option<Arc<dyn ModelCatalogBackend>>, ui: Arc<dyn UiDispatcher>) -> Self {
        Self::with_options(backend, ui, ModelCatalogOptions::native())
    }

    /// [`new`](Self::new) with explicit per-platform options (tests of both platforms).
    pub fn with_options(backend: Option<Arc<dyn ModelCatalogBackend>>, ui: Arc<dyn UiDispatcher>, options: ModelCatalogOptions) -> Self {
        Self {
            inner: Arc::new(CatalogInner {
                state: Mutex::new(CatalogState {
                    models: fallback_models(),
                    is_loading: false,
                    has_loaded: false,
                    load_error: None,
                    deleting: HashSet::new(),
                    delete_errors: HashMap::new(),
                    active_models: HashSet::new(),
                    delete_notice: None,
                    notice_epoch: 0,
                }),
                backend,
                ui,
                changed: Subscribers::new(),
                options,
                cancel: CancellationToken::new(),
            }),
        }
    }

    /// `true` when an engine backs the catalog.
    pub fn has_engine(&self) -> bool {
        self.inner.backend.is_some()
    }
    pub fn options(&self) -> ModelCatalogOptions {
        self.inner.options
    }
    pub fn models(&self) -> Vec<EngineModel> {
        self.inner.state.lock().models.clone()
    }
    pub fn is_loading(&self) -> bool {
        self.inner.state.lock().is_loading
    }
    /// The list came from the engine at least once (until then the download flags mean nothing).
    pub fn has_loaded(&self) -> bool {
        self.inner.state.lock().has_loaded
    }
    pub fn load_error(&self) -> Option<LocalizedMessage> {
        self.inner.state.lock().load_error.clone()
    }
    /// Ids being deleted.
    pub fn deleting(&self) -> HashSet<String> {
        self.inner.state.lock().deleting.clone()
    }
    pub fn is_deleting(&self, id: &str) -> bool {
        self.inner.state.lock().deleting.contains(id)
    }
    pub fn active_models(&self) -> HashSet<String> {
        self.inner.state.lock().active_models.clone()
    }
    pub fn is_active(&self, id: &str) -> bool {
        self.inner.state.lock().active_models.contains(id)
    }

    /// Starts `models --json` (no-op while loading). A non-empty result replaces the list; errors keep it.
    pub fn load(&self) {
        let Some(backend) = self.inner.backend.clone() else {
            self.inner.state.lock().load_error = Some(LocalizedMessage::error(&EngineError::NotFound));
            self.inner.notify(&[ModelCatalogChange::LoadError]);
            return;
        };
        {
            let mut g = self.inner.state.lock();
            if g.is_loading {
                return;
            }
            g.is_loading = true;
        }
        self.inner.notify(&[ModelCatalogChange::IsLoading]);
        let weak = Arc::downgrade(&self.inner);
        let ui = self.inner.ui.clone();
        let cancel = self.inner.cancel.clone();
        let work = move || {
            let result = backend.models(&cancel);
            CatalogInner::post(weak, &ui, move |inner| {
                let mut changes = Vec::new();
                {
                    let mut g = inner.state.lock();
                    match result {
                        Ok(list) => {
                            if !list.is_empty() {
                                if g.models != list {
                                    changes.extend([ModelCatalogChange::Models, ModelCatalogChange::Rows]);
                                }
                                g.models = list;
                                g.has_loaded = true;
                            }
                            if g.load_error.take().is_some() {
                                changes.push(ModelCatalogChange::LoadError);
                            }
                        }
                        Err(EngineError::Cancelled) => {}
                        Err(e) => {
                            let message = LocalizedMessage::error(&e);
                            if g.load_error.as_ref() != Some(&message) {
                                changes.push(ModelCatalogChange::LoadError);
                            }
                            g.load_error = Some(message);
                        }
                    }
                    g.is_loading = false;
                    changes.push(ModelCatalogChange::IsLoading);
                }
                inner.notify(&changes);
            });
        };
        spawn_or_run("model-catalog-load", work);
    }

    pub fn transcription_models(&self) -> Vec<EngineModel> {
        self.filtered(EngineModel::is_transcription)
    }
    /// Windows: catalog order. macOS: llama models first, then the classic (CTranslate2) ones.
    pub fn translation_models(&self) -> Vec<EngineModel> {
        if self.inner.options.llama_translation_first {
            let mut llama = self.llama_translation_models();
            llama.extend(self.classic_translation_models());
            llama
        } else {
            self.filtered(EngineModel::is_translation)
        }
    }
    /// Translation models run by llama.cpp (Qwen, Gemma), catalog order.
    pub fn llama_translation_models(&self) -> Vec<EngineModel> {
        self.filtered(|m| m.is_translation() && m.is_llama())
    }
    /// CTranslate2 translation models (MADLAD, NLLB), catalog order.
    pub fn classic_translation_models(&self) -> Vec<EngineModel> {
        self.filtered(|m| m.is_translation() && !m.is_llama())
    }
    pub fn diarization_models(&self) -> Vec<EngineModel> {
        self.filtered(EngineModel::is_diarization)
    }
    /// Transcription models for a language: `""` → whisper only, else `supports(code)`.
    pub fn models_for(&self, language: &str) -> Vec<EngineModel> {
        if language.is_empty() {
            self.filtered(|m| m.is_transcription() && m.detects_language())
        } else {
            self.filtered(|m| m.is_transcription() && m.supports(language))
        }
    }
    pub fn model(&self, id: &str) -> Option<EngineModel> {
        self.inner.state.lock().models.iter().find(|m| m.id == id).cloned()
    }

    fn filtered(&self, f: impl Fn(&EngineModel) -> bool) -> Vec<EngineModel> {
        self.inner.state.lock().models.iter().filter(|m| f(m)).cloned().collect()
    }

    /// Models a running job uses (incl. `speaker-diarization`): they cannot be deleted.
    pub fn set_active_models(&self, ids: HashSet<String>) {
        {
            let mut g = self.inner.state.lock();
            if g.active_models == ids {
                return;
            }
            g.active_models = ids;
        }
        self.inner.notify(&[ModelCatalogChange::ActiveModels, ModelCatalogChange::Rows]);
    }
    /// Downloaded, not being deleted and not used by a running job.
    pub fn can_delete(&self, id: &str) -> bool {
        let g = self.inner.state.lock();
        g.models.iter().any(|m| m.id == id && m.downloaded) && !g.deleting.contains(id) && !g.active_models.contains(id)
    }

    /// Deletes a downloaded model; `on_done(Some(freed_mb))` on success, `None` when nothing was deleted (not
    /// downloaded, busy, in use, no engine, or an error kept in [`delete_errors`](Self::delete_errors)). `on_done`
    /// runs on the UI thread (inline when the call is refused at once).
    pub fn delete(&self, id: &str, on_done: Box<dyn FnOnce(Option<i64>) + Send>) {
        if !self.can_delete(id) {
            on_done(None);
            return;
        }
        let Some(backend) = self.inner.backend.clone() else {
            self.inner.state.lock().delete_errors.insert(id.to_owned(), LocalizedMessage::error(&EngineError::NotFound));
            self.inner.notify(&[ModelCatalogChange::DeleteErrors, ModelCatalogChange::Rows]);
            on_done(None);
            return;
        };
        let had_error = {
            let mut g = self.inner.state.lock();
            g.deleting.insert(id.to_owned());
            g.delete_errors.remove(id).is_some()
        };
        let mut changes = vec![ModelCatalogChange::Deleting];
        if had_error {
            changes.push(ModelCatalogChange::DeleteErrors);
        }
        changes.push(ModelCatalogChange::Rows);
        self.inner.notify(&changes);

        let weak = Arc::downgrade(&self.inner);
        let ui = self.inner.ui.clone();
        let cancel = self.inner.cancel.clone();
        let id = id.to_owned();
        let work = move || {
            let result = backend.delete_model(&id, &cancel);
            let weak_notice = weak.clone();
            CatalogInner::post(weak, &ui, move |inner| {
                let mut changes = Vec::new();
                let freed = {
                    let mut g = inner.state.lock();
                    let freed = match result {
                        Ok(freed) => {
                            g.delete_notice = Some(delete_notice_for(&id, freed));
                            g.notice_epoch += 1;
                            changes.push(ModelCatalogChange::DeleteNotice);
                            if let Some(m) = g.models.iter_mut().find(|m| m.id == id) {
                                m.downloaded = false;
                                m.disk_mb = None;
                            }
                            changes.push(ModelCatalogChange::Models);
                            Some(freed)
                        }
                        Err(EngineError::Cancelled) => None,
                        Err(e) => {
                            g.delete_errors.insert(id.clone(), LocalizedMessage::error(&e));
                            changes.push(ModelCatalogChange::DeleteErrors);
                            None
                        }
                    };
                    g.deleting.remove(&id);
                    changes.extend([ModelCatalogChange::Deleting, ModelCatalogChange::Rows]);
                    if freed.is_some()
                        && let Some(duration) = inner.options.notice_duration
                    {
                        schedule_notice_dismissal(weak_notice, inner.ui.clone(), g.notice_epoch, duration);
                    }
                    freed
                };
                inner.notify(&changes);
                on_done(freed);
            });
        };
        spawn_or_run("model-catalog-delete", work);
    }

    pub fn delete_errors(&self) -> HashMap<String, LocalizedMessage> {
        self.inner.state.lock().delete_errors.clone()
    }

    /// macOS: the result of the last successful deletion (`1,5 GB boşaltıldı.`), until dismissed or timed out.
    pub fn delete_notice(&self) -> Option<LocalizedMessage> {
        self.inner.state.lock().delete_notice.clone()
    }

    /// Hides the delete notice (and cancels its timer).
    pub fn dismiss_delete_notice(&self) {
        let had = {
            let mut g = self.inner.state.lock();
            g.notice_epoch += 1;
            g.delete_notice.take().is_some()
        };
        if had {
            self.inner.notify(&[ModelCatalogChange::DeleteNotice]);
        }
    }

    /// Every row (catalog order).
    pub fn rows(&self) -> Vec<ModelRow> {
        self.rows_of(|_| true)
    }
    /// "Altyazı modelleri" section.
    pub fn transcription_rows(&self) -> Vec<ModelRow> {
        self.rows_for(self.transcription_models())
    }
    /// "Konuşmacı ayırma" section.
    pub fn diarization_rows(&self) -> Vec<ModelRow> {
        self.rows_for(self.diarization_models())
    }
    /// "Çeviri modelleri" section (platform order, see [`translation_models`](Self::translation_models)).
    pub fn translation_rows(&self) -> Vec<ModelRow> {
        self.rows_for(self.translation_models())
    }

    fn rows_of(&self, f: impl Fn(&EngineModel) -> bool) -> Vec<ModelRow> {
        let models = self.filtered(f);
        self.rows_for(models)
    }

    fn rows_for(&self, models: Vec<EngineModel>) -> Vec<ModelRow> {
        let (deleting, active, errors) = {
            let g = self.inner.state.lock();
            (g.deleting.clone(), g.active_models.clone(), g.delete_errors.clone())
        };
        models
            .iter()
            .map(|m| ModelRow::new(m, deleting.contains(&m.id), active.contains(&m.id), errors.get(&m.id)))
            .collect()
    }

    /// Σ over downloaded models of `disk_mb ?? size_mb`.
    pub fn total_disk_mb(&self) -> i64 {
        self.inner.state.lock().models.iter().filter(|m| m.downloaded).map(|m| m.disk_mb.unwrap_or(m.size_mb)).sum()
    }

    /// `Toplam %@ disk kullanılıyor.` / `Henüz indirilmiş model yok.`
    pub fn total_disk_text(&self) -> String {
        let total = self.total_disk_mb();
        if total > 0 {
            crate::l!("Toplam %@ disk kullanılıyor.", ModelLabel::size(total))
        } else {
            crate::l10n::l("Henüz indirilmiş model yok.")
        }
    }
    /// Rebuild rows/texts after a UI language switch.
    pub fn refresh_localization(&self) {
        self.inner.notify(&[
            ModelCatalogChange::LoadError,
            ModelCatalogChange::DeleteErrors,
            ModelCatalogChange::DeleteNotice,
            ModelCatalogChange::Models,
            ModelCatalogChange::Rows,
        ]);
    }
    pub fn subscribe(&self, handler: impl Fn(&ModelCatalogChange) + Send + Sync + 'static) -> Subscription {
        self.inner.changed.subscribe(handler)
    }
}

/// Text for a finished deletion (mac-viewmodels §8.1): 0 MB freed (every file shared with another model) says so
/// instead of "0 MB boşaltıldı.".
pub fn delete_notice_for(id: &str, freed_mb: i64) -> LocalizedMessage {
    let id_owned = id.to_owned();
    if freed_mb <= 0 {
        return LocalizedMessage::new(format!("deleted-shared:{id}"), move || {
            crate::l!("%@ silindi; dosyalarını başka bir model de kullandığı için yer açılmadı.", ModelLabel::name(&id_owned))
        });
    }
    LocalizedMessage::new(format!("deleted-freed:{id}:{freed_mb}"), move || crate::l!("%@ boşaltıldı.", ModelLabel::size(freed_mb)))
}

fn schedule_notice_dismissal(weak: Weak<CatalogInner>, ui: Arc<dyn UiDispatcher>, epoch: u64, duration: Duration) {
    let spawned = std::thread::Builder::new().name("model-catalog-notice".into()).spawn(move || {
        std::thread::sleep(duration);
        if weak.strong_count() == 0 {
            return;
        }
        CatalogInner::post(weak, &ui, move |inner| {
            let cleared = {
                let mut g = inner.state.lock();
                if g.notice_epoch != epoch {
                    return;
                }
                g.delete_notice.take().is_some()
            };
            if cleared {
                inner.notify(&[ModelCatalogChange::DeleteNotice]);
            }
        });
    });
    let _ = spawned;
}

/// Engine calls run on a worker thread (never on the UI thread); if no thread can be started the work runs inline.
fn spawn_or_run(name: &str, work: impl FnOnce() + Send + 'static) {
    let slot = Arc::new(Mutex::new(Some(work)));
    let thread_slot = slot.clone();
    let spawned = std::thread::Builder::new().name(name.to_owned()).spawn(move || {
        if let Some(w) = thread_slot.lock().take() {
            w();
        }
    });
    if spawned.is_err()
        && let Some(w) = slot.lock().take()
    {
        w();
    }
}

/// Model display texts (win-providers §10.2 = mac-ui §12.2).
pub struct ModelLabel;

impl ModelLabel {
    /// Catalog-key name per id (`base` → `Hızlı`, …); unknown id → the id.
    pub fn name(id: &str) -> String {
        let key = match id {
            "base" => "Hızlı",
            "small" => "Dengeli",
            "medium" => "Doğru",
            "large-v3-turbo" => "Çok doğru (turbo)",
            "large-v3" => "En doğru",
            "parakeet-v3" => "Parakeet v3 (hızlı, çok dilli)",
            "canary-1b-v2" => "Canary (çok dilli)",
            "gigaam-v3" => "GigaAM (Rusça)",
            "nllb-1.3b" => "NLLB-200 1.3B",
            "qwen3.5-4b" => "Qwen 3.5 4B",
            "madlad-3b" => "MADLAD-400 3B",
            "gemma-4-e4b" => "Gemma 4 E4B",
            "qwen3.5-9b" => "Qwen 3.5 9B",
            "gemma-4-12b" => "Gemma 4 12B",
            "speaker-diarization" => "Konuşmacı ayırma",
            _ => return id.to_owned(),
        };
        crate::l10n::l(key)
    }
    /// `"{name} · {detail}"`.
    pub fn menu_title(model: &EngineModel) -> String {
        format!("{} · {}", Self::name(&model.id), Self::detail(model))
    }
    /// macOS translation menu row: the menu title plus ` · Ticari olmayan lisans` for non-commercial licenses.
    pub fn translation_menu_title(model: &EngineModel) -> String {
        let title = Self::menu_title(model);
        if model.has_non_commercial_license() {
            format!("{title} · {}", crate::l10n::l("Ticari olmayan lisans"))
        } else {
            title
        }
    }
    /// `hazır` / `ilk kullanımda ~%@ indirilir`.
    pub fn detail(model: &EngineModel) -> String {
        if model.downloaded {
            crate::l10n::l("hazır")
        } else {
            crate::l!("ilk kullanımda ~%@ indirilir", Self::size(model.size_mb))
        }
    }
    /// `Dilden bağımsız` (diarization) / `Tüm diller` / the first three codes (`tr, en, pt…`).
    pub fn language_summary(model: &EngineModel) -> String {
        if model.is_diarization() {
            return crate::l10n::l("Dilden bağımsız");
        }
        let Some(languages) = &model.languages else { return crate::l10n::l("Tüm diller") };
        let shown = languages.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        if languages.len() > 3 { format!("{shown}…") } else { shown }
    }
    /// macOS `Hız %lld/5 · Doğruluk %lld/5`; `None` when speed or accuracy is unknown (≤ 0).
    pub fn rating_text(model: &EngineModel) -> Option<String> {
        if model.speed <= 0 || model.accuracy <= 0 {
            return None;
        }
        Some(crate::l!("Hız %lld/5 · Doğruluk %lld/5", model.speed.min(5), model.accuracy.min(5)))
    }
    pub fn storage_status(model: &EngineModel) -> String {
        if model.downloaded {
            crate::l!("Yüklü — diskte %@", Self::size(model.disk_mb.unwrap_or(model.size_mb)))
        } else {
            crate::l!("Yüklü değil — ~%@ indirilir", Self::size(model.size_mb))
        }
    }
    pub fn license_text(model: &EngineModel) -> String {
        let license = model.license.clone().unwrap_or_else(|| crate::l10n::l("bilinmiyor"));
        crate::l!("Lisans: %@", license)
    }
    /// `465 MB`, `2,8 GB` (tr) / `2.8 GB` (en), `3 GB`; integer tenths rounding (never `{:.1}`).
    pub fn size_text(mb: i64, language: UiLanguage) -> String {
        if mb < 1000 {
            return format!("{mb} MB");
        }
        let tenths = (mb * 10 + 500) / 1000;
        let sep = crate::text::decimal_separator(language);
        let text = if tenths % 10 == 0 { format!("{}", tenths / 10) } else { format!("{}{sep}{}", tenths / 10, tenths % 10) };
        format!("{text} GB")
    }
    /// [`size_text`](Self::size_text) in the current UI language.
    pub fn size(mb: i64) -> String {
        Self::size_text(mb, LanguageState::shared().language())
    }
}

/// Picks the most accurate model: max by `(accuracy, speed)`, first wins ties.
pub fn most_accurate(models: &[EngineModel]) -> Option<&EngineModel> {
    let mut best: Option<&EngineModel> = None;
    for m in models {
        if best.is_none_or(|b| (m.accuracy, m.speed) > (b.accuracy, b.speed)) {
            best = Some(m);
        }
    }
    best
}

/// Toolbar language picker entry (`""` = automatic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubtitleLanguage {
    pub code: &'static str,
    title_key: &'static str,
    short_key: &'static str,
    target_key: &'static str,
}

impl SubtitleLanguage {
    /// Automatic, tr, pt, es, ru, en, de, fr, ja, zh.
    pub const ALL: [SubtitleLanguage; 10] = [
        Self::new("", "Otomatik", "Otomatik algılama", "Otomatik"),
        Self::new("tr", "Türkçe (tr)", "Türkçe", "Türkçe"),
        Self::new("pt", "Portekizce – Brezilya (pt)", "Portekizce", "Portekizce – Brezilya"),
        Self::new("es", "İspanyolca (es)", "İspanyolca", "İspanyolca"),
        Self::new("ru", "Rusça (ru)", "Rusça", "Rusça"),
        Self::new("en", "İngilizce (en)", "İngilizce", "İngilizce"),
        Self::new("de", "Almanca (de)", "Almanca", "Almanca"),
        Self::new("fr", "Fransızca (fr)", "Fransızca", "Fransızca"),
        Self::new("ja", "Japonca (ja)", "Japonca", "Japonca"),
        Self::new("zh", "Çince (zh)", "Çince", "Çince"),
    ];

    const fn new(code: &'static str, title_key: &'static str, short_key: &'static str, target_key: &'static str) -> Self {
        Self { code, title_key, short_key, target_key }
    }

    /// Exact match (`""` → Automatic, `ko` → None).
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.code == code)
    }
    /// All minus Automatic.
    pub fn translation_targets() -> &'static [SubtitleLanguage] {
        &Self::ALL[1..]
    }
    pub fn is_automatic(&self) -> bool {
        self.code.is_empty()
    }
    pub fn title(&self) -> String {
        crate::l10n::l(self.title_key)
    }
    pub fn short_name(&self) -> String {
        crate::l10n::l(self.short_key)
    }
    pub fn target_name(&self) -> String {
        crate::l10n::l(self.target_key)
    }
}
