//! Ports of `TranslationProviderStoreTests.swift` (33) plus the rule-5 relaunch test (mac-librarykit §21).
//! Run on every platform (the store is portable); the chmod test is unix-only.

use std::sync::Arc;

use parking_lot::Mutex;
use sa_core::secrets::{SecretBytes, SecretRead, SecretStore};
use sa_core::settings::{InMemorySettingsStore, SettingsStore};
use sa_enginekit::launch::ExternalLlm;
use sa_enginekit::presets::TranslationProviderPreset;
use uuid::Uuid;

use super::*;
use crate::{ProviderStore, TranslationProviderProfile, mask_key};

/// In-memory secrets that list their accounts (Swift `InMemorySecretStore.accounts`).
#[derive(Default)]
struct Secrets {
    map: Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
}

impl Secrets {
    fn accounts(&self) -> Vec<String> {
        self.map.lock().keys().cloned().collect()
    }
    fn string(&self, account: &str) -> Option<String> {
        self.map.lock().get(account).map(|b| String::from_utf8_lossy(b).into_owned())
    }
    fn put(&self, account: &str, v: &str) {
        self.map.lock().insert(account.to_owned(), v.as_bytes().to_vec());
    }
}

impl SecretStore for Secrets {
    fn read(&self, account: &str) -> SecretRead {
        match self.map.lock().get(account) {
            Some(b) => SecretRead::Found(SecretBytes::new(b.clone())),
            None => SecretRead::Absent,
        }
    }
    fn set(&self, bytes: &[u8], account: &str) -> bool {
        self.map.lock().insert(account.to_owned(), bytes.to_vec());
        true
    }
    fn remove(&self, account: &str) {
        self.map.lock().remove(account);
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    secrets: Arc<Secrets>,
}

impl Fixture {
    fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap(), secrets: Arc::new(Secrets::default()) }
    }
    fn file(&self) -> std::path::PathBuf {
        self.dir.path().join("providers").join(FILE_NAME)
    }
    fn store(&self) -> Arc<MacProviderStore> {
        MacProviderStore::new(self.file(), self.secrets.clone())
    }
    fn text(&self) -> String {
        std::fs::read_to_string(self.file()).unwrap()
    }
    fn json(&self) -> serde_json::Map<String, serde_json::Value> {
        match serde_json::from_str(&self.text()).unwrap() {
            serde_json::Value::Object(o) => o,
            _ => panic!("not an object"),
        }
    }
    fn write(&self, text: &str) {
        std::fs::create_dir_all(self.file().parent().unwrap()).unwrap();
        std::fs::write(self.file(), text).unwrap();
    }
    fn aside_files(&self) -> Vec<std::path::PathBuf> {
        let Ok(rd) = std::fs::read_dir(self.file().parent().unwrap()) else { return Vec::new() };
        let mut v: Vec<_> = rd
            .map(|e| e.unwrap().path())
            .filter(|p| {
                let n = p.file_name().unwrap().to_string_lossy();
                n.starts_with("translation-providers.corrupt-") && n.ends_with(".json")
            })
            .collect();
        v.sort();
        v
    }
}

fn profile(preset: &str, model: Option<&str>, url: Option<&str>, async_translation: bool) -> TranslationProviderProfile {
    let p = TranslationProviderPreset::preset(Some(preset));
    let mut profile = TranslationProviderProfile::from_preset(p, p.translates_in_background_by_default());
    if let Some(m) = model {
        profile.model = m.to_owned();
    }
    if let Some(u) = url {
        profile.base_url = u.to_owned();
    }
    profile.async_translation = async_translation;
    profile
}

fn bare(name: &str, preset: &str, url: &str, model: &str) -> TranslationProviderProfile {
    TranslationProviderProfile {
        id: Uuid::new_v4(),
        name: name.into(),
        preset_id: preset.into(),
        base_url: url.into(),
        model: model.into(),
        async_translation: true,
        discovered_models: Vec::new(),
    }
}

fn account(id: Uuid) -> String {
    TranslationProviderProfile::secret_account(id)
}

fn upper(id: Uuid) -> String {
    id.hyphenated().to_string().to_uppercase()
}

// MARK: persistence

#[test]
fn starts_empty_without_a_file() {
    let f = Fixture::new();
    let s = f.store();
    assert!(s.providers().is_empty());
    assert_eq!(s.active_provider_id(), None);
    assert!(s.active_provider().is_none());
    assert!(s.active_external_llm().is_none());
    assert!(!f.file().exists());
}

