//! Provider UI logic (UNIFIED.md §3.13; mac-viewmodels §6, §9; win-ui-hub-settings §7–§8, win-providers §7):
//! provider rows with key captions (key cache, reads off the UI thread), the add/edit draft, connection tests with
//! their notices, and the toolbar translation-model picker model.
//!
//! Platform differences come from [`crate::policy::PlatformPolicy`] (new-provider async default, model ordering) and
//! from the injected [`ProviderConnectionTester`]: the default [`EngineKitConnectionTester`] maps
//! `ExternalLlmServer::test_connection` onto the macOS notices; the Windows app injects a tester over
//! `WinProviderManager::test_draft` (C# messages and merged model lists).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sa_core::cancel::CancellationToken;
use sa_core::l;
use sa_core::l10n::LocalizedMessage;
use sa_core::models::{ModelCatalog, ModelLabel};
use sa_core::secrets::SecretString;
use sa_core::settings::{defaults, keys};
use sa_core::subscription::Subscription;
use sa_enginekit::external_llm::{ConnectionResult, ExternalLlmServer, HttpFetch};
use sa_enginekit::presets::TranslationProviderPreset;
use sa_providers::selection::TranslationModelChoice;
use sa_providers::{ProviderKeyRead, TranslationProviderProfile, mask_key};
use uuid::Uuid;

use crate::services::AppServices;
use crate::uiref::UiKey;

/// Key state caption of a provider row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCaption {
    /// `Anahtar okunuyor…`
    Reading,
    /// `Anahtar yok`
    None,
    /// Masked key.
    Masked(String),
    /// `Anahtar okunamadı`
    Unreadable,
}

impl KeyCaption {
    /// The localized caption (mac-viewmodels §6.3; the Windows card shows the same texts).
    pub fn text(&self) -> String {
        match self {
            KeyCaption::Reading => l!("Anahtar okunuyor…"),
            KeyCaption::None => l!("Anahtar yok"),
            KeyCaption::Masked(m) => m.clone(),
            KeyCaption::Unreadable => l!("Anahtar okunamadı"),
        }
    }
}

/// The cached key of a provider (mac-viewmodels §6.3). Reads are slow/fallible and run off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProviderKey {
    #[default]
    Loading,
    Absent,
    /// Trimmed key.
    Present(SecretString),
    Unreadable,
}

impl ProviderKey {
    pub fn from_read(read: ProviderKeyRead) -> Self {
        match read {
            ProviderKeyRead::Found(k) => {
                let t = k.expose().trim();
                if t.is_empty() { ProviderKey::Absent } else { ProviderKey::Present(SecretString::new(t)) }
            }
            ProviderKeyRead::Absent => ProviderKey::Absent,
            ProviderKeyRead::Unavailable => ProviderKey::Unreadable,
        }
    }
    pub fn masked(&self) -> Option<String> {
        match self {
            ProviderKey::Present(k) => mask_key(k.expose()),
            _ => None,
        }
    }
    /// The key, or `""`.
    pub fn value(&self) -> String {
        match self {
            ProviderKey::Present(k) => k.expose().to_owned(),
            _ => String::new(),
        }
    }
    pub fn is_settled(&self) -> bool {
        matches!(self, ProviderKey::Absent | ProviderKey::Present(_))
    }
    pub fn caption(&self) -> KeyCaption {
        match self {
            ProviderKey::Loading => KeyCaption::Reading,
            ProviderKey::Absent => KeyCaption::None,
            ProviderKey::Present(_) => self.masked().map(KeyCaption::Masked).unwrap_or(KeyCaption::None),
            ProviderKey::Unreadable => KeyCaption::Unreadable,
        }
    }
}

/// A connection-test notice (macOS `ExternalConnectionNotice`; Windows status InfoBar with severity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalConnectionNotice {
    pub message: LocalizedMessage,
    pub is_error: bool,
}

impl ExternalConnectionNotice {
    pub fn error(message: LocalizedMessage) -> Self {
        Self { message, is_error: true }
    }
    pub fn success(message: LocalizedMessage) -> Self {
        Self { message, is_error: false }
    }
    pub fn text(&self) -> String {
        self.message.text()
    }
}

/// What a connection test produced. `notice == None` = cancelled (the old notice stays).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TestReport {
    pub notice: Option<ExternalConnectionNotice>,
    /// Discovered models (only on success).
    pub models: Vec<String>,
}

/// Runs a connection test (blocking; called on a worker thread).
pub trait ProviderConnectionTester: Send + Sync {
    fn test(&self, preset_id: &str, base_url: &str, api_key: &str, model: &str, cancel: &CancellationToken) -> TestReport;

    /// A connection test of the backend behind this tester is running anywhere in the app, not only through this
    /// model (Windows: C# `TranslationProviderManager.IsTesting`, which also counts the Settings window's tests).
    fn is_busy(&self) -> bool {
        false
    }
}

/// The offline notice (cloud providers are not contacted while offline).
pub fn offline_notice() -> ExternalConnectionNotice {
    ExternalConnectionNotice::error(LocalizedMessage::key("Çevrimdışı mod açık: internetteki sağlayıcılara bağlanılmaz."))
}

/// `"key-unreadable:<name>"` (UNIFIED.md §8: never mentions the Keychain).
pub fn key_unreadable_message(name: &str) -> LocalizedMessage {
    let n = name.to_owned();
    LocalizedMessage::new(format!("key-unreadable:{name}"), move || {
        l!("%@ için API anahtarı okunamadı. Anahtarı yeniden girip tekrar dene.", n.as_str())
    })
}

/// The macOS notice of a connection result (mac-viewmodels §6.4); `None` for a cancelled test.
pub fn notice_for(result: &ConnectionResult) -> Option<ExternalConnectionNotice> {
    Some(match result {
        ConnectionResult::Cancelled => return None,
        ConnectionResult::InvalidAddress => {
            ExternalConnectionNotice::error(LocalizedMessage::key("Lütfen geçerli bir sunucu adresi girin."))
        }
        ConnectionResult::Unauthorized { status } => {
            let s = *status;
            ExternalConnectionNotice::error(LocalizedMessage::new(format!("external-unauthorized:{s}"), move || {
                ExternalLlmServer::unauthorized_message(s)
            }))
        }
        ConnectionResult::Unreachable { reason } => {
            let r = reason.clone();
            ExternalConnectionNotice::error(LocalizedMessage::new(format!("external-unreachable:{reason}"), move || {
                l!("Sunucuya ulaşılamadı: %@", r.as_str())
            }))
        }
        ConnectionResult::InsecureKeyTransport => {
            ExternalConnectionNotice::error(LocalizedMessage::new("external-insecure-key", ExternalLlmServer::insecure_key_message))
        }
        ConnectionResult::Reachable { host, latency_ms, models } => {
            let (h, ms, n) = (host.clone(), *latency_ms as i64, models.len() as i64);
            ExternalConnectionNotice::success(LocalizedMessage::new(format!("external-ok:{host}:{latency_ms}:{n}"), move || {
                l!("Bağlantı başarılı (%@ · %lld ms · %lld uygun model bulundu)", h.as_str(), ms, n)
            }))
        }
    })
}

/// The default tester: `ExternalLlmServer::test_connection` over an [`HttpFetch`] (macOS notices).
pub struct EngineKitConnectionTester {
    fetch: Arc<dyn HttpFetch>,
}

