//! Ports of `TranslationProviderTests.cs` plus the new key-encryption, migration and file-safety tests
//! (win-providers §13). Every test uses its own temp folder, a fresh `EngineLaunchState` (the C# tests saved and
//! restored `ExternalLlmState.Shared`) and the real DPAPI-protected secret store.

use std::sync::Arc;

use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_core::subscription::{Subscribers, Subscription};
use sa_enginekit::external_llm::{HttpFetch, HttpRequest};
use sa_enginekit::launch::{EngineLaunchSettings, EngineLaunchState};
use sa_enginekit::presets::TranslationProviderPreset;
use sa_sync::{StateSyncEvent, SyncBus, SyncHandler, Topic, TopicFilter};
use uuid::Uuid;

use super::*;
use crate::secrets::EncryptedSecretStore;
use crate::{ProviderKeyRead, ProviderStore, TranslationProviderProfile, mask_key};

/// Serializes the tests of this module: they assert Turkish messages of the process-wide language state.
static LANGUAGE: Mutex<()> = Mutex::new(());

struct Fixture {
    dir: tempfile::TempDir,
    launch: Arc<EngineLaunchState>,
    _language: parking_lot::MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let guard = LANGUAGE.lock();
        sa_core::l10n::LanguageState::shared().set_language(sa_core::UiLanguage::Tr);
        Self {
            dir: tempfile::tempdir().unwrap(),
            launch: EngineLaunchState::new(EngineLaunchSettings { offline: true, external_llm: None }),
            _language: guard,
        }
    }
    /// A second folder for tests that need two (shares the first fixture's language lock).
    fn sibling(&self) -> (tempfile::TempDir, Arc<EngineLaunchState>) {
        (tempfile::tempdir().unwrap(), EngineLaunchState::new(EngineLaunchSettings { offline: true, external_llm: None }))
    }
    fn file(&self) -> std::path::PathBuf {
        self.dir.path().join(FILE_NAME)
    }
    fn secrets(&self) -> Arc<EncryptedSecretStore> {
        EncryptedSecretStore::for_app_data(self.dir.path())
    }
    fn manager(&self) -> Arc<WinProviderManager> {
        WinProviderManager::with_launch_state(self.file(), self.secrets(), None, self.launch.clone())
    }
    fn manager_on(&self, bus: Arc<dyn SyncBus>) -> Arc<WinProviderManager> {
        WinProviderManager::with_launch_state(self.file(), self.secrets(), Some(bus), self.launch.clone())
    }
    fn state(&self) -> (bool, String, String, String) {
        match self.launch.get().external_llm {
            Some(l) => (true, l.url, l.model, l.api_key),
            None => (false, String::new(), String::new(), String::new()),
        }
    }
    fn text(&self) -> String {
        std::fs::read_to_string(self.file()).unwrap()
    }
    /// All files of the folder as lossy text.
    fn all_text(&self) -> String {
        let mut s = String::new();
        for e in std::fs::read_dir(self.dir.path()).unwrap() {
            let p = e.unwrap().path();
            if p.is_file() {
                s.push_str(&String::from_utf8_lossy(&std::fs::read(p).unwrap()));
            }
        }
        s
    }
    fn aside_files(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("translation-providers.corrupt-"))
            .collect();
        v.sort();
        v
    }
}

/// C#-style `AddProvider(presetId, name, baseUrl, apiKey, model, asyncTranslation, setActive)`.
fn add(
    m: &WinProviderManager,
    preset: &str,
    name: Option<&str>,
    url: Option<&str>,
    key: Option<&str>,
    model: Option<&str>,
    set_active: bool,
) -> TranslationProviderProfile {
    let mut p = TranslationProviderProfile::from_preset(TranslationProviderPreset::preset(Some(preset)), true);
    p.preset_id = preset.to_owned();
    p.name = name.unwrap_or("").to_owned();
    p.base_url = url.unwrap_or("").to_owned();
    p.model = model.unwrap_or("").to_owned();
    m.add(p, Some(key.unwrap_or("")), set_active)
}

fn key_of(m: &WinProviderManager, id: Uuid) -> Option<String> {
    match m.read_key(id) {
        ProviderKeyRead::Found(k) => Some(k.expose().to_owned()),
        _ => None,
    }
}

/// Fake HTTP: records every request, answers with `respond`.
struct FakeFetch {
    requests: Mutex<Vec<HttpRequest>>,
    respond: Box<dyn Fn(&HttpRequest) -> Result<(u16, String), Option<String>> + Send + Sync>,
}

impl FakeFetch {
    fn new(respond: impl Fn(&HttpRequest) -> Result<(u16, String), Option<String>> + Send + Sync + 'static) -> Self {
        Self { requests: Mutex::new(Vec::new()), respond: Box::new(respond) }
    }
    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().clone()
    }
}

impl HttpFetch for FakeFetch {
    fn fetch(&self, request: HttpRequest, _cancel: &CancellationToken) -> Result<(u16, String), Option<String>> {
        let r = (self.respond)(&request);
        self.requests.lock().push(request);
        r
    }
}

/// In-process broker: every subscriber sees every published event (consumers filter their own).
#[derive(Default)]
struct TestBus {
    subscribers: Subscribers<StateSyncEvent>,
    published: Mutex<Vec<StateSyncEvent>>,
}

impl SyncBus for TestBus {
    fn publish(&self, _topic: Topic, event: StateSyncEvent) {
        self.published.lock().push(event.clone());
        self.subscribers.notify(&event);
    }
    fn subscribe(&self, filter: TopicFilter, handler: SyncHandler) -> Subscription {
        self.subscribers.subscribe(move |e: &StateSyncEvent| {
            if filter == TopicFilter::All || TopicFilter::Only(Topic::Providers) == filter && e.topic == "providers" {
                handler(e)
            }
        })
    }
    fn active_instance_count(&self) -> usize {
        1
    }
    fn has_other_active_instances(&self) -> bool {
        false
    }
    fn instance_id(&self) -> &str {
        "bus"
    }
    fn dispose(&self) {}
}

// MARK: ported C# tests

#[test]
fn built_in_presets_include_all_required_providers() {
    let ids: Vec<&str> = TranslationProviderPreset::ALL.iter().map(|p| p.id).collect();
    for id in ["opencode-go", "openrouter", "openai", "deepseek", "groq", "gemini", "lan", "custom"] {
        assert!(ids.contains(&id), "{id}");
    }
    let oc = TranslationProviderPreset::preset(Some("opencode-go"));
    assert_eq!(oc.name(None), "OpenCode Go");
    assert_eq!(oc.base_url, "https://opencode.ai/inference/go/openai/v1");
    assert!(oc.requires_api_key);
    assert!(oc.suggested_models.contains(&"glm-5.3-flash"));
    assert!(oc.suggested_models.contains(&"deepseek-v4.1-flash"));
}