#[test]
fn round_trips_through_the_file() {
    let f = Fixture::new();
    let s = f.store();
    let mut openai = profile("openai", Some("gpt-4.1-mini"), None, false);
    openai.discovered_models = vec!["gpt-4.1-mini".into(), "o3".into()];
    let lan = profile("lan", None, Some("http://192.168.1.50:11434"), true);
    s.add(openai.clone(), Some("sk-1234567890"), false);
    s.add(lan.clone(), Some(""), true);
    assert_eq!(s.active_provider_id(), Some(lan.id));

    let r = f.store();
    assert_eq!(r.providers(), vec![openai.clone(), lan.clone()]);
    assert_eq!(r.active_provider_id(), Some(lan.id));
    assert_eq!(r.active_provider().unwrap().preset_id, "lan");
    assert_eq!(r.api_key(openai.id), "sk-1234567890");
    assert!(!r.providers()[0].async_translation);
    assert_eq!(r.providers()[0].discovered_models, vec!["gpt-4.1-mini", "o3"]);
}

#[test]
fn the_file_has_the_windows_shape_without_the_key() {
    let f = Fixture::new();
    let s = f.store();
    let stored = s.add(profile("groq", Some("gemma2-9b-it"), None, true), Some("gsk-super-secret-9999"), false);
    let json = f.json();
    let keys: std::collections::BTreeSet<_> = json.keys().cloned().collect();
    assert_eq!(keys, ["activeProviderId", "providers"].iter().map(|s| s.to_string()).collect());
    assert!(json["activeProviderId"].is_null());
    let entry = json["providers"][0].as_object().unwrap();
    let ekeys: std::collections::BTreeSet<_> = entry.keys().cloned().collect();
    assert_eq!(
        ekeys,
        ["id", "name", "presetId", "baseUrl", "model", "asyncTranslation", "discoveredModels"].iter().map(|s| s.to_string()).collect()
    );
    assert_eq!(entry["id"].as_str().unwrap(), upper(stored.id));
    assert_eq!(entry["presetId"], "groq");
    assert_eq!(entry["baseUrl"], "https://api.groq.com/openai/v1");
    assert_eq!(entry["asyncTranslation"], true);

    s.set_active(Some(stored.id));
    assert_eq!(f.json()["activeProviderId"].as_str().unwrap(), upper(stored.id));
    let text = f.text();
    assert!(text.contains("https://api.groq.com/openai/v1"));
    assert!(!text.contains("\\/"));
    assert!(!text.contains("gsk-super-secret"));
    assert!(!text.contains("apiKey"));
    // Apple pretty style: sorted keys, " : ", empty arrays on their own lines.
    assert!(text.starts_with("{\n  \"activeProviderId\" : \""), "{text}");
    assert!(text.contains("\"discoveredModels\" : [\n\n      ],"), "{text}");
}

#[test]
fn the_key_lives_only_in_the_secret_store() {
    let f = Fixture::new();
    let s = f.store();
    let stored = s.add(profile("openai", None, None, true), Some("  sk-abc  "), true);
    assert_eq!(f.secrets.accounts(), vec![format!("translation-provider.{}", upper(stored.id))]);
    assert_eq!(f.secrets.string(&account(stored.id)).as_deref(), Some("sk-abc"));
    assert_eq!(s.api_key(stored.id), "sk-abc");
    s.add(profile("lan", None, None, true), Some("   "), true);
    assert_eq!(f.secrets.accounts().len(), 1);
}

#[test]
fn reads_a_windows_file_and_never_writes_its_plain_text_key_back() {
    let f = Fixture::new();
    let id = "e621e1f8-c36c-495a-93fc-0c247a3e6e5f";
    let other = Uuid::new_v4().hyphenated().to_string();
    f.write(&format!(
        r#"{{"activeProviderId": "{id}", "providers": [
          {{"id": "{id}", "name": "OpenAI", "apiKey": "sk-leaked", "baseUrl": "https://api.openai.com/v1"}},
          {{"id": "{other}", "name": "x", "presetId": "lan", "baseUrl": "h", "model": "m",
           "asyncTranslation": false, "discoveredModels": ["a"]}}
        ]}}"#
    ));
    let s = f.store();
    assert_eq!(s.providers().len(), 2);
    assert_eq!(s.active_provider_id(), Some(Uuid::parse_str(id).unwrap()));
    let first = s.providers()[0].clone();
    assert_eq!(first.preset_id, "custom");
    assert!(first.model.is_empty());
    assert!(first.async_translation);
    assert!(first.discovered_models.is_empty());
    assert!(!s.providers()[1].async_translation);
    assert!(s.api_key(first.id).is_empty());
    s.update_model(first.id, "gpt-4o-mini");
    assert!(!f.text().contains("sk-leaked"));
}

