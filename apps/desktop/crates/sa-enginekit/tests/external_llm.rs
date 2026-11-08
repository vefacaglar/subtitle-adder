//! External LLM address rules and the connection test (Swift ExternalLLMServerTests / LocalNetworkHostTests /
//! TranslationProviderPresetTests, the key-safety FIXes of UNIFIED.md §3.7).

use std::collections::HashMap;
#[cfg(feature = "ureq-fetch")]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(feature = "ureq-fetch")]
use std::net::TcpListener;
#[cfg(feature = "ureq-fetch")]
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_core::l10n::UiLanguage;
use sa_enginekit::external_llm::{
    ConnectionResult, ExternalLlmServer as S, HttpFetch, HttpRequest, OPENCODE_GO_CHAT_URL, OPENCODE_GO_MODELS_URL,
    opencode_session_id, parse_url,
};
#[cfg(feature = "ureq-fetch")]
use sa_enginekit::external_llm::UreqFetch;
use sa_enginekit::launch::ExternalLlm;
use sa_enginekit::presets::TranslationProviderPreset;

// MARK: normalization

#[test]
fn normalizes_addresses() {
    let n = S::normalize_base_url;
    assert_eq!(n("192.168.1.50:1234").as_deref(), Some("http://192.168.1.50:1234"));
    assert_eq!(n("  http://mac-studio.local:11434/api/chat  ").as_deref(), Some("http://mac-studio.local:11434"));
    assert_eq!(n("192.168.1.50:1234/v1/chat/completions").as_deref(), Some("http://192.168.1.50:1234"));
    assert_eq!(n("HTTPS://host.lan/V1/Models/").as_deref(), Some("HTTPS://host.lan"));
    assert_eq!(n("host:1/chat/completions").as_deref(), Some("http://host:1"));
    assert_eq!(n("host:1/api/tags").as_deref(), Some("http://host:1"));
    assert_eq!(n("http://host:1/").as_deref(), Some("http://host:1"));
    assert_eq!(n("host:1/v1").as_deref(), Some("http://host:1/v1"));
    assert_eq!(n("host:1/v1/v1").as_deref(), Some("http://host:1/v1/v1"));
    assert_eq!(n("host:1/chat/completions/chat/completions").as_deref(), Some("http://host:1/chat/completions"));
    for blank in ["", "   ", "http://", "https://", "/v1", "a b"] {
        assert_eq!(n(blank), None, "{blank:?}");
    }
}

#[test]
fn a_public_address_typed_without_a_scheme_gets_https() {
    let n = S::normalize_base_url;
    for (input, expected) in [
        ("api.openai.com/v1", "https://api.openai.com/v1"),
        ("api.openai.com", "https://api.openai.com"),
        ("  api.groq.com/openai/v1/chat/completions ", "https://api.groq.com/openai/v1"),
        ("openrouter.ai/api/v1/models", "https://openrouter.ai/api/v1"),
        ("llm.example.com:8443", "https://llm.example.com:8443"),
        ("8.8.8.8:1234", "https://8.8.8.8:1234"),
        ("172.32.0.1:80", "https://172.32.0.1:80"),
        ("100.128.0.1", "https://100.128.0.1"),
    ] {
        assert_eq!(n(input).as_deref(), Some(expected), "{input}");
    }
}

#[test]
fn local_addresses_typed_without_a_scheme_keep_plain_http() {
    for input in [
        "localhost:11434",
        "127.0.0.1:1234/v1",
        "[::1]:8080",
        "10.0.0.2:8080",
        "172.16.5.5:1234",
        "100.100.1.1:1234",
        "mac-studio.local:11434",
        "nas.lan",
        "my-mac:1234",
    ] {
        assert_eq!(S::normalize_base_url(input), Some(format!("http://{input}")), "{input}");
    }
}

#[test]
fn a_scheme_the_user_typed_is_never_changed() {
    for input in ["http://api.openai.com/v1", "https://localhost:1234", "HTTP://example.com"] {
        assert_eq!(S::normalize_base_url(input).as_deref(), Some(input));
    }
}

#[test]
fn preset_addresses_survive_normalization() {
    for preset in TranslationProviderPreset::ALL.iter().filter(|p| !p.base_url.is_empty()) {
        assert_eq!(S::normalize_base_url(preset.base_url).as_deref(), Some(preset.base_url), "{}", preset.id);
    }
}