#[test]
fn add_multiple_providers_persists_and_syncs_external_llm_state() {
    let f = Fixture::new();
    let m = f.manager();
    assert!(m.providers().is_empty());
    assert_eq!(m.active_provider_id(), None);
    assert!(!m.is_external_provider_active());
    assert!(!f.state().0);

    let oc = add(&m, "opencode-go", None, None, Some("sk-oc-1234567890"), Some("kimi-k2.5"), true);
    assert_eq!(m.providers().len(), 1);
    assert_eq!(m.active_provider_id(), Some(oc.id));
    assert!(m.is_external_provider_active());
    assert_eq!(
        f.state(),
        (true, "https://opencode.ai/inference/go/openai/v1".into(), "kimi-k2.5".into(), "sk-oc-1234567890".into())
    );
    assert_eq!(m.masked_key(oc.id), Ok(Some("sk-\u{2022}\u{2022}\u{2022}\u{2022}7890".into())));

    let or = add(&m, "openrouter", Some("My OpenRouter"), None, Some("sk-or-abcdef9876"), Some("google/gemma-3-27b-it"), false);
    let lan = add(&m, "lan", None, Some("http://192.168.1.50:11434"), Some(""), Some("gemma4:e4b"), false);
    assert_eq!(m.providers().len(), 3);
    assert_eq!(m.active_provider_id(), Some(oc.id));

    m.set_active(Some(or.id));
    assert_eq!(m.active_provider_id(), Some(or.id));
    assert_eq!(f.state(), (true, "https://openrouter.ai/api/v1".into(), "google/gemma-3-27b-it".into(), "sk-or-abcdef9876".into()));

    m.set_active(Some(lan.id));
    assert_eq!(f.state(), (true, "http://192.168.1.50:11434".into(), "gemma4:e4b".into(), String::new()));

    let reloaded = f.manager();
    assert_eq!(reloaded.providers().len(), 3);
    assert_eq!(reloaded.active_provider_id(), Some(lan.id));
    assert_eq!(key_of(&reloaded, or.id).as_deref(), Some("sk-or-abcdef9876"));

    reloaded.set_active(None);
    assert_eq!(reloaded.active_provider_id(), None);
    assert!(!reloaded.is_external_provider_active());
    assert!(!f.state().0);
}

#[test]
fn update_and_remove_provider_updates_state_and_disk() {
    let f = Fixture::new();
    let m = f.manager();
    let p1 = add(&m, "openai", None, None, Some("sk-old-1111"), Some("gpt-4o-mini"), true);
    let p2 = add(&m, "deepseek", None, None, Some("sk-ds-2222"), Some("deepseek-chat"), false);
    let mut edit = p1.clone();
    edit.name = "OpenAI Pro".into();
    edit.base_url = "https://api.openai.com/v1".into();
    edit.model = "gpt-4.1-mini".into();
    edit.async_translation = false;
    assert!(m.update(edit, Some("sk-new-9999")));
    assert_eq!(m.active_provider().unwrap().name, "OpenAI Pro");
    let (_, _, model, key) = f.state();
    assert_eq!((model.as_str(), key.as_str()), ("gpt-4.1-mini", "sk-new-9999"));
    assert!(!m.active_provider().unwrap().async_translation);

    m.remove(p1.id);
    assert_eq!(m.providers().len(), 1);
    assert_eq!(m.providers()[0].id, p2.id);
    assert_eq!(m.active_provider_id(), None);
    assert!(!f.state().0);
    // The removed provider's key is gone from the sidecar too.
    let s = f.secrets();
    assert!(matches!(
        sa_core::secrets::SecretStore::read(&*s, &TranslationProviderProfile::secret_account(p1.id)),
        sa_core::secrets::SecretRead::Absent
    ));
    assert_eq!(key_of(&f.manager(), p2.id).as_deref(), Some("sk-ds-2222"));
}

