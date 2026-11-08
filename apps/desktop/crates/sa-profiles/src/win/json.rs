//! C#-compatible JSON shapes of the Windows profile store: the `profiles.json` v3 header, the encrypted SQLite row
//! payloads (`VideoRowDto`, `WatchProgressRowDto`, `CustomListHeaderDto`), the sealed broker payload and the legacy
//! monolithic vault. Dates are .NET UTC ticks in memory and System.Text.Json ISO 8601 text on disk.

use base64::Engine as _;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use crate::Ticks;

// ---------------------------------------------------------------------------------------------------------------
// DateTimeOffset

const TICKS_PER_SECOND: i64 = 10_000_000;
const TICKS_PER_DAY: i64 = 86_400 * TICKS_PER_SECOND;

fn split_ticks(ticks: i64) -> (chrono::NaiveDate, i64) {
    let t = ticks.max(0);
    let days = t / TICKS_PER_DAY;
    let rem = t % TICKS_PER_DAY;
    let date = chrono::NaiveDate::from_num_days_from_ce_opt(days as i32 + 1).unwrap_or(chrono::NaiveDate::MIN);
    (date, rem)
}

fn format_ticks(ticks: i64, trim_fraction: bool) -> String {
    use chrono::Datelike;
    let (date, rem) = split_ticks(ticks);
    let secs = rem / TICKS_PER_SECOND;
    let frac = rem % TICKS_PER_SECOND;
    let base = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        date.year(),
        date.month(),
        date.day(),
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    );
    let frac_text = if trim_fraction {
        if frac == 0 {
            String::new()
        } else {
            let digits = format!("{frac:07}");
            format!(".{}", digits.trim_end_matches('0'))
        }
    } else {
        format!(".{frac:07}")
    };
    format!("{base}{frac_text}+00:00")
}

/// System.Text.Json `DateTimeOffset` text (fraction trimmed, `+00:00`).
pub fn format_json_date(t: Ticks) -> String {
    format_ticks(t.0, true)
}

/// `DateTimeOffset.ToString("O")` (always 7 fraction digits, `+00:00`).
pub fn format_round_trip(t: Ticks) -> String {
    format_ticks(t.0, false)
}

/// Parses ISO 8601 date/time text the way System.Text.Json accepts it and converts it to UTC ticks.
pub fn parse_date(text: &str) -> Option<Ticks> {
    let s = text.trim();
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = s.get(from..from + len)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let date = chrono::NaiveDate::from_ymd_opt(y as i32, mo as u32, d as u32)?;
    let mut i = 10;
    let (mut h, mut mi, mut se, mut frac) = (0i64, 0i64, 0i64, 0i64);
    if i < b.len() && (b[i] == b'T' || b[i] == b't') {
        i += 1;
        h = num(i, 2)?;
        i += 2;
        if b.get(i) != Some(&b':') {
            return None;
        }
        mi = num(i + 1, 2)?;
        i += 3;
        if b.get(i) == Some(&b':') {
            se = num(i + 1, 2)?;
            i += 3;
            if b.get(i) == Some(&b'.') || b.get(i) == Some(&b',') {
                i += 1;
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                let mut digits = s[start..i].to_owned();
                digits.truncate(7);
                while digits.len() < 7 {
                    digits.push('0');
                }
                frac = digits.parse().ok()?;
            }
        }
        if h > 23 || mi > 59 || se > 59 {
            return None;
        }
    }
    let mut offset_minutes = 0i64;
    if i < b.len() {
        match b[i] {
            b'Z' | b'z' => i += 1,
            b'+' | b'-' => {
                let sign = if b[i] == b'-' { -1 } else { 1 };
                let oh = num(i + 1, 2)?;
                i += 3;
                let mut om = 0;
                if b.get(i) == Some(&b':') {
                    om = num(i + 1, 2)?;
                    i += 3;
                } else if i + 2 <= b.len() && b[i].is_ascii_digit() {
                    om = num(i, 2)?;
                    i += 2;
                }
                offset_minutes = sign * (oh * 60 + om);
            }
            _ => return None,
        }
    }
    if i != b.len() {
        return None;
    }
    let days = i64::from(date.num_days_from_ce() - 1);
    let local = days * TICKS_PER_DAY + ((h * 60 + mi) * 60 + se) * TICKS_PER_SECOND + frac;
    Some(Ticks(local - offset_minutes * 60 * TICKS_PER_SECOND))
}

