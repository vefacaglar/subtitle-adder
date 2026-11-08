//! `profiles.json` version 2 (mac-librarykit §5) and the vault plaintext (§6): lenient readers with Swift
//! `JSONDecoder` type rules, Apple-style writers.

use serde_json::{Map, Value, json};

use super::dates;
use super::json;
use super::paths::{ci, swift_cmp};
use crate::{Ticks, UserVideoList, VideoUserStats};

pub const DEFAULT_ITERATIONS: i64 = 100_000;
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// One entry of `profiles.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileDto {
    pub username: String,
    pub salt_base64: String,
    pub password_hash_base64: String,
    pub iterations: i64,
    pub created_at: Ticks,
    pub encrypted_vault_base64: Option<String>,
}

/// `profiles.json` as read or to be written.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfilesDocument {
    pub remember_me: bool,
    pub last_username: Option<String>,
    pub remembered_username: Option<String>,
    pub profiles: Vec<ProfileDto>,
}

/// What a `profiles.json` holds as far as this build is concerned.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedFile {
    Document(ProfilesDocument),
    /// `version > 2`: written by a newer or different app; never damage, never touched.
    Unsupported(i64),
    Damaged,
}

/// The epoch default of missing dates (1970-01-01).
pub const fn epoch() -> Ticks {
    Ticks(Ticks::UNIX_EPOCH)
}

fn strip_bom(data: &[u8]) -> &[u8] {
    data.strip_prefix(BOM).unwrap_or(data)
}

// ----- typed field readers (Swift `decodeIfPresent`: missing or null = absent, a wrong type is an error)

type Field<T> = Result<Option<T>, ()>;

fn present<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    obj.get(key).filter(|v| !v.is_null())
}

fn opt_str(obj: &Map<String, Value>, key: &str) -> Field<String> {
    match present(obj, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(()),
    }
}

fn opt_bool(obj: &Map<String, Value>, key: &str) -> Field<bool> {
    match present(obj, key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(()),
    }
}

fn as_int(v: &Value) -> Option<i64> {
    let n = v.as_number()?;
    if let Some(i) = n.as_i64() {
        return Some(i);
    }
    let x = n.as_f64()?;
    (x.is_finite() && x.fract() == 0.0 && x >= i64::MIN as f64 && x < i64::MAX as f64).then_some(x as i64)
}

fn opt_int(obj: &Map<String, Value>, key: &str) -> Field<i64> {
    match present(obj, key) {
        None => Ok(None),
        Some(v) => as_int(v).map(Some).ok_or(()),
    }
}

fn opt_f64(obj: &Map<String, Value>, key: &str) -> Field<f64> {
    match present(obj, key) {
        None => Ok(None),
        Some(Value::Number(n)) => n.as_f64().map(Some).ok_or(()),
        Some(_) => Err(()),
    }
}

fn opt_date(obj: &Map<String, Value>, key: &str) -> Field<Ticks> {
    match opt_str(obj, key)? {
        None => Ok(None),
        Some(text) => dates::parse(&text).map(Some).ok_or(()),
    }
}

// ----- profiles.json

/// §5.2: version header first (a newer format may not decode), then the lenient full decode.
pub fn parse_profiles(data: &[u8]) -> ParsedFile {
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(strip_bom(data)) else {
        return ParsedFile::Damaged;
    };
    let version = match opt_int(&root, "version") {
        Ok(v) => v,
        Err(()) => return ParsedFile::Damaged,
    };
    if let Some(v) = version
        && v > 2
    {
        return ParsedFile::Unsupported(v);
    }
    match decode_document(&root) {
        Ok(doc) => ParsedFile::Document(doc),
        Err(()) => ParsedFile::Damaged,
    }
}

fn decode_document(root: &Map<String, Value>) -> Result<ProfilesDocument, ()> {
    let remember_me = opt_bool(root, "rememberMe")?.unwrap_or(true);
    let last_username = opt_str(root, "lastUsername")?;
    let remembered_username = opt_str(root, "rememberedUsername")?;
    let profiles = match present(root, "profiles") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(decode_profile).collect::<Result<Vec<_>, ()>>()?,
        Some(_) => return Err(()),
    };
    Ok(ProfilesDocument { remember_me, last_username, remembered_username, profiles })
}

