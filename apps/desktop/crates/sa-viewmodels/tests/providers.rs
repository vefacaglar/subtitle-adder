//! Ports of `ProviderTests`, `ProviderKeyTests`, `TranslationModelPickerTests` (Swift) and the Windows
//! `TranslationProviderTests` VM-level cases (rule 5: keys are stored encrypted, never in memory only, never in the
//! providers file; no Keychain wording anywhere).

mod common;

use std::rc::Rc;
use std::sync::Arc;

use common::*;
use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::settings::{SettingsStore, keys};
use sa_enginekit::external_llm::{HttpFetch, HttpRequest};
use sa_viewmodels::providers_ui::*;
use sa_viewmodels::types::{JobStatus, JobStatusKind};

/// Answers every request with `(status, body)` and records it.
struct FakeFetch {
    requests: Mutex<Vec<HttpRequest>>,
    answer: Mutex<Result<(u16, String), Option<String>>>,
}

impl FakeFetch {
    fn new(answer: Result<(u16, String), Option<String>>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::new(vec![]), answer: Mutex::new(answer) })
    }
}

impl HttpFetch for FakeFetch {
    fn fetch(&self, request: HttpRequest, _cancel: &CancellationToken) -> Result<(u16, String), Option<String>> {
        self.requests.lock().push(request);
        self.answer.lock().clone()
    }
}

const MODELS_JSON: &str = r#"{"data":[{"id":"gemma-3"},{"id":"text-embedding-3-small"},{"id":"llama-4"}]}"#;

fn model_with(env: &Env, fetch: Arc<FakeFetch>) -> ProvidersModel {
    ProvidersModel::with_tester(env.services(), EngineKitConnectionTester::new(fetch))
}

fn add(env: &Env, p: sa_providers::TranslationProviderProfile, key: Option<&str>, active: bool) -> uuid::Uuid {
    use sa_providers::ProviderStore;
    env.providers.add(p, key, active).id
}

#[test]
fn key_captions() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    assert_eq!(ProviderKey::Loading.caption().text(), "Anahtar okunuyor…");
    assert_eq!(ProviderKey::Absent.caption().text(), "Anahtar yok");
    assert_eq!(ProviderKey::Unreadable.caption().text(), "Anahtar okunamadı");
    assert_eq!(ProviderKey::Present(sa_core::secrets::SecretString::new("sk-saved")).caption().text(), "••••aved");
    assert_eq!(ProviderKey::from_read(sa_providers::ProviderKeyRead::Found(sa_core::secrets::SecretString::new("  "))), ProviderKey::Absent);
}

#[test]
fn launch_never_reads_keys_on_the_ui_thread() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let id = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "gpt-4o-mini", false), Some("sk-saved"), true);
    let m = ProvidersModel::new(env.services());
    m.start_loading_keys();
    assert_eq!(m.provider_key(id), ProviderKey::Loading);
    assert_eq!(m.providers()[0].1.text(), "Anahtar okunuyor…");
    env.eventually("loaded", || m.provider_key(id).is_settled());
    assert_eq!(m.key_caption(id).text(), "••••aved");
}

#[test]
fn a_refused_read_is_not_treated_as_no_key_and_a_known_key_survives_a_later_failed_read() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let id = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk-secret-1234"), true);
    env.providers.st.lock().unreadable.insert(id);
    let m = ProvidersModel::new(env.services());
    m.start_loading_keys();
    env.eventually("read", || m.provider_key(id) == ProviderKey::Unreadable);
    assert_eq!(m.key_caption(id).text(), "Anahtar okunamadı");
    assert!(env.providers.st.lock().keys.contains_key(&id), "nothing removed");
    env.providers.st.lock().unreadable.clear();
    m.load_keys(vec![id]);
    env.eventually("recovered", || matches!(m.provider_key(id), ProviderKey::Present(_)));
    env.providers.st.lock().unreadable.insert(id);
    m.load_keys(vec![id]);
    env.pump();
    env.ui.run_until(std::time::Duration::from_millis(100), || false);
    assert!(matches!(m.provider_key(id), ProviderKey::Present(_)), "a known key survives a failed read");
}

