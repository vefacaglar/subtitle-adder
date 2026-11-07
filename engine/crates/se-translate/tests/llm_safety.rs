//! Port of `tests/test_external_llm_safety.py`: where an API key may travel, what the subnet scan sends, and
//! what strict offline mode allows. The CLI-level tests of that file (`cli.main([...])`) belong to se-cli and
//! are covered there; here the same rules are checked on `check_external_llm` / `LlamaTranslator`.

mod common;

use std::io;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use common::*;
use serial_test::serial;

use se_translate::external::{api_key_transport_allowed, check_external_llm, is_local_network_host};
use se_translate::http::{HttpFailure, HttpRequest, redirect_request};
use se_translate::lan::{LanNet, discover_lan_server_ip_with, resolve_lan_url_with};
use se_translate::llm::LlmOptions;
use se_translate::LlamaTranslator;

const KEY: &str = "sk-test-secret-key-123";
const KEY_ERROR_TR: &str = "API anahtarı şifrelenmemiş bağlantıyla (http) gönderilmez; adresi https:// ile girin.";
const KEY_ERROR_EN: &str = "The API key is not sent over an unencrypted (http) connection; enter the address with https://.";

const LOCAL_HOSTS: &[&str] = &[
    "localhost", "LOCALHOST", "app.localhost", "127.0.0.1", "127.1", "0x7f.1", "2130706433", "[::1]", "::1",
    "10.1.2.3", "172.16.0.5", "172.31.255.254", "192.168.1.42", "100.64.0.1", "100.127.255.254", "169.254.10.20",
    "fd12:3456::1", "fc00::1", "fe80::1", "fe80::1%en0", "::ffff:192.168.1.5",
    "mac-studio.local", "MAC-STUDIO.LOCAL", "mac-studio.local.", "mac-studio", "ollama",
];
const REMOTE_HOSTS: &[&str] = &[
    "api.openai.com", "opencode.ai", "openrouter.ai", "generativelanguage.googleapis.com", "example.com",
    "8.8.8.8", "134744072", "172.32.0.1", "172.15.255.255", "100.128.0.1", "100.63.255.255", "192.169.0.1",
    "11.0.0.1", "169.255.0.1", "2001:4860:4860::8888", "fec0::1", "::ffff:8.8.8.8",
    "10.0.0.1.nip.io", "localhost.example.com", "mac-studio.local.example.com", "",
];

#[test]
fn test_local_network_hosts() {
    for host in LOCAL_HOSTS {
        assert!(is_local_network_host(Some(host)), "{host}");
    }
}

#[test]
fn test_internet_hosts() {
    for host in REMOTE_HOSTS {
        assert!(!is_local_network_host(Some(host)), "{host}");
    }
    assert!(!is_local_network_host(None));
}

#[test]
fn test_api_key_transport_rule() {
    for (url, allowed) in [
        ("https://api.openai.com/v1/chat/completions", true),
        ("HTTPS://opencode.ai/zen/go/v1/chat/completions", true),
        ("http://192.168.1.42:1234/v1/chat/completions", true),
        ("http://mac-studio.local:8080/v1/chat/completions", true),
        ("http://[fd00::1]:8080/v1/chat/completions", true),
        ("http://ollama:11434/api/chat", true),
        ("http://api.openai.com/v1/chat/completions", false),
        ("http://203.0.113.9:1234/v1/chat/completions", false),
        ("http://[2001:db8::1]/v1", false),
        ("ftp://192.168.1.42/v1", false),
        ("not a url", false),
    ] {
        assert_eq!(api_key_transport_allowed(url), allowed, "{url}");
    }
}

fn keyed(url: &str, transport: Arc<FakeTransport>) -> se_core::Result<LlamaTranslator> {
    LlamaTranslator::new("madlad-3b", external(url, Some("m"), Some(KEY), transport))
}

#[test]
#[serial]
fn test_api_key_over_plain_http_to_the_internet_is_refused_before_any_request() {
    for url in ["http://api.openai.com/v1", "api.openai.com", "http://203.0.113.9:1234", "http://openrouter.ai/api/v1"] {
        let env = Env::new();
        let tr = keyed(url, FakeTransport::forbid()).err().expect("refused").to_string();
        assert_eq!(tr, KEY_ERROR_TR);
        assert!(!tr.contains(KEY));
        env.english();
        let en = keyed(url, FakeTransport::forbid()).err().expect("refused").to_string();
        assert_eq!(en, KEY_ERROR_EN);
    }
}