use chrono::Datelike as _;

fn de_date<'de, D: Deserializer<'de>>(d: D) -> Result<Ticks, D::Error> {
    let s = String::deserialize(d)?;
    parse_date(&s).ok_or_else(|| serde::de::Error::custom("invalid DateTimeOffset"))
}

fn de_opt_date<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Ticks>, D::Error> {
    match Option::<String>::deserialize(d)? {
        None => Ok(None),
        Some(s) => parse_date(&s).map(Some).ok_or_else(|| serde::de::Error::custom("invalid DateTimeOffset")),
    }
}

fn some_empty() -> Option<String> {
    Some(String::new())
}

fn default_iterations() -> i32 {
    crate::crypto::PBKDF2_ITERATIONS as i32
}

fn default_version() -> i32 {
    3
}

fn some_empty_vec<T>() -> Option<Vec<T>> {
    Some(Vec::new())
}

// ---------------------------------------------------------------------------------------------------------------
// base64 (.NET Convert)

/// `Convert.FromBase64String`: whitespace ignored, canonical padding required.
pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    const ENGINE: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
    );
    let compact: String = text.chars().filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n')).collect();
    ENGINE.decode(compact).ok()
}

/// `Convert.ToBase64String`.
pub fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `TryDecodeBase64`: blank or invalid → `None`.
pub fn try_b64(text: Option<&str>) -> Option<Vec<u8>> {
    let t = text?;
    if t.trim().is_empty() {
        return None;
    }
    b64_decode(t)
}

// ---------------------------------------------------------------------------------------------------------------
// profiles.json header

/// `ProfilesDocument` as read (case-sensitive camelCase; unknown keys ignored).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderDocIn {
    #[serde(default = "default_version")]
    #[allow(dead_code)]
    pub version: i32,
    #[serde(default)]
    pub remember_me: bool,
    #[serde(default)]
    pub remember_me_opt_in: Option<bool>,
    #[serde(default)]
    pub last_username: Option<String>,
    #[serde(default)]
    pub remembered_username: Option<String>,
    #[serde(default)]
    pub remembered_protected_key_base64: Option<String>,
    /// Missing → `[]`; `null` → invalid document.
    #[serde(default = "some_empty_vec")]
    pub profiles: Option<Vec<ProfileDtoIn>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDtoIn {
    #[serde(default = "some_empty")]
    pub username: Option<String>,
    #[serde(default = "some_empty")]
    pub salt_base64: Option<String>,
    #[serde(default = "some_empty")]
    pub password_hash_base64: Option<String>,
    #[serde(default = "default_iterations")]
    pub iterations: i32,
    #[serde(default, deserialize_with = "de_date")]
    pub created_at_utc: Ticks,
    #[serde(default)]
    pub encrypted_vault_base64: Option<String>,
}

/// One profile as written.
pub struct ProfileDtoOut<'a> {
    pub username: &'a str,
    pub salt_base64: &'a str,
    pub password_hash_base64: &'a str,
    pub iterations: i32,
    pub created_at_utc: Ticks,
    pub encrypted_vault_base64: Option<&'a str>,
}

/// The header as written by `TryWriteAuthHeaderLocked`: STJ `WriteIndented` (2 spaces, CRLF), nulls omitted.
pub struct HeaderDocOut<'a> {
    pub remember_me: bool,
    pub remember_me_opt_in: Option<bool>,
    pub last_username: Option<&'a str>,
    pub remembered_username: Option<&'a str>,
    pub remembered_protected_key_base64: Option<&'a str>,
    pub profiles: Vec<ProfileDtoOut<'a>>,
}