#[test]
fn pasted_endpoints_keep_the_version_path_of_nested_providers() {
    let n = S::normalize_base_url;
    assert_eq!(n("https://api.groq.com/openai/v1/chat/completions").as_deref(), Some("https://api.groq.com/openai/v1"));
    assert_eq!(n("https://api.groq.com/openai/v1/models/").as_deref(), Some("https://api.groq.com/openai/v1"));
    assert_eq!(n("https://openrouter.ai/api/v1/chat/completions").as_deref(), Some("https://openrouter.ai/api/v1"));
    assert_eq!(n("https://opencode.ai/zen/go/v1/models").as_deref(), Some("https://opencode.ai/zen/go/v1"));
    let gemini = "https://generativelanguage.googleapis.com/v1beta/openai";
    assert_eq!(n(&format!("{gemini}/chat/completions")).as_deref(), Some(gemini));
    assert_eq!(n("https://api.openai.com/v1/chat/completions").as_deref(), Some("https://api.openai.com"));
}

#[test]
fn hosts_are_extracted_without_reinterpretation() {
    assert_eq!(S::host("http://1.2.3:80/x").as_deref(), Some("1.2.3"));
    assert_eq!(S::host("http://10.1").as_deref(), Some("10.1"));
    assert_eq!(S::host("http://999.1.1.1").as_deref(), Some("999.1.1.1"));
    assert_eq!(S::host("HTTP://Mac.LOCAL:1/").as_deref(), Some("Mac.LOCAL"));
    assert_eq!(S::host("http://user:pw@host:8/").as_deref(), Some("host"));
    assert_eq!(S::host("http://[fe80::1]:8080/v1").as_deref(), Some("fe80::1"));
    assert_eq!(S::host("http://a b"), None);
    assert_eq!(S::host("http://host:port"), None);
    assert_eq!(S::host("no scheme"), None);
    let parts = parse_url("https://x.test:8443/p/q?a=1#f").unwrap();
    assert_eq!((parts.scheme, parts.port, parts.path, parts.query, parts.fragment), ("https", Some("8443"), "/p/q", Some("a=1"), Some("f")));
}

// MARK: local network hosts

#[test]
fn local_network_hosts() {
    for host in [
        "localhost", "127.0.0.1", "127.8.9.10", "10.0.0.2", "192.168.1.50", "172.16.0.1", "172.31.255.255", "169.254.3.4",
        "100.64.0.1", "100.127.1.1", "mac-mini", "mac.local", "Mac.LOCAL", "nas.lan", "box.home.arpa", "app.localhost", "::1",
        "fe80::1", "fd12:3456::1", "fc00::2", "[::1]",
    ] {
        assert!(S::is_local_network_host(host), "{host}");
    }
    for host in [
        "", "api.openai.com", "openrouter.ai", "generativelanguage.googleapis.com", "8.8.8.8", "172.32.0.1", "172.15.0.1",
        "100.63.0.1", "100.128.0.1", "192.169.1.1", "11.0.0.1", "2001:db8::1", "999.1.1.1", "1.2.3", "evil-local.com",
    ] {
        assert!(!S::is_local_network_host(host), "{host}");
    }
}

#[test]
fn the_server_address_decides() {
    let llm = |url: &str| ExternalLlm { url: url.into(), ..Default::default() };
    assert!(llm("10.0.0.2:11434").is_on_local_network());
    assert!(llm("http://localhost:1234/v1/models").is_on_local_network());
    assert!(!llm("https://api.openai.com/v1").is_on_local_network());
    assert!(!llm("https://generativelanguage.googleapis.com/v1beta/openai").is_on_local_network());
    assert!(!llm("  ").is_on_local_network());
}

#[test]
fn a_key_is_never_sent_in_the_clear_to_a_public_host() {
    assert!(S::is_insecure_key_destination("http://api.openai.com/v1"));
    assert!(S::is_insecure_key_destination("HTTP://8.8.8.8:1234"));
    assert!(!S::is_insecure_key_destination("https://api.openai.com/v1"));
    assert!(!S::is_insecure_key_destination("http://192.168.1.5:1234"));
    assert!(!S::is_insecure_key_destination("http://mac.local:11434"));
}

// MARK: model lists and endpoints

