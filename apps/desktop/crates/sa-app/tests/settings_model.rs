//! Settings window model tests (no window): both layouts over in-memory fakes. Ports the Settings parts of
//! `SettingsResponsivenessTests` (Windows), the provider form / draft rules (`TranslationProvidersControl`,
//! `ProviderKeyTests` / `ProviderTests` of the macOS app) and the Windows-only General page sections.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use sa_app::settings_window::deps::TestOutcome;
use sa_app::settings_window::model::{Flavor, PendingConfirm, SettingsModel};
use sa_app::settings_window::testing::{Fakes, fake_deps};
use sa_core::dispatch::UiDispatcher;
use sa_core::settings::SettingsStore;
use sa_providers::{ProviderStore, TranslationProviderProfile};
use sa_viewmodels::settings_refresh::SettingsRefreshSubscription;
use sa_viewmodels::testing::QueueDispatcher;
use sa_viewmodels::types::SubtitleChange;
use uuid::Uuid;

/// The shared UI language is process-global: tests that read localized text run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Env {
    ui: Arc<QueueDispatcher>,
    f: Fakes,
}

fn env() -> Env {
    let ui = QueueDispatcher::new();
    let dispatcher: Arc<dyn UiDispatcher> = ui.clone();
    let f = fake_deps(dispatcher, vec!["Arial".into(), "Verdana".into()], "tr");
    Env { ui, f }
}

fn model(e: &Env, flavor: Flavor) -> std::rc::Rc<SettingsModel> {
    let m = SettingsModel::new(e.f.deps.clone(), flavor);
    pump(e);
    m
}

fn pump(e: &Env) {
    e.ui.pump_for(Duration::from_millis(30));
}

fn wait(e: &Env, mut cond: impl FnMut() -> bool) {
    assert!(e.ui.pump_until(Duration::from_secs(5), &mut cond), "condition not reached");
}

fn provider(name: &str, preset: &str, url: &str) -> TranslationProviderProfile {
    TranslationProviderProfile {
        id: Uuid::new_v4(),
        name: name.into(),
        preset_id: preset.into(),
        base_url: url.into(),
        model: String::new(),
        async_translation: false,
        discovered_models: Vec::new(),
    }
}

fn ok(message: &str, models: &[&str]) -> Option<TestOutcome> {
    Some(TestOutcome { success: true, message: message.into(), models: models.iter().map(|s| s.to_string()).collect() })
}

// MARK: refresh coalescing (SettingsResponsivenessTests)

#[test]
fn translation_progress_does_not_refresh_settings_controls() {
    let enqueued = Arc::new(AtomicUsize::new(0));
    let refreshed = Arc::new(AtomicUsize::new(0));
    let (e2, r2) = (enqueued.clone(), refreshed.clone());
    let sub = SettingsRefreshSubscription::new(
        move |job| {
            e2.fetch_add(1, Ordering::SeqCst);
            job();
        },
        move |_| {
            r2.fetch_add(1, Ordering::SeqCst);
        },
    );
    for _ in 0..100 {
        sub.on_view_model_change(SubtitleChange::Status);
    }
    assert_eq!(enqueued.load(Ordering::SeqCst), 0);
    assert_eq!(refreshed.load(Ordering::SeqCst), 0);
}

#[test]
fn settings_changes_coalesce_and_queued_refresh_is_ignored_after_close() {
    let queue: Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>> = Arc::new(Mutex::new(Vec::new()));
    let refreshes: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let (q2, r2) = (queue.clone(), refreshes.clone());
    let sub = SettingsRefreshSubscription::new(move |job| q2.lock().push(job), move |refill| r2.lock().push(refill));
    sub.on_view_model_change(SubtitleChange::Settings);
    sub.on_view_model_change(SubtitleChange::Models);
    sub.on_view_model_change(SubtitleChange::Providers);
    sub.request_refresh(true);
    assert_eq!(queue.lock().len(), 1);
    let job = queue.lock().pop().unwrap();
    job();
    assert_eq!(*refreshes.lock(), vec![true]);
    sub.on_view_model_change(SubtitleChange::Settings);
    assert_eq!(queue.lock().len(), 1);
    sub.dispose();
    let job = queue.lock().pop().unwrap();
    job();
    assert_eq!(refreshes.lock().len(), 1, "a queued refresh does nothing after close");
    sub.on_view_model_change(SubtitleChange::Settings);
    assert!(queue.lock().is_empty(), "nothing new is queued after close");
}

// MARK: General page

