//! Windows connection test and model discovery (win-providers §5, §6.3) with the shared key-safety rules of
//! UNIFIED.md §3.7: a key is never sent over plain `http://` to a host outside the local network (the engine's
//! localized `https://` error is returned before any request), the `.local` subnet scan is skipped when a key is set,
//! and the LAN-section test sends no key at all.
//!
//! HTTP goes through [`HttpFetch`] (the app passes `sa_enginekit::external_llm::UreqFetch`; tests pass fakes). The
//! per-request timeout belongs to the fetcher (8 s for `UreqFetch`).

use sa_core::cancel::CancellationToken;
use sa_core::l10n::Arg;
use sa_enginekit::external_llm::{
    ExternalLlmServer, HttpFetch, HttpRequest, OPENCODE_GO_CHAT_URL, OPENCODE_GO_MODELS_URL, USER_AGENT, parse_url,
};
use sa_enginekit::presets::TranslationProviderPreset;

use crate::fold_case;

/// Result of a test (`(success, message, models)` of the C# app).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTestOutcome {
    pub success: bool,
    /// Localized, one line; never contains the key.
    pub message: String,
    pub models: Vec<String>,
}

impl ProviderTestOutcome {
    fn failure(message: String, models: Vec<String>) -> Self {
        Self { success: false, message, models }
    }
}

/// The caller cancelled the test (C# `OperationCanceledException`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestCancelled;

