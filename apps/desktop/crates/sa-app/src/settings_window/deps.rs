//! What the Settings window needs from the rest of the app, as narrow traits so the settings model is testable with
//! in-memory stores (and `sa-shot` can render it without an engine, profiles or a media index).

use std::rc::Rc;
use std::sync::Arc;

use sa_core::cancel::CancellationToken;
use sa_core::dispatch::UiDispatcher;
use sa_core::engine_types::EngineModel;
use sa_core::l10n::AppLanguage;
use sa_core::models::ModelCatalog;
use sa_core::appearance::AppearanceSettings;
use sa_core::platform::FileAssociations;
use sa_core::settings::SettingsStore;
use sa_enginekit::external_llm::{ConnectionResult, ExternalLlmServer, HttpFetch};
use sa_providers::{ProviderStore, TranslationProviderProfile};
use sa_viewmodels::SubtitleViewModel;
use sa_viewmodels::types::SubtitleChange;
use uuid::Uuid;

/// The subtitle view-model settings the Settings window reads and writes (UI thread).
pub trait SettingsVm {
    fn language(&self) -> String;
    fn set_language(&self, code: &str);
    fn model(&self) -> String;
    fn set_model(&self, id: &str);
    /// Catalog transcription models for the current language.
    fn available_models(&self) -> Vec<EngineModel>;
    fn translation_model(&self) -> String;
    fn set_translation_model(&self, id: &str);
    fn diarize(&self) -> bool;
    fn set_diarize(&self, v: bool);
    fn speaker_count(&self) -> i64;
    fn set_speaker_count(&self, v: i64);
    fn offline_mode(&self) -> bool;
    fn set_offline_mode(&self, v: bool);
    fn auto_play_video(&self) -> bool;
    fn set_auto_play_video(&self, v: bool);
    /// The active provider's name when it sends text over the internet (macOS footer and callout).
    fn active_cloud_provider_name(&self) -> Option<String>;
    /// Registers a UI-thread change handler (handlers cannot be removed: the host guards them with a weak ref).
    fn subscribe_changes(&self, handler: Box<dyn Fn(SubtitleChange)>);
}

impl SettingsVm for SubtitleViewModel {
    fn language(&self) -> String {
        SubtitleViewModel::language(self)
    }
    fn set_language(&self, code: &str) {
        SubtitleViewModel::set_language(self, code)
    }
    fn model(&self) -> String {
        SubtitleViewModel::model(self)
    }
    fn set_model(&self, id: &str) {
        SubtitleViewModel::set_model(self, id)
    }
    fn available_models(&self) -> Vec<EngineModel> {
        SubtitleViewModel::available_models(self)
    }
    fn translation_model(&self) -> String {
        SubtitleViewModel::translation_model(self)
    }
    fn set_translation_model(&self, id: &str) {
        SubtitleViewModel::set_translation_model(self, id)
    }
    fn diarize(&self) -> bool {
        SubtitleViewModel::diarize(self)
    }
    fn set_diarize(&self, v: bool) {
        SubtitleViewModel::set_diarize(self, v)
    }
    fn speaker_count(&self) -> i64 {
        SubtitleViewModel::speaker_count(self)
    }
    fn set_speaker_count(&self, v: i64) {
        SubtitleViewModel::set_speaker_count(self, v)
    }
    fn offline_mode(&self) -> bool {
        SubtitleViewModel::offline_mode(self)
    }
    fn set_offline_mode(&self, v: bool) {
        SubtitleViewModel::set_offline_mode(self, v)
    }
    fn auto_play_video(&self) -> bool {
        SubtitleViewModel::auto_play_video(self)
    }
    fn set_auto_play_video(&self, v: bool) {
        SubtitleViewModel::set_auto_play_video(self, v)
    }
    fn active_cloud_provider_name(&self) -> Option<String> {
        SubtitleViewModel::active_cloud_provider_name(self)
    }
    fn subscribe_changes(&self, handler: Box<dyn Fn(SubtitleChange)>) {
        self.on_change(handler)
    }
}

/// Outcome of a connection test, already localized for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOutcome {
    pub success: bool,
    pub message: String,
    pub models: Vec<String>,
}

/// Connection tests (blocking; the model calls them on a worker thread). `None` = cancelled.
pub trait ProviderTester: Send + Sync {
    /// "Test et" on a saved provider: on success with models the provider's discovered list is stored.
    fn test_saved(&self, id: Uuid, cancel: &CancellationToken) -> Option<TestOutcome>;
    /// "Bağlantıyı test et" on the unsaved form: stores nothing.
    fn test_draft(&self, preset_id: &str, base_url: &str, api_key: &str, model: &str, cancel: &CancellationToken) -> Option<TestOutcome>;
}