#[test]
#[serial]
fn test_api_key_is_sent_over_https_or_to_the_local_network() {
    for url in [
        "https://api.openai.com/v1",
        "http://192.168.1.42:1234",
        "http://10.0.0.5:1234",
        "http://100.100.1.1:1234",
        "http://127.0.0.1:1234",
        "http://localhost:11434",
        "http://[fd00::1]:1234",
        "http://mac-studio.local:1234",
        "http://ollama:11434",
        "mac-studio:1234",
    ] {
        let _env = Env::new();
        let transport = FakeTransport::json(CHAT_REPLY);
        let mut options = external(url, Some("m"), Some(KEY), transport.clone());
        options.resolver = identity_resolver();
        let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
        translator.complete(&messages()).unwrap();
        assert_eq!(transport.requests()[0].header("Authorization"), Some(format!("Bearer {KEY}").as_str()), "{url}");
    }
}

#[test]
#[serial]
fn test_plain_http_to_the_internet_without_a_key_is_unchanged() {
    let _env = Env::new();
    let transport = FakeTransport::json(CHAT_REPLY);
    let mut options = external("http://api.example.com:8080", Some("m"), None, transport.clone());
    options.resolver = identity_resolver();
    let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
    translator.complete(&messages()).unwrap();
    let requests = transport.requests();
    assert_eq!(requests[0].url, "http://api.example.com:8080/v1/chat/completions");
    assert_eq!(requests[0].header("Authorization"), None);
}

#[test]
#[serial]
fn test_the_key_is_checked_against_the_url_after_local_name_resolution() {
    let _env = Env::new();
    let mut options = external("http://mac-studio.local:1234", Some("m"), Some(KEY), FakeTransport::forbid());
    options.resolver = Some(Box::new(|_: &str, _: bool| "http://203.0.113.9:1234/v1/chat/completions".to_string()));
    let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
    assert_eq!(translator.complete(&messages()).unwrap_err().to_string(), KEY_ERROR_TR);
}

#[test]
#[serial]
fn test_a_refused_key_is_not_swallowed_by_model_discovery() {
    let _env = Env::new();
    let mut translator =
        LlamaTranslator::new("madlad-3b", external("http://mac-studio.local:1234", None, Some(KEY), FakeTransport::forbid()))
            .unwrap();
    translator.set_external_chat_url(Some("http://203.0.113.9:1234/v1/chat/completions".to_string()));
    assert!(translator.discover_external_model().unwrap_err().to_string().contains("API anahtarı"));
}

#[test]
#[serial]
fn test_the_key_is_not_sent_to_a_local_llama_server() {
    let _env = Env::new();
    let mut translator = with_completion("gemma-4-e4b", Box::new(|_| Ok("ok".to_string())));
    translator.set_external_api_key(Some(KEY.to_string()));
    let headers = translator.request_headers(Some("http://127.0.0.1:8080/v1/chat/completions")).unwrap();
    assert!(!headers.iter().any(|(k, _)| k == "Authorization"));
}

// ---- the `.local` subnet-scan fallback must never carry the key -----------------------------------------

/// mDNS blocked (`socket.gaierror`); the scan returns `scan` and records the ports it was asked for.
struct FakeNet {
    scan: Option<String>,
    scanned: Mutex<Vec<u16>>,
    forbid_scan: bool,
}

impl LanNet for FakeNet {
    fn getaddrinfo_v4(&self, _host: &str, _port: u16) -> io::Result<Vec<Ipv4Addr>> {
        Err(io::Error::other("mDNS blocked"))
    }

    fn discover_lan_server_ip(&self, port: u16) -> Option<String> {
        assert!(!self.forbid_scan, "a scan would pick any host");
        self.scanned.lock().unwrap().push(port);
        self.scan.clone()
    }
}

fn fake_net(scan: Option<&str>, forbid_scan: bool) -> FakeNet {
    FakeNet { scan: scan.map(str::to_string), scanned: Mutex::new(Vec::new()), forbid_scan }
}

#[test]
fn test_scan_fallback_is_skipped_when_a_key_is_configured() {
    let url = "http://mac-studio.local:1234/v1/chat/completions";
    assert_eq!(resolve_lan_url_with(url, false, &fake_net(None, true)), url);
}

#[test]
fn test_scan_fallback_still_runs_without_a_key() {
    let net = fake_net(Some("192.168.1.77"), false);
    let resolved = resolve_lan_url_with("http://mac-studio.local:1234/v1/chat/completions", true, &net);
    assert_eq!(resolved, "http://192.168.1.77:1234/v1/chat/completions");
    assert_eq!(*net.scanned.lock().unwrap(), vec![1234]);
}

