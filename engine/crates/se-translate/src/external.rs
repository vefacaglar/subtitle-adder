//! External OpenAI-compatible server configuration and safety rules (spec translate-llm.md §5, §6).

use std::net::{IpAddr, Ipv4Addr};

use se_core::{EngineError, Result, pyfmt, t};

use crate::pyurl;

pub const EXTERNAL_LLM_URL_ENV_VAR: &str = "SUBTITLE_ENGINE_EXTERNAL_LLM_URL";
pub const EXTERNAL_LLM_MODEL_ENV_VAR: &str = "SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL";
pub const EXTERNAL_LLM_API_KEY_ENV_VAR: &str = "SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY";
/// OpenCode Go v2 chat base.
pub const OPENCODE_GO_BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
/// OpenCode Go v2 model discovery route.
pub const OPENCODE_GO_MODELS_URL: &str = "https://opencode.ai/inference/go/v1/models";

fn env_value(name: &str) -> Option<String> {
    let raw = std::env::var_os(name)?;
    let raw = se_core::paths::os_to_py_string(&raw);
    let v = pyfmt::strip(&raw);
    if v.is_empty() { None } else { Some(v.to_string()) }
}

/// `external_llm_url()`: the stripped env value, `None` when unset or blank.
pub fn external_llm_url() -> Option<String> {
    env_value(EXTERNAL_LLM_URL_ENV_VAR)
}

/// `external_llm_model()`: the stripped env value, `None` when unset or blank.
pub fn external_llm_model() -> Option<String> {
    env_value(EXTERNAL_LLM_MODEL_ENV_VAR)
}

/// `external_llm_api_key()`: the stripped env value, `None` when unset or blank.
pub fn external_llm_api_key() -> Option<String> {
    env_value(EXTERNAL_LLM_API_KEY_ENV_VAR)
}

/// `_is_opencode_go_url(url)`: https `opencode.ai` (exact netloc) with one of the known Go paths, no query or
/// fragment. A malformed URL is `false` (Python raised `ValueError`, unreachable for validated URLs).
pub fn is_opencode_go_url(url: &str) -> bool {
    let Ok(p) = pyurl::urlsplit(url) else { return false };
    p.query.is_empty()
        && p.fragment.is_empty()
        && p.scheme == "https"
        && p.netloc.to_lowercase() == "opencode.ai"
        && matches!(
            p.path.trim_end_matches('/'),
            "/zen/go/v1"
                | "/zen/go/v1/chat/completions"
                | "/inference/go/v1"
                | "/inference/go/openai/v1"
                | "/inference/go/openai/v1/chat/completions"
        )
}

/// `normalize_external_chat_url(raw)`: bare `host:port` → `http://host:port/v1/chat/completions`; a base
/// ending in `/v1` or `/openai` gets `/chat/completions`; OpenCode Go addresses → the v2 route. No host →
/// localized `ModelError` `external_llm_url_empty`. Uses Python `urlsplit` semantics (own parser).
pub fn normalize_external_chat_url(raw_url: &str) -> Result<String> {
    let mut url = pyfmt::strip(raw_url).trim_end_matches('/').to_string();
    let lower = url.to_lowercase();
    if lower.is_empty() || lower == "http:" || lower == "https:" {
        return Err(EngineError::model(t!("external_llm_url_empty")));
    }
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        url = format!("http://{url}");
    }
    if pyurl::url_host(&url).is_none() {
        return Err(EngineError::model(t!("external_llm_url_empty")));
    }
    if is_opencode_go_url(&url) {
        return Ok(format!("{OPENCODE_GO_BASE_URL}/chat/completions"));
    }
    let lower = url.to_lowercase();
    if lower.ends_with("/v1/chat/completions") || lower.ends_with("/chat/completions") || lower.ends_with("/api/chat") {
        return Ok(url);
    }
    if lower.ends_with("/v1") || lower.ends_with("/openai") {
        return Ok(format!("{url}/chat/completions"));
    }
    Ok(format!("{url}/v1/chat/completions"))
}

/// `_external_models_url(chat_url)`.
pub fn external_models_url(chat_url: &str) -> Option<String> {
    if is_opencode_go_url(chat_url) {
        return Some(OPENCODE_GO_MODELS_URL.to_string());
    }
    let lower = chat_url.to_lowercase();
    if lower.ends_with("/chat/completions") {
        return Some(format!("{}/models", &chat_url[..chat_url.len() - "/chat/completions".len()]));
    }
    if lower.ends_with("/api/chat") {
        return Some(format!("{}/v1/models", &chat_url[..chat_url.len() - "/api/chat".len()]));
    }
    None
}