#[test]
fn test_and_fetch_models_sends_bearer_header_and_falls_back_to_chat_completions() {
    let f = Fixture::new();
    let fetch = FakeFetch::new(|r| {
        if r.url.to_lowercase().ends_with("/models") {
            Ok((404, String::new()))
        } else if r.url.to_lowercase().ends_with("/chat/completions") {
            Ok((200, r#"{"choices":[{"message":{"content":"ok"}}]}"#.into()))
        } else {
            Ok((404, String::new()))
        }
    });
    let m = f.manager();
    let p = add(&m, "opencode-go", None, None, Some("sk-opencode-secret"), Some("glm-5"), true);
    let out = m.test_and_fetch_models(p.id, &fetch, &CancellationToken::new()).unwrap();
    assert!(out.success, "{}", out.message);
    assert!(out.models.iter().any(|m| m == "glm-5"));
    let reqs = fetch.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].url, "https://opencode.ai/inference/go/v1/models");
    assert_eq!(reqs[0].header("Authorization"), Some("Bearer sk-opencode-secret"));
    assert_eq!(reqs[1].url, "https://opencode.ai/inference/go/openai/v1/chat/completions");
    assert_eq!(reqs[1].header("Authorization"), Some("Bearer sk-opencode-secret"));
    assert_eq!(reqs[1].method, "POST");
    assert_eq!(
        reqs[1].body.as_deref(),
        Some(r#"{"model":"glm-5","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"stream":false}"#)
    );
    assert_eq!(reqs[1].header("Content-Type"), Some("application/json; charset=utf-8"));
    assert!(out.message.starts_with("Bağlantı başarılı (opencode.ai · "), "{}", out.message);
    assert!(!m.is_testing());
}

#[test]
fn cloud_providers_never_overwrite_user_lan_settings_on_view_model_or_lan_profile() {
    let f = Fixture::new();
    let m = f.manager();
    let mut vm = ProviderViewState {
        lan: LegacyLanSettings {
            enabled: true,
            url: "http://mac-studio.local:11434".into(),
            model: "gemma4:e4b".into(),
            async_translation: true,
            models: Vec::new(),
        },
        ..Default::default()
    };
    m.ensure_migrated_from_legacy(&mut vm);
    let lans: Vec<_> = m.providers().into_iter().filter(|p| p.preset_id == "lan").collect();
    assert_eq!(lans.len(), 1);
    let lan = lans[0].clone();
    assert_eq!(lan.base_url, "http://mac-studio.local:11434");
    assert_eq!(lan.model, "gemma4:e4b");

    let _oc = add(&m, "opencode-go", None, None, Some("oc_sk_test_123456"), Some("deepseek-v4.1-flash"), true);
    let or = add(&m, "openrouter", None, None, Some("sk-or-test-654321"), Some("google/gemma-3-27b-it"), false);

    m.apply_to_view_state(&mut vm);
    m.sync_from_legacy_lan_settings(&vm.lan);
    assert_eq!(vm.lan.url, "http://mac-studio.local:11434");
    assert_eq!(vm.lan.model, "gemma4:e4b");
    assert_eq!(vm.active_provider_name.as_deref(), Some("OpenCode Go"));
    assert_eq!(vm.active_provider_model.as_deref(), Some("deepseek-v4.1-flash"));
    assert_eq!(
        f.state(),
        (true, "https://opencode.ai/inference/go/openai/v1".into(), "deepseek-v4.1-flash".into(), "oc_sk_test_123456".into())
    );

    m.set_active(Some(or.id));
    m.apply_to_view_state(&mut vm);
    m.sync_from_legacy_lan_settings(&vm.lan);
    assert_eq!(vm.lan.url, "http://mac-studio.local:11434");
    assert_eq!(vm.lan.model, "gemma4:e4b");
    assert_eq!(vm.active_provider_name.as_deref(), Some("OpenRouter"));
    assert_eq!(vm.active_provider_model.as_deref(), Some("google/gemma-3-27b-it"));
    assert_eq!(f.state().1, "https://openrouter.ai/api/v1");

    let preserved = m.providers().into_iter().find(|p| p.id == lan.id).unwrap();
    assert_eq!(preserved.base_url, "http://mac-studio.local:11434");
    assert_eq!(preserved.model, "gemma4:e4b");
}

#[test]
fn picker_selections_keep_cloud_lan_and_on_device_models_separate_and_persisted() {
    let f = Fixture::new();
    let m = f.manager();
    let mut vm = ProviderViewState {
        lan: LegacyLanSettings { enabled: true, url: "http://lan-server:11434".into(), model: "lan-model".into(), ..Default::default() },
        ..Default::default()
    };
    m.ensure_migrated_from_legacy(&mut vm);
    assert_eq!(m.providers().len(), 1);
    let lan = m.providers()[0].clone();
    let cloud = add(&m, "openrouter", None, None, Some("test-key"), Some("cloud-a"), true);
    m.update_provider_discovered_models(cloud.id, &["cloud-b".into(), "cloud-a".into(), " cloud-b ".into()], None);
    m.apply_to_view_state(&mut vm);

    let ids: Vec<String> =
        crate::selection::provider_models_for(&m.providers().into_iter().find(|p| p.id == cloud.id).unwrap(), false)
            .into_iter()
            .map(|c| c.id)
            .collect();
    assert_eq!(ids, vec!["cloud-a", "cloud-b"]);
    assert!(m.select_model(&mut vm, Some(cloud.id), "cloud-b", &[]));
    assert_eq!(vm.active_provider_model.as_deref(), Some("cloud-b"));
    assert_eq!(f.state().2, "cloud-b");
    assert_eq!(vm.lan.model, "lan-model");
    assert_eq!(vm.lan.url, "http://lan-server:11434");

    let local = "madlad-3b";
    assert!(m.select_model(&mut vm, None, local, &["madlad-3b", "nllb-1.3b"]));
    assert!(vm.active_provider_name.is_none() && !vm.lan.enabled);
    assert!(!f.state().0);
    assert_eq!(f.state().3, "");

    assert!(m.select_model(&mut vm, Some(lan.id), "lan-model", &[]));
    assert_eq!(f.state().1, "http://lan-server:11434");
    assert_eq!(f.state().2, "lan-model");

    let reloaded = f.manager();
    assert_eq!(reloaded.providers().iter().find(|p| p.id == cloud.id).unwrap().model, "cloud-b");
    assert_eq!(reloaded.providers().iter().find(|p| p.id == lan.id).unwrap().model, "lan-model");
}

#[test]
fn picker_rejects_another_providers_model_and_removed_provider() {
    let f = Fixture::new();
    let m = f.manager();
    let mut vm = ProviderViewState::default();
    let active = add(&m, "custom", None, Some("https://example.test/v1"), None, Some("cloud-model"), true);
    let removed = add(&m, "lan", None, None, None, Some("lan-model"), false);
    m.apply_to_view_state(&mut vm);
    m.remove(removed.id);

    assert!(!m.select_model(&mut vm, Some(active.id), "lan-model", &[]));
    assert!(!m.select_provider(&mut vm, Some(removed.id)));
    assert!(!crate::selection::can_select_model(&*m, Some(removed.id), "lan-model", &[]));
    assert_eq!(m.active_provider_id(), Some(active.id));
    assert_eq!(f.state().2, "cloud-model");
}

/// A fetch whose responses wait until the test releases them.
struct DeferredFetch {
    rx: Mutex<crossbeam::Receiver<(u16, String)>>,
}

mod crossbeam {
    pub use std::sync::mpsc::{Receiver, Sender, channel};
}

impl HttpFetch for DeferredFetch {
    fn fetch(&self, _request: HttpRequest, _cancel: &CancellationToken) -> Result<(u16, String), Option<String>> {
        self.rx.lock().recv().map_err(|_| None)
    }
}

#[test]
fn model_refresh_finishing_after_provider_switch_does_not_change_selection() {
    let f = Fixture::new();
    let (tx, rx): (crossbeam::Sender<(u16, String)>, _) = crossbeam::channel();
    let fetch = Arc::new(DeferredFetch { rx: Mutex::new(rx) });
    let m = f.manager();
    let vm = Arc::new(Mutex::new(ProviderViewState {
        lan: LegacyLanSettings { url: "http://lan-server:11434".into(), model: "lan-model".into(), ..Default::default() },
        ..Default::default()
    }));
    let first = add(&m, "custom", None, Some("https://first.test/v1"), None, Some("first-model"), true);
    let second = add(&m, "custom", None, Some("https://second.test/v1"), None, Some("second-model"), false);
    // The app's subscription applies every provider notification to the view model.
    let weak = Arc::downgrade(&m);
    let vm2 = vm.clone();
    let _sub = m.subscribe(Box::new(move |_| {
        if let Some(m) = weak.upgrade() {
            m.apply_to_view_state(&mut vm2.lock());
        }
    }));
    m.apply_to_view_state(&mut vm.lock());

    let m2 = m.clone();
    let f2 = fetch.clone();
    let refresh = std::thread::spawn(move || m2.test_and_fetch_models(first.id, &*f2, &CancellationToken::new()));
    // Wait until the test is in flight.
    while !m.is_testing() {
        std::thread::yield_now();
    }
    // The view state is shared with the subscription, which applies it during the switch.
    let mut local = vm.lock().clone();
    assert!(m.select_provider(&mut local, Some(second.id)));
    tx.send((200, r#"{"data":[{"id":"first-new-model"}]}"#.into())).unwrap();
    let out = refresh.join().unwrap().unwrap();
    assert!(out.success, "{}", out.message);
    assert_eq!(m.active_provider_id(), Some(second.id));
    assert_eq!(f.state().2, "second-model");
    assert_eq!(vm.lock().active_provider_model.as_deref(), Some("second-model"));
    assert_eq!(vm.lock().lan.model, "lan-model");
    let second_now = m.providers().into_iter().find(|p| p.id == second.id).unwrap();
    assert!(!crate::selection::provider_models_for(&second_now, false).iter().any(|c| c.id == "first-new-model"));
    let first_now = m.providers().into_iter().find(|p| p.id == first.id).unwrap();
    assert!(first_now.discovered_models.contains(&"first-new-model".to_owned()));
}

#[test]
fn failed_refresh_keeps_previously_discovered_models_and_selection() {
    let f = Fixture::new();
    let fetch = FakeFetch::new(|_| Ok((401, String::new())));
    let m = f.manager();
    let p = add(&m, "openrouter", None, None, None, Some("saved-model"), true);
    m.update_provider_discovered_models(p.id, &["saved-model".into(), "other-model".into()], None);
    let out = m.test_and_fetch_models(p.id, &fetch, &CancellationToken::new()).unwrap();
    assert!(!out.success);
    assert_eq!(out.message, "API anahtarı geçersiz veya yetkisiz (HTTP 401).");
    assert_eq!(m.active_provider().unwrap().discovered_models, vec!["saved-model", "other-model"]);
    assert_eq!(m.active_provider().unwrap().model, "saved-model");
}

#[test]
fn open_code_v2_routes_saved_profiles_and_shows_gateway_rejection() {
    for saved in ["https://opencode.ai/zen/go/v1", "https://opencode.ai/zen/go/v1/chat/completions", "https://opencode.ai/inference/go/openai/v1"] {
        let f = Fixture::new();
        let f = &f;
        let fetch = FakeFetch::new(|r| {
            if r.method == "GET" {
                Ok((404, String::new()))
            } else {
                Ok((400, r#"{"error":{"message":"Global region required (sk-private)"}}"#.into()))
            }
        });
        let m = f.manager();
        let p = add(&m, "opencode-go", None, Some(saved), Some("sk-private"), Some("deepseek-v4.1-flash"), true);
        let out = m.test_and_fetch_models(p.id, &fetch, &CancellationToken::new()).unwrap();
        assert!(!out.success);
        assert!(out.message.contains("HTTP 400: Global region required"), "{}", out.message);
        assert!(!out.message.contains("sk-private"));
        let reqs = fetch.requests();
        assert_eq!(reqs[0].url, "https://opencode.ai/inference/go/v1/models");
        assert_eq!(reqs[1].url, "https://opencode.ai/inference/go/openai/v1/chat/completions");
        for r in &reqs {
            assert_eq!(r.headers.iter().filter(|(k, _)| k == "x-opencode-session").count(), 1);
            assert_eq!(r.header("User-Agent"), Some("SubtitleAdder/1.0"));
            assert_eq!(r.header("Authorization"), Some("Bearer sk-private"));
        }
        assert_eq!(reqs[0].header("x-opencode-session"), reqs[1].header("x-opencode-session"));
        assert_eq!(reqs[0].header("x-opencode-session"), Some(m.instance_id()));
        let unchanged = m.providers();
        assert_eq!(unchanged.len(), 1);
        assert_eq!(unchanged[0].base_url, saved);
        assert_eq!(unchanged[0].model, "deepseek-v4.1-flash");
        assert_eq!(key_of(&m, p.id).as_deref(), Some("sk-private"));
    }
}

// MARK: key encryption and migration (rule 5)

const CSHARP_FILE: &str = "{\r\n  \"activeProviderId\": \"1bed0ce2-2d54-4a5d-a9fa-52fd7ad76929\",\r\n  \"providers\": [\r\n    {\r\n      \"id\": \"1bed0ce2-2d54-4a5d-a9fa-52fd7ad76929\",\r\n      \"name\": \"Yerel A\\u011F (Ollama / LM Studio)\",\r\n      \"presetId\": \"lan\",\r\n      \"baseUrl\": \"http://192.168.68.58:11434\",\r\n      \"apiKey\": \"\",\r\n      \"model\": \"gemma4:12b\",\r\n      \"asyncTranslation\": true,\r\n      \"discoveredModels\": [\r\n        \"gemma4:e2b\",\r\n        \"qwen3.5:4b\"\r\n      ]\r\n    },\r\n    {\r\n      \"id\": \"adf244d7-d14a-4a40-abf1-2da9a43ba655\",\r\n      \"name\": \"OpenCode Go\",\r\n      \"presetId\": \"opencode-go\",\r\n      \"baseUrl\": \"https://opencode.ai/inference/go/openai/v1\",\r\n      \"apiKey\": \"oc_plaintext_key_0123456789abcdef\",\r\n      \"model\": \"glm-5.3-flash\",\r\n      \"asyncTranslation\": true,\r\n      \"discoveredModels\": []\r\n    }\r\n  ]\r\n}";

const OC_ID: &str = "adf244d7-d14a-4a40-abf1-2da9a43ba655";

#[test]
fn plaintext_key_in_a_csharp_file_is_migrated_and_scrubbed() {
    let f = Fixture::new();
    std::fs::write(f.file(), CSHARP_FILE).unwrap();
    let m = f.manager();
    let oc = Uuid::parse_str(OC_ID).unwrap();
    assert_eq!(m.providers().len(), 2);
    assert_eq!(key_of(&m, oc).as_deref(), Some("oc_plaintext_key_0123456789abcdef"));
    assert_eq!(m.masked_key(oc), Ok(mask_key("oc_plaintext_key_0123456789abcdef")));
    // Sidecar holds a blob; the providers file has an empty key; no plaintext anywhere on disk.
    assert!(f.dir.path().join("translation-provider-keys.json").exists());
    assert!(f.dir.path().join("translation-provider-keys.dpapi").exists());
    let text = f.text();
    assert!(text.contains("\"apiKey\": \"\""));
    assert!(!f.all_text().contains("oc_plaintext_key"));
    // Same C# shape: CRLF, escapes, key order, lowercase UUIDs, active kept.
    assert!(text.starts_with("{\r\n  \"activeProviderId\": \"1bed0ce2-2d54-4a5d-a9fa-52fd7ad76929\",\r\n"));
    assert!(text.contains("\"name\": \"Yerel A\\u011F (Ollama / LM Studio)\""));
    let expected_scrubbed = CSHARP_FILE.replace("oc_plaintext_key_0123456789abcdef", "");
    assert_eq!(text, expected_scrubbed, "the rewrite is byte-identical to the C# writer apart from the key");
    // A relaunch reads the key back from the sidecar.
    assert_eq!(key_of(&f.manager(), oc).as_deref(), Some("oc_plaintext_key_0123456789abcdef"));
}

#[test]
fn plaintext_reentered_by_csharp_later_wins_over_the_old_blob() {
    let f = Fixture::new();
    std::fs::write(f.file(), CSHARP_FILE).unwrap();
    drop(f.manager());
    // The C# app runs again: it sees "apiKey": "" and the user re-enters another key there.
    let text = f.text().replace("\"apiKey\": \"\",\r\n      \"model\": \"glm-5.3-flash\"", "\"apiKey\": \"oc_new_key_reentered_9999\",\r\n      \"model\": \"glm-5.3-flash\"");
    std::fs::write(f.file(), text).unwrap();
    let m = f.manager();
    assert_eq!(key_of(&m, Uuid::parse_str(OC_ID).unwrap()).as_deref(), Some("oc_new_key_reentered_9999"));
    assert!(!f.all_text().contains("oc_new_key_reentered"));
}

#[test]
fn csharp_app_saving_empty_keys_keeps_the_encrypted_ones() {
    let f = Fixture::new();
    std::fs::write(f.file(), CSHARP_FILE).unwrap();
    drop(f.manager());
    // The C# app rewrites the file (keys empty, a model changed).
    std::fs::write(f.file(), f.text().replace("glm-5.3-flash", "kimi-k3.1-pro")).unwrap();
    let m = f.manager();
    let oc = Uuid::parse_str(OC_ID).unwrap();
    assert_eq!(m.providers().iter().find(|p| p.id == oc).unwrap().model, "kimi-k3.1-pro");
    assert_eq!(key_of(&m, oc).as_deref(), Some("oc_plaintext_key_0123456789abcdef"));
}

#[test]
fn orphan_blobs_are_pruned() {
    let f = Fixture::new();
    let m = f.manager();
    let keep = add(&m, "openai", None, None, Some("sk-keep-1234"), None, false);
    let gone = add(&m, "groq", None, None, Some("sk-gone-5678"), None, false);
    drop(m);
    // The C# app deletes `gone` behind our back.
    let text = f.text();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut v = v;
    v["providers"].as_array_mut().unwrap().retain(|p| p["id"] != gone.id.hyphenated().to_string());
    std::fs::write(f.file(), v.to_string()).unwrap();
    let m = WinProviderManager::open(f.dir.path(), None);
    assert_eq!(m.providers().len(), 1);
    let sidecar = std::fs::read_to_string(f.dir.path().join("translation-provider-keys.json")).unwrap();
    assert!(sidecar.contains(&keep.id.hyphenated().to_string()));
    assert!(!sidecar.contains(&gone.id.hyphenated().to_string()));
}

#[test]
fn a_damaged_providers_file_never_prunes_the_keys() {
    let f = Fixture::new();
    let m = f.manager();
    let p = add(&m, "openai", None, None, Some("sk-survive-1234"), None, true);
    drop(m);
    let good = f.text();
    std::fs::write(f.file(), "{\"providers\": [").unwrap();
    let m = WinProviderManager::open(f.dir.path(), None);
    assert!(m.providers().is_empty());
    assert_eq!(m.load_problem(), Some(ProviderLoadProblem::Damaged));
    let sidecar = std::fs::read_to_string(f.dir.path().join("translation-provider-keys.json")).unwrap();
    assert!(sidecar.contains(&p.id.hyphenated().to_string()), "keys survive a damaged providers file");
    drop(m);
    // Once the file is readable again the key is still there.
    std::fs::write(f.file(), good).unwrap();
    let m = WinProviderManager::open(f.dir.path(), None);
    match m.read_key(p.id) {
        ProviderKeyRead::Found(k) => assert_eq!(k.expose(), "sk-survive-1234"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn tampered_blob_reads_unavailable_without_crash_and_is_kept() {
    let f = Fixture::new();
    let m = f.manager();
    let p = add(&m, "openai", None, None, Some("sk-tamper-0000"), None, true);
    drop(m);
    let path = f.dir.path().join("translation-provider-keys.json");
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entry = p.id.hyphenated().to_string();
    let blob = v["keys"][&entry].as_str().unwrap().to_owned();
    let mut chars: Vec<char> = blob.chars().collect();
    chars[20] = if chars[20] == 'A' { 'B' } else { 'A' };
    v["keys"][&entry] = chars.into_iter().collect::<String>().into();
    std::fs::write(&path, v.to_string()).unwrap();
    let m = f.manager();
    assert!(matches!(m.read_key(p.id), ProviderKeyRead::Unavailable));
    assert_eq!(m.masked_key(p.id), Err(()));
    assert_eq!(f.state().3, "", "an unreadable key is not sent");
    // Unrelated edits never delete the unreadable blob.
    m.update_model(p.id, "gpt-4.1-mini");
    assert!(std::fs::read_to_string(&path).unwrap().contains(&entry));
    // Re-entering the key replaces it.
    assert!(m.set_key(p.id, "sk-fresh-1111"));
    assert_eq!(key_of(&f.manager(), p.id).as_deref(), Some("sk-fresh-1111"));
}

#[test]
fn keys_never_appear_in_written_files_or_debug_output() {
    let f = Fixture::new();
    let m = f.manager();
    let p = add(&m, "openai", None, None, Some("sk-grep-me-424242"), Some("gpt-4o-mini"), true);
    m.update_model(p.id, "gpt-4.1-mini");
    assert!(!f.all_text().contains("sk-grep-me"));
    assert!(!format!("{:?}", m.read_key(p.id)).contains("sk-grep-me"));
    assert!(!format!("{:?}", m.active_external_llm()).contains("sk-grep-me"));
    assert!(!format!("{:?}", f.launch.get()).contains("sk-grep-me"));
    assert_eq!(m.active_external_llm().unwrap().llm.api_key, "sk-grep-me-424242");
}

#[test]
fn update_with_none_keeps_and_blank_removes_the_key() {
    let f = Fixture::new();
    let m = f.manager();
    let p = add(&m, "openai", None, None, Some("sk-old-1111"), None, true);
    assert!(m.update(p.clone(), None));
    assert_eq!(key_of(&m, p.id).as_deref(), Some("sk-old-1111"));
    assert!(m.update(p.clone(), Some("   ")));
    assert!(matches!(m.read_key(p.id), ProviderKeyRead::Absent));
    assert!(!m.update(TranslationProviderProfile { id: Uuid::new_v4(), ..p.clone() }, Some("x")));
    assert!(!m.set_key(Uuid::new_v4(), "sk-orphan"));
}

// MARK: file format and safety

#[test]
fn reads_case_insensitively_with_null_defaults_and_fresh_ids() {
    let f = Fixture::new();
    std::fs::write(
        f.file(),
        "\u{feff}{\"ACTIVEPROVIDERID\":null,\"Providers\":[{\"Name\":null,\"PRESETID\":\"lan\",\"baseurl\":\"http://h:1\",\"ApiKey\":null,\"Model\":\"m\",\"DiscoveredModels\":null,\"unknown\":5},null]}",
    )
    .unwrap();
    let m = f.manager();
    let ps = m.providers();
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].name, "");
    assert_eq!(ps[0].preset_id, "lan");
    assert_eq!(ps[0].base_url, "http://h:1");
    assert!(ps[0].async_translation);
    assert!(ps[0].discovered_models.is_empty());
    assert_eq!(m.load_problem(), None);
    // `null` for a value-type field, or a Guid outside the strict `D` form, fails the whole document in C#.
    let (dir2, launch2) = f.sibling();
    let file2 = dir2.path().join(FILE_NAME);
    for bad in [
        r#"{"providers":[{"asyncTranslation":null}]}"#,
        r#"{"providers":[{"id":null}]}"#,
        r#"{"providers":[{"id":"{1bed0ce2-2d54-4a5d-a9fa-52fd7ad76929}"}]}"#,
        r#"{"providers":[{"id":"1bed0ce22d544a5da9fa52fd7ad76929"}]}"#,
        r#"{"providers":[{"id":" 1bed0ce2-2d54-4a5d-a9fa-52fd7ad76929"}]}"#,
    ] {
        std::fs::write(&file2, bad).unwrap();
        let m2 = WinProviderManager::with_launch_state(
            file2.clone(),
            EncryptedSecretStore::for_app_data(dir2.path()),
            None,
            launch2.clone(),
        );
        assert_eq!(m2.load_problem(), Some(ProviderLoadProblem::Damaged), "{bad}");
    }
    std::fs::write(&file2, r#"{"providers":[{"id":"1BED0CE2-2D54-4A5D-A9FA-52FD7AD76929"}]}"#).unwrap();
    let upper = WinProviderManager::with_launch_state(file2, EncryptedSecretStore::for_app_data(dir2.path()), None, launch2);
    assert_eq!(upper.providers()[0].id, Uuid::parse_str("1bed0ce2-2d54-4a5d-a9fa-52fd7ad76929").unwrap());
}

#[test]
fn a_lan_sync_that_changes_nothing_notifies_without_writing() {
    // C# `SyncFromLegacyLanSettings` with a LAN address, no `lan` profile and the section off: no save, but the
    // engine state is synced and listeners are notified.
    let f = Fixture::new();
    std::fs::write(f.file(), "{broken").unwrap();
    let m = f.manager();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = count.clone();
    let _s = m.subscribe(Box::new(move |_| {
        c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    m.sync_from_legacy_lan_settings(&LegacyLanSettings { enabled: false, url: "http://h:1".into(), ..Default::default() });
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(f.text(), "{broken", "nothing was saved");
    assert!(f.aside_files().is_empty());
    m.sync_from_legacy_lan_settings(&LegacyLanSettings { enabled: true, url: "http://h:1".into(), ..Default::default() });
    assert_eq!(m.providers().len(), 1);
    assert_eq!(f.aside_files().len(), 1, "the first real save sets the damaged file aside");
}

#[test]
fn a_reload_keeps_the_state_when_the_file_is_gone_unreadable_or_null() {
    // C# `LoadFromDisk` returns early for a missing file and for JSON `null`; a parse error empties the list.
    let f = Fixture::new();
    let m = f.manager();
    let p = add(&m, "openai", None, None, Some("sk-keep-0000"), None, true);
    std::fs::remove_file(f.file()).unwrap();
    m.reload();
    assert_eq!(m.providers().len(), 1);
    assert_eq!(m.active_provider_id(), Some(p.id));
    assert_eq!(m.load_problem(), None);
    std::fs::write(f.file(), "null").unwrap();
    m.reload();
    assert_eq!(m.providers().len(), 1);
    assert!(f.state().0, "the engine state still has the active provider");
    std::fs::write(f.file(), "{broken").unwrap();
    m.reload();
    assert!(m.providers().is_empty());
    assert_eq!(m.load_problem(), Some(ProviderLoadProblem::Damaged));
    assert!(!f.state().0);
}

#[test]
fn an_unknown_active_id_is_treated_as_none() {
    let f = Fixture::new();
    std::fs::write(f.file(), r#"{"activeProviderId":"e621e1f8-c36c-495a-93fc-0c247a3e6e5f","providers":[]}"#).unwrap();
    assert_eq!(f.manager().active_provider_id(), None);
}

#[test]
fn a_corrupt_file_is_set_aside_before_the_first_save() {
    let f = Fixture::new();
    let broken = r#"{"activeProviderId": null, "providers": [{"id": "x", "na"#;
    std::fs::write(f.file(), broken).unwrap();
    let m = f.manager();
    assert!(m.providers().is_empty());
    assert_eq!(m.load_problem(), Some(ProviderLoadProblem::Damaged));
    assert_eq!(f.text(), broken, "loading never touches the file");
    let added = add(&m, "openai", None, None, Some("sk-new-1"), None, true);
    let aside = f.aside_files();
    assert_eq!(aside.len(), 1);
    assert_eq!(std::fs::read_to_string(f.dir.path().join(&aside[0])).unwrap(), broken);
    assert_eq!(f.manager().providers(), vec![added.clone()]);
    m.update_model(added.id, "gpt-4.1-mini");
    assert_eq!(f.aside_files().len(), 1, "only the first save after damage makes a copy");
    // A type error anywhere fails the whole document, as in C#.
    let (dir2, launch2) = f.sibling();
    std::fs::write(dir2.path().join(FILE_NAME), r#"{"providers":[{"name":5}]}"#).unwrap();
    let m2 = WinProviderManager::with_launch_state(
        dir2.path().join(FILE_NAME),
        EncryptedSecretStore::for_app_data(dir2.path()),
        None,
        launch2,
    );
    assert_eq!(m2.load_problem(), Some(ProviderLoadProblem::Damaged));
}

#[test]
fn add_canonicalizes_preset_and_fills_defaults_in_the_current_language() {
    let f = Fixture::new();
    let m = f.manager();
    let p = add(&m, "LAN", Some("  "), None, None, None, false);
    assert_eq!(p.preset_id, "lan");
    assert_eq!(p.name, "Yerel Ağ (Ollama / LM Studio)");
    assert_eq!(p.base_url, "http://127.0.0.1:11434");
    assert_eq!(p.model, "gemma4:e4b");
    let unknown = add(&m, "no-such", None, Some(" https://x.test/v1 "), None, Some(" m "), false);
    assert_eq!(unknown.preset_id, "custom");
    assert_eq!(unknown.base_url, "https://x.test/v1");
    assert_eq!(unknown.model, "m");
    // Update with a blank name falls back to the preset name; the preset and discovered models stay.
    m.update_provider_discovered_models(p.id, &["a".into()], None);
    let mut edit = p.clone();
    edit.name = " ".into();
    edit.preset_id = "openai".into();
    edit.discovered_models.clear();
    assert!(m.update(edit, None));
    let now = m.providers().into_iter().find(|x| x.id == p.id).unwrap();
    assert_eq!(now.preset_id, "lan");
    assert_eq!(now.discovered_models, vec!["a"]);
    assert_eq!(now.name, "Yerel Ağ (Ollama / LM Studio)");
}

#[test]
fn discovered_models_are_stored_verbatim_and_fill_an_empty_model() {
    let f = Fixture::new();
    let m = f.manager();
    let p = add(&m, "custom", None, Some("https://x.test/v1"), None, None, true);
    assert_eq!(p.model, "");
    m.set_discovered_models(p.id, &[" a ".into(), "A".into()]);
    let now = m.active_provider().unwrap();
    assert_eq!(now.discovered_models, vec![" a ", "A"]);
    assert_eq!(now.model, " a ");
    m.update_provider_discovered_models(p.id, &["z".into()], Some(" chosen "));
    assert_eq!(m.active_provider().unwrap().model, "chosen");
}

#[test]
fn update_model_without_a_change_is_silent() {
    let f = Fixture::new();
    let m = f.manager();
    let p = add(&m, "openai", None, None, None, Some("gpt-4o-mini"), true);
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = count.clone();
    let _s = m.subscribe(Box::new(move |_| {
        c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    m.update_model(p.id, " gpt-4o-mini ");
    m.update_model(Uuid::new_v4(), "x");
    m.set_active(Some(p.id));
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    m.update_model(p.id, "gpt-4.1-mini");
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn set_active_with_an_unknown_id_deactivates() {
    let f = Fixture::new();
    let m = f.manager();
    let p = add(&m, "openai", None, None, None, None, true);
    assert_eq!(m.active_provider_id(), Some(p.id));
    m.set_active(Some(Uuid::new_v4()));
    assert_eq!(m.active_provider_id(), None);
    assert!(!f.state().0);
}

#[test]
fn a_blank_address_is_active_but_not_an_external_server() {
    let f = Fixture::new();
    let m = f.manager();
    let p = add(&m, "custom", None, None, Some("sk"), None, true);
    assert_eq!(p.base_url, "");
    assert!(m.is_external_provider_active());
    assert!(m.active_external_llm().is_none());
    assert!(!f.state().0);
}

// MARK: broker

#[test]
fn other_instances_reload_and_own_events_are_ignored() {
    let f = Fixture::new();
    let bus: Arc<dyn SyncBus> = Arc::new(TestBus::default());
    let a = f.manager_on(bus.clone());
    let b = f.manager_on(bus.clone());
    let changes = Arc::new(Mutex::new(Vec::new()));
    let ca = changes.clone();
    let _s = a.subscribe(Box::new(move |c| ca.lock().push(c)));
    let p = add(&b, "openai", None, None, Some("sk-shared-777"), None, true);
    assert_eq!(a.providers().len(), 1, "a reloaded b's save");
    assert_eq!(key_of(&a, p.id).as_deref(), Some("sk-shared-777"));
    assert_eq!(a.active_provider_id(), Some(p.id));
    assert_eq!(*changes.lock(), vec![ProviderChange::External]);
    // a's own publish does not make a reload itself (no External), b reloads.
    changes.lock().clear();
    a.update_model(p.id, "gpt-4.1-nano");
    assert_eq!(*changes.lock(), vec![ProviderChange::Providers]);
    assert_eq!(b.providers()[0].model, "gpt-4.1-nano");
}

#[test]
fn events_for_another_file_are_ignored() {
    let f = Fixture::new();
    let (other, other_launch) = f.sibling();
    let bus: Arc<dyn SyncBus> = Arc::new(TestBus::default());
    let a = f.manager_on(bus.clone());
    let b = WinProviderManager::with_launch_state(
        other.path().join(FILE_NAME),
        EncryptedSecretStore::for_app_data(other.path()),
        Some(bus.clone()),
        other_launch,
    );
    add(&b, "openai", None, None, None, None, true);
    assert!(a.providers().is_empty());
}

#[test]
fn published_events_carry_no_secret_and_the_csharp_wire_shape() {
    let f = Fixture::new();
    let bus = Arc::new(TestBus::default());
    let m = f.manager_on(bus.clone());
    add(&m, "openai", None, None, Some("sk-wire-31337"), None, true);
    let events = bus.published.lock().clone();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.topic, "providers");
    assert_eq!(e.kind, sa_sync::EventKind::ProvidersUpdated);
    assert_eq!(e.sender_instance_id, m.instance_id());
    assert_eq!(e.scope_path, m.file_path().to_string_lossy());
    let wire = serde_json::to_string(e).unwrap();
    assert!(wire.contains("\"kind\":11"));
    assert!(!wire.contains("sk-wire"));
}

// MARK: connection-test hardening (UNIFIED.md §3.7)

#[test]
fn key_over_plain_http_to_a_public_host_is_refused_before_any_request() {
    let f = Fixture::new();
    let fetch = FakeFetch::new(|_| Ok((200, "{}".into())));
    let m = f.manager();
    let out = m
        .test_draft("custom", "http://api.example.com/v1", "sk-secret", None, &fetch, &CancellationToken::new())
        .unwrap();
    assert!(!out.success);
    assert_eq!(out.message, "API anahtarı şifrelenmemiş bağlantıyla (http) gönderilmez; adresi https:// ile girin.");
    assert!(fetch.requests().is_empty());
    // Without a key, or to a LAN host, plain http is fine.
    let out = m.test_draft("custom", "http://192.168.1.50:1234/v1", "sk-secret", None, &fetch, &CancellationToken::new()).unwrap();
    assert!(out.success, "{}", out.message);
    let out = m.test_draft("custom", "http://api.example.com/v1", "", None, &fetch, &CancellationToken::new()).unwrap();
    assert!(out.success);
}

#[test]
fn draft_test_rejects_blank_addresses_and_reports_unreachable_servers() {
    let f = Fixture::new();
    let m = f.manager();
    let fetch = FakeFetch::new(|_| Err(Some("No such host is known.".into())));
    let out = m.test_draft("openai", "   ", "", None, &fetch, &CancellationToken::new()).unwrap();
    assert_eq!(out.message, "Lütfen geçerli bir sunucu adresi girin.");
    assert_eq!(out.models, vec!["gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano"]);
    let out = m.test_draft("custom", "https://nowhere.test", "", Some("mine"), &fetch, &CancellationToken::new()).unwrap();
    assert!(!out.success);
    assert_eq!(out.message, "Sunucuya ulaşılamadı: No such host is known.");
    assert_eq!(out.models, vec!["mine"]);
    let reqs = fetch.requests();
    assert_eq!(reqs.iter().map(|r| r.url.as_str()).collect::<Vec<_>>(), vec![
        "https://nowhere.test/v1/models",
        "https://nowhere.test/models",
        "https://nowhere.test/api/tags",
        "https://nowhere.test/v1/chat/completions"
    ]);
    assert!(reqs.iter().all(|r| r.header("Authorization").is_none() && r.header("x-opencode-session").is_none()));
}

#[test]
fn lan_discovery_drops_suggestions_and_counts_merged_models() {
    let f = Fixture::new();
    let m = f.manager();
    let fetch = FakeFetch::new(|r| {
        if r.url.ends_with("/v1/models") {
            Ok((200, r#"{"models":[{"name":"qwen3:14b"},{"name":"nomic-embed-text"}]}"#.into()))
        } else {
            Ok((404, String::new()))
        }
    });
    let out = m.test_draft("lan", "127.0.0.1:11434", "", Some("gemma4:e4b"), &fetch, &CancellationToken::new()).unwrap();
    assert!(out.success);
    assert_eq!(out.models, vec!["gemma4:e4b", "qwen3:14b"]);
    assert!(out.message.starts_with("Bağlantı başarılı (127.0.0.1 · "), "{}", out.message);
    assert!(out.message.ends_with(" ms · 2 uygun model bulundu)"), "{}", out.message);
}

#[test]
fn lan_section_test_sends_no_key_and_updates_only_the_lan_profile() {
    let f = Fixture::new();
    let m = f.manager();
    let lan = add(&m, "lan", None, Some("http://127.0.0.1:11434"), None, Some(""), false);
    let cloud = add(&m, "openrouter", None, None, Some("sk-cloud-key"), Some("cloud-a"), true);
    let fetch = FakeFetch::new(|r| {
        if r.url.ends_with("/api/tags") {
            Ok((200, r#"{"models":[{"name":"gemma4:e4b"}]}"#.into()))
        } else {
            Ok((404, String::new()))
        }
    });
    let out = test_lan_server("http://127.0.0.1:11434/api/chat", &fetch, &CancellationToken::new()).unwrap();
    assert!(out.success);
    assert_eq!(out.models, vec!["gemma4:e4b"]);
    let reqs = fetch.requests();
    assert_eq!(reqs[0].url, "http://127.0.0.1:11434/api/tags");
    assert!(reqs.iter().all(|r| r.header("Authorization").is_none()));
    m.update_lan_discovered_models(&out.models, None);
    let now = m.providers();
    assert_eq!(now.iter().find(|p| p.id == lan.id).unwrap().discovered_models, vec!["gemma4:e4b"]);
    assert_eq!(now.iter().find(|p| p.id == lan.id).unwrap().model, "gemma4:e4b");
    assert!(now.iter().find(|p| p.id == cloud.id).unwrap().discovered_models.is_empty());
    let invalid = test_lan_server("  ", &fetch, &CancellationToken::new()).unwrap();
    assert_eq!(invalid.message, "Lütfen geçerli bir sunucu adresi girin.");
}

#[test]
fn a_cancelled_test_reports_cancellation_and_resets_testing() {
    let f = Fixture::new();
    let m = f.manager();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let fetch = FakeFetch::new(move |_| {
        c2.cancel();
        Err(None)
    });
    let p = add(&m, "openai", None, None, None, None, true);
    assert_eq!(m.test_and_fetch_models(p.id, &fetch, &cancel), Err(TestCancelled));
    assert!(!m.is_testing());
    let missing = m.test_and_fetch_models(Uuid::new_v4(), &fetch, &CancellationToken::new()).unwrap();
    assert_eq!(missing.message, "Sağlayıcı bulunamadı.");
}

#[test]
fn english_messages_follow_the_ui_language() {
    let f = Fixture::new();
    let m = f.manager();
    let fetch = FakeFetch::new(|_| Ok((403, String::new())));
    sa_core::l10n::LanguageState::shared().set_language(sa_core::UiLanguage::En);
    let out = m.test_draft("openai", "https://api.openai.com/v1", "k", None, &fetch, &CancellationToken::new()).unwrap();
    sa_core::l10n::LanguageState::shared().set_language(sa_core::UiLanguage::Tr);
    assert_eq!(out.message, "Invalid or unauthorized API key (HTTP 403).");
}
