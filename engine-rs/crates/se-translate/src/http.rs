//! HTTP layer of the LLM translator, reproducing `urllib.request` + `_KeySafeRedirectHandler` (spec
//! translate-llm.md §7.4, §7.5).
//!
//! [`HttpTransport::open`] is the seam that replaces Python's `urllib.request.urlopen` (tests inject fakes that
//! record the first-hop request). The production [`UreqTransport`] sends one hop at a time with redirects
//! disabled and applies the urllib redirect rules itself, so the API key never follows a redirect to another
//! host or over plain http to the internet.

use std::collections::HashMap;
use std::time::Duration;

use se_core::{Value, json, pyfmt};

use crate::external::api_key_transport_allowed;
use crate::pyurl;

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
        req = next;
    }
}

fn strip_fragment(mut req: HttpRequest) -> HttpRequest {
    if let Some(i) = req.url.find('#') {
        req.url.truncate(i);
    }
    req
}

/// The production transport: `ureq` (HTTP/1.1, rustls), one hop at a time, per-read timeouts like Python's
/// socket timeouts, environment proxies except for loopback hosts.
pub struct UreqTransport {
    _private: (),
}

impl UreqTransport {
    pub fn new() -> Self {
        Self { _private: () }
    }

    fn agent(&self, url: &str, timeout: Duration) -> ureq::Agent {
        let host = pyurl::url_host(url).unwrap_or_default();
        let loopback = host == "localhost"
            || host.ends_with(".localhost")
            || pyurl::ip_address(&host).is_some_and(|ip| ip.is_loopback());
        let proxy = if loopback { None } else { ureq::Proxy::try_from_env() };
        ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(proxy)
            .timeout_connect(Some(timeout))
            .timeout_per_read(Some(timeout))
            .timeout_per_write(Some(timeout))
            .user_agent(ureq::config::AutoHeaderValue::None)
            .accept(ureq::config::AutoHeaderValue::None)
            .accept_encoding(ureq::config::AutoHeaderValue::Provided(std::sync::Arc::new("identity".to_string())))
            .max_idle_connections(0)
            .build()
            .new_agent()
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
        let agent = self.agent(&request.url, timeout);
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
        let reason = status.canonical_reason().unwrap_or("").to_string();
        let code = status.as_u16();
        let mut body = response.into_body();
        let bytes = body.with_config().limit(u64::MAX).read_to_vec().map_err(map_ureq_error)?;
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
    fn error_detail_shapes() {
        let f = |body: &str| HttpFailure::Http { code: 400, reason: "Bad Request".into(), body: body.as_bytes().to_vec() };
        assert_eq!(http_error_detail(&f(r#"{"error": "text"}"#), None), "HTTP 400: text");
        assert_eq!(http_error_detail(&f(r#"{"message": "x"}"#), None), "HTTP 400: x");
        assert_eq!(http_error_detail(&f(r#"["x"]"#), None), "HTTP Error 400: Bad Request");
        assert_eq!(http_error_detail(&f(r#"{"error": {"message": "a  key k1\n b"}}"#), Some("k1")), "HTTP 400: a key [redacted] b");
        assert_eq!(http_error_detail(&f(""), None), "HTTP Error 400: Bad Request");
    }
}