impl EngineKitConnectionTester {
    pub fn new(fetch: Arc<dyn HttpFetch>) -> Arc<Self> {
        Arc::new(Self { fetch })
    }
}

impl ProviderConnectionTester for EngineKitConnectionTester {
    fn test(&self, preset_id: &str, base_url: &str, api_key: &str, model: &str, cancel: &CancellationToken) -> TestReport {
        let result = ExternalLlmServer::test_connection(base_url, api_key, model, Some(preset_id), self.fetch.as_ref(), cancel);
        TestReport { notice: notice_for(&result), models: result.models().to_vec() }
    }
}

/// A tester that never reaches anything (no network configured; tests).
pub struct UnavailableTester;

impl ProviderConnectionTester for UnavailableTester {
    fn test(&self, _: &str, base_url: &str, _: &str, _: &str, _: &CancellationToken) -> TestReport {
        let result = if ExternalLlmServer::normalize_base_url(base_url).is_none() {
            ConnectionResult::InvalidAddress
        } else {
            ConnectionResult::Unreachable { reason: ExternalLlmServer::failure_reason(None) }
        };
        TestReport { notice: notice_for(&result), models: Vec::new() }
    }
}

/// Add/edit form state; nothing stored until Save (mac-viewmodels §6.5). Preset defaults re-apply on a language
/// switch only while the draft is `pristine` (UNIFIED.md §3.13 FIX-W).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderDraft {
    /// `Some(id)` while editing a saved provider.
    pub editing: Option<Uuid>,
    /// The profile id (fresh for a new provider).
    pub id: Uuid,
    pub preset_id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub api_key: SecretString,
    pub async_translation: bool,
    pub discovered_models: Vec<String>,
    /// The stored key of the edited provider.
    pub stored_key: ProviderKey,
    /// The key field was touched.
    pub key_edited: bool,
    async_translation_chosen: bool,
    /// No field was edited by hand since the preset defaults were applied.
    pub pristine: bool,
}

impl ProviderDraft {
    /// A new provider from `preset_id` (unknown → custom); `async_default` per policy.
    pub fn new(preset_id: &str, async_default: bool) -> Self {
        let preset = TranslationProviderPreset::preset(Some(preset_id));
        Self {
            editing: None,
            id: Uuid::new_v4(),
            preset_id: preset.id.to_owned(),
            name: preset.name(None),
            base_url: preset.base_url.to_owned(),
            model: preset.default_model().to_owned(),
            api_key: SecretString::new(""),
            async_translation: async_default,
            discovered_models: Vec::new(),
            stored_key: ProviderKey::Absent,
            key_edited: false,
            async_translation_chosen: false,
            pristine: true,
        }
    }

    /// Editing a saved provider (the key field starts empty; `stored_key` holds the cached key).
    pub fn editing(profile: &TranslationProviderProfile, stored_key: ProviderKey) -> Self {
        Self {
            editing: Some(profile.id),
            id: profile.id,
            preset_id: profile.preset_id.clone(),
            name: profile.name.clone(),
            base_url: profile.base_url.clone(),
            model: profile.model.clone(),
            api_key: SecretString::new(""),
            async_translation: profile.async_translation,
            discovered_models: profile.discovered_models.clone(),
            stored_key,
            key_edited: false,
            async_translation_chosen: false,
            pristine: false,
        }
    }

    pub fn is_new(&self) -> bool {
        self.editing.is_none()
    }

    pub fn preset(&self) -> &'static TranslationProviderPreset {
        TranslationProviderPreset::preset(Some(&self.preset_id))
    }

    pub fn set_name(&mut self, name: &str) {
        if self.name != name {
            self.name = name.to_owned();
            self.pristine = false;
        }
    }

    /// A changed (trimmed) address forgets the models discovered for the old one.
    pub fn set_base_url(&mut self, url: &str) {
        if self.base_url.trim() != url.trim() {
            self.discovered_models.clear();
        }
        if self.base_url != url {
            self.base_url = url.to_owned();
            self.pristine = false;
        }
    }

    pub fn set_model(&mut self, model: &str) {
        if self.model != model {
            self.model = model.to_owned();
            self.pristine = false;
        }
    }

    /// Any change marks the key as edited.
    pub fn set_api_key(&mut self, key: &str) {
        if self.api_key.expose() != key {
            self.api_key = SecretString::new(key);
            self.key_edited = true;
            self.pristine = false;
        }
    }

    pub fn set_async_translation(&mut self, v: bool) {
        if self.async_translation != v {
            self.async_translation = v;
            self.async_translation_chosen = true;
            self.pristine = false;
        }
    }

    pub fn set_stored_key(&mut self, key: ProviderKey) {
        self.stored_key = key;
    }

    /// Switches the preset: name, address, model, discovered models follow; a new draft whose async flag was not
    /// chosen takes the preset default. The key is untouched.
    pub fn apply_preset(&mut self, preset_id: &str, async_default: bool) {
        let preset = TranslationProviderPreset::preset(Some(preset_id));
        self.preset_id = preset.id.to_owned();
        self.name = preset.name(None);
        self.base_url = preset.base_url.to_owned();
        self.model = preset.default_model().to_owned();
        self.discovered_models.clear();
        if self.is_new() && !self.async_translation_chosen {
            self.async_translation = async_default;
        }
        if self.is_new() && !self.key_edited {
            self.pristine = true;
        }
    }

    /// Language switch: a pristine new draft re-reads its (localized) preset defaults; anything typed is kept.
    pub fn relocalize(&mut self) {
        if self.is_new() && self.pristine {
            let preset = self.preset();
            self.name = preset.name(None);
        }
    }

    pub fn can_remove_stored_key(&self) -> bool {
        !self.is_new() && !self.key_edited && matches!(self.stored_key, ProviderKey::Present(_) | ProviderKey::Unreadable)
    }

    pub fn removes_stored_key(&self) -> bool {
        !self.is_new() && self.key_edited && self.api_key.expose().trim().is_empty() && self.stored_key != ProviderKey::Absent
    }

    pub fn remove_stored_key(&mut self) {
        self.api_key = SecretString::new("");
        self.key_edited = true;
        self.pristine = false;
    }

    /// The raw field when it must be written (`None` keeps the stored key).
    pub fn key_to_save(&self) -> Option<SecretString> {
        (self.is_new() || self.key_edited).then(|| self.api_key.clone())
    }

    /// The key a test of this draft sends.
    pub fn effective_key(&self) -> String {
        if self.is_new() || self.key_edited {
            self.api_key.expose().to_owned()
        } else {
            self.stored_key.value()
        }
    }

    /// The profile to store (never carries the key).
    pub fn profile(&self) -> TranslationProviderProfile {
        TranslationProviderProfile {
            id: self.id,
            name: self.name.clone(),
            preset_id: self.preset_id.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            async_translation: self.async_translation,
            discovered_models: self.discovered_models.clone(),
        }
    }

    /// Model suggestions of the draft (`model_first` = macOS order).
    pub fn available_models(&self, model_first: bool) -> Vec<String> {
        self.profile().available_models(model_first)
    }

    pub fn has_usable_address(&self) -> bool {
        ExternalLlmServer::normalize_base_url(&self.base_url).is_some()
    }
}

/// What a save did.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSaveResult {
    pub profile: TranslationProviderProfile,
    /// False when the encrypted secret store refused the key (`API anahtarı kaydedilemedi…`).
    pub key_stored: bool,
    /// The Windows status line (`%@ kaydedildi ve etkinleştirildi.` / `Sağlayıcı güncellendi ve etkinleştirildi.`).
    pub message: String,
}