impl HeaderDocOut<'_> {
    pub fn to_json(&self) -> String {
        let mut m = Map::new();
        m.insert("version".into(), json!(3));
        m.insert("rememberMe".into(), json!(self.remember_me));
        if let Some(v) = self.remember_me_opt_in {
            m.insert("rememberMeOptIn".into(), json!(v));
        }
        if let Some(v) = self.last_username {
            m.insert("lastUsername".into(), json!(v));
        }
        if let Some(v) = self.remembered_username {
            m.insert("rememberedUsername".into(), json!(v));
        }
        if let Some(v) = self.remembered_protected_key_base64 {
            m.insert("rememberedProtectedKeyBase64".into(), json!(v));
        }
        let profiles: Vec<Value> = self
            .profiles
            .iter()
            .map(|p| {
                let mut o = Map::new();
                o.insert("username".into(), json!(p.username));
                o.insert("saltBase64".into(), json!(p.salt_base64));
                o.insert("passwordHashBase64".into(), json!(p.password_hash_base64));
                o.insert("iterations".into(), json!(p.iterations));
                o.insert("createdAtUtc".into(), json!(format_json_date(p.created_at_utc)));
                if let Some(v) = p.encrypted_vault_base64 {
                    o.insert("encryptedVaultBase64".into(), json!(v));
                }
                Value::Object(o)
            })
            .collect();
        m.insert("profiles".into(), Value::Array(profiles));
        let text = serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_else(|_| "{}".into());
        // String contents never hold a raw newline (JSON escapes them), so this only changes line breaks.
        text.replace('\n', "\r\n")
    }
}

/// `StripRememberedSession`: removes the remembered-session members, re-serialized indented.
pub fn strip_remembered_session(header_json: &str) -> String {
    let Ok(Value::Object(mut node)) = serde_json::from_str::<Value>(header_json) else {
        return header_json.to_owned();
    };
    let mut changed = false;
    for name in ["rememberMe", "rememberMeOptIn", "rememberedUsername", "rememberedProtectedKeyBase64"] {
        changed |= node.shift_remove(name).is_some();
    }
    if !changed {
        return header_json.to_owned();
    }
    serde_json::to_string_pretty(&Value::Object(node)).map(|t| t.replace('\n', "\r\n")).unwrap_or_else(|_| header_json.to_owned())
}

// ---------------------------------------------------------------------------------------------------------------
// Row payloads

/// `VideoRowDto` (read side).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoRowIn {
    #[serde(default)]
    pub content_fingerprint: Option<String>,
    #[serde(default = "some_empty")]
    pub video_path: Option<String>,
    #[serde(default = "some_empty")]
    pub file_name: Option<String>,
    /// Missing → `""` (kept); `null` → recomputed.
    #[serde(default = "some_empty")]
    pub directory_path: Option<String>,
    #[serde(default)]
    pub view_count: i32,
    #[serde(default)]
    pub is_liked: bool,
    #[serde(default)]
    pub rating: i32,
    #[serde(default, deserialize_with = "de_opt_date")]
    pub last_watched_at_utc: Option<Ticks>,
    #[serde(default, deserialize_with = "de_date")]
    pub updated_at_utc: Ticks,
}

/// `WatchProgressRowDto`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressRowIn {
    #[serde(default)]
    pub last_position_seconds: f64,
    #[serde(default)]
    pub duration_seconds: f64,
    #[serde(default, deserialize_with = "de_date")]
    pub updated_at_utc: Ticks,
}

/// `CustomListHeaderDto`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListHeaderIn {
    #[serde(default = "some_empty")]
    pub id: Option<String>,
    #[serde(default = "some_empty")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "de_date")]
    pub created_at_utc: Ticks,
    #[serde(default, deserialize_with = "de_date")]
    pub updated_at_utc: Ticks,
}

/// `SealedWatchPayloadDto`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedPayloadIn {
    #[serde(default = "some_empty")]
    pub video_path: Option<String>,
    #[serde(default)]
    pub position_seconds: Option<f64>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub thumbnail_base64: Option<String>,
}

/// A JSON number for a `double` (finite; non-finite values become 0 so the C# reader never sees `null`).
fn num(v: f64) -> Value {
    if v.is_finite() {
        if v.fract() == 0.0 && v.abs() < 1e15 {
            // STJ writes integral doubles without a fraction.
            json!(v as i64)
        } else {
            json!(v)
        }
    } else {
        json!(0)
    }
}