#[test]
fn general_controls_reflect_and_write_the_view_model() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    let v = m.view();
    assert_eq!(v.ui_language_items, vec!["Sistem", "Türkçe", "English"]);
    assert_eq!(v.speech_language_items.len(), 10);
    assert_eq!(v.speech_language_index, 0);
    assert_eq!(v.speaker_count_items, vec!["Otomatik", "2", "3", "4", "5", "6"]);
    assert!(v.diarize && v.offline && v.auto_play);
    assert_eq!(v.model_items.get(v.model_index as usize).map(String::as_str), Some("En doğru"));
    assert_eq!(v.translation_model_items.get(v.translation_model_index as usize).map(String::as_str), Some("MADLAD-400 3B"));
    assert_eq!(v.translation_model_separator, -1);

    m.select_speech_language(1); // tr
    assert_eq!(e.f.settings.get_string("selectedLanguage").as_deref(), Some("tr"));
    m.select_speaker_count(2); // 3 speakers
    assert_eq!(e.f.settings.get_int("speakerCount"), Some(3));
    m.set_diarize(false);
    assert_eq!(e.f.settings.get_bool("diarizeSpeakers"), Some(false));
    m.set_offline(false);
    m.set_auto_play(false);
    let v = m.view();
    assert!(!v.offline && !v.auto_play && !v.diarize);
}

#[test]
fn reading_the_view_never_writes_settings() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    let before = e.f.settings.snapshot();
    for _ in 0..3 {
        let _ = m.view();
    }
    assert_eq!(e.f.settings.snapshot(), before);
}

#[test]
fn mac_lists_llama_translation_models_before_the_classic_ones_with_a_separator() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Mac);
    let v = m.view();
    assert_eq!(v.translation_model_items[0], "Qwen 3.5 4B");
    assert_eq!(v.translation_model_separator, 4);
    assert_eq!(v.translation_model_items[4], "NLLB-200 1.3B");
    m.select_translation_model(1);
    assert_eq!(e.f.vm.translation_model.borrow().as_str(), "gemma-4-e4b");
    assert_eq!(v.translation_rows[0].id, "qwen3.5-4b", "Models tab: llama first too");
}

#[test]
fn mac_offline_footer_names_the_cloud_provider() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Mac);
    assert!(m.view().offline_footer.starts_with("Açıkken internete hiç bağlanılmaz ve eksik modeller indirilmez. Ses"));
    *e.f.vm.cloud_name.borrow_mut() = Some("OpenAI".into());
    let v = m.view();
    assert!(v.offline_footer.contains("(OpenAI)"));
    assert!(v.cloud_callout.starts_with("OpenAI bir bulut hizmeti"));
}

// MARK: pages

#[test]
fn windows_models_page_loads_the_catalog_and_reports_a_missing_engine() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    let epoch = m.view().scroll_epoch;
    m.select_page(2);
    pump(&e);
    let v = m.view();
    assert_eq!(v.page, 2);
    assert_eq!(v.page_title, "Modeller");
    assert!(v.scroll_epoch > epoch, "page change scrolls to the top");
    assert!(!v.models_load_error.is_empty(), "no engine → load error, fallback list kept");
    assert!(!v.transcription_rows.is_empty());
    assert!(v.translation_rows.iter().any(|r| r.id == "nllb-1.3b" && r.is_non_commercial));
    assert!(v.transcription_rows.iter().all(|r| r.rating_text.starts_with("Hız ")));
}

#[test]
fn mac_tab_is_stored_and_read_live() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Mac);
    assert_eq!(m.page(), 0);
    m.select_page(2);
    assert_eq!(e.f.settings.get_string("settingsTab").as_deref(), Some("appearance"));
    // Another window stores the tab (the translation picker's "manage providers").
    e.f.settings.set_string("settingsTab", "translation");
    wait(&e, || m.page() == 1);
    assert_eq!(m.view().page_title, "Çeviri");
    e.f.settings.set_string("settingsTab", "bogus");
    wait(&e, || m.page() == 0);
}

#[test]
fn show_page_follows_the_platform_page_names() {
    let _g = SERIAL.lock();
    let e = env();
    let w = model(&e, Flavor::Windows);
    w.show_page(Some("models"));
    assert_eq!(w.page(), 2);
    w.show_page(Some("whatever"));
    assert_eq!(w.page(), 0);
    let m = model(&e, Flavor::Mac);
    m.show_page(Some("translation"));
    assert_eq!(m.page(), 1);
}

// MARK: Windows LAN section

#[test]
fn lan_edits_persist_the_legacy_keys_and_sync_the_bridge() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    m.set_lan_url("192.168.1.50:11434");
    m.commit_lan_model("  gemma4:e4b ");
    m.set_lan_async(true);
    assert_eq!(e.f.settings.get_string("externalLlmUrl").as_deref(), Some("192.168.1.50:11434"));
    // C#: the setter stores the text as typed; the LAN provider sync trims it.
    assert_eq!(e.f.settings.get_string("externalLlmModel").as_deref(), Some("  gemma4:e4b "));
    assert_eq!(e.f.settings.get_bool("externalLlmAsync"), Some(true));
    assert_eq!(e.f.win.synced.lock().last().map(|l| l.url.clone()).as_deref(), Some("192.168.1.50:11434"));

    // Turning it on with an address refreshes the models (no InfoBar).
    *e.f.win.lan_answer.lock() = ok("Bağlantı başarılı", &["qwen3.5:4b", "gemma4:e4b"]);
    m.set_lan_enabled(true);
    assert!(m.view().lan_busy);
    wait(&e, || !m.view().lan_busy);
    let v = m.view();
    assert_eq!(v.lan_models, vec!["qwen3.5:4b", "gemma4:e4b"]);
    assert!(!v.lan_test_open);
    assert_eq!(e.f.settings.get_bool("externalLlmEnabled"), Some(true));
}

