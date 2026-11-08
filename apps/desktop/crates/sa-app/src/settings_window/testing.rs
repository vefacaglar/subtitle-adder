//! In-memory stand-ins for the Settings window's dependencies: used by the tests and by `sa-shot` (renders the real
//! Settings model without an engine, a profile store or the user's data).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_core::dispatch::UiDispatcher;
use sa_core::engine_types::EngineModel;
use sa_core::l10n::{AppLanguage, LanguageState};
use sa_core::models::ModelCatalog;
use sa_core::appearance::AppearanceSettings;
use sa_core::platform::FileAssociations;
use sa_core::settings::{InMemorySettingsStore, SettingsStore};
use sa_core::subscription::{Subscribers, Subscription};
use sa_providers::{ActiveExternalLlm, ProviderChange, ProviderKeyRead, ProviderLoadProblem, ProviderStore, TranslationProviderProfile};
use sa_viewmodels::types::SubtitleChange;
use uuid::Uuid;

use super::deps::{LanSettings, ProviderTester, SettingsDeps, SettingsVm, TestOutcome, WinSettingsOps};

/// A view model over plain fields (persisting through the settings store like the real one).
pub struct FakeVm {
    pub catalog: Rc<ModelCatalog>,
    pub settings: Arc<dyn SettingsStore>,
    pub language: RefCell<String>,
    pub model: RefCell<String>,
    pub translation_model: RefCell<String>,
    pub diarize: RefCell<bool>,
    pub speaker_count: RefCell<i64>,
    pub offline: RefCell<bool>,
    pub auto_play: RefCell<bool>,
    pub cloud_name: RefCell<Option<String>>,
    handlers: RefCell<Vec<Rc<dyn Fn(SubtitleChange)>>>,
}

impl FakeVm {
    pub fn new(catalog: Rc<ModelCatalog>, settings: Arc<dyn SettingsStore>) -> Rc<Self> {
        Rc::new(Self {
            catalog,
            settings,
            language: RefCell::new(String::new()),
            model: RefCell::new("large-v3".into()),
            translation_model: RefCell::new("madlad-3b".into()),
            diarize: RefCell::new(true),
            speaker_count: RefCell::new(0),
            offline: RefCell::new(true),
            auto_play: RefCell::new(true),
            cloud_name: RefCell::new(None),
            handlers: RefCell::new(Vec::new()),
        })
    }

    /// Raises a change like the real view model does.
    pub fn emit(&self, change: SubtitleChange) {
        let hs: Vec<_> = self.handlers.borrow().clone();
        for h in hs {
            h(change);
        }
    }
}

impl SettingsVm for FakeVm {
    fn language(&self) -> String {
        self.language.borrow().clone()
    }
    fn set_language(&self, code: &str) {
        *self.language.borrow_mut() = code.into();
        self.settings.set_string("selectedLanguage", code);
        self.emit(SubtitleChange::Settings);
    }
    fn model(&self) -> String {
        self.model.borrow().clone()
    }
    fn set_model(&self, id: &str) {
        *self.model.borrow_mut() = id.into();
        self.settings.set_string("selectedModel", id);
        self.emit(SubtitleChange::Settings);
    }
    fn available_models(&self) -> Vec<EngineModel> {
        self.catalog.models_for(&self.language())
    }
    fn translation_model(&self) -> String {
        self.translation_model.borrow().clone()
    }
    fn set_translation_model(&self, id: &str) {
        *self.translation_model.borrow_mut() = id.into();
        self.settings.set_string("translationModel", id);
        self.emit(SubtitleChange::Settings);
    }
    fn diarize(&self) -> bool {
        *self.diarize.borrow()
    }
    fn set_diarize(&self, v: bool) {
        *self.diarize.borrow_mut() = v;
        self.settings.set_bool("diarizeSpeakers", v);
        self.emit(SubtitleChange::Settings);
    }
    fn speaker_count(&self) -> i64 {
        *self.speaker_count.borrow()
    }
    fn set_speaker_count(&self, v: i64) {
        *self.speaker_count.borrow_mut() = v;
        self.settings.set_int("speakerCount", v);
        self.emit(SubtitleChange::Settings);
    }
    fn offline_mode(&self) -> bool {
        *self.offline.borrow()
    }
    fn set_offline_mode(&self, v: bool) {
        *self.offline.borrow_mut() = v;
        self.settings.set_bool("offlineMode", v);
        self.emit(SubtitleChange::Settings);
    }
    fn auto_play_video(&self) -> bool {
        *self.auto_play.borrow()
    }
    fn set_auto_play_video(&self, v: bool) {
        *self.auto_play.borrow_mut() = v;
        self.settings.set_bool("autoPlayVideo", v);
        self.emit(SubtitleChange::Settings);
    }
    fn active_cloud_provider_name(&self) -> Option<String> {
        self.cloud_name.borrow().clone()
    }
    fn subscribe_changes(&self, handler: Box<dyn Fn(SubtitleChange)>) {
        self.handlers.borrow_mut().push(Rc::from(handler));
    }
}

