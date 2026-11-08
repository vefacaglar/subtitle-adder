//! macOS `TranslationProviderStore` (mac-librarykit §14): sorted keys, uppercase UUIDs, keys never in the file,
//! legacy `externalLlm*` defaults migrated once (`translationProvidersMigrated`).
//!
//! The store is portable Rust (only the file mode is unix-specific), so it is also compiled for this crate's tests on
//! Windows. Keys live in the [`SecretStore`] under `translation-provider.<UUID>`; in the app that is the
//! [`EncryptedSecretStore`](crate::secrets::EncryptedSecretStore) over the 0600 key file (no Keychain). The Swift app
//! kept keys only in memory, so nothing is migrated: users re-enter each key once.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use sa_core::secrets::{SecretRead, SecretStore, SecretString};
use sa_core::settings::{SettingsStore, keys};
use sa_core::subscription::{Subscribers, Subscription};
use sa_enginekit::launch::ExternalLlm;
use sa_enginekit::presets::TranslationProviderPreset;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::fileio::{self, ReadOutcome};
use crate::json::{self, Style};
use crate::{ActiveExternalLlm, ProviderChange, ProviderKeyRead, ProviderLoadProblem, ProviderStore, TranslationProviderProfile};

/// `translation-providers.json`.
pub const FILE_NAME: &str = "translation-providers.json";
/// UserDefaults flag set once the legacy single-server settings were looked at.
pub const MIGRATION_DEFAULTS_KEY: &str = keys::TRANSLATION_PROVIDERS_MIGRATED;

enum FileRead {
    Missing,
    Unreadable,
    Damaged,
    Loaded { active: Option<Uuid>, providers: Vec<TranslationProviderProfile>, skipped: usize },
}

/// Swift `UUID(uuidString:)`: only the 36-character hyphenated form, any case.
fn parse_swift_uuid(s: &str) -> Option<Uuid> {
    if s.len() != 36 {
        return None;
    }
    Uuid::parse_str(s).ok()
}

fn uuid_upper(id: Uuid) -> String {
    id.hyphenated().to_string().to_uppercase()
}

/// One element of `providers`; `Err` = skipped (Swift `LossyProfile`).
fn decode_profile(v: &Value) -> Result<TranslationProviderProfile, ()> {
    let Value::Object(o) = v else { return Err(()) };
    fn string(o: &Map<String, Value>, key: &str, default: &str) -> Result<String, ()> {
        match o.get(key) {
            None | Some(Value::Null) => Ok(default.to_owned()),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(()),
        }
    }
    let id = match o.get("id") {
        None | Some(Value::Null) => Uuid::new_v4(),
        Some(Value::String(s)) => parse_swift_uuid(s).ok_or(())?,
        Some(_) => return Err(()),
    };
    let async_translation = match o.get("asyncTranslation") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(()),
    };
    let discovered_models = match o.get("discoveredModels") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(|i| i.as_str().map(str::to_owned).ok_or(())).collect::<Result<_, _>>()?,
        Some(_) => return Err(()),
    };
    Ok(TranslationProviderProfile {
        id,
        name: string(o, "name", "")?,
        preset_id: string(o, "presetId", TranslationProviderPreset::CUSTOM_ID)?,
        base_url: string(o, "baseUrl", "")?,
        model: string(o, "model", "")?,
        async_translation,
        discovered_models,
    })
}

fn read_file(path: &std::path::Path) -> FileRead {
    let bytes = match fileio::read_file(path) {
        ReadOutcome::Missing => return FileRead::Missing,
        ReadOutcome::Unreadable => return FileRead::Unreadable,
        ReadOutcome::Content(b) => b,
    };
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&bytes);
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(bytes) else { return FileRead::Damaged };
    let mut skipped = 0;
    let active = match root.get("activeProviderId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => match parse_swift_uuid(s) {
            Some(id) => Some(id),
            None => {
                skipped += 1;
                None
            }
        },
        Some(_) => {
            skipped += 1;
            None
        }
    };
    let providers = match root.get("providers") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let decoded: Vec<_> = items.iter().map(decode_profile).collect();
            skipped += decoded.iter().filter(|r| r.is_err()).count();
            decoded.into_iter().flatten().collect()
        }
        Some(_) => return FileRead::Damaged,
    };
    FileRead::Loaded { active, providers, skipped }
}

#[derive(Default)]
struct MacState {
    providers: Vec<TranslationProviderProfile>,
    active: Option<Uuid>,
    problem: Option<ProviderLoadProblem>,
}