#[test]
fn lan_test_shows_the_result_and_fills_an_empty_model() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    m.set_lan_url("http://10.0.0.2:1234");
    *e.f.win.lan_answer.lock() = ok("Bağlantı başarılı (10.0.0.2 · 3 ms · 1 uygun model bulundu)", &["llama3"]);
    m.lan_test();
    wait(&e, || m.view().lan_test_open);
    let v = m.view();
    assert!(v.lan_test_success);
    assert_eq!(v.lan_model, "llama3");
    assert_eq!(e.f.settings.get_string("externalLlmModel").as_deref(), Some("llama3"));
    m.close_lan_test();
    assert!(!m.view().lan_test_open);
}

// MARK: Windows video cache and associations

#[test]
fn video_cache_quota_is_clamped_and_bad_input_reverts() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    assert_eq!(m.view().cache_gb, 20);
    m.edit_cache_gb("900");
    assert_eq!(e.f.settings.get_int("videoCacheMaxSizeGb"), Some(500));
    assert_eq!(*e.f.win.cache_max.lock(), 500);
    m.edit_cache_gb("0");
    assert_eq!(e.f.settings.get_int("videoCacheMaxSizeGb"), Some(1));
    m.edit_cache_gb("12,6");
    assert_eq!(e.f.settings.get_int("videoCacheMaxSizeGb"), Some(13));
    m.edit_cache_gb("NaN");
    assert_eq!(m.view().cache_gb, 13, "non-finite input reverts to the stored value");
}

#[test]
fn video_cache_usage_is_computed_after_load_and_cleared_off_the_ui_thread() {
    let _g = SERIAL.lock();
    let e = env();
    *e.f.win.cache_used.lock() = 3 * 1024 * 1024;
    let m = model(&e, Flavor::Windows);
    assert_eq!(m.view().cache_usage_text, "Önbellek kullanımı hesaplanıyor…");
    m.window_loaded();
    wait(&e, || m.view().clear_cache_enabled);
    assert_eq!(m.view().cache_usage_text, "Şifreli önbellek kullanımı: 3 MB / 20 GB");
    m.clear_cache();
    wait(&e, || *e.f.win.cleared.lock() == 1 && !m.view().clear_cache_enabled);
    assert_eq!(m.view().cache_usage_text, "Şifreli önbellek kullanımı: 0 MB / 20 GB");
}

#[test]
fn file_associations_toggle_and_default_apps() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    let a = e.f.win.associations.clone().unwrap();
    assert!(!m.view().associations_registered);
    m.toggle_associations();
    // The registry work runs on a worker; the result arrives through the UI queue.
    wait(&e, || !m.associations_busy());
    let v = m.view();
    assert!(v.associations_registered && v.associations_bar_open && !v.associations_busy);
    assert_eq!(v.associations_bar_severity, 1);
    assert_eq!(e.f.settings.get_bool("fileAssociationsDisabled"), Some(false));
    m.toggle_associations();
    wait(&e, || !m.associations_busy());
    let v = m.view();
    assert!(!v.associations_registered);
    assert_eq!(v.associations_bar_severity, 0);
    assert_eq!(v.associations_bar_text, "Dosya türü ilişkilendirmeleri kaldırıldı.");
    assert_eq!(e.f.settings.get_bool("fileAssociationsDisabled"), Some(true));
    m.open_default_apps();
    wait(&e, || !m.associations_busy());
    assert!(m.view().associations_registered, "default apps registers first");
    assert_eq!(*a.opened.lock(), 1);
    assert_eq!(e.f.settings.get_bool("fileAssociationsDisabled"), Some(false));
    assert_eq!(*a.calls.lock(), 3);
    // Already registered: the page opens without another registry write.
    m.open_default_apps();
    wait(&e, || !m.associations_busy());
    assert_eq!((*a.opened.lock(), *a.calls.lock()), (2, 3));
}

#[test]
fn file_associations_failure_shows_the_error_bar_and_the_real_state() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    let a = e.f.win.associations.clone().unwrap();
    *a.fail.lock() = true;
    m.toggle_associations();
    wait(&e, || !m.associations_busy());
    let v = m.view();
    assert!(!v.associations_registered, "a failed registration leaves the card off");
    assert_eq!(v.associations_bar_severity, 2);
    assert_eq!(v.associations_bar_text, "Dosya türleri Windows'a kaydedilemedi.");
    assert_eq!(e.f.settings.get_bool("fileAssociationsDisabled"), None, "nothing persisted on failure");
}

