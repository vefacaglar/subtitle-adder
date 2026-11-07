//! The CLI tests of `tests/test_translate.py`, `tests/test_external_llm_safety.py` and
//! `tests/test_translation_quality.py` (they drive `cli.main`, so they live with the binary's crate).

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serial_test::serial;

use common::*;
use se_core::json::Value;
use se_core::{BatchFn, ProgressFn, Result, Sentence};
use se_translate::Translator;
use se_translate::http::{HttpFailure, HttpRequest, HttpResponse, HttpTransport};
use se_translate::lan::LanResolver;
use se_translate::llm::CheckpointMode;
use se_translate::{LlamaTranslator, LlmOptions};

const KEY: &str = "sk-test-secret-key-123";
const KEY_ERROR_TR: &str = "API anahtarı şifrelenmemiş bağlantıyla (http) gönderilmez; adresi https:// ile girin.";

/// A fake translator: `on_progress(0.5)`, `on_progress(1.0)`, then `text` for every non-blank line.
struct FakeTranslator {
    text: &'static str,
    progress: bool,
}

impl Translator for FakeTranslator {
    fn translate(
        &mut self,
        sentences: &[Sentence],
        _target: &str,
        _source: Option<&str>,
        on_progress: Option<ProgressFn<'_>>,
        _on_batch: Option<BatchFn<'_>>,
    ) -> Result<Vec<Sentence>> {
        if self.progress {
            if let Some(p) = on_progress {
                p(0.5);
                p(1.0);
            }
        }
        Ok(sentences
            .iter()
            .map(|s| {
                let text = if s.text.trim().is_empty() { s.text.clone() } else { self.text.to_string() };
                Sentence { text, words: Vec::new(), ..s.clone() }
            })
            .collect())
    }
}

fn texts(event: &Value) -> Vec<String> {
    event.get("sentences").unwrap().as_array().unwrap().iter().map(|s| ev_str(s, "text").unwrap().to_string()).collect()
}

#[test]
#[serial]
fn test_cli_json_event_order_and_translation_event() {
    let _g = Guard::new();
    let payload = r#"[{"id": 1, "start": 0.0, "end": 2.0, "text": "Merhaba.", "words": []}, {"id": 2, "start": 2.0, "end": 4.0, "text": " ", "words": []}]"#;
    let fake = Fake {
        needs: Some(Box::new(|_| true)),
        ensure_fractions: Some(vec![0.5, 1.0]),
        translator: Some(Box::new(|_| Ok(Box::new(FakeTranslator { text: "Hola.", progress: true }) as Box<dyn Translator>))),
        ..Fake::default()
    };
    let (code, out, err) = run_with_stdin(&fake, &["translate", "--to", "es", "--json"], payload.as_bytes());
    assert_eq!(code, 0, "{out}{err}");
    let evs = events(&out);
    assert_eq!(
        stages(&evs),
        [
            ("downloading_model".to_string(), Some(0.0)),
            ("downloading_model".to_string(), Some(0.5)),
            ("downloading_model".to_string(), Some(1.0)),
            ("loading_model".to_string(), None),
            ("translating".to_string(), None),
            ("translating".to_string(), Some(0.5)),
            ("translating".to_string(), Some(1.0)),
        ]
    );
    let translated = evs.last().unwrap();
    assert_eq!(ev_str(translated, "event"), Some("translation"));
    assert_eq!(ev_str(translated, "language"), Some("es"));
    assert_eq!(texts(translated), ["Hola.", " "]);
    assert!(
        translated
            .get("sentences")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s.get("words").unwrap().as_array().unwrap().is_empty())
    );
    assert_eq!(fake.ensured.borrow().as_slice(), ["madlad-3b".to_string()]);
}

#[test]
#[serial]
fn test_cli_nllb_without_source_is_turkish_error() {
    let _g = Guard::new();
    let (code, out, _) = run_with_stdin(&Fake::default(), &["translate", "--to", "ru", "--model", "nllb-1.3b", "--json"], b"[]");
    assert_eq!(code, 1);
    let evs = events(&out);
    assert_eq!(evs.len(), 1, "{out}");
    assert_eq!(ev_str(&evs[0], "event"), Some("error"));
    assert!(ev_str(&evs[0], "message").unwrap().contains("kaynak dil"), "{out}");
}