fn decode_profile(value: &Value) -> Result<ProfileDto, ()> {
    let Value::Object(obj) = value else { return Err(()) };
    Ok(ProfileDto {
        username: opt_str(obj, "username")?.unwrap_or_default(),
        salt_base64: opt_str(obj, "saltBase64")?.unwrap_or_default(),
        password_hash_base64: opt_str(obj, "passwordHashBase64")?.unwrap_or_default(),
        iterations: opt_int(obj, "iterations")?.unwrap_or(DEFAULT_ITERATIONS),
        // Lenient: missing, wrong type or unparsable → the epoch.
        created_at: opt_date(obj, "createdAtUtc").ok().flatten().unwrap_or(epoch()),
        encrypted_vault_base64: opt_str(obj, "encryptedVaultBase64")?,
    })
}

/// §5.1: pretty, sorted keys, profiles sorted by lowercased username, nulls omitted.
pub fn encode_profiles(doc: &ProfilesDocument) -> String {
    let mut profiles: Vec<&ProfileDto> = doc.profiles.iter().collect();
    profiles.sort_by(|a, b| swift_cmp(&a.username.to_lowercase(), &b.username.to_lowercase()));
    let mut root = Map::new();
    if let Some(last) = &doc.last_username {
        root.insert("lastUsername".into(), json!(last));
    }
    root.insert(
        "profiles".into(),
        Value::Array(
            profiles
                .into_iter()
                .map(|p| {
                    let mut o = Map::new();
                    o.insert("createdAtUtc".into(), json!(dates::to_string(p.created_at)));
                    if let Some(vault) = &p.encrypted_vault_base64 {
                        o.insert("encryptedVaultBase64".into(), json!(vault));
                    }
                    o.insert("iterations".into(), json!(p.iterations));
                    o.insert("passwordHashBase64".into(), json!(p.password_hash_base64));
                    o.insert("saltBase64".into(), json!(p.salt_base64));
                    o.insert("username".into(), json!(p.username));
                    Value::Object(o)
                })
                .collect(),
        ),
    );
    root.insert("rememberMe".into(), json!(doc.remember_me));
    if let Some(remembered) = &doc.remembered_username {
        root.insert("rememberedUsername".into(), json!(remembered));
    }
    root.insert("version".into(), json!(2));
    json::pretty(&Value::Object(root))
}

// ----- vault

/// The plaintext inside a profile's vault.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VaultPayload {
    pub videos: Vec<VideoUserStats>,
    pub custom_lists: Vec<UserVideoList>,
    /// Video key (fingerprint or path) → base64 image (legacy; this build always writes `{}`).
    pub thumbnails: Vec<(String, String)>,
}

/// §6 reading: any present key of the wrong type, or an element that fails, makes the whole vault unreadable.
pub fn parse_vault(data: &[u8]) -> Option<VaultPayload> {
    let Value::Object(root) = serde_json::from_slice::<Value>(strip_bom(data)).ok()? else { return None };
    let videos = match present(&root, "videos") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(decode_video).collect::<Result<Vec<_>, ()>>().ok()?,
        Some(_) => return None,
    };
    let custom_lists = match present(&root, "customLists") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(decode_list).collect::<Result<Vec<_>, ()>>().ok()?,
        Some(_) => return None,
    };
    let thumbnails = match present(&root, "thumbnails") {
        None => Vec::new(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())).ok_or(()))
            .collect::<Result<Vec<_>, ()>>()
            .ok()?,
        Some(_) => return None,
    };
    Some(VaultPayload { videos, custom_lists, thumbnails })
}

fn decode_video(value: &Value) -> Result<VideoUserStats, ()> {
    let Value::Object(obj) = value else { return Err(()) };
    let video_path = match obj.get("videoPath") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err(()),
    };
    Ok(VideoUserStats {
        content_fingerprint: opt_str(obj, "contentFingerprint")?,
        video_path,
        file_name: opt_str(obj, "fileName")?.unwrap_or_default(),
        directory_path: opt_str(obj, "directoryPath")?.unwrap_or_default(),
        view_count: opt_int(obj, "viewCount")?.unwrap_or(0),
        is_liked: opt_bool(obj, "isLiked")?.unwrap_or(false),
        rating: opt_int(obj, "rating")?.unwrap_or(0),
        last_position_seconds: opt_f64(obj, "lastPositionSeconds")?.unwrap_or(0.0),
        duration_seconds: opt_f64(obj, "durationSeconds")?.unwrap_or(0.0),
        last_watched_at: opt_date(obj, "lastWatchedAtUtc")?,
        updated_at: opt_date(obj, "updatedAtUtc")?.unwrap_or(epoch()),
        thumbnail: None,
    })
}

