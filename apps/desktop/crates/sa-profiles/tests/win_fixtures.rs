//! Data compatibility with the C# app (win-profiles §17 "format compatibility"), checked against golden data the C#
//! `LocalProfileManager` / `SqliteProfileStore` / `StateSyncBroker` wrote before the C# app was removed (frozen under
//! `tests/fixtures/csharp/`, see the README there):
//!
//! * C# writes → Rust reads: `profiles.json` + `profiles.db` written by C# (passwords, DPAPI remember-me, history,
//!   likes, ratings, progress, lists, thumbnails), also after a second C# step on top, read by Rust exactly as C# read
//!   them (the frozen C# "dump");
//! * Rust writes on top of C# data, and Rust writes from scratch: Rust reads its own result back exactly as C# read
//!   the data C# wrote for the same scenario (only times and new list ids may differ), and the written files have the
//!   C# structure (header JSON text, SQLite schema, row counts, blob sizes);
//! * a version-2 header with legacy monolithic vaults migrates to the data C# migrated it to;
//! * fingerprints, crypto primitives and sealed broker events match the C# values.
//!
//! The C# data refers to media files at a fixed absolute path ([`FIXTURE_ROOT`]); the tests recreate them there
//! (deterministic contents written atomically, then only read). The sealed-event test also needs the C# data at a
//! fixed folder there (`event-data`, its scope path is part of the event's AAD): it holds a cross-process lock on it
//! while it runs and removes it afterwards. The remembered sessions in the C# data are DPAPI-protected for the
//! Windows account that produced them; on any other account the restore part is skipped with a message.
#![cfg(windows)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use sa_profiles::win::WinProfileStore;
use sa_profiles::{ProfileStore, VideoFeedFilter, VideoUserStats};
use sa_sync::{InProcessBus, ProfileWriteBroker};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// Where the C# fixtures kept their media (and the folder of the sealed event's scope path).
const FIXTURE_ROOT: &str = r"C:\ProgramData\SubtitleAdderTestFixtures\profiles";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/csharp")
}

fn frozen_json(rel: &str) -> Value {
    let text = std::fs::read_to_string(fixtures().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

// ---------------------------------------------------------------------------------------------------------------
// Shared scenario (mirrors the C# fixture tool's generate / mutate steps)

const A: &str = "Yaz Tatili Ç.mp4";
const B: &str = "missing_episode.mkv";
const C: &str = "belgesel-uzay.mp4";
const D: &str = "zeynep_klip.mp4";

/// The fixture tool's `Bytes`: xorshift32.
fn bytes(len: usize, seed: u32) -> Vec<u8> {
    let mut x: u32 = if seed == 0 { 0x9E37_79B9 } else { seed.wrapping_mul(0x9E37_79B9) };
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

struct Media {
    a: String,
    b: String,
    c: String,
    d: String,
}

/// Writes `content` to `path` unless it is already there (atomically, so parallel test processes never see a
/// partial file).
fn ensure_file(path: &Path, content: &[u8]) {
    if std::fs::read(path).ok().as_deref() == Some(content) {
        return;
    }
    let name = path.file_name().expect("name").to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, content).expect("media temp file");
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(std::fs::read(path).ok().as_deref(), Some(content), "media file {path:?}");
    }
}

/// The media the C# data refers to, at the fixed root: `a`, `c`, `d` exist, `b` never does.
fn media() -> Media {
    static M: OnceLock<()> = OnceLock::new();
    let dir = Path::new(FIXTURE_ROOT).join("media");
    let p = |n: &str| dir.join(n).to_string_lossy().into_owned();
    let m = Media { a: p(A), b: p(B), c: p(C), d: p(D) };
    M.get_or_init(|| {
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("media folder {dir:?}: {e}"));
        ensure_file(Path::new(&m.a), &bytes(5000, 1));
        ensure_file(Path::new(&m.c), &bytes(70000, 2));
        ensure_file(Path::new(&m.d), &bytes(1234, 3));
        assert!(!Path::new(&m.b).exists(), "{} must not exist", m.b);
    });
    m
}

/// A private copy of a frozen C# data folder (`<scenario>/data` or another folder of it) at `dst`.
fn frozen_data(rel: &str, dst: &Path) -> PathBuf {
    media();
    let _ = std::fs::remove_dir_all(dst);
    std::fs::create_dir_all(dst).expect("data dir");
    for e in std::fs::read_dir(fixtures().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}")) {
        let e = e.expect("entry");
        if e.file_type().expect("type").is_file() {
            std::fs::copy(e.path(), dst.join(e.file_name())).expect("copy");
        }
    }
    dst.to_path_buf()
}

fn open(data: &Path) -> Arc<WinProfileStore> {
    let bus = InProcessBus::new();
    let broker: Arc<dyn ProfileWriteBroker> = bus;
    WinProfileStore::open(data.join("profiles.json"), broker)
}