/// A provider store in memory (keys in a map; `refuse_keys` simulates a secret store that refuses writes).
#[derive(Default)]
pub struct MemoryProviderStore {
    pub state: Mutex<(Vec<TranslationProviderProfile>, Option<Uuid>)>,
    pub keys: Mutex<HashMap<Uuid, String>>,
    pub unreadable: Mutex<std::collections::HashSet<Uuid>>,
    pub refuse_keys: Mutex<bool>,
    subscribers: Subscribers<ProviderChange>,
}

impl MemoryProviderStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn notify(&self, c: ProviderChange) {
        self.subscribers.notify(&c);
    }
}

impl ProviderStore for MemoryProviderStore {
    fn providers(&self) -> Vec<TranslationProviderProfile> {
        self.state.lock().0.clone()
    }
    fn active_provider_id(&self) -> Option<Uuid> {
        self.state.lock().1
    }
    fn load_problem(&self) -> Option<ProviderLoadProblem> {
        None
    }
    fn add(&self, profile: TranslationProviderProfile, key: Option<&str>, set_active: bool) -> TranslationProviderProfile {
        {
            let mut st = self.state.lock();
            st.0.retain(|p| p.id != profile.id);
            st.0.push(profile.clone());
            if set_active {
                st.1 = Some(profile.id);
            }
        }
        if let Some(k) = key {
            self.set_key(profile.id, k);
        }
        self.notify(ProviderChange::Providers);
        profile
    }
    fn update(&self, profile: TranslationProviderProfile, key: Option<&str>) -> bool {
        let found = {
            let mut st = self.state.lock();
            match st.0.iter_mut().find(|p| p.id == profile.id) {
                Some(p) => {
                    *p = profile.clone();
                    true
                }
                None => false,
            }
        };
        if found {
            if let Some(k) = key {
                self.set_key(profile.id, k);
            }
            self.notify(ProviderChange::Providers);
        }
        found
    }
    fn remove(&self, id: Uuid) {
        {
            let mut st = self.state.lock();
            st.0.retain(|p| p.id != id);
            if st.1 == Some(id) {
                st.1 = None;
            }
        }
        self.keys.lock().remove(&id);
        self.notify(ProviderChange::Providers);
    }
    fn set_active(&self, id: Option<Uuid>) {
        self.state.lock().1 = id;
        self.notify(ProviderChange::Active);
    }
    fn update_model(&self, id: Uuid, model: &str) {
        if let Some(p) = self.state.lock().0.iter_mut().find(|p| p.id == id) {
            p.model = model.to_owned();
        }
        self.notify(ProviderChange::Providers);
    }
    fn set_discovered_models(&self, id: Uuid, models: &[String]) {
        if let Some(p) = self.state.lock().0.iter_mut().find(|p| p.id == id) {
            p.discovered_models = models.to_vec();
        }
        self.notify(ProviderChange::Providers);
    }
    fn set_key(&self, id: Uuid, key: &str) -> bool {
        if *self.refuse_keys.lock() && !key.trim().is_empty() {
            return false;
        }
        if key.trim().is_empty() {
            self.keys.lock().remove(&id);
        } else {
            self.keys.lock().insert(id, key.trim().to_owned());
        }
        self.notify(ProviderChange::Keys);
        true
    }
    fn read_key(&self, id: Uuid) -> ProviderKeyRead {
        if self.unreadable.lock().contains(&id) {
            return ProviderKeyRead::Unavailable;
        }
        match self.keys.lock().get(&id) {
            Some(k) => ProviderKeyRead::Found(sa_core::secrets::SecretString::new(k.clone())),
            None => ProviderKeyRead::Absent,
        }
    }
    fn active_external_llm(&self) -> Option<ActiveExternalLlm> {
        None
    }
    fn subscribe(&self, handler: Box<dyn Fn(ProviderChange) + Send + Sync>) -> Subscription {
        self.subscribers.subscribe(move |c| handler(*c))
    }
}

/// Canned connection tests; every call is recorded.
#[derive(Default)]
pub struct FakeTester {
    pub answer: Mutex<Option<TestOutcome>>,
    pub calls: Mutex<Vec<String>>,
}

impl ProviderTester for FakeTester {
    fn test_saved(&self, id: Uuid, _cancel: &CancellationToken) -> Option<TestOutcome> {
        self.calls.lock().push(format!("saved:{id}"));
        self.answer.lock().clone()
    }
    fn test_draft(&self, preset_id: &str, base_url: &str, api_key: &str, model: &str, _cancel: &CancellationToken) -> Option<TestOutcome> {
        self.calls.lock().push(format!("draft:{preset_id}|{base_url}|{api_key}|{model}"));
        self.answer.lock().clone()
    }
}

