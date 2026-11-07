//! Python 3.11 `urllib.parse` semantics the external-server rules depend on (spec translate-llm.md §5.3):
//! `urlsplit` (with `hostname` / `port`), `urlparse` / `urlunparse` (for `.local` rewrites), `urljoin` and the
//! redirect `quote`. The `url` crate normalizes too much (WHATWG) for these rules.

use std::net::{IpAddr, Ipv6Addr};

/// `urlsplit(url)` result.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SplitResult {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

const SCHEME_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-.";

fn is_c0_or_space(c: char) -> bool {
    (c as u32) <= 0x20
}

/// Python `ipaddress.ip_address(text)` (strict IPv4 quads, every IPv6 text form, scoped IPv6 `addr%zone`).
pub fn ip_address(text: &str) -> Option<IpAddr> {
    if let Ok(v4) = text.parse::<std::net::Ipv4Addr>() {
        return Some(IpAddr::V4(v4));
    }
    let (addr, scope) = match text.split_once('%') {
        Some((a, s)) => (a, Some(s)),
        None => (text, None),
    };
    if let Some(s) = scope {
        if s.is_empty() || s.contains('%') {
            return None;
        }
    }
    addr.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
}

fn check_bracketed_host(hostname: &str) -> Result<(), String> {
    if let Some(rest) = hostname.strip_prefix('v') {
        // \Av[a-fA-F0-9]+\..+\Z
        let hex_len = rest.chars().take_while(|c| c.is_ascii_hexdigit()).count();
        let ok = hex_len > 0 && rest[hex_len..].starts_with('.') && rest[hex_len + 1..].chars().any(|c| c != '\n')
            && !rest[hex_len + 1..].is_empty()
            && !rest[hex_len + 1..].contains('\n');
        if !ok {
            return Err("IPvFuture address is invalid".to_string());
        }
        return Ok(());
    }
    match ip_address(hostname) {
        Some(IpAddr::V6(_)) => Ok(()),
        Some(IpAddr::V4(_)) => Err("An IPv4 address cannot be in brackets".to_string()),
        None => Err(format!("{} does not appear to be an IPv4 or IPv6 address", se_core::pyfmt::repr_str(hostname))),
    }
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), String> {
    let hostname_and_port = netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(netloc);
    let hostname = if let Some((before, bracketed)) = hostname_and_port.split_once('[') {
        if !before.is_empty() {
            return Err("Invalid IPv6 URL".to_string());
        }
        let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
        if !port.is_empty() && !port.starts_with(':') {
            return Err("Invalid IPv6 URL".to_string());
        }
        hostname
    } else {
        hostname_and_port.split_once(':').map(|(h, _)| h).unwrap_or(hostname_and_port)
    };
    check_bracketed_host(hostname)
}

fn check_netloc(netloc: &str) -> Result<(), String> {
    if netloc.is_ascii() {
        return Ok(());
    }
    use unicode_normalization::UnicodeNormalization;
    let n: String = netloc.chars().filter(|c| !matches!(c, '@' | ':' | '#' | '?')).collect();
    let n2: String = n.nfkc().collect();
    if n == n2 {
        return Ok(());
    }
    if n2.chars().any(|c| "/?#@:".contains(c)) {
        return Err(format!("netloc '{netloc}' contains invalid characters under NFKC normalization"));
    }
    Ok(())
}

/// Python `urlsplit(url)`; `Err` carries the `ValueError` text.
pub fn urlsplit(url: &str) -> Result<SplitResult, String> {
    let url = url.trim_start_matches(is_c0_or_space);
    let mut url: String = url.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut out = SplitResult::default();
    if let Some(i) = url.find(':') {
        let first = url.chars().next().unwrap_or(' ');
        if i > 0 && first.is_ascii_alphabetic() && url[..i].chars().all(|c| SCHEME_CHARS.contains(c)) {
            out.scheme = url[..i].to_ascii_lowercase();
            url = url[i + 1..].to_string();
        }
    }
    if url.starts_with("//") {
        let rest = &url[2..];
        let delim = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        out.netloc = rest[..delim].to_string();
        url = rest[delim..].to_string();
        let open = out.netloc.contains('[');
        let close = out.netloc.contains(']');
        if open != close {
            return Err("Invalid IPv6 URL".to_string());
        }
        if open && close {
            check_bracketed_netloc(&out.netloc)?;
        }
    }
    if let Some((u, f)) = url.split_once('#') {
        out.fragment = f.to_string();
        url = u.to_string();
    }
    if let Some((u, q)) = url.split_once('?') {
        out.query = q.to_string();
        url = u.to_string();
    }
    check_netloc(&out.netloc)?;
    out.path = url;
    Ok(out)
}