impl MacState {
    fn index(&self, id: Uuid) -> Option<usize> {
        self.providers.iter().position(|p| p.id == id)
    }
}

/// The macOS provider store.
pub struct MacProviderStore {
    file: PathBuf,
    secrets: Arc<dyn SecretStore>,
    state: Mutex<MacState>,
    subscribers: Subscribers<ProviderChange>,
}

impl MacProviderStore {
    /// Loads the file if it exists (loading never writes). A missing one starts empty; an unreadable or damaged one
    /// (or one with skipped entries) is not replaced until it is safe (see `save`).
    pub fn new(file: PathBuf, secrets: Arc<dyn SecretStore>) -> Arc<Self> {
        let mut st = MacState::default();
        match read_file(&file) {
            FileRead::Missing => {}
            FileRead::Unreadable => st.problem = Some(ProviderLoadProblem::Unreadable),
            FileRead::Damaged => st.problem = Some(ProviderLoadProblem::Damaged),
            FileRead::Loaded { active, providers, skipped } => {
                st.providers = providers;
                st.active = active.filter(|id| st.index(*id).is_some());
                if skipped > 0 {
                    st.problem = Some(ProviderLoadProblem::Damaged);
                }
            }
        }
        Arc::new(Self { file, secrets, state: Mutex::new(st), subscribers: Subscribers::new() })
    }

    /// Once: legacy LAN keys → a provider (mac-librarykit §14.9). The flag is set even when nothing is migrated; an
    /// existing providers file is never touched. Returns whether a provider was created.
    pub fn migrate_legacy_if_needed(&self, defaults: &dyn SettingsStore) -> bool {
        if defaults.get_bool(MIGRATION_DEFAULTS_KEY).unwrap_or(false) {
            return false;
        }
        let created = self.migrate(defaults);
        defaults.set_bool(MIGRATION_DEFAULTS_KEY, true);
        created
    }

    fn migrate(&self, defaults: &dyn SettingsStore) -> bool {
        if !self.state.lock().providers.is_empty() || self.file.exists() {
            return false;
        }
        let url = defaults.get_string(keys::EXTERNAL_LLM_URL).unwrap_or_default().trim().to_owned();
        if url.is_empty() {
            return false;
        }
        let lower = url.to_lowercase();
        let address = if lower.starts_with("http://") || lower.starts_with("https://") { url } else { format!("http://{url}") };
        let profile = TranslationProviderProfile {
            id: Uuid::new_v4(),
            name: TranslationProviderPreset::preset(Some(TranslationProviderPreset::LAN_ID)).name(None),
            preset_id: TranslationProviderPreset::LAN_ID.to_owned(),
            base_url: address,
            model: defaults.get_string(keys::EXTERNAL_LLM_MODEL).unwrap_or_default().trim().to_owned(),
            async_translation: defaults.get_bool(keys::EXTERNAL_LLM_ASYNC).unwrap_or(false),
            discovered_models: Vec::new(),
        };
        let enabled = defaults.get_bool(keys::EXTERNAL_LLM_ENABLED).unwrap_or(false);
        self.mutate(|st| {
            st.active = enabled.then_some(profile.id);
            st.providers.push(profile);
            ((), Some(ProviderChange::Providers))
        });
        true
    }

    /// Removes key blobs of providers that are not in the file (mac-librarykit §13.2: harmless and recommended at
    /// launch). Skipped while the file is unreadable or damaged, where the in-memory list proves nothing.
    pub fn prune_orphan_keys(&self, store: &crate::secrets::EncryptedSecretStore) {
        let ids: Vec<Uuid> = {
            let st = self.state.lock();
            if st.problem.is_some() {
                return;
            }
            st.providers.iter().map(|p| p.id).collect()
        };
        store.prune_provider_keys(&ids);
    }

    /// `<folder>/translation-providers.json`.
    pub fn default_file(app_data_dir: &std::path::Path) -> PathBuf {
        app_data_dir.join(FILE_NAME)
    }

    // MARK: keys

    /// The provider's key, empty when none or unreadable (prefer [`ProviderStore::read_key`] in UI paths).
    pub fn api_key(&self, id: Uuid) -> String {
        match self.read_key(id) {
            ProviderKeyRead::Found(k) => k.expose().to_owned(),
            _ => String::new(),
        }
    }

    /// The key as shown in lists; `None` when there is none (the app shows `Anahtar yok`).
    pub fn masked_key(&self, id: Uuid) -> Option<String> {
        crate::mask_key(&self.api_key(id))
    }