/// Result of testing an unsaved draft.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderDraftTest {
    pub notice: Option<ExternalConnectionNotice>,
    pub models: Vec<String>,
}

/// What changed in [`ProvidersModel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProvidersChange {
    /// The provider list or the active provider.
    Providers,
    /// Key captions.
    Keys,
    /// Test notices / testing flags.
    Tests,
}

struct PState {
    keys: HashMap<Uuid, ProviderKey>,
    key_epochs: HashMap<Uuid, i64>,
    notices: HashMap<Uuid, ExternalConnectionNotice>,
    testing: HashSet<Uuid>,
    /// Tests the Settings window runs itself ([`ProvidersModel::begin_external_test`]).
    external_testing: HashSet<Uuid>,
    test_ids: HashMap<Uuid, Uuid>,
    test_tokens: HashMap<Uuid, CancellationToken>,
    loads_started: bool,
}

struct PInner {
    svc: AppServices,
    tester: Arc<dyn ProviderConnectionTester>,
    key: Cell<Option<UiKey<PInner>>>,
    st: RefCell<PState>,
    /// With a liveness flag for handlers registered through [`ProvidersModel::subscribe`].
    handlers: RefCell<Vec<(Option<Arc<AtomicBool>>, Rc<dyn Fn(ProvidersChange)>)>>,
    after_test: RefCell<Vec<(Uuid, Box<dyn FnOnce()>)>>,
    subs: RefCell<Vec<Subscription>>,
}

impl PInner {
    fn key(&self) -> UiKey<PInner> {
        self.key.get().expect("registered")
    }

    fn notify(&self, c: ProvidersChange) {
        let hs: Vec<Rc<dyn Fn(ProvidersChange)>> = {
            let mut all = self.handlers.borrow_mut();
            all.retain(|(alive, _)| alive.as_ref().is_none_or(|a| a.load(Ordering::SeqCst)));
            all.iter().map(|(_, h)| h.clone()).collect()
        };
        for h in hs {
            h(c);
        }
    }

    fn offline(&self) -> bool {
        self.svc.settings.get_bool(keys::OFFLINE_MODE).unwrap_or(defaults::OFFLINE_MODE)
    }

    fn provider(&self, id: Uuid) -> Option<TranslationProviderProfile> {
        self.svc.providers.providers().into_iter().find(|p| p.id == id)
    }

    /// Reads `ids`' keys on a worker; `done` runs on the UI thread after they were applied.
    fn load_keys(self: &Rc<Self>, ids: Vec<Uuid>, done: Option<Box<dyn FnOnce(&Rc<PInner>)>>) {
        let known: HashSet<Uuid> = self.svc.providers.providers().iter().map(|p| p.id).collect();
        let ids: Vec<Uuid> = ids.into_iter().filter(|i| known.contains(i)).collect();
        if ids.is_empty() {
            if let Some(d) = done {
                d(self);
            }
            return;
        }
        let epochs: HashMap<Uuid, i64> = {
            let mut st = self.st.borrow_mut();
            for id in &ids {
                st.keys.entry(*id).or_insert(ProviderKey::Loading);
            }
            ids.iter().map(|i| (*i, st.key_epochs.get(i).copied().unwrap_or(0))).collect()
        };
        self.notify(ProvidersChange::Keys);
        let store = self.svc.providers.clone();
        let key = self.key();
        let ui = self.svc.ui.clone();
        // `done` is not `Send`: park it on the UI thread under a token.
        let done_slot: u64 = DONE_SLOTS.with(|d| d.borrow_mut().put(done));
        let _ = std::thread::Builder::new().name("sa-provider-keys".into()).spawn(move || {
            let reads: Vec<(Uuid, ProviderKey)> = ids.iter().map(|id| (*id, ProviderKey::from_read(store.read_key(*id)))).collect();
            key.post(&ui, move |inner| {
                {
                    let mut st = inner.st.borrow_mut();
                    let known: HashSet<Uuid> = inner.svc.providers.providers().iter().map(|p| p.id).collect();
                    for (id, read) in reads {
                        if st.key_epochs.get(&id).copied().unwrap_or(0) != epochs[&id] || !known.contains(&id) {
                            continue;
                        }
                        if read == ProviderKey::Unreadable && matches!(st.keys.get(&id), Some(ProviderKey::Present(_))) {
                            continue;
                        }
                        st.keys.insert(id, read);
                    }
                }
                inner.notify(ProvidersChange::Keys);
                if let Some(d) = DONE_SLOTS.with(|d| d.borrow_mut().take(done_slot)) {
                    d(inner);
                }
            });
        });
    }

    fn record_key_written(self: &Rc<Self>, id: Uuid, key: &str, stored: bool) {
        {
            let mut st = self.st.borrow_mut();
            *st.key_epochs.entry(id).or_insert(0) += 1;
            let t = key.trim();
            let value = if t.is_empty() {
                ProviderKey::Absent
            } else if stored {
                ProviderKey::Present(SecretString::new(t))
            } else {
                ProviderKey::Loading
            };
            st.keys.insert(id, value.clone());
            if value == ProviderKey::Loading {
                drop(st);
                self.load_keys(vec![id], None);
            }
        }
        self.notify(ProvidersChange::Keys);
    }

    fn forget_key(&self, id: Uuid) {
        let mut st = self.st.borrow_mut();
        *st.key_epochs.entry(id).or_insert(0) += 1;
        st.keys.remove(&id);
    }

    fn forget_tests(&self, id: Uuid) {
        let mut st = self.st.borrow_mut();
        st.test_ids.remove(&id);
        st.testing.remove(&id);
        st.notices.remove(&id);
        if let Some(t) = st.test_tokens.remove(&id) {
            t.cancel();
        }
    }

    fn offline_blocks_server(&self, base_url: &str) -> bool {
        self.offline() && !offline_allowed_url(base_url)
    }
}

/// While offline only servers on the local network may be contacted (the engine's host rule).
fn offline_allowed_url(base_url: &str) -> bool {
    ExternalLlmServer::normalize_base_url(base_url)
        .and_then(|u| ExternalLlmServer::host(&u))
        .is_some_and(|h| ExternalLlmServer::is_local_network_host(&h))
}

/// UI-thread parking slots for non-`Send` continuations.
#[derive(Default)]
struct DoneSlots {
    next: u64,
    slots: HashMap<u64, Box<dyn FnOnce(&Rc<PInner>)>>,
}

impl DoneSlots {
    fn put(&mut self, f: Option<Box<dyn FnOnce(&Rc<PInner>)>>) -> u64 {
        self.next += 1;
        if let Some(f) = f {
            self.slots.insert(self.next, f);
        }
        self.next
    }
    fn take(&mut self, id: u64) -> Option<Box<dyn FnOnce(&Rc<PInner>)>> {
        self.slots.remove(&id)
    }
}

thread_local! {
    static DONE_SLOTS: RefCell<DoneSlots> = RefCell::new(DoneSlots::default());
}

/// Settings ▸ providers model (macOS Çeviri tab; Windows providers section of Genel). UI thread only.
pub struct ProvidersModel {
    inner: Rc<PInner>,
}

impl Drop for ProvidersModel {
    fn drop(&mut self) {
        if let Some(k) = self.inner.key.get() {
            k.unregister();
        }
    }
}