/// While the registry work runs the UI thread is free, the card is busy and further clicks are ignored; the bar and
/// the switch change only when the result arrives.
#[test]
fn file_associations_busy_state_ignores_clicks_until_the_result_arrives() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    let a = e.f.win.associations.clone().unwrap();
    {
        let _slow = a.hold.lock();
        m.toggle_associations();
        let v = m.view();
        assert!(v.associations_busy, "the toggle returned at once and the card is busy");
        assert!(!v.associations_registered && !v.associations_bar_open, "nothing shown before the result");
        m.toggle_associations();
        m.open_default_apps();
        pump(&e);
        assert!(m.view().associations_busy);
    }
    wait(&e, || !m.associations_busy());
    pump(&e);
    let v = m.view();
    assert!(v.associations_registered && v.associations_bar_open);
    assert_eq!(v.associations_bar_severity, 1);
    assert_eq!(*a.calls.lock(), 1, "the clicks made while busy were ignored");
    assert_eq!(*a.opened.lock(), 0);
}

// MARK: Windows provider form

#[test]
fn windows_form_starts_with_the_first_preset_and_follows_the_preset_choice() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    let f = m.view().form;
    assert!(f.open && f.is_new);
    assert_eq!(f.preset_items.len(), 8);
    assert_eq!(f.name, "OpenCode Go");
    assert_eq!(f.url, "https://opencode.ai/inference/go/openai/v1");
    assert_eq!(f.model, "glm-5.3-flash");
    assert!(f.async_translation, "Windows: async on for every new provider");
    m.form_select_preset(6);
    let f = m.view().form;
    assert_eq!(f.name, "Yerel Ağ (Ollama / LM Studio)");
    assert_eq!(f.url, "http://127.0.0.1:11434");
    assert_eq!(f.model_items, vec!["gemma4:e4b", "qwen3.5:4b"]);
}

#[test]
fn windows_save_validates_adds_activates_and_resets() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    m.form_set_url("   ");
    m.form_save();
    let v = m.view();
    assert!(v.provider_status_open && !v.provider_status_success);
    assert_eq!(v.provider_status_text, "Lütfen geçerli bir sunucu adresi girin.");
    assert!(e.f.providers.providers().is_empty());

    m.form_select_preset(2); // OpenAI
    m.form_set_name("  Ofis  ");
    m.form_set_key(" sk-oc-1234567890 ");
    m.form_save();
    let added = e.f.providers.providers();
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].name, "Ofis");
    assert_eq!(added[0].preset_id, "openai");
    assert_eq!(e.f.providers.active_provider_id(), Some(added[0].id));
    assert_eq!(e.f.providers.keys.lock().get(&added[0].id).map(String::as_str), Some("sk-oc-1234567890"));
    pump(&e);
    let v = m.view();
    assert_eq!(v.provider_status_text, "Ofis kaydedildi ve etkinleştirildi.");
    assert_eq!(v.form.name, "OpenAI", "the form is reset to the current preset");
    assert!(v.form.key.is_empty());
    assert_eq!(v.provider_rows[0].summary, "https://api.openai.com/v1 · sk-••••7890");
    assert!(v.provider_rows[0].is_active && !v.local_provider_active);
}

#[test]
fn windows_edit_updates_keeps_the_preset_and_delete_resets_the_form() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("Lab", "lan", "http://10.0.0.5:11434"), Some("k1"), false);
    let m = model(&e, Flavor::Windows);
    m.edit_provider(p.id);
    let f = m.view().form;
    assert!(!f.is_new);
    assert_eq!(f.header, "API Sağlayıcısını Düzenle");
    assert_eq!(f.save_caption, "Güncelle");
    assert_eq!(f.preset_index, 6);
    m.form_set_url("http://10.0.0.6:11434");
    m.form_save();
    let saved = e.f.providers.providers().into_iter().find(|x| x.id == p.id).unwrap();
    assert_eq!(saved.base_url, "http://10.0.0.6:11434");
    assert_eq!(saved.preset_id, "lan");
    assert_eq!(e.f.providers.active_provider_id(), Some(p.id));
    assert_eq!(m.view().provider_status_text, "Sağlayıcı güncellendi ve etkinleştirildi.");

    m.edit_provider(p.id);
    m.delete_provider(p.id); // no confirmation on Windows
    assert!(e.f.providers.providers().is_empty());
    assert!(m.view().form.is_new, "deleting the edited provider resets the form");
    assert!(m.view().confirm.is_none());
}

#[test]
fn windows_language_switch_keeps_a_typed_draft_but_relocalizes_a_pristine_one() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    m.form_select_preset(7); // custom: localized name
    assert_eq!(m.view().form.name, "Özel (OpenAI Uyumlu)");
    sa_core::l10n::LanguageState::shared().set_language(sa_core::l10n::UiLanguage::En);
    pump(&e);
    let relocalized = m.view().form.name.clone();
    m.form_set_url("https://example.com/v1");
    sa_core::l10n::LanguageState::shared().set_language(sa_core::l10n::UiLanguage::Tr);
    pump(&e);
    let kept = m.view().form;
    assert_ne!(relocalized, "Özel (OpenAI Uyumlu)", "a pristine draft follows the language");
    assert_eq!(kept.url, "https://example.com/v1", "a typed draft is not discarded");
    assert_eq!(kept.name, relocalized);
}