    /// `available_models` of the provider with this id (model first); empty for an unknown id.
    pub fn available_models(&self, id: Uuid) -> Vec<String> {
        self.state.lock().providers.iter().find(|p| p.id == id).map(|p| p.available_models(true)).unwrap_or_default()
    }

    /// Trim; blank → remove (true); else `secrets.set`.
    fn store_key(&self, key: &str, id: Uuid) -> bool {
        let account = TranslationProviderProfile::secret_account(id);
        let k = zeroize::Zeroizing::new(key.trim().to_owned());
        if k.is_empty() {
            self.secrets.remove(&account);
            true
        } else {
            self.secrets.set(k.as_bytes(), &account)
        }
    }

    // MARK: storage

    fn normalized(profile: TranslationProviderProfile) -> TranslationProviderProfile {
        let mut p = profile;
        p.preset_id = p.preset_id.trim().to_owned();
        if p.preset_id.is_empty() {
            p.preset_id = TranslationProviderPreset::CUSTOM_ID.to_owned();
        }
        p.name = p.name.trim().to_owned();
        if p.name.is_empty() {
            p.name = p.preset().name(None);
        }
        p.base_url = p.base_url.trim().to_owned();
        p.model = p.model.trim().to_owned();
        p
    }

    /// Whether the file may be replaced now (Swift `prepareFileForSave`).
    fn prepare(&self, st: &mut MacState) -> bool {
        if st.problem == Some(ProviderLoadProblem::Unreadable) {
            match read_file(&self.file) {
                FileRead::Unreadable => return false,
                FileRead::Missing => st.problem = None,
                FileRead::Damaged => st.problem = Some(ProviderLoadProblem::Damaged),
                FileRead::Loaded { active, providers, skipped } => {
                    let mut merged: Vec<_> = providers.into_iter().filter(|d| st.index(d.id).is_none()).collect();
                    merged.append(&mut st.providers);
                    st.providers = merged;
                    if st.active.is_none() {
                        st.active = active.filter(|id| st.index(*id).is_some());
                    }
                    st.problem = (skipped > 0).then_some(ProviderLoadProblem::Damaged);
                }
            }
        }
        if st.problem != Some(ProviderLoadProblem::Damaged) {
            return true;
        }
        match fileio::preserve_damaged(&self.file, chrono::Utc::now()) {
            Ok(_) => {
                st.problem = None;
                true
            }
            Err(_) => false,
        }
    }

    fn document(st: &MacState) -> Value {
        let providers = st
            .providers
            .iter()
            .map(|p| {
                let mut o = Map::new();
                o.insert("asyncTranslation".into(), Value::Bool(p.async_translation));
                o.insert("baseUrl".into(), Value::String(p.base_url.clone()));
                o.insert("discoveredModels".into(), Value::Array(p.discovered_models.iter().cloned().map(Value::String).collect()));
                o.insert("id".into(), Value::String(uuid_upper(p.id)));
                o.insert("model".into(), Value::String(p.model.clone()));
                o.insert("name".into(), Value::String(p.name.clone()));
                o.insert("presetId".into(), Value::String(p.preset_id.clone()));
                Value::Object(o)
            })
            .collect();
        let mut root = Map::new();
        root.insert("activeProviderId".into(), st.active.map(|id| Value::String(uuid_upper(id))).unwrap_or(Value::Null));
        root.insert("providers".into(), Value::Array(providers));
        Value::Object(root)
    }

    /// Atomic write; failures keep the state in memory for this session.
    fn save(&self, st: &mut MacState) {
        if !self.prepare(st) {
            return;
        }
        let text = json::pretty(&Self::document(st), Style::Apple);
        let _ = fileio::write_atomic(&self.file, text.as_bytes());
    }

    fn mutate<R>(&self, f: impl FnOnce(&mut MacState) -> (R, Option<ProviderChange>)) -> R {
        let (r, change) = {
            let mut st = self.state.lock();
            let (r, change) = f(&mut st);
            if change.is_some() {
                self.save(&mut st);
            }
            (r, change)
        };
        if let Some(c) = change {
            self.subscribers.notify(&c);
        }
        r
    }
}

impl ProviderStore for MacProviderStore {
    fn providers(&self) -> Vec<TranslationProviderProfile> {
        self.state.lock().providers.clone()
    }

    fn active_provider_id(&self) -> Option<Uuid> {
        self.state.lock().active
    }

    fn load_problem(&self) -> Option<ProviderLoadProblem> {
        self.state.lock().problem
    }

