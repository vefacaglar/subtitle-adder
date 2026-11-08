//! External LLM server address rules and the connection test (mac-enginekit §7, win-providers §5–6,
//! UNIFIED.md §3.7 key-safety FIXes). Mirrors the engine's rules so the app never sends a key the engine would
//! refuse to send.
//!
//! Addresses are handled with a small RFC 3986 authority extractor ([`parse_url`]), never a WHATWG parser: hosts are
//! not reinterpreted (`1.2.3` stays `1.2.3`, `999.1.1.1` is not rejected as a URL but is not a local address) and
//! every function returns the user's own string with only the documented edits (case kept).

use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::ops::Range;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;

use sa_core::cancel::CancellationToken;

use crate::presets::TranslationProviderPreset;

/// Per-request timeout.
pub const TIMEOUT: Duration = Duration::from_secs(8);
pub const USER_AGENT: &str = "SubtitleAdder/1.0";
pub const OPENCODE_GO_MODELS_URL: &str = "https://opencode.ai/inference/go/v1/models";
pub const OPENCODE_GO_CHAT_URL: &str = "https://opencode.ai/inference/go/openai/v1/chat/completions";
pub const FALLBACK_PROBE_MODEL: &str = "gpt-4o-mini";
/// OpenCode Go paths (trailing `/` trimmed, compared case-sensitively).
pub const OPENCODE_GO_PATHS: [&str; 5] = [
    "/zen/go/v1",
    "/zen/go/v1/chat/completions",
    "/inference/go/v1",
    "/inference/go/openai/v1",
    "/inference/go/openai/v1/chat/completions",
];
/// Suffixes `normalize_base_url` strips (first match only), in this order.
pub const STRIPPED_SUFFIXES: [&str; 5] = ["/v1/chat/completions", "/chat/completions", "/api/chat", "/api/tags", "/v1/models"];
/// Default port probed for `.local` LAN servers (Ollama).
pub const DEFAULT_LAN_PORT: u16 = 11434;
/// TCP probe of a cached LAN address.
pub const CACHED_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
/// Overall deadline of the /24 scan.
pub const SUBNET_SCAN_DEADLINE: Duration = Duration::from_millis(900);
/// Largest response body read by the default fetcher.
pub const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// New transport-error texts (UNIFIED.md §8: never raw English in a Turkish UI). Keys live in the DesktopWindows and
/// DesktopMac overlay catalogs.
pub mod strings {
    pub const HOST_NOT_FOUND: &str = "Sunucu bulunamadı.";
    pub const CONNECTION_REFUSED: &str = "Bağlantı reddedildi.";
    pub const TLS_FAILED: &str = "Güvenli bağlantı kurulamadı.";
    pub const CONNECTION_FAILED: &str = "Bağlantı kurulamadı.";
    /// The engine's `external_llm_key_insecure` text (engine `i18n`), for [`super::ConnectionResult::InsecureKeyTransport`].
    pub const INSECURE_KEY: &str = "API anahtarı şifrelenmemiş bağlantıyla (http) gönderilmez; adresi https:// ile girin.";
}

/// Result of an explicit "Test" click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionResult {
    InvalidAddress,
    Unauthorized { status: u16 },
    Unreachable { reason: String },
    Reachable { host: String, latency_ms: u64, models: Vec<String> },
    /// A key would go over plain http to a host outside the local network (engine rule; localized `https://` error).
    InsecureKeyTransport,
    Cancelled,
}

impl ConnectionResult {
    pub fn models(&self) -> &[String] {
        match self {
            ConnectionResult::Reachable { models, .. } => models,
            _ => &[],
        }
    }
}

/// One HTTP exchange (injectable for tests; the default uses `ureq`).
pub trait HttpFetch: Send + Sync {
    /// Returns `(status, body)` or a transport error description; `Err(None)` = timeout. HTTP error statuses are
    /// **not** errors. Implementations return promptly (any `Err`) once `cancel` fires.
    fn fetch(&self, request: HttpRequest, cancel: &CancellationToken) -> Result<(u16, String), Option<String>>;
}

/// A request the connection test makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