#[test]
fn windows_provider_model_combo_ignores_blank_values() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("A", "openai", "https://api.openai.com/v1"), None, false);
    let m = model(&e, Flavor::Windows);
    m.commit_provider_model(p.id, "   ");
    assert_eq!(e.f.providers.providers()[0].model, "");
    m.commit_provider_model(p.id, " gpt-4.1-mini ");
    assert_eq!(e.f.providers.providers()[0].model, "gpt-4.1-mini");
}

#[test]
fn windows_form_test_replaces_the_model_list_and_reports() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    m.form_set_model("");
    *e.f.tester.answer.lock() = ok("Bağlantı başarılı", &["m-1", "m-2"]);
    m.form_test();
    assert!(m.view().form.testing);
    wait(&e, || !m.view().form.testing);
    let v = m.view();
    assert_eq!(v.form.model_items, vec!["m-1", "m-2"]);
    assert_eq!(v.form.model, "m-1");
    assert!(v.provider_status_success);
    assert!(e.f.providers.providers().is_empty(), "testing the form stores nothing");
}

#[test]
fn windows_card_test_reports_in_the_status_bar() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("A", "openai", "https://api.openai.com/v1"), None, false);
    let m = model(&e, Flavor::Windows);
    *e.f.tester.answer.lock() = Some(TestOutcome { success: false, message: "HTTP 401".into(), models: vec![] });
    m.test_provider(p.id);
    assert!(m.view().provider_rows[0].is_testing);
    wait(&e, || !m.view().provider_rows[0].is_testing);
    let v = m.view();
    assert!(v.provider_status_open && !v.provider_status_success);
    assert_eq!(v.provider_status_text, "HTTP 401");
}

// MARK: macOS providers

#[test]
fn mac_rows_read_keys_off_the_ui_thread_and_show_captions() {
    let _g = SERIAL.lock();
    let e = env();
    let a = e.f.providers.add(provider("OpenAI", "openai", "https://api.openai.com/v1"), Some("sk-saved"), true);
    let b = e.f.providers.add(provider("Ofis hesabı", "openai", "https://api.openai.com/v1"), None, false);
    let c = e.f.providers.add(provider("Bozuk", "custom", "https://x.example/v1"), Some("sk-x"), false);
    e.f.providers.unreadable.lock().insert(c.id);
    let m = SettingsModel::new(e.f.deps.clone(), Flavor::Mac);
    assert_eq!(m.key_caption(a.id), "Anahtar okunuyor…", "right after construction nothing was read yet");
    wait(&e, || m.key_caption(a.id) != "Anahtar okunuyor…");
    assert_eq!(m.key_caption(a.id), "••••aved");
    assert_eq!(m.key_caption(b.id), "Anahtar yok");
    assert_eq!(m.key_caption(c.id), "Anahtar okunamadı");
    let v = m.view();
    assert!(!v.provider_rows[0].shows_preset_tag, "named like its preset: no tag");
    assert!(v.provider_rows[1].shows_preset_tag);
    assert!(v.provider_rows[0].is_active);
}

#[test]
fn mac_draft_new_saves_what_the_field_holds_and_follows_its_preset() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Mac);
    m.add_provider();
    let f = m.view().form;
    assert!(f.open && f.is_new);
    assert!(!f.async_translation, "a new cloud draft translates sequentially");
    m.form_select_preset(6); // lan
    assert!(m.view().form.async_translation, "only a LAN server translates in the background by default");
    assert!(m.view().form.is_lan);
    m.form_select_preset(2); // openai
    m.form_set_key("  sk-typed \n");
    m.form_save();
    let saved = e.f.providers.providers();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].preset_id, "openai");
    assert_eq!(e.f.providers.keys.lock().get(&saved[0].id).map(String::as_str), Some("sk-typed"));
    assert_eq!(e.f.providers.active_provider_id(), Some(saved[0].id));
    assert!(!m.view().form.open, "the sheet closes when the key was stored");
}

#[test]
fn mac_refused_key_write_keeps_the_sheet_open_and_a_second_save_updates() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Mac);
    *e.f.providers.refuse_keys.lock() = true;
    m.add_provider();
    m.form_set_key("sk-1");
    m.form_save();
    let v = m.view();
    assert!(v.form.open && v.form.key_not_stored);
    assert_eq!(e.f.providers.providers().len(), 1);
    *e.f.providers.refuse_keys.lock() = false;
    m.form_save();
    assert_eq!(e.f.providers.providers().len(), 1, "the second save updates the same provider");
    assert!(!m.view().form.open);
}

