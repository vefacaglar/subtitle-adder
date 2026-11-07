//! Port of the LLM parts of `tests/test_translate.py` (prompt construction, retries, external servers,
//! OpenCode / DeepSeek specifics, error texts and `[translation]` diagnostics), plus the Appendix A checkpoint
//! hash vectors of the spec and `test_external_llm_failure_names_the_server_not_llama_cpp` (`test_i18n.py`).

mod common;

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use common::*;
use serial_test::serial;

use se_core::{Sentence, Value};
use se_translate::external::{
    EXTERNAL_LLM_API_KEY_ENV_VAR, EXTERNAL_LLM_MODEL_ENV_VAR, EXTERNAL_LLM_URL_ENV_VAR, external_models_url,
    normalize_external_chat_url,
};
use se_translate::http::{HttpFailure, HttpResponse, http_error_detail};
use se_translate::llm::{CheckpointMode, LlmOptions};
use se_translate::{LlamaTranslator, Translator, checkpoint};

#[test]
#[serial]
fn test_llama_translator_builds_direct_prompt_with_dialect_and_context() {
    let _env = Env::new();
    let (f, calls) = scripted(&[
        "<think>Thinking about Brazilian Portuguese and Mexican Spanish slang...</think>\n[1] Ne haber dostum?\n[2] Hiçbir şey olmaz, merak etme.",
        "[1] Harika görünüyorsun!",
    ]);
    let mut translator = LlamaTranslator::new(
        "gemma-4-e4b",
        LlmOptions { completion_fn: Some(f), batch_size: 2, context_lines: 2, ..LlmOptions::default() },
    )
    .unwrap();
    let mut sentences = vec![
        sentence(1, 0.0, 1.5, "E aí, mano?"),
        sentence(2, 1.5, 3.0, "No pasa nada, güey."),
        sentence(3, 3.0, 4.5, "Tá mandando muito bem!"),
    ];
    sentences[0].speaker = Some(0);
    sentences[1].speaker = Some(1);
    sentences[2].speaker = Some(0);
    let (result, _, progress) = translate_collect(&mut translator, &sentences, "tr", Some("pt"));
    let result = result.unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    let sys_prompt = &calls[0][0].content;
    assert!(sys_prompt.contains("Brazilian Portuguese"));
    assert!(sys_prompt.contains("Turkish"));
    assert!(calls[0][1].content.contains("[1] E aí, mano?\n[2] No pasa nada, güey."));
    assert!(calls[1][1].content.contains("Previous lines (for context only, do NOT translate):"));
    assert!(calls[1][1].content.contains("- E aí, mano?"));
    assert!(calls[1][1].content.contains("[1] Tá mandando muito bem!"));
    let texts: Vec<&str> = result.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, vec!["Ne haber dostum?", "Hiçbir şey olmaz, merak etme.", "Harika görünüyorsun!"]);
    let speakers: Vec<Option<i64>> = result.iter().map(|r| r.speaker).collect();
    assert_eq!(speakers, vec![Some(0), Some(1), Some(0)]);
    assert_eq!(progress, vec![2.0 / 3.0, 1.0]);
}

#[test]
#[serial]
fn test_llama_translator_retries_missing_line_with_whole_batch() {
    let _env = Env::new();
    let (f, calls) = scripted(&["[1] Birinci satır çevirisi", "[1] Birinci satır çevirisi\n[2] İkinci satır çevirisi"]);
    let mut translator = with_completion("qwen3.5-4b", f);
    let sentences = vec![sentence(1, 0.0, 1.0, "Line one."), sentence(2, 1.0, 2.0, "Line two.")];
    let result = translator.translate(&sentences, "tr", Some("en"), None, None).unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls[1][1].content.contains("[1] Line one.\n[2] Line two."));
    let texts: Vec<&str> = result.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, vec!["Birinci satır çevirisi", "İkinci satır çevirisi"]);
}