#[test]
fn editing_without_touching_the_key_keeps_it_and_a_typed_key_replaces_it() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let id = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk-old"), true);
    let m = ProvidersModel::new(env.services());
    m.start_loading_keys();
    env.eventually("loaded", || m.provider_key(id).is_settled());
    let mut d = m.begin_edit(id).unwrap();
    assert!(d.can_remove_stored_key());
    d.set_name("Ofis");
    assert_eq!(d.key_to_save(), None);
    m.save(d).unwrap();
    assert_eq!(env.providers.st.lock().keys.get(&id).map(String::as_str), Some("sk-old"));
    let mut d = m.begin_edit(id).unwrap();
    d.set_api_key("  sk-typed \n");
    assert_eq!(d.key_to_save().map(|k| k.expose().to_owned()).as_deref(), Some("  sk-typed \n"), "raw field");
    let r = m.save(d).unwrap();
    assert!(r.key_stored);
    assert_eq!(env.providers.st.lock().keys.get(&id).map(String::as_str), Some("sk-typed"), "stored trimmed");
    let mut d = m.begin_edit(id).unwrap();
    d.remove_stored_key();
    assert!(d.removes_stored_key());
    m.save(d).unwrap();
    assert!(!env.providers.st.lock().keys.contains_key(&id));
    assert_eq!(m.provider_key(id), ProviderKey::Absent);
    assert_eq!(m.key_caption(id).text(), "Anahtar yok");
}

#[test]
fn a_new_provider_saves_what_the_field_holds_and_a_refused_write_is_reported() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let m = ProvidersModel::new(env.services());
    let mut d = m.begin_add("openai");
    assert_eq!(d.name, "OpenAI");
    assert_eq!(d.base_url, "https://api.openai.com/v1");
    d.set_api_key("sk-new-key-123");
    let r = m.save(d).unwrap();
    assert!(r.key_stored);
    assert_eq!(env.providers.st.lock().active, Some(r.profile.id), "saved and activated");
    assert_eq!(env.providers.st.lock().keys.get(&r.profile.id).map(String::as_str), Some("sk-new-key-123"));
    assert_eq!(r.message, "OpenAI kaydedildi ve etkinleştirildi.");
    env.providers.st.lock().refuse_writes = true;
    let mut d = m.begin_add("openrouter");
    d.set_api_key("sk-x");
    assert!(!m.save(d).unwrap().key_stored);
    let d = m.begin_add("lan");
    assert!(m.save(d).unwrap().key_stored, "a blank key counts as stored");
}

#[test]
fn a_blank_address_is_reported() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let m = ProvidersModel::new(env.services());
    let mut d = m.begin_add("custom");
    d.set_base_url("   ");
    assert_eq!(m.save(d), Err("Lütfen geçerli bir sunucu adresi girin.".to_owned()));
}

#[test]
fn a_draft_follows_its_preset_and_forgets_models_of_an_old_address() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let m = ProvidersModel::new(env.services());
    let mut d = m.begin_add("openai");
    assert!(!d.async_translation, "macOS: only LAN translates in the background by default");
    d.discovered_models = vec!["a".into()];
    d.set_base_url("https://api.openai.com/v1 ");
    assert_eq!(d.discovered_models, vec!["a".to_owned()], "same trimmed address keeps models");
    d.set_base_url("https://other.example/v1");
    assert!(d.discovered_models.is_empty());
    m.apply_preset(&mut d, "lan");
    assert_eq!(d.preset_id, "lan");
    assert_eq!(d.base_url, "http://127.0.0.1:11434");
    assert!(d.async_translation);
    d.set_async_translation(false);
    m.apply_preset(&mut d, "lan");
    assert!(!d.async_translation, "a chosen async flag is kept");
    let w = Env::win();
    let mw = ProvidersModel::new(w.services());
    assert!(mw.begin_add("openai").async_translation, "Windows: async on for every preset");
}

#[test]
fn a_pristine_draft_relocalizes_but_a_typed_one_is_kept() {
    let l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let m = ProvidersModel::new(env.services());
    let mut d = m.begin_add("lan");
    let tr_name = d.name.clone();
    l.set(UiLanguage::En);
    d.relocalize();
    assert_ne!(d.name, tr_name, "pristine → preset name in the new language");
    d.set_api_key("x");
    let typed = d.clone();
    l.set(UiLanguage::Tr);
    d.relocalize();
    assert_eq!(d, typed, "a typed draft is never discarded");
}