pub fn video_row_json(s: &crate::VideoUserStats) -> Vec<u8> {
    let mut m = Map::new();
    if let Some(fp) = &s.content_fingerprint {
        m.insert("contentFingerprint".into(), json!(fp));
    }
    m.insert("videoPath".into(), json!(s.video_path));
    m.insert("fileName".into(), json!(s.file_name));
    m.insert("directoryPath".into(), json!(s.directory_path));
    m.insert("viewCount".into(), json!(clamp_i32(s.view_count)));
    m.insert("isLiked".into(), json!(s.is_liked));
    m.insert("rating".into(), json!(clamp_i32(s.rating)));
    if let Some(t) = s.last_watched_at {
        m.insert("lastWatchedAtUtc".into(), json!(format_json_date(t)));
    }
    m.insert("updatedAtUtc".into(), json!(format_json_date(s.updated_at)));
    serde_json::to_vec(&Value::Object(m)).unwrap_or_default()
}

pub fn progress_row_json(s: &crate::VideoUserStats) -> Vec<u8> {
    let mut m = Map::new();
    m.insert("lastPositionSeconds".into(), num(s.last_position_seconds));
    m.insert("durationSeconds".into(), num(s.duration_seconds));
    m.insert("updatedAtUtc".into(), json!(format_json_date(s.updated_at)));
    serde_json::to_vec(&Value::Object(m)).unwrap_or_default()
}

pub fn list_header_json(l: &crate::UserVideoList) -> Vec<u8> {
    let mut m = Map::new();
    m.insert("id".into(), json!(l.id));
    m.insert("name".into(), json!(l.name));
    m.insert("createdAtUtc".into(), json!(format_json_date(l.created_at)));
    m.insert("updatedAtUtc".into(), json!(format_json_date(l.updated_at)));
    serde_json::to_vec(&Value::Object(m)).unwrap_or_default()
}

pub fn sealed_payload_json(video_path: &str, position: Option<f64>, duration: Option<f64>, thumbnail_b64: Option<&str>) -> Vec<u8> {
    let mut m = Map::new();
    m.insert("videoPath".into(), json!(video_path));
    if let Some(p) = position {
        m.insert("positionSeconds".into(), num(p));
    }
    if let Some(d) = duration {
        m.insert("durationSeconds".into(), num(d));
    }
    if let Some(t) = thumbnail_b64 {
        m.insert("thumbnailBase64".into(), json!(t));
    }
    serde_json::to_vec(&Value::Object(m)).unwrap_or_default()
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

// ---------------------------------------------------------------------------------------------------------------
// Legacy monolithic vault (profiles.json v2 `encryptedVaultBase64`)

/// One `VideoUserStats` of a legacy vault. `None` strings are JSON `null` (the C# reader keeps them as null).
#[derive(Debug, Clone)]
pub struct LegacyVideo {
    pub content_fingerprint: Option<String>,
    pub video_path: Option<String>,
    pub file_name: Option<String>,
    pub directory_path: Option<String>,
    pub view_count: i32,
    pub is_liked: bool,
    pub rating: i32,
    pub last_position_seconds: f64,
    pub duration_seconds: f64,
    pub last_watched_at: Option<Ticks>,
    pub updated_at: Ticks,
}

#[derive(Debug, Clone)]
pub struct LegacyList {
    pub id: Option<String>,
    pub name: Option<String>,
    pub video_paths: Option<Vec<Option<String>>>,
    pub created_at: Ticks,
    pub updated_at: Ticks,
}

#[derive(Debug, Clone, Default)]
pub struct LegacyVault {
    pub videos: Option<Vec<LegacyVideo>>,
    pub custom_lists: Option<Vec<LegacyList>>,
    /// Case-insensitive keys (last duplicate wins), values may be `null`.
    pub thumbnails: Option<Vec<(String, Option<String>)>>,
}

fn opt_string(v: Option<&Value>) -> Result<Option<String>, ()> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(()),
    }
}

fn req_string(o: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    if !o.contains_key(key) {
        return Err(()); // C# `required` member missing
    }
    opt_string(o.get(key))
}

fn int_field(o: &Map<String, Value>, key: &str) -> Result<i32, ()> {
    match o.get(key) {
        None => Ok(0),
        Some(v) => v.as_i64().and_then(|n| i32::try_from(n).ok()).ok_or(()),
    }
}

fn f64_field(o: &Map<String, Value>, key: &str) -> Result<f64, ()> {
    match o.get(key) {
        None => Ok(0.0),
        Some(v) => v.as_f64().ok_or(()),
    }
}