#[test]
#[serial]
fn test_complete_asks_for_a_scan_only_without_a_key() {
    for (key, allow_scan) in [(Some(KEY), false), (None, true)] {
        let _env = Env::new();
        let seen: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let rec = seen.clone();
        let mut options = external("http://mac-studio.local:1234", Some("m"), key, FakeTransport::json(CHAT_REPLY));
        options.resolver = Some(Box::new(move |u: &str, allow: bool| {
            rec.lock().unwrap().push(allow);
            u.to_string()
        }));
        let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
        translator.complete(&messages()).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![allow_scan]);
    }
}

#[test]
#[serial]
fn test_a_failed_mdns_lookup_with_a_key_makes_no_probe_connection() {
    let _env = Env::new();
    let transport = FakeTransport::json(CHAT_REPLY);
    let mut options = external("http://mac-studio.local:1234", Some("m"), Some(KEY), transport.clone());
    options.resolver = Some(Box::new(se_translate::lan::NetResolver(fake_net(None, true))));
    let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
    translator.complete(&messages()).unwrap();
    let urls: Vec<String> = transport.requests().into_iter().map(|r| r.url).collect();
    assert_eq!(urls, vec!["http://mac-studio.local:1234/v1/chat/completions"]); // unchanged address
}

#[test]
fn test_probing_only_opens_connections_and_sends_no_bytes() {
    // The probe seam can only connect (it has no way to send); record what it was asked to probe.
    let probed: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let found = discover_lan_server_ip_with(
        1234,
        &|| Ok("192.168.1.10".to_string()),
        &|ip: Ipv4Addr, port: u16| {
            assert_eq!(port, 1234);
            probed.lock().unwrap().push(ip.to_string());
            ip == Ipv4Addr::new(192, 168, 1, 77)
        },
    );
    assert_eq!(found.as_deref(), Some("192.168.1.77"));
    let probed = probed.into_inner().unwrap();
    assert!(!probed.is_empty() && !probed.contains(&"192.168.1.10".to_string()));
    assert_eq!(probed.len(), 253);
}

#[test]
fn test_scan_picks_the_lowest_open_octet_and_handles_no_route() {
    let found = discover_lan_server_ip_with(80, &|| Ok("10.0.0.5".to_string()), &|ip: Ipv4Addr, _| {
        ip.octets()[3] == 200 || ip.octets()[3] == 9
    });
    assert_eq!(found.as_deref(), Some("10.0.0.9"));
    assert_eq!(discover_lan_server_ip_with(80, &|| Err(io::Error::other("no route")), &|_, _| true), None);
}

// ---- the key must not follow a redirect to another host ---------------------------------------------------

fn redirect(handler_url: &str, target: &str, code: u16) -> Result<HttpRequest, HttpFailure> {
    let request =
        HttpRequest::new("GET", handler_url, vec![("Authorization".to_string(), format!("Bearer {KEY}"))], None);
    redirect_request(&request, code, "Found", target)
}

#[test]
fn test_redirect_to_another_host_drops_the_key() {
    for code in [301, 302, 303] {
        for target in [
            "http://203.0.113.9/v1/models",
            "https://evil.example.com/v1/models",
            "http://192.168.1.99:1234/v1/models",
            "http://localhost:1234/v1/models",
        ] {
            let redirected = redirect("http://192.168.1.42:1234/v1/models", target, code).unwrap();
            assert_eq!(redirected.url, target);
            assert_eq!(redirected.header("Authorization"), None);
        }
    }
}

#[test]
fn test_redirect_to_another_host_keeps_other_headers() {
    let request = HttpRequest::new(
        "GET",
        "https://api.example.com/v1/models",
        vec![
            ("Authorization".to_string(), format!("Bearer {KEY}")),
            ("Accept".to_string(), "application/json".to_string()),
        ],
        None,
    );
    let redirected = redirect_request(&request, 302, "Found", "https://other.example.org/models").unwrap();
    assert_eq!(redirected.header("Authorization"), None);
    assert_eq!(redirected.header("Accept"), Some("application/json"));
}

#[test]
fn test_redirect_within_the_same_host_keeps_the_key() {
    for (source, target) in [
        ("http://192.168.1.42:1234/v1/models", "http://192.168.1.42:1234/v1/models/"),
        ("http://192.168.1.42:1234/v1/models", "http://192.168.1.42:8080/v1/models"),
        ("https://api.example.com/v1/models", "https://API.example.com/v2/models"),
        ("http://192.168.1.42/v1/models", "https://192.168.1.42/v1/models"),
    ] {
        let redirected = redirect(source, target, 302).unwrap();
        assert_eq!(redirected.header("Authorization"), Some(format!("Bearer {KEY}").as_str()), "{target}");
    }
}