#[test]
fn a_test_sends_the_saved_key_stores_the_models_and_fills_an_empty_model() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, false);
    let fetch = FakeFetch::new(Ok((200, MODELS_JSON.into())));
    let id = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "", false), Some("sk-live"), true);
    let m = model_with(&env, fetch.clone());
    m.test_saved(id);
    assert!(m.is_testing(id));
    env.eventually("notice", || m.notice(id).is_some());
    assert!(!m.is_testing(id));
    let req = fetch.requests.lock()[0].clone();
    assert_eq!(req.url, "https://api.openai.com/v1/models");
    assert_eq!(req.header("Authorization"), Some("Bearer sk-live"));
    let n = m.notice(id).unwrap();
    assert!(!n.is_error);
    assert!(n.text().starts_with("Bağlantı başarılı (api.openai.com · "), "{}", n.text());
    assert!(n.text().ends_with(" ms · 2 uygun model bulundu)"), "{}", n.text());
    let p = env.providers.st.lock().providers[0].clone();
    assert_eq!(p.discovered_models, vec!["gemma-3".to_owned(), "llama-4".to_owned()], "embedding model dropped");
    assert_eq!(p.model, "gemma-3", "empty model takes the first");
}

#[test]
fn a_test_keeps_the_model_the_user_chose_and_a_refused_key_is_reported() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, false);
    let fetch = FakeFetch::new(Ok((200, MODELS_JSON.into())));
    let id = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "mine", false), Some("sk-live"), true);
    let m = model_with(&env, fetch.clone());
    m.test_saved(id);
    env.eventually("notice", || m.notice(id).is_some());
    assert_eq!(env.providers.st.lock().providers[0].model, "mine");
    *fetch.answer.lock() = Ok((401, "{}".into()));
    m.test_saved(id);
    env.eventually("unauthorized", || m.notice(id).is_some_and(|n| n.is_error));
    assert!(m.notice(id).unwrap().text().contains("(HTTP 401)"), "{}", m.notice(id).unwrap().text());
    assert_eq!(env.providers.st.lock().providers[0].discovered_models.len(), 2, "stored models kept");
}

#[test]
fn an_unreachable_server_is_reported() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, false);
    let fetch = FakeFetch::new(Err(Some("timed out".into())));
    let id = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk"), true);
    let m = model_with(&env, fetch);
    m.test_saved(id);
    env.eventually("notice", || m.notice(id).is_some());
    assert_eq!(m.notice(id).unwrap().text(), "Sunucuya ulaşılamadı: timed out");
}

#[test]
fn a_connection_test_with_an_unreadable_key_says_so_instead_of_testing_without_it() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, false);
    let fetch = FakeFetch::new(Ok((200, MODELS_JSON.into())));
    let id = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk"), true);
    env.providers.st.lock().unreadable.insert(id);
    let m = model_with(&env, fetch.clone());
    m.test_saved(id);
    env.eventually("notice", || m.notice(id).is_some());
    let n = m.notice(id).unwrap();
    assert!(n.is_error);
    assert_eq!(n.message.id(), "key-unreadable:OpenAI");
    assert!(n.text().contains("OpenAI"));
    assert!(!n.text().contains("Anahtar Zinciri"), "never mentions the Keychain");
    assert!(fetch.requests.lock().is_empty(), "no request");
}

#[test]
fn offline_mode_skips_connection_tests_of_cloud_providers() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, true);
    let fetch = FakeFetch::new(Ok((200, MODELS_JSON.into())));
    let cloud = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk"), true);
    let m = model_with(&env, fetch.clone());
    m.test_saved(cloud);
    assert_eq!(m.notice(cloud).unwrap().text(), "Çevrimdışı mod açık: internetteki sağlayıcılara bağlanılmaz.");
    assert!(fetch.requests.lock().is_empty());
    let lan = add(&env, provider("lan", "LAN", "http://192.168.1.5:1234", "", true), None, false);
    m.test_saved(lan);
    env.eventually("lan tested", || m.notice(lan).is_some());
    assert!(!fetch.requests.lock().is_empty(), "a LAN server is still contacted");
}

#[test]
fn only_a_server_on_the_local_network_is_contacted_by_itself() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, false);
    let fetch = FakeFetch::new(Ok((200, MODELS_JSON.into())));
    add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk"), true);
    let m = model_with(&env, fetch.clone());
    m.refresh_active_provider_models_if_needed();
    env.ui.run_until(std::time::Duration::from_millis(100), || false);
    assert!(fetch.requests.lock().is_empty(), "cloud only on request");
    let lan = add(&env, provider("lan", "LAN", "http://10.0.0.2:1234", "", true), None, false);
    m.activate(Some(lan));
    env.eventually("lan refreshed", || !fetch.requests.lock().is_empty());
}

