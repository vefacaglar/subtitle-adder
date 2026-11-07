//! Translate part of `tests/test_i18n.py` (`test_external_llm_failure_names_the_server_not_llama_cpp` is in
//! `tests/llm_translate.rs`, next to the other HTTP-seam tests).

use serial_test::serial;

use se_core::i18n;
use se_translate::{check_request, normalize_external_chat_url};

struct Guard;

impl Guard {
    fn new() -> Self {
        // SAFETY: #[serial] tests.
        unsafe {
            std::env::remove_var(i18n::ENV_VAR);
            std::env::remove_var(se_translate::external::EXTERNAL_LLM_URL_ENV_VAR);
        }
        i18n::set_language(None);
        Guard
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var(i18n::ENV_VAR) };
        i18n::set_language(None);
    }
}

fn set_env(value: &str) {
    unsafe { std::env::set_var(i18n::ENV_VAR, value) };
}

#[test]
#[serial]
fn test_unsupported_language_and_translation_errors() {
    let _g = Guard::new();
    assert!(check_request("nllb-1.3b", "en", None).unwrap_err().to_string().contains("kaynak dil"));
    assert!(check_request("base", "en", None).unwrap_err().to_string().contains("Bilinmeyen çeviri modeli: base"));
    set_env("en");
    assert!(check_request("nllb-1.3b", "en", None).unwrap_err().to_string().contains("requires a source language"));
    assert!(check_request("base", "en", None).unwrap_err().to_string().contains("Unknown translation model: base"));
}

#[test]
#[serial]
fn test_empty_external_llm_url_error_is_localized() {
    let _g = Guard::new();
    assert_eq!(normalize_external_chat_url("  ").unwrap_err().to_string(), "Harici LLM sunucusu adresi boş olamaz.");
    set_env("en");
    assert_eq!(normalize_external_chat_url("/").unwrap_err().to_string(), "The external LLM server address cannot be empty.");
}

#[test]
#[serial]
fn test_external_llm_url_without_a_host_is_rejected() {
    let _g = Guard::new();
    for raw in ["", "  ", "/", "http://", "https://", "http:///", "HTTP://", "http://:1234"] {
        assert_eq!(
            normalize_external_chat_url(raw).unwrap_err().to_string(),
            "Harici LLM sunucusu adresi boş olamaz.",
            "{raw:?}"
        );
    }
}