impl HttpRequest {
    /// The first header value with that name (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// The parts of an absolute URL, as slices of the original string (RFC 3986, no reinterpretation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlParts<'a> {
    pub scheme: &'a str,
    pub userinfo: Option<&'a str>,
    /// Without brackets for IPv6 literals; may be empty.
    pub host: &'a str,
    /// Byte range of `host` in the original string.
    pub host_range: Range<usize>,
    pub port: Option<&'a str>,
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub fragment: Option<&'a str>,
}

fn is_url_char(c: char) -> bool {
    c.is_ascii_graphic() && !matches!(c, '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '|' | '}')
}

/// Splits `scheme://authority/path?query#fragment`. `None` when there is no valid scheme, the string contains a
/// character a URL forbids (whitespace, control or non-ASCII characters, `"<>\^`{|}`), a bracketed host is not
/// closed or the port is not numeric.
pub fn parse_url(url: &str) -> Option<UrlParts<'_>> {
    if url.is_empty() || !url.chars().all(is_url_char) {
        return None;
    }
    let sep = url.find("://")?;
    let scheme = &url[..sep];
    let mut chars = scheme.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    {
        return None;
    }
    let auth_start = sep + 3;
    let auth_end = url[auth_start..].find(['/', '?', '#']).map_or(url.len(), |i| auth_start + i);
    let authority = &url[auth_start..auth_end];
    let (userinfo, hostport, hostport_start) = match authority.rfind('@') {
        Some(at) => (Some(&authority[..at]), &authority[at + 1..], auth_start + at + 1),
        None => (None, authority, auth_start),
    };
    let (host_range, port) = if let Some(inner) = hostport.strip_prefix('[') {
        let close = inner.find(']')?;
        let after = &inner[close + 1..];
        let port = if after.is_empty() {
            None
        } else {
            Some(after.strip_prefix(':')?)
        };
        (hostport_start + 1..hostport_start + 1 + close, port)
    } else {
        match hostport.rfind(':') {
            Some(colon) => (hostport_start..hostport_start + colon, Some(&hostport[colon + 1..])),
            None => (hostport_start..hostport_start + hostport.len(), None),
        }
    };
    if let Some(p) = port
        && !p.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let rest = &url[auth_end..];
    let (before_fragment, fragment) = match rest.find('#') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let (path, query) = match before_fragment.find('?') {
        Some(i) => (&before_fragment[..i], Some(&before_fragment[i + 1..])),
        None => (before_fragment, None),
    };
    Some(UrlParts { scheme, userinfo, host: &url[host_range.clone()], host_range, port, path, query, fragment })
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn ends_with_ci(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len() && s.as_bytes()[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

fn has_scheme(url: &str) -> bool {
    starts_with_ci(url, "http://") || starts_with_ci(url, "https://")
}

/// The process-wide OpenCode session id (lower-case UUID v4, one per app process; `x-opencode-session`).
pub fn opencode_session_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().hyphenated().to_string())
}

/// Address rules.
pub struct ExternalLlmServer;

impl ExternalLlmServer {
    /// Normalizes a typed address to a base URL (bare `host:port` → `http://` for local hosts, `https://` otherwise;
    /// the first known endpoint suffix stripped; a typed scheme is never changed). `None` = no usable host.
    pub fn normalize_base_url(raw: &str) -> Option<String> {
        let mut url = raw.trim().trim_end_matches('/').to_owned();
        if url.is_empty() || url.eq_ignore_ascii_case("http:") || url.eq_ignore_ascii_case("https:") {
            return None;
        }
        if !has_scheme(&url) {
            let probe = format!("http://{url}");
            let local = match Self::host(&probe) {
                Some(h) => Self::is_local_network_host(&h),
                None => true,
            };
            url = format!("{}{url}", if local { "http://" } else { "https://" });
        }
        for suffix in STRIPPED_SUFFIXES {
            if ends_with_ci(&url, suffix) {
                url.truncate(url.len() - suffix.len());
                let trimmed = url.trim_end_matches('/').len();
                url.truncate(trimmed);
                if suffix.starts_with("/v1/") && parse_url(&url).is_some_and(|p| !p.path.is_empty()) {
                    url.push_str("/v1");
                }
                break;
            }
        }
        match parse_url(&url) {
            Some(p) if !p.host.is_empty() => Some(url),
            _ => None,
        }
    }