fn bool_field(o: &Map<String, Value>, key: &str) -> Result<bool, ()> {
    match o.get(key) {
        None => Ok(false),
        Some(v) => v.as_bool().ok_or(()),
    }
}

fn date_field(o: &Map<String, Value>, key: &str) -> Result<Ticks, ()> {
    match o.get(key) {
        None => Ok(Ticks(0)),
        Some(Value::String(s)) => parse_date(s).ok_or(()),
        Some(_) => Err(()),
    }
}

fn opt_date_field(o: &Map<String, Value>, key: &str) -> Result<Option<Ticks>, ()> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => parse_date(s).map(Some).ok_or(()),
        Some(_) => Err(()),
    }
}

/// Parses the plaintext of a legacy vault (`ProfileSecretVault`, camelCase, case-sensitive, `required` members
/// enforced). Any violation fails the whole vault, like System.Text.Json.
pub fn parse_legacy_vault(plain: &[u8]) -> Option<LegacyVault> {
    let root: Value = serde_json::from_slice(plain).ok()?;
    let obj = root.as_object()?;
    let mut vault = LegacyVault { videos: Some(Vec::new()), custom_lists: Some(Vec::new()), thumbnails: Some(Vec::new()) };
    match obj.get("videos") {
        None => {}
        Some(Value::Null) => vault.videos = None,
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let o = item.as_object()?;
                out.push(LegacyVideo {
                    content_fingerprint: opt_string(o.get("contentFingerprint")).ok()?,
                    video_path: req_string(o, "videoPath").ok()?,
                    file_name: req_string(o, "fileName").ok()?,
                    directory_path: req_string(o, "directoryPath").ok()?,
                    view_count: int_field(o, "viewCount").ok()?,
                    is_liked: bool_field(o, "isLiked").ok()?,
                    rating: int_field(o, "rating").ok()?,
                    last_position_seconds: f64_field(o, "lastPositionSeconds").ok()?,
                    duration_seconds: f64_field(o, "durationSeconds").ok()?,
                    last_watched_at: opt_date_field(o, "lastWatchedAtUtc").ok()?,
                    updated_at: date_field(o, "updatedAtUtc").ok()?,
                });
            }
            vault.videos = Some(out);
        }
        Some(_) => return None,
    }
    match obj.get("customLists") {
        None => {}
        Some(Value::Null) => vault.custom_lists = None,
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let o = item.as_object()?;
                if !o.contains_key("videoPaths") {
                    return None;
                }
                let paths = match o.get("videoPaths") {
                    Some(Value::Null) => None,
                    Some(Value::Array(ps)) => {
                        let mut v = Vec::with_capacity(ps.len());
                        for p in ps {
                            v.push(opt_string(Some(p)).ok()?);
                        }
                        Some(v)
                    }
                    _ => return None,
                };
                out.push(LegacyList {
                    id: req_string(o, "id").ok()?,
                    name: req_string(o, "name").ok()?,
                    video_paths: paths,
                    created_at: date_field(o, "createdAtUtc").ok()?,
                    updated_at: date_field(o, "updatedAtUtc").ok()?,
                });
            }
            vault.custom_lists = Some(out);
        }
        Some(_) => return None,
    }
    match obj.get("thumbnails") {
        None => {}
        Some(Value::Null) => vault.thumbnails = None,
        Some(Value::Object(map)) => {
            let mut out: Vec<(String, Option<String>)> = Vec::with_capacity(map.len());
            for (k, v) in map {
                let value = opt_string(Some(v)).ok()?;
                if let Some(slot) = out.iter_mut().find(|(ek, _)| super::netcompat::eq_ci(ek, k)) {
                    slot.1 = value;
                } else {
                    out.push((k.clone(), value));
                }
            }
            vault.thumbnails = Some(out);
        }
        Some(_) => return None,
    }
    Some(vault)
}