fn ends_with_ci(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len() && s.is_char_boundary(s.len() - suffix.len()) && s[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// .NET `Contains(needle, OrdinalIgnoreCase)`: a non-ASCII character never matches an ASCII one (the Kelvin sign is
/// not `k`, as `to_lowercase` would make it).
fn contains_ci(s: &str, needle: &str) -> bool {
    fold_case(s).contains(&fold_case(needle))
}

/// The six cloud presets' addresses and host names (win-providers §3.8).
const CLOUD_HOSTS: [&str; 6] =
    ["opencode.ai", "openrouter.ai", "api.openai.com", "api.deepseek.com", "api.groq.com", "generativelanguage.googleapis.com"];

/// True for an address of a cloud preset (prefix of a non-LAN preset URL, or one of the cloud host names), so a
/// cloud URL never overwrites the LAN profile or the legacy LAN settings.
pub fn is_cloud_provider_url(url: &str) -> bool {
    let t = url.trim();
    if t.is_empty() {
        return false;
    }
    let by_prefix = TranslationProviderPreset::ALL.iter().any(|p| {
        p.id != TranslationProviderPreset::LAN_ID && !p.base_url.trim().is_empty() && starts_with_ci(t, p.base_url.trim().trim_end_matches('/'))
    });
    by_prefix || CLOUD_HOSTS.iter().any(|h| contains_ci(t, h))
}

/// OpenCode Go address (saved legacy addresses included), with the .NET `Uri` semantics of the C# check: https,
/// `Authority` exactly `opencode.ai` (the default port, written or not, is omitted from it and user info is not part
/// of it), no query and no fragment at all (`Query.Length == 0`: a bare `?` counts), and the canonical
/// `AbsolutePath` (backslashes as slashes, unreserved `%XX` decoded, dot segments removed; trailing `/` trimmed) in
/// the fixed list (case-sensitive).
pub fn is_opencode_go_url(url: &str) -> bool {
    // .NET trims surrounding whitespace and reads `\` in the path as `/` (not in the authority).
    let url = url.trim();
    let url = match url.find("://").and_then(|s| url[s + 3..].find('/').map(|i| s + 3 + i)) {
        Some(path_start) => format!("{}{}", &url[..path_start], url[path_start..].replace('\\', "/")),
        None => url.to_owned(),
    };
    let Some(p) = parse_url(&url) else { return false };
    if !p.scheme.eq_ignore_ascii_case("https") || !p.host.eq_ignore_ascii_case("opencode.ai") {
        return false;
    }
    if p.port.is_some_and(|port| !port.is_empty() && port.parse::<u32>().ok() != Some(443)) {
        return false;
    }
    if p.query.is_some() || p.fragment.is_some() {
        return false;
    }
    matches!(
        dotnet_absolute_path(p.path).trim_end_matches('/'),
        "/zen/go/v1" | "/zen/go/v1/chat/completions" | "/inference/go/v1" | "/inference/go/openai/v1" | "/inference/go/openai/v1/chat/completions"
    )
}

/// .NET `Uri.AbsolutePath` canonical form of a path: `%XX` escapes of unreserved characters decoded, dot segments
/// removed (RFC 3986 §5.2.4), an empty path becomes `/`.
fn dotnet_absolute_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut decoded = String::with_capacity(path.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = ((bytes[i + 1] as char).to_digit(16), (bytes[i + 2] as char).to_digit(16))
        {
            let c = (h * 16 + l) as u8 as char;
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~') {
                decoded.push(c);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i] as char);
        i += 1;
    }
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = decoded.split('/').collect();
    for (n, seg) in segments.iter().enumerate() {
        let last = n + 1 == segments.len();
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if joined.is_empty() { "/".to_owned() } else if joined.starts_with('/') { joined } else { format!("/{joined}") }
}

/// What the C# tests take from `Uri.TryCreate(url, UriKind.Absolute, out uri) && uri.Host != ""` for an `http(s)://`
/// address: `Some(uri.Host)` (the host label of the success message), else `None` ("enter a valid address").
/// Emulates the .NET rules (verified against the C# code by `tests/csharp_behavior.rs`):
///
/// * the authority ends at the first `/`, `?` or `#`; a `\` inside it, more than one `@`, an empty host, a port that
///   is not all digits or above 65535 make the address invalid (an empty port is the default one);
/// * `[v6]` (an optional `%zone` dropped) → `[canonical v6]`; text after `]` that is not a port is ignored;
/// * an inet_aton-style IPv4 (1–4 parts, decimal, `0x` hex or `0` octal) → the dotted quad (`127.1` → `127.0.0.1`);
/// * otherwise a host name: labels of ASCII letters, digits, `-`, `_` or any non-ASCII character, none empty except
///   a single trailing dot; lower-cased (invariant, one-to-one mappings only) only when it has an ASCII capital.
pub(crate) fn dotnet_uri_host(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = if starts_with_ci(url, "http://") {
        &url[7..]
    } else if starts_with_ci(url, "https://") {
        &url[8..]
    } else {
        return None;
    };
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    if authority.contains('\\') || authority.matches('@').count() > 1 {
        return None;
    }
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let port_ok = |p: &str| {
        let significant = p.trim_start_matches('0');
        p.bytes().all(|b| b.is_ascii_digit()) && significant.len() <= 5 && significant.parse::<u32>().map_or(true, |v| v <= 65535)
    };
    if let Some(inner) = hostport.strip_prefix('[') {
        let close = inner.find(']')?;
        let addr = inner[..close].split('%').next().unwrap_or("");
        let ip: std::net::Ipv6Addr = addr.parse().ok()?;
        if let Some(port) = inner[close + 1..].strip_prefix(':')
            && !port_ok(port)
        {
            return None;
        }
        return Some(format!("[{ip}]"));
    }
    let (host, port) = hostport.rsplit_once(':').unwrap_or((hostport, ""));
    if host.is_empty() || !port_ok(port) {
        return None;
    }
    if let Some(v4) = inet_aton(host) {
        return Some(v4.to_string());
    }
    let labels = host.strip_suffix('.').unwrap_or(host);
    let label_ok = |l: &str| {
        !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_') || (!c.is_ascii() && !c.is_control()))
    };
    if !labels.split('.').all(label_ok) {
        return None;
    }
    if !host.bytes().any(|b| b.is_ascii_uppercase()) {
        return Some(host.to_owned());
    }
    Some(
        host.chars()
            .map(|c| {
                let mut lower = c.to_lowercase();
                match (lower.next(), lower.next()) {
                    (Some(l), None) => l,
                    _ => c,
                }
            })
            .collect(),
    )
}

/// The .NET (and inet_aton) IPv4 forms: 1–4 dot-separated parts, each decimal, `0x` hex or `0`-prefixed octal; the
/// last part fills the remaining bytes. `None` when the text is not such an address (it is then a host name).
fn inet_aton(host: &str) -> Option<std::net::Ipv4Addr> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut values = Vec::with_capacity(parts.len());
    for part in &parts {
        let (digits, radix) = if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
            (hex, 16)
        } else if part.len() > 1 && part.starts_with('0') {
            (&part[1..], 8)
        } else {
            (*part, 10)
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        values.push(u64::from_str_radix(digits, radix).ok()?);
    }
    let (last, head) = values.split_last()?;
    if head.iter().any(|v| *v > 255) || *last >= 1u64 << (8 * (5 - values.len())) {
        return None;
    }
    let mut n: u64 = *last;
    for (i, v) in head.iter().enumerate() {
        n |= v << (8 * (3 - i));
    }
    Some(std::net::Ipv4Addr::from(n as u32))
}