/// BSD `inet_aton` legacy forms (`127.1`, `0x7f.1`, `2130706433`).
fn inet_aton(name: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut values = Vec::with_capacity(4);
    for part in &parts {
        if part.is_empty() {
            return None;
        }
        let (digits, radix) = if let Some(h) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
            (h, 16)
        } else if part.len() > 1 && part.starts_with('0') {
            (&part[1..], 8)
        } else {
            (*part, 10)
        };
        let v = if digits.is_empty() {
            // glibc/BSD read a bare "0x" as 0; Windows inet_addr (what CPython uses there) rejects it.
            if radix == 16 && !cfg!(windows) { 0 } else { return None }
        } else {
            u64::from_str_radix(digits, radix).ok()?
        };
        if v > u32::MAX as u64 {
            return None;
        }
        values.push(v as u32);
    }
    let n = values.len();
    let (head, last) = values.split_at(n - 1);
    let last = last[0];
    let mut addr: u32 = 0;
    for (i, &v) in head.iter().enumerate() {
        if v > 0xFF {
            return None;
        }
        addr |= v << (24 - 8 * i);
    }
    let max_last: u64 = match n {
        1 => u32::MAX as u64,
        2 => 0xFF_FFFF,
        3 => 0xFFFF,
        _ => 0xFF,
    };
    if last as u64 > max_last {
        return None;
    }
    addr |= last;
    // CPython on Windows has no inet_aton and uses inet_addr, whose INADDR_NONE result is only accepted for the
    // literal "255.255.255.255" (any other spelling of it raises OSError).
    if cfg!(windows) && addr == u32::MAX && name != "255.255.255.255" {
        return None;
    }
    Some(Ipv4Addr::from(addr))
}

/// `_parse_ip(name)`: strict `ipaddress.ip_address`, else (for `[0-9a-fx.]+`) the legacy `inet_aton` forms.
fn parse_ip(name: &str) -> Option<IpAddr> {
    if let Some(ip) = pyurl::ip_address(name) {
        return Some(ip);
    }
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c) || c == 'x' || c == '.') {
        return inet_aton(name).map(IpAddr::V4);
    }
    None
}

fn v4_local(a: Ipv4Addr) -> bool {
    let o = a.octets();
    o[0] == 127
        || o[0] == 10
        || (o[0] == 172 && (16..=31).contains(&o[1]))
        || (o[0] == 192 && o[1] == 168)
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        || (o[0] == 169 && o[1] == 254)
}

/// `is_local_network_host(host)`: loopback, RFC 1918, 100.64/10, link-local, IPv6 ULA/link-local, `.local`
/// and bare single-label names.
pub fn is_local_network_host(host: Option<&str>) -> bool {
    let h = pyfmt::strip(host.unwrap_or(""));
    let h = h.trim_matches(|c| c == '[' || c == ']');
    let h = h.split('%').next().unwrap_or("");
    let name = h.trim_end_matches('.').to_lowercase();
    if name.is_empty() {
        return false;
    }
    if let Some(ip) = parse_ip(&name) {
        return match ip {
            IpAddr::V4(a) => v4_local(a),
            IpAddr::V6(a) => {
                if let Some(v4) = a.to_ipv4_mapped() {
                    return v4_local(v4);
                }
                let s = a.segments();
                a.is_loopback() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80
            }
        };
    }
    if name == "localhost" || name.ends_with(".localhost") || name.ends_with(".local") {
        return true;
    }
    !name.contains('.')
}

/// `api_key_transport_allowed(url)`: https, or a local-network host (checked after `.local` resolution).
pub fn api_key_transport_allowed(url: &str) -> bool {
    let Ok(p) = pyurl::urlsplit(url) else { return false };
    let scheme = p.scheme.to_lowercase();
    if scheme == "https" {
        return true;
    }
    scheme == "http" && is_local_network_host(p.hostname().as_deref())
}

/// `check_external_llm(raw_url, api_key)`: normalizes the URL; strict offline + non-local host →
/// `offline_external_llm`; a key over plain http to a non-local host → `external_llm_key_insecure`.
/// Returns the normalized chat URL.
pub fn check_external_llm(raw_url: &str, api_key: Option<&str>) -> Result<String> {
    let chat_url = normalize_external_chat_url(raw_url)?;
    let host = pyurl::url_host(&chat_url);
    if se_core::offline::offline_requested() && !is_local_network_host(host.as_deref()) {
        let shown = host.unwrap_or_else(|| pyfmt::strip(raw_url).to_string());
        return Err(EngineError::model(t!("offline_external_llm", host = shown)));
    }
    if !pyfmt::strip(api_key.unwrap_or("")).is_empty() && !api_key_transport_allowed(&chat_url) {
        return Err(EngineError::model(t!("external_llm_key_insecure")));
    }
    Ok(chat_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inet_aton_forms() {
        assert_eq!(inet_aton("127.1"), Some(Ipv4Addr::new(127, 0, 0, 1)));
        assert_eq!(inet_aton("0x7f.1"), Some(Ipv4Addr::new(127, 0, 0, 1)));
        assert_eq!(inet_aton("2130706433"), Some(Ipv4Addr::new(127, 0, 0, 1)));
        assert_eq!(inet_aton("134744072"), Some(Ipv4Addr::new(8, 8, 8, 8)));
        assert_eq!(inet_aton("010.1.1.1"), Some(Ipv4Addr::new(8, 1, 1, 1)));
        assert_eq!(inet_aton("99999999999"), None);
        assert_eq!(inet_aton("1..2"), None);
        assert_eq!(inet_aton("cafe"), None);
        assert!(pyurl::ip_address("010.1.1.1").is_none());
    }

    #[test]
    fn models_url() {
        assert_eq!(external_models_url("http://h:1/v1/chat/completions").as_deref(), Some("http://h:1/v1/models"));
        assert_eq!(external_models_url("http://h:9/api/chat").as_deref(), Some("http://h:9/v1/models"));
        assert_eq!(external_models_url("http://h:9/x"), None);
    }
}