impl ProvidersModel {
    /// With the [`UnavailableTester`] (the app passes a real tester through [`Self::with_tester`]).
    pub fn new(services: AppServices) -> Self {
        Self::with_tester(services, Arc::new(UnavailableTester))
    }

    pub fn with_tester(services: AppServices, tester: Arc<dyn ProviderConnectionTester>) -> Self {
        let inner = Rc::new(PInner {
            svc: services,
            tester,
            key: Cell::new(None),
            st: RefCell::new(PState {
                keys: HashMap::new(),
                key_epochs: HashMap::new(),
                notices: HashMap::new(),
                testing: HashSet::new(),
                external_testing: HashSet::new(),
                test_ids: HashMap::new(),
                test_tokens: HashMap::new(),
                loads_started: false,
            }),
            handlers: RefCell::new(Vec::new()),
            after_test: RefCell::new(Vec::new()),
            subs: RefCell::new(Vec::new()),
        });
        let key = UiKey::register(&inner);
        inner.key.set(Some(key));
        {
            let ui = inner.svc.ui.clone();
            let sub = inner.svc.providers.subscribe(Box::new(move |_| {
                key.post(&ui, |inner| {
                    // Keys of removed providers are forgotten; new providers start loading.
                    let ids: Vec<Uuid> = inner.svc.providers.providers().iter().map(|p| p.id).collect();
                    let missing: Vec<Uuid> = {
                        let mut st = inner.st.borrow_mut();
                        st.keys.retain(|k, _| ids.contains(k));
                        ids.iter().filter(|i| !st.keys.contains_key(i)).copied().collect()
                    };
                    if !missing.is_empty() && inner.st.borrow().loads_started {
                        inner.load_keys(missing, None);
                    }
                    inner.notify(ProvidersChange::Providers);
                });
            }));
            inner.subs.borrow_mut().push(sub);
        }
        Self { inner }
    }

    pub fn on_change(&self, handler: Box<dyn Fn(ProvidersChange)>) {
        self.inner.handlers.borrow_mut().push((None, Rc::from(handler)));
    }

    /// Like [`Self::on_change`], but only while the returned subscription lives (a Settings window that comes and
    /// goes).
    pub fn subscribe(&self, handler: Box<dyn Fn(ProvidersChange)>) -> Subscription {
        let alive = Arc::new(AtomicBool::new(true));
        self.inner.handlers.borrow_mut().push((Some(alive.clone()), Rc::from(handler)));
        Subscription::new(move || alive.store(false, Ordering::SeqCst))
    }

    /// Runs once after the next saved-provider test of `id` applied its result (the picker's refresh uses it).
    fn once_after_test(&self, id: Uuid, handler: Box<dyn FnOnce()>) {
        self.inner.after_test.borrow_mut().push((id, handler));
    }

    /// Once at launch: reads every stored key off the UI thread.
    pub fn start_loading_keys(&self) {
        if std::mem::replace(&mut self.inner.st.borrow_mut().loads_started, true) {
            return;
        }
        let ids: Vec<Uuid> = self.inner.svc.providers.providers().iter().map(|p| p.id).collect();
        self.inner.load_keys(ids, None);
    }

    /// Re-reads the keys of `ids` (off the UI thread).
    pub fn load_keys(&self, ids: Vec<Uuid>) {
        self.inner.load_keys(ids, None);
    }

    pub fn providers(&self) -> Vec<(TranslationProviderProfile, KeyCaption)> {
        let st = self.inner.st.borrow();
        self.inner
            .svc
            .providers
            .providers()
            .into_iter()
            .map(|p| {
                let cap = st.keys.get(&p.id).cloned().unwrap_or_default().caption();
                (p, cap)
            })
            .collect()
    }

    pub fn provider_key(&self, id: Uuid) -> ProviderKey {
        self.inner.st.borrow().keys.get(&id).cloned().unwrap_or_default()
    }

    pub fn key_caption(&self, id: Uuid) -> KeyCaption {
        self.provider_key(id).caption()
    }

    pub fn active_provider_id(&self) -> Option<Uuid> {
        self.inner.svc.providers.active_provider_id()
    }

    /// The active provider with a usable address.
    pub fn active_external_provider(&self) -> Option<TranslationProviderProfile> {
        self.inner
            .svc
            .providers
            .active_provider()
            .filter(|p| ExternalLlmServer::normalize_base_url(&p.base_url).is_some())
    }

    /// `!ExternalLlm{url}.is_on_local_network`.
    pub fn sends_text_over_internet(provider: &TranslationProviderProfile) -> bool {
        !offline_allowed_url(&provider.base_url)
    }

    pub fn offline_blocks_server(&self, base_url: &str) -> bool {
        self.inner.offline_blocks_server(base_url)
    }

    pub fn begin_add(&self, preset_id: &str) -> ProviderDraft {
        let async_default = self.inner.svc.policy.new_provider_async_default(TranslationProviderPreset::preset(Some(preset_id)).id);
        ProviderDraft::new(preset_id, async_default)
    }

    /// Applies a preset to a draft with this platform's async default.
    pub fn apply_preset(&self, draft: &mut ProviderDraft, preset_id: &str) {
        let async_default = self.inner.svc.policy.new_provider_async_default(TranslationProviderPreset::preset(Some(preset_id)).id);
        draft.apply_preset(preset_id, async_default);
    }

    pub fn begin_edit(&self, id: Uuid) -> Option<ProviderDraft> {
        let p = self.inner.provider(id)?;
        Some(ProviderDraft::editing(&p, self.provider_key(id)))
    }

    /// Validates (`Lütfen geçerli bir sunucu adresi girin.`), stores (key encrypted) and activates.
    pub fn save(&self, draft: ProviderDraft) -> Result<ProviderSaveResult, String> {
        if draft.base_url.trim().is_empty() {
            return Err(l!("Lütfen geçerli bir sunucu adresi girin."));
        }
        let store = self.inner.svc.providers.clone();
        let mut profile = draft.profile();
        profile.name = profile.name.trim().to_owned();
        if profile.name.is_empty() {
            profile.name = draft.preset().name(None);
        }
        profile.base_url = profile.base_url.trim().to_owned();
        profile.model = profile.model.trim().to_owned();
        let key = draft.key_to_save();
        let trimmed_key = key.as_ref().map(|k| k.expose().trim().to_owned());
        match draft.editing {
            None => {
                let added = store.add(profile, None, true);
                let stored = match &trimmed_key {
                    Some(k) if !k.is_empty() => store.set_key(added.id, k),
                    _ => true,
                };
                self.inner.record_key_written(added.id, trimmed_key.as_deref().unwrap_or(""), stored);
                self.inner.forget_tests(added.id);
                let message = l!("%@ kaydedildi ve etkinleştirildi.", added.name.as_str());
                self.inner.notify(ProvidersChange::Providers);
                Ok(ProviderSaveResult { profile: added, key_stored: stored, message })
            }
            Some(id) => {
                if !store.update(profile, None) {
                    return Err(l!("Sağlayıcı bulunamadı."));
                }
                let stored = match &trimmed_key {
                    Some(k) => {
                        let ok = store.set_key(id, k);
                        self.inner.record_key_written(id, k, ok);
                        ok
                    }
                    None => true,
                };
                self.inner.forget_tests(id);
                store.set_active(Some(id));
                let stored_profile = self.inner.provider(id).unwrap_or_else(|| draft.profile());
                self.inner.notify(ProvidersChange::Providers);
                Ok(ProviderSaveResult {
                    profile: stored_profile,
                    key_stored: stored,
                    message: l!("Sağlayıcı güncellendi ve etkinleştirildi."),
                })
            }
        }
    }