/// GET endpoints tried for model discovery (deduplicated case-insensitively).
pub fn candidate_model_endpoints(resolved: &str) -> Vec<String> {
    let mut clean = resolved.trim_end_matches('/').to_owned();
    if is_opencode_go_url(&clean) {
        return vec![OPENCODE_GO_MODELS_URL.to_owned()];
    }
    if ends_with_ci(&clean, "/chat/completions") {
        clean.truncate(clean.len() - "/chat/completions".len());
        clean = clean.trim_end_matches('/').to_owned();
    } else if ends_with_ci(&clean, "/models") {
        return vec![clean];
    }
    let list = if ends_with_ci(&clean, "/v1") || ends_with_ci(&clean, "/openai") {
        vec![format!("{clean}/models")]
    } else {
        vec![format!("{clean}/v1/models"), format!("{clean}/models"), format!("{clean}/api/tags")]
    };
    let mut seen = std::collections::HashSet::new();
    list.into_iter().filter(|u| seen.insert(u.to_lowercase())).collect()
}

/// The chat endpoint of the probe.
pub fn chat_completions_endpoint(resolved: &str) -> String {
    let clean = resolved.trim_end_matches('/');
    if is_opencode_go_url(clean) {
        OPENCODE_GO_CHAT_URL.to_owned()
    } else if ends_with_ci(clean, "/chat/completions") || ends_with_ci(clean, "/api/chat") {
        clean.to_owned()
    } else if ends_with_ci(clean, "/v1") || ends_with_ci(clean, "/openai") {
        format!("{clean}/chat/completions")
    } else {
        format!("{clean}/v1/chat/completions")
    }
}

/// `HTTP <status>[: <gateway message>]`, key redacted, CR/LF replaced by spaces, at most 1000 characters.
pub fn read_provider_error(status: u16, body: &str, api_key: &str) -> String {
    let mut detail = format!("HTTP {status}");
    let mut cut = body.len().min(8192);
    while !body.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &body[..cut];
    if crate::json_depth_within(head, crate::DOTNET_MAX_JSON_DEPTH)
        && let Ok(root) = serde_json::from_str::<serde_json::Value>(head)
    {
        let mut e = &root;
        if let Some(inner) = e.as_object().and_then(|o| o.get("error")) {
            e = inner;
        }
        if let Some(inner) = e.as_object().and_then(|o| o.get("message")) {
            e = inner;
        }
        if let Some(s) = e.as_str() {
            detail.push_str(": ");
            detail.push_str(s);
        }
    }
    if !api_key.is_empty() {
        detail = detail.replace(api_key, "[redacted]");
    }
    let detail = detail.replace(['\r', '\n'], " ");
    take_utf16_units(&detail, 1000)
}

/// The longest prefix of at most `max` UTF-16 code units (C# `s[..Math.Min(s.Length, max)]`; a surrogate pair the
/// C# cut would split is dropped whole).
fn take_utf16_units(s: &str, max: usize) -> String {
    let mut units = 0;
    let mut end = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        end = i + c.len_utf8();
    }
    s[..end].to_owned()
}

/// Discovered, then suggested (trimmed, new case-insensitively); an explicit model is inserted first when new.
pub fn merge_models(discovered: &[String], suggested: &[String], explicit: Option<&str>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in discovered.iter().chain(suggested) {
        let t = m.trim();
        if !t.is_empty() && seen.insert(fold_case(t)) {
            out.push(t.to_owned());
        }
    }
    if let Some(e) = explicit.map(str::trim).filter(|e| !e.is_empty())
        && seen.insert(fold_case(e))
    {
        out.insert(0, e.to_owned());
    }
    out
}

/// Model ids of a `/models` (`data`) or Ollama `/api/tags` (`models`) body without embedding models.
pub fn parse_suitable_external_models(json: &str) -> Vec<String> {
    if !crate::json_depth_within(json, crate::DOTNET_MAX_JSON_DEPTH) {
        return Vec::new();
    }
    let Ok(serde_json::Value::Object(root)) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for prop in ["data", "models"] {
        let Some(serde_json::Value::Array(items)) = root.get(prop) else { continue };
        for item in items {
            let Some(obj) = item.as_object() else { continue };
            let name = match (obj.get("id"), obj.get("name")) {
                (Some(serde_json::Value::String(id)), _) => id,
                (_, Some(serde_json::Value::String(n))) => n,
                _ => continue,
            };
            let t = name.trim();
            if t.is_empty() {
                continue;
            }
            let lower = t.to_lowercase();
            if lower.contains("embed") || lower.contains("minilm") || lower.contains("bge-") {
                continue;
            }
            if seen.insert(fold_case(t)) {
                out.push(t.to_owned());
            }
        }
    }
    out
}

