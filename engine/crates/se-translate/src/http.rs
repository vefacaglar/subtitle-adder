//! HTTP layer of the LLM translator, reproducing `urllib.request` + `_KeySafeRedirectHandler` (spec
//! translate-llm.md §7.4, §7.5).
//!
//! [`HttpTransport::open`] is the seam that replaces Python's `urllib.request.urlopen` (tests inject fakes that
//! record the first-hop request). The production [`UreqTransport`] sends one hop at a time with redirects
//! disabled and applies the urllib redirect rules itself, so the API key never follows a redirect to another
//! host or over plain http to the internet.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport};

use se_core::{Value, json, pyfmt, t};

use crate::external::{api_key_transport_allowed, is_local_network_host};
use crate::pyurl;

/// Largest chat completion response body read into memory (larger ones fail the request).
pub const MAX_CHAT_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
/// Largest model discovery (`GET .../models`) response body read into memory.
pub const MAX_DISCOVERY_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// A request as `urllib.request.Request` holds it: the full URL, the method, the headers given at
/// construction (in order) and the body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    pub fn new(method: &str, url: &str, headers: Vec<(String, String)>, body: Option<Vec<u8>>) -> Self {
        Self { method: method.to_string(), url: url.to_string(), headers, body }
    }

    /// `Request.get_header(name)` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// The body parsed as JSON (tests).
    pub fn json_body(&self) -> Option<Value> {
        let body = self.body.as_ref()?;
        json::parse(std::str::from_utf8(body).ok()?).ok()
    }
}

/// A successful (2xx) response body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// A failed request: `urllib.error.HTTPError` (status outside 2xx, body kept for `_http_error_detail`) or any
/// other transport error (Python `str(exc)` text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpFailure {
    Http { code: u16, reason: String, body: Vec<u8> },
    Other(String),
}

impl HttpFailure {
    /// The HTTP status, if this is an `HTTPError`.
    pub fn code(&self) -> Option<u16> {
        match self {
            HttpFailure::Http { code, .. } => Some(*code),
            HttpFailure::Other(_) => None,
        }
    }
}

impl std::fmt::Display for HttpFailure {
    /// Python `str(exc)`: `HTTP Error {code}: {reason}` for an `HTTPError`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpFailure::Http { code, reason, .. } => write!(f, "HTTP Error {code}: {reason}"),
            HttpFailure::Other(text) => f.write_str(text),
        }
    }
}

/// `urllib.request.urlopen(req, timeout)` (redirects included).
pub trait HttpTransport {
    fn open(&self, request: &HttpRequest, timeout: Duration) -> Result<HttpResponse, HttpFailure>;
}

/// `HTTPRedirectHandler.redirect_request` + `_KeySafeRedirectHandler`: the follow-up request for a redirect
/// response, or the `HTTPError` urllib raises. `new_url` is the already joined and quoted target.
///
/// The new request is always a GET without a body, keeps every construction header except `Content-Length` /
/// `Content-Type`, and loses `Authorization` when the host name changes (case-insensitive, port and scheme
/// ignored) or the target may not carry a key ([`api_key_transport_allowed`]).
pub fn redirect_request(request: &HttpRequest, code: u16, reason: &str, new_url: &str) -> Result<HttpRequest, HttpFailure> {
    let m = request.method.as_str();
    let allowed = (matches!(code, 301 | 302 | 303 | 307 | 308) && matches!(m, "GET" | "HEAD"))
        || (matches!(code, 301 | 302 | 303) && m == "POST");
    if !allowed {
        return Err(HttpFailure::Http { code, reason: reason.to_string(), body: Vec::new() });
    }
    let new_url = new_url.replace(' ', "%20");
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("content-length") && !k.eq_ignore_ascii_case("content-type"))
        .cloned()
        .collect();
    let old_host = pyurl::url_host(&request.url).unwrap_or_default().to_lowercase();
    let new_host = pyurl::url_host(&new_url).unwrap_or_default().to_lowercase();
    if old_host != new_host || !api_key_transport_allowed(&new_url) {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
    }
    Ok(HttpRequest { method: "GET".to_string(), url: new_url, headers, body: None })
}