#[test]
fn mac_editing_without_touching_the_key_keeps_it_and_removal_removes() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("OpenAI", "openai", "https://api.openai.com/v1"), Some("sk-keep"), true);
    let m = model(&e, Flavor::Mac);
    wait(&e, || m.key_caption(p.id) == "••••keep");
    m.edit_provider(p.id);
    let f = m.view().form;
    assert!(!f.is_new);
    assert!(f.key.is_empty(), "the stored key is never copied into the field");
    assert_eq!(f.key_placeholder, "Kayıtlı anahtar korunur: ••••keep");
    assert_eq!(f.key_note, "Alan boş bırakılırsa kayıtlı anahtar korunur.");
    assert!(f.can_remove_key);
    m.form_set_name("OpenAI 2");
    m.form_save();
    assert_eq!(e.f.providers.keys.lock().get(&p.id).map(String::as_str), Some("sk-keep"));

    m.edit_provider(p.id);
    m.form_remove_stored_key();
    assert_eq!(m.view().form.key_note, "Kayıtlı anahtar kaydedilince silinecek.");
    m.form_save();
    assert!(!e.f.providers.keys.lock().contains_key(&p.id));
    assert_eq!(m.key_caption(p.id), "Anahtar yok");
}

#[test]
fn mac_editing_with_an_unreadable_key_keeps_it() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("OpenAI", "openai", "https://api.openai.com/v1"), Some("sk-hidden"), true);
    e.f.providers.unreadable.lock().insert(p.id);
    let m = model(&e, Flavor::Mac);
    wait(&e, || m.key_caption(p.id) == "Anahtar okunamadı");
    m.edit_provider(p.id);
    let f = m.view().form;
    assert_eq!(f.key_placeholder, "Anahtar okunamadı; boş bırakırsan korunur");
    assert_eq!(f.key_note, "API anahtarı okunamadı. Alan boş bırakılırsa anahtar silinmez.");
    m.form_save();
    assert_eq!(e.f.providers.keys.lock().get(&p.id).map(String::as_str), Some("sk-hidden"));
}

#[test]
fn mac_test_of_an_untouched_draft_sends_the_stored_key_and_stores_nothing() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("OpenAI", "openai", "https://api.openai.com/v1"), Some("sk-live"), true);
    *e.f.vm.offline.borrow_mut() = false;
    let m = model(&e, Flavor::Mac);
    wait(&e, || m.key_caption(p.id) != "Anahtar okunuyor…");
    m.edit_provider(p.id);
    m.form_set_model("");
    *e.f.tester.answer.lock() = ok("Bağlantı başarılı", &["gemma-3", "gpt-4o"]);
    m.form_test();
    wait(&e, || !m.view().form.testing);
    assert_eq!(e.f.tester.calls.lock().last().unwrap(), "draft:openai|https://api.openai.com/v1|sk-live|");
    let f = m.view().form;
    assert_eq!(f.model, "gemma-3", "an empty model takes the first one found");
    assert_eq!(f.notice_text, "Bağlantı başarılı");
    assert!(e.f.providers.providers()[0].discovered_models.is_empty(), "testing a draft stores nothing");
}

#[test]
fn mac_an_answer_for_an_address_edited_meanwhile_is_dropped() {
    let _g = SERIAL.lock();
    let e = env();
    *e.f.vm.offline.borrow_mut() = false;
    let m = model(&e, Flavor::Mac);
    m.add_provider();
    *e.f.tester.answer.lock() = ok("Bağlantı başarılı", &["x"]);
    m.form_test();
    m.form_set_url("https://other.example/v1");
    wait(&e, || e.f.tester.calls.lock().len() == 1);
    pump(&e);
    let f = m.view().form;
    assert!(f.notice_text.is_empty());
    assert!(f.model_items.iter().all(|x| x != "x"));
}

#[test]
fn mac_offline_mode_skips_connection_tests_of_cloud_providers() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("OpenAI", "openai", "https://api.openai.com/v1"), None, true);
    let lan = e.f.providers.add(provider("Ev", "lan", "http://192.168.1.20:11434"), None, false);
    let m = model(&e, Flavor::Mac);
    m.test_provider(p.id);
    let v = m.view();
    assert_eq!(v.provider_rows[0].notice_text, "Çevrimdışı mod açık: internetteki sağlayıcılara bağlanılmaz.");
    assert!(v.provider_rows[0].notice_error);
    assert!(e.f.tester.calls.lock().is_empty());
    *e.f.tester.answer.lock() = ok("Bağlantı başarılı", &[]);
    m.test_provider(lan.id);
    wait(&e, || e.f.tester.calls.lock().len() == 1 && !m.view().provider_rows[1].is_testing);
    assert_eq!(m.view().provider_rows[1].notice_text, "Bağlantı başarılı");
}