    /// RFC 3986 host extraction (no WHATWG reinterpretation; mac-enginekit §7.2). Brackets of IPv6 literals are
    /// removed; an empty host is `None`.
    pub fn host(url: &str) -> Option<String> {
        parse_url(url).map(|p| p.host).filter(|h| !h.is_empty()).map(str::to_owned)
    }

    /// Loopback, RFC 1918, 100.64/10, link-local, IPv6 ULA/link-local, `.local`/`.lan`/`.localhost`/`.home.arpa`,
    /// bare single-label names.
    pub fn is_local_network_host(host: &str) -> bool {
        let h = host.to_lowercase();
        let h = h.trim_matches(|c| c == '[' || c == ']');
        if h.is_empty() {
            return false;
        }
        if h.contains(':') {
            return h == "::1" || h.starts_with("fe80:") || h.starts_with("fc") || h.starts_with("fd");
        }
        let parts: Vec<&str> = h.split('.').collect();
        if parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
            let mut octets = [0u32; 4];
            for (o, p) in octets.iter_mut().zip(&parts) {
                match p.parse::<u32>() {
                    Ok(v) if v <= 255 => *o = v,
                    _ => return false,
                }
            }
            return match octets {
                [10, ..] | [127, ..] => true,
                [192, 168, ..] | [169, 254, ..] => true,
                [172, b, ..] => (16..=31).contains(&b),
                [100, b, ..] => (64..=127).contains(&b),
                _ => false,
            };
        }
        if !h.contains('.') {
            return true;
        }
        h.ends_with(".localhost") || h.ends_with(".local") || h.ends_with(".lan") || h.ends_with(".home.arpa")
    }

    /// True when a key must not be sent to `url`: plain `http://` to a host outside the local network
    /// (the engine's rule).
    pub fn is_insecure_key_destination(url: &str) -> bool {
        match parse_url(url) {
            Some(p) => p.scheme.eq_ignore_ascii_case("http") && !Self::is_local_network_host(p.host),
            None => false,
        }
    }

    /// Model ids from a `/models` or `/api/tags` body, embedding models filtered out, de-duplicated
    /// case-insensitively (first spelling kept).
    pub fn parse_suitable_models(body: &str) -> Vec<String> {
        let Ok(Value::Object(root)) = serde_json::from_str::<Value>(body) else { return Vec::new() };
        let mut out: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for key in ["data", "models"] {
            let Some(Value::Array(items)) = root.get(key) else { continue };
            for item in items {
                let Value::Object(o) = item else { continue };
                let name = match (o.get("id"), o.get("name")) {
                    (Some(Value::String(id)), _) => id,
                    (_, Some(Value::String(name))) => name,
                    _ => continue,
                };
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                let lower = name.to_lowercase();
                if lower.contains("embed") || lower.contains("minilm") || lower.contains("bge-") {
                    continue;
                }
                if seen.contains(&lower) {
                    continue;
                }
                seen.push(lower);
                out.push(name.to_owned());
            }
        }
        out
    }

    /// True for the OpenCode Go addresses (https, host `opencode.ai`, no user and no explicit port — not even `:443`,
    /// as Foundation's `URLComponents.port == nil` — no query/fragment, a known path). The Windows provider test keeps
    /// its own C# rule (`sa_providers::win::connection::is_opencode_go_url`).
    pub fn is_opencode_go(url: &str) -> bool {
        let Some(p) = parse_url(url) else { return false };
        p.scheme.eq_ignore_ascii_case("https")
            && p.host.eq_ignore_ascii_case("opencode.ai")
            && p.userinfo.is_none()
            && p.port.is_none_or(str::is_empty)
            && p.query.is_none_or(str::is_empty)
            && p.fragment.is_none_or(str::is_empty)
            && OPENCODE_GO_PATHS.contains(&p.path.trim_end_matches('/'))
    }

    /// GET endpoints tried in order.
    pub fn model_endpoints(base: &str) -> Vec<String> {
        let mut base = base.trim_end_matches('/');
        if Self::is_opencode_go(base) {
            return vec![OPENCODE_GO_MODELS_URL.to_owned()];
        }
        if ends_with_ci(base, "/chat/completions") {
            base = base[..base.len() - "/chat/completions".len()].trim_end_matches('/');
        } else if ends_with_ci(base, "/models") {
            return vec![base.to_owned()];
        }
        if ends_with_ci(base, "/v1") || ends_with_ci(base, "/openai") {
            return vec![format!("{base}/models")];
        }
        vec![format!("{base}/v1/models"), format!("{base}/models"), format!("{base}/api/tags")]
    }

    /// POST endpoint of the chat probe (the engine's rule).
    pub fn chat_endpoint(base: &str) -> String {
        let base = base.trim_end_matches('/');
        if Self::is_opencode_go(base) {
            return OPENCODE_GO_CHAT_URL.to_owned();
        }
        if ends_with_ci(base, "/chat/completions") {
            return base.to_owned();
        }
        if ends_with_ci(base, "/v1") || ends_with_ci(base, "/openai") {
            return format!("{base}/chat/completions");
        }
        format!("{base}/v1/chat/completions")
    }

    /// The headers every request carries: `User-Agent`, `Accept`, `x-opencode-session` (OpenCode Go only),
    /// `Authorization: Bearer` (only with a non-blank key), `Content-Type` (with a body).
    pub fn request(url: &str, method: &'static str, api_key: &str, body: Option<String>) -> HttpRequest {
        let mut headers = vec![("User-Agent".to_owned(), USER_AGENT.to_owned()), ("Accept".to_owned(), "application/json".to_owned())];
        if Self::is_opencode_go(url) || url == OPENCODE_GO_MODELS_URL {
            headers.push(("x-opencode-session".to_owned(), opencode_session_id().to_owned()));
        }
        let key = api_key.trim();
        if !key.is_empty() {
            headers.push(("Authorization".to_owned(), format!("Bearer {key}")));
        }
        if body.is_some() {
            headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
        }
        HttpRequest { method, url: url.to_owned(), headers, body }
    }

    /// The probe body with sorted keys: `{"max_tokens":1,"messages":[{"content":"hi","role":"user"}],"model":M,"stream":false}`.
    pub fn probe_body(model: &str) -> String {
        format!(
            r#"{{"max_tokens":1,"messages":[{{"content":"hi","role":"user"}}],"model":{},"stream":false}}"#,
            Value::String(model.to_owned())
        )
    }

    /// The probe model: the chosen one, else the preset's first suggestion, else `gpt-4o-mini`.
    pub fn probe_model(model: &str, preset_id: Option<&str>) -> String {
        let chosen = model.trim();
        if !chosen.is_empty() {
            return chosen.to_owned();
        }
        let preset = TranslationProviderPreset::preset(preset_id);
        let first = preset.default_model();
        if first.is_empty() { FALLBACK_PROBE_MODEL.to_owned() } else { first.to_owned() }
    }

    /// `HTTP <code>: <gateway message>` (whitespace collapsed, key redacted, ≤ 1000 chars, no HTML).
    pub fn http_error_detail(status: u16, body: &str, api_key: &str) -> String {
        let mut detail = format!("HTTP {status}");
        let bytes = body.as_bytes();
        let prefix = &bytes[..bytes.len().min(8192)];
        if let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(prefix) {
            let root = Value::Object(obj);
            let err = root.get("error").unwrap_or(&root);
            let msg = match err {
                Value::Object(o) if o.contains_key("message") => &o["message"],
                other => other,
            };
            if let Value::String(m) = msg
                && !m.trim().is_empty()
            {
                detail.push_str(": ");
                detail.push_str(m);
            }
        }
        let key = api_key.trim();
        if !key.is_empty() {
            detail = detail.replace(key, "[redacted]");
        }
        let collapsed = detail.split_whitespace().collect::<Vec<_>>().join(" ");
        // Swift `String.prefix(1000)` counts characters (grapheme clusters).
        unicode_segmentation::UnicodeSegmentation::graphemes(collapsed.as_str(), true).take(1000).collect()
    }

    /// `L("API anahtarı geçersiz veya yetkisiz (HTTP %lld).", status)` (EngineKit catalog).
    pub fn unauthorized_message(status: u16) -> String {
        sa_core::engine_string!("API anahtarı geçersiz veya yetkisiz (HTTP %lld).", None, i64::from(status))
    }

    /// [`unauthorized_message`](Self::unauthorized_message) in an explicit language.
    pub fn unauthorized_message_in(status: u16, language: sa_core::l10n::UiLanguage) -> String {
        sa_core::engine_string!("API anahtarı geçersiz veya yetkisiz (HTTP %lld).", Some(language), i64::from(status))
    }

    /// The localized `https://` refusal (the engine's own text) for [`ConnectionResult::InsecureKeyTransport`].
    pub fn insecure_key_message() -> String {
        sa_core::l("API anahtarı şifrelenmemiş bağlantıyla (http) gönderilmez; adresi https:// ile girin.")
    }

    /// `Zaman aşımı` in the app language for a timeout, else the transport description.
    pub fn failure_reason(error: Option<String>) -> String {
        error.unwrap_or_else(|| sa_core::engine_string!("Zaman aşımı", None))
    }

    /// The explicit connection test (mac-enginekit §7.9). With a key: refuses plain http to a public host before any
    /// request (engine rule, UNIFIED.md §3.7) and never runs the `.local` subnet scan.
    pub fn test_connection(
        raw_url: &str,
        api_key: &str,
        model: &str,
        preset_id: Option<&str>,
        fetch: &dyn HttpFetch,
        cancel: &CancellationToken,
    ) -> ConnectionResult {
        let Some(base) = Self::normalize_base_url(raw_url) else { return ConnectionResult::InvalidAddress };
        let Some(host) = Self::host(&base) else { return ConnectionResult::InvalidAddress };
        if !api_key.trim().is_empty() && Self::is_insecure_key_destination(&base) {
            return ConnectionResult::InsecureKeyTransport;
        }
        let started = Instant::now();
        let mut models: Vec<String> = Vec::new();
        let mut reachable = false;
        let mut unauthorized: Option<u16> = None;
        let mut last_error: Option<String> = None;

        for endpoint in Self::model_endpoints(&base) {
            if cancel.is_cancelled() {
                break;
            }
            if parse_url(&endpoint).is_none() {
                continue;
            }
            match fetch.fetch(Self::request(&endpoint, "GET", api_key, None), cancel) {
                Err(e) => {
                    if cancel.is_cancelled() {
                        break;
                    }
                    last_error = Some(Self::failure_reason(e));
                }
                Ok((status @ (401 | 403), _)) => {
                    unauthorized = Some(status);
                    break;
                }
                Ok((status, body)) if (200..300).contains(&status) => {
                    reachable = true;
                    models = Self::parse_suitable_models(&body);
                    if !models.is_empty() {
                        break;
                    }
                }
                Ok((status, body)) => last_error = Some(Self::http_error_detail(status, &body, api_key)),
            }
        }

        if !reachable && unauthorized.is_none() && !cancel.is_cancelled() {
            let chat = Self::chat_endpoint(&base);
            let body = Self::probe_body(&Self::probe_model(model, preset_id));
            match fetch.fetch(Self::request(&chat, "POST", api_key, Some(body)), cancel) {
                Ok((status @ (401 | 403), _)) => unauthorized = Some(status),
                Ok((status, _)) if (200..300).contains(&status) => reachable = true,
                Ok((status, body)) => last_error = Some(Self::http_error_detail(status, &body, api_key)),
                Err(e) => {
                    if !cancel.is_cancelled() {
                        last_error = Some(Self::failure_reason(e));
                    }
                }
            }
        }

        if cancel.is_cancelled() {
            return ConnectionResult::Cancelled;
        }
        if let Some(status) = unauthorized {
            return ConnectionResult::Unauthorized { status };
        }
        if !reachable {
            return ConnectionResult::Unreachable { reason: Self::failure_reason(last_error) };
        }
        let latency_ms = (started.elapsed().as_millis() as u64).max(1);
        ConnectionResult::Reachable { host, latency_ms, models }
    }

    /// Windows LAN discovery (win-enginekit §9.2): the local IPv4 of a UDP socket "connected" to `8.8.8.8:80` (no
    /// packet sent), then the cached address (250 ms TCP probe), then TCP connects to every other `x.y.z.1–254` in
    /// parallel with a 900 ms deadline; the first that connects wins and is cached. Callers skip this when an API key
    /// is configured (UNIFIED.md §3.7).
    pub fn discover_lan_server_ip(port: u16, cancel: &CancellationToken) -> Option<String> {
        let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("8.8.8.8:80").ok()?;
        let local = match socket.local_addr().ok()?.ip() {
            std::net::IpAddr::V4(v4) if !v4.is_unspecified() => v4,
            _ => return None,
        };
        if let Some(cached) = cached_lan_ip()
            && tcp_reachable(&cached, port, CACHED_PROBE_TIMEOUT)
        {
            return Some(cached);
        }
        let [a, b, c, own] = local.octets();
        let deadline = Instant::now() + SUBNET_SCAN_DEADLINE;
        let (tx, rx) = crossbeam_channel::unbounded::<Ipv4Addr>();
        for d in 1u8..=254 {
            if d == own {
                continue;
            }
            let tx = tx.clone();
            let addr = Ipv4Addr::new(a, b, c, d);
            let spawned = std::thread::Builder::new().name("sa-lan-probe".into()).stack_size(64 * 1024).spawn(move || {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return;
                }
                if TcpStream::connect_timeout(&SocketAddr::from((addr, port)), remaining).is_ok() {
                    let _ = tx.send(addr);
                }
            });
            if spawned.is_err() {
                break;
            }
        }
        drop(tx);
        loop {
            if cancel.is_cancelled() {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            match rx.recv_timeout((deadline - now).min(Duration::from_millis(25))) {
                Ok(ip) => {
                    let ip = ip.to_string();
                    set_cached_lan_ip(&ip);
                    return Some(ip);
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Windows engine launch rule (win-enginekit §9.2 `resolve_active_url`, applied to the normalized URL): a
    /// `.local` host is replaced by its first IPv4 from the OS resolver; failing that, and only without an API key,
    /// by the cached LAN address (250 ms probe) or the /24 scan. Any other URL is returned unchanged.
    pub fn resolve_local_host(url: &str, has_api_key: bool, cancel: &CancellationToken) -> String {
        let Some(parts) = parse_url(url) else { return url.to_owned() };
        if !ends_with_ci(parts.host, ".local") {
            return url.to_owned();
        }
        let port = match parts.port.and_then(|p| p.parse::<u16>().ok()) {
            Some(p) if !(p == 80 && parts.scheme.eq_ignore_ascii_case("http")) && !(p == 443 && parts.scheme.eq_ignore_ascii_case("https")) => p,
            _ => DEFAULT_LAN_PORT,
        };
        let replace = |ip: &str| {
            let mut out = String::with_capacity(url.len());
            out.push_str(&url[..parts.host_range.start]);
            out.push_str(ip);
            out.push_str(&url[parts.host_range.end..]);
            out.trim_end_matches('/').to_owned()
        };
        let resolved = (parts.host, port).to_socket_addrs().ok().and_then(|mut addrs| {
            addrs.find_map(|a| match a {
                SocketAddr::V4(v4) => Some(v4.ip().to_string()),
                SocketAddr::V6(_) => None,
            })
        });
        if let Some(ip) = resolved {
            set_cached_lan_ip(&ip);
            return replace(&ip);
        }
        if has_api_key {
            return url.to_owned();
        }
        if let Some(cached) = cached_lan_ip()
            && tcp_reachable(&cached, port, CACHED_PROBE_TIMEOUT)
        {
            return replace(&cached);
        }
        match Self::discover_lan_server_ip(port, cancel) {
            Some(ip) => replace(&ip),
            None => url.to_owned(),
        }
    }
}

static CACHED_LAN_IP: Mutex<Option<String>> = Mutex::new(None);

/// The last LAN server address found by resolution or the scan (process-wide, like the C# `cached_resolved_host_ip`).
pub fn cached_lan_ip() -> Option<String> {
    CACHED_LAN_IP.lock().clone()
}

/// Remembers a LAN server address (resolution and the scan do this; tests seed it).
#[doc(hidden)]
pub fn set_cached_lan_ip(ip: &str) {
    *CACHED_LAN_IP.lock() = Some(ip.to_owned());
}

/// Forgets the cached LAN address (the configured URL changed).
pub fn clear_cached_lan_ip() {
    *CACHED_LAN_IP.lock() = None;
}

fn tcp_reachable(ip: &str, port: u16, timeout: Duration) -> bool {
    match ip.parse::<std::net::IpAddr>() {
        Ok(addr) => TcpStream::connect_timeout(&SocketAddr::new(addr, port), timeout).is_ok(),
        Err(_) => false,
    }
}

#[cfg(feature = "ureq-fetch")]
pub use ureq_fetch::UreqFetch;

#[cfg(feature = "ureq-fetch")]
mod ureq_fetch {
    use super::*;

    /// Default `ureq` fetcher: one shared agent per scheme family (no cookies, no cache), 8 s timeout, status codes
    /// returned rather than raised, `Authorization` kept only on a redirect to the same host (never onto plain http),
    /// https requests never downgraded to http on a redirect, bodies capped at 8 MiB.
    pub struct UreqFetch;

    impl UreqFetch {
        fn agent(https: bool) -> &'static ureq::Agent {
            static PLAIN: OnceLock<ureq::Agent> = OnceLock::new();
            static HTTPS: OnceLock<ureq::Agent> = OnceLock::new();
            let build = |https_only: bool| -> ureq::Agent {
                ureq::Agent::config_builder()
                    .timeout_global(Some(TIMEOUT))
                    .http_status_as_error(false)
                    .https_only(https_only)
                    // Like URLSession / reqwest (mac-enginekit §7.7): the key follows a redirect on the same host
                    // (never onto plain http) and is dropped for any other host.
                    .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
                    .build()
                    .into()
            };
            if https { HTTPS.get_or_init(|| build(true)) } else { PLAIN.get_or_init(|| build(false)) }
        }

        /// The shared agent used for `url` (exposed so tests can check it is reused).
        pub fn shared_agent(url: &str) -> &'static ureq::Agent {
            Self::agent(starts_with_ci(url, "https://"))
        }

        fn run(request: &HttpRequest) -> Result<(u16, String), Option<String>> {
            let agent = Self::shared_agent(&request.url);
            let result = if request.method == "POST" {
                let mut b = agent.post(&request.url);
                for (k, v) in &request.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                b.send(request.body.as_deref().unwrap_or(""))
            } else {
                let mut b = agent.get(&request.url);
                for (k, v) in &request.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                b.call()
            };
            let mut response = result.map_err(transport_error)?;
            let status = response.status().as_u16();
            let body = response.body_mut().with_config().limit(MAX_BODY_BYTES).lossy_utf8(true).read_to_string().unwrap_or_default();
            Ok((status, body))
        }
    }

    fn transport_error(e: ureq::Error) -> Option<String> {
        use std::io::ErrorKind;
        let key = match &e {
            ureq::Error::Timeout(_) => return None,
            ureq::Error::Io(io) if matches!(io.kind(), ErrorKind::TimedOut) => return None,
            ureq::Error::Io(io) if matches!(io.kind(), ErrorKind::ConnectionRefused) => strings::CONNECTION_REFUSED,
            ureq::Error::HostNotFound => strings::HOST_NOT_FOUND,
            ureq::Error::Tls(_) | ureq::Error::Rustls(_) | ureq::Error::Pem(_) => strings::TLS_FAILED,
            _ => strings::CONNECTION_FAILED,
        };
        Some(sa_core::l(key))
    }

    impl HttpFetch for UreqFetch {
        fn fetch(&self, request: HttpRequest, cancel: &CancellationToken) -> Result<(u16, String), Option<String>> {
            if cancel.is_cancelled() {
                return Err(Some(String::new()));
            }
            let (tx, rx) = crossbeam_channel::bounded(1);
            let spawned = std::thread::Builder::new().name("sa-http".into()).spawn(move || {
                let _ = tx.send(Self::run(&request));
            });
            if spawned.is_err() {
                return Err(Some(sa_core::l(strings::CONNECTION_FAILED)));
            }
            loop {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(result) => return result,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        if cancel.is_cancelled() {
                            return Err(Some(String::new()));
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Err(Some(sa_core::l(strings::CONNECTION_FAILED))),
                }
            }
        }
    }
}