    /// Removes the provider, its stored key and its test result (the active one falls back to on-device).
    pub fn delete(&self, id: Uuid) {
        self.inner.svc.providers.remove(id);
        self.inner.forget_key(id);
        self.inner.forget_tests(id);
        self.inner.notify(ProvidersChange::Providers);
    }

    pub fn activate(&self, id: Option<Uuid>) {
        if id.is_some_and(|i| self.inner.provider(i).is_none()) {
            return;
        }
        self.inner.svc.providers.set_active(id);
        self.inner.notify(ProvidersChange::Providers);
        self.refresh_active_provider_models_if_needed();
    }

    /// Settings row model (no activation).
    pub fn set_provider_model(&self, id: Uuid, model: &str) {
        let m = model.trim();
        if m.is_empty() {
            return;
        }
        self.inner.svc.providers.update_model(id, m);
    }

    /// Toolbar: the model AND activation.
    pub fn choose_provider_model(&self, id: Uuid, model: &str) {
        self.inner.svc.providers.update_model(id, model.trim());
        self.inner.svc.providers.set_active(Some(id));
        self.inner.notify(ProvidersChange::Providers);
    }

    pub fn notice(&self, id: Uuid) -> Option<ExternalConnectionNotice> {
        self.inner.st.borrow().notices.get(&id).cloned()
    }

    pub fn clear_notice(&self, id: Uuid) {
        self.inner.st.borrow_mut().notices.remove(&id);
        self.inner.notify(ProvidersChange::Tests);
    }

    pub fn is_testing(&self, id: Uuid) -> bool {
        let st = self.inner.st.borrow();
        st.testing.contains(&id) || st.external_testing.contains(&id)
    }

    /// Cancels the running saved-provider test of `id`: the request is cancelled, a late answer is dropped (nothing
    /// is stored) and the `once_after_test` handlers run now.
    pub fn cancel_test(&self, id: Uuid) {
        let had = {
            let mut st = self.inner.st.borrow_mut();
            let had = st.test_ids.remove(&id).is_some();
            st.testing.remove(&id);
            if let Some(t) = st.test_tokens.remove(&id) {
                t.cancel();
            }
            had
        };
        if had {
            self.inner.notify(ProvidersChange::Tests);
            Self::finish_test(&self.inner, id);
        }
    }

    /// Any connection test of the tester's backend runs anywhere in the app ([`ProviderConnectionTester::is_busy`]).
    pub fn is_tester_busy(&self) -> bool {
        self.inner.tester.is_busy()
    }

    /// The Settings window started its own test of `id` (macOS: `vm.testingProviders` is shared, so the picker's
    /// "Yenile" waits for it). The previous notice is about an earlier test and goes.
    pub fn begin_external_test(&self, id: Uuid) {
        {
            let mut st = self.inner.st.borrow_mut();
            st.external_testing.insert(id);
            st.notices.remove(&id);
        }
        self.inner.notify(ProvidersChange::Tests);
    }

    /// The Settings window's test of `id` ended; `notice` (if any) is its outcome, shown by both the Settings card
    /// and the picker (macOS `vm.providerNotices`).
    pub fn end_external_test(&self, id: Uuid, notice: Option<ExternalConnectionNotice>) {
        {
            let mut st = self.inner.st.borrow_mut();
            st.external_testing.remove(&id);
            if let Some(n) = notice {
                st.notices.insert(id, n);
            }
        }
        self.inner.notify(ProvidersChange::Tests);
    }

    /// Sets (or with `None` clears) the notice of `id` without a test (the Settings window's offline notice, a
    /// forgotten test after an edit).
    pub fn set_notice(&self, id: Uuid, notice: Option<ExternalConnectionNotice>) {
        {
            let mut st = self.inner.st.borrow_mut();
            match notice {
                Some(n) => st.notices.insert(id, n),
                None => st.notices.remove(&id),
            };
        }
        self.inner.notify(ProvidersChange::Tests);
    }

    /// Tests a saved provider (mac-viewmodels §6.4): offline-blocked cloud servers are not contacted; the key comes
    /// from the cache (read again when not settled); an unreadable key is reported without testing; late answers
    /// for a superseded test, a removed provider, an edited address or a changed key are dropped. On success the
    /// discovered models are stored and an empty model takes the first.
    pub fn test_saved(&self, id: Uuid) {
        self.test_saved_gated(id, true);
    }

    /// [`Self::test_saved`]; `offline_gate == false` contacts the server even in offline mode (the Windows picker's
    /// "Yenile": C# `TestAndFetchModelsAsync` has no offline check).
    fn test_saved_gated(&self, id: Uuid, offline_gate: bool) {
        let inner = &self.inner;
        let Some(provider) = inner.provider(id) else { return };
        if offline_gate && inner.offline_blocks_server(&provider.base_url) {
            inner.forget_tests(id);
            inner.st.borrow_mut().notices.insert(id, offline_notice());
            inner.notify(ProvidersChange::Tests);
            Self::finish_test(inner, id);
            return;
        }
        let test_id = Uuid::new_v4();
        {
            let mut st = inner.st.borrow_mut();
            st.test_ids.insert(id, test_id);
            st.testing.insert(id);
            if let Some(t) = st.test_tokens.remove(&id) {
                t.cancel();
            }
            st.test_tokens.insert(id, CancellationToken::new());
        }
        inner.notify(ProvidersChange::Tests);
        let settled = inner.st.borrow().keys.get(&id).is_some_and(ProviderKey::is_settled);
        if settled {
            Self::run_saved_test(inner, id, test_id);
        } else {
            inner.load_keys(
                vec![id],
                Some(Box::new(move |inner: &Rc<PInner>| Self::run_saved_test(inner, id, test_id))),
            );
        }
    }