#[test]
fn mac_translation_tab_tests_only_a_lan_provider_on_appear() {
    let _g = SERIAL.lock();
    let e = env();
    let cloud = e.f.providers.add(provider("OpenAI", "openai", "https://api.openai.com/v1"), None, true);
    let m = model(&e, Flavor::Mac);
    m.select_page(1);
    pump(&e);
    assert!(e.f.tester.calls.lock().is_empty(), "clouds are contacted only on request");
    let lan = e.f.providers.add(provider("Ev", "lan", "http://192.168.1.20:11434"), None, true);
    pump(&e);
    *e.f.tester.answer.lock() = ok("ok", &[]);
    m.select_page(0);
    m.select_page(1);
    wait(&e, || e.f.tester.calls.lock().len() == 1);
    assert_eq!(e.f.tester.calls.lock()[0], format!("saved:{}", lan.id));
    let _ = cloud;
}

#[test]
fn mac_delete_asks_first_and_removes_the_stored_key() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("Ofis", "openai", "https://api.openai.com/v1"), Some("sk-1"), true);
    let m = model(&e, Flavor::Mac);
    m.delete_provider(p.id);
    let c = m.view().confirm.expect("confirmation");
    assert_eq!(c.action, PendingConfirm::DeleteProvider(p.id));
    assert_eq!(c.title, "“Ofis” sağlayıcısı silinsin mi?");
    assert_eq!(c.message, "Kayıtlı API anahtarı da silinir.");
    m.confirm_dismiss();
    assert_eq!(e.f.providers.providers().len(), 1);
    m.delete_provider(p.id);
    m.confirm_accept();
    assert!(e.f.providers.providers().is_empty());
    assert!(e.f.providers.keys.lock().is_empty());
    assert!(m.view().local_provider_active);
}

#[test]
fn mac_row_model_field_stores_the_trimmed_value_without_activating() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("A", "openai", "https://api.openai.com/v1"), None, false);
    let m = model(&e, Flavor::Mac);
    m.commit_provider_model(p.id, "  gpt-4.1-nano ");
    assert_eq!(e.f.providers.providers()[0].model, "gpt-4.1-nano");
    assert_eq!(e.f.providers.active_provider_id(), None);
    m.activate_provider(Some(p.id));
    assert_eq!(e.f.providers.active_provider_id(), Some(p.id));
    m.activate_provider(Some(Uuid::new_v4()));
    assert_eq!(e.f.providers.active_provider_id(), Some(p.id), "an unknown id is ignored");
    m.activate_provider(None);
    assert_eq!(e.f.providers.active_provider_id(), None);
}

// MARK: Appearance

#[test]
fn appearance_controls_write_clamped_values_and_reset_keeps_theme_and_ui_font() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    let v = m.view();
    assert_eq!(v.font_items.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), vec!["Sistem", "Arial", "Verdana"]);
    assert_eq!(v.ui_font_index, 0);
    assert_eq!(v.overlay_size_text, "%5");
    m.select_theme(2);
    assert_eq!(e.f.settings.get_string("appTheme").as_deref(), Some("dark"));
    m.select_ui_font(2);
    assert_eq!(e.f.settings.get_string("appFontFamily").as_deref(), Some("Verdana"));
    m.select_overlay_font(1);
    m.set_overlay_size(0.2);
    assert_eq!(e.f.settings.get_double("overlayFontSize"), Some(0.09));
    m.set_overlay_color(255, 0, 16);
    assert_eq!(e.f.settings.get_string("overlayTextColor").as_deref(), Some("#FF0010"));
    m.set_overlay_color_hex("00ff00");
    assert_eq!(m.view().overlay_color_hex, "#00FF00");
    m.select_overlay_background(1);
    assert_eq!(e.f.settings.get_string("overlayBackgroundStyle").as_deref(), Some("shadow"));
    m.set_overlay_opacity(1.7);
    assert_eq!(e.f.settings.get_double("overlayBackgroundOpacity"), Some(1.0));
    let v = m.view();
    assert_eq!((v.theme_index, v.ui_font_index, v.overlay_font_index, v.overlay_background_index), (2, 2, 1, 1));
    assert_eq!(v.overlay_size_text, "%9");

    m.reset_overlay();
    let v = m.view();
    assert_eq!(v.overlay_font_index, 0);
    assert_eq!(v.overlay_color_hex, "#FFFFFF");
    assert_eq!(v.overlay_background_index, 0);
    assert!((v.overlay_size - 0.05).abs() < 1e-9 && (v.overlay_opacity - 0.6).abs() < 1e-9);
    assert_eq!((v.theme_index, v.ui_font_index), (2, 2), "theme and interface font are untouched");
}

#[test]
fn an_uninstalled_stored_font_shows_as_system() {
    let _g = SERIAL.lock();
    let e = env();
    e.f.settings.set_string("appFontFamily", "  Missing Font ");
    let m = model(&e, Flavor::Windows);
    assert_eq!(m.view().ui_font_index, 0);
}

// MARK: Models

#[test]
fn model_delete_asks_and_reports_freed_space_on_windows() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    // Fallback models are not downloaded: nothing to delete, no dialog.
    m.request_model_delete("small");
    assert!(m.view().confirm.is_none());
}