#[test]
fn test_redirect_downgrading_to_plain_http_on_the_internet_drops_the_key() {
    let redirected = redirect("https://api.example.com/v1/models", "http://api.example.com/v1/models", 302).unwrap();
    assert_eq!(redirected.header("Authorization"), None);
}

#[test]
fn test_redirect_rules_for_methods_and_bodies() {
    let post = HttpRequest::new(
        "POST",
        "http://192.168.1.42/v1/chat/completions",
        vec![("Content-Type".into(), "application/json".into()), ("Accept".into(), "application/json".into())],
        Some(b"{}".to_vec()),
    );
    let next = redirect_request(&post, 303, "See Other", "http://192.168.1.42/x").unwrap();
    assert_eq!(next.method, "GET");
    assert_eq!(next.body, None);
    assert_eq!(next.header("Content-Type"), None);
    assert_eq!(next.header("Accept"), Some("application/json"));
    let err = redirect_request(&post, 307, "Temporary Redirect", "http://192.168.1.42/x").unwrap_err();
    assert_eq!(err.to_string(), "HTTP Error 307: Temporary Redirect");
}

fn real_discovery(target_host: &str) -> (String, Arc<Mutex<Vec<(&'static str, Option<String>)>>>) {
    let seen: Arc<Mutex<Vec<(&'static str, Option<String>)>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = seen.clone();
    let target_port = serve(move |r| {
        rec.lock().unwrap().push(("target", r.header("Authorization")));
        Reply::json(r#"{"data": [{"id": "remote-model"}]}"#)
    });
    let rec = seen.clone();
    let location = format!("http://{target_host}:{target_port}/v1/models");
    let origin_port = serve(move |r| {
        rec.lock().unwrap().push(("origin", r.header("Authorization")));
        Reply::redirect(302, &location)
    });
    let options = LlmOptions {
        external_url: Some(format!("http://127.0.0.1:{origin_port}")),
        external_api_key: Some(KEY.to_string()),
        ..LlmOptions::default()
    };
    let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
    let model = translator.discover_external_model().unwrap();
    (model, seen)
}

#[test]
#[serial]
fn test_real_redirect_to_another_host_name_does_not_forward_the_key() {
    let _env = Env::new();
    let (model, seen) = real_discovery("localhost"); // same machine, other name
    assert_eq!(model, "remote-model"); // the redirect was followed
    assert_eq!(
        *seen.lock().unwrap(),
        vec![("origin", Some(format!("Bearer {KEY}"))), ("target", None)]
    );
}

#[test]
#[serial]
fn test_real_redirect_within_the_same_host_keeps_the_key() {
    let _env = Env::new();
    let (model, seen) = real_discovery("127.0.0.1");
    assert_eq!(model, "remote-model");
    assert_eq!(seen.lock().unwrap()[1], ("target", Some(format!("Bearer {KEY}"))));
}

#[test]
#[serial]
fn test_real_post_redirect_to_another_host_name_does_not_forward_the_key() {
    let _env = Env::new();
    let seen: Arc<Mutex<Vec<(String, String, Option<String>)>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = seen.clone();
    let target_port = serve(move |r| {
        rec.lock().unwrap().push(("target".into(), r.method.clone(), r.header("Authorization")));
        Reply::json(CHAT_REPLY)
    });
    let rec = seen.clone();
    let origin_port = serve(move |r| {
        rec.lock().unwrap().push(("origin".into(), r.method.clone(), r.header("Authorization")));
        Reply::redirect(303, &format!("http://localhost:{target_port}/v1/chat/completions"))
    });
    let options = LlmOptions {
        external_url: Some(format!("http://127.0.0.1:{origin_port}")),
        external_model: Some("m".to_string()),
        external_api_key: Some(KEY.to_string()),
        ..LlmOptions::default()
    };
    let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
    assert_eq!(translator.complete(&messages()).unwrap(), "[1] Merhaba!");
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("origin".to_string(), "POST".to_string(), Some(format!("Bearer {KEY}"))),
            ("target".to_string(), "GET".to_string(), None),
        ]
    );
}

// ---- strict offline mode blocks servers outside the local network --------------------------------------

const CLOUD_URLS: &[(&str, &str)] = &[
    ("https://api.openai.com/v1", "api.openai.com"),
    ("https://opencode.ai/zen/go/v1", "opencode.ai"),
    ("https://openrouter.ai/api/v1", "openrouter.ai"),
    ("https://generativelanguage.googleapis.com/v1beta/openai", "generativelanguage.googleapis.com"),
    ("http://203.0.113.9:1234", "203.0.113.9"),
    ("api.deepseek.com", "api.deepseek.com"),
];
const LAN_URLS: &[&str] = &[
    "192.168.1.42:1234",
    "http://10.0.0.5:1234",
    "http://172.20.1.1",
    "http://100.101.102.103:8080",
    "http://169.254.1.2:1234",
    "http://[fd00::1]:1234",
    "http://[fe80::1]:1234",
    "http://127.0.0.1:1234",
    "http://localhost:11434",
    "http://mac-studio.local:1234",
    "http://ollama:11434",
];

#[test]
#[serial]
fn test_offline_mode_blocks_an_internet_server_before_any_request() {
    for (url, host) in CLOUD_URLS {
        let env = Env::new();
        env.offline();
        let tr = LlamaTranslator::new("madlad-3b", external(url, Some("m"), None, FakeTransport::forbid()))
            .err()
            .expect("blocked")
            .to_string();
        assert_eq!(
            tr,
            format!(
                "Çevrimdışı mod açık: çeviri internetteki bir sağlayıcıya ({host}) gönderilemez. \
                 Çevrimdışı modu kapatın ya da yerel bir model/sunucu seçin."
            )
        );
        env.english();
        let en = LlamaTranslator::new("madlad-3b", external(url, Some("m"), None, FakeTransport::forbid()))
            .err()
            .expect("blocked")
            .to_string();
        assert_eq!(
            en,
            format!(
                "Offline mode is on: translation can't be sent to an internet provider ({host}). \
                 Turn offline mode off or choose a local model/server."
            )
        );
    }
}

#[test]
#[serial]
fn test_offline_mode_allows_servers_on_the_local_network() {
    for url in LAN_URLS {
        let env = Env::new();
        env.offline();
        let transport = FakeTransport::json(CHAT_REPLY);
        let mut options = external(url, Some("m"), None, transport.clone());
        options.resolver = identity_resolver();
        let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
        assert_eq!(translator.complete(&messages()).unwrap(), "[1] Merhaba!");
        assert_eq!(transport.requests().len(), 1, "{url}");
    }
}

#[test]
#[serial]
fn test_offline_mode_allows_a_local_name_however_it_resolves() {
    let env = Env::new();
    env.offline();
    let transport = FakeTransport::json(CHAT_REPLY);
    let mut options = external("http://mac-studio.local:1234", Some("m"), None, transport.clone());
    options.resolver = Some(Box::new(se_translate::lan::NetResolver(fake_net(Some("192.168.1.77"), false))));
    let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
    translator.complete(&messages()).unwrap();
    assert_eq!(transport.requests()[0].url, "http://192.168.1.77:1234/v1/chat/completions");
}

#[test]
#[serial]
fn test_online_mode_allows_a_cloud_server() {
    for (url, _host) in CLOUD_URLS {
        let _env = Env::new();
        let transport = FakeTransport::json(CHAT_REPLY);
        let mut options = external(url, Some("m"), None, transport.clone());
        options.resolver = identity_resolver();
        let mut translator = LlamaTranslator::new("madlad-3b", options).unwrap();
        assert_eq!(translator.complete(&messages()).unwrap(), "[1] Merhaba!");
        assert_eq!(transport.requests().len(), 1);
    }
}

#[test]
#[serial]
fn test_offline_flags_that_are_off_do_not_block() {
    for value in ["", "0", "false", "no", "off"] {
        let env = Env::new();
        env.set(se_core::offline::OFFLINE_ENV_VAR, value);
        check_external_llm("https://api.openai.com/v1", None).unwrap();
    }
}

#[test]
#[serial]
fn test_offline_rule_is_checked_before_the_key_rule() {
    let env = Env::new();
    env.offline();
    assert!(check_external_llm("http://api.openai.com/v1", Some(KEY)).unwrap_err().to_string().contains("Çevrimdışı mod açık"));
}

#[test]
#[serial]
fn test_check_external_llm_returns_the_chat_url() {
    let _env = Env::new();
    assert_eq!(check_external_llm("192.168.1.42:1234", None).unwrap(), "http://192.168.1.42:1234/v1/chat/completions");
    assert!(check_external_llm("https://openrouter.ai/api/v1", Some(KEY)).unwrap().ends_with("/api/v1/chat/completions"));
}

#[test]
#[serial]
fn test_a_hostless_address_still_gets_the_address_error() {
    let _env = Env::new();
    assert!(check_external_llm("http://", Some(KEY)).unwrap_err().to_string().contains("Harici LLM sunucusu adresi boş olamaz."));
}