/// The LAN-section address rule, exactly the C# `NormalizeExternalBaseUrl`: trim, drop trailing `/`, `None` when
/// empty, add `http://` without a scheme, strip the first matching API suffix (and trailing `/` again), `None` when
/// nothing is left. It does not validate the host (`"http://"` gives `"http://http:"`); the test then fails to
/// reach it, as in the C# app.
pub fn normalize_external_base_url(raw: &str) -> Option<String> {
    let mut url = raw.trim().trim_end_matches('/').to_owned();
    if url.is_empty() {
        return None;
    }
    if !starts_with_ci(&url, "http://") && !starts_with_ci(&url, "https://") {
        url = format!("http://{url}");
    }
    for suffix in ["/v1/chat/completions", "/chat/completions", "/api/chat", "/api/tags", "/v1/models", "/v1"] {
        if ends_with_ci(&url, suffix) {
            url.truncate(url.len() - suffix.len());
            url = url.trim_end_matches('/').to_owned();
            break;
        }
    }
    (!url.is_empty()).then_some(url)
}

fn request(method: &'static str, url: &str, api_key: &str, session: &str, body: Option<String>) -> HttpRequest {
    let mut headers = vec![("User-Agent".to_owned(), USER_AGENT.to_owned()), ("Accept".to_owned(), "application/json".to_owned())];
    if is_opencode_go_url(url) || url == OPENCODE_GO_MODELS_URL {
        headers.push(("x-opencode-session".to_owned(), session.to_owned()));
    }
    let key = api_key.trim();
    if !key.is_empty() {
        headers.push(("Authorization".to_owned(), format!("Bearer {key}")));
    }
    if body.is_some() {
        headers.push(("Content-Type".to_owned(), "application/json; charset=utf-8".to_owned()));
    }
    HttpRequest { method, url: url.to_owned(), headers, body }
}

fn probe_body(model: &str) -> String {
    // C# key order: model, messages, max_tokens, stream (serde_json keeps insertion order here).
    serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1,
        "stream": false,
    })
    .to_string()
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Per-request timeout of the legacy LAN-section test (C# uses a separate `HttpClient` with 3.5 s there).
pub const LAN_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(3500);

/// Wraps a fetcher with a shorter per-request deadline: the request runs on a scoped thread under a child token
/// that is cancelled when the deadline passes, and a timed-out request reports `Err(None)` (a timeout) while the
/// caller's own cancellation still reports as cancellation.
pub(crate) struct DeadlineFetch<'a> {
    pub inner: &'a dyn HttpFetch,
    pub timeout: std::time::Duration,
}

impl HttpFetch for DeadlineFetch<'_> {
    fn fetch(&self, request: HttpRequest, cancel: &CancellationToken) -> Result<(u16, String), Option<String>> {
        let child = cancel.child();
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let token = child.clone();
            let inner = self.inner;
            scope.spawn(move || {
                let _ = tx.send(inner.fetch(request, &token));
            });
            match rx.recv_timeout(self.timeout) {
                Ok(result) => result,
                Err(_) => {
                    // The deadline passed: stop the request (the scope waits for the thread to notice).
                    child.cancel();
                    if cancel.is_cancelled() { Err(Some(String::new())) } else { Err(None) }
                }
            }
        })
    }
}

/// `.local` resolution of the test: `(resolved URL, host label)`, `host` being the .NET host of `raw`
/// ([`dotnet_uri_host`]). Without a key the cached address and the /24 scan are tried after DNS; with one only DNS
/// (engine rule).
fn resolve_dot_local(raw: &str, host: String, has_key: bool, cancel: &CancellationToken) -> (String, String) {
    if !ends_with_ci(&host, ".local") {
        return (raw.to_owned(), host);
    }
    let resolved = ExternalLlmServer::resolve_local_host(raw, has_key, cancel);
    if resolved == raw {
        return (raw.to_owned(), host);
    }
    let ip = parse_url(&resolved).map(|p| p.host.to_owned()).unwrap_or_default();
    (resolved, format!("{host} \u{2192} {ip}"))
}