/// One raw HTTP exchange (no redirect handling): status, reason, `Location`, `URI` and body.
pub struct RawResponse {
    pub status: u16,
    pub reason: String,
    pub location: Option<String>,
    pub uri: Option<String>,
    pub body: Vec<u8>,
}

/// Sends a single hop. Implemented by [`UreqTransport`]; tests may implement it to exercise the redirect loop.
pub trait HopSender {
    fn send(&self, request: &HttpRequest, timeout: Duration) -> Result<RawResponse, HttpFailure>;
}

const INF_MSG: &str =
    "The HTTP server returned a redirect error that would lead to an infinite loop.\nThe last 30x error message was:\n";

/// urllib's `OpenerDirector.open` with `HTTPErrorProcessor`, `HTTPRedirectHandler.http_error_30x` (loop
/// detection: 4 repeats / 10 URLs) and the key-safe header rule, over any [`HopSender`].
pub fn open_with_redirects(sender: &dyn HopSender, request: &HttpRequest, timeout: Duration) -> Result<HttpResponse, HttpFailure> {
    let mut req = strip_fragment(request.clone());
    let mut visited: Option<HashMap<String, u32>> = None;
    loop {
        let resp = sender.send(&req, timeout)?;
        if (200..300).contains(&resp.status) {
            return Ok(HttpResponse { status: resp.status, body: resp.body });
        }
        let code = resp.status;
        if !matches!(code, 301 | 302 | 303 | 307 | 308) {
            return Err(HttpFailure::Http { code, reason: resp.reason, body: resp.body });
        }
        let Some(target) = resp.location.clone().or_else(|| resp.uri.clone()) else {
            return Err(HttpFailure::Http { code, reason: resp.reason, body: resp.body });
        };
        let parts = pyurl::urlsplit(&target).map_err(HttpFailure::Other)?;
        if !matches!(parts.scheme.as_str(), "http" | "https" | "ftp" | "") {
            return Err(HttpFailure::Http {
                code,
                reason: format!("{} - Redirection to url '{}' is not allowed", resp.reason, target),
                body: resp.body,
            });
        }
        let fixed = pyurl::fix_redirect_target(&target).map_err(HttpFailure::Other)?;
        let new_url = pyurl::urljoin(&req.url, &fixed);
        let mut next = redirect_request(&req, code, &resp.reason, &new_url)
            .map_err(|_| HttpFailure::Http { code, reason: resp.reason.clone(), body: resp.body.clone() })?;
        match visited.as_mut() {
            Some(v) => {
                if v.get(&new_url).copied().unwrap_or(0) >= 4 || v.len() >= 10 {
                    return Err(HttpFailure::Http { code, reason: format!("{INF_MSG}{}", resp.reason), body: resp.body });
                }
                *v.entry(new_url.clone()).or_insert(0) += 1;
            }
            None => {
                visited = Some(HashMap::from([(new_url.clone(), 1)]));
            }
        }
        if pyurl::urlsplit(&next.url).map(|p| p.scheme == "ftp").unwrap_or(false) {
            return Err(HttpFailure::Other("<urlopen error ftp error: redirects to ftp are not supported>".to_string()));
        }
        next = strip_fragment(next);
        // Strict offline mode: a LAN server must not bounce the request (key, subtitle text) to the internet.
        if se_core::offline::offline_requested() {
            let host = pyurl::url_host(&next.url);
            if !is_local_network_host(host.as_deref()) {
                let shown = host.unwrap_or_else(|| next.url.clone());
                return Err(HttpFailure::Other(t!("offline_external_llm", host = shown)));
            }
        }
        req = next;
    }
}

