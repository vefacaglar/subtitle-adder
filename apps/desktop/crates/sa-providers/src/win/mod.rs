//! Windows `TranslationProviderManager` (win-providers §2–§3, §8): C# file shape, CRLF, case-insensitive read,
//! legacy "Harici LLM sunucusu (Yerel ağ)" bridge to a `lan` profile, broker `providers` topic.
//! FIX-W: corrupt file set aside before the first save; plaintext `apiKey` migrated into the encrypted sidecar.
//!
//! Keys: the C# app keeps API keys in plaintext in `translation-providers.json` (there are no DPAPI-protected
//! provider keys to read; win-providers §0). On every load a non-blank plaintext `apiKey` is encrypted into the
//! sidecar first (plaintext wins over an older blob), then the JSON is rewritten with `"apiKey": ""`. If the sidecar
//! cannot be written (data key unavailable) the plaintext the C# app wrote is left where it is rather than lost.
//! Decrypted keys are held in memory as zeroizing [`SecretString`]s and never logged or published.
//!
//! Threading: every mutation runs under one mutex (the file write happens inside it); notifications, engine-state sync
//! and broker publishing run after it is released, on the calling thread. Broker events arrive on the bus thread;
//! subscribers re-post UI work to the event loop.

mod connection;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_core::secrets::{SecretRead, SecretStore, SecretString};
use sa_core::subscription::{Subscribers, Subscription};
use sa_enginekit::external_llm::HttpFetch;
use sa_enginekit::launch::{EngineLaunchState, ExternalLlm};
use sa_enginekit::presets::TranslationProviderPreset;
use sa_sync::{EventKind, StateSyncEvent, SyncBus, Topic, TopicFilter};
use serde_json::{Map, Value};
use uuid::Uuid;

pub use connection::{
    LAN_TEST_TIMEOUT, ProviderTestOutcome, TestCancelled, candidate_model_endpoints, chat_completions_endpoint, is_cloud_provider_url,
    is_opencode_go_url, merge_models, normalize_external_base_url, parse_suitable_external_models, read_provider_error,
    test_lan_server,
};

use crate::fileio::{self, ReadOutcome};
use crate::json::{self, Style};
use crate::secrets::EncryptedSecretStore;
use crate::{ActiveExternalLlm, ProviderChange, ProviderKeyRead, ProviderLoadProblem, ProviderStore, TranslationProviderProfile};

/// `translation-providers.json`.
pub const FILE_NAME: &str = "translation-providers.json";

/// In-memory key of a provider.
#[derive(Clone)]
enum KeySlot {
    Absent,
    Found(SecretString),
    /// The sidecar entry exists but cannot be decrypted (kept on disk, never treated as absent).
    Unavailable,
}

impl KeySlot {
    fn engine_key(&self) -> String {
        match self {
            KeySlot::Found(k) => k.expose().trim().to_owned(),
            _ => String::new(),
        }
    }
}

#[derive(Clone)]
struct Entry {
    profile: TranslationProviderProfile,
    key: KeySlot,
    /// A C#-written plaintext key that could not be moved into the sidecar yet: written back as-is (never lost).
    plaintext_pending: bool,
}

#[derive(Default)]
struct WinState {
    entries: Vec<Entry>,
    active: Option<Uuid>,
    /// Why the file may not be overwritten yet (Unreadable: re-read first; Damaged: aside copy first).
    problem: Option<ProviderLoadProblem>,
}

impl WinState {
    fn find(&self, id: Uuid) -> Option<&Entry> {
        self.entries.iter().find(|e| e.profile.id == id)
    }
    fn find_mut(&mut self, id: Uuid) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.profile.id == id)
    }
    fn active_entry(&self) -> Option<&Entry> {
        self.active.and_then(|id| self.find(id))
    }
}

/// A profile as read from a C#-written file, with its plaintext key.
struct ParsedProfile {
    profile: TranslationProviderProfile,
    plaintext_key: String,
}

struct ParsedDoc {
    active: Option<Uuid>,
    profiles: Vec<ParsedProfile>,
}

/// Lowercases object keys (System.Text.Json `PropertyNameCaseInsensitive`; the last duplicate wins).
fn lower_keys(map: &Map<String, Value>) -> std::collections::HashMap<String, &Value> {
    map.iter().map(|(k, v)| (k.to_lowercase(), v)).collect()
}

fn opt_string(map: &std::collections::HashMap<String, &Value>, key: &str, default: &str) -> Result<String, ()> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(default.to_owned()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(()),
    }
}

/// System.Text.Json `Guid`: only the 36-character `D` form (`8-4-4-4-12` hex digits, either case), no whitespace,
/// braces or other forms (verified against the C# reader by `tests/csharp_behavior.rs`).
fn parse_dotnet_guid(s: &str) -> Option<Uuid> {
    let b = s.as_bytes();
    let shape_ok = b.len() == 36
        && b.iter().enumerate().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { *c == b'-' } else { c.is_ascii_hexdigit() });
    if shape_ok { Uuid::parse_str(s).ok() } else { None }
}