#[test]
fn parses_ollama_and_openai_model_lists() {
    assert_eq!(
        S::parse_suitable_models(r#"{"models": [{"name": "gemma4:e4b"}, {"name": "nomic-embed-text:latest"}, {"name": "qwen3:14b"}]}"#),
        ["gemma4:e4b", "qwen3:14b"]
    );
    assert_eq!(
        S::parse_suitable_models(
            r#"{"object": "list", "data": [{"id": "qwen/qwen3-8b"}, {"id": "all-MiniLM-L6"}, {"id": "bge-m3"}, {"id": " QWEN/qwen3-8b "}, {"name": "named"}, {"id": ""}]}"#
        ),
        ["qwen/qwen3-8b", "named"]
    );
    assert!(S::parse_suitable_models("[]").is_empty());
    assert!(S::parse_suitable_models("nope").is_empty());
}

#[test]
fn model_endpoints_follow_the_shape_of_the_address() {
    assert_eq!(S::model_endpoints("http://host:1"), ["http://host:1/v1/models", "http://host:1/models", "http://host:1/api/tags"]);
    assert_eq!(S::model_endpoints("http://host:1/"), S::model_endpoints("http://host:1"));
    assert_eq!(S::model_endpoints("https://api.openai.com/v1"), ["https://api.openai.com/v1/models"]);
    assert_eq!(S::model_endpoints("https://api.groq.com/openai/v1"), ["https://api.groq.com/openai/v1/models"]);
    let gemini = "https://generativelanguage.googleapis.com/v1beta/openai";
    assert_eq!(S::model_endpoints(gemini), [format!("{gemini}/models")]);
    assert_eq!(S::model_endpoints("HTTPS://X.test/V1"), ["HTTPS://X.test/V1/models"]);
    assert_eq!(S::model_endpoints("https://x.test/v1/chat/completions"), ["https://x.test/v1/models"]);
    assert_eq!(S::model_endpoints("https://x.test/chat/completions"), S::model_endpoints("https://x.test"));
    assert_eq!(S::model_endpoints("https://x.test/v1beta/models"), ["https://x.test/v1beta/models"]);
}

#[test]
fn chat_endpoint_follows_the_engines_rule() {
    assert_eq!(S::chat_endpoint("http://host:1"), "http://host:1/v1/chat/completions");
    assert_eq!(S::chat_endpoint("http://host:1/"), "http://host:1/v1/chat/completions");
    assert_eq!(S::chat_endpoint("https://api.openai.com/v1"), "https://api.openai.com/v1/chat/completions");
    assert_eq!(S::chat_endpoint("https://api.groq.com/openai/v1"), "https://api.groq.com/openai/v1/chat/completions");
    let gemini = "https://generativelanguage.googleapis.com/v1beta/openai";
    assert_eq!(S::chat_endpoint(gemini), format!("{gemini}/chat/completions"));
    assert_eq!(S::chat_endpoint("https://x.test/v1/chat/completions"), "https://x.test/v1/chat/completions");
}

#[test]
fn recognizes_the_open_code_go_addresses() {
    for url in [
        "https://opencode.ai/zen/go/v1",
        "https://opencode.ai/zen/go/v1/chat/completions",
        "https://opencode.ai/inference/go/v1",
        "https://opencode.ai/inference/go/openai/v1",
        "https://opencode.ai/inference/go/openai/v1/chat/completions",
        "https://OpenCode.ai/inference/go/openai/v1/",
    ] {
        assert!(S::is_opencode_go(url), "{url}");
        assert_eq!(S::model_endpoints(url), [OPENCODE_GO_MODELS_URL], "{url}");
        assert_eq!(S::chat_endpoint(url), OPENCODE_GO_CHAT_URL, "{url}");
    }
    for url in [
        "http://opencode.ai/inference/go/openai/v1",
        "https://opencode.ai/v1",
        "https://opencode.ai/zen/go/v1?x=1",
        "https://opencode.ai/zen/go/v1#top",
        "https://opencode.ai:8443/zen/go/v1",
        "https://opencode.ai:443/zen/go/v1",
        "https://proxy.example.com/inference/go/openai/v1",
        "https://api.openai.com/v1",
        "not a url",
        "",
    ] {
        assert!(!S::is_opencode_go(url), "{url}");
    }
    assert_eq!(S::model_endpoints("https://proxy.example.com/zen/go/v1"), ["https://proxy.example.com/zen/go/v1/models"]);
    assert_eq!(S::chat_endpoint("https://proxy.example.com/zen/go/v1"), "https://proxy.example.com/zen/go/v1/chat/completions");
}

#[test]
fn gateway_error_messages_are_read_from_the_body() {
    let d = S::http_error_detail;
    assert_eq!(d(429, r#"{"error": {"message": "Rate limited,\n slow  down"}}"#, ""), "HTTP 429: Rate limited, slow down");
    assert_eq!(d(402, r#"{"error": "Out of credits"}"#, ""), "HTTP 402: Out of credits");
    assert_eq!(d(500, r#"{"message": "boom"}"#, ""), "HTTP 500: boom");
    assert_eq!(d(502, "<html><body>Bad gateway</body></html>", ""), "HTTP 502");
    assert_eq!(d(503, "", ""), "HTTP 503");
    assert_eq!(d(400, r#"{"error": {"message": 5}}"#, ""), "HTTP 400");
    assert_eq!(d(400, r#"{"error": {"message": "  "}}"#, ""), "HTTP 400");
    assert_eq!(d(400, r#"{"error": null, "message": "ignored"}"#, ""), "HTTP 400");
    assert_eq!(d(401, r#"{"error": {"message": "bad key sk-secret"}}"#, " sk-secret "), "HTTP 401: bad key [redacted]");
    let long = format!(r#"{{"error": "{}"}}"#, "x".repeat(3000));
    assert_eq!(d(500, &long, "").chars().count(), 1000);
    // Characters as Swift counts them: a decomposed `é` is one.
    let decomposed = format!(r#"{{"error": "{}"}}"#, "e\u{301}".repeat(1500));
    let cut = d(500, &decomposed, "");
    assert_eq!(cut.chars().count(), "HTTP 500: ".len() + 2 * (1000 - "HTTP 500: ".len()));
    let truncated = format!(r#"{{"error": "{}"}}"#, "y".repeat(9000));
    assert_eq!(d(500, &truncated, ""), "HTTP 500");
}

#[test]
fn the_refusal_message_is_localized() {
    assert_eq!(S::unauthorized_message_in(401, UiLanguage::Tr), "API anahtarı geçersiz veya yetkisiz (HTTP 401).");
    assert_eq!(S::unauthorized_message_in(403, UiLanguage::En), "Invalid or unauthorized API key (HTTP 403).");
}

#[test]
fn new_transport_texts_exist_in_both_languages() {
    use sa_core::l10n::{CatalogChain, localize_in};
    use sa_enginekit::external_llm::strings::*;
    for key in [HOST_NOT_FOUND, CONNECTION_REFUSED, TLS_FAILED, CONNECTION_FAILED, INSECURE_KEY] {
        assert!(CatalogChain::native().has_both_languages(key), "{key}");
        assert_eq!(localize_in(CatalogChain::native(), UiLanguage::Tr, key, &[]), key);
        assert_ne!(localize_in(CatalogChain::native(), UiLanguage::En, key, &[]), key);
    }
    assert_eq!(
        localize_in(CatalogChain::native(), UiLanguage::En, INSECURE_KEY, &[]),
        "The API key is not sent over an unencrypted (http) connection; enter the address with https://."
    );
}

// MARK: the connection test over a recording fetch

type Table = HashMap<&'static str, (u16, &'static str)>;

#[derive(Default)]
struct Recorder {
    requests: Mutex<Vec<HttpRequest>>,
    table: Table,
    down: Option<Option<String>>,
}

impl Recorder {
    fn new(table: &[(&'static str, (u16, &'static str))]) -> Self {
        Recorder { table: table.iter().cloned().collect(), ..Default::default() }
    }
    fn down(error: Option<String>) -> Self {
        Recorder { down: Some(error), ..Default::default() }
    }
    fn urls(&self) -> Vec<String> {
        self.requests.lock().iter().map(|r| r.url.clone()).collect()
    }
    fn methods(&self) -> Vec<&'static str> {
        self.requests.lock().iter().map(|r| r.method).collect()
    }
}

impl HttpFetch for Recorder {
    fn fetch(&self, request: HttpRequest, _cancel: &CancellationToken) -> Result<(u16, String), Option<String>> {
        let path = parse_url(&request.url).map(|p| p.path.to_owned()).unwrap_or_default();
        self.requests.lock().push(request);
        if let Some(e) = &self.down {
            return Err(e.clone());
        }
        let (status, body) = self.table.get(path.as_str()).copied().unwrap_or((404, ""));
        Ok((status, body.to_owned()))
    }
}

fn test(raw: &str, key: &str, model: &str, preset: Option<&str>, fetch: &dyn HttpFetch) -> ConnectionResult {
    S::test_connection(raw, key, model, preset, fetch, &CancellationToken::new())
}

const OPENAI_LIST: (u16, &str) = (200, r#"{"object": "list", "data": [{"id": "qwen3-8b"}, {"id": "gemma-3"}]}"#);

#[test]
fn stops_at_first_endpoint_with_models() {
    let r = Recorder::new(&[("/v1/models", OPENAI_LIST)]);
    let ConnectionResult::Reachable { host, latency_ms, models } = test("192.168.1.50:11434/api/chat", "", "", None, &r) else { panic!() };
    assert_eq!(host, "192.168.1.50");
    assert!(latency_ms >= 1);
    assert_eq!(models, ["qwen3-8b", "gemma-3"]);
    assert_eq!(r.urls(), ["http://192.168.1.50:11434/v1/models"]);
}

#[test]
fn falls_back_to_ollamas_own_list() {
    let r = Recorder::new(&[("/api/tags", (200, r#"{"models": [{"name": "gemma4:e4b"}]}"#))]);
    assert_eq!(test("http://mac.local:11434", "", "", None, &r).models(), ["gemma4:e4b"]);
    assert_eq!(r.urls(), ["http://mac.local:11434/v1/models", "http://mac.local:11434/models", "http://mac.local:11434/api/tags"]);
    assert_eq!(r.methods(), ["GET", "GET", "GET"]);
}

#[test]
fn an_openai_root_is_asked_only_for_its_model_list() {
    let r = Recorder::new(&[("/openai/v1/models", OPENAI_LIST)]);
    assert_eq!(test("https://api.groq.com/openai/v1", "gsk-1", "", None, &r).models(), ["qwen3-8b", "gemma-3"]);
    assert_eq!(r.urls(), ["https://api.groq.com/openai/v1/models"]);
}

#[test]
fn every_request_carries_the_headers_and_the_key_only_when_there_is_one() {
    let r = Recorder::new(&[]);
    let _ = test("https://x.test/v1", "  sk-secret \n", "", None, &r);
    let requests = r.requests.lock().clone();
    assert_eq!(requests.len(), 2);
    for req in &requests {
        assert_eq!(req.header("Authorization"), Some("Bearer sk-secret"));
        assert_eq!(req.header("User-Agent"), Some("SubtitleAdder/1.0"));
        assert_eq!(req.header("Accept"), Some("application/json"));
        assert_eq!(req.header("x-opencode-session"), None);
    }
    let keyless = Recorder::new(&[]);
    let _ = test("http://host:1", "   ", "", None, &keyless);
    let requests = keyless.requests.lock().clone();
    assert_eq!(requests.len(), 4);
    for req in &requests {
        assert_eq!(req.header("Authorization"), None);
        assert_eq!(req.header("User-Agent"), Some("SubtitleAdder/1.0"));
    }
}

#[test]
fn a_gateway_without_a_model_list_is_probed_with_one_chat_request() {
    let r = Recorder::new(&[("/inference/go/openai/v1/chat/completions", (200, r#"{"choices": []}"#))]);
    let ConnectionResult::Reachable { host, models, .. } =
        test("https://opencode.ai/inference/go/openai/v1", "sk-oc", "kimi-k3.1-pro", Some("opencode-go"), &r)
    else {
        panic!()
    };
    assert_eq!(host, "opencode.ai");
    assert!(models.is_empty());
    assert_eq!(r.urls(), [OPENCODE_GO_MODELS_URL, OPENCODE_GO_CHAT_URL]);
    assert_eq!(r.methods(), ["GET", "POST"]);
    let requests = r.requests.lock().clone();
    let probe = &requests[1];
    assert_eq!(probe.header("Authorization"), Some("Bearer sk-oc"));
    assert_eq!(probe.header("Content-Type"), Some("application/json"));
    assert_eq!(
        probe.body.as_deref(),
        Some(r#"{"max_tokens":1,"messages":[{"content":"hi","role":"user"}],"model":"kimi-k3.1-pro","stream":false}"#)
    );
    // Both requests carry the same session header, a valid UUID.
    let sessions: Vec<_> = requests.iter().map(|q| q.header("x-opencode-session").map(str::to_owned)).collect();
    assert_eq!(sessions[0].as_deref(), Some(opencode_session_id()));
    assert_eq!(sessions[0], sessions[1]);
    assert!(uuid::Uuid::parse_str(opencode_session_id()).is_ok());
}

#[test]
fn a_failed_request_reports_the_gateway_message() {
    let r = Recorder::new(&[
        ("/inference/go/v1/models", (500, r#"{"error": {"message": "upstream down"}}"#)),
        ("/inference/go/openai/v1/chat/completions", (503, r#"{"error": {"message": "model sk-oc offline"}}"#)),
    ]);
    assert_eq!(
        test("https://opencode.ai/inference/go/openai/v1", "sk-oc", "", None, &r),
        ConnectionResult::Unreachable { reason: "HTTP 503: model [redacted] offline".into() }
    );
}

#[test]
fn the_probe_model_is_the_chosen_one_then_the_presets_first_then_a_generic_one() {
    let probed = |model: &str, preset: Option<&str>| {
        let r = Recorder::new(&[("/v1/chat/completions", (200, "{}"))]);
        let _ = test("http://host:1", "", model, preset, &r);
        let body = r.requests.lock().last().unwrap().body.clone().unwrap();
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["model"].as_str().unwrap().to_owned()
    };
    assert_eq!(probed(" glm-5 ", Some("deepseek")), "glm-5");
    assert_eq!(probed("", Some("deepseek")), "deepseek-chat");
    assert_eq!(probed("", Some("custom")), "gpt-4o-mini");
    assert_eq!(probed("", None), "gpt-4o-mini");
    assert_eq!(probed("", Some("no-such-preset")), "gpt-4o-mini");
}

#[test]
fn a_bare_servers_probe_goes_to_the_v1_chat_endpoint() {
    let r = Recorder::new(&[]);
    let _ = test("host:1234", "", "", None, &r);
    assert_eq!(r.urls().last().map(String::as_str), Some("http://host:1234/v1/chat/completions"));
    assert_eq!(r.methods(), ["GET", "GET", "GET", "POST"]);
}

#[test]
fn a_refused_key_stops_the_test_at_once() {
    let r = Recorder::new(&[("/v1/models", (401, r#"{"error": "no"}"#))]);
    assert_eq!(test("http://host:1", "bad", "", None, &r), ConnectionResult::Unauthorized { status: 401 });
    assert_eq!(r.requests.lock().len(), 1);
    let second = Recorder::new(&[("/models", (403, ""))]);
    assert_eq!(test("http://host:1", "", "", None, &second), ConnectionResult::Unauthorized { status: 403 });
    assert_eq!(second.urls(), ["http://host:1/v1/models", "http://host:1/models"]);
}

#[test]
fn the_chat_probe_can_refuse_the_key_too() {
    let r = Recorder::new(&[("/v1/chat/completions", (401, ""))]);
    assert_eq!(test("https://x.test/v1", "bad", "", None, &r), ConnectionResult::Unauthorized { status: 401 });
    assert_eq!(r.methods(), ["GET", "POST"]);
}

#[test]
fn reachable_without_suitable_models() {
    let r = Recorder::new(&[("/v1/models", (200, r#"{"data": [{"id": "nomic-embed-text"}]}"#)), ("/api/tags", (200, r#"{"models": []}"#))]);
    let ConnectionResult::Reachable { models, .. } = test("host:1", "", "", None, &r) else { panic!() };
    assert!(models.is_empty());
    assert_eq!(r.methods(), ["GET", "GET", "GET"]);
}

#[test]
fn unreachable_and_invalid() {
    let r = Recorder::down(Some("Bağlantı reddedildi.".into()));
    let ConnectionResult::Unreachable { reason } = test("host:1", "", "", None, &r) else { panic!() };
    assert!(!reason.is_empty());
    assert_eq!(r.requests.lock().len(), 4);
    assert_eq!(test("  ", "", "", None, &r), ConnectionResult::InvalidAddress);
    assert_eq!(r.requests.lock().len(), 4);
}

#[test]
fn an_error_status_everywhere_is_reported_as_unreachable() {
    let r = Recorder::new(&[("/v1/models", (500, "")), ("/v1/chat/completions", (404, ""))]);
    assert_eq!(test("https://x.test/v1", "", "", None, &r), ConnectionResult::Unreachable { reason: "HTTP 404".into() });
}

#[test]
fn a_timeout_is_reported_in_the_apps_language() {
    let ConnectionResult::Unreachable { reason } = test("host:1", "", "", None, &Recorder::down(None)) else { panic!() };
    assert!(reason == "Zaman aşımı" || reason == "Timed out", "{reason}");
}

#[test]
fn a_key_for_a_public_plain_http_address_is_refused_before_any_request() {
    let r = Recorder::new(&[("/v1/models", OPENAI_LIST)]);
    assert_eq!(test("http://api.openai.com/v1", "sk-1", "", None, &r), ConnectionResult::InsecureKeyTransport);
    assert!(r.requests.lock().is_empty());
    // Without a key, or on the local network, the test runs.
    assert!(matches!(test("http://api.openai.com/v1", "  ", "", None, &r), ConnectionResult::Reachable { .. }));
    assert!(matches!(test("http://10.0.0.2:1234", "sk-1", "", None, &r), ConnectionResult::Reachable { .. }));
}

struct Sleepy;

impl HttpFetch for Sleepy {
    fn fetch(&self, _request: HttpRequest, cancel: &CancellationToken) -> Result<(u16, String), Option<String>> {
        for _ in 0..600 {
            if cancel.is_cancelled() {
                return Err(Some("cancelled".into()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok((200, String::new()))
    }
}

#[test]
fn a_cancelled_probe_is_not_reported_as_unreachable() {
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    let probe = std::thread::spawn(move || S::test_connection("host:1", "", "", None, &Sleepy, &c));
    std::thread::sleep(Duration::from_millis(50));
    let started = std::time::Instant::now();
    cancel.cancel();
    assert_eq!(probe.join().unwrap(), ConnectionResult::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
}

// MARK: the real ureq fetcher against a local stub server

#[cfg(feature = "ureq-fetch")]
/// A tiny HTTP/1.1 server answering `path -> (status, body)` and recording request heads.
fn stub_server(table: Vec<(&'static str, u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let heads = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&heads);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
                head.push_str(&line);
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
            recorded.lock().push(head);
            let (status, text) = table.iter().find(|(p, _, _)| *p == path).map_or((404, "no"), |(_, s, b)| (*s, *b));
            // A 3xx entry's body is the redirect target.
            let location = if (300..400).contains(&status) { format!("Location: {text}\r\n") } else { String::new() };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = stream.flush();
        }
    });
    (base, heads)
}

#[cfg(feature = "ureq-fetch")]
#[test]
fn the_ureq_fetch_returns_the_status_instead_of_failing() {
    let (base, heads) = stub_server(vec![("/v1/models", 404, "no"), ("/models", 401, "denied"), ("/api/tags", 200, r#"{"models": [{"name": "m1"}]}"#)]);
    let cancel = CancellationToken::new();
    let missing = UreqFetch.fetch(S::request(&format!("{base}/v1/models"), "GET", "", None), &cancel).unwrap();
    assert_eq!(missing, (404, "no".to_owned()));
    let denied = UreqFetch.fetch(S::request(&format!("{base}/models"), "GET", "k", None), &cancel).unwrap();
    assert_eq!(denied.0, 401);
    let head = heads.lock()[1].to_ascii_lowercase();
    assert!(head.contains("authorization: bearer k"), "{head}");
    assert!(head.contains("user-agent: subtitleadder/1.0"), "{head}");
    let (base, _) = stub_server(vec![("/api/tags", 200, r#"{"models": [{"name": "m1"}]}"#)]);
    assert_eq!(S::test_connection(&base, "", "", None, &UreqFetch, &cancel).models(), ["m1"]);
}

#[cfg(feature = "ureq-fetch")]
#[test]
fn the_key_follows_a_redirect_on_the_same_host_only() {
    // Same host (another path): URLSession and reqwest keep `Authorization`.
    let (target, target_heads) = stub_server(vec![("/v1/models", 200, r#"{"data": [{"id": "m1"}]}"#)]);
    let leaked: &'static str = Box::leak(format!("{target}/v1/models").into_boxed_str());
    let (base, _) = stub_server(vec![("/old/models", 307, leaked)]);
    let cancel = CancellationToken::new();
    let (status, _) = UreqFetch.fetch(S::request(&format!("{base}/old/models"), "GET", "sk-1", None), &cancel).unwrap();
    assert_eq!(status, 200);
    let head = target_heads.lock()[0].to_ascii_lowercase();
    assert!(head.contains("authorization: bearer sk-1"), "{head}");

    // Another host: the key is dropped. 127.0.0.2 is a separate loopback host on Windows and Linux (not bindable on
    // a stock macOS, where this half is skipped).
    let Ok(other) = TcpListener::bind("127.0.0.2:0") else { return };
    let other_port = other.local_addr().unwrap().port();
    let other_heads = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = Arc::clone(&other_heads);
    std::thread::spawn(move || {
        for stream in other.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            recorded.lock().push(head);
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}");
        }
    });
    let leaked: &'static str = Box::leak(format!("http://127.0.0.2:{other_port}/v1/models").into_boxed_str());
    let (base, _) = stub_server(vec![("/away", 302, leaked)]);
    let (status, _) = UreqFetch.fetch(S::request(&format!("{base}/away"), "GET", "sk-1", None), &cancel).unwrap();
    assert_eq!(status, 200);
    let head = other_heads.lock()[0].to_ascii_lowercase();
    assert!(!head.contains("authorization"), "{head}");
    assert!(head.contains("user-agent: subtitleadder/1.0"), "{head}");
}

#[cfg(feature = "ureq-fetch")]
#[test]
fn the_ureq_fetch_reports_a_refused_connection_in_the_apps_language() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let result = UreqFetch.fetch(S::request(&format!("http://127.0.0.1:{port}/v1/models"), "GET", "", None), &CancellationToken::new());
    let Err(Some(reason)) = result else { panic!("{result:?}") };
    assert!(!reason.is_empty() && !reason.contains("os error"), "{reason}");
}

#[cfg(feature = "ureq-fetch")]
#[test]
fn the_default_fetch_shares_one_agent() {
    assert!(std::ptr::eq(UreqFetch::shared_agent("http://a"), UreqFetch::shared_agent("http://b")));
    assert!(std::ptr::eq(UreqFetch::shared_agent("https://a"), UreqFetch::shared_agent("HTTPS://b")));
}

// MARK: Windows `.local` resolution of the launch URL

#[test]
fn non_local_hosts_are_passed_through_untouched() {
    let cancel = CancellationToken::new();
    for url in ["http://192.168.1.5:1234/v1", "https://api.openai.com/v1", "not a url"] {
        assert_eq!(S::resolve_local_host(url, false, &cancel), url);
    }
}

// MARK: presets

#[test]
fn presets_keep_their_order_and_table() {
    let ids: Vec<_> = TranslationProviderPreset::ALL.iter().map(|p| p.id).collect();
    assert_eq!(ids, ["opencode-go", "openrouter", "openai", "deepseek", "groq", "gemini", "lan", "custom"]);
    let lan = TranslationProviderPreset::preset(Some("LAN"));
    assert_eq!((lan.id, lan.base_url, lan.requires_api_key), ("lan", "http://127.0.0.1:11434", false));
    assert_eq!(lan.suggested_models, ["gemma4:e4b", "qwen3.5:4b"]);
    assert!(lan.translates_in_background_by_default());
    assert_eq!(TranslationProviderPreset::preset(Some("OpenAI")).id, "openai");
    for id in [None, Some(""), Some("unknown")] {
        assert_eq!(TranslationProviderPreset::preset(id).id, "custom");
    }
    let localized: Vec<_> = TranslationProviderPreset::ALL.iter().filter(|p| p.localizes_name).map(|p| p.id).collect();
    assert_eq!(localized, ["lan", "custom"]);
    assert_eq!(lan.name(Some(UiLanguage::En)), "Local LAN (Ollama / LM Studio)");
    assert_eq!(TranslationProviderPreset::preset(None).name(Some(UiLanguage::En)), "Custom (OpenAI Compatible)");
    assert_eq!(TranslationProviderPreset::preset(None).name(Some(UiLanguage::Tr)), "Özel (OpenAI Uyumlu)");
    assert_eq!(TranslationProviderPreset::preset(Some("groq")).name(Some(UiLanguage::Tr)), "Groq");
    assert_eq!(TranslationProviderPreset::preset(Some("deepseek")).default_model(), "deepseek-chat");
    assert_eq!(TranslationProviderPreset::preset(None).default_model(), "");
    let gemini = TranslationProviderPreset::preset(Some("gemini"));
    assert_eq!(gemini.base_url, "https://generativelanguage.googleapis.com/v1beta/openai");
    assert_eq!(gemini.suggested_models, ["gemini-2.5-flash", "gemini-2.5-pro"]);
    assert!(!TranslationProviderPreset::preset(Some("openai")).translates_in_background_by_default());
}
