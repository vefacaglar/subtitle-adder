//! Model catalog tests: ports of ModelCatalogTests, PresentationTests.SubtitleLanguages, the macOS
//! ModelCatalog/ModelLabel behaviors (delete notice, llama-first order, ratings) and UiThreadTests (catalog part).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::Duration;

use sa_core::CancellationToken;
use sa_core::dispatch::QueueDispatcher;
use sa_core::engine_types::{EngineError, EngineModel};
use sa_core::l10n::{LanguageState, UiLanguage};
use sa_core::models::{
    DEFAULT_MODEL, DEFAULT_TRANSLATION_MODEL, ModelCatalog, ModelCatalogBackend, ModelCatalogChange, ModelCatalogOptions, ModelLabel,
    SubtitleLanguage, delete_notice_for, fallback_models, most_accurate,
};

static SHARED_LANGUAGE: Mutex<()> = Mutex::new(());

struct LanguageScope {
    saved: UiLanguage,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl LanguageScope {
    fn new(language: UiLanguage) -> Self {
        let guard = SHARED_LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
        let saved = LanguageState::shared().language();
        LanguageState::shared().set_language(language);
        Self { saved, _guard: guard }
    }
}

impl Drop for LanguageScope {
    fn drop(&mut self) {
        LanguageState::shared().set_language(self.saved);
    }
}

#[derive(Default)]
struct FakeEngine {
    catalog: Mutex<Vec<EngineModel>>,
    models_error: Mutex<Option<EngineError>>,
    delete: Mutex<Option<Result<i64, EngineError>>>,
    delete_calls: Mutex<Vec<String>>,
}

impl ModelCatalogBackend for FakeEngine {
    fn models(&self, _cancel: &CancellationToken) -> Result<Vec<EngineModel>, EngineError> {
        if let Some(e) = self.models_error.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(self.catalog.lock().unwrap().clone())
    }
    fn delete_model(&self, id: &str, _cancel: &CancellationToken) -> Result<i64, EngineError> {
        self.delete_calls.lock().unwrap().push(id.to_owned());
        self.delete.lock().unwrap().clone().unwrap_or(Ok(0))
    }
}

fn model(id: &str, downloaded: bool, disk_mb: Option<i64>) -> EngineModel {
    let mut m = EngineModel::new(id, 100, 3, 3, downloaded);
    m.disk_mb = disk_mb;
    m
}

const WAIT: Duration = Duration::from_secs(5);

fn load(catalog: &ModelCatalog, ui: &QueueDispatcher) {
    catalog.load();
    assert!(ui.run_until(WAIT, || !catalog.is_loading()));
}

fn delete(catalog: &ModelCatalog, ui: &QueueDispatcher, id: &str) -> Option<i64> {
    let result = Arc::new(Mutex::new(None::<Option<i64>>));
    let r = result.clone();
    catalog.delete(id, Box::new(move |freed| *r.lock().unwrap() = Some(freed)));
    assert!(ui.run_until(WAIT, || result.lock().unwrap().is_some()));
    result.lock().unwrap().unwrap()
}

fn ids(models: &[EngineModel]) -> Vec<&str> {
    models.iter().map(|m| m.id.as_str()).collect()
}

#[test]
fn lists_by_kind_and_language() {
    let catalog = ModelCatalog::with_options(None, QueueDispatcher::new(), ModelCatalogOptions::WINDOWS);
    assert_eq!(ids(&catalog.translation_models()), ["nllb-1.3b", "qwen3.5-4b", "madlad-3b", "gemma-4-e4b", "qwen3.5-9b", "gemma-4-12b"]);
    assert_eq!(ids(&catalog.diarization_models()), ["speaker-diarization"]);
    assert!(catalog.transcription_models().iter().all(|m| !m.is_translation() && !m.is_diarization()));
    assert_eq!(ids(&catalog.models_for("")), ["base", "small", "medium", "large-v3-turbo", "large-v3"]);
    assert!(catalog.models_for("ru").iter().any(|m| m.id == "gigaam-v3"));
    assert!(!catalog.models_for("tr").iter().any(|m| m.id == "parakeet-v3"));
    assert!(catalog.models_for("pt").iter().any(|m| m.id == "canary-1b-v2"));
    assert_eq!(catalog.total_disk_mb(), 0);
    assert!(catalog.model(DEFAULT_MODEL).is_some());
    assert!(catalog.model(DEFAULT_TRANSLATION_MODEL).is_some());
}

#[test]
fn mac_lists_llama_translation_models_first() {
    let catalog = ModelCatalog::with_options(None, QueueDispatcher::new(), ModelCatalogOptions::MAC);
    assert_eq!(ids(&catalog.translation_models()), ["qwen3.5-4b", "gemma-4-e4b", "qwen3.5-9b", "gemma-4-12b", "nllb-1.3b", "madlad-3b"]);
    assert_eq!(ids(&catalog.classic_translation_models()), ["nllb-1.3b", "madlad-3b"]);
    assert_eq!(ids(&catalog.translation_rows().iter().map(|r| r.model.clone()).collect::<Vec<_>>())[0], "qwen3.5-4b");
}

#[test]
fn fallback_list_matches_the_engine_catalog() {
    let models = fallback_models();
    assert_eq!(
        ids(&models),
        [
            "base", "gigaam-v3", "small", "parakeet-v3", "canary-1b-v2", "medium", "large-v3-turbo", "large-v3", "speaker-diarization",
            "nllb-1.3b", "qwen3.5-4b", "madlad-3b", "gemma-4-e4b", "qwen3.5-9b", "gemma-4-12b",
        ]
    );
    assert!(models.iter().all(|m| !m.downloaded && m.disk_mb.is_none()));
    let parakeet = &models[3];
    assert_eq!(parakeet.languages.as_ref().unwrap().len(), 25);
    assert_eq!(parakeet.backend, "onnx");
    assert_eq!(models[8].license.as_deref(), Some("MIT AND Apache-2.0"));
    assert_eq!(models[8].kind, "diarization");
    assert_eq!(models[13].size_mb, 5417);
}

#[test]
fn load_without_engine_reports_not_found() {
    let _l = LanguageScope::new(UiLanguage::En);
    let catalog = ModelCatalog::new(None, QueueDispatcher::new());
    catalog.load();
    let error = catalog.load_error().expect("error");
    assert_eq!(error.id(), "error:notFound");
    assert_eq!(error.text(), "Subtitle engine not found (is engine/dist built, or is SUBTITLE_ENGINE set?).");
    assert_eq!(catalog.models(), fallback_models());
    assert!(!catalog.has_loaded());
}

#[test]
fn load_error_keeps_the_list() {
    let ui = QueueDispatcher::new();
    let engine = Arc::new(FakeEngine::default());
    *engine.models_error.lock().unwrap() = Some(EngineError::Failed("boom".into()));
    let catalog = ModelCatalog::new(Some(engine.clone()), ui.clone());
    load(&catalog, &ui);
    assert_eq!(catalog.load_error().map(|e| e.text()).as_deref(), Some("boom"));
    assert_eq!(catalog.models(), fallback_models());
    *engine.models_error.lock().unwrap() = None;
    *engine.catalog.lock().unwrap() = vec![model("base", true, Some(140))];
    load(&catalog, &ui);
    assert!(catalog.load_error().is_none());
    assert_eq!(catalog.total_disk_mb(), 140);
    assert!(catalog.has_loaded());
}

#[test]
fn an_empty_or_cancelled_answer_keeps_the_list_silently() {
    let ui = QueueDispatcher::new();
    let engine = Arc::new(FakeEngine::default());
    let catalog = ModelCatalog::new(Some(engine.clone()), ui.clone());
    load(&catalog, &ui);
    assert_eq!(catalog.models(), fallback_models());
    assert!(!catalog.has_loaded());
    *engine.models_error.lock().unwrap() = Some(EngineError::Cancelled);
    load(&catalog, &ui);
    assert!(catalog.load_error().is_none());
}

#[test]
fn delete_marks_row_not_downloaded_and_returns_freed_mb() {
    let ui = QueueDispatcher::new();
    let engine = Arc::new(FakeEngine::default());
    *engine.catalog.lock().unwrap() = vec![model("small", true, Some(464)), model("base", true, None)];
    *engine.delete.lock().unwrap() = Some(Ok(464));
    let catalog = ModelCatalog::new(Some(engine.clone()), ui.clone());
    load(&catalog, &ui);
    assert!(catalog.can_delete("small"));
    assert_eq!(delete(&catalog, &ui, "small"), Some(464));
    assert!(!catalog.models()[0].downloaded);
    assert_eq!(catalog.models()[0].disk_mb, None);
    assert!(catalog.deleting().is_empty());
    assert_eq!(*engine.delete_calls.lock().unwrap(), ["small"]);
    assert!(!catalog.transcription_rows()[0].shows_delete_button);
    // Not downloaded any more: no second call.
    assert_eq!(delete(&catalog, &ui, "small"), None);
    assert_eq!(engine.delete_calls.lock().unwrap().len(), 1);
}

#[test]
fn active_models_cannot_be_deleted() {
    let ui = QueueDispatcher::new();
    let engine = Arc::new(FakeEngine::default());
    *engine.catalog.lock().unwrap() = vec![model("base", true, None)];
    let catalog = ModelCatalog::new(Some(engine.clone()), ui.clone());
    load(&catalog, &ui);
    catalog.set_active_models(HashSet::from(["base".to_owned()]));
    assert!(!catalog.can_delete("base"));
    assert!(!catalog.transcription_rows()[0].can_delete);
    assert!(catalog.transcription_rows()[0].shows_delete_button);
    assert_eq!(delete(&catalog, &ui, "base"), None);
    assert!(engine.delete_calls.lock().unwrap().is_empty());
}

#[test]
fn delete_error_is_kept_per_model() {
    let ui = QueueDispatcher::new();
    let engine = Arc::new(FakeEngine::default());
    *engine.catalog.lock().unwrap() = vec![model("base", true, None)];
    *engine.delete.lock().unwrap() = Some(Err(EngineError::Failed("kısmen silindi".into())));
    let catalog = ModelCatalog::new(Some(engine), ui.clone());
    load(&catalog, &ui);
    assert_eq!(delete(&catalog, &ui, "base"), None);
    assert_eq!(catalog.delete_errors()["base"].text(), "kısmen silindi");
    assert_eq!(catalog.transcription_rows()[0].error_text.as_deref(), Some("kısmen silindi"));
    assert!(catalog.models()[0].downloaded);
    assert!(catalog.delete_notice().is_none());
}

#[test]
fn delete_without_engine_is_refused_at_once() {
    let ui = QueueDispatcher::new();
    let catalog = ModelCatalog::new(None, ui.clone());
    assert_eq!(delete(&catalog, &ui, "base"), None, "fallback models are not downloaded");
    assert!(catalog.delete_errors().is_empty());
}

#[test]
fn delete_notice_texts_and_auto_dismissal() {
    let _l = LanguageScope::new(UiLanguage::Tr);
    assert_eq!(delete_notice_for("small", 1500).text(), "1,5 GB boşaltıldı.");
    assert_eq!(delete_notice_for("small", 1500).id(), "deleted-freed:small:1500");
    assert_eq!(delete_notice_for("base", 0).id(), "deleted-shared:base");
    assert!(delete_notice_for("base", 0).text().starts_with("Hızlı silindi;"));

    let ui = QueueDispatcher::new();
    let engine = Arc::new(FakeEngine::default());
    *engine.catalog.lock().unwrap() = vec![model("small", true, Some(464))];
    *engine.delete.lock().unwrap() = Some(Ok(464));
    let options = ModelCatalogOptions { llama_translation_first: true, notice_duration: Some(Duration::from_millis(50)) };
    let catalog = ModelCatalog::with_options(Some(engine), ui.clone(), options);
    load(&catalog, &ui);
    assert_eq!(delete(&catalog, &ui, "small"), Some(464));
    assert_eq!(catalog.delete_notice().map(|n| n.text()).as_deref(), Some("464 MB boşaltıldı."));
    assert!(ui.run_until(WAIT, || catalog.delete_notice().is_none()), "the notice hides itself");
}

#[test]
fn dismissed_notice_stays_dismissed() {
    let ui = QueueDispatcher::new();
    let engine = Arc::new(FakeEngine::default());
    *engine.catalog.lock().unwrap() = vec![model("small", true, None)];
    *engine.delete.lock().unwrap() = Some(Ok(10));
    let catalog = ModelCatalog::with_options(Some(engine), ui.clone(), ModelCatalogOptions::WINDOWS);
    load(&catalog, &ui);
    delete(&catalog, &ui, "small");
    assert!(catalog.delete_notice().is_some());
    std::thread::sleep(Duration::from_millis(30));
    ui.run_pending();
    assert!(catalog.delete_notice().is_some(), "no timer without a duration");
    catalog.dismiss_delete_notice();
    assert!(catalog.delete_notice().is_none());
}

#[test]
fn notifications_arrive_on_the_ui_thread() {
    let ui = QueueDispatcher::new();
    let ui_thread = std::thread::current().id();
    let engine = Arc::new(FakeEngine::default());
    *engine.catalog.lock().unwrap() = vec![model("small", true, None)];
    *engine.delete.lock().unwrap() = Some(Ok(1));
    let catalog = ModelCatalog::new(Some(engine.clone()), ui.clone());
    let seen: Arc<Mutex<Vec<(ModelCatalogChange, ThreadId)>>> = Arc::default();
    let s = seen.clone();
    let _sub = catalog.subscribe(move |c| s.lock().unwrap().push((*c, std::thread::current().id())));
    load(&catalog, &ui);
    delete(&catalog, &ui, "small");
    *engine.models_error.lock().unwrap() = Some(EngineError::Cancelled);
    load(&catalog, &ui);
    let seen = seen.lock().unwrap();
    assert!(seen.iter().any(|(c, _)| *c == ModelCatalogChange::Models));
    assert!(seen.iter().any(|(c, _)| *c == ModelCatalogChange::Deleting));
    assert!(seen.iter().all(|(_, t)| *t == ui_thread));
}

#[test]
fn rows_carry_localized_texts() {
    let _l = LanguageScope::new(UiLanguage::En);
    let catalog = ModelCatalog::new(None, QueueDispatcher::new());
    let rows = catalog.rows();
    assert_eq!(rows.len(), 15);
    let nllb = rows.iter().find(|r| r.id == "nllb-1.3b").unwrap();
    assert!(nllb.is_non_commercial);
    assert_eq!(nllb.license_text, "License: CC-BY-NC-4.0");
    assert_eq!(nllb.delete_confirm_title, "Delete the NLLB-200 1.3B model?");
    assert_eq!(nllb.delete_confirm_message, "1.3 GB will be freed. It will be downloaded again if you use it later.");
    assert_eq!(nllb.rating_text.as_deref(), Some("Speed 3/5 · Accuracy 4/5"));
    assert!(!nllb.shows_delete_button);
    assert_eq!(catalog.total_disk_text(), "No models downloaded yet.");
    catalog.set_active_models(HashSet::from(["nllb-1.3b".to_owned()]));
    let nllb = catalog.translation_rows().into_iter().find(|r| r.id == "nllb-1.3b").unwrap();
    assert_eq!(nllb.delete_help, "A running job is using this model; you can delete it when the job finishes.");
}

#[test]
fn model_labels() {
    let tr = LanguageScope::new(UiLanguage::Tr);
    let models = fallback_models();
    let madlad = models.iter().find(|m| m.id == "madlad-3b").unwrap();
    assert_eq!(ModelLabel::name("madlad-3b"), "MADLAD-400 3B");
    assert_eq!(ModelLabel::name("unknown-id"), "unknown-id");
    assert_eq!(ModelLabel::size_text(2839, UiLanguage::Tr), "2,8 GB");
    assert_eq!(ModelLabel::size_text(3000, UiLanguage::Tr), "3 GB");
    assert_eq!(ModelLabel::size_text(465, UiLanguage::Tr), "465 MB");
    assert_eq!(ModelLabel::size_text(1250, UiLanguage::Tr), "1,3 GB", "half away from zero, not half-even");
    assert_eq!(ModelLabel::size_text(1000, UiLanguage::Tr), "1 GB");
    assert_eq!(ModelLabel::size_text(999, UiLanguage::Tr), "999 MB");
    assert_eq!(ModelLabel::size_text(9960, UiLanguage::En), "10 GB");
    assert_eq!(ModelLabel::language_summary(madlad), "tr, en, pt…");
    assert_eq!(ModelLabel::language_summary(&models[0]), "Tüm diller");
    assert_eq!(ModelLabel::language_summary(&models[1]), "ru");
    assert_eq!(ModelLabel::language_summary(models.iter().find(|m| m.is_diarization()).unwrap()), "Dilden bağımsız");
    assert_eq!(ModelLabel::license_text(madlad), "Lisans: Apache-2.0");
    assert_eq!(ModelLabel::license_text(&EngineModel::new("x", 1, 1, 1, false)), "Lisans: bilinmiyor");
    assert_eq!(ModelLabel::menu_title(&models[2]), "Dengeli · ilk kullanımda ~465 MB indirilir");
    assert_eq!(ModelLabel::menu_title(&model("base", true, None)), "Hızlı · hazır");
    assert_eq!(ModelLabel::storage_status(&model("x", true, Some(120))), "Yüklü — diskte 120 MB");
    assert_eq!(ModelLabel::storage_status(&models[7]), "Yüklü değil — ~3 GB indirilir");
    let nllb = models.iter().find(|m| m.id == "nllb-1.3b").unwrap();
    assert_eq!(ModelLabel::translation_menu_title(nllb), "NLLB-200 1.3B · ilk kullanımda ~1,3 GB indirilir · Ticari olmayan lisans");
    assert_eq!(ModelLabel::translation_menu_title(madlad), "MADLAD-400 3B · ilk kullanımda ~2,8 GB indirilir");
    assert_eq!(ModelLabel::rating_text(&EngineModel::new("x", 1, 0, 3, false)), None);
    assert_eq!(ModelLabel::rating_text(&EngineModel::new("x", 1, 9, 3, false)).as_deref(), Some("Hız 5/5 · Doğruluk 3/5"));
    drop(tr);
    let _en = LanguageScope::new(UiLanguage::En);
    assert_eq!(ModelLabel::size_text(2839, UiLanguage::En), "2.8 GB");
    assert_eq!(ModelLabel::size(2839), "2.8 GB");
    assert!(nllb.has_non_commercial_license());
    assert_eq!(ModelLabel::name("speaker-diarization"), "Speaker separation");
}

#[test]
fn most_accurate_prefers_accuracy_then_speed_and_first_on_ties() {
    let models = fallback_models();
    let transcription: Vec<EngineModel> = models.iter().filter(|m| m.is_transcription()).cloned().collect();
    assert_eq!(most_accurate(&transcription).map(|m| m.id.as_str()), Some("large-v3"));
    let tied = vec![EngineModel::new("a", 1, 2, 4, false), EngineModel::new("b", 1, 2, 4, false), EngineModel::new("c", 1, 1, 4, false)];
    assert_eq!(most_accurate(&tied).map(|m| m.id.as_str()), Some("a"));
    assert!(most_accurate(&[]).is_none());
    let translation: Vec<EngineModel> = models.into_iter().filter(|m| m.is_translation()).collect();
    assert_eq!(most_accurate(&translation).map(|m| m.id.as_str()), Some("madlad-3b"));
}

#[test]
fn subtitle_languages() {
    let en = LanguageScope::new(UiLanguage::En);
    let codes: Vec<&str> = SubtitleLanguage::ALL.iter().map(|l| l.code).collect();
    assert_eq!(codes, ["", "tr", "pt", "es", "ru", "en", "de", "fr", "ja", "zh"]);
    let targets: Vec<&str> = SubtitleLanguage::translation_targets().iter().map(|l| l.code).collect();
    assert_eq!(targets, ["tr", "pt", "es", "ru", "en", "de", "fr", "ja", "zh"]);
    for code in ["de", "fr", "ja", "zh"] {
        assert_eq!(SubtitleLanguage::from_code(code).map(|l| l.code), Some(code));
    }
    assert_eq!(SubtitleLanguage::from_code("ko"), None);
    assert!(SubtitleLanguage::from_code("").unwrap().is_automatic());
    assert_eq!(SubtitleLanguage::from_code("tr").unwrap().short_name(), "Turkish");
    assert_eq!(SubtitleLanguage::from_code("").unwrap().short_name(), "Automatic detection");
    drop(en);
    let _tr = LanguageScope::new(UiLanguage::Tr);
    assert_eq!(SubtitleLanguage::from_code("tr").unwrap().title(), "Türkçe (tr)");
    assert_eq!(SubtitleLanguage::from_code("pt").unwrap().target_name(), "Portekizce – Brezilya");
}