/// `Guid?` (`activeProviderId`): `null`/absent → None.
fn opt_uuid(v: Option<&&Value>) -> Result<Option<Uuid>, ()> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => parse_dotnet_guid(s).map(Some).ok_or(()),
        Some(_) => Err(()),
    }
}

/// What the C# reader makes of the file.
enum WindowsFile {
    /// JSON `null`: `Deserialize` returns null and the C# loader keeps its current state.
    Null,
    Doc(ParsedDoc),
}

/// C# reader rules (win-providers §2): case-insensitive keys (the last duplicate wins), unknown keys ignored, a
/// missing id becomes a fresh UUID, `null` = the default for the reference-type fields (strings, the list; the C# app
/// would crash on them later) but a type error for the value types (`id`, `asyncTranslation`), as is any other type
/// error or a nesting deeper than System.Text.Json's 64 levels: they fail the whole document (C# `JsonException`).
fn parse_windows_file(text: &str) -> Result<WindowsFile, ()> {
    if !crate::json_depth_within(text, crate::DOTNET_MAX_JSON_DEPTH) {
        return Err(());
    }
    let root = match serde_json::from_str::<Value>(text).map_err(|_| ())? {
        Value::Object(o) => o,
        Value::Null => return Ok(WindowsFile::Null),
        _ => return Err(()),
    };
    let root = lower_keys(&root);
    let active = opt_uuid(root.get("activeproviderid"))?;
    let mut profiles = Vec::new();
    match root.get("providers") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for item in items {
                match item {
                    Value::Null => {}
                    Value::Object(o) => profiles.push(parse_windows_profile(o)?),
                    _ => return Err(()),
                }
            }
        }
        Some(_) => return Err(()),
    }
    Ok(WindowsFile::Doc(ParsedDoc { active, profiles }))
}

fn parse_windows_profile(obj: &Map<String, Value>) -> Result<ParsedProfile, ()> {
    let m = lower_keys(obj);
    let id = match m.get("id") {
        None => Uuid::new_v4(),
        Some(Value::String(s)) => parse_dotnet_guid(s).ok_or(())?,
        Some(_) => return Err(()),
    };
    let async_translation = match m.get("asynctranslation") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(()),
    };
    let discovered_models = match m.get("discoveredmodels") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut out = Vec::new();
            for i in items {
                match i {
                    Value::String(s) => out.push(s.clone()),
                    Value::Null => {}
                    _ => return Err(()),
                }
            }
            out
        }
        Some(_) => return Err(()),
    };
    Ok(ParsedProfile {
        profile: TranslationProviderProfile {
            id,
            name: opt_string(&m, "name", "")?,
            preset_id: opt_string(&m, "presetid", TranslationProviderPreset::CUSTOM_ID)?,
            base_url: opt_string(&m, "baseurl", "")?,
            model: opt_string(&m, "model", "")?,
            async_translation,
            discovered_models,
        },
        plaintext_key: opt_string(&m, "apikey", "")?,
    })
}

/// Legacy LAN settings of the Windows Settings window (`externalLlmEnabled`, `externalLlmUrl`, `externalLlmModel`,
/// `externalLlmAsync` in `settings.json`) plus the in-memory discovered list (`ExternalLlmModels`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyLanSettings {
    pub enabled: bool,
    pub url: String,
    pub model: String,
    pub async_translation: bool,
    pub models: Vec<String>,
}

/// The provider-related view-model state `apply_to_view_model` writes (win-providers §3.8). The view model persists
/// `lan` back to its settings keys; `active_provider_*` drive the toolbar label (`TranslationModelName`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderViewState {
    pub lan: LegacyLanSettings,
    /// Blank → None, else trimmed (C# setter rule).
    pub active_provider_name: Option<String>,
    pub active_provider_model: Option<String>,
    pub active_provider_async: bool,
}

fn non_blank_trimmed(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

fn is_lan_id(id: &str) -> bool {
    id.eq_ignore_ascii_case(TranslationProviderPreset::LAN_ID)
}

/// RAII counter for `is_testing`.
struct TestingGuard<'a>(&'a AtomicUsize);