    fn finish_test(inner: &Rc<PInner>, id: Uuid) {
        let ready: Vec<Box<dyn FnOnce()>> = {
            let mut all = inner.after_test.borrow_mut();
            let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut *all).into_iter().partition(|(i, _)| *i == id);
            *all = rest;
            mine.into_iter().map(|(_, f)| f).collect()
        };
        for f in ready {
            f();
        }
    }

    fn run_saved_test(inner: &Rc<PInner>, id: Uuid, test_id: Uuid) {
        if inner.st.borrow().test_ids.get(&id) != Some(&test_id) {
            return;
        }
        let Some(provider) = inner.provider(id) else {
            inner.forget_tests(id);
            inner.notify(ProvidersChange::Tests);
            Self::finish_test(inner, id);
            return;
        };
        let cached = inner.st.borrow().keys.get(&id).cloned().unwrap_or_default();
        if cached == ProviderKey::Unreadable {
            {
                let mut st = inner.st.borrow_mut();
                st.test_ids.remove(&id);
                st.testing.remove(&id);
                st.notices.insert(id, ExternalConnectionNotice::error(key_unreadable_message(&provider.name)));
            }
            inner.notify(ProvidersChange::Tests);
            Self::finish_test(inner, id);
            return;
        }
        let key_value = cached.value();
        let token = inner.st.borrow().test_tokens.get(&id).cloned().unwrap_or_default();
        let tester = inner.tester.clone();
        let ui = inner.svc.ui.clone();
        let ukey = inner.key();
        let p = provider.clone();
        let _ = std::thread::Builder::new().name("sa-provider-test".into()).spawn(move || {
            let report = tester.test(&p.preset_id, &p.base_url, &key_value, &p.model, &token);
            ukey.post(&ui, move |inner| {
                if inner.st.borrow().test_ids.get(&id) != Some(&test_id) {
                    return;
                }
                {
                    let mut st = inner.st.borrow_mut();
                    st.test_ids.remove(&id);
                    st.testing.remove(&id);
                    st.test_tokens.remove(&id);
                }
                let current = inner.provider(id);
                let key_now = inner.st.borrow().keys.get(&id).cloned().unwrap_or_default();
                let still_valid = current.as_ref().is_some_and(|c| c.base_url == p.base_url) && key_now.value() == key_value;
                if still_valid && let Some(notice) = report.notice.clone() {
                    let ok = !notice.is_error;
                    inner.st.borrow_mut().notices.insert(id, notice);
                    if ok && !report.models.is_empty() {
                        inner.svc.providers.set_discovered_models(id, &report.models);
                        if current.as_ref().is_some_and(|c| c.model.trim().is_empty()) {
                            inner.svc.providers.update_model(id, &report.models[0]);
                        }
                    }
                }
                inner.notify(ProvidersChange::Tests);
                Self::finish_test(inner, id);
            });
        });
    }

    /// Tests an unsaved draft; nothing is stored. `done` runs on the UI thread.
    pub fn test_draft(&self, draft: &ProviderDraft, done: Box<dyn FnOnce(ProviderDraftTest)>) {
        if self.inner.offline_blocks_server(&draft.base_url) {
            done(ProviderDraftTest { notice: Some(offline_notice()), models: Vec::new() });
            return;
        }
        let tester = self.inner.tester.clone();
        let (preset, url, key, model) = (draft.preset_id.clone(), draft.base_url.clone(), draft.effective_key(), draft.model.clone());
        let ui = self.inner.svc.ui.clone();
        let ukey = self.inner.key();
        // `done` is not `Send`: park it on the UI thread under a slot id.
        let slot = DRAFT_DONE.with(|d| {
            let mut d = d.borrow_mut();
            d.0 += 1;
            let id = d.0;
            d.1.insert(id, done);
            id
        });
        let _ = std::thread::Builder::new().name("sa-provider-test".into()).spawn(move || {
            let report = tester.test(&preset, &url, &key, &model, &CancellationToken::new());
            ukey.post(&ui, move |_inner| {
                if let Some(done) = DRAFT_DONE.with(|d| d.borrow_mut().1.remove(&slot)) {
                    done(ProviderDraftTest { notice: report.notice, models: report.models });
                }
            });
        });
    }

    /// The explicit "Test" click of a draft (API.md signature): reports the notice result as a
    /// [`ConnectionResult`]-free [`ProviderDraftTest`].
    pub fn test(&self, draft: &ProviderDraft, done: Box<dyn FnOnce(ProviderDraftTest)>) {
        self.test_draft(draft, done);
    }

    /// Active usable provider on the LOCAL network with nothing discovered, not testing, not offline-blocked →
    /// test it (cloud servers are contacted only on request). At launch and on provider switch.
    pub fn refresh_active_provider_models_if_needed(&self) {
        let Some(p) = self.active_external_provider() else { return };
        if !p.discovered_models.is_empty() || Self::sends_text_over_internet(&p) || self.is_testing(p.id) {
            return;
        }
        if self.offline_blocks_server(&p.base_url) {
            return;
        }
        self.test_saved(p.id);
    }
}

thread_local! {
    static DRAFT_DONE: RefCell<(u64, HashMap<u64, Box<dyn FnOnce(ProviderDraftTest)>>)> = RefCell::new((0, HashMap::new()));
}

/// What the translation-model picker needs from the editor view model (implemented by `SubtitleViewModel`).
pub trait PickerHost {
    /// The on-device translation model (`translationModel`).
    fn translation_model(&self) -> String;
    fn set_translation_model(&self, id: &str);
}

/// A picker row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    pub id: String,
    pub title: String,
    pub is_current: bool,
    pub is_enabled: bool,
}

/// A provider choice of the picker (`None` = on-device).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChoice {
    pub id: Option<Uuid>,
    pub name: String,
}

struct PickerState {
    browsed: Option<Uuid>,
    search: String,
    highlighted: Option<String>,
    status: Option<LocalizedMessage>,
    refreshing: bool,
    /// The provider this picker's running refresh tests.
    refresh_id: Option<Uuid>,
    epoch: i64,
}

type PickerHandler = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// Toolbar translation-model picker (Windows flyout / macOS popover with `browsedProviderID`; mac-viewmodels §9,
/// win-ui-hub-settings §8). The two apps differ (UNIFIED.md §17, KEEP each):
///
/// * Windows (C# `TranslationModelPicker` + `TranslationModelSelection`): the provider combo **activates** the provider
///   it shows; rows are never disabled; the status line shows only the last refresh's message (verbatim, also for a
///   failed test); "Yenile" is enabled for any provider; the selected row is the provider's `Model` / the
///   `translationModel` setting.
/// * macOS (`TranslationModelPickerModel.swift`): browsing does not change what translates; rows of a blocked provider
///   (offline, unusable address) are disabled with the reason in the status line; a failed refresh reads
///   `Modeller yenilenemedi: <reason>`; ↑/↓/Return move and choose the highlight.
pub struct TranslationModelPickerModel {
    svc: AppServices,
    catalog: Rc<ModelCatalog>,
    providers: Rc<ProvidersModel>,
    host: Rc<dyn PickerHost>,
    st: Rc<RefCell<PickerState>>,
    changed: PickerHandler,
}

/// A [`TranslationModelPickerModel`] held weakly (late test answers must not keep a closed picker alive).
struct WeakPicker {
    svc: AppServices,
    catalog: Weak<ModelCatalog>,
    providers: Weak<ProvidersModel>,
    host: Weak<dyn PickerHost>,
    st: Weak<RefCell<PickerState>>,
    changed: Weak<RefCell<Option<Rc<dyn Fn()>>>>,
}

impl WeakPicker {
    fn upgrade(&self) -> Option<TranslationModelPickerModel> {
        Some(TranslationModelPickerModel {
            svc: self.svc.clone(),
            catalog: self.catalog.upgrade()?,
            providers: self.providers.upgrade()?,
            host: self.host.upgrade()?,
            st: self.st.upgrade()?,
            changed: self.changed.upgrade()?,
        })
    }
}

/// macOS model ordering (`model_first`, lowercase sort) vs Windows (OrdinalIgnoreCase).
fn picker_is_mac(svc: &AppServices) -> bool {
    svc.policy.settings_manage_providers_page == crate::policy::SettingsPage::Translation
}

impl TranslationModelPickerModel {
    pub fn new(services: AppServices, catalog: Rc<ModelCatalog>, providers: Rc<ProvidersModel>, host: Rc<dyn PickerHost>) -> Self {
        let picker = Self {
            svc: services,
            catalog,
            providers,
            host,
            st: Rc::new(RefCell::new(PickerState {
                browsed: None,
                search: String::new(),
                highlighted: None,
                status: None,
                refreshing: false,
                refresh_id: None,
                epoch: 0,
            })),
            changed: Rc::new(RefCell::new(None)),
        };
        picker.st.borrow_mut().browsed = picker.initial_browsed();
        picker.reset_highlight();
        picker
    }

    fn is_mac(&self) -> bool {
        picker_is_mac(&self.svc)
    }