#[test]
fn testing_a_draft_stores_nothing() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, false);
    let fetch = FakeFetch::new(Ok((200, MODELS_JSON.into())));
    let m = model_with(&env, fetch.clone());
    let mut d = m.begin_add("openai");
    d.set_api_key("sk-draft");
    let out = Rc::new(std::cell::RefCell::new(None));
    let o2 = out.clone();
    m.test_draft(&d, Box::new(move |r| *o2.borrow_mut() = Some(r)));
    env.eventually("draft tested", || out.borrow().is_some());
    let r = out.borrow().clone().unwrap();
    assert_eq!(r.models, vec!["gemma-3".to_owned(), "llama-4".to_owned()]);
    assert!(env.providers.st.lock().providers.is_empty());
    assert_eq!(fetch.requests.lock()[0].header("Authorization"), Some("Bearer sk-draft"));
}

#[test]
fn editing_or_removing_a_provider_drops_its_test_result() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, false);
    let fetch = FakeFetch::new(Ok((200, MODELS_JSON.into())));
    let id = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk"), true);
    let m = model_with(&env, fetch);
    m.test_saved(id);
    env.eventually("notice", || m.notice(id).is_some());
    let d = m.begin_edit(id).unwrap();
    m.save(d).unwrap();
    assert!(m.notice(id).is_none());
    m.delete(id);
    assert!(env.providers.st.lock().providers.is_empty());
    assert!(!env.providers.st.lock().keys.contains_key(&id), "the stored key goes with it");
}

// ----- VM-level provider behaviour

#[test]
fn the_toolbar_title_names_the_provider_and_its_model() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, _c) = env.vm();
    let check = |p: sa_providers::TranslationProviderProfile, want: &str| {
        use sa_providers::ProviderStore;
        env.providers.st.lock().providers.clear();
        env.providers.add(p, None, true);
        env.eventually(want, || vm.translation_model_name() == want);
    };
    check(provider("openai", "OpenAI", "https://api.openai.com/v1", "gpt-4o-mini", false), "OpenAI: gpt-4o-mini");
    check(provider("openai", "OpenAI", "https://api.openai.com/v1", " ", false), "OpenAI: Otomatik");
    check(provider("lan", "LAN", "http://10.0.0.2:1234", "", false), "Harici: Harici LLM");
    check(provider("lan", "LAN", "http://10.0.0.2:1234", "gemma4:e4b", false), "Harici: gemma4:e4b");
    check(provider("lan", "LAN", "http://10.0.0.2:1234", "gemma4:e4b", true), "Harici: gemma4:e4b (Asenkron)");
    check(provider("custom", "Ofis hesabı", "https://llm.example.com/v1", "", false), "Ofis hesabı: Otomatik");
}

#[test]
fn a_provider_without_a_usable_address_is_not_active() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    add(&env, provider("custom", "Boş", "http://", "", true), None, true);
    let (vm, _c) = env.vm();
    assert!(!vm.is_external_llm_active());
    assert_eq!(vm.translation_model_name(), "MADLAD-400 3B");
}

#[test]
fn only_a_provider_outside_the_local_network_is_a_cloud_service() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    add(&env, provider("openai", "Bulut", "https://api.openai.com/v1", "m", false), None, true);
    let (vm, _c) = env.vm();
    assert_eq!(vm.active_cloud_provider_name().as_deref(), Some("Bulut"));
    let env2 = Env::mac();
    add(&env2, provider("openai", "Evde", "http://192.168.1.10:8080/v1", "m", false), None, true);
    let (vm2, _c2) = env2.vm();
    assert!(vm2.active_cloud_provider_name().is_none(), "the address decides");
}

#[test]
fn offline_mode_blocks_translating_with_a_cloud_provider_but_not_with_a_lan_server() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, true);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    *env.engine.models.lock() = downloaded_models();
    let cloud = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk"), true);
    let (vm, _c) = env.vm();
    let sub = env.touch("a.srt", "1\n00:00:01,000 --> 00:00:02,000\nMerhaba\n\n");
    assert!(vm.load_subtitle_file(&sub));
    vm.translate();
    assert_eq!(vm.status().kind(), JobStatusKind::Failed);
    assert!(vm.status_text().contains("OpenAI"), "{}", vm.status_text());
    assert_eq!(vm.offline_retry_title(), "Çevrimdışı modu kapat ve çevir");
    let _ = cloud;
    let lan = add(&env, provider("lan", "LAN", "http://10.0.0.2:1234", "", false), None, true);
    env.eventually("switched", || vm.active_cloud_provider_name().is_none());
    let _ = lan;
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.finish();
}

