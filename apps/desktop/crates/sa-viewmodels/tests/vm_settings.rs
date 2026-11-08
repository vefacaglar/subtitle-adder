//! Ports of `ViewModelSettingsTests` (C#) and `AppFoundationTests` (Swift): persisted settings, defaults,
//! reconciliation of models and targets.

mod common;

use common::*;
use sa_core::appearance::SubtitleDisplay;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::settings::{SettingsStore, keys};
use sa_viewmodels::policy::PlatformPolicy;

#[test]
fn defaults_match_macos() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    for policy in [PlatformPolicy::windows(), PlatformPolicy::mac()] {
        let env = Env::new(policy);
        let (vm, _c) = env.vm();
        assert_eq!(vm.language(), "");
        assert_eq!(vm.model(), "large-v3");
        assert_eq!(vm.translation_target(), "en");
        assert_eq!(vm.translation_model(), "madlad-3b");
        assert_eq!(vm.display_mode(), SubtitleDisplay::Original);
        assert!(vm.diarize());
        assert_eq!(vm.speaker_count(), 0);
        assert!(vm.model_notice().is_none());
        assert!(vm.auto_save_subtitle());
        assert!(vm.auto_play_video());
        assert!(!vm.side_by_side_layout());
        assert!(!vm.hide_subtitle_panel());
    }
}

#[test]
fn choices_are_persisted_under_the_macos_keys() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    vm.set_language("tr");
    vm.set_model("small");
    vm.set_translation_target("es");
    vm.set_translation_model("nllb-1.3b");
    vm.set_display_mode(SubtitleDisplay::Both);
    vm.set_diarize(false);
    vm.set_speaker_count(3);
    let st = &env.settings;
    assert_eq!(st.get_string(keys::SELECTED_LANGUAGE).as_deref(), Some("tr"));
    assert_eq!(st.get_string(keys::SELECTED_MODEL).as_deref(), Some("small"));
    assert_eq!(st.get_string(keys::TRANSLATION_TARGET).as_deref(), Some("es"));
    assert_eq!(st.get_string(keys::TRANSLATION_MODEL).as_deref(), Some("nllb-1.3b"));
    assert_eq!(st.get_string(keys::SUBTITLE_DISPLAY_MODE).as_deref(), Some("both"));
    assert_eq!(st.get_bool(keys::DIARIZE_SPEAKERS), Some(false));
    assert_eq!(st.get_int(keys::SPEAKER_COUNT), Some(3));
    let (vm2, _c2) = env.vm();
    assert_eq!(vm2.language(), "tr");
    assert_eq!(vm2.model(), "small");
    assert_eq!(vm2.translation_target(), "es");
    assert_eq!(vm2.translation_model(), "nllb-1.3b");
    assert_eq!(vm2.display_mode(), SubtitleDisplay::Both);
    assert!(!vm2.diarize());
    assert_eq!(vm2.speaker_count(), 3);
}

#[test]
fn invalid_stored_values_fall_back() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "Türkçe");
    env.settings.set_string(keys::TRANSLATION_TARGET, "");
    env.settings.set_string(keys::SUBTITLE_DISPLAY_MODE, "weird");
    env.settings.set_int(keys::SPEAKER_COUNT, 1);
    let (vm, _c) = env.vm();
    assert_eq!(vm.language(), "");
    assert_eq!(vm.translation_target(), "en");
    assert_eq!(vm.display_mode(), SubtitleDisplay::Original);
    assert_eq!(vm.speaker_count(), 0);
}

#[test]
fn language_filter_picks_most_accurate_model_with_notice() {
    let l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "ru");
    env.settings.set_string(keys::SELECTED_MODEL, "gigaam-v3");
    let (vm, _c) = env.vm();
    assert_eq!(vm.model(), "gigaam-v3", "gigaam is kept for ru");
    assert!(vm.model_notice().is_none());
    vm.set_language("");
    assert_eq!(vm.model(), "large-v3");
    l.set(UiLanguage::En);
    let notice = vm.model_notice().expect("notice");
    assert_eq!(notice.text(), "Selected Most accurate for Automatic detection.");
    assert!(!vm.available_models().iter().any(|m| m.id == "parakeet-v3"), "parakeet absent for automatic");
    vm.set_model("small");
    assert!(vm.model_notice().is_none(), "a manual model change clears the notice");
}