#[test]
#[serial]
fn test_cli_unsupported_language_is_turkish_error() {
    let _g = Guard::new();
    let (code, out, _) = run_with_stdin(&Fake::default(), &["translate", "--to", "ko", "--json"], b"[]");
    assert_eq!(code, 1);
    let evs = events(&out);
    assert_eq!(evs.len(), 1, "{out}");
    assert_eq!(ev_str(&evs[0], "event"), Some("error"));
    assert!(ev_str(&evs[0], "message").unwrap().contains("desteklemiyor"), "{out}");
}

#[test]
#[serial]
fn test_cli_writes_srt_without_json() {
    let _g = Guard::new();
    let payload = r#"[{"id": 1, "start": 0.0, "end": 2.0, "text": "Merhaba.", "words": []}]"#;
    let fake = Fake {
        translator: Some(Box::new(|_| Ok(Box::new(FakeTranslator { text: "Hola.", progress: false }) as Box<dyn Translator>))),
        ..Fake::default()
    };
    let (code, out, err) = run_with_stdin(&fake, &["translate", "--to", "es"], payload.as_bytes());
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "1\n00:00:00,000 --> 00:00:02,000\nHola.\n");
    // Without --json only the indeterminate stages are logged.
    assert_eq!(err, "\u{2026} loading_model\n\u{2026} translating\n");
}

#[test]
#[serial]
fn test_api_key_over_plain_http_is_refused_from_the_environment_through_the_cli() {
    let _g = Guard::new();
    let _env = LlmEnv;
    set_env(se_translate::external::EXTERNAL_LLM_URL_ENV_VAR, "http://api.openai.com/v1");
    set_env(se_translate::external::EXTERNAL_LLM_API_KEY_ENV_VAR, KEY);
    let fake = Fake { translator: Some(Box::new(|_| panic!("no request may be made"))), ..Fake::default() };
    let (code, out, _) = run_with_stdin(&fake, &["translate", "--to", "tr", "--json"], b"[]");
    assert_eq!(code, 1);
    assert_eq!(out, format!("{{\"event\": \"error\", \"message\": \"{KEY_ERROR_TR}\"}}\n"));
    assert!(!out.contains(KEY));
}

/// Clears the external-LLM and offline variables when dropped (also after a failed assertion).
struct LlmEnv;

impl Drop for LlmEnv {
    fn drop(&mut self) {
        clear_llm_env();
    }
}

fn clear_llm_env() {
    for name in [
        se_translate::external::EXTERNAL_LLM_URL_ENV_VAR,
        se_translate::external::EXTERNAL_LLM_API_KEY_ENV_VAR,
        se_translate::external::EXTERNAL_LLM_MODEL_ENV_VAR,
        se_core::offline::OFFLINE_ENV_VAR,
    ] {
        // SAFETY: #[serial] tests.
        unsafe { std::env::remove_var(name) };
    }
}

fn offline_blocks_internet_server(flag: &str) {
    let _g = Guard::new();
    let _env = LlmEnv;
    set_env(se_translate::external::EXTERNAL_LLM_URL_ENV_VAR, "https://api.openai.com/v1");
    set_env(se_translate::external::EXTERNAL_LLM_API_KEY_ENV_VAR, KEY);
    let fake = Fake { translator: Some(Box::new(|_| panic!("must not load anything"))), ..Fake::default() };
    let mut args = vec!["translate", "--to", "tr", "--json"];
    if flag == "env" {
        set_env(se_core::offline::OFFLINE_ENV_VAR, "1");
    } else {
        args.insert(0, "--offline");
    }
    let (code, out, _) = run_with_stdin(&fake, &args, b"[]");
    assert_eq!(code, 1);
    let evs = events(&out);
    assert_eq!(evs.len(), 1, "a single line: no stage event came first: {out}");
    let message = ev_str(&evs[0], "message").unwrap();
    assert_eq!(ev_str(&evs[0], "event"), Some("error"));
    assert!(message.contains("Çevrimdışı mod açık") && message.contains("(api.openai.com)"), "{message}");
    assert!(!out.contains(KEY));
}