fn rust_generate(data: &Path) {
    std::fs::create_dir_all(data).expect("data dir");
    let m = media();
    let s = open(data);
    s.register_user("Emre", "pässwörd-1", false).expect("register Emre");
    s.record_watch_with(&m.a, 10.0, 100.0, None, None);
    s.record_watch_with(&m.a, 40.0, 100.0, None, None);
    s.update_playback_progress_with(&m.a, 55.0, 100.0, None);
    s.set_liked_path(&m.a, true);
    s.set_rating_path(&m.a, 4);
    assert!(s.set_thumbnail_with(&m.a, "THUMB-A-ÇĞ".as_bytes(), None));
    s.record_watch_with(&m.b, 5.0, 50.0, None, None);
    s.set_liked_path(&m.c, true);
    let l1 = s.create_custom_list("İzlenecekler").expect("list 1");
    assert!(s.add_video_to_custom_list(&l1.id, &m.a));
    assert!(s.add_video_to_custom_list(&l1.id, &m.b));
    let l2 = s.create_custom_list("Belgeseller").expect("list 2");
    assert!(s.add_video_to_custom_list(&l2.id, &m.c));
    assert!(s.set_thumbnail_with(&m.c, &bytes(3000, 4), None));
    s.logout_user();

    s.register_user("zeynep", "z-pass", true).expect("register zeynep");
    s.record_watch_with(&m.d, 12.5, 60.0, None, None);
    s.set_rating_path(&m.d, 5);
    assert!(s.flush_pending_writes(Duration::from_secs(10)));
    s.dispose();
}

fn rust_mutate(data: &Path) {
    let m = media();
    let s = open(data);
    s.login_user("emre", "pässwörd-1", false).expect("login emre");
    s.record_watch_with(&m.a, 70.0, 100.0, None, None);
    assert!(!s.toggle_like(&m.a), "unlike a");
    s.set_rating_path(&m.c, 3);
    let lists = s.get_custom_lists();
    let l1 = lists.iter().find(|l| l.name == "İzlenecekler").expect("list 1").clone();
    let l2 = lists.iter().find(|l| l.name == "Belgeseller").expect("list 2").clone();
    assert!(s.remove_video_from_custom_list(&l1.id, &m.b), "list 1 remove b");
    assert!(s.rename_custom_list(&l2.id, "Uzay Belgeselleri"));
    assert!(s.set_thumbnail_with(&m.b, &bytes(700, 5), None));
    let l3 = s.create_custom_list("Yeni Liste").expect("list 3");
    assert!(s.add_video_to_custom_list(&l3.id, &m.d));
    assert!(s.add_video_to_custom_list(&l3.id, &m.a));
    s.logout_user();

    s.login_user("ZEYNEP", "z-pass", true).expect("login zeynep");
    assert!(s.clear_history());
    s.set_liked_path(&m.c, true);
    assert!(s.flush_pending_writes(Duration::from_secs(10)));
    s.dispose();
}

// ---------------------------------------------------------------------------------------------------------------
// Rust dump (mirrors the fixture tool's `Dump`, the canonical JSON of what an app reads)

fn paths_of(items: &[VideoUserStats]) -> Value {
    Value::Array(items.iter().map(|v| json!(v.video_path)).collect())
}

fn rust_dump_profile(s: &WinProfileStore) -> Value {
    let never = || true;
    let mut all = s.get_feed(&VideoFeedFilter::All, None, None, &never);
    all.sort_by_key(|v| upper_ordinal(&v.video_path));
    let videos: Vec<Value> = all
        .iter()
        .map(|v| {
            let engaged = v.view_count > 0 || v.is_liked || v.rating > 0 || v.last_position_seconds > 0.0 || v.last_watched_at.is_some();
            let thumb = s.get_thumbnail(&v.video_path);
            json!({
                "videoPath": v.video_path,
                "fileName": v.file_name,
                "directoryPath": v.directory_path,
                "contentFingerprint": v.content_fingerprint,
                "viewCount": v.view_count,
                "isLiked": v.is_liked,
                "rating": v.rating,
                "lastPositionSeconds": v.last_position_seconds,
                "durationSeconds": v.duration_seconds,
                "lastWatchedTicks": v.last_watched_at.map(|t| t.0),
                "updatedTicks": if engaged { Some(v.updated_at.0) } else { None },
                "thumbnailSha256": thumb.map(|t| hex(&Sha256::digest(&t))),
            })
        })
        .collect();
    let lists: Vec<Value> = s
        .get_custom_lists()
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "name": l.name,
                "videoPaths": l.video_paths,
                "createdTicks": l.created_at.0,
                "updatedTicks": l.updated_at.0,
            })
        })
        .collect();
    let t = s.totals();
    json!({
        "username": s.current_username(),
        "videos": videos,
        "history": paths_of(&s.get_feed(&VideoFeedFilter::History, None, None, &never)),
        "liked": paths_of(&s.get_feed(&VideoFeedFilter::Liked, None, None, &never)),
        "mostWatched": paths_of(&s.get_feed(&VideoFeedFilter::MostWatched, None, None, &never)),
        "topRated": paths_of(&s.get_feed(&VideoFeedFilter::TopRated, None, None, &never)),
        "lists": lists,
        "totals": { "watched": t.watched, "views": t.views, "liked": t.liked, "rated": t.rated },
    })
}