#[test]
#[serial]
fn test_normalize_external_chat_url_handles_lm_studio_ollama_and_raw_ip() {
    let _env = Env::new();
    assert_eq!(normalize_external_chat_url("192.168.1.50:1234").unwrap(), "http://192.168.1.50:1234/v1/chat/completions");
    assert_eq!(
        normalize_external_chat_url("http://192.168.1.50:11434/").unwrap(),
        "http://192.168.1.50:11434/v1/chat/completions"
    );
    assert_eq!(normalize_external_chat_url("http://192.168.1.50:1234/v1").unwrap(), "http://192.168.1.50:1234/v1/chat/completions");
    assert_eq!(
        normalize_external_chat_url("http://mac-studio.local:8080/v1/chat/completions").unwrap(),
        "http://mac-studio.local:8080/v1/chat/completions"
    );
    // Quirks kept from Python.
    assert_eq!(normalize_external_chat_url("ftp://h").unwrap(), "http://ftp://h/v1/chat/completions");
    assert_eq!(normalize_external_chat_url("http://h/v1?x=1").unwrap(), "http://h/v1?x=1/v1/chat/completions");
    assert_eq!(normalize_external_chat_url("HTTPS://Host/v1").unwrap(), "HTTPS://Host/v1/chat/completions");
    assert!(normalize_external_chat_url("http://[::1").is_err());
}

#[test]
#[serial]
fn test_external_llm_skips_local_model_download_and_posts_to_remote_server() {
    let env = Env::new();
    env.set(EXTERNAL_LLM_URL_ENV_VAR, "192.168.1.42:1234");
    env.set(EXTERNAL_LLM_MODEL_ENV_VAR, "gemma-3-27b-it");
    // create_translator picks the LLM translator for madlad-3b (no download, no request in the constructor).
    assert!(se_translate::create_translator("madlad-3b", se_translate::Device::Auto, "int8").is_ok());

    let transport = FakeTransport::json(r#"{"choices": [{"message": {"content": "[1] Merhaba M1 Max!"}}]}"#);
    let mut translator = LlamaTranslator::new(
        "madlad-3b",
        LlmOptions { transport: Some(transport.clone()), checkpoint: CheckpointMode::Disabled, ..LlmOptions::default() },
    )
    .unwrap();
    let result = translator.translate(&[sentence(1, 0.0, 1.5, "Hello M1 Max!")], "tr", Some("en"), None, None).unwrap();
    assert_eq!(result[0].text, "Merhaba M1 Max!");
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "http://192.168.1.42:1234/v1/chat/completions");
    assert_eq!(requests[0].json_body().unwrap().get("model").and_then(Value::as_str), Some("gemma-3-27b-it"));
}