impl SplitResult {
    fn hostinfo(&self) -> (String, Option<String>) {
        let hostinfo = self.netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(&self.netloc);
        let (hostname, port) = if let Some((_, bracketed)) = hostinfo.split_once('[') {
            let (h, p) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            let p = p.split_once(':').map(|(_, p)| p).unwrap_or("");
            (h, p)
        } else {
            hostinfo.split_once(':').unwrap_or((hostinfo, ""))
        };
        (hostname.to_string(), if port.is_empty() { None } else { Some(port.to_string()) })
    }

    /// `.hostname`: lowercased (the part before `%` only), `None` when empty.
    pub fn hostname(&self) -> Option<String> {
        let (h, _) = self.hostinfo();
        if h.is_empty() {
            return None;
        }
        Some(match h.split_once('%') {
            Some((a, zone)) => format!("{}%{}", a.to_lowercase(), zone),
            None => h.to_lowercase(),
        })
    }

    /// `.port`: `Ok(None)` when absent, `Err` (ValueError) when not ASCII digits or out of range.
    pub fn port(&self) -> Result<Option<u16>, String> {
        match self.hostinfo().1 {
            None => Ok(None),
            Some(p) => {
                if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
                    return Err(format!("Port could not be cast to integer value as {}", se_core::pyfmt::repr_str(&p)));
                }
                let trimmed = p.trim_start_matches('0');
                if trimmed.len() > 5 {
                    return Err("Port out of range 0-65535".to_string());
                }
                let v: u32 = if trimmed.is_empty() { 0 } else { trimmed.parse().unwrap_or(u32::MAX) };
                if v > 65535 {
                    return Err("Port out of range 0-65535".to_string());
                }
                Ok(Some(v as u16))
            }
        }
    }
}

/// `urlsplit(url).hostname`, `None` on `ValueError` (Python `_url_host`).
pub fn url_host(url: &str) -> Option<String> {
    urlsplit(url).ok().and_then(|s| s.hostname())
}

/// Python `urlunsplit((scheme, netloc, url, query, fragment))`.
pub fn urlunsplit(scheme: &str, netloc: &str, path: &str, query: &str, fragment: &str) -> String {
    const USES_NETLOC: &[&str] = &[
        "", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https", "shttp", "snews",
        "prospero", "rtsp", "rtspu", "rsync", "svn", "svn+ssh", "sftp", "nfs", "git", "git+ssh", "ws", "wss",
    ];
    let mut url = path.to_string();
    if !netloc.is_empty() || (!scheme.is_empty() && USES_NETLOC.contains(&scheme) && !url.starts_with("//")) {
        if !url.is_empty() && !url.starts_with('/') {
            url = format!("/{url}");
        }
        url = format!("//{netloc}{url}");
    }
    if !scheme.is_empty() {
        url = format!("{scheme}:{url}");
    }
    if !query.is_empty() {
        url = format!("{url}?{query}");
    }
    if !fragment.is_empty() {
        url = format!("{url}#{fragment}");
    }
    url
}