fn rust_dump(data: &Path, creds: &[(&str, &str)]) -> Value {
    let s = open(data);
    let mut root = Map::new();
    root.insert("restoredUser".into(), json!(s.current_username()));
    root.insert("rememberMe".into(), json!(s.remember_me()));
    root.insert("registered".into(), json!(s.registered_usernames()));
    root.insert("lastUsername".into(), json!(s.last_username()));
    if s.is_logged_in() {
        root.insert("restored".into(), rust_dump_profile(&s));
    }
    let mut profiles = Map::new();
    for (u, p) in creds {
        s.login_user(u, p, false).unwrap_or_else(|e| panic!("login {u}: {e:?}"));
        profiles.insert((*u).into(), rust_dump_profile(&s));
    }
    root.insert("profiles".into(), Value::Object(profiles));
    assert!(s.flush_pending_writes(Duration::from_secs(10)), "dump flush");
    s.dispose();
    Value::Object(root)
}

fn upper_ordinal(s: &str) -> Vec<u16> {
    // Ordinal order of ToUpperInvariant (the fixture paths are ASCII + Ç, which upper-cases to itself).
    s.to_uppercase().encode_utf16().collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Copies a data folder (a dump writes: it logs in, which clears a remembered session).
fn copy_data(data: &Path, name: &str) -> PathBuf {
    let dst = data.parent().expect("parent").join(name);
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&dst).expect("copy dir");
    for e in std::fs::read_dir(data).expect("read data") {
        let e = e.expect("entry");
        if e.file_type().expect("type").is_file() {
            std::fs::copy(e.path(), dst.join(e.file_name())).expect("copy");
        }
    }
    dst
}

// ---------------------------------------------------------------------------------------------------------------
// Comparison

/// Integral numbers as i64, the rest as f64 (C# writes `60`, Rust `60.0`); ticks keep full precision.
fn canon(v: &Value) -> Value {
    match v {
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => json!(i),
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 9.0e15 => json!(f as i64),
            (None, Some(f)) => json!(f),
            _ => panic!("number {n}"),
        },
        Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), canon(v))).collect()),
        other => other.clone(),
    }
}

/// UNIFIED.md §4 item 7: the Rust app gives reachable path-keyed rows their fingerprint at login; the C# app reads
/// them without one. Accept exactly that difference (C# `null`, Rust = the file's fingerprint).
fn allow_fingerprint_upgrade(expected: &mut Value, actual: &Value) {
    fn walk(e: &mut Value, a: &Value) {
        match (e, a) {
            (Value::Object(eo), Value::Object(ao)) => {
                if let (Some(Value::Null), Some(Value::String(fp)), Some(Value::String(path))) =
                    (eo.get("contentFingerprint"), ao.get("contentFingerprint"), ao.get("videoPath"))
                {
                    if sa_profiles::fingerprint::compute(Path::new(path)).as_deref() == Some(fp.as_str()) {
                        eo.insert("contentFingerprint".into(), Value::String(fp.clone()));
                    }
                }
                for (k, ev) in eo.iter_mut() {
                    if let Some(av) = ao.get(k) {
                        walk(ev, av);
                    }
                }
            }
            (Value::Array(ea), Value::Array(aa)) => {
                for (ev, av) in ea.iter_mut().zip(aa) {
                    walk(ev, av);
                }
            }
            _ => {}
        }
    }
    walk(expected, actual);
}

fn assert_same(what: &str, expected: &Value, actual: &Value, fp_upgrade: bool) {
    let mut e = canon(expected);
    let a = canon(actual);
    if fp_upgrade {
        allow_fingerprint_upgrade(&mut e, &a);
    }
    if e != a {
        panic!(
            "{what}: dumps differ\n--- C#\n{}\n--- Rust\n{}",
            serde_json::to_string_pretty(&e).unwrap(),
            serde_json::to_string_pretty(&a).unwrap()
        );
    }
}

fn header(data: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(data.join("profiles.json")).expect("header")).expect("header json")
}