    /// macOS `vm.activeExternalProvider?.id` (a provider with an unusable address lists the local models); Windows
    /// `manager.ActiveProviderId`.
    fn initial_browsed(&self) -> Option<Uuid> {
        if self.is_mac() {
            self.providers.active_external_provider().map(|p| p.id)
        } else {
            self.providers.active_provider_id()
        }
    }

    /// Called after state changes that do not come from a call of the UI (a refresh that finished). One handler; the
    /// presenter re-reads everything it shows.
    pub fn set_on_change(&self, handler: Box<dyn Fn()>) {
        *self.changed.borrow_mut() = Some(Rc::from(handler));
    }

    /// The popover/flyout opens: browse the active provider, clear search and status.
    pub fn open(&self) {
        self.abandon_refresh();
        let browsed = self.initial_browsed();
        {
            let mut st = self.st.borrow_mut();
            st.browsed = browsed;
            st.search.clear();
            st.status = None;
        }
        self.reset_highlight();
    }

    pub fn close(&self) {
        self.abandon_refresh();
        self.st.borrow_mut().status = None;
    }

    /// `None` = on-device.
    pub fn browsed_provider(&self) -> Option<Uuid> {
        self.st.borrow().browsed
    }

    /// The browsed provider still exists (macOS shows "Yenile" only then).
    pub fn has_browsed_profile(&self) -> bool {
        self.browsed_profile().is_some()
    }

    pub fn provider_choices(&self) -> Vec<ProviderChoice> {
        let mut v = vec![ProviderChoice { id: None, name: l!("Yerel modeller (Cihaz üzerinde)") }];
        v.extend(self.svc.providers.providers().into_iter().map(|p| ProviderChoice { id: Some(p.id), name: p.name }));
        v
    }

    /// Index of the browsed provider in [`Self::provider_choices`] (`-1` when it is not listed).
    pub fn provider_index(&self) -> i32 {
        let browsed = self.st.borrow().browsed;
        self.provider_choices().iter().position(|c| c.id == browsed).map(|i| i as i32).unwrap_or(-1)
    }

    fn browsed_profile(&self) -> Option<TranslationProviderProfile> {
        let id = self.st.borrow().browsed?;
        self.svc.providers.providers().into_iter().find(|p| p.id == id)
    }

    /// Switches the browsed provider. Windows: selecting a provider in the flyout activates it at once (C#
    /// `SelectProvider`: a removed provider is refused, nothing is tested). macOS: browsing only.
    pub fn browse(&self, provider: Option<Uuid>) -> bool {
        if let Some(id) = provider
            && !self.svc.providers.providers().iter().any(|p| p.id == id)
        {
            return false;
        }
        // Re-choosing the shown provider changes nothing (WinUI raises no SelectionChanged for it).
        if self.st.borrow().browsed == provider {
            return true;
        }
        self.abandon_refresh();
        if !self.is_mac() {
            self.svc.providers.set_active(provider);
        }
        {
            let mut st = self.st.borrow_mut();
            st.browsed = provider;
            st.search.clear();
            st.status = None;
        }
        self.reset_highlight();
        true
    }

    /// The store changed (macOS `providersChanged`): a browsed provider that is gone falls back to on-device with
    /// `Bu sağlayıcı artık yok.`.
    pub fn providers_changed(&self) {
        let gone = self.st.borrow().browsed.is_some() && self.browsed_profile().is_none();
        if gone {
            self.abandon_refresh();
            let mut st = self.st.borrow_mut();
            st.browsed = None;
            st.search.clear();
            st.status = Some(LocalizedMessage::key("Bu sağlayıcı artık yok."));
        }
        self.reset_highlight();
    }

    pub fn search_text(&self) -> String {
        self.st.borrow().search.clone()
    }

    pub fn set_search_text(&self, text: &str) {
        let changed = {
            let mut st = self.st.borrow_mut();
            if st.search == text {
                false
            } else {
                st.search = text.to_owned();
                true
            }
        };
        if changed {
            self.reset_highlight();
        }
    }

    /// Model names of a provider (discovered + model, or the suggestions; trimmed, distinct, sorted).
    pub fn model_names(&self, provider: &TranslationProviderProfile) -> Vec<String> {
        sa_providers::selection::provider_models_for(provider, self.is_mac()).into_iter().map(|c| c.id).collect()
    }

    fn block_reason(&self, p: &TranslationProviderProfile) -> Option<LocalizedMessage> {
        if ExternalLlmServer::normalize_base_url(&p.base_url).is_none() {
            return Some(LocalizedMessage::key("Lütfen geçerli bir sunucu adresi girin."));
        }
        if self.providers.offline_blocks_server(&p.base_url) {
            return Some(LocalizedMessage::key("Çevrimdışı mod açık: internetteki sağlayıcılara bağlanılmaz."));
        }
        None
    }

    fn all_rows(&self) -> Vec<PickerRow> {
        let browsed = self.st.borrow().browsed;
        let mac = self.is_mac();
        match browsed {
            None => {
                let active = self.svc.providers.active_provider_id();
                let current = self.host.translation_model();
                self.catalog
                    .translation_models()
                    .iter()
                    .map(|m| PickerRow {
                        id: m.id.clone(),
                        title: if mac { ModelLabel::translation_menu_title(m) } else { ModelLabel::menu_title(m) },
                        // Windows: `SelectedItem` = the model equal to `vm.TranslationModel`.
                        is_current: (!mac || active.is_none()) && m.id == current,
                        is_enabled: true,
                    })
                    .collect()
            }
            Some(_) => {
                let Some(p) = self.browsed_profile() else { return Vec::new() };
                let active = self.svc.providers.active_provider_id() == Some(p.id);
                let enabled = !mac || self.block_reason(&p).is_none();
                let model = p.model.trim().to_lowercase();
                self.model_names(&p)
                    .into_iter()
                    .map(|n| PickerRow {
                        // Windows: the provider's `Model` (ordinal); macOS: the active provider's model, any case.
                        is_current: if mac { active && n.to_lowercase() == model } else { n == p.model },
                        title: n.clone(),
                        id: n,
                        is_enabled: enabled,
                    })
                    .collect()
            }
        }
    }

    /// Visible rows (title contains the trimmed search, case-insensitive).
    pub fn picker_rows(&self) -> Vec<PickerRow> {
        let q = self.st.borrow().search.trim().to_lowercase();
        self.all_rows().into_iter().filter(|r| q.is_empty() || r.title.to_lowercase().contains(&q)).collect()
    }

    /// API.md shape: `(id, title)` of the visible rows.
    pub fn rows(&self) -> Vec<TranslationModelChoice> {
        self.picker_rows().into_iter().map(|r| TranslationModelChoice { id: r.id, title: r.title }).collect()
    }

    pub fn empty_text(&self) -> Option<String> {
        if !self.picker_rows().is_empty() {
            return None;
        }
        if !self.all_rows().is_empty() || !self.st.borrow().search.trim().is_empty() {
            Some(l!("Aramayla eşleşen model yok."))
        } else {
            Some(l!("Model listesi boş. Yenile düğmesini kullanın."))
        }
    }

    /// The line under the list. macOS: the refresh state/outcome, else why the provider can't be chosen from.
    /// Windows: only the last refresh (`StatusText`, cleared by `LoadModels`).
    pub fn status_text(&self) -> Option<String> {
        if let Some(s) = &self.st.borrow().status {
            return Some(s.text());
        }
        if !self.is_mac() {
            return None;
        }
        self.browsed_profile().and_then(|p| self.block_reason(&p)).map(|m| m.text())
    }