/// `urlparse` path/params split (`_splitparams`, only for schemes in `uses_params`).
fn split_params(scheme: &str, path: &str) -> (String, String) {
    const USES_PARAMS: &[&str] =
        &["", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtspu", "sip", "sips", "mms", "sftp", "tel"];
    if !USES_PARAMS.contains(&scheme) || !path.contains(';') {
        return (path.to_string(), String::new());
    }
    let i = if let Some(slash) = path.rfind('/') {
        match path[slash..].find(';') {
            Some(j) => slash + j,
            None => return (path.to_string(), String::new()),
        }
    } else {
        path.find(';').unwrap_or(path.len())
    };
    (path[..i].to_string(), path[i + 1..].to_string())
}

/// `urlunparse(urlparse(url)._replace(netloc=netloc))` (params are re-joined only when non-empty).
pub fn replace_netloc(parts: &SplitResult, netloc: &str) -> String {
    let (path, params) = split_params(&parts.scheme, &parts.path);
    let path = if params.is_empty() { path } else { format!("{path};{params}") };
    urlunsplit(&parts.scheme, netloc, &path, &parts.query, &parts.fragment)
}

/// The redirect-target fix-up of `HTTPRedirectHandler.http_error_302` before `urljoin`: empty path with a netloc
/// becomes `/`, then `quote(newurl, encoding="iso-8859-1", safe=string.punctuation)`.
pub fn fix_redirect_target(newurl: &str) -> Result<String, String> {
    let mut parts = urlsplit(newurl)?;
    let (path, params) = split_params(&parts.scheme, &parts.path);
    let mut path = path;
    if path.is_empty() && !parts.netloc.is_empty() {
        path = "/".to_string();
    }
    parts.path = if params.is_empty() { path } else { format!("{path};{params}") };
    let joined = urlunsplit(&parts.scheme, &parts.netloc, &parts.path, &parts.query, &parts.fragment);
    Ok(quote_punctuation_safe(&joined))
}

/// `quote(s, encoding="iso-8859-1", safe=string.punctuation)` (characters above U+00FF, which Python cannot
/// encode as ISO-8859-1, are percent-encoded as UTF-8 here).
pub fn quote_punctuation_safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c.is_ascii_punctuation() {
            out.push(c);
        } else if (c as u32) <= 0xFF {
            out.push_str(&format!("%{:02X}", c as u32));
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// Python `urljoin(base, url)` for the redirect case: an absolute target with a netloc is returned as is,
/// anything relative is resolved against `base` (RFC 3986, through the `url` crate).
pub fn urljoin(base: &str, target: &str) -> String {
    if target.is_empty() {
        return base.to_string();
    }
    if let Ok(t) = urlsplit(target) {
        let base_scheme = urlsplit(base).map(|b| b.scheme).unwrap_or_default();
        if !t.scheme.is_empty() && (t.scheme != base_scheme || !t.netloc.is_empty()) {
            return target.to_string();
        }
        if t.scheme.is_empty() && target.starts_with("//") {
            return format!("{base_scheme}:{target}");
        }
    }
    match url::Url::parse(base).and_then(|b| b.join(target)) {
        Ok(u) => u.to_string(),
        Err(_) => target.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlsplit_matches_python() {
        let s = urlsplit("HTTP://User@Mac-Studio.LOCAL:1234/v1/chat?x=1#f").unwrap();
        assert_eq!(s.scheme, "http");
        assert_eq!(s.netloc, "User@Mac-Studio.LOCAL:1234");
        assert_eq!(s.path, "/v1/chat");
        assert_eq!(s.query, "x=1");
        assert_eq!(s.fragment, "f");
        assert_eq!(s.hostname().as_deref(), Some("mac-studio.local"));
        assert_eq!(s.port(), Ok(Some(1234)));
        assert_eq!(urlsplit("http://[fd00::1]:8080/x").unwrap().hostname().as_deref(), Some("fd00::1"));
        assert!(urlsplit("http://[fd00::1/x").is_err());
        assert!(urlsplit("http://[1.2.3.4]/x").is_err());
        assert!(urlsplit("http://x[::1]/").is_err());
        assert_eq!(urlsplit("http://:1234").unwrap().hostname(), None);
        assert_eq!(urlsplit("not a url").unwrap().scheme, "");
        assert_eq!(urlsplit("http://ftp://h").unwrap().hostname().as_deref(), Some("ftp"));
        assert!(urlsplit("http://h:x").unwrap().port().is_err());
        assert!(urlsplit("http://h:70000").unwrap().port().is_err());
    }

    #[test]
    fn netloc_replacement_and_redirect_helpers() {
        let p = urlsplit("http://user@mac.local:1234/v1/chat/completions").unwrap();
        assert_eq!(replace_netloc(&p, "192.168.1.77:1234"), "http://192.168.1.77:1234/v1/chat/completions");
        assert_eq!(fix_redirect_target("http://h").unwrap(), "http://h/");
        assert_eq!(fix_redirect_target("/a b").unwrap(), "/a%20b");
        assert_eq!(urljoin("http://127.0.0.1:5/v1/models", "/v2/models"), "http://127.0.0.1:5/v2/models");
        assert_eq!(urljoin("http://a/v1/models", "https://b/x"), "https://b/x");
    }
}