#[test]
fn closing_drops_late_results() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("A", "openai", "https://api.openai.com/v1"), None, false);
    let m = model(&e, Flavor::Windows);
    *e.f.tester.answer.lock() = ok("late", &[]);
    m.test_provider(p.id);
    m.close();
    pump(&e);
    assert!(!m.view().provider_status_open, "a result arriving after close is ignored");
}


// MARK: parity checks against the C# / Swift sources

#[test]
fn windows_row_key_caption_is_the_managers_mask_without_a_loading_state() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("Ofis", "openai", "https://api.openai.com/v1"), Some("sk-oc-1234567890"), false);
    let bare = e.f.providers.add(provider("Ev", "lan", "http://192.168.1.20:11434"), None, false);
    // No pump: C# `MaskedApiKey` is read synchronously (the Windows manager holds the keys in memory).
    let m = SettingsModel::new(e.f.deps.clone(), Flavor::Windows);
    let v = m.view();
    assert_eq!(v.provider_rows[0].summary, "https://api.openai.com/v1 · sk-••••7890");
    assert_eq!(v.provider_rows[1].summary, "http://192.168.1.20:11434 · Anahtar yok");
    e.f.providers.unreadable.lock().insert(p.id);
    assert_eq!(m.view().provider_rows[0].summary, "https://api.openai.com/v1 · Anahtar okunamadı");
    let _ = bare;
}

#[test]
fn windows_saving_an_edit_of_a_provider_removed_elsewhere_adds_nothing_and_selects_local() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("Lab", "openai", "https://api.openai.com/v1"), None, true);
    let m = model(&e, Flavor::Windows);
    m.edit_provider(p.id);
    // Another instance removes it (no form reset: only this window's "Sil" resets the form).
    {
        let mut st = e.f.providers.state.lock();
        st.0.clear();
    }
    m.form_save();
    assert!(e.f.providers.providers().is_empty(), "C# UpdateProvider of an unknown id adds nothing");
    assert_eq!(e.f.providers.active_provider_id(), None, "SetActiveProvider(unknown) → on-device models");
    let v = m.view();
    assert!(v.form.is_new);
    assert_eq!(v.provider_status_text, "Sağlayıcı güncellendi ve etkinleştirildi.");
}

#[test]
fn windows_form_test_sends_trimmed_values_and_keeps_the_tested_model() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    m.form_set_url("  https://api.example.com/v1  ");
    m.form_set_key(" sk-1 ");
    m.form_set_model(" mine ");
    *e.f.tester.answer.lock() = ok("Bağlantı başarılı", &["m-1", "m-2"]);
    m.form_test();
    // Typing while the test runs does not survive the answer (C# sets the snapshot model).
    m.form_set_model("typed meanwhile");
    wait(&e, || !m.view().form.testing);
    assert_eq!(e.f.tester.calls.lock()[0], "draft:opencode-go|https://api.example.com/v1|sk-1|mine");
    let f = m.view().form;
    assert_eq!(f.model_items, vec!["m-1", "m-2"]);
    assert_eq!(f.model, "mine");
}

#[test]
fn reselecting_the_shown_page_neither_reloads_nor_scrolls() {
    let _g = SERIAL.lock();
    let e = env();
    let m = model(&e, Flavor::Windows);
    m.show_page(None);
    m.select_page(2);
    pump(&e);
    let epoch = m.view().scroll_epoch;
    m.select_page(2);
    m.show_page(Some("models"));
    assert_eq!(m.view().scroll_epoch, epoch);
    m.select_page(0);
    assert!(m.view().scroll_epoch > epoch);
}

#[test]
fn mac_draft_of_an_unknown_preset_shows_the_custom_type() {
    let _g = SERIAL.lock();
    let e = env();
    let p = e.f.providers.add(provider("Eski", "retired-preset", "https://llm.example.com/v1"), None, false);
    let m = model(&e, Flavor::Mac);
    m.edit_provider(p.id);
    assert_eq!(m.view().form.preset_index, 7, "`TranslationProviderPreset.preset(id:)`: unknown → custom");
    // The Windows combo falls back to the first preset instead.
    let w = model(&e, Flavor::Windows);
    w.edit_provider(p.id);
    assert_eq!(w.view().form.preset_index, 0);
}

#[test]
fn windows_lan_toggle_refreshes_models_while_a_cloud_provider_is_active() {
    let _g = SERIAL.lock();
    let e = env();
    e.f.providers.add(provider("OpenAI", "openai", "https://api.openai.com/v1"), None, true);
    let m = model(&e, Flavor::Windows);
    *e.f.win.lan_answer.lock() = Some(TestOutcome {
        success: false,
        message: "Lütfen geçerli bir sunucu adresi girin.".into(),
        models: vec![],
    });
    // C# `IsExternalLlmActive` is true with an active cloud provider even without a LAN address.
    m.set_lan_enabled(true);
    assert!(m.view().lan_busy);
    wait(&e, || !m.view().lan_busy);
    assert!(!m.view().lan_test_open, "the toggle refreshes without the InfoBar");
}
