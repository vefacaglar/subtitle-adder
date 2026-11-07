//! Port of `tests/test_i18n.py` (the parts that live in se-core). The CLI, models and translate parts of that
//! file are ported in `se-cli/tests/i18n_cli.rs`, `se-models` and `se-translate` tests.

use std::collections::BTreeSet;

use serial_test::serial;

use se_core::i18n::{self, Lang};
use se_core::t;

/// conftest `_default_language`.
struct DefaultLanguage;

impl DefaultLanguage {
    fn new() -> Self {
        // SAFETY: tests touching the environment are #[serial].
        unsafe { std::env::remove_var(i18n::ENV_VAR) };
        i18n::set_language(None);
        DefaultLanguage
    }
}

impl Drop for DefaultLanguage {
    fn drop(&mut self) {
        unsafe { std::env::remove_var(i18n::ENV_VAR) };
        i18n::set_language(None);
    }
}

fn set_env(value: &str) {
    unsafe { std::env::set_var(i18n::ENV_VAR, value) };
}

/// `support_error(gigaam-v3, lang)` for a model whose only language is Russian (models.support_error).
fn gigaam_only_russian() -> String {
    t!("only_language", name = i18n::display_name("gigaam-v3"), language = i18n::language_name("ru"))
}

#[test]
#[serial]
fn test_default_is_turkish() {
    let _g = DefaultLanguage::new();
    assert_eq!(i18n::current(), Lang::Tr);
    assert_eq!(gigaam_only_russian(), "GigaAM yalnız Rusça destekler.");
}

#[test]
#[serial]
fn test_env_selects_english() {
    let _g = DefaultLanguage::new();
    set_env("en");
    assert_eq!(gigaam_only_russian(), "GigaAM supports only Russian.");
}

#[test]
#[serial]
fn test_invalid_env_falls_back_to_turkish() {
    let _g = DefaultLanguage::new();
    for value in ["", "de", "EN-us", "turkish"] {
        set_env(value);
        assert_eq!(i18n::current(), Lang::Tr, "{value:?}");
    }
}

#[test]
#[serial]
fn test_env_value_is_case_insensitive() {
    let _g = DefaultLanguage::new();
    set_env(" EN ");
    assert_eq!(i18n::current(), Lang::En);
}

#[test]
fn test_every_key_has_both_languages_and_same_placeholders() {
    let mut keys = BTreeSet::new();
    for (key, tr, en) in i18n::MESSAGES {
        assert!(keys.insert(*key), "duplicate key {key}");
        assert!(!tr.is_empty() && !en.is_empty(), "{key}");
        let a: BTreeSet<&str> = i18n::placeholders(tr).into_iter().collect();
        let b: BTreeSet<&str> = i18n::placeholders(en).into_iter().collect();
        assert_eq!(a, b, "{key}");
    }
    assert_eq!(i18n::MESSAGES.len(), 57);
}

#[test]
#[serial]
fn test_partial_delete_name_follows_language() {
    let _g = DefaultLanguage::new();
    set_env("en");
    assert_eq!(i18n::display_name("speaker-diarization"), "Speaker separation");
    assert_eq!(i18n::language_name("es"), "Spanish");
    assert_eq!(i18n::language_name("xx"), "xx");
}

#[test]
#[serial]
fn test_set_language_pins_and_none_goes_back_to_env() {
    let _g = DefaultLanguage::new();
    set_env("en");
    i18n::set_language(Some("tr"));
    assert_eq!(i18n::current(), Lang::Tr);
    i18n::set_language(Some("xx"));
    assert_eq!(i18n::current(), Lang::Tr);
    i18n::set_language(Some(" En "));
    assert_eq!(i18n::current(), Lang::En);
    set_env("tr");
    assert_eq!(i18n::current(), Lang::En);
    i18n::set_language(None);
    assert_eq!(i18n::current(), Lang::Tr);
}

#[test]
#[serial]
fn test_format_fills_named_placeholders_once() {
    let _g = DefaultLanguage::new();
    assert_eq!(t!("deleted", id = "base", freed = 123), "base silindi, 123 MB boşaldı");
    assert_eq!(t!("video_not_found", video = "{video}"), "Video bulunamadı: {video}");
    assert_eq!(t!("summary", language = "tr", count = 2), "dil: tr | 2 altyazı satırı");
    i18n::set_language(Some("en"));
    assert_eq!(
        t!("offline_external_llm", host = "api.openai.com"),
        "Offline mode is on: translation can't be sent to an internet provider (api.openai.com). Turn offline mode off or choose a local model/server."
    );
}