fn decode_list(value: &Value) -> Result<UserVideoList, ()> {
    let Value::Object(obj) = value else { return Err(()) };
    let id = match obj.get("id") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err(()),
    };
    let name = match obj.get("name") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err(()),
    };
    let video_paths = match present(obj, "videoPaths") {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            items.iter().map(|v| v.as_str().map(str::to_string).ok_or(())).collect::<Result<Vec<_>, ()>>()?
        }
        Some(_) => return Err(()),
    };
    let created_at = opt_date(obj, "createdAtUtc")?.unwrap_or(epoch());
    let updated_at = opt_date(obj, "updatedAtUtc")?.unwrap_or(created_at);
    Ok(UserVideoList { id, name, video_paths, created_at, updated_at })
}

/// §6 writing: compact, sorted keys; videos sorted by lowercased path; no `thumbnail` field.
pub fn encode_vault(payload: &VaultPayload) -> String {
    let mut videos: Vec<&VideoUserStats> = payload.videos.iter().collect();
    videos.sort_by(|a, b| swift_cmp(&a.video_path.to_lowercase(), &b.video_path.to_lowercase()));
    let videos: Vec<Value> = videos
        .into_iter()
        .map(|v| {
            let mut o = Map::new();
            if let Some(fp) = &v.content_fingerprint {
                o.insert("contentFingerprint".into(), json!(fp));
            }
            o.insert("directoryPath".into(), json!(v.directory_path));
            o.insert("durationSeconds".into(), json::number(v.duration_seconds));
            o.insert("fileName".into(), json!(v.file_name));
            o.insert("isLiked".into(), json!(v.is_liked));
            o.insert("lastPositionSeconds".into(), json::number(v.last_position_seconds));
            if let Some(at) = v.last_watched_at {
                o.insert("lastWatchedAtUtc".into(), json!(dates::to_string(at)));
            }
            o.insert("rating".into(), json!(v.rating));
            o.insert("updatedAtUtc".into(), json!(dates::to_string(v.updated_at)));
            o.insert("videoPath".into(), json!(v.video_path));
            o.insert("viewCount".into(), json!(v.view_count));
            Value::Object(o)
        })
        .collect();
    let lists: Vec<Value> = payload
        .custom_lists
        .iter()
        .map(|l| {
            json!({
                "createdAtUtc": dates::to_string(l.created_at),
                "id": l.id,
                "name": l.name,
                "updatedAtUtc": dates::to_string(l.updated_at),
                "videoPaths": l.video_paths,
            })
        })
        .collect();
    let mut thumbnails = Map::new();
    for (k, v) in &payload.thumbnails {
        thumbnails.insert(k.clone(), json!(v));
    }
    json::compact(&json!({"customLists": lists, "thumbnails": Value::Object(thumbnails), "videos": videos}))
}