#[test]
fn the_active_providers_address_model_and_key_reach_the_engine() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let id = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "gpt-4o-mini", false), Some("sk-old"), true);
    let (vm, _c) = env.vm();
    env.eventually("launch settings", || env.launch.get().external_llm.is_some_and(|l| l.api_key == "sk-old"));
    let l = env.launch.get().external_llm.unwrap();
    assert_eq!(l.url, "https://api.openai.com/v1");
    assert_eq!(l.model, "gpt-4o-mini");
    let sub = env.touch("a.srt", "1\n00:00:01,000 --> 00:00:02,000\nMerhaba\n\n");
    assert!(vm.load_subtitle_file(&sub));
    {
        use sa_providers::ProviderStore;
        // The key changes after the request: the translate launch reads it again.
        vm.translate();
        env.providers.set_key(id, "sk-new");
    }
    let call = env.next(CallKind::Translate);
    let key = call.record.launch.external_llm.as_ref().map(|l| l.api_key.clone());
    assert!(matches!(key.as_deref(), Some("sk-new") | Some("sk-old")));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished || vm.status().kind() == JobStatusKind::Idle);
    vm.translate();
    let call = env.next(CallKind::Translate);
    assert_eq!(call.record.launch.external_llm.as_ref().map(|l| l.api_key.as_str()), Some("sk-new"));
    call.finish();
}

#[test]
fn a_key_that_cannot_be_read_when_the_translation_starts_fails_without_sending_anything() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let id = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk"), true);
    let (vm, _c) = env.vm();
    env.providers.st.lock().unreadable.insert(id);
    let sub = env.touch("a.srt", "1\n00:00:01,000 --> 00:00:02,000\nMerhaba\n\n");
    assert!(vm.load_subtitle_file(&sub));
    vm.translate();
    env.eventually("failed", || vm.status().kind() == JobStatusKind::Failed);
    assert!(vm.status_text().contains("OpenAI"), "{}", vm.status_text());
    assert!(env.engine.calls_of(CallKind::Translate).is_empty(), "nothing sent");
}

#[test]
fn nllb_without_a_source_language_is_fine_through_an_external_server() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_string(keys::TRANSLATION_MODEL, "nllb-1.3b");
    add(&env, provider("lan", "LAN", "http://10.0.0.2:1234", "", false), None, true);
    let (vm, _c) = env.vm();
    let sub = env.touch("a.srt", "1\n00:00:01,000 --> 00:00:02,000\nMerhaba\n\n");
    assert!(vm.load_subtitle_file(&sub));
    vm.translate();
    let call = env.next(CallKind::Translate);
    assert_eq!(call.record.from, None);
    call.finish();
}

// ----- picker

struct Host(Rc<sa_viewmodels::SubtitleViewModel>);
impl PickerHost for Host {
    fn translation_model(&self) -> String {
        self.0.translation_model()
    }
    fn set_translation_model(&self, id: &str) {
        self.0.set_translation_model(id);
    }
}

#[test]
fn picker_lists_local_and_provider_models_and_selecting_activates() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, false);
    let mut p = provider("openrouter", "OR", "https://openrouter.ai/api/v1", "cloud-a", false);
    p.discovered_models = vec!["cloud-b".into(), "cloud-a".into(), " cloud-b ".into()];
    let pid = add(&env, p, Some("sk"), false);
    let (vm, catalog) = env.vm();
    let vm = Rc::new(vm);
    let providers = Rc::new(ProvidersModel::new(env.services()));
    let picker = TranslationModelPickerModel::new(env.services(), catalog.clone(), providers.clone(), Rc::new(Host(vm.clone())));
    picker.open();
    assert_eq!(picker.browsed_provider(), None);
    let local = picker.picker_rows();
    assert_eq!(local.len(), catalog.translation_models().len());
    assert!(local.iter().any(|r| r.id == "madlad-3b" && r.is_current));
    assert_eq!(picker.provider_choices()[0].name, "Yerel modeller (Cihaz üzerinde)");
    assert!(picker.browse(Some(pid)));
    let ids: Vec<String> = picker.picker_rows().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, vec!["cloud-a", "cloud-b"]);
    assert!(!picker.select("missing"));
    assert!(picker.select("cloud-b"));
    assert_eq!(env.providers.st.lock().active, Some(pid));
    assert_eq!(env.providers.st.lock().providers[0].model, "cloud-b");
    picker.browse(None);
    assert!(picker.select("nllb-1.3b"));
    assert_eq!(env.providers.st.lock().active, None, "on-device deactivates the provider");
    assert_eq!(vm.translation_model(), "nllb-1.3b");
    picker.set_search_text("zzz");
    assert!(picker.picker_rows().is_empty());
    assert_eq!(picker.empty_text().as_deref(), Some("Aramayla eşleşen model yok."));
}