/// Whether the remembered session of `data` must restore on this Windows account: true when none is stored, or when
/// the tests run as the account that recorded the C# data (SHA-256 of its user SID in `recording-account.json`;
/// DPAPI user scope binds the blob to that account). Deliberately independent of the Rust DPAPI code under test, so
/// a broken unprotect fails on the recording account instead of looking like another account.
fn remembered_session_opens_here(data: &Path) -> bool {
    let h = header(data);
    if h["rememberedProtectedKeyBase64"].as_str().is_none() {
        return true;
    }
    let recorded = frozen_json("recording-account.json")["user_sid_sha256"].as_str().expect("user_sid_sha256").to_owned();
    let sid = sa_sync::win_testing::current_user_sid().expect("current user SID");
    let here: String = Sha256::digest(sid.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    here == recorded
}

/// Dumps a copy of `data` with Rust and compares it with the frozen C# dump of the same data. When the remembered
/// session in the C# data was protected for another Windows account, the restore is not compared (Rust must then
/// restore nothing) and the rest still is.
fn assert_rust_reads_like_csharp(what: &str, data: &Path, creds: &[(&str, &str)], expected: &Value, fp_upgrade: bool) -> Value {
    let restorable = remembered_session_opens_here(data);
    let actual = rust_dump(&copy_data(data, "dump-rs"), creds);
    if restorable {
        assert_same(what, expected, &actual, fp_upgrade);
    } else {
        eprintln!("SKIPPED ({what}): the C# remembered session is DPAPI-protected for another Windows account");
        assert_eq!(actual["restoredUser"], Value::Null, "{what}: nothing to restore on this account");
        let strip = |v: &Value| {
            let mut v = v.clone();
            let o = v.as_object_mut().expect("dump object");
            for k in ["restoredUser", "restored", "rememberMe"] {
                o.shift_remove(k);
            }
            v
        };
        assert_same(what, &strip(expected), &strip(&actual), fp_upgrade);
    }
    actual
}

/// The largest `*Ticks` value of a dump.
fn max_ticks(v: &Value) -> i64 {
    match v {
        Value::Object(o) => o
            .iter()
            .map(|(k, v)| if k.ends_with("Ticks") { v.as_i64().unwrap_or(i64::MIN) } else { max_ticks(v) })
            .max()
            .unwrap_or(i64::MIN),
        Value::Array(a) => a.iter().map(max_ticks).max().unwrap_or(i64::MIN),
        _ => i64::MIN,
    }
}

/// Replaces the values two independent runs of one scenario cannot share: `*Ticks` later than `after` (written by
/// the run itself, not read from older data) and list ids outside `known_ids`; lists are then ordered by name (their
/// order follows the random ids).
fn mask_new(v: &Value, after: i64, known_ids: &HashSet<String>) -> Value {
    fn walk(v: &Value, after: i64, known: &HashSet<String>) -> Value {
        match v {
            Value::Object(o) => {
                let mut out = Map::new();
                for (k, x) in o {
                    let masked = if k.ends_with("Ticks") && x.as_i64().is_some_and(|t| t > after) {
                        json!("<new tick>")
                    } else if k == "id" && x.as_str().is_some_and(|id| !known.contains(id)) {
                        json!("<new id>")
                    } else {
                        walk(x, after, known)
                    };
                    out.insert(k.clone(), masked);
                }
                if let Some(Value::Array(lists)) = out.get_mut("lists") {
                    lists.sort_by_key(|l| l["name"].as_str().unwrap_or("").to_owned());
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, after, known)).collect()),
            other => other.clone(),
        }
    }
    walk(&canon(v), after, known_ids)
}