#[test]
fn an_unknown_active_id_or_a_broken_file_starts_clean() {
    let f = Fixture::new();
    f.write(r#"{"activeProviderId": "e621e1f8-c36c-495a-93fc-0c247a3e6e5f", "providers": []}"#);
    assert_eq!(f.store().active_provider_id(), None);
    f.write("not json");
    let broken = f.store();
    assert!(broken.providers().is_empty() && broken.active_provider_id().is_none());
}

// MARK: damaged and unreadable files

fn is_aside_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("translation-providers.corrupt-") else { return false };
    let Some(ts) = rest.strip_suffix(".json") else { return false };
    let b = ts.as_bytes();
    b.len() == 15 && b[8] == b'-' && b.iter().enumerate().all(|(i, c)| i == 8 || c.is_ascii_digit())
}

#[test]
fn one_bad_entry_is_skipped_and_the_others_survive() {
    let f = Fixture::new();
    let good = Uuid::new_v4();
    let other = Uuid::new_v4();
    let text = format!(
        r#"{{"activeProviderId": "{o}", "providers": [
          {{"id": "{g}", "name": "Birinci", "presetId": "lan", "baseUrl": "http://a:1", "model": "m1"}},
          {{"id": "not-a-uuid", "name": "Bozuk", "presetId": "openai", "baseUrl": "https://api.openai.com/v1"}},
          {{"id": "{o}", "name": "İkinci", "presetId": "groq", "baseUrl": "https://g/v1", "asyncTranslation": false}}
        ]}}"#,
        o = upper(other),
        g = upper(good)
    );
    f.write(&text);
    f.secrets.put(&account(good), "sk-keep");
    let s = f.store();
    assert_eq!(s.providers().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["Birinci", "İkinci"]);
    assert_eq!(s.active_provider_id(), Some(other));
    assert_eq!(s.api_key(good), "sk-keep");
    assert_eq!(f.text(), text, "loading never touches the file");
    assert!(f.aside_files().is_empty());

    s.update_model(other, "m2");
    let aside = f.aside_files();
    assert_eq!(aside.len(), 1);
    assert_eq!(std::fs::read_to_string(&aside[0]).unwrap(), text);
    assert!(is_aside_name(&aside[0].file_name().unwrap().to_string_lossy()));
    let r = f.store();
    assert_eq!(r.providers().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["Birinci", "İkinci"]);
    assert_eq!(r.providers()[1].model, "m2");
    assert_eq!(r.api_key(good), "sk-keep");
    s.update_model(other, "m3");
    assert_eq!(f.aside_files().len(), 1);
}