/// The proxy `urllib.request.ProxyHandler` would use for `url`, from the environment (`env(name)` reads one
/// variable): only the scheme's own `<scheme>_proxy` (lowercase wins, an empty lowercase value disables the
/// uppercase one; `ALL_PROXY` is never used), `HTTP_PROXY` ignored when `REQUEST_METHOD` is set (CGI; a
/// lowercase `http_proxy` still counts outside Windows, as in urllib), and
/// `no_proxy` honored. A host on the local network (loopback, private ranges, `.local`, single-label names,
/// see [`is_local_network_host`]) never goes through a proxy, so LAN requests stay on the LAN.
pub fn proxy_for_url(url: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let parts = pyurl::urlsplit(url).ok()?;
    let scheme = parts.scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if is_local_network_host(parts.hostname().as_deref()) {
        return None;
    }
    let proxy = if scheme == "http" && env("REQUEST_METHOD").is_some() {
        // CGI: `HTTP_PROXY` may come from a request header, so urllib drops it, but its second pass re-adds a
        // lowercase `http_proxy`. On Windows the names are one case-insensitive variable and Python's
        // `os.environ` keys are uppercase, so nothing is re-added there.
        if cfg!(windows) {
            return None;
        }
        env("http_proxy").filter(|v| !v.is_empty())?
    } else {
        env_pair(&format!("{scheme}_proxy"), env)?
    };
    let host = parts.netloc.rsplit('@').next().unwrap_or("").to_lowercase();
    if env_pair("no_proxy", env).is_some_and(|no| proxy_bypass(&host, &no)) {
        return None;
    }
    Some(proxy)
}

/// `getproxies_environment` for one name: the lowercase variable when set (empty = none), else the uppercase.
fn env_pair(lower: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    match env(lower) {
        Some(v) => Some(v).filter(|v| !v.is_empty()),
        None => env(&lower.to_uppercase()).filter(|v| !v.is_empty()),
    }
}

/// `proxy_bypass_environment(host, {"no": no_proxy})`; `host` is the lowercase `host[:port]`.
fn proxy_bypass(host: &str, no_proxy: &str) -> bool {
    if no_proxy == "*" {
        return true;
    }
    let host_only = match host.rfind(':') {
        Some(i) if host[i + 1..].chars().all(|c| c.is_ascii_digit()) && !host[..i].ends_with(':') => &host[..i],
        _ => host,
    };
    no_proxy.split(',').map(str::trim).filter(|n| !n.is_empty()).any(|name| {
        let name = name.trim_start_matches('.').to_lowercase();
        let dotted = format!(".{name}");
        host_only == name || host == name || host_only.ends_with(&dotted) || host.ends_with(&dotted)
    })
}

fn system_env(name: &str) -> Option<String> {
    std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
}

fn strip_fragment(mut req: HttpRequest) -> HttpRequest {
    if let Some(i) = req.url.find('#') {
        req.url.truncate(i);
    }
    req
}

/// The production transport: `ureq` (HTTP/1.1, rustls with the operating system's certificate verifier like the
/// model downloads), one hop at a time, per-read timeouts like Python's socket timeouts, the scheme's environment
/// proxy ([`proxy_for_url`]; never for local-network hosts).
pub struct UreqTransport {
    _private: (),
}

impl UreqTransport {
    pub fn new() -> Self {
        Self { _private: () }
    }

    fn config(&self, url: &str, timeout: Duration) -> ureq::config::Config {
        let proxy = proxy_for_url(url, &system_env).and_then(|p| ureq::Proxy::new(&p).ok());
        se_models::http::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(proxy)
            .timeout_connect(Some(timeout))
            .timeout_per_read(Some(timeout))
            .timeout_per_write(Some(timeout))
            .user_agent(ureq::config::AutoHeaderValue::None)
            .accept(ureq::config::AutoHeaderValue::None)
            .accept_encoding(ureq::config::AutoHeaderValue::Provided(Arc::new("identity".to_string())))
            .max_idle_connections(0)
            .build()
    }
}

/// Where [`ReasonSniffer`] stores the reason phrase of the response status line of one request.
type ReasonSlot = Arc<Mutex<Option<String>>>;

/// A connector at the end of ureq's default chain that wraps the (decrypted) transport so the HTTP/1.1 status
/// line can be read: ureq drops the reason phrase, but Python's `HTTPError` text (`HTTP Error 503: Model
/// Loading`) carries the server's own phrase, which may differ from the canonical one or be absent from it.
#[derive(Debug)]
struct ReasonSniffer(ReasonSlot);

impl Connector<Box<dyn Transport>> for ReasonSniffer {
    type Out = Box<dyn Transport>;

    fn connect(
        &self,
        _details: &ConnectionDetails,
        chained: Option<Box<dyn Transport>>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| Box::new(SniffTransport { inner, slot: self.0.clone(), done: false }) as Box<dyn Transport>))
    }
}