/// Case-insensitive identity of a username (the key of the profile table).
pub fn username_key(name: &str) -> String {
    ci(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_header_decides_before_the_shape() {
        for (json, version) in [
            (r#"{"version": 3, "accounts": {"alice": {"salt": 5}}, "profiles": "a different shape"}"#, 3),
            (r#"{"version": 99, "profiles": {"alice": {"username": 5}}}"#, 99),
            (r#"{"version": 4, "profiles": [{"username": ["not", "a", "string"]}], "rememberMe": "yes"}"#, 4),
            (r#"{"version": 3}"#, 3),
            (r#"{"version": 3, "database": "profiles.db"}"#, 3),
        ] {
            assert_eq!(parse_profiles(json.as_bytes()), ParsedFile::Unsupported(version), "{json}");
        }
        for json in [r#"{"version": "two"}"#, r#"{"version": 2, "profiles": "x"}"#, "[1, 2, 3]", "", "{ this is not json"] {
            assert_eq!(parse_profiles(json.as_bytes()), ParsedFile::Damaged, "{json}");
        }
    }

    #[test]
    fn lenient_profile_fields() {
        let data = "\u{feff}{\"profiles\":[{\"username\":\"a\",\"createdAtUtc\":42,\"iterations\":null},{}],\"rememberedProtectedKeyBase64\":\"AAAA\",\"lastUsername\":null}";
        let ParsedFile::Document(doc) = parse_profiles(data.as_bytes()) else { panic!() };
        assert!(doc.remember_me);
        assert_eq!(doc.last_username, None);
        assert_eq!(doc.profiles.len(), 2);
        assert_eq!(doc.profiles[0].created_at, epoch());
        assert_eq!(doc.profiles[0].iterations, DEFAULT_ITERATIONS);
        assert_eq!(doc.profiles[1].username, "");
    }

    #[test]
    fn written_document_has_the_shared_shape() {
        let doc = ProfilesDocument {
            remember_me: false,
            last_username: Some("alice".into()),
            remembered_username: None,
            profiles: vec![
                ProfileDto {
                    username: "Zed".into(),
                    salt_base64: "c2FsdA==".into(),
                    password_hash_base64: "aGFzaA==".into(),
                    iterations: 100_000,
                    created_at: epoch(),
                    encrypted_vault_base64: None,
                },
                ProfileDto {
                    username: "alice".into(),
                    salt_base64: "c2FsdA==".into(),
                    password_hash_base64: "aGFzaA==".into(),
                    iterations: 100_000,
                    created_at: epoch(),
                    encrypted_vault_base64: Some("v/x+".into()),
                },
            ],
        };
        let text = encode_profiles(&doc);
        assert!(!text.contains("null"));
        assert!(!text.contains("\\/"));
        assert!(text.contains("\"iterations\" : 100000"));
        assert!(text.find("\"alice\"").unwrap() < text.find("\"Zed\"").unwrap());
        assert_eq!(parse_profiles(text.as_bytes()), ParsedFile::Document(ProfilesDocument {
            profiles: vec![doc.profiles[1].clone(), doc.profiles[0].clone()],
            ..doc.clone()
        }));
    }

    #[test]
    fn vault_round_trips_and_rejects_bad_elements() {
        let text = r#"{"videos":[{"contentFingerprint":"fp:5:abcdef","videoPath":"/Movies/film.mkv","fileName":"film.mkv","directoryPath":"/Movies","viewCount":3,"isLiked":true,"rating":4,"lastPositionSeconds":12.5,"durationSeconds":99,"lastWatchedAtUtc":"2026-05-01T10:20:30.1234567+00:00","updatedAtUtc":"2026-05-01T10:20:30.1234567+00:00"}],"customLists":[{"id":"0123456789abcdef0123456789abcdef","name":"Liste","videoPaths":["\/Movies\/film.mkv"],"createdAtUtc":"2026-05-01T10:00:00+03:00","updatedAtUtc":"2026-05-01T10:00:00Z"}],"thumbnails":{"fp:5:abcdef":"SU1H"}}"#;
        let payload = parse_vault(text.as_bytes()).unwrap();
        assert_eq!(payload.videos[0].view_count, 3);
        assert_eq!(payload.custom_lists[0].video_paths, ["/Movies/film.mkv"]);
        assert_eq!(payload.custom_lists[0].created_at, dates::parse("2026-05-01T07:00:00Z").unwrap());
        let again = parse_vault(encode_vault(&payload).as_bytes()).unwrap();
        assert_eq!(again, payload);
        assert!(encode_vault(&payload).starts_with("{\"customLists\":[{\"createdAtUtc\""));

        assert!(parse_vault(br#"{"videos":[{"fileName":"x"}]}"#).is_none(), "videoPath is required");
        assert!(parse_vault(br#"{"videos":[{"videoPath":"/a","updatedAtUtc":"yesterday"}]}"#).is_none());
        assert!(parse_vault(br#"{"videos":{}}"#).is_none());
        assert!(parse_vault(br#"{"customLists":[{"id":"a"}]}"#).is_none());
        let minimal = parse_vault(br#"{"customLists":[{"id":"a","name":"n","createdAtUtc":"2026-01-01T00:00:00Z"}]}"#).unwrap();
        assert_eq!(minimal.custom_lists[0].updated_at, minimal.custom_lists[0].created_at);
        assert_eq!(parse_vault(b"{}").unwrap(), VaultPayload::default());
    }
}