#[test]
fn a_truncated_file_is_set_aside_before_the_first_save() {
    let f = Fixture::new();
    let id = Uuid::new_v4();
    let truncated = format!(r#"{{"activeProviderId": null, "providers": [{{"id": "{}", "name": "Yarım", "presetId": "lan", "baseUr"#, upper(id));
    f.write(&truncated);
    let s = f.store();
    assert!(s.providers().is_empty());
    assert_eq!(f.text(), truncated);
    let added = s.add(profile("openai", None, None, true), Some("sk-new"), true);
    let aside: Vec<String> = f.aside_files().iter().map(|p| std::fs::read_to_string(p).unwrap()).collect();
    assert_eq!(aside, vec![truncated]);
    let r = f.store();
    assert_eq!(r.providers(), vec![added.clone()]);
    assert_eq!(r.api_key(added.id), "sk-new");
    assert_eq!(f.aside_files().len(), 1);
}

#[test]
fn a_malformed_active_id_is_dropped_and_the_file_is_kept_aside() {
    let f = Fixture::new();
    let id = Uuid::new_v4();
    f.write(&format!(r#"{{"activeProviderId": 42, "providers": [{{"id": "{}", "name": "Tek", "presetId": "lan", "baseUrl": "http://a:1"}}]}}"#, upper(id)));
    let s = f.store();
    assert_eq!(s.providers().iter().map(|p| p.id).collect::<Vec<_>>(), vec![id]);
    assert_eq!(s.active_provider_id(), None);
    s.set_active(Some(id));
    assert_eq!(f.aside_files().len(), 1);
}

#[test]
fn a_missing_file_is_saved_without_any_copy() {
    let f = Fixture::new();
    f.store().add(profile("lan", None, Some("http://a:1"), true), Some(""), true);
    assert!(f.file().exists());
    assert!(f.aside_files().is_empty());
}

#[test]
fn a_clean_file_is_rewritten_without_any_copy() {
    let f = Fixture::new();
    f.store().add(profile("lan", None, Some("http://a:1"), true), Some(""), true);
    f.store().add(profile("openai", None, None, true), Some(""), true);
    assert!(f.aside_files().is_empty());
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_is_never_overwritten_and_is_merged_once_it_can_be_read() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: getuid never fails.
    if unsafe { libc::getuid() } == 0 {
        return; // root ignores file permissions
    }
    let f = Fixture::new();
    let on_disk = f.store();
    let first = on_disk.add(profile("lan", None, Some("http://a:1"), true), Some(""), true);
    let original = f.text();
    std::fs::set_permissions(f.file(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let blind = f.store();
    assert!(blind.providers().is_empty());
    let second = blind.add(profile("openai", None, None, true), Some("sk-2"), false);
    assert_eq!(blind.providers(), vec![second.clone()]);
    std::fs::set_permissions(f.file(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(f.text(), original);
    assert!(f.aside_files().is_empty());
    blind.update_model(second.id, "gpt-4.1");
    assert_eq!(blind.providers().iter().map(|p| p.id).collect::<Vec<_>>(), vec![first.id, second.id]);
    assert_eq!(blind.active_provider_id(), Some(first.id));
    let r = f.store();
    assert_eq!(r.providers().iter().map(|p| p.id).collect::<Vec<_>>(), vec![first.id, second.id]);
    assert_eq!(r.providers()[1].model, "gpt-4.1");
    assert_eq!(r.active_provider_id(), Some(first.id));
    assert!(f.aside_files().is_empty());
}

// MARK: editing

#[test]
fn add_trims_and_names_after_the_preset_when_blank() {
    let f = Fixture::new();
    let s = f.store();
    let stored = s.add(bare("  ", "deepseek", " https://api.deepseek.com/v1 ", " deepseek-chat "), Some("k"), false);
    assert_eq!(stored.name, "DeepSeek");
    assert_eq!(stored.base_url, "https://api.deepseek.com/v1");
    assert_eq!(stored.model, "deepseek-chat");
    assert_eq!(s.providers(), vec![stored.clone()]);
    assert_eq!(s.active_provider_id(), None);
    let mut again = stored.clone();
    again.name = "Mine".into();
    s.add(again, Some("k2"), true);
    assert_eq!(s.providers().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["Mine"]);
    assert_eq!(s.api_key(stored.id), "k2");
    assert_eq!(s.active_provider_id(), Some(stored.id));
}

#[test]
fn update_keeps_replaces_or_removes_the_key() {
    let f = Fixture::new();
    let s = f.store();
    let mut stored = s.add(profile("openai", None, None, true), Some("sk-old"), true);
    stored.model = "gpt-4.1-nano".into();
    stored.async_translation = false;
    assert!(s.update(stored.clone(), None));
    assert_eq!(s.api_key(stored.id), "sk-old");
    assert!(s.providers()[0].model == "gpt-4.1-nano" && !s.providers()[0].async_translation);
    assert!(s.update(stored.clone(), Some("sk-new")));
    assert_eq!(s.api_key(stored.id), "sk-new");
    assert!(s.update(stored.clone(), Some("  ")));
    assert!(s.api_key(stored.id).is_empty());
    assert!(f.secrets.accounts().is_empty());
    assert_eq!(f.store().providers()[0].model, "gpt-4.1-nano");
    assert!(!s.update(profile("groq", None, None, true), Some("x")));
    assert!(s.providers().len() == 1 && f.secrets.accounts().is_empty());
}

#[test]
fn set_key_replaces_the_key_of_an_existing_provider() {
    let f = Fixture::new();
    let s = f.store();
    let stored = s.add(profile("openai", None, None, true), Some(""), true);
    assert!(s.set_key(stored.id, "sk-late"));
    assert_eq!(s.api_key(stored.id), "sk-late");
    assert!(!s.set_key(Uuid::new_v4(), "sk-orphan"));
    assert_eq!(f.secrets.accounts().len(), 1);
}

#[test]
fn removing_deletes_the_key_and_clears_the_active_provider() {
    let f = Fixture::new();
    let s = f.store();
    let first = s.add(profile("openai", None, None, true), Some("sk-1"), false);
    let second = s.add(profile("deepseek", None, None, true), Some("sk-2"), true);
    assert_eq!(s.active_provider_id(), Some(second.id));
    s.remove(first.id);
    assert_eq!(s.providers(), vec![second.clone()]);
    assert_eq!(s.active_provider_id(), Some(second.id));
    assert_eq!(f.secrets.accounts(), vec![account(second.id)]);
    s.remove(second.id);
    assert!(s.providers().is_empty());
    assert_eq!(s.active_provider_id(), None);
    assert!(s.active_external_llm().is_none());
    assert!(f.secrets.accounts().is_empty());
    let r = f.store();
    assert!(r.providers().is_empty() && r.active_provider_id().is_none());
}

#[test]
fn setting_the_active_provider() {
    let f = Fixture::new();
    let s = f.store();
    let first = s.add(profile("openai", None, None, true), Some(""), false);
    let second = s.add(profile("lan", None, None, true), Some(""), false);
    assert_eq!(s.active_provider_id(), None);
    s.set_active(Some(first.id));
    assert_eq!(f.store().active_provider_id(), Some(first.id));
    s.set_active(Some(second.id));
    assert_eq!(s.active_provider().unwrap().id, second.id);
    s.set_active(Some(Uuid::new_v4()));
    assert_eq!(s.active_provider_id(), None);
    s.set_active(Some(first.id));
    s.set_active(None);
    assert_eq!(f.store().active_provider_id(), None);
}

#[test]
fn updating_the_model_only() {
    let f = Fixture::new();
    let s = f.store();
    let stored = s.add(profile("openai", None, None, true), Some(""), true);
    s.update_model(stored.id, "  gpt-4.1-mini ");
    assert_eq!(f.store().providers()[0].model, "gpt-4.1-mini");
    s.update_model(Uuid::new_v4(), "x");
    assert_eq!(s.providers()[0].model, "gpt-4.1-mini");
}

#[test]
fn discovered_models_are_cleaned_and_fill_an_empty_model() {
    let f = Fixture::new();
    let s = f.store();
    let blank = s.add(profile("custom", Some(""), Some("https://x.test/v1"), true), Some(""), true);
    s.set_discovered_models(blank.id, &[" a ".into(), "b".into(), "A".into(), "".into(), "c".into()]);
    assert_eq!(s.providers()[0].discovered_models, vec!["a", "b", "c"]);
    assert_eq!(s.providers()[0].model, "a");
    s.update_model(blank.id, "mine");
    s.set_discovered_models(blank.id, &["z".into()]);
    assert_eq!(s.providers()[0].model, "mine");
    assert_eq!(f.store().providers()[0].discovered_models, vec!["z"]);
    s.set_discovered_models(Uuid::new_v4(), &["q".into()]);
    assert_eq!(s.providers()[0].discovered_models, vec!["z"]);
}

// MARK: key mask

#[test]
fn mask_rule() {
    assert_eq!(mask_key(""), None);
    assert_eq!(mask_key("   "), None);
    assert_eq!(mask_key("abcd").as_deref(), Some("••••"));
    assert_eq!(mask_key("a").as_deref(), Some("••••"));
    assert_eq!(mask_key("abcde").as_deref(), Some("••••bcde"));
    assert_eq!(mask_key("12345678").as_deref(), Some("••••5678"));
    assert_eq!(mask_key("123456789").as_deref(), Some("123••••6789"));
    assert_eq!(mask_key("sk-1234567890").as_deref(), Some("sk-••••7890"));
    assert_eq!(mask_key("  sk-1234567890\n").as_deref(), Some("sk-••••7890"));
}

#[test]
fn masked_key_reads_the_stored_key() {
    let f = Fixture::new();
    let s = f.store();
    let with = s.add(profile("openai", None, None, true), Some("sk-1234567890"), false);
    let without = s.add(profile("lan", None, None, true), Some(""), true);
    assert_eq!(s.masked_key(with.id).as_deref(), Some("sk-••••7890"));
    assert_eq!(s.masked_key(without.id), None);
    assert_eq!(s.masked_key(Uuid::new_v4()), None);
}

// MARK: models

#[test]
fn available_models_list_the_chosen_one_then_discovered_then_suggested() {
    let mut openai = profile("openai", Some("gpt-4.1-nano"), None, true);
    openai.discovered_models = vec!["gpt-4o-mini".into(), "o3".into()];
    assert_eq!(openai.available_models(true), vec!["gpt-4.1-nano", "gpt-4o-mini", "o3", "gpt-4.1-mini"]);
    let mut shouting = profile("openai", Some("GPT-4O-MINI"), None, true);
    shouting.discovered_models = vec!["gpt-4o-mini".into()];
    assert_eq!(shouting.available_models(true), vec!["GPT-4O-MINI", "gpt-4.1-mini", "gpt-4.1-nano"]);
    assert_eq!(profile("openai", Some(""), None, true).available_models(true), vec!["gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano"]);
    assert!(profile("custom", Some(""), None, true).available_models(true).is_empty());
    assert_eq!(profile("custom", Some(" mine "), None, true).available_models(true), vec!["mine"]);
    // Windows order: discovered first, the chosen model inserted first only when new.
    assert_eq!(openai.available_models(false), vec!["gpt-4o-mini", "o3", "gpt-4.1-mini", "gpt-4.1-nano"]);
    let mut fresh = profile("openai", Some("custom-x"), None, true);
    fresh.discovered_models = vec!["o3".into()];
    assert_eq!(fresh.available_models(false), vec!["custom-x", "o3", "gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano"]);
    let mut listed = profile("openai", Some("o3"), None, true);
    listed.discovered_models = vec!["gpt-4o-mini".into(), "o3".into()];
    assert_eq!(listed.available_models(false), vec!["gpt-4o-mini", "o3", "gpt-4.1-mini", "gpt-4.1-nano"]);
    assert_eq!(listed.available_models(true), vec!["o3", "gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano"]);
}

#[test]
fn a_lan_server_that_reported_its_models_drops_the_suggestions() {
    assert_eq!(profile("lan", Some(""), None, true).available_models(true), vec!["gemma4:e4b", "qwen3.5:4b"]);
    let mut lan = profile("lan", Some(""), None, true);
    lan.discovered_models = vec!["qwen3:14b".into(), "llama3.2".into()];
    assert_eq!(lan.available_models(true), vec!["qwen3:14b", "llama3.2"]);
    lan.model = "llama3.2".into();
    assert_eq!(lan.available_models(true), vec!["llama3.2", "qwen3:14b"]);
}

#[test]
fn available_models_by_id_and_an_unknown_preset_is_custom() {
    let f = Fixture::new();
    let s = f.store();
    let stored = s.add(profile("gemini", None, None, true), Some(""), true);
    assert_eq!(s.available_models(stored.id), vec!["gemini-2.5-flash", "gemini-2.5-pro"]);
    assert!(s.available_models(Uuid::new_v4()).is_empty());
    let odd = bare("", "no-such-preset", "", "m");
    assert_eq!(odd.preset().id, "custom");
    assert_eq!(odd.available_models(true), vec!["m"]);
}

// MARK: engine mapping

#[test]
fn the_active_provider_becomes_the_external_llm() {
    let f = Fixture::new();
    let s = f.store();
    let openai = s.add(profile("openai", Some(" gpt-4.1-mini "), None, false), Some(" sk-live \n"), false);
    let lan = s.add(profile("lan", Some("gemma4:e4b"), Some("10.0.0.2:11434"), true), Some(""), false);
    assert!(s.active_external_llm().is_none());
    s.set_active(Some(openai.id));
    assert_eq!(
        s.active_external_llm(),
        Some(ActiveExternalLlm {
            llm: ExternalLlm { url: "https://api.openai.com/v1".into(), model: "gpt-4.1-mini".into(), api_key: "sk-live".into() },
            async_translation: false,
        })
    );
    s.set_active(Some(lan.id));
    assert_eq!(
        s.active_external_llm(),
        Some(ActiveExternalLlm {
            llm: ExternalLlm { url: "10.0.0.2:11434".into(), model: "gemma4:e4b".into(), api_key: String::new() },
            async_translation: true,
        })
    );
    s.set_active(None);
    assert!(s.active_external_llm().is_none());
}

#[test]
fn a_blank_address_is_no_external_server() {
    let f = Fixture::new();
    let s = f.store();
    s.add(profile("custom", None, Some("  "), true), Some("sk"), true);
    assert!(s.active_provider().is_some());
    assert!(s.active_external_llm().is_none());
    let mut unusable = s.providers()[0].clone();
    unusable.base_url = "http://".into();
    s.update(unusable, None);
    assert!(s.active_external_llm().is_none());
}

// MARK: migration

fn legacy_defaults(url: Option<&str>, model: Option<&str>, enabled: bool, async_translation: bool) -> Arc<InMemorySettingsStore> {
    let d = InMemorySettingsStore::new();
    if let Some(u) = url {
        d.set_string("externalLlmUrl", u);
    }
    if let Some(m) = model {
        d.set_string("externalLlmModel", m);
    }
    d.set_bool("externalLlmEnabled", enabled);
    d.set_bool("externalLlmAsync", async_translation);
    d
}

fn lan_names() -> [String; 2] {
    let p = TranslationProviderPreset::preset(Some("lan"));
    [p.name(Some(sa_core::UiLanguage::Tr)), p.name(Some(sa_core::UiLanguage::En))]
}

#[test]
fn an_enabled_legacy_server_becomes_the_active_lan_provider() {
    let f = Fixture::new();
    let s = f.store();
    let d = legacy_defaults(Some(" 192.168.1.50:11434 "), Some(" gemma4:e4b "), true, true);
    assert!(s.migrate_legacy_if_needed(&*d));
    assert_eq!(s.providers().len(), 1);
    let p = s.providers()[0].clone();
    assert_eq!(p.preset_id, "lan");
    assert!(lan_names().contains(&p.name));
    assert_eq!(p.base_url, "http://192.168.1.50:11434");
    assert_eq!(p.model, "gemma4:e4b");
    assert!(p.async_translation);
    assert_eq!(s.active_provider_id(), Some(p.id));
    assert!(f.secrets.accounts().is_empty());
    assert!(f.file().exists());
    assert_eq!(d.get_bool(MIGRATION_DEFAULTS_KEY), Some(true));
    assert_eq!(d.get_string("externalLlmUrl").as_deref(), Some(" 192.168.1.50:11434 "));
    assert_eq!(f.store().active_provider_id(), Some(p.id));
}

#[test]
fn a_disabled_legacy_server_is_kept_but_not_active() {
    let f = Fixture::new();
    let s = f.store();
    assert!(s.migrate_legacy_if_needed(&*legacy_defaults(Some("mac.local:1234"), None, false, false)));
    let p = s.providers()[0].clone();
    assert_eq!(p.base_url, "http://mac.local:1234");
    assert!(p.model.is_empty());
    assert!(!p.async_translation);
    assert_eq!(s.active_provider_id(), None);
    assert!(s.active_external_llm().is_none());
}

#[test]
fn a_scheme_less_legacy_address_keeps_meaning_plain_http() {
    for (legacy, expected) in [
        ("gpu.home.example:11434", "http://gpu.home.example:11434"),
        ("box.tail123.ts.net:11434", "http://box.tail123.ts.net:11434"),
        ("192.168.1.50:1234", "http://192.168.1.50:1234"),
        ("HTTP://Mac.local:1234", "HTTP://Mac.local:1234"),
        ("https://llm.example.com/v1", "https://llm.example.com/v1"),
        ("http://nas.lan:8080", "http://nas.lan:8080"),
    ] {
        let f = Fixture::new();
        let s = f.store();
        assert!(s.migrate_legacy_if_needed(&*legacy_defaults(Some(legacy), None, true, false)));
        let p = s.providers()[0].clone();
        assert_eq!(p.base_url, expected, "{legacy}");
        if !legacy.to_lowercase().starts_with("https") {
            let n = ExternalLlm { url: p.base_url.clone(), ..Default::default() }.normalized_url();
            assert!(n.unwrap().to_lowercase().starts_with("http://"), "{legacy}");
        }
    }
    assert_eq!(
        ExternalLlm { url: "gpu.home.example:11434".into(), ..Default::default() }.normalized_url().as_deref(),
        Some("https://gpu.home.example:11434")
    );
}

#[test]
fn new_providers_translate_in_the_background_only_on_the_local_network() {
    for preset in TranslationProviderPreset::ALL.iter() {
        let created = TranslationProviderProfile::from_preset(preset, preset.translates_in_background_by_default());
        assert_eq!(created.async_translation, preset.id == "lan", "{}", preset.id);
        assert_eq!(preset.translates_in_background_by_default(), preset.id == "lan");
    }
}

#[test]
fn a_blank_legacy_address_migrates_nothing_and_writes_no_file() {
    let f = Fixture::new();
    for url in [None, Some(""), Some("   ")] {
        let d = legacy_defaults(url, None, true, false);
        let s = f.store();
        assert!(!s.migrate_legacy_if_needed(&*d));
        assert!(s.providers().is_empty());
        assert_eq!(d.get_bool(MIGRATION_DEFAULTS_KEY), Some(true));
    }
    assert!(!f.file().exists());
}

#[test]
fn an_existing_providers_file_is_never_overwritten_by_the_migration() {
    let f = Fixture::new();
    let existing = f.store().add(profile("openai", None, None, true), Some(""), false);
    let s = f.store();
    assert!(!s.migrate_legacy_if_needed(&*legacy_defaults(Some("mac.local:1234"), None, true, false)));
    assert_eq!(s.providers(), vec![existing]);
    assert_eq!(s.active_provider_id(), None);
}

#[test]
fn the_migration_runs_only_once() {
    let f = Fixture::new();
    let d = legacy_defaults(Some("mac.local:1234"), None, true, false);
    assert!(f.store().migrate_legacy_if_needed(&*d));
    let _ = std::fs::remove_file(f.file());
    let again = f.store();
    assert!(!again.migrate_legacy_if_needed(&*d));
    assert!(again.providers().is_empty());
    let flagged = legacy_defaults(Some("mac.local:1234"), None, true, false);
    flagged.set_bool(MIGRATION_DEFAULTS_KEY, true);
    assert!(!f.store().migrate_legacy_if_needed(&*flagged));
}

// MARK: rule 5 (encrypted keys, no Keychain)

#[test]
fn with_the_encrypted_store_a_key_survives_a_relaunch_and_never_appears_in_either_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(FILE_NAME);
    let secrets = crate::secrets::EncryptedSecretStore::for_app_data(dir.path());
    let s = MacProviderStore::new(file.clone(), secrets);
    let p = s.add(profile("openai", None, None, true), Some("sk-relaunch-8888"), true);
    drop(s);
    let r = MacProviderStore::new(file, crate::secrets::EncryptedSecretStore::for_app_data(dir.path()));
    assert_eq!(r.api_key(p.id), "sk-relaunch-8888");
    assert_eq!(r.active_external_llm().unwrap().llm.api_key, "sk-relaunch-8888");
    for e in std::fs::read_dir(dir.path()).unwrap() {
        let path = e.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("sk-relaunch"), "{}", path.display());
    }
    let sidecar = std::fs::read_to_string(dir.path().join("translation-provider-keys.json")).unwrap();
    assert!(sidecar.contains(&p.id.hyphenated().to_string()), "entry = lowercase UUID");
}

#[test]
fn orphan_keys_are_pruned_only_when_the_file_loaded_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(FILE_NAME);
    let secrets = crate::secrets::EncryptedSecretStore::for_app_data(dir.path());
    let s = MacProviderStore::new(file.clone(), secrets.clone());
    let keep = s.add(profile("openai", None, None, true), Some("sk-keep-1111"), true);
    // A blob whose provider is gone (written by an older build, or the file was replaced).
    let orphan = Uuid::new_v4();
    assert!(secrets.set(b"sk-orphan-2222", &TranslationProviderProfile::secret_account(orphan)));
    drop(s);
    let sidecar = || std::fs::read_to_string(dir.path().join("translation-provider-keys.json")).unwrap();

    // Damaged file: nothing is pruned.
    let good = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, "{\"providers\": [").unwrap();
    let damaged = MacProviderStore::new(file.clone(), secrets.clone());
    damaged.prune_orphan_keys(&secrets);
    assert!(sidecar().contains(&keep.id.hyphenated().to_string()));
    assert!(sidecar().contains(&orphan.hyphenated().to_string()));
    drop(damaged);

    std::fs::write(&file, good).unwrap();
    let s = MacProviderStore::new(file, secrets.clone());
    s.prune_orphan_keys(&secrets);
    assert!(sidecar().contains(&keep.id.hyphenated().to_string()));
    assert!(!sidecar().contains(&orphan.hyphenated().to_string()));
    assert_eq!(s.api_key(keep.id), "sk-keep-1111");
}

#[test]
fn an_unreadable_key_is_never_treated_as_absent() {
    struct Broken;
    impl SecretStore for Broken {
        fn read(&self, _: &str) -> SecretRead {
            SecretRead::Unavailable
        }
        fn set(&self, _: &[u8], _: &str) -> bool {
            false
        }
        fn remove(&self, _: &str) {}
    }
    let dir = tempfile::tempdir().unwrap();
    let s = MacProviderStore::new(dir.path().join(FILE_NAME), Arc::new(Broken));
    let p = s.add(profile("openai", None, None, true), None, true);
    assert!(matches!(s.read_key(p.id), crate::ProviderKeyRead::Unavailable));
    assert!(!s.set_key(p.id, "sk-x"));
}