/// The draft / saved-profile test (win-providers §5.2). `session` is the manager's instance id.
pub(crate) fn test_provider(
    preset_id: &str,
    base_url: &str,
    api_key: &str,
    model: Option<&str>,
    session: &str,
    fetch: &dyn HttpFetch,
    cancel: &CancellationToken,
) -> Result<ProviderTestOutcome, TestCancelled> {
    let preset = TranslationProviderPreset::preset(Some(preset_id));
    let suggested: Vec<String> = preset.suggested_models.iter().map(|s| (*s).to_owned()).collect();
    let invalid = || ProviderTestOutcome::failure(sa_core::l("Lütfen geçerli bir sunucu adresi girin."), suggested.clone());
    let mut raw = base_url.trim().trim_end_matches('/').to_owned();
    if raw.is_empty() {
        return Ok(invalid());
    }
    if !starts_with_ci(&raw, "http://") && !starts_with_ci(&raw, "https://") {
        raw = format!("http://{raw}");
    }
    let Some(host) = dotnet_uri_host(&raw) else { return Ok(invalid()) };
    let key = api_key.trim();
    let (resolved, host_label) = resolve_dot_local(&raw, host.clone(), !key.is_empty(), cancel);
    if cancel.is_cancelled() {
        return Err(TestCancelled);
    }
    if !key.is_empty() && is_insecure_key_destination(&resolved, &host) {
        return Ok(ProviderTestOutcome::failure(ExternalLlmServer::insecure_key_message(), suggested));
    }

    let started = std::time::Instant::now();
    let mut discovered: Vec<String> = Vec::new();
    let mut reachable = false;
    let mut auth_code: Option<u16> = None;
    let mut last_error: Option<String> = None;
    for endpoint in candidate_model_endpoints(&resolved) {
        match fetch.fetch(request("GET", &endpoint, api_key, session, None), cancel) {
            Ok((status, _)) if status == 401 || status == 403 => {
                auth_code = Some(status);
                break;
            }
            Ok((status, body)) if is_success(status) => {
                reachable = true;
                discovered = parse_suitable_external_models(&body);
                if !discovered.is_empty() {
                    break;
                }
            }
            Ok(_) => {}
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(TestCancelled);
                }
                last_error = Some(ExternalLlmServer::failure_reason(e));
            }
        }
    }
    if let Some(code) = auth_code {
        return Ok(ProviderTestOutcome::failure(
            sa_core::l_args("API anahtarı geçersiz veya yetkisiz (HTTP %lld).", &[Arg::Int(code as i64)]),
            suggested,
        ));
    }
    if !reachable {
        let chat = chat_completions_endpoint(&resolved);
        let test_model = model
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .or_else(|| preset.suggested_models.first().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "gpt-4o-mini".to_owned());
        match fetch.fetch(request("POST", &chat, api_key, session, Some(probe_body(&test_model))), cancel) {
            Ok((status, _)) if status == 401 || status == 403 => {
                return Ok(ProviderTestOutcome::failure(
                    sa_core::l_args("API anahtarı geçersiz veya yetkisiz (HTTP %lld).", &[Arg::Int(status as i64)]),
                    suggested,
                ));
            }
            Ok((status, _)) if is_success(status) => reachable = true,
            Ok((status, body)) => last_error = Some(read_provider_error(status, &body, api_key)),
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(TestCancelled);
                }
                last_error = Some(ExternalLlmServer::failure_reason(e));
            }
        }
    }
    let elapsed = started.elapsed();
    let fallback: Vec<String> =
        if !discovered.is_empty() && preset.id == TranslationProviderPreset::LAN_ID { Vec::new() } else { suggested.clone() };
    let merged = merge_models(&discovered, &fallback, model);
    if !reachable {
        let reason = single_line(&last_error.unwrap_or_else(|| "timeout".to_owned()), api_key);
        return Ok(ProviderTestOutcome::failure(sa_core::l_args("Sunucuya ulaşılamadı: %@", &[Arg::Str(reason)]), merged));
    }
    let ms = (elapsed.as_millis() as i64).max(1);
    let message = sa_core::l_args(
        "Bağlantı başarılı (%@ · %lld ms · %lld uygun model bulundu)",
        &[Arg::Str(host_label), Arg::Int(ms), Arg::Int(merged.len() as i64)],
    );
    Ok(ProviderTestOutcome { success: true, message, models: merged })
}