#[derive(Debug)]
struct SniffTransport {
    inner: Box<dyn Transport>,
    slot: ReasonSlot,
    done: bool,
}

/// `http.client.HTTPResponse._read_status`: the line is decoded as ISO-8859-1, split on whitespace at most
/// twice, and the reason is the stripped rest (`""` when absent). Returns `(status, reason)`.
fn parse_status_line(line: &[u8]) -> Option<(u16, String)> {
    let text: String = line.iter().map(|&b| b as char).collect();
    let mut parts = pyfmt::lstrip(&text).splitn(2, pyfmt::isspace);
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let rest = pyfmt::lstrip(parts.next().unwrap_or(""));
    let mut parts = rest.splitn(2, pyfmt::isspace);
    let status = parts.next()?.parse::<u16>().ok()?;
    let reason = pyfmt::strip(parts.next().unwrap_or("")).to_string();
    Some((status, reason))
}

impl Transport for SniffTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let progress = self.inner.await_input(timeout)?;
        if !self.done {
            let input = self.inner.buffers().input();
            if let Some(end) = input.windows(2).position(|w| w == b"\r\n") {
                if let Some((status, reason)) = parse_status_line(&input[..end]) {
                    *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
                    // Python skips interim 1xx responses; keep looking for the final status line.
                    self.done = status >= 200;
                } else {
                    self.done = true;
                }
            }
        }
        Ok(progress)
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

fn map_ureq_error(err: ureq::Error) -> HttpFailure {
    match err {
        ureq::Error::Timeout(_) => HttpFailure::Other("timed out".to_string()),
        ureq::Error::Io(e) => {
            if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock {
                HttpFailure::Other("timed out".to_string())
            } else {
                HttpFailure::Other(format!("<urlopen error {}>", pyfmt::winerror_text(&e, None, None)))
            }
        }
        ureq::Error::HostNotFound => HttpFailure::Other(if cfg!(windows) {
            "<urlopen error [Errno 11001] getaddrinfo failed>".to_string()
        } else {
            "<urlopen error [Errno -2] Name or service not known>".to_string()
        }),
        other => HttpFailure::Other(format!("<urlopen error {other}>")),
    }
}

impl HopSender for UreqTransport {
    fn send(&self, request: &HttpRequest, timeout: Duration) -> Result<RawResponse, HttpFailure> {
        let slot: ReasonSlot = Arc::new(Mutex::new(None));
        let connector = DefaultConnector::new().chain(ReasonSniffer(slot.clone()));
        let agent = ureq::Agent::with_parts(self.config(&request.url, timeout), connector, DefaultResolver::default());
        let mut builder = http_request_builder(request);
        for (k, v) in &request.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let result = match &request.body {
            Some(body) => {
                let req = builder.body(body.clone()).map_err(|e| HttpFailure::Other(format!("<urlopen error {e}>")))?;
                agent.run(req)
            }
            None => {
                let req = builder.body(()).map_err(|e| HttpFailure::Other(format!("<urlopen error {e}>")))?;
                agent.run(req)
            }
        };
        let response = result.map_err(map_ureq_error)?;
        let status = response.status();
        let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        let location = header("location");
        let uri = header("uri");
        let sniffed = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        let reason = sniffed.unwrap_or_else(|| status.canonical_reason().unwrap_or("").to_string());
        let code = status.as_u16();
        let limit = if request.method == "GET" { MAX_DISCOVERY_RESPONSE_BYTES } else { MAX_CHAT_RESPONSE_BYTES };
        let mut body = response.into_body();
        let bytes = body.with_config().limit(limit).read_to_vec().map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => {
                HttpFailure::Other(t!("external_llm_response_too_large", limit = limit / (1024 * 1024)))
            }
            other => map_ureq_error(other),
        })?;
        Ok(RawResponse { status: code, reason, location, uri, body: bytes })
    }
}

fn http_request_builder(request: &HttpRequest) -> ureq::http::request::Builder {
    ureq::http::Request::builder().method(request.method.as_str()).uri(request.url.as_str())
}

impl HttpTransport for UreqTransport {
    fn open(&self, request: &HttpRequest, timeout: Duration) -> Result<HttpResponse, HttpFailure> {
        open_with_redirects(self, request, timeout)
    }
}