/// Serializes a legacy vault the way the old C# writer did (used by fixtures and tests).
pub fn legacy_vault_json(vault: &LegacyVault) -> Vec<u8> {
    let mut m = Map::new();
    let videos: Vec<Value> = vault
        .videos
        .iter()
        .flatten()
        .map(|v| {
            let mut o = Map::new();
            if let Some(fp) = &v.content_fingerprint {
                o.insert("contentFingerprint".into(), json!(fp));
            }
            o.insert("videoPath".into(), json!(v.video_path));
            o.insert("fileName".into(), json!(v.file_name));
            o.insert("directoryPath".into(), json!(v.directory_path));
            o.insert("viewCount".into(), json!(v.view_count));
            o.insert("isLiked".into(), json!(v.is_liked));
            o.insert("rating".into(), json!(v.rating));
            o.insert("lastPositionSeconds".into(), num(v.last_position_seconds));
            o.insert("durationSeconds".into(), num(v.duration_seconds));
            if let Some(t) = v.last_watched_at {
                o.insert("lastWatchedAtUtc".into(), json!(format_json_date(t)));
            }
            o.insert("updatedAtUtc".into(), json!(format_json_date(v.updated_at)));
            Value::Object(o)
        })
        .collect();
    m.insert("videos".into(), Value::Array(videos));
    let lists: Vec<Value> = vault
        .custom_lists
        .iter()
        .flatten()
        .map(|l| {
            json!({
                "id": l.id, "name": l.name, "videoPaths": l.video_paths,
                "createdAtUtc": format_json_date(l.created_at), "updatedAtUtc": format_json_date(l.updated_at)
            })
        })
        .collect();
    m.insert("customLists".into(), Value::Array(lists));
    let mut thumbs = Map::new();
    for (k, v) in vault.thumbnails.iter().flatten() {
        thumbs.insert(k.clone(), json!(v));
    }
    m.insert("thumbnails".into(), Value::Object(thumbs));
    serde_json::to_vec(&Value::Object(m)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip_in_stj_and_o_forms() {
        let t = parse_date("2026-11-02T17:06:17.5945068+00:00").unwrap();
        assert_eq!(format_json_date(t), "2026-11-02T17:06:17.5945068+00:00");
        assert_eq!(format_round_trip(t), "2026-11-02T17:06:17.5945068+00:00");
        let whole = parse_date("2026-11-02T17:06:17+00:00").unwrap();
        assert_eq!(format_json_date(whole), "2026-11-02T17:06:17+00:00");
        assert_eq!(format_round_trip(whole), "2026-11-02T17:06:17.0000000+00:00");
        let half = parse_date("2026-11-02T17:06:17.5Z").unwrap();
        assert_eq!(format_json_date(half), "2026-11-02T17:06:17.5+00:00");
        assert_eq!(parse_date("2026-11-02T20:06:17.5+03:00"), Some(half));
        assert_eq!(parse_date("0001-01-01T00:00:00+00:00"), Some(Ticks(0)));
        assert_eq!(parse_date("1970-01-01T00:00:00Z"), Some(Ticks(Ticks::UNIX_EPOCH)));
        assert!(parse_date("garbage").is_none());
    }

    #[test]
    fn header_is_indented_with_crlf_and_omits_nulls() {
        let doc = HeaderDocOut {
            remember_me: false,
            remember_me_opt_in: None,
            last_username: Some("emre"),
            remembered_username: None,
            remembered_protected_key_base64: None,
            profiles: vec![],
        };
        let text = doc.to_json();
        assert_eq!(text, "{\r\n  \"version\": 3,\r\n  \"rememberMe\": false,\r\n  \"lastUsername\": \"emre\",\r\n  \"profiles\": []\r\n}");
    }

    #[test]
    fn legacy_vault_requires_members() {
        assert!(parse_legacy_vault(br#"{"videos":[{"videoPath":"a","fileName":"a"}]}"#).is_none());
        let ok = parse_legacy_vault(br#"{"videos":[{"videoPath":"a","fileName":"a","directoryPath":null,"viewCount":2}],"thumbnails":{"K":"AA==","k":"AQ=="}}"#).unwrap();
        assert_eq!(ok.videos.unwrap()[0].view_count, 2);
        assert_eq!(ok.thumbnails.unwrap().len(), 1);
        assert!(parse_legacy_vault(br#"{"videos":[{"videoPath":"a","fileName":"a","directoryPath":"","viewCount":2.5}]}"#).is_none());
    }

    #[test]
    fn base64_tolerates_whitespace() {
        assert_eq!(b64_decode("AQID\r\nBA=="), Some(vec![1, 2, 3, 4]));
        assert_eq!(b64_decode("AQI"), None);
    }
}