fn list_ids(dump: &Value) -> HashSet<String> {
    fn walk(v: &Value, out: &mut HashSet<String>) {
        match v {
            Value::Object(o) => {
                if let Some(Value::Array(lists)) = o.get("lists") {
                    out.extend(lists.iter().filter_map(|l| l["id"].as_str().map(str::to_owned)));
                }
                o.values().for_each(|x| walk(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = HashSet::new();
    walk(dump, &mut out);
    out
}

fn assert_masked_same(what: &str, expected: &Value, actual: &Value) {
    if expected != actual {
        panic!(
            "{what}: dumps differ\n--- C#\n{}\n--- Rust\n{}",
            serde_json::to_string_pretty(expected).unwrap(),
            serde_json::to_string_pretty(actual).unwrap()
        );
    }
}

/// `profiles.json` text with the values that are random per run (salts, hashes, sealed vaults, DPAPI blobs, creation
/// times) replaced; everything else (keys, their order, indentation, CRLF line breaks, escaping, fixed values) stays
/// byte for byte.
fn masked_header_text(data: &Path) -> String {
    masked_json_text(&std::fs::read_to_string(data.join("profiles.json")).expect("header"))
}

/// [`masked_header_text`] of a header text. The masked values are base64 / `DateTimeOffset` text, where C# escapes
/// `+` as `+` and Rust does not (both read either form).
fn masked_json_text(text: &str) -> String {
    const RANDOM: [&str; 5] = ["saltBase64", "passwordHashBase64", "createdAtUtc", "encryptedVaultBase64", "rememberedProtectedKeyBase64"];
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        match RANDOM.iter().find(|k| trimmed.starts_with(&format!("\"{k}\": \""))) {
            Some(k) => {
                let indent = &line[..line.len() - trimmed.len()];
                let value_start = indent.len() + k.len() + 5;
                let value_end = line.rfind('"').expect("closing quote");
                let value = &line[value_start..value_end];
                let kept = if *k == "rememberedProtectedKeyBase64" { value.split_once(':').map_or("", |(p, _)| p) } else { "" };
                out.push_str(&format!("{indent}\"{k}\": \"<{kept}>{}", &line[value_end..]));
            }
            None => out.push_str(line),
        }
    }
    out
}

/// What a C# reader depends on in `profiles.db`, independent of random keys and times: the schema, the journal mode
/// and user version, the `meta` rows, the row count of every table (except the request-id journal) and the sorted
/// sizes of the encrypted blobs.
fn db_structure(data: &Path) -> Value {
    // Read a copy: opening a WAL database leaves -shm/-wal files next to it.
    let tmp = tempfile::tempdir().expect("temp");
    for e in std::fs::read_dir(data).expect("read data") {
        let e = e.expect("entry");
        if e.file_name().to_string_lossy().starts_with("profiles.db") {
            std::fs::copy(e.path(), tmp.path().join(e.file_name())).expect("copy db");
        }
    }
    let db = rusqlite::Connection::open(tmp.path().join("profiles.db")).expect("open db");
    let schema: Vec<Value> = db
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name")
        .unwrap()
        .query_map([], |r| {
            Ok(json!([r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?]))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let meta: Vec<Value> = db
        .prepare("SELECT key, value FROM meta ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok(json!([r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?])))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut tables = Map::new();
    for t in ["profiles", "videos", "watch_progress", "thumbnails", "custom_lists", "custom_list_items"] {
        let count: i64 = db.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap();
        let mut blob_sizes: Vec<i64> = Vec::new();
        let has_blob: bool = db
            .query_row(&format!("SELECT COUNT(*) FROM pragma_table_info('{t}') WHERE name = 'encrypted_blob'"), [], |r| r.get::<_, i64>(0))
            .unwrap()
            > 0;
        if has_blob {
            blob_sizes = db
                .prepare(&format!("SELECT length(encrypted_blob) FROM {t}"))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            blob_sizes.sort_unstable();
        }
        tables.insert(t.into(), json!({ "rows": count, "blobSizes": blob_sizes }));
    }
    let journal: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    let user_version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    json!({ "schema": schema, "journalMode": journal, "userVersion": user_version, "meta": meta, "tables": tables })
}

fn assert_same_db_structure(what: &str, csharp: &Path, rust: &Path) {
    let (e, a) = (db_structure(csharp), db_structure(rust));
    if e != a {
        panic!(
            "{what}: profiles.db differs\n--- C#\n{}\n--- Rust\n{}",
            serde_json::to_string_pretty(&e).unwrap(),
            serde_json::to_string_pretty(&a).unwrap()
        );
    }
}

const CREDS: [(&str, &str); 2] = [("Emre", "pässwörd-1"), ("zeynep", "z-pass")];

// ---------------------------------------------------------------------------------------------------------------
// Tests

#[test]
fn csharp_written_profiles_read_identically_in_rust() {
    let root = tempfile::tempdir().expect("temp");
    let data = frozen_data("generate/data", &root.path().join("data"));
    let cs = frozen_json("generate/dump.json");

    // C# → Rust: the remembered session (DPAPI, written by C#) restores in Rust too.
    assert_rust_reads_like_csharp("C# generate", &data, &CREDS, &cs, false);
    assert_eq!(cs["restoredUser"], json!("zeynep"));
    assert_eq!(cs["profiles"]["Emre"]["lists"].as_array().map(Vec::len), Some(2));
    assert_eq!(cs["profiles"]["Emre"]["videos"][2]["viewCount"], json!(2));
}

#[test]
fn csharp_writes_on_top_of_csharp_data_read_identically_in_rust() {
    // The second C# step (view-count deltas, HMAC list-item removal, renames, thumbnails, history clearing) on top
    // of the first, read by Rust.
    let root = tempfile::tempdir().expect("temp");
    let data = frozen_data("generate-mutate/data", &root.path().join("data"));
    let cs = frozen_json("generate-mutate/dump.json");
    assert_rust_reads_like_csharp("C# generate + C# mutate", &data, &CREDS, &cs, false);
    assert_eq!(cs["restoredUser"], json!("zeynep"));
    assert_eq!(cs["restored"]["totals"]["watched"], json!(0));
}

#[test]
fn rust_writes_on_top_of_csharp_data_like_csharp_does() {
    // Rust runs the second step on the C# data; reading the result must give what C# read after running the same
    // step itself (only the times the step wrote and the new list's id differ), and the files keep the C# structure.
    let root = tempfile::tempdir().expect("temp");
    let data = frozen_data("generate/data", &root.path().join("data"));
    rust_mutate(&data);
    let rs = rust_dump(&copy_data(&data, "dump-rs"), &CREDS);

    let base = frozen_json("generate/dump.json");
    let cs = frozen_json("generate-mutate/dump.json");
    let (after, known) = (max_ticks(&base), list_ids(&base));
    assert_masked_same("C# generate + Rust mutate", &mask_new(&cs, after, &known), &mask_new(&rs, after, &known));

    let emre = &rs["profiles"]["Emre"];
    let a = emre["videos"].as_array().unwrap().iter().find(|v| v["fileName"] == json!(A)).expect("a");
    assert_eq!(a["viewCount"], json!(3));
    assert_eq!(a["isLiked"], json!(false));
    let names: Vec<&str> = emre["lists"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Uzay Belgeselleri") && names.contains(&"Yeni Liste"), "{names:?}");
    assert_eq!(rs["restoredUser"], json!("zeynep"), "Rust re-wrote the remembered session");
    assert_eq!(rs["restored"]["totals"]["watched"], json!(0), "history cleared by Rust");

    let cs_data = frozen_data("generate-mutate/data", &root.path().join("cs-data"));
    assert_eq!(masked_header_text(&data), masked_header_text(&cs_data), "profiles.json as C# writes it");
    assert_same_db_structure("C# generate + Rust mutate", &cs_data, &data);
}

#[test]
fn rust_written_profiles_match_the_csharp_written_data_for_the_same_scenario() {
    let root = tempfile::tempdir().expect("temp");
    let data = root.path().join("data");
    rust_generate(&data);

    let h = header(&data);
    assert_eq!(h["version"], json!(3));
    assert_eq!(h["rememberMeOptIn"], json!(true));
    assert!(h["rememberedProtectedKeyBase64"].as_str().unwrap().starts_with("dpapi1:"));
    let cs_data = frozen_data("generate/data", &root.path().join("cs-data"));
    assert_eq!(masked_header_text(&data), masked_header_text(&cs_data), "profiles.json as C# writes it");
    assert_same_db_structure("Rust generate", &cs_data, &data);

    // Rust reads its own data (the DPAPI remember-me written by Rust restores) as C# read the C# data: everything
    // but times and list ids is equal.
    let rs = rust_dump(&copy_data(&data, "dump-rs"), &CREDS);
    assert_eq!(rs["restoredUser"], json!("zeynep"));
    assert_eq!(rs["restored"]["totals"], json!({"watched": 1, "views": 1, "liked": 0, "rated": 1}));
    let cs = frozen_json("generate/dump.json");
    let none = HashSet::new();
    assert_masked_same("Rust generate", &mask_new(&cs, i64::MIN, &none), &mask_new(&rs, i64::MIN, &none));
}

#[test]
fn legacy_version_2_vaults_migrate_to_the_same_data_on_both_sides() {
    let root = tempfile::tempdir().expect("temp");
    let data = frozen_data("legacy/data", &root.path().join("data"));
    let creds = [("eski", "eski-şifre"), ("bos", "bos-pw")];

    let rs_dir = copy_data(&data, "legacy-rs");
    let cs = frozen_json("legacy/dump.json");
    let rs = rust_dump(&rs_dir, &creds);
    assert_same("legacy migration", &cs, &rs, true);
    assert_eq!(rs["restoredUser"], Value::Null, "a pre-opt-in remembered session is dropped");
    assert_eq!(rs["lastUsername"], json!("eski"));
    let eski = &rs["profiles"]["eski"];
    assert_eq!(eski["lists"][0]["name"], json!("Eski Liste"));
    assert_eq!(eski["lists"][0]["videoPaths"].as_array().map(Vec::len), Some(2), "deduplicated, blank dropped");

    // Rust migrated: the header carries markers, a backup exists without a remembered key, and all of it has the
    // shape of the C# migration of the same file.
    let h = header(&rs_dir);
    assert_eq!(h["version"], json!(3));
    for p in h["profiles"].as_array().unwrap() {
        assert_eq!(p["encryptedVaultBase64"].as_str().map(str::len), Some(68), "vault marker");
    }
    assert!(h.get("rememberedProtectedKeyBase64").is_none() && h["rememberMe"] == json!(false));
    let bak = std::fs::read_to_string(rs_dir.join("profiles.json.bak")).expect("backup");
    assert!(!bak.contains("rememberedProtectedKeyBase64"));
    let cs_migrated = frozen_data("legacy/migrated", &root.path().join("legacy-cs"));
    let cs_bak = std::fs::read_to_string(cs_migrated.join("profiles.json.bak")).expect("C# backup");
    // The backup keeps the original values: the same members and values as the C# backup, laid out the same way.
    let parse = |t: &str| serde_json::from_str::<Value>(t).expect("backup json");
    assert_eq!(parse(&bak), parse(&cs_bak), "backup contents");
    assert_eq!(json_keys(&parse(&bak)), json_keys(&parse(&cs_bak)), "backup member order");
    assert_eq!(masked_json_text(&bak), masked_json_text(&cs_bak), "backup as C# writes it");
    assert_eq!(masked_header_text(&rs_dir), masked_header_text(&cs_migrated), "migrated profiles.json as C# writes it");
    assert_same_db_structure("legacy migration", &cs_migrated, &rs_dir);

    // The migrated folders read back the same: Rust's own migration, and the C# migration read by Rust.
    let again = rust_dump(&copy_data(&rs_dir, "legacy-rs-again"), &creds);
    assert_eq!(again["lastUsername"], json!("bos"));
    assert_same("Rust reading the Rust migration", &rs["profiles"], &again["profiles"], false);
    let cs_again = rust_dump(&cs_migrated, &creds);
    assert_same("Rust reading the C# migration", &cs["profiles"], &cs_again["profiles"], true);
}

#[test]
fn csharp_fingerprints_match_rust_for_overlapping_slices() {
    let root = tempfile::tempdir().expect("temp");
    let cases = frozen_json("fingerprints.json");
    let cases = cases.as_array().expect("cases");
    assert_eq!(cases.len(), 5);
    for case in cases {
        let (len, seed) = (case["length"].as_u64().unwrap() as usize, case["seed"].as_u64().unwrap() as u32);
        let p = root.path().join(format!("f{len}.bin"));
        std::fs::write(&p, bytes(len, seed)).expect("write");
        assert_eq!(sa_profiles::fingerprint::compute(&p).as_deref(), case["fingerprint"].as_str(), "len {len}");
    }
}

#[test]
fn csharp_crypto_vectors_match_rust() {
    let v = frozen_json("vectors.json");
    let s = |k: &str| v[k].as_str().unwrap_or_else(|| panic!("{k}")).to_owned();
    let unhex = |h: &str| (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
    let b64 = base64::engine::general_purpose::STANDARD;

    let salt = unhex(&s("saltHex"));
    let (hash, key) = sa_profiles::crypto::derive(&s("password"), &salt, 100_000);
    assert_eq!(b64.encode(hash), s("hashBase64"));
    assert_eq!(hex(key.expose()), s("keyHex"));
    let path = s("turkishPath");
    assert_eq!(sa_profiles::win::storage_video_key(&key, &format!("  {path} ")), s("storageVideoKey"));
    assert_eq!(sa_profiles::win::storage_video_key(&key, "fp:5000:3ab99b16d24511552b0d2f52"), s("storageVideoKeyFp"));
    assert_eq!(sa_profiles::win::storage_owner_tag(&key), s("ownerTag"));
    assert_eq!(sa_profiles::win::list_item_hash("Emre", "0123456789ABCDEFabcdef0123456789", &path, &key), s("listItemHash"));
    assert_eq!(sa_profiles::win::netcompat::upper_invariant("ıiİß ǅ ﬃ ΐ ſ µ ẞ ǰ"), s("upperInvariant"));
    let blob = b64.decode(s("csharpBlobBase64")).expect("blob");
    assert_eq!(sa_profiles::crypto::open(&blob, &key).as_deref(), Some(sa_profiles::crypto::VAULT_MARKER.as_bytes()));
    assert_eq!(sa_profiles::win::vault_marker(&key).len(), s("csharpBlobBase64").len(), "a Rust marker has the C# size");
    assert_eq!(v["vaultMarkerPlainLength"], json!(sa_profiles::crypto::VAULT_MARKER.len()));
    assert_eq!(v["pad300"], json!(sa_profiles::win::pad_to_bucket(vec![0; 300], 256, b' ').len()));
    assert_eq!(v["pad1025"], json!(sa_profiles::win::pad_to_bucket(vec![0; 1025], 1024, b' ').len()));
}

/// Exclusive use of `<FIXTURE_ROOT>\<name>` across test processes (concurrent `cargo test` runs on one machine would
/// otherwise delete each other's data mid-test): an open handle with no sharing on `<name>.lock`, retried for up to
/// two minutes. Dropping it removes the folder and releases the lock.
struct FixedRootLock {
    dir: PathBuf,
    _file: std::fs::File,
}

impl FixedRootLock {
    fn acquire(name: &str) -> FixedRootLock {
        use std::os::windows::fs::OpenOptionsExt;
        let root = Path::new(FIXTURE_ROOT);
        std::fs::create_dir_all(root).unwrap_or_else(|e| panic!("{root:?}: {e}"));
        let lock = root.join(format!("{name}.lock"));
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            match std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).share_mode(0).open(&lock) {
                Ok(file) => return FixedRootLock { dir: root.join(name), _file: file },
                Err(e) if std::time::Instant::now() >= deadline => panic!("{lock:?} stays locked by another test run: {e}"),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }
}

impl Drop for FixedRootLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn json_keys(v: &Value) -> Vec<String> {
    v.as_object().expect("object").keys().cloned().collect()
}

#[test]
fn sealed_broker_events_cross_between_the_csharp_and_rust_apps() {
    // A C# window and a Rust window on one broker channel: watch-progress events sealed by either side open on the
    // other (same IPC key derivation and AAD), so live sync keeps working while both apps run. The C# event's scope
    // (part of its AAD) is the profiles.json it was sealed for, so the C# data is opened at that fixed path.
    let _lock = FixedRootLock::acquire("event-data");
    let data = frozen_data("generate/data", &Path::new(FIXTURE_ROOT).join("event-data"));
    let m = media();

    let evt_json = std::fs::read_to_string(fixtures().join("generate/watch-progress-event.json")).expect("C# event");
    let evt = sa_sync::wire::parse_event(&evt_json).expect("C# event parses");
    assert_eq!(evt.scope_path, data.join("profiles.json").to_string_lossy());
    assert!(evt.video_path.is_none() && evt.position_seconds.is_none() && evt.sealed_payload_base64.is_some());
    let bus = InProcessBus::new();
    let broker: Arc<dyn ProfileWriteBroker> = bus.clone();
    let s = WinProfileStore::open(data.join("profiles.json"), broker);
    s.login_user("Emre", "pässwörd-1", false).expect("login");
    assert_eq!(s.get_stats_fast_in_memory(&m.a).map(|v| v.last_position_seconds), Some(55.0));
    sa_sync::SyncBus::publish(bus.as_ref(), sa_sync::Topic::WatchProgress, evt);
    assert_eq!(s.get_stats_fast_in_memory(&m.a).map(|v| v.last_position_seconds), Some(61.5), "C# event applied in Rust");

    // Rust → C#: the event a Rust window publishes has the C# wire shape (the same fields in the same order, the same
    // topic, kind and scope, and for the same values a sealed payload with the same members and values; C# escapes
    // the non-ASCII path in it, Rust does not) ...
    let csharp: Value = serde_json::from_str(&evt_json).expect("C# event json");
    let same_values = s.create_broker_event_for_tests(
        sa_sync::Topic::WatchProgress,
        sa_sync::EventKind::ProgressUpdated,
        Some(&m.a),
        Some(61.5),
        Some(300.0),
        None,
    );
    let same_values: Value = serde_json::from_str(&sa_sync::wire::event_line(&same_values).expect("line")).expect("json");
    assert_eq!(json_keys(&same_values), json_keys(&csharp));
    for k in ["topic", "kind", "scopePath"] {
        assert_eq!(same_values[k], csharp[k], "{k}");
    }
    let (_, vault_key) = s.try_get_active_profile_vault_key().expect("vault key");
    let ipc_key = sa_profiles::crypto::hmac_sha256(vault_key.expose(), b"SubtitleAdder.StateSync.SealedEvent.v1");
    let open_payload = |e: &Value| -> Value {
        let aad = format!(
            "{}|{}|ProgressUpdated|{}",
            e["eventId"].as_str().unwrap(),
            e["topic"].as_str().unwrap(),
            sa_profiles::win::netcompat::upper_invariant(e["scopePath"].as_str().unwrap())
        );
        let sealed = base64::engine::general_purpose::STANDARD.decode(e["sealedPayloadBase64"].as_str().unwrap()).expect("b64");
        let plain = sa_profiles::crypto::open_raw(&sealed, &ipc_key, aad.as_bytes()).expect("payload opens");
        serde_json::from_slice(&plain).expect("payload json")
    };
    let (rust_payload, csharp_payload) = (open_payload(&same_values), open_payload(&csharp));
    assert_eq!(json_keys(&rust_payload), json_keys(&csharp_payload));
    assert_eq!(canon(&rust_payload), canon(&csharp_payload));
    assert_eq!(csharp_payload["videoPath"], json!(m.a));
    for e in [&same_values, &csharp] {
        let t = e["timestampUtc"].as_str().expect("timestamp");
        assert!(t.ends_with("+00:00") && sa_sync::wire::parse_timestamp(t).is_some(), "{t}");
    }

    // ... and another window (the C# app's stand-in) applies it after the wire round trip.
    let mine = s.create_broker_event_for_tests(
        sa_sync::Topic::WatchProgress,
        sa_sync::EventKind::ProgressUpdated,
        Some(&m.a),
        Some(42.25),
        Some(100.0),
        None,
    );
    let line = sa_sync::wire::event_line(&mine).expect("line");
    s.dispose();
    let bus2 = InProcessBus::new();
    let broker2: Arc<dyn ProfileWriteBroker> = bus2.clone();
    let other = WinProfileStore::open(data.join("profiles.json"), broker2);
    other.login_user("Emre", "pässwörd-1", false).expect("login other");
    let received = sa_sync::wire::parse_event(&line).expect("Rust event parses");
    sa_sync::SyncBus::publish(bus2.as_ref(), sa_sync::Topic::WatchProgress, received);
    assert_eq!(other.get_stats_fast_in_memory(&m.a).map(|v| v.last_position_seconds), Some(42.25), "Rust event applied");
    other.dispose();
}