/// The engine's key rule on the address a test is about to call: plain `http://` to a host outside the local network.
/// The host is the canonical one of `resolved` (`127.1` is loopback), or the typed address's .NET host when the HTTP
/// stack's parser cannot read it (a non-ASCII name, a space in the path): such an address is refused rather than
/// waved through.
fn is_insecure_key_destination(resolved: &str, typed_host: &str) -> bool {
    if !starts_with_ci(resolved.trim(), "http://") {
        return false;
    }
    let host = dotnet_uri_host(resolved).unwrap_or_else(|| typed_host.to_owned());
    !ExternalLlmServer::is_local_network_host(&host)
}

/// Transport errors are shown on one line and never carry the key.
fn single_line(text: &str, api_key: &str) -> String {
    let mut t = text.replace(['\r', '\n'], " ");
    let key = api_key.trim();
    if !key.is_empty() {
        t = t.replace(key, "[redacted]");
    }
    t.chars().take(1000).collect()
}

/// The legacy "Harici LLM sunucusu (Yerel ağ)" test (`TestExternalLlmConnectionAsync`, win-providers §6.3): GET
/// `{base}/api/tags`, `{base}/v1/models`, `{base}/models`, non-2xx skipped, no 401 handling, no chat probe. Sends
/// **no** API key (UNIFIED.md §3.7: the active cloud provider's key must not reach a LAN server). `models` are the
/// discovered ones (possibly empty). Each request is limited to [`LAN_TEST_TIMEOUT`] on top of the fetcher's own.
pub fn test_lan_server(
    raw_url: &str,
    fetch: &dyn HttpFetch,
    cancel: &CancellationToken,
) -> Result<ProviderTestOutcome, TestCancelled> {
    let fetch = &DeadlineFetch { inner: fetch, timeout: LAN_TEST_TIMEOUT };
    let Some(base) = normalize_external_base_url(raw_url) else {
        return Ok(ProviderTestOutcome::failure(sa_core::l("Lütfen geçerli bir sunucu adresi girin."), Vec::new()));
    };
    // C#: the host label and `.local` resolution only when the address parses; otherwise the requests are still
    // made (and fail), with an empty label.
    let (resolved, host_label) = match dotnet_uri_host(&base) {
        Some(host) => resolve_dot_local(&base, host, false, cancel),
        None => (base.clone(), String::new()),
    };
    if cancel.is_cancelled() {
        return Err(TestCancelled);
    }
    let started = std::time::Instant::now();
    let mut discovered = Vec::new();
    let mut reachable = false;
    let mut last_error: Option<String> = None;
    for endpoint in [format!("{resolved}/api/tags"), format!("{resolved}/v1/models"), format!("{resolved}/models")] {
        // A bare request, as the C# LAN test sends it (no User-Agent / Accept; no key, UNIFIED.md §3.7).
        let request = HttpRequest { method: "GET", url: endpoint, headers: Vec::new(), body: None };
        match fetch.fetch(request, cancel) {
            Ok((status, body)) if is_success(status) => {
                reachable = true;
                discovered = parse_suitable_external_models(&body);
                if !discovered.is_empty() {
                    break;
                }
            }
            Ok(_) => {}
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(TestCancelled);
                }
                last_error = Some(ExternalLlmServer::failure_reason(e));
            }
        }
    }
    if !reachable {
        let reason = single_line(&last_error.unwrap_or_else(|| "timeout".to_owned()), "");
        return Ok(ProviderTestOutcome::failure(sa_core::l_args("Sunucuya ulaşılamadı: %@", &[Arg::Str(reason)]), Vec::new()));
    }
    let ms = (started.elapsed().as_millis() as i64).max(1);
    let message = sa_core::l_args(
        "Bağlantı başarılı (%@ · %lld ms · %lld uygun model bulundu)",
        &[Arg::Str(host_label), Arg::Int(ms), Arg::Int(discovered.len() as i64)],
    );
    Ok(ProviderTestOutcome { success: true, message, models: discovered })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        assert_eq!(candidate_model_endpoints("http://127.0.0.1:11434"), vec![
            "http://127.0.0.1:11434/v1/models",
            "http://127.0.0.1:11434/models",
            "http://127.0.0.1:11434/api/tags"
        ]);
        assert_eq!(chat_completions_endpoint("http://127.0.0.1:11434"), "http://127.0.0.1:11434/v1/chat/completions");
        assert_eq!(candidate_model_endpoints("https://openrouter.ai/api/v1"), vec!["https://openrouter.ai/api/v1/models"]);
        assert_eq!(chat_completions_endpoint("https://openrouter.ai/api/v1/"), "https://openrouter.ai/api/v1/chat/completions");
        assert_eq!(
            candidate_model_endpoints("https://generativelanguage.googleapis.com/v1beta/openai"),
            vec!["https://generativelanguage.googleapis.com/v1beta/openai/models"]
        );
        assert_eq!(candidate_model_endpoints("https://x.test/v1/chat/completions"), vec!["https://x.test/v1/models"]);
        assert_eq!(candidate_model_endpoints("https://x.test/v1beta/models"), vec!["https://x.test/v1beta/models"]);
        assert_eq!(chat_completions_endpoint("http://h:1/api/chat"), "http://h:1/api/chat");
        for saved in ["https://opencode.ai/zen/go/v1", "https://opencode.ai/zen/go/v1/chat/completions", "https://opencode.ai/inference/go/openai/v1"] {
            assert_eq!(candidate_model_endpoints(saved), vec![OPENCODE_GO_MODELS_URL]);
            assert_eq!(chat_completions_endpoint(saved), OPENCODE_GO_CHAT_URL);
        }
    }

    #[test]
    fn opencode_detection() {
        assert!(is_opencode_go_url("https://OpenCode.ai/inference/go/openai/v1/"));
        assert!(is_opencode_go_url("https://opencode.ai:443/zen/go/v1"));
        for not in [
            "http://opencode.ai/zen/go/v1",
            "https://opencode.ai/v1",
            "https://opencode.ai/zen/go/v1?x=1",
            "https://opencode.ai/zen/go/v1#top",
            "https://opencode.ai:8443/zen/go/v1",
            "https://proxy.example.com/zen/go/v1",
            "https://opencode.ai/inference/go/v1/chat/completions",
            "https://opencode.ai/inference/go/openai/v1/models",
            "not a url",
            "",
        ] {
            assert!(!is_opencode_go_url(not), "{not}");
        }
    }

    #[test]
    fn provider_error_text() {
        let m = read_provider_error(400, r#"{"error":{"message":"Global region required (sk-private)"}}"#, "sk-private");
        assert_eq!(m, "HTTP 400: Global region required ([redacted])");
        assert_eq!(read_provider_error(402, r#"{"error":"Out of credits"}"#, ""), "HTTP 402: Out of credits");
        assert_eq!(read_provider_error(500, r#"{"message":"boom"}"#, ""), "HTTP 500: boom");
        assert_eq!(read_provider_error(502, "<html>bad</html>", ""), "HTTP 502");
        assert_eq!(read_provider_error(429, "{\"error\":{\"message\":\"a\\r\\nb\"}}", ""), "HTTP 429: a  b");
        let long = format!("{{\"message\":\"{}\"}}", "x".repeat(3000));
        assert_eq!(read_provider_error(500, &long, "").chars().count(), 1000);
    }

    #[test]
    fn merge_and_parse() {
        let d = vec!["b".to_owned(), "B".to_owned(), " ".to_owned()];
        let s = vec!["c".to_owned(), "b".to_owned()];
        assert_eq!(merge_models(&d, &s, Some(" x ")), vec!["x", "b", "c"]);
        assert_eq!(merge_models(&d, &s, Some("C")), vec!["b", "c"]);
        let ollama = r#"{"models":[{"name":"gemma4:e4b"},{"name":"nomic-embed-text:latest"},{"name":"qwen3:14b"}]}"#;
        assert_eq!(parse_suitable_external_models(ollama), vec!["gemma4:e4b", "qwen3:14b"]);
        let openai = r#"{"object":"list","data":[{"id":"qwen/qwen3-8b"},{"id":"all-MiniLM-L6"},{"id":"bge-m3"},{"id":" QWEN/qwen3-8b "},{"name":"named"},{"id":""}]}"#;
        assert_eq!(parse_suitable_external_models(openai), vec!["qwen/qwen3-8b", "named"]);
        assert!(parse_suitable_external_models("[]").is_empty());
        assert!(parse_suitable_external_models("nope").is_empty());
    }

    #[test]
    fn lan_address_normalization() {
        assert_eq!(normalize_external_base_url("http://mac-studio.local:11434/api/chat").as_deref(), Some("http://mac-studio.local:11434"));
        assert_eq!(normalize_external_base_url("192.168.1.50:1234/v1/chat/completions").as_deref(), Some("http://192.168.1.50:1234"));
        assert_eq!(normalize_external_base_url("  http://h:1/v1/ ").as_deref(), Some("http://h:1"));
        assert_eq!(normalize_external_base_url("   "), None);
        // C# does not validate the host: these reach the request stage and fail there.
        assert_eq!(normalize_external_base_url("http://").as_deref(), Some("http://http:"));
        assert_eq!(normalize_external_base_url("/v1").as_deref(), Some("http:"));
    }

    #[test]
    fn dotnet_uri_rules_of_the_opencode_check() {
        assert!(is_opencode_go_url("https://u:p@opencode.ai/zen/go/v1"), "Authority leaves out user info");
        assert!(is_opencode_go_url("https://opencode.ai:/zen/go/v1"), "an empty port is the default port");
        assert!(is_opencode_go_url("https://opencode.ai/zen/go/v1/../v1"));
        assert!(is_opencode_go_url("https://opencode.ai/zen/%67o/v1"));
        assert!(is_opencode_go_url(r"https://opencode.ai/zen\go\v1"), "backslashes in the path are slashes");
        assert!(!is_opencode_go_url(r"https://opencode.ai\zen\go\v1"), "but not right after the host");
        assert!(is_opencode_go_url("https://opencode.ai/zen/go/v1/x/.%2e"));
        assert!(!is_opencode_go_url("https://opencode.ai/zen/go/v1/x/..."));
        assert!(is_opencode_go_url("https://opencode.ai/zen/go/v1 "), ".NET trims surrounding whitespace");
        assert!(!is_opencode_go_url("https://opencode.ai/zen/go/v1?"), "a bare ? is a query");
        assert!(!is_opencode_go_url("https://opencode.ai/zen/go/v1#"));
        assert_eq!(dotnet_absolute_path(""), "/");
        assert_eq!(dotnet_absolute_path("/a/./b/../c/."), "/a/c/");
        assert_eq!(dotnet_uri_host("https://API.OpenAI.com/v1").as_deref(), Some("api.openai.com"));
        assert_eq!(dotnet_uri_host("http://[0:0::1]:8080").as_deref(), Some("[::1]"));
        assert_eq!(dotnet_uri_host("http://127.1").as_deref(), Some("127.0.0.1"));
        assert_eq!(dotnet_uri_host("http://ŞİRKET.test/v1").as_deref(), Some("şİrket.test"));
        assert_eq!(dotnet_uri_host("http://École").as_deref(), Some("École"));
        for invalid in ["http://", "http://h:65536", "http://h..x", "http://%41", "http://a@b@h", r"http://h\x", "ftp://h"] {
            assert_eq!(dotnet_uri_host(invalid), None, "{invalid}");
        }
        assert_eq!(take_utf16_units("ab😀c", 3), "ab");
        assert_eq!(take_utf16_units("ab😀c", 4), "ab😀");
    }

    #[test]
    fn cloud_urls() {
        assert!(is_cloud_provider_url("https://openrouter.ai/api/v1"));
        assert!(is_cloud_provider_url(" HTTPS://API.OPENAI.COM/v1/chat/completions"));
        assert!(is_cloud_provider_url("https://opencode.ai/zen/go/v1"));
        assert!(is_cloud_provider_url("api.groq.com/openai/v1"));
        assert!(!is_cloud_provider_url("http://mac-studio.local:11434"));
        assert!(!is_cloud_provider_url("http://127.0.0.1:11434"));
        assert!(!is_cloud_provider_url("  "));
    }

    /// Blocks until its token is cancelled (a server that never answers).
    struct Hanging;

    impl HttpFetch for Hanging {
        fn fetch(&self, _request: HttpRequest, cancel: &CancellationToken) -> Result<(u16, String), Option<String>> {
            while !cancel.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(Some(String::new()))
        }
    }

    #[test]
    fn the_deadline_turns_a_hanging_request_into_a_timeout_and_keeps_cancellation_apart() {
        let fetch = DeadlineFetch { inner: &Hanging, timeout: std::time::Duration::from_millis(60) };
        let request = || HttpRequest { method: "GET", url: "http://h:1/api/tags".into(), headers: Vec::new(), body: None };
        let started = std::time::Instant::now();
        assert_eq!(fetch.fetch(request(), &CancellationToken::new()), Err(None));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(fetch.fetch(request(), &cancel), Err(Some(String::new())));
        assert_eq!(LAN_TEST_TIMEOUT, std::time::Duration::from_millis(3500));
    }
}