#[test]
fn tie_on_accuracy_prefers_faster_model() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    *env.engine.models.lock() = vec![
        model("slow", "transcription", Some(&["de"]), 1, 3, true),
        model("fast", "transcription", Some(&["en"]), 5, 5, true),
        model("also-fast", "transcription", Some(&["en"]), 5, 5, true),
        model("madlad-3b", "translation", Some(&["en", "tr"]), 2, 5, true),
    ];
    env.settings.set_string(keys::SELECTED_MODEL, "slow");
    env.settings.set_string(keys::SELECTED_LANGUAGE, "en");
    let (vm, _c) = env.vm();
    assert_eq!(vm.model(), "fast");
}

#[test]
fn translation_target_never_equals_source() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    vm.set_language("en");
    assert_eq!(vm.translation_target(), "tr");
    assert!(!vm.is_translation_target_enabled("en"));
    assert!(vm.is_translation_target_enabled("es"));
}

#[test]
fn catalog_reload_reconciles_translation_model() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_string(keys::TRANSLATION_MODEL, "old-model");
    let (vm, catalog) = env.vm();
    assert_eq!(vm.translation_model(), "madlad-3b");
    vm.set_translation_model("gone");
    *env.engine.models.lock() = vec![
        model("large-v3", "transcription", None, 1, 5, true),
        model("nllb-1.3b", "translation", Some(&["en", "tr"]), 3, 4, true),
    ];
    catalog.load();
    env.eventually("reconciled to nllb", || vm.translation_model() == "nllb-1.3b");
}

#[test]
fn auto_play_video_defaults_to_true_and_persists() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    assert!(vm.auto_play_video());
    vm.set_auto_play_video(false);
    assert_eq!(env.settings.get_bool(keys::AUTO_PLAY_VIDEO), Some(false));
    let (vm2, _c2) = env.vm();
    assert!(!vm2.auto_play_video());
}

#[test]
fn offline_mode_defaults_on_persists_and_reaches_the_engine() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.remove(keys::OFFLINE_MODE);
    let (vm, _c) = env.vm();
    assert!(vm.offline_mode());
    assert!(env.launch.get().offline);
    vm.set_offline_mode(false);
    assert_eq!(env.settings.get_bool(keys::OFFLINE_MODE), Some(false));
    assert!(!env.launch.get().offline);
}

#[test]
fn the_layout_choice_is_persisted_and_theater_mode_is_not() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, _c) = env.vm();
    assert!(env.settings.get_bool(keys::SIDE_BY_SIDE_LAYOUT).is_none(), "absent until changed");
    vm.set_side_by_side_layout(true);
    assert_eq!(env.settings.get_bool(keys::SIDE_BY_SIDE_LAYOUT), Some(true));
    vm.set_theater_mode(true);
    assert!(vm.is_theater_mode());
    let (vm2, _c2) = env.vm();
    assert!(vm2.side_by_side_layout());
    assert!(!vm2.is_theater_mode());
}

#[test]
fn auto_save_defaults_on_and_persists() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, _c) = env.vm();
    assert!(vm.auto_save_subtitle());
    vm.set_auto_save_subtitle(false);
    assert_eq!(env.settings.get_bool(keys::AUTO_SAVE_SUBTITLE), Some(false));
}

#[test]
fn models_are_filtered_by_the_new_languages() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, _c) = env.vm();
    vm.set_language("tr");
    let ids: Vec<String> = vm.available_models().into_iter().map(|m| m.id).collect();
    assert!(ids.contains(&"large-v3".to_owned()));
    assert!(!ids.contains(&"parakeet-v3".to_owned()), "tr is only in Whisper");
    assert!(!ids.contains(&"gigaam-v3".to_owned()));
    vm.set_language("ru");
    let ids: Vec<String> = vm.available_models().into_iter().map(|m| m.id).collect();
    assert!(ids.contains(&"gigaam-v3".to_owned()));
    assert!(ids.contains(&"parakeet-v3".to_owned()));
}

#[test]
fn speaker_count_choices_and_diarization_id() {
    assert_eq!(sa_viewmodels::SubtitleViewModel::SPEAKER_COUNT_CHOICES, [0, 2, 3, 4, 5, 6]);
    assert_eq!(sa_viewmodels::SubtitleViewModel::DIARIZATION_MODEL_ID, "speaker-diarization");
    assert_eq!(sa_viewmodels::SubtitleViewModel::MIN_ASYNC_BATCH_SENTENCES, 10);
}