/// Windows-only services in memory.
#[derive(Default)]
pub struct FakeWinOps {
    pub synced: Mutex<Vec<LanSettings>>,
    pub lan_answer: Mutex<Option<TestOutcome>>,
    pub cache_used: Mutex<i64>,
    pub cache_max: Mutex<i64>,
    pub cleared: Mutex<usize>,
    pub associations: Option<Arc<FakeAssociations>>,
    /// The provider store whose keys `masked_key` / `plain_key` report (the Windows manager holds them in memory).
    pub keys_from: Option<Arc<MemoryProviderStore>>,
}

impl WinSettingsOps for FakeWinOps {
    fn sync_lan(&self, lan: &LanSettings) {
        self.synced.lock().push(lan.clone());
    }
    fn apply_providers(&self, _lan: &mut LanSettings) {}
    fn test_lan(&self, _url: &str, _cancel: &CancellationToken) -> Option<TestOutcome> {
        self.lan_answer.lock().clone()
    }
    fn store_lan_models(&self, _models: &[String], _selected: Option<&str>) {}
    fn masked_key(&self, id: Uuid) -> Result<Option<String>, ()> {
        match self.keys_from.as_ref().map(|s| s.read_key(id)) {
            Some(ProviderKeyRead::Found(k)) => Ok(sa_providers::mask_key(k.expose())),
            Some(ProviderKeyRead::Unavailable) => Err(()),
            _ => Ok(None),
        }
    }
    fn plain_key(&self, id: Uuid) -> String {
        match self.keys_from.as_ref().map(|s| s.read_key(id)) {
            Some(ProviderKeyRead::Found(k)) => k.expose().to_owned(),
            _ => String::new(),
        }
    }
    fn is_testing(&self) -> bool {
        false
    }
    fn cache_used_bytes(&self) -> i64 {
        *self.cache_used.lock()
    }
    fn clear_cache(&self) {
        *self.cleared.lock() += 1;
        *self.cache_used.lock() = 0;
    }
    fn set_cache_max_gb(&self, gb: i64) {
        *self.cache_max.lock() = gb;
    }
    fn associations(&self) -> Option<Arc<dyn FileAssociations>> {
        self.associations.clone().map(|a| a as Arc<dyn FileAssociations>)
    }
}

#[derive(Default)]
pub struct FakeAssociations {
    pub registered: Mutex<bool>,
    pub opened: Mutex<usize>,
}

impl FileAssociations for FakeAssociations {
    fn register(&self) -> bool {
        *self.registered.lock() = true;
        true
    }
    fn unregister(&self) -> bool {
        *self.registered.lock() = false;
        true
    }
    fn is_registered(&self) -> bool {
        *self.registered.lock()
    }
    fn open_default_apps_settings(&self) {
        *self.opened.lock() += 1;
    }
}

/// The fakes behind a [`SettingsDeps`].
pub struct Fakes {
    pub deps: SettingsDeps,
    pub settings: Arc<InMemorySettingsStore>,
    pub vm: Rc<FakeVm>,
    pub providers: Arc<MemoryProviderStore>,
    pub tester: Arc<FakeTester>,
    pub win: Arc<FakeWinOps>,
}

/// Dependencies over in-memory fakes. `ui` = the dispatcher of the current (UI) thread.
/// `ui_language` is stored as `uiLanguage` first (`"tr"`, `"en"`, `"system"`), so the shared language follows it.
pub fn fake_deps(ui: Arc<dyn UiDispatcher>, fonts: Vec<String>, ui_language: &str) -> Fakes {
    let catalog = Rc::new(ModelCatalog::new(None, ui.clone()));
    fake_deps_with(ui, fonts, ui_language, catalog)
}

/// Like [`fake_deps`] over a given model catalog.
pub fn fake_deps_with(ui: Arc<dyn UiDispatcher>, fonts: Vec<String>, ui_language: &str, catalog: Rc<ModelCatalog>) -> Fakes {
    let settings = InMemorySettingsStore::new();
    settings.set_string("uiLanguage", ui_language);
    let store: Arc<dyn SettingsStore> = settings.clone();
    let vm = FakeVm::new(catalog.clone(), store.clone());
    let providers = MemoryProviderStore::new();
    let tester = Arc::new(FakeTester::default());
    let win = Arc::new(FakeWinOps {
        associations: Some(Arc::new(FakeAssociations::default())),
        keys_from: Some(providers.clone()),
        ..Default::default()
    });
    let language = Arc::new(AppLanguage::new(store.clone(), || vec!["tr-TR".to_owned()], LanguageState::shared().clone()));
    let deps = SettingsDeps {
        settings: store.clone(),
        ui,
        vm: vm.clone(),
        catalog,
        appearance: AppearanceSettings::new(store),
        language,
        providers: providers.clone(),
        tester: tester.clone(),
        win: Some(win.clone()),
        font_families: Arc::new(move || fonts.clone()),
        activity: None,
    };
    Fakes { deps, settings, vm, providers, tester, win }
}