impl Drop for TestingGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The Windows provider manager (C#-compatible `translation-providers.json`).
pub struct WinProviderManager {
    file: PathBuf,
    /// Full path text: broker scope.
    scope: String,
    instance_id: String,
    secrets: Arc<dyn SecretStore>,
    bus: Option<Arc<dyn SyncBus>>,
    launch: Arc<EngineLaunchState>,
    state: Mutex<WinState>,
    subscribers: Subscribers<ProviderChange>,
    testing: AtomicUsize,
    bus_subscription: Mutex<Option<Subscription>>,
}

impl WinProviderManager {
    /// Loads (migrating plaintext keys), syncs the engine launch state, subscribes to `providers`.
    pub fn new(file: PathBuf, secrets: Arc<dyn SecretStore>, bus: Option<Arc<dyn SyncBus>>) -> Arc<Self> {
        Self::with_launch_state(file, secrets, bus, EngineLaunchState::shared().clone())
    }

    /// Production entry point: `<app_data_dir>\translation-providers.json`, keys in the DPAPI-protected sidecar of the
    /// same folder, orphan key blobs (providers the C# app deleted) pruned once.
    pub fn open(app_data_dir: &Path, bus: Option<Arc<dyn SyncBus>>) -> Arc<Self> {
        let secrets = EncryptedSecretStore::for_app_data(app_data_dir);
        let manager = Self::new(app_data_dir.join(FILE_NAME), secrets.clone(), bus);
        manager.prune_orphan_keys(&secrets);
        manager
    }

    /// Like [`new`](Self::new) with an explicit launch state (tests; the app uses the shared one).
    pub fn with_launch_state(
        file: PathBuf,
        secrets: Arc<dyn SecretStore>,
        bus: Option<Arc<dyn SyncBus>>,
        launch: Arc<EngineLaunchState>,
    ) -> Arc<Self> {
        let file = std::path::absolute(&file).unwrap_or(file);
        let scope = file.to_string_lossy().into_owned();
        let manager = Arc::new(Self {
            file,
            scope,
            instance_id: Uuid::new_v4().simple().to_string(),
            secrets,
            bus,
            launch,
            state: Mutex::new(WinState::default()),
            subscribers: Subscribers::new(),
            testing: AtomicUsize::new(0),
            bus_subscription: Mutex::new(None),
        });
        {
            let mut st = manager.state.lock();
            manager.load_locked(&mut st);
        }
        manager.sync_to_engine_state();
        if let Some(bus) = &manager.bus {
            let weak = Arc::downgrade(&manager);
            let sub = bus.subscribe(
                TopicFilter::Only(Topic::Providers),
                Box::new(move |event: &StateSyncEvent| {
                    if let Some(m) = weak.upgrade() {
                        m.on_sync_event(event);
                    }
                }),
            );
            *manager.bus_subscription.lock() = Some(sub);
        }
        manager
    }

    /// 32 hex; broker sender id and `x-opencode-session` value.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Full path of the providers file (broker scope).
    pub fn file_path(&self) -> &Path {
        &self.file
    }

    /// Removes sidecar blobs of providers that no longer exist (the C# app may delete providers behind our back).
    /// Skipped while the providers file could not be loaded (unreadable or damaged): the empty in-memory list says
    /// nothing about which providers exist, and pruning then would destroy every key.
    pub fn prune_orphan_keys(&self, store: &EncryptedSecretStore) {
        let ids: Vec<Uuid> = {
            let st = self.state.lock();
            if st.problem.is_some() {
                return;
            }
            st.entries.iter().map(|e| e.profile.id).collect()
        };
        store.prune_provider_keys(&ids);
    }

    /// `IsExternalProviderActive`: true while a provider is active, even with a blank address.
    pub fn is_external_provider_active(&self) -> bool {
        self.state.lock().active_entry().is_some()
    }

    /// A connection test is running (the picker refuses to start another refresh).
    pub fn is_testing(&self) -> bool {
        self.testing.load(Ordering::SeqCst) > 0
    }

    // MARK: loading and saving

    fn read_key_slot(&self, id: Uuid) -> KeySlot {
        match self.secrets.read(&TranslationProviderProfile::secret_account(id)) {
            SecretRead::Found(b) => KeySlot::Found(SecretString::new(String::from_utf8_lossy(b.expose()).into_owned())),
            SecretRead::Absent => KeySlot::Absent,
            SecretRead::Unavailable => KeySlot::Unavailable,
        }
    }

    /// Entries of a parsed document; plaintext keys are moved into the sidecar. Returns whether the file must be
    /// rewritten (a plaintext key was migrated).
    fn entries_from(&self, doc: ParsedDoc) -> (Vec<Entry>, bool) {
        let mut needs_rewrite = false;
        let entries = doc
            .profiles
            .into_iter()
            .map(|p| {
                let plain = zeroize::Zeroizing::new(p.plaintext_key);
                let trimmed = plain.trim();
                let id = p.profile.id;
                if trimmed.is_empty() {
                    Entry { key: self.read_key_slot(id), profile: p.profile, plaintext_pending: false }
                } else if self.secrets.set(trimmed.as_bytes(), &TranslationProviderProfile::secret_account(id)) {
                    needs_rewrite = true;
                    Entry { key: KeySlot::Found(SecretString::new(trimmed)), profile: p.profile, plaintext_pending: false }
                } else {
                    Entry { key: KeySlot::Found(SecretString::new(trimmed)), profile: p.profile, plaintext_pending: true }
                }
            })
            .collect();
        (entries, needs_rewrite)
    }

    /// Reads the file into `st` (C# `LoadFromDisk`, at startup on an empty state and again for every broker event).
    /// A document replaces the state; a parse error empties it (`Damaged`: the file stays untouched until the first
    /// save sets it aside). A missing file, one that cannot be read, or JSON `null` keep the current state, as in C#
    /// (`ReadAllTextShared` returned null / `Deserialize` returned null); an unreadable one is marked `Unreadable` so
    /// the next save re-reads and merges instead of overwriting it.
    fn load_locked(&self, st: &mut WinState) {
        let doc = match fileio::read_file(&self.file) {
            ReadOutcome::Missing => return,
            ReadOutcome::Unreadable => {
                st.problem = Some(ProviderLoadProblem::Unreadable);
                return;
            }
            ReadOutcome::Content(bytes) => match parse_windows_file(&fileio::decode_text(&bytes)) {
                Ok(WindowsFile::Null) => return,
                Ok(WindowsFile::Doc(doc)) => doc,
                Err(()) => {
                    *st = WinState { problem: Some(ProviderLoadProblem::Damaged), ..WinState::default() };
                    return;
                }
            },
        };
        let active = doc.active;
        let (entries, needs_rewrite) = self.entries_from(doc);
        *st = WinState::default();
        st.entries = entries;
        st.active = active.filter(|id| st.find(*id).is_some());
        if needs_rewrite {
            self.save_locked(st);
        }
    }

    /// Whether the file may be replaced now (Unreadable → re-read and merge; Damaged → aside copy first).
    fn prepare_save_locked(&self, st: &mut WinState) -> bool {
        if st.problem == Some(ProviderLoadProblem::Unreadable) {
            match fileio::read_file(&self.file) {
                ReadOutcome::Unreadable => return false,
                ReadOutcome::Missing => st.problem = None,
                ReadOutcome::Content(bytes) => match parse_windows_file(&fileio::decode_text(&bytes)) {
                    Err(()) => st.problem = Some(ProviderLoadProblem::Damaged),
                    Ok(WindowsFile::Null) => st.problem = None,
                    Ok(WindowsFile::Doc(doc)) => {
                        let disk_active = doc.active;
                        let (disk, _) = self.entries_from(doc);
                        let mut merged: Vec<Entry> =
                            disk.into_iter().filter(|d| st.find(d.profile.id).is_none()).collect();
                        merged.append(&mut st.entries);
                        st.entries = merged;
                        if st.active.is_none() {
                            st.active = disk_active.filter(|id| st.find(*id).is_some());
                        }
                        st.problem = None;
                    }
                },
            }
        }
        if st.problem == Some(ProviderLoadProblem::Damaged) {
            if fileio::preserve_damaged(&self.file, chrono::Utc::now()).is_err() {
                return false;
            }
            st.problem = None;
        }
        true
    }

    /// The C# document: camelCase keys in C# order, `"apiKey": ""` (or a still-pending C# plaintext key).
    fn document(st: &WinState) -> Value {
        let providers: Vec<Value> = st
            .entries
            .iter()
            .map(|e| {
                let p = &e.profile;
                let mut o = Map::new();
                o.insert("id".into(), Value::String(p.id.hyphenated().to_string()));
                o.insert("name".into(), Value::String(p.name.clone()));
                o.insert("presetId".into(), Value::String(p.preset_id.clone()));
                o.insert("baseUrl".into(), Value::String(p.base_url.clone()));
                let api_key = match (&e.key, e.plaintext_pending) {
                    (KeySlot::Found(k), true) => k.expose().to_owned(),
                    _ => String::new(),
                };
                o.insert("apiKey".into(), Value::String(api_key));
                o.insert("model".into(), Value::String(p.model.clone()));
                o.insert("asyncTranslation".into(), Value::Bool(p.async_translation));
                o.insert("discoveredModels".into(), Value::Array(p.discovered_models.iter().cloned().map(Value::String).collect()));
                Value::Object(o)
            })
            .collect();
        let mut root = Map::new();
        root.insert(
            "activeProviderId".into(),
            st.active.map(|id| Value::String(id.hyphenated().to_string())).unwrap_or(Value::Null),
        );
        root.insert("providers".into(), Value::Array(providers));
        Value::Object(root)
    }

    /// Writes the file (inside the state lock). I/O failures leave the state in memory for the session.
    fn save_locked(&self, st: &mut WinState) {
        if !self.prepare_save_locked(st) {
            return;
        }
        for e in st.entries.iter_mut().filter(|e| e.plaintext_pending) {
            if let KeySlot::Found(k) = &e.key
                && self.secrets.set(k.expose().as_bytes(), &TranslationProviderProfile::secret_account(e.profile.id))
            {
                e.plaintext_pending = false;
            }
        }
        let text = zeroize::Zeroizing::new(json::pretty(&Self::document(st), Style::CSharp));
        let _ = fileio::write_atomic(&self.file, text.as_bytes());
    }

    /// Stores (or removes, when blank) a key in the sidecar first, then in memory. False when the store refused it
    /// (the key is then kept in memory for this session only).
    fn store_key_locked(&self, entry: &mut Entry, key: &str) -> bool {
        let account = TranslationProviderProfile::secret_account(entry.profile.id);
        let k = key.trim();
        entry.plaintext_pending = false;
        if k.is_empty() {
            self.secrets.remove(&account);
            entry.key = KeySlot::Absent;
            true
        } else {
            let ok = self.secrets.set(k.as_bytes(), &account);
            entry.key = KeySlot::Found(SecretString::new(k));
            ok
        }
    }

    /// Runs a mutation; `Some(change)` saves, syncs the engine state, publishes and notifies.
    fn mutate<R>(&self, f: impl FnOnce(&mut WinState) -> (R, Option<ProviderChange>)) -> R {
        self.mutate_saving(|st| {
            let (r, change) = f(st);
            (r, change, change.is_some())
        })
    }

    /// [`mutate`](Self::mutate) where the closure also says whether to save (C# notifies without a save in one
    /// branch of `SyncFromLegacyLanSettings`).
    fn mutate_saving<R>(&self, f: impl FnOnce(&mut WinState) -> (R, Option<ProviderChange>, bool)) -> R {
        let (r, change) = {
            let mut st = self.state.lock();
            let (r, change, save) = f(&mut st);
            if save {
                self.save_locked(&mut st);
            }
            (r, change)
        };
        if let Some(change) = change {
            self.sync_to_engine_state();
            self.publish();
            self.subscribers.notify(&change);
        }
        r
    }

    fn publish(&self) {
        if let Some(bus) = &self.bus {
            bus.publish(
                Topic::Providers,
                StateSyncEvent::new(Topic::Providers, EventKind::ProvidersUpdated, &self.instance_id, &self.scope),
            );
        }
    }

    fn on_sync_event(&self, event: &StateSyncEvent) {
        if event.sender_instance_id == self.instance_id || event.scope_path.to_lowercase() != self.scope.to_lowercase() {
            return;
        }
        self.reload();
    }

    /// Re-reads the file and the key sidecar (another instance changed them), syncs the engine state and notifies
    /// `External` without republishing.
    pub fn reload(&self) {
        {
            let mut st = self.state.lock();
            self.load_locked(&mut st);
        }
        self.sync_to_engine_state();
        self.subscribers.notify(&ProviderChange::External);
    }

    /// `SyncToEngineState`: the active provider with a non-blank address, else no external LLM.
    pub fn sync_to_engine_state(&self) {
        let llm = {
            let st = self.state.lock();
            st.active_entry().filter(|e| !e.profile.base_url.trim().is_empty()).map(|e| ExternalLlm {
                url: e.profile.base_url.trim().to_owned(),
                model: e.profile.model.trim().to_owned(),
                api_key: e.key.engine_key(),
            })
        };
        self.launch.set_external_llm(llm);
    }

    // MARK: C#-specific editing

    /// `UpdateProviderDiscoveredModels`: the list is stored verbatim; the model becomes `selected_model` (trimmed)
    /// if given, else stays, else the first discovered one.
    pub fn update_provider_discovered_models(&self, id: Uuid, discovered: &[String], selected_model: Option<&str>) {
        self.mutate(|st| {
            let Some(e) = st.find_mut(id) else { return ((), None) };
            let next_model = match selected_model.map(str::trim).filter(|m| !m.is_empty()) {
                Some(m) => m.to_owned(),
                None if !e.profile.model.trim().is_empty() => e.profile.model.clone(),
                None => discovered.first().cloned().unwrap_or_default(),
            };
            e.profile.discovered_models = discovered.to_vec();
            e.profile.model = next_model;
            ((), Some(ProviderChange::Providers))
        })
    }

    /// The LAN section's discovered models go to the `lan` profile only (UNIFIED.md §3.7 FIX-W; C# wrote them into
    /// whichever provider was active).
    pub fn update_lan_discovered_models(&self, discovered: &[String], selected_model: Option<&str>) {
        let lan = self.state.lock().entries.iter().find(|e| is_lan_id(&e.profile.preset_id)).map(|e| e.profile.id);
        if let Some(id) = lan {
            self.update_provider_discovered_models(id, discovered, selected_model);
        }
    }

    // MARK: legacy LAN bridge (win-providers §3.8)

    fn new_lan_entry(lan: &LegacyLanSettings, url: &str, model: &str) -> Entry {
        Entry {
            profile: TranslationProviderProfile {
                id: Uuid::new_v4(),
                name: TranslationProviderPreset::preset(Some(TranslationProviderPreset::LAN_ID)).name(None),
                preset_id: TranslationProviderPreset::LAN_ID.to_owned(),
                base_url: url.to_owned(),
                model: model.to_owned(),
                async_translation: lan.async_translation,
                discovered_models: lan.models.clone(),
            },
            key: KeySlot::Absent,
            plaintext_pending: false,
        }
    }

    /// `EnsureMigratedFromViewModel` (once at startup): keeps exactly one `lan` profile in step with the legacy LAN
    /// settings (a cloud URL never overwrites it), then applies the active provider to `view` if there is one.
    pub fn ensure_migrated_from_legacy(&self, view: &mut ProviderViewState) {
        let url = view.lan.url.trim().to_owned();
        let model = view.lan.model.trim().to_owned();
        let enabled = view.lan.enabled;
        if !url.is_empty() && !is_cloud_provider_url(&url) {
            let lan_settings = view.lan.clone();
            self.mutate(|st| {
                let mut changed = false;
                match st.entries.iter().position(|e| is_lan_id(&e.profile.preset_id)) {
                    None if st.entries.is_empty() || enabled => {
                        let entry = Self::new_lan_entry(&lan_settings, &url, &model);
                        let id = entry.profile.id;
                        st.entries.push(entry);
                        if enabled && st.active.is_none() {
                            st.active = Some(id);
                        }
                        changed = true;
                    }
                    None => {}
                    Some(i) => {
                        let lan = &mut st.entries[i].profile;
                        let mut next = lan.clone();
                        next.base_url = url.clone();
                        if !model.is_empty() {
                            next.model = model.clone();
                        }
                        next.async_translation = lan_settings.async_translation;
                        if !lan_settings.models.is_empty() {
                            next.discovered_models = lan_settings.models.clone();
                        }
                        if next != *lan {
                            *lan = next;
                            changed = true;
                        }
                        let id = lan.id;
                        if enabled && st.active.is_none() {
                            st.active = Some(id);
                            changed = true;
                        }
                    }
                }
                ((), changed.then_some(ProviderChange::Providers))
            });
        }
        if self.is_external_provider_active() {
            self.apply_to_view_state(view);
        }
    }

    /// `SyncFromLegacyLanSettings`: after every edit in the Settings LAN section.
    pub fn sync_from_legacy_lan_settings(&self, lan: &LegacyLanSettings) {
        let url = lan.url.trim().to_owned();
        let model = lan.model.trim().to_owned();
        if !url.is_empty() && !is_cloud_provider_url(&url) {
            self.mutate_saving(|st| {
                let active_is_cloud = st.active_entry().is_some_and(|e| !is_lan_id(&e.profile.preset_id));
                let saved = match st.entries.iter().position(|e| is_lan_id(&e.profile.preset_id)) {
                    Some(i) => {
                        let p = &mut st.entries[i].profile;
                        p.base_url = url.clone();
                        if !model.is_empty() {
                            p.model = model.clone();
                        }
                        p.async_translation = lan.async_translation;
                        if !lan.models.is_empty() {
                            p.discovered_models = lan.models.clone();
                        }
                        let id = p.id;
                        if lan.enabled && !active_is_cloud {
                            st.active = Some(id);
                        } else if !lan.enabled && st.active == Some(id) {
                            st.active = None;
                        }
                        true
                    }
                    None if lan.enabled => {
                        let entry = Self::new_lan_entry(lan, &url, &model);
                        let id = entry.profile.id;
                        st.entries.push(entry);
                        if !active_is_cloud {
                            st.active = Some(id);
                        }
                        true
                    }
                    None => false,
                };
                // C#: synced and notified (and published) unconditionally in this branch; saved only when a `lan`
                // profile was updated or created.
                ((), Some(ProviderChange::Providers), saved)
            });
        } else if !lan.enabled && self.active_provider().is_some_and(|p| is_lan_id(&p.preset_id)) {
            self.set_active(None);
        }
    }

    /// `ApplyToViewModel`: mirrors the active provider into the view state (a LAN provider into the legacy LAN fields,
    /// a cloud one into `active_provider_*`) and the engine launch state.
    pub fn apply_to_view_state(&self, view: &mut ProviderViewState) {
        self.sync_to_engine_state();
        let active = {
            let st = self.state.lock();
            st.active_entry().filter(|e| !e.profile.base_url.trim().is_empty()).cloned()
        };
        match active {
            Some(e) => {
                let p = &e.profile;
                if is_lan_id(&p.preset_id) {
                    view.active_provider_name = None;
                    view.active_provider_model = None;
                    view.active_provider_async = false;
                    if !is_cloud_provider_url(&p.base_url) {
                        view.lan.url = p.base_url.trim().to_owned();
                    }
                    if !p.model.trim().is_empty() {
                        view.lan.model = p.model.trim().to_owned();
                    }
                    view.lan.async_translation = p.async_translation;
                    view.lan.enabled = true;
                } else {
                    view.active_provider_name = non_blank_trimmed(&p.name);
                    view.active_provider_model = non_blank_trimmed(&p.model);
                    view.active_provider_async = p.async_translation;
                }
                self.launch.set_external_llm(Some(ExternalLlm {
                    url: p.base_url.trim().to_owned(),
                    model: p.model.trim().to_owned(),
                    api_key: e.key.engine_key(),
                }));
            }
            None => {
                view.active_provider_name = None;
                view.active_provider_model = None;
                view.active_provider_async = false;
                view.lan.enabled = false;
                self.launch.set_external_llm(None);
            }
        }
    }

    // MARK: toolbar picker (win-providers §7)

    /// `SelectProvider`: an unknown `Some(id)` is refused (a stale picker must not switch to on-device).
    pub fn select_provider(&self, view: &mut ProviderViewState, provider_id: Option<Uuid>) -> bool {
        if !crate::selection::can_select_provider(self, provider_id) {
            return false;
        }
        self.set_active(provider_id);
        self.apply_to_view_state(view);
        true
    }

    /// `SelectModel`: `model_id` must be one of the provider's picker models (exact match) or, on-device, one of
    /// `on_device_ids` (the catalog's translation models). On-device returns true after deactivating the provider;
    /// the caller then sets `translationModel`.
    pub fn select_model(
        &self,
        view: &mut ProviderViewState,
        provider_id: Option<Uuid>,
        model_id: &str,
        on_device_ids: &[&str],
    ) -> bool {
        match provider_id {
            Some(id) => {
                let Some(p) = self.providers().into_iter().find(|p| p.id == id) else { return false };
                if !crate::selection::provider_models_for(&p, false).iter().any(|c| c.id == model_id) {
                    return false;
                }
                self.update_model(id, model_id);
                self.select_provider(view, Some(id))
            }
            None => {
                if !on_device_ids.contains(&model_id) {
                    return false;
                }
                self.select_provider(view, None)
            }
        }
    }

    // MARK: connection tests (win-providers §5)

    /// `TestAndFetchModelsAsync(id)`: tests the saved provider; on success with models the list (merged, including
    /// suggestions) is stored and an empty model takes the first. The active provider is never changed; a failure
    /// persists nothing. Blocking: call it on a background lane.
    pub fn test_and_fetch_models(
        &self,
        id: Uuid,
        fetch: &dyn HttpFetch,
        cancel: &CancellationToken,
    ) -> Result<ProviderTestOutcome, TestCancelled> {
        let snapshot = {
            let st = self.state.lock();
            st.find(id).map(|e| (e.profile.clone(), SecretString::new(e.key.engine_key())))
        };
        let Some((profile, key)) = snapshot else {
            return Ok(ProviderTestOutcome {
                success: false,
                message: sa_core::l("Sağlayıcı bulunamadı."),
                models: Vec::new(),
            });
        };
        let outcome = self.test_draft(&profile.preset_id, &profile.base_url, key.expose(), Some(&profile.model), fetch, cancel)?;
        if outcome.success && !outcome.models.is_empty() {
            let models = outcome.models.clone();
            self.mutate(|st| {
                let Some(e) = st.find_mut(id) else { return ((), None) };
                if e.profile.model.trim().is_empty() {
                    e.profile.model = models[0].clone();
                }
                e.profile.discovered_models = models;
                ((), Some(ProviderChange::Providers))
            });
        }
        Ok(outcome)
    }

    /// `TestAndFetchModelsAsync(preset, url, key, model)`: the Settings form's test of unsaved credentials.
    pub fn test_draft(
        &self,
        preset_id: &str,
        base_url: &str,
        api_key: &str,
        model: Option<&str>,
        fetch: &dyn HttpFetch,
        cancel: &CancellationToken,
    ) -> Result<ProviderTestOutcome, TestCancelled> {
        self.testing.fetch_add(1, Ordering::SeqCst);
        let _guard = TestingGuard(&self.testing);
        connection::test_provider(preset_id, base_url, api_key, model, &self.instance_id, fetch, cancel)
    }

    /// The masked key of a provider for its card: `Some(mask)`, `None` for no key ("Anahtar yok"); an unreadable
    /// sidecar entry is `Err(())` (the card says the key could not be read).
    pub fn masked_key(&self, id: Uuid) -> Result<Option<String>, ()> {
        match self.state.lock().find(id).map(|e| e.key.clone()) {
            Some(KeySlot::Found(k)) => Ok(crate::mask_key(k.expose())),
            Some(KeySlot::Unavailable) => Err(()),
            Some(KeySlot::Absent) | None => Ok(None),
        }
    }
}

impl Drop for WinProviderManager {
    fn drop(&mut self) {
        self.bus_subscription.get_mut().take();
    }
}

impl ProviderStore for WinProviderManager {
    fn providers(&self) -> Vec<TranslationProviderProfile> {
        self.state.lock().entries.iter().map(|e| e.profile.clone()).collect()
    }

    fn active_provider_id(&self) -> Option<Uuid> {
        self.state.lock().active
    }

    fn load_problem(&self) -> Option<ProviderLoadProblem> {
        self.state.lock().problem
    }

    /// `AddProvider`: preset canonicalized (unknown → custom), blank name → preset name in the current language,
    /// blank URL → preset URL, blank model → first suggestion; an existing id is replaced in place.
    fn add(&self, profile: TranslationProviderProfile, key: Option<&str>, set_active: bool) -> TranslationProviderProfile {
        self.mutate(|st| {
            // C# `GetPreset(presetId)` matches the id as given (no trim; `" openai"` is custom).
            let preset = TranslationProviderPreset::preset(Some(&profile.preset_id));
            let stored = TranslationProviderProfile {
                id: profile.id,
                name: non_blank_trimmed(&profile.name).unwrap_or_else(|| preset.name(None)),
                preset_id: preset.id.to_owned(),
                base_url: non_blank_trimmed(&profile.base_url).unwrap_or_else(|| preset.base_url.to_owned()),
                model: non_blank_trimmed(&profile.model).unwrap_or_else(|| preset.default_model().to_owned()),
                async_translation: profile.async_translation,
                discovered_models: profile.discovered_models.clone(),
            };
            let index = match st.entries.iter().position(|e| e.profile.id == stored.id) {
                Some(i) => {
                    st.entries[i].profile = stored.clone();
                    i
                }
                None => {
                    st.entries.push(Entry { profile: stored.clone(), key: KeySlot::Absent, plaintext_pending: false });
                    st.entries.len() - 1
                }
            };
            if let Some(k) = key {
                let mut e = st.entries[index].clone();
                self.store_key_locked(&mut e, k);
                st.entries[index] = e;
            }
            if set_active {
                st.active = Some(stored.id);
            }
            (stored, Some(ProviderChange::Providers))
        })
    }

    /// `UpdateProvider`: name (blank → preset name), URL, model (trimmed, blank allowed) and async; preset, id and
    /// discovered models stay.
    fn update(&self, profile: TranslationProviderProfile, key: Option<&str>) -> bool {
        self.mutate(|st| {
            let Some(e) = st.find(profile.id).cloned() else { return (false, None) };
            let mut e = e;
            e.profile.name = non_blank_trimmed(&profile.name)
                .unwrap_or_else(|| TranslationProviderPreset::preset(Some(&e.profile.preset_id)).name(None));
            e.profile.base_url = profile.base_url.trim().to_owned();
            e.profile.model = profile.model.trim().to_owned();
            e.profile.async_translation = profile.async_translation;
            if let Some(k) = key {
                self.store_key_locked(&mut e, k);
            }
            if let Some(slot) = st.find_mut(profile.id) {
                *slot = e;
            }
            (true, Some(ProviderChange::Providers))
        })
    }

    fn remove(&self, id: Uuid) {
        self.secrets.remove(&TranslationProviderProfile::secret_account(id));
        self.mutate(|st| {
            let before = st.entries.len();
            st.entries.retain(|e| e.profile.id != id);
            if st.entries.len() == before {
                return ((), None);
            }
            if st.active == Some(id) {
                st.active = None;
            }
            ((), Some(ProviderChange::Providers))
        })
    }

    /// An unknown id deactivates external translation (callers that must not do that check first). The engine state
    /// is always synced; listeners hear only a real change.
    fn set_active(&self, id: Option<Uuid>) {
        self.mutate(|st| {
            let resolved = id.filter(|id| st.find(*id).is_some());
            if st.active == resolved {
                return ((), None);
            }
            st.active = resolved;
            ((), Some(ProviderChange::Active))
        });
        self.sync_to_engine_state();
    }

    fn update_model(&self, id: Uuid, model: &str) {
        self.mutate(|st| {
            let next = model.trim();
            let Some(e) = st.find_mut(id) else { return ((), None) };
            if e.profile.model == next {
                return ((), None);
            }
            e.profile.model = next.to_owned();
            ((), Some(ProviderChange::Providers))
        })
    }

    fn set_discovered_models(&self, id: Uuid, models: &[String]) {
        self.update_provider_discovered_models(id, models, None);
    }

    fn set_key(&self, id: Uuid, key: &str) -> bool {
        self.mutate(|st| {
            let Some(e) = st.find(id).cloned() else { return (false, None) };
            let mut e = e;
            let ok = self.store_key_locked(&mut e, key);
            if let Some(slot) = st.find_mut(id) {
                *slot = e;
            }
            (ok, Some(ProviderChange::Keys))
        })
    }

    fn read_key(&self, id: Uuid) -> ProviderKeyRead {
        match self.state.lock().find(id).map(|e| e.key.clone()) {
            Some(KeySlot::Found(k)) => ProviderKeyRead::Found(k),
            Some(KeySlot::Unavailable) => ProviderKeyRead::Unavailable,
            Some(KeySlot::Absent) | None => ProviderKeyRead::Absent,
        }
    }

    fn active_external_llm(&self) -> Option<ActiveExternalLlm> {
        let st = self.state.lock();
        let e = st.active_entry().filter(|e| !e.profile.base_url.trim().is_empty())?;
        Some(ActiveExternalLlm {
            llm: ExternalLlm {
                url: e.profile.base_url.trim().to_owned(),
                model: e.profile.model.trim().to_owned(),
                api_key: e.key.engine_key(),
            },
            async_translation: e.profile.async_translation,
        })
    }

    fn subscribe(&self, handler: Box<dyn Fn(ProviderChange) + Send + Sync>) -> Subscription {
        self.subscribers.subscribe(move |c: &ProviderChange| handler(*c))
    }
}

#[cfg(test)]
mod tests;