/// macOS semantics (mac-viewmodels §6.4) over `ExternalLlmServer::test_connection`: the saved provider's key is read
/// from the store, a reachable answer with models is stored with `set_discovered_models`.
pub struct ServerTester {
    pub providers: Arc<dyn ProviderStore>,
    pub fetch: Arc<dyn HttpFetch>,
}

impl ServerTester {
    /// The localized notice of a connection result (`None` for a cancelled test).
    pub fn outcome(result: &ConnectionResult) -> Option<TestOutcome> {
        let (success, message) = match result {
            ConnectionResult::Cancelled => return None,
            ConnectionResult::InvalidAddress => (false, sa_core::l("Lütfen geçerli bir sunucu adresi girin.")),
            ConnectionResult::Unauthorized { status } => (false, ExternalLlmServer::unauthorized_message(*status)),
            ConnectionResult::Unreachable { reason } => (false, sa_core::l!("Sunucuya ulaşılamadı: %@", reason.as_str())),
            ConnectionResult::InsecureKeyTransport => (false, ExternalLlmServer::insecure_key_message()),
            ConnectionResult::Reachable { host, latency_ms, models } => (
                true,
                sa_core::l!(
                    "Bağlantı başarılı (%@ · %lld ms · %lld uygun model bulundu)",
                    host.as_str(),
                    *latency_ms as i64,
                    models.len() as i64
                ),
            ),
        };
        Some(TestOutcome { success, message, models: result.models().to_vec() })
    }
}

impl ProviderTester for ServerTester {
    fn test_saved(&self, id: Uuid, cancel: &CancellationToken) -> Option<TestOutcome> {
        let profile = self.providers.providers().into_iter().find(|p| p.id == id)?;
        let key = match self.providers.read_key(id) {
            sa_providers::ProviderKeyRead::Found(k) => k.expose().trim().to_owned(),
            sa_providers::ProviderKeyRead::Absent => String::new(),
            sa_providers::ProviderKeyRead::Unavailable => {
                return Some(TestOutcome {
                    success: false,
                    message: sa_core::l!(
                        "%@ için API anahtarı okunamadı. Anahtarı yeniden girip tekrar dene.",
                        profile.name.as_str()
                    ),
                    models: Vec::new(),
                });
            }
        };
        let result = ExternalLlmServer::test_connection(
            &profile.base_url,
            &key,
            &profile.model,
            Some(&profile.preset_id),
            self.fetch.as_ref(),
            cancel,
        );
        let outcome = Self::outcome(&result)?;
        // The provider may have been edited or removed meanwhile: store models only for the tested address.
        let still = self.providers.providers().into_iter().find(|p| p.id == id);
        if outcome.success
            && !outcome.models.is_empty()
            && still.as_ref().is_some_and(|p: &TranslationProviderProfile| p.base_url == profile.base_url)
        {
            self.providers.set_discovered_models(id, &outcome.models);
        }
        Some(outcome)
    }

    fn test_draft(&self, preset_id: &str, base_url: &str, api_key: &str, model: &str, cancel: &CancellationToken) -> Option<TestOutcome> {
        let result = ExternalLlmServer::test_connection(base_url, api_key.trim(), model, Some(preset_id), self.fetch.as_ref(), cancel);
        Self::outcome(&result)
    }
}

/// Windows-only services of the General page: the legacy LAN bridge, the encrypted video cache and file
/// associations (W-ONLY, UNIFIED.md §3.13 / §3.15).
pub trait WinSettingsOps: Send + Sync {
    /// `SyncFromLegacyLanSettings` after every LAN-section edit.
    fn sync_lan(&self, lan: &LanSettings);
    /// Mirrors the active provider into the LAN fields (`ApplyToViewModel`); returns the updated fields.
    fn apply_providers(&self, lan: &mut LanSettings);
    /// The legacy LAN test (`TestExternalLlmConnectionAsync`, no key sent). Blocking; `None` = cancelled.
    fn test_lan(&self, url: &str, cancel: &CancellationToken) -> Option<TestOutcome>;
    /// The LAN profile's discovered models (`UpdateLanDiscoveredModels`).
    fn store_lan_models(&self, models: &[String], selected: Option<&str>);
    /// `Ok(Some(mask))`, `Ok(None)` = no key, `Err` = unreadable.
    fn masked_key(&self, id: Uuid) -> Result<Option<String>, ()>;
    /// The decrypted key for the edit form (memory only; never logged).
    fn plain_key(&self, id: Uuid) -> String;
    /// A connection test is running somewhere (picker refresh is refused meanwhile).
    fn is_testing(&self) -> bool;
    /// Video cache: blocking sum of the profile cache's chunk files.
    fn cache_used_bytes(&self) -> i64;
    fn clear_cache(&self);
    fn set_cache_max_gb(&self, gb: i64);
    fn associations(&self) -> Option<Arc<dyn FileAssociations>>;
}