/// `_http_error_detail(exc, api_key)`: the gateway's `error.message` as `HTTP <code>: <message>` (body read up
/// to 8192 bytes; invalid/HTML JSON keeps `str(exc)`), the key redacted, whitespace collapsed, at most 1000
/// characters.
pub fn http_error_detail(failure: &HttpFailure, api_key: Option<&str>) -> String {
    let mut detail = failure.to_string();
    if let HttpFailure::Http { code, body, .. } = failure {
        let head = &body[..body.len().min(8192)];
        if let Ok(text) = std::str::from_utf8(head) {
            if let Ok(payload) = json::parse(text) {
                let error = match &payload {
                    Value::Object(map) => Some(map.get("error").cloned().unwrap_or_else(|| payload.clone())),
                    _ => None,
                };
                let message = match &error {
                    Some(Value::Object(e)) => e.get("message").cloned(),
                    other => other.clone(),
                };
                if let Some(Value::Str(message)) = message {
                    if !pyfmt::strip(&message).is_empty() {
                        detail = format!("HTTP {code}: {message}");
                    }
                }
            }
        }
    }
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        detail = detail.replace(key, "[redacted]");
    }
    let collapsed = crate::pyunicode::collapse_whitespace(&detail);
    pyfmt::char_prefix(&collapsed, 1000).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_error_does_not_dump_html_or_large_body() {
        let failure = HttpFailure::Http { code: 502, reason: "Bad Gateway".into(), body: b"<html>".repeat(3000) };
        assert_eq!(http_error_detail(&failure, None), "HTTP Error 502: Bad Gateway");
    }

    #[test]
    fn status_line_reason_like_http_client() {
        assert_eq!(parse_status_line(b"HTTP/1.1 503 Model Loading "), Some((503, "Model Loading".to_string())));
        assert_eq!(parse_status_line(b"HTTP/1.1 599"), Some((599, String::new())));
        assert_eq!(parse_status_line(b"HTTP/1.0 200 OK"), Some((200, "OK".to_string())));
        assert_eq!(parse_status_line(b"HTTP/1.1 418 I\xe7m"), Some((418, "I\u{e7}m".to_string())));
        assert_eq!(parse_status_line(b"garbage"), None);
    }

    #[test]
    fn real_response_keeps_the_servers_reason_phrase() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for reply in [&b"HTTP/1.1 503 Model Loading\r\nContent-Length: 1\r\n\r\nx"[..], b"HTTP/1.1 599\r\nContent-Length: 0\r\n\r\n"] {
                let (mut conn, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = conn.read(&mut buf);
                conn.write_all(reply).unwrap();
            }
        });
        let transport = UreqTransport::new();
        let req = HttpRequest::new("GET", &format!("http://127.0.0.1:{port}/v1/models"), Vec::new(), None);
        let first = transport.open(&req, Duration::from_secs(5)).unwrap_err();
        assert_eq!(first.to_string(), "HTTP Error 503: Model Loading");
        let second = transport.open(&req, Duration::from_secs(5)).unwrap_err();
        assert_eq!(second.to_string(), "HTTP Error 599: ");
        server.join().unwrap();
    }

    fn env_of(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
    }

    #[test]
    fn proxy_is_only_the_schemes_own_and_never_all_proxy() {
        let env = env_of(&[("all_proxy", "http://all:1"), ("https_proxy", "http://secure:3128")]);
        assert_eq!(proxy_for_url("https://api.openai.com/v1/chat/completions", &env).as_deref(), Some("http://secure:3128"));
        // No http_proxy: plain http goes direct instead of through ALL_PROXY or the https proxy.
        assert_eq!(proxy_for_url("http://api.example.com:8080/v1/chat/completions", &env), None);
        let env = env_of(&[("HTTP_PROXY", "http://plain:8080")]);
        assert_eq!(proxy_for_url("http://api.example.com/v1", &env).as_deref(), Some("http://plain:8080"));
        assert_eq!(proxy_for_url("https://api.example.com/v1", &env), None);
        // An empty lowercase variable disables the uppercase one (urllib).
        let env = env_of(&[("http_proxy", ""), ("HTTP_PROXY", "http://plain:8080")]);
        assert_eq!(proxy_for_url("http://api.example.com/v1", &env), None);
        // CGI: HTTP_PROXY may come from a request header and is dropped; urllib re-adds a lowercase http_proxy
        // (outside Windows, where the two names are the same variable).
        let env = env_of(&[("HTTP_PROXY", "http://plain:8080"), ("REQUEST_METHOD", "GET")]);
        assert_eq!(proxy_for_url("http://api.example.com/v1", &env), None);
        let env = env_of(&[("http_proxy", "http://lower:8080"), ("REQUEST_METHOD", "GET")]);
        let expected = if cfg!(windows) { None } else { Some("http://lower:8080") };
        assert_eq!(proxy_for_url("http://api.example.com/v1", &env).as_deref(), expected);
        let env = env_of(&[("https_proxy", "http://secure:3128"), ("REQUEST_METHOD", "GET")]);
        assert_eq!(proxy_for_url("https://api.example.com/v1", &env).as_deref(), Some("http://secure:3128"));
    }

    #[test]
    fn local_network_hosts_never_use_a_proxy() {
        let env = env_of(&[("http_proxy", "http://p:1"), ("https_proxy", "http://p:1")]);
        for url in [
            "http://127.0.0.1:8080/v1/chat/completions",
            "http://localhost:11434/api/chat",
            "http://192.168.1.42:1234/v1/models",
            "http://10.0.0.5:1234/v1",
            "http://[fd00::1]:1234/v1",
            "http://mac-studio.local:1234/v1",
            "https://mac-studio.local:1234/v1",
            "http://ollama:11434/v1",
        ] {
            assert_eq!(proxy_for_url(url, &env), None, "{url}");
        }
        assert_eq!(proxy_for_url("http://203.0.113.9:1234/v1", &env).as_deref(), Some("http://p:1"));
    }

    #[test]
    fn no_proxy_is_honored() {
        let env = env_of(&[("https_proxy", "http://p:1"), ("no_proxy", " .example.com, other.org:443 ")]);
        assert_eq!(proxy_for_url("https://api.example.com/v1", &env), None);
        assert_eq!(proxy_for_url("https://example.com/v1", &env), None);
        assert_eq!(proxy_for_url("https://other.org/v1", &env).as_deref(), Some("http://p:1"));
        assert_eq!(proxy_for_url("https://other.org:443/v1", &env), None);
        assert_eq!(proxy_for_url("https://notexample.com/v1", &env).as_deref(), Some("http://p:1"));
        let env = env_of(&[("https_proxy", "http://p:1"), ("NO_PROXY", "*")]);
        assert_eq!(proxy_for_url("https://api.openai.com/v1", &env), None);
    }

    #[test]
    fn oversized_response_bodies_are_refused() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let size = MAX_DISCOVERY_RESPONSE_BYTES as usize + 1;
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let _ = conn.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n\r\n").as_bytes());
            let chunk = vec![b' '; 1 << 20];
            let mut left = size;
            while left > 0 {
                let n = left.min(chunk.len());
                if conn.write_all(&chunk[..n]).is_err() {
                    break;
                }
                left -= n;
            }
        });
        let req = HttpRequest::new("GET", &format!("http://127.0.0.1:{port}/v1/models"), Vec::new(), None);
        let err = UreqTransport::new().open(&req, Duration::from_secs(10)).unwrap_err();
        assert_eq!(err, HttpFailure::Other(t!("external_llm_response_too_large", limit = 16)));
        server.join().unwrap();
    }

    #[test]
    fn error_detail_shapes() {
        let f = |body: &str| HttpFailure::Http { code: 400, reason: "Bad Request".into(), body: body.as_bytes().to_vec() };
        assert_eq!(http_error_detail(&f(r#"{"error": "text"}"#), None), "HTTP 400: text");
        assert_eq!(http_error_detail(&f(r#"{"message": "x"}"#), None), "HTTP 400: x");
        assert_eq!(http_error_detail(&f(r#"["x"]"#), None), "HTTP Error 400: Bad Request");
        assert_eq!(http_error_detail(&f(r#"{"error": {"message": "a  key k1\n b"}}"#), Some("k1")), "HTTP 400: a key [redacted] b");
        assert_eq!(http_error_detail(&f(""), None), "HTTP Error 400: Bad Request");
    }
}