    /// Normalized (trimmed; blank preset → custom; blank name → preset name); an existing id is replaced in place.
    /// `key = Some(k)` stores it (blank removes) after the profile is saved.
    fn add(&self, profile: TranslationProviderProfile, key: Option<&str>, set_active: bool) -> TranslationProviderProfile {
        let stored = Self::normalized(profile);
        self.mutate(|st| {
            match st.index(stored.id) {
                Some(i) => st.providers[i] = stored.clone(),
                None => st.providers.push(stored.clone()),
            }
            if set_active {
                st.active = Some(stored.id);
            }
            ((), Some(ProviderChange::Providers))
        });
        if let Some(k) = key {
            self.store_key(k, stored.id);
            self.subscribers.notify(&ProviderChange::Keys);
        }
        stored
    }

    fn update(&self, profile: TranslationProviderProfile, key: Option<&str>) -> bool {
        let id = profile.id;
        let stored = Self::normalized(profile);
        let found = self.mutate(|st| match st.index(id) {
            Some(i) => {
                st.providers[i] = stored;
                (true, Some(ProviderChange::Providers))
            }
            None => (false, None),
        });
        if found && let Some(k) = key {
            self.store_key(k, id);
            self.subscribers.notify(&ProviderChange::Keys);
        }
        found
    }

    fn remove(&self, id: Uuid) {
        self.secrets.remove(&TranslationProviderProfile::secret_account(id));
        self.mutate(|st| {
            let Some(i) = st.index(id) else { return ((), None) };
            st.providers.remove(i);
            if st.active == Some(id) {
                st.active = None;
            }
            ((), Some(ProviderChange::Providers))
        })
    }

    fn set_active(&self, id: Option<Uuid>) {
        self.mutate(|st| {
            let resolved = id.filter(|id| st.index(*id).is_some());
            if resolved == st.active {
                return ((), None);
            }
            st.active = resolved;
            ((), Some(ProviderChange::Active))
        })
    }

    fn update_model(&self, id: Uuid, model: &str) {
        let model = model.trim().to_owned();
        self.mutate(|st| {
            let Some(i) = st.index(id) else { return ((), None) };
            if st.providers[i].model == model {
                return ((), None);
            }
            st.providers[i].model = model;
            ((), Some(ProviderChange::Providers))
        })
    }

    fn set_discovered_models(&self, id: Uuid, models: &[String]) {
        self.mutate(|st| {
            let Some(i) = st.index(id) else { return ((), None) };
            let mut seen = std::collections::HashSet::new();
            let cleaned: Vec<String> = models
                .iter()
                .map(|m| m.trim().to_owned())
                .filter(|m| !m.is_empty() && seen.insert(m.to_lowercase()))
                .collect();
            let p = &mut st.providers[i];
            if p.model.is_empty()
                && let Some(first) = cleaned.first()
            {
                p.model = first.clone();
            }
            p.discovered_models = cleaned;
            ((), Some(ProviderChange::Providers))
        })
    }

    fn set_key(&self, id: Uuid, key: &str) -> bool {
        if self.state.lock().index(id).is_none() {
            return false;
        }
        let ok = self.store_key(key, id);
        self.subscribers.notify(&ProviderChange::Keys);
        ok
    }

    fn read_key(&self, id: Uuid) -> ProviderKeyRead {
        match self.secrets.read(&TranslationProviderProfile::secret_account(id)) {
            SecretRead::Found(b) => ProviderKeyRead::Found(SecretString::new(String::from_utf8_lossy(b.expose()).into_owned())),
            SecretRead::Absent => ProviderKeyRead::Absent,
            SecretRead::Unavailable => ProviderKeyRead::Unavailable,
        }
    }

    /// The active provider with trimmed URL (as typed), model and key; `None` for local models or an unusable address.
    fn active_external_llm(&self) -> Option<ActiveExternalLlm> {
        let provider = self.active_provider()?;
        let llm = ExternalLlm {
            url: provider.base_url.trim().to_owned(),
            model: provider.model.trim().to_owned(),
            api_key: self.api_key(provider.id).trim().to_owned(),
        };
        llm.normalized_url()?;
        Some(ActiveExternalLlm { llm, async_translation: provider.async_translation })
    }

    fn subscribe(&self, handler: Box<dyn Fn(ProviderChange) + Send + Sync>) -> Subscription {
        self.subscribers.subscribe(move |c: &ProviderChange| handler(*c))
    }
}

#[cfg(test)]
mod tests;