/// The Windows legacy LAN settings (`externalLlmEnabled/Url/Model/Async` + discovered list in memory).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanSettings {
    pub enabled: bool,
    pub url: String,
    pub model: String,
    pub async_translation: bool,
    pub models: Vec<String>,
}

/// Everything the Settings window is built from.
#[derive(Clone)]
pub struct SettingsDeps {
    pub settings: Arc<dyn SettingsStore>,
    pub ui: Arc<dyn UiDispatcher>,
    pub vm: Rc<dyn SettingsVm>,
    pub catalog: Rc<ModelCatalog>,
    pub appearance: Arc<AppearanceSettings>,
    pub language: Arc<AppLanguage>,
    pub providers: Arc<dyn ProviderStore>,
    pub tester: Arc<dyn ProviderTester>,
    /// `Some` in the Windows build.
    pub win: Option<Arc<dyn WinSettingsOps>>,
    /// Installed font families (DirectWrite / CTFontManager order); called off the UI thread once per window.
    pub font_families: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

#[cfg(windows)]
mod win_impl {
    use super::*;
    use sa_providers::win::{LegacyLanSettings, ProviderViewState, WinProviderManager};

    /// The production Windows services.
    pub struct WinOps {
        pub providers: Arc<WinProviderManager>,
        pub fetch: Arc<dyn HttpFetch>,
        pub cache: Option<Arc<sa_media::win::VideoCacheStore>>,
        pub associations: Option<Arc<dyn FileAssociations>>,
    }

    fn legacy(lan: &LanSettings) -> LegacyLanSettings {
        LegacyLanSettings {
            enabled: lan.enabled,
            url: lan.url.clone(),
            model: lan.model.clone(),
            async_translation: lan.async_translation,
            models: lan.models.clone(),
        }
    }

    impl WinSettingsOps for WinOps {
        fn sync_lan(&self, lan: &LanSettings) {
            self.providers.sync_from_legacy_lan_settings(&legacy(lan));
        }
        fn apply_providers(&self, lan: &mut LanSettings) {
            let mut view = ProviderViewState { lan: legacy(lan), ..Default::default() };
            self.providers.apply_to_view_state(&mut view);
            lan.enabled = view.lan.enabled;
            lan.url = view.lan.url;
            lan.model = view.lan.model;
            lan.async_translation = view.lan.async_translation;
        }
        fn test_lan(&self, url: &str, cancel: &CancellationToken) -> Option<TestOutcome> {
            sa_providers::win::test_lan_server(url, self.fetch.as_ref(), cancel)
                .ok()
                .map(|o| TestOutcome { success: o.success, message: o.message, models: o.models })
        }
        fn store_lan_models(&self, models: &[String], selected: Option<&str>) {
            self.providers.update_lan_discovered_models(models, selected);
        }
        fn masked_key(&self, id: Uuid) -> Result<Option<String>, ()> {
            self.providers.masked_key(id)
        }
        fn plain_key(&self, id: Uuid) -> String {
            match self.providers.read_key(id) {
                sa_providers::ProviderKeyRead::Found(k) => k.expose().to_owned(),
                _ => String::new(),
            }
        }
        fn is_testing(&self) -> bool {
            self.providers.is_testing()
        }
        fn cache_used_bytes(&self) -> i64 {
            self.cache.as_ref().map(|c| c.total_used_bytes()).unwrap_or(0)
        }
        fn clear_cache(&self) {
            if let Some(c) = &self.cache {
                c.clear_all();
            }
        }
        fn set_cache_max_gb(&self, gb: i64) {
            if let Some(c) = &self.cache {
                c.set_max_cache_size_gb(gb);
            }
        }
        fn associations(&self) -> Option<Arc<dyn FileAssociations>> {
            self.associations.clone()
        }
    }

    /// Windows provider tests with the C# messages (`TestAndFetchModelsAsync`).
    pub struct WinTester {
        pub providers: Arc<WinProviderManager>,
        pub fetch: Arc<dyn HttpFetch>,
    }

    impl ProviderTester for WinTester {
        fn test_saved(&self, id: Uuid, cancel: &CancellationToken) -> Option<TestOutcome> {
            self.providers
                .test_and_fetch_models(id, self.fetch.as_ref(), cancel)
                .ok()
                .map(|o| TestOutcome { success: o.success, message: o.message, models: o.models })
        }
        fn test_draft(&self, preset_id: &str, base_url: &str, api_key: &str, model: &str, cancel: &CancellationToken) -> Option<TestOutcome> {
            let model = (!model.trim().is_empty()).then_some(model);
            self.providers
                .test_draft(preset_id, base_url, api_key, model, self.fetch.as_ref(), cancel)
                .ok()
                .map(|o| TestOutcome { success: o.success, message: o.message, models: o.models })
        }
    }
}

#[cfg(windows)]
pub use win_impl::{WinOps, WinTester};