#[test]
#[serial]
fn test_external_llm_api_key_attaches_bearer_header_and_retries_without_chat_template_kwargs() {
    let env = Env::new();
    env.set(EXTERNAL_LLM_URL_ENV_VAR, "https://opencode.ai/zen/go/v1");
    env.set(EXTERNAL_LLM_MODEL_ENV_VAR, "qwen3-235b");
    env.set(EXTERNAL_LLM_API_KEY_ENV_VAR, "sk-opencode-test-key-123");
    let transport = FakeTransport::new(|req| {
        let body = req.json_body().unwrap();
        if body.get("chat_template_kwargs").is_some() {
            return Err(HttpFailure::Http {
                code: 400,
                reason: "Bad Request: unrecognized field chat_template_kwargs".into(),
                body: Vec::new(),
            });
        }
        Ok(HttpResponse {
            status: 200,
            body: br#"{"choices": [{"message": {"content": "[1] Merhaba OpenCode Go!"}}]}"#.to_vec(),
        })
    });
    let mut translator = LlamaTranslator::new(
        "madlad-3b",
        LlmOptions { transport: Some(transport.clone()), checkpoint: CheckpointMode::Disabled, ..LlmOptions::default() },
    )
    .unwrap();
    let result =
        translator.translate(&[sentence(1, 0.0, 1.5, "Hello OpenCode Go!")], "tr", Some("en"), None, None).unwrap();
    assert_eq!(result[0].text, "Merhaba OpenCode Go!");
    let r = transport.requests();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].url, "https://opencode.ai/inference/go/openai/v1/chat/completions");
    assert_eq!(r[0].header("authorization"), Some("Bearer sk-opencode-test-key-123"));
    assert_eq!(r[0].header("user-agent"), Some("SubtitleAdder/1.0"));
    assert!(r[0].json_body().unwrap().get("chat_template_kwargs").is_some());
    assert!(r[1].json_body().unwrap().get("chat_template_kwargs").is_none());
    assert_eq!(r[1].header("authorization"), Some("Bearer sk-opencode-test-key-123"));
    let session = r[0].header("x-opencode-session").unwrap();
    assert_eq!(Some(session), r[1].header("x-opencode-session"));
    assert_eq!(session.len(), 32);
    assert!(session.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[test]
#[serial]
fn test_temp_checkpoint_path_unaffected_by_external_api_key() {
    let env = Env::new();
    let sentences = vec![sentence(1, 0.0, 1.5, "Hello world")];
    env.set(EXTERNAL_LLM_URL_ENV_VAR, "https://opencode.ai/zen/go/v1");
    env.set(EXTERNAL_LLM_MODEL_ENV_VAR, "glm-5");
    env.set(EXTERNAL_LLM_API_KEY_ENV_VAR, "sk-secret-alpha");
    let a = checkpoint::checkpoint_path("gemma-4-e4b", "tr", Some("en"), &sentences, "prompt").unwrap();
    env.set(EXTERNAL_LLM_API_KEY_ENV_VAR, "sk-secret-beta");
    let b = checkpoint::checkpoint_path("gemma-4-e4b", "tr", Some("en"), &sentences, "prompt").unwrap();
    assert_eq!(a, b);
    assert!(!a.to_string_lossy().contains("sk-secret"));
    assert!(a.starts_with(env.temp.path().join("_tmp").join("SubtitleAdder").join("translations")));
}

#[test]
#[serial]
fn test_opencode_v2_chat_and_model_discovery_endpoints() {
    let _env = Env::new();
    for base in [
        "https://opencode.ai/zen/go/v1",
        "https://opencode.ai/zen/go/v1/chat/completions/",
        "https://opencode.ai/inference/go/openai/v1",
        "https://opencode.ai/inference/go/openai/v1/chat/completions",
    ] {
        let chat = normalize_external_chat_url(base).unwrap();
        assert_eq!(chat, "https://opencode.ai/inference/go/openai/v1/chat/completions");
        assert_eq!(external_models_url(&chat).as_deref(), Some("https://opencode.ai/inference/go/v1/models"));
    }
}

#[test]
#[serial]
fn test_opencode_mapping_does_not_rewrite_custom_proxies() {
    let _env = Env::new();
    let custom = "https://proxy.example/zen/go/v1";
    assert_eq!(normalize_external_chat_url(custom).unwrap(), format!("{custom}/chat/completions"));
}

#[test]
#[serial]
fn test_remote_errors_keep_gateway_reason_without_key_and_do_not_retry() {
    for status in [400u16, 403, 429] {
        let _env = Env::new();
        let key = "sk-test-secret";
        let body = format!(r#"{{"error": {{"message": "Global region is required; key {key}"}}}}"#);
        let transport = FakeTransport::new(move |_| {
            Err(HttpFailure::Http { code: status, reason: "Bad Request".into(), body: body.as_bytes().to_vec() })
        });
        let mut options = external("https://opencode.ai/zen/go/v1", Some("deepseek-v4.1-flash"), Some(key), transport.clone());
        options.checkpoint = CheckpointMode::Disabled;
        let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
        let error = translator
            .translate(&[int_sentence(1, 0, 1, "Hello world")], "tr", Some("en"), None, None)
            .unwrap_err()
            .to_string();
        assert_eq!(transport.requests().len(), 1);
        assert!(error.contains(&format!("HTTP {status}")), "{error}");
        assert!(error.contains("Global region is required"));
        assert!(!error.contains(key));
        assert!(error.contains("[redacted]"));
        assert_eq!(
            error,
            format!(
                "Harici LLM sunucusuna ulaşılamadı (https://opencode.ai/inference/go/openai/v1/chat/completions): \
                 HTTP {status}: Global region is required; key [redacted]"
            )
        );
    }
}

#[test]
fn test_http_error_does_not_dump_html_or_large_body() {
    let failure = HttpFailure::Http { code: 502, reason: "Bad Gateway".into(), body: b"<html>".repeat(3000) };
    assert_eq!(http_error_detail(&failure, None), "HTTP Error 502: Bad Gateway");
}

#[test]
#[serial]
fn test_opencode_session_is_stable_and_isolated_from_other_translators() {
    let _env = Env::new();
    let translator = |url: &str| {
        LlamaTranslator::new("madlad-3b", external(url, Some("deepseek-v4.1-flash"), None, FakeTransport::forbid())).unwrap()
    };
    let session = |t: &LlamaTranslator| {
        t.request_headers(None).unwrap().into_iter().find(|(k, _)| k == "x-opencode-session").map(|(_, v)| v)
    };
    let first = translator("https://opencode.ai/zen/go/v1");
    let second = translator("https://opencode.ai/inference/go/openai/v1");
    assert!(session(&first).is_some());
    assert_eq!(session(&first), session(&first));
    assert_ne!(session(&first), session(&second));
    assert_eq!(session(&translator("http://localhost:1234/v1")), None);
}

#[test]
#[serial]
fn test_deepseek_subtitles_disable_thinking_only_on_supported_gateways() {
    for (url, model, disabled) in [
        ("https://opencode.ai/zen/go/v1", "deepseek-v4.1-flash", true),
        ("https://opencode.ai/inference/go/openai/v1", "deepseek-v4-pro", true),
        ("https://api.deepseek.com/v1", "deepseek-flash", true),
        ("https://opencode.ai/inference/go/openai/v1", "glm-5.3-flash", false),
        ("http://localhost:1234/v1", "deepseek-v4.1-flash", false),
        ("https://proxy.example/v1", "deepseek-v4.1-flash", false),
    ] {
        let _env = Env::new();
        let secret = "sk-private-test";
        let transport = FakeTransport::json(
            r#"{"choices": [{"finish_reason": "stop", "message": {"content": "[1] Merhaba.", "reasoning_content": "private reasoning text"}}]}"#,
        );
        let (sink, events) = capture_diagnostics();
        let mut options = external(url, Some(model), Some(secret), transport.clone());
        options.diagnostics = Some(sink);
        options.resolver = identity_resolver();
        let mut client = LlamaTranslator::new("madlad-3b", options).unwrap();
        let reply = client.complete(&[se_translate::ChatMessage::user("private subtitle text")]).unwrap();
        assert_eq!(reply, "[1] Merhaba.");
        let body = transport.requests()[0].json_body().unwrap();
        let thinking = body.get("thinking").map(|v| se_core::json::dumps(v, false));
        assert_eq!(thinking.as_deref(), if disabled { Some(r#"{"type": "disabled"}"#) } else { None }, "{url} {model}");
        let err = diagnostics_text(&events);
        assert!(err.contains("request_start") && err.contains("request_complete"));
        for private in [secret, "private subtitle text", "private reasoning text", "Merhaba"] {
            assert!(!err.contains(private), "{private}");
        }
    }
}

#[test]
#[serial]
fn test_pending_request_reports_wait_then_completion() {
    let _env = Env::new();
    let waited = Arc::new((Mutex::new(false), Condvar::new()));
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (w, rec) = (waited.clone(), events.clone());
    let sink: se_translate::diag::DiagSink = Arc::new(move |event: &str, _fields: &se_core::Map| {
        rec.lock().unwrap().push(event.to_string());
        if event == "request_waiting" {
            *w.0.lock().unwrap() = true;
            w.1.notify_all();
        }
    });
    let w = waited.clone();
    let transport = FakeTransport::new(move |_| {
        let (lock, cv) = &*w;
        let guard = lock.lock().unwrap();
        let (guard, _) = cv.wait_timeout_while(guard, Duration::from_secs(2), |seen| !*seen).unwrap();
        assert!(*guard, "pending request should report that it is waiting");
        Ok(HttpResponse {
            status: 200,
            body: br#"{"choices":[{"finish_reason":"stop","message":{"content":"[1] Merhaba."}}]}"#.to_vec(),
        })
    });
    let mut options = external("https://opencode.ai/zen/go/v1", Some("deepseek-v4.1-flash"), None, transport);
    options.http_trace_interval = Duration::from_millis(10);
    options.diagnostics = Some(sink);
    options.resolver = identity_resolver();
    let mut client = LlamaTranslator::new("madlad-3b", options).unwrap();
    client.complete(&[se_translate::ChatMessage::user("test")]).unwrap();
    let events = events.lock().unwrap();
    assert_eq!(events[0], "request_start");
    assert!(events.contains(&"request_waiting".to_string()));
    assert_eq!(events.last().map(String::as_str), Some("request_complete"));
}

#[test]
#[serial]
fn test_request_failure_diagnostics_do_not_dump_error_body() {
    let _env = Env::new();
    let transport = FakeTransport::new(|_| {
        Err(HttpFailure::Http {
            code: 429,
            reason: "Rate limit".into(),
            body: br#"{"error":{"message":"private response detail"}}"#.to_vec(),
        })
    });
    let (sink, events) = capture_diagnostics();
    let mut options = external("https://opencode.ai/zen/go/v1", Some("deepseek-v4.1-flash"), None, transport);
    options.diagnostics = Some(sink);
    options.resolver = identity_resolver();
    let mut client = LlamaTranslator::new("madlad-3b", options).unwrap();
    assert!(client.complete(&[se_translate::ChatMessage::user("private subtitle")]).is_err());
    let trace = diagnostics_text(&events);
    assert!(trace.contains(r#""http_status": 429"#), "{trace}");
    assert!(!trace.contains("private"));
}

#[test]
#[serial]
fn test_external_llm_failure_names_the_server_not_llama_cpp() {
    let env = Env::new();
    let transport =
        FakeTransport::new(|_| Err(HttpFailure::Other("<urlopen error [Errno 61] Connection refused>".to_string())));
    let mut options = external("192.168.1.50:1234", Some("qwen3"), None, transport);
    options.resolver = identity_resolver();
    let mut translator = LlamaTranslator::new("gemma-4-e4b", options).unwrap();
    let tr = translator.complete(&messages()).unwrap_err().to_string();
    assert!(tr.contains("Harici LLM sunucusuna ulaşılamadı (http://192.168.1.50:1234/v1/chat/completions)"), "{tr}");
    assert!(tr.contains("Connection refused") && !tr.to_lowercase().contains("llama"));
    env.english();
    let en = translator.complete(&messages()).unwrap_err().to_string();
    assert!(en.starts_with("Could not reach the external LLM server (http://192.168.1.50:1234"), "{en}");
    assert!(!en.to_lowercase().contains("llama"));
}

#[test]
#[serial]
fn test_real_connection_refused_is_reported_with_the_server_address() {
    let _env = Env::new();
    // A port nobody listens on (bound then released).
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut translator = LlamaTranslator::new(
        "gemma-4-e4b",
        LlmOptions { external_url: Some(format!("127.0.0.1:{port}")), external_model: Some("m".into()), ..LlmOptions::default() },
    )
    .unwrap();
    let err = translator.complete(&messages()).unwrap_err().to_string();
    assert!(err.starts_with(&format!("Harici LLM sunucusuna ulaşılamadı (http://127.0.0.1:{port}/v1/chat/completions): <urlopen error")), "{err}");
}

#[test]
#[serial]
fn test_real_http_round_trip_and_chat_template_kwargs_retry() {
    let _env = Env::new();
    let bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = bodies.clone();
    let port = serve(move |r| {
        let body = se_core::json::parse(std::str::from_utf8(&r.body).unwrap()).unwrap();
        let has_kwargs = body.get("chat_template_kwargs").is_some();
        rec.lock().unwrap().push(body);
        if has_kwargs {
            Reply { status: 400, headers: vec![], body: br#"{"error": {"message": "unknown field"}}"#.to_vec() }
        } else {
            Reply::json(r#"{"choices": [{"message": {"content": "[1] Merhaba!"}}]}"#)
        }
    });
    let (sink, events) = capture_diagnostics();
    let mut translator = LlamaTranslator::new(
        "madlad-3b",
        LlmOptions {
            external_url: Some(format!("http://127.0.0.1:{port}")),
            external_model: Some("qwen3:8b".into()),
            diagnostics: Some(sink),
            checkpoint: CheckpointMode::Disabled,
            ..LlmOptions::default()
        },
    )
    .unwrap();
    let result = translator.translate(&[sentence(1, 0.0, 1.0, "Hello!")], "tr", Some("en"), None, None).unwrap();
    assert_eq!(result[0].text, "Merhaba!");
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0].get("model").and_then(Value::as_str), Some("qwen3:8b"));
    let names: Vec<String> = events.lock().unwrap().iter().map(|(e, _)| e.clone()).collect();
    assert_eq!(
        names,
        vec!["batch_start", "request_start", "request_failed", "request_start", "request_complete", "batch_complete"]
    );
    let failed = events.lock().unwrap()[2].1.clone();
    assert_eq!(failed.get("http_status").and_then(Value::as_i64), Some(400));
}

// ---- checkpoint hash vectors (Appendix A, generated by the Python engine) ---------------------------------

#[test]
#[serial]
fn test_checkpoint_hash_vectors_match_the_python_engine() {
    let _env = Env::new();
    let path = checkpoint::checkpoint_path("gemma-4-e4b", "tr", None, &[sentence(1, 0.0, 1.5, "Hi")], "prompt").unwrap();
    assert_eq!(path.file_name().unwrap(), "chunk-e8da10fe36d3c2e0fbc5.json");

    // Local gemma-4-e4b tr<-en through translate(): the identity is repr(profile), the system prompt, "" and "".
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = seen.clone();
    let mut first = int_sentence(10, 0, 2, "Hello.");
    first.speaker = Some(0);
    let second = sentence(11, 2.5, 4.25, "  Bye. ");
    let mut translator = LlamaTranslator::new(
        "gemma-4-e4b",
        LlmOptions {
            completion_fn: Some(Box::new(|_| Ok("[1] Merhaba.\n[2] Hoşça kal.".to_string()))),
            completion_keeps_checkpoint: true,
            checkpoint: CheckpointMode::Custom(Box::new(move |m, t, s, sents, identity| {
                let p = checkpoint::checkpoint_path(m, t, s, sents, identity)?;
                rec.lock().unwrap().push(p.file_name().unwrap().to_string_lossy().into_owned());
                Ok(Some(p))
            })),
            ..LlmOptions::default()
        },
    )
    .unwrap();
    translator.translate(&[first, second], "tr", Some("en"), None, None).unwrap();
    assert_eq!(*seen.lock().unwrap(), vec!["chunk-760c53ac3ee049c03168.json".to_string()]);
}

#[test]
#[serial]
fn test_external_checkpoint_hash_vector_matches_the_python_engine() {
    let env = Env::new();
    env.set(EXTERNAL_LLM_URL_ENV_VAR, "192.168.1.42:1234");
    env.set(EXTERNAL_LLM_MODEL_ENV_VAR, "qwen3");
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = seen.clone();
    let mut translator = LlamaTranslator::new(
        "madlad-3b",
        LlmOptions {
            transport: Some(FakeTransport::json(CHAT_REPLY)),
            resolver: identity_resolver(),
            checkpoint: CheckpointMode::Custom(Box::new(move |m, t, s, sents, identity| {
                let p = checkpoint::checkpoint_path(m, t, s, sents, identity)?;
                rec.lock().unwrap().push(p.file_name().unwrap().to_string_lossy().into_owned());
                Ok(Some(p))
            })),
            ..LlmOptions::default()
        },
    )
    .unwrap();
    let result = translator.translate(&[sentence(1, 0.0, 2.0, "Hello!")], "tr", Some("en"), None, None).unwrap();
    assert_eq!(result[0].text, "Merhaba!");
    assert_eq!(*seen.lock().unwrap(), vec!["chunk-71b32932a7fa4503bc88.json".to_string()]);
}

#[test]
#[serial]
fn test_blank_input_reports_full_progress_without_requests() {
    let _env = Env::new();
    let mut translator = with_completion("gemma-4-e4b", Box::new(|_| panic!("no request")));
    let (result, batches, progress) =
        translate_collect(&mut translator, &[sentence(1, 0.0, 1.0, "  "), sentence(2, 1.0, 2.0, "")], "tr", None);
    let result = result.unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].text, "  ");
    assert!(batches.is_empty());
    assert_eq!(progress, vec![1.0]);
}

#[test]
#[serial]
fn test_unknown_or_non_llama_model_without_a_server_is_rejected() {
    let _env = Env::new();
    let err = LlamaTranslator::new("madlad-3b", LlmOptions::default()).err().unwrap().to_string();
    assert_eq!(err, "Bilinmeyen çeviri modeli: madlad-3b.");
    let err = LlamaTranslator::new("base", LlmOptions { external_url: Some("h:1".into()), ..LlmOptions::default() })
        .err()
        .unwrap()
        .to_string();
    assert_eq!(err, "Bilinmeyen çeviri modeli: base.");
}

#[test]
#[serial]
fn test_sentences_are_rebuilt_without_words_and_review_flags() {
    let _env = Env::new();
    let mut s = sentence(5, 1.0, 2.0, "Hello there.");
    s.words = vec![se_core::Word::new(1.0, 1.5, "Hello")];
    s.needs_review = true;
    let mut translator = with_completion("gemma-4-e4b", Box::new(|_| Ok("[1] Merhaba.".to_string())));
    let result: Vec<Sentence> = translator.translate(&[s], "tr", Some("en"), None, None).unwrap();
    assert!(result[0].words.is_empty() && !result[0].needs_review);
}