    /// Picks a row: a provider model updates the provider's model AND activates it; an on-device model
    /// deactivates the provider and sets `translationModel`.
    pub fn select(&self, model_id: &str) -> bool {
        let browsed = self.st.borrow().browsed;
        match browsed {
            Some(_) => {
                let Some(p) = self.browsed_profile() else {
                    if self.is_mac() {
                        self.providers_changed();
                    }
                    return false;
                };
                if self.is_mac() && self.block_reason(&p).is_some() {
                    return false;
                }
                if !self.model_names(&p).iter().any(|n| n == model_id) {
                    return false;
                }
                self.providers.choose_provider_model(p.id, model_id);
                true
            }
            None => {
                if !self.catalog.translation_models().iter().any(|m| m.id == model_id) {
                    return false;
                }
                self.providers.activate(None);
                self.host.set_translation_model(model_id);
                true
            }
        }
    }

    pub fn highlighted_id(&self) -> Option<String> {
        self.st.borrow().highlighted.clone()
    }

    pub fn choose_highlighted(&self) -> bool {
        let Some(h) = self.highlighted_id() else { return false };
        if !self.picker_rows().iter().any(|r| r.id == h && r.is_enabled) {
            return false;
        }
        self.select(&h)
    }

    pub fn move_highlight(&self, step: i64) {
        let ids: Vec<String> = self.picker_rows().into_iter().filter(|r| r.is_enabled).map(|r| r.id).collect();
        let next = if ids.is_empty() {
            None
        } else {
            let cur = self.st.borrow().highlighted.clone();
            match cur.and_then(|c| ids.iter().position(|i| *i == c)) {
                None => Some(if step >= 0 { ids[0].clone() } else { ids[ids.len() - 1].clone() }),
                Some(i) => {
                    let j = (i as i64 + step).clamp(0, ids.len() as i64 - 1) as usize;
                    Some(ids[j].clone())
                }
            }
        };
        self.st.borrow_mut().highlighted = next;
    }

    fn reset_highlight(&self) {
        let rows: Vec<PickerRow> = self.picker_rows().into_iter().filter(|r| r.is_enabled).collect();
        let pick = rows
            .iter()
            .find(|r| r.is_current)
            .or_else(|| {
                let model = self.browsed_profile().map(|p| p.model.trim().to_lowercase());
                model.and_then(|m| rows.iter().find(|r| r.id.to_lowercase() == m))
            })
            .or(rows.first())
            .map(|r| r.id.clone());
        self.st.borrow_mut().highlighted = pick;
    }

    pub fn is_refreshing(&self) -> bool {
        self.st.borrow().refreshing
    }

    /// macOS: a saved provider that can be asked, not while a test of it runs. Windows: `RefreshButton.IsEnabled` =
    /// a provider is browsed (and no refresh of this panel is running).
    pub fn can_refresh(&self) -> bool {
        let Some(p) = self.browsed_profile() else { return false };
        if self.st.borrow().refreshing {
            return false;
        }
        if !self.is_mac() {
            return true;
        }
        !self.providers.is_testing(p.id) && self.block_reason(&p).is_none()
    }

    fn downgrade(&self) -> WeakPicker {
        WeakPicker {
            svc: self.svc.clone(),
            catalog: Rc::downgrade(&self.catalog),
            providers: Rc::downgrade(&self.providers),
            host: Rc::downgrade(&self.host),
            st: Rc::downgrade(&self.st),
            changed: Rc::downgrade(&self.changed),
        }
    }

    /// "Yenile": tests the browsed provider; a late answer after a switch or close is dropped and never selects a
    /// provider on the user's behalf. Windows: ignored while any connection test runs (C# `_manager.IsTesting`, the
    /// Settings window's included); the status is the test's own message (C# `result.Message`); offline mode does
    /// not block the request (C# has no check there). macOS: when it ends the highlight moves to the current row
    /// (`finishRefresh` → `resetHighlight()`).
    pub fn refresh(&self) {
        if !self.can_refresh() {
            return;
        }
        let Some(p) = self.browsed_profile() else { return };
        let mac = self.is_mac();
        if !mac && (self.providers.is_testing(p.id) || self.providers.is_tester_busy()) {
            return;
        }
        let mine = {
            let mut st = self.st.borrow_mut();
            st.epoch += 1;
            st.refreshing = true;
            st.refresh_id = Some(p.id);
            st.status = Some(LocalizedMessage::key("Modeller yenileniyor…"));
            st.epoch
        };
        self.providers.clear_notice(p.id);
        let weak = self.downgrade();
        let id = p.id;
        self.providers.once_after_test(id, Box::new(move || {
            let Some(picker) = weak.upgrade() else { return };
            {
                let mut s = picker.st.borrow_mut();
                if s.epoch != mine {
                    return;
                }
                s.refreshing = false;
                s.refresh_id = None;
            }
            // Swift `finishRefresh`: the answer counts only while that provider is still browsed and still saved.
            let still_browsed = picker.st.borrow().browsed == Some(id) && picker.browsed_profile().is_some();
            let status = match picker.providers.notice(id) {
                Some(notice) if still_browsed => Some(if notice.is_error && mac {
                    let inner = notice.message.clone();
                    LocalizedMessage::new(format!("refresh-failed:{}", inner.id()), move || {
                        l!("Modeller yenilenemedi: %@", inner.text())
                    })
                } else {
                    notice.message
                }),
                _ => None,
            };
            picker.st.borrow_mut().status = status;
            if mac {
                picker.reset_highlight();
            }
            let handler = picker.changed.borrow().clone();
            if let Some(h) = handler {
                h();
            }
        }));
        self.providers.test_saved_gated(id, mac);
    }

    /// Drops a refresh under way and cancels its request (C# `Close()`: revision++ and the token is cancelled; Swift
    /// `abandonRefresh` cancels the task): a late answer neither shows nor stores discovered models.
    fn abandon_refresh(&self) {
        let running = {
            let mut st = self.st.borrow_mut();
            st.epoch += 1;
            st.refreshing = false;
            st.refresh_id.take()
        };
        if let Some(id) = running {
            self.providers.cancel_test(id);
        }
    }

    /// Re-reads after a refresh or store change (highlight follows).
    pub fn refresh_rows(&self) {
        self.reset_highlight();
    }

    /// SF Symbol / glyph of the toolbar button: `None` (on-device), `"cloud"` or `"network"`.
    pub fn button_symbol(&self) -> Option<&'static str> {
        let p = self.providers.active_external_provider()?;
        Some(if ProvidersModel::sends_text_over_internet(&p) { "cloud" } else { "network" })
    }

    /// Toolbar tooltip second line.
    pub fn help_detail(&self) -> String {
        match self.providers.active_external_provider() {
            None => l!("Çeviri motoru (Gemma 4, Qwen 3.5, MADLAD, NLLB); ilk kullanımda indirilir."),
            Some(p) if ProvidersModel::sends_text_over_internet(&p) => {
                l!("Çeviri %@ sağlayıcısında yapılır: çevrilecek metin ona gönderilir, ses gönderilmez.", p.name.as_str())
            }
            Some(_) => l!("Çeviriyi yerel ağdaki başka bir bilgisayara veya Mac'e (LM Studio, Ollama, llama-server) yaptırır."),
        }
    }
}