#[test]
#[serial]
fn test_offline_mode_blocks_an_internet_server_through_the_cli_before_any_work_env() {
    offline_blocks_internet_server("env");
}

#[test]
#[serial]
fn test_offline_mode_blocks_an_internet_server_through_the_cli_before_any_work_cli() {
    offline_blocks_internet_server("cli");
}

/// `recording_urlopen`: records every request and answers with `CHAT_REPLY`.
struct Recording(Arc<Mutex<Vec<String>>>);

impl HttpTransport for Recording {
    fn open(&self, request: &HttpRequest, _timeout: Duration) -> std::result::Result<HttpResponse, HttpFailure> {
        self.0.lock().unwrap().push(request.url.clone());
        Ok(HttpResponse { status: 200, body: br#"{"choices": [{"message": {"content": "[1] Merhaba!"}}]}"#.to_vec() })
    }
}

/// `_resolve_lan_url = lambda u, **kwargs: u`.
struct Identity;

impl LanResolver for Identity {
    fn resolve(&self, url: &str, _allow_scan: bool) -> String {
        url.to_string()
    }
}

#[test]
#[serial]
fn test_offline_mode_allows_a_lan_server_through_the_cli() {
    let _g = Guard::new();
    let _env = LlmEnv;
    set_env(se_core::offline::OFFLINE_ENV_VAR, "1");
    set_env(se_translate::external::EXTERNAL_LLM_URL_ENV_VAR, "http://192.168.1.42:1234");
    set_env(se_translate::external::EXTERNAL_LLM_MODEL_ENV_VAR, "qwen3");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    // The real `create_translator` path for an external URL, with `urlopen`, `_resolve_lan_url` and
    // `_temp_checkpoint_path` patched as in the Python test.
    let fake = Fake {
        translator: Some(Box::new(move |model_id| {
            let options = LlmOptions {
                transport: Some(Arc::new(Recording(recorded.clone()))),
                resolver: Some(Box::new(Identity)),
                checkpoint: CheckpointMode::Disabled,
                ..LlmOptions::default()
            };
            Ok(Box::new(LlamaTranslator::new(model_id, options)?) as Box<dyn Translator>)
        })),
        ..Fake::default()
    };
    let payload = r#"[{"id": 1, "start": 0.0, "end": 2.0, "text": "Hello!", "words": []}]"#;
    let (code, out, err) = run_with_stdin(&fake, &["translate", "--to", "tr", "--from", "en", "--json"], payload.as_bytes());
    assert_eq!(code, 0, "{out}{err}");
    let evs = events(&out);
    let last = evs.last().unwrap();
    assert_eq!(ev_str(last, "event"), Some("translation"));
    assert_eq!(texts(last)[0], "Merhaba!");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.first().map(String::as_str), Some("http://192.168.1.42:1234/v1/chat/completions"));
}

#[test]
#[serial]
fn test_cli_completes_with_review_metadata_for_unrecoverable_line() {
    let _g = Guard::new();
    let payload = r#"[{"id": 10, "start": 0, "end": 2, "text": "Hello there.", "words": [], "speaker": 0}]"#;
    let fake = Fake {
        require: Some(Box::new(|_| Ok(false))),
        translator: Some(Box::new(|_| {
            let options = LlmOptions {
                completion_fn: Some(Box::new(|_| Ok("Explanation only.".to_string()))),
                ..LlmOptions::default()
            };
            Ok(Box::new(LlamaTranslator::new("gemma-4-e4b", options)?) as Box<dyn Translator>)
        })),
        ..Fake::default()
    };
    let (code, out, err) = run_with_stdin(&fake, &["translate", "--to", "tr", "--from", "en", "--json"], payload.as_bytes());
    assert_eq!(code, 0, "{out}{err}");
    let evs = events(&out);
    let last = evs.last().unwrap();
    assert_eq!(ev_str(last, "event"), Some("translation"));
    let first = &last.get("sentences").unwrap().as_array().unwrap()[0];
    assert_eq!(first.get("needs_review").and_then(Value::as_bool), Some(true));
    assert!(!evs.iter().any(|e| ev_str(e, "event") == Some("error")));
}