#[test]
fn a_stale_picker_never_falls_back_to_on_device() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let pid = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk"), true);
    let (vm, catalog) = env.vm();
    let vm = Rc::new(vm);
    let providers = Rc::new(ProvidersModel::new(env.services()));
    let picker = TranslationModelPickerModel::new(env.services(), catalog, providers.clone(), Rc::new(Host(vm.clone())));
    picker.open();
    assert_eq!(picker.browsed_provider(), Some(pid));
    {
        use sa_providers::ProviderStore;
        env.providers.remove(pid);
    }
    assert!(!picker.select("m"));
    assert_eq!(picker.browsed_provider(), None);
    assert_eq!(picker.status_text().as_deref(), Some("Bu sağlayıcı artık yok."));
    assert!(!picker.browse(Some(pid)), "a removed provider is refused");
}

#[test]
fn picker_highlight_moves_over_enabled_rows() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, catalog) = env.vm();
    let vm = Rc::new(vm);
    let providers = Rc::new(ProvidersModel::new(env.services()));
    let picker = TranslationModelPickerModel::new(env.services(), catalog, providers, Rc::new(Host(vm.clone())));
    picker.open();
    assert_eq!(picker.highlighted_id().as_deref(), Some("madlad-3b"), "the current one");
    let rows = picker.picker_rows();
    picker.move_highlight(-100);
    assert_eq!(picker.highlighted_id(), Some(rows[0].id.clone()));
    picker.move_highlight(1);
    assert_eq!(picker.highlighted_id(), Some(rows[1].id.clone()));
    assert!(picker.choose_highlighted());
    assert_eq!(vm.translation_model(), rows[1].id);
}

#[test]
fn picker_refresh_reports_the_result_and_blocked_providers_cannot_refresh() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::OFFLINE_MODE, false);
    let fetch = FakeFetch::new(Ok((200, MODELS_JSON.into())));
    let pid = add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), Some("sk"), true);
    let (vm, catalog) = env.vm();
    let vm = Rc::new(vm);
    let providers = Rc::new(model_with(&env, fetch.clone()));
    let picker = TranslationModelPickerModel::new(env.services(), catalog, providers.clone(), Rc::new(Host(vm.clone())));
    picker.open();
    assert!(picker.can_refresh());
    picker.refresh();
    assert_eq!(picker.status_text().as_deref(), Some("Modeller yenileniyor…"));
    env.eventually("refreshed", || !picker.is_refreshing());
    assert!(picker.status_text().unwrap().starts_with("Bağlantı başarılı"), "{:?}", picker.status_text());
    assert!(picker.picker_rows().iter().any(|r| r.id == "gemma-3"));
    *fetch.answer.lock() = Ok((500, "{}".into()));
    picker.refresh();
    env.eventually("failed refresh", || !picker.is_refreshing());
    assert!(picker.status_text().unwrap().starts_with("Modeller yenilenemedi: "), "{:?}", picker.status_text());
    env.settings.set_bool(keys::OFFLINE_MODE, true);
    assert!(!picker.can_refresh(), "offline blocks a cloud provider");
    assert_eq!(picker.status_text().as_deref().is_some(), true);
    let _ = pid;
}

#[test]
fn picker_help_text_and_symbol() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, catalog) = env.vm();
    let vm = Rc::new(vm);
    let providers = Rc::new(ProvidersModel::new(env.services()));
    let picker = TranslationModelPickerModel::new(env.services(), catalog, providers, Rc::new(Host(vm)));
    assert_eq!(picker.button_symbol(), None);
    assert!(picker.help_detail().starts_with("Çeviri motoru"));
    add(&env, provider("openai", "OpenAI", "https://api.openai.com/v1", "m", false), None, true);
    assert_eq!(picker.button_symbol(), Some("cloud"));
    assert!(picker.help_detail().contains("OpenAI"));
    add(&env, provider("lan", "LAN", "http://10.0.0.2:1234", "", false), None, true);
    assert_eq!(picker.button_symbol(), Some("network"));
}
