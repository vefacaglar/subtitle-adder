//! Data compatibility with the C# app (win-profiles §17 "format compatibility"): the C# `LocalProfileManager` /
//! `SqliteProfileStore` (compiled from apps/windows by `tools/fixtures/ProfileFixtures`) and the Rust
//! `WinProfileStore` write the same scenarios, and each side reads the other's `profiles.json` + `profiles.db`:
//!
//! * C# writes → Rust reads (passwords, DPAPI remember-me, history, likes, ratings, progress, lists, thumbnails);
//! * Rust writes on top of C# data → C# reads it (and the reverse: C# writes on top of Rust data);
//! * Rust writes from scratch → C# reads it (DPAPI remember-me written by Rust restores in C#);
//! * a version-2 header with legacy monolithic vaults migrates to the same data on both sides.
//!
//! Both sides describe what they read in the same canonical JSON ("dump"); the dumps must be equal. The fixture is
//! built with `dotnet build -c Release` on first use; without a .NET SDK the tests print SKIPPED and pass, unless
//! `SA_PROFILES_REQUIRE_CSHARP=1` is set.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use sa_profiles::win::WinProfileStore;
use sa_profiles::{ProfileStore, VideoFeedFilter, VideoUserStats};
use sa_sync::{InProcessBus, ProfileWriteBroker};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/ProfileFixtures")
}

/// Path of the built fixture DLL, building it once; `None` = skip.
fn fixture() -> Option<PathBuf> {
    static F: OnceLock<Option<PathBuf>> = OnceLock::new();
    F.get_or_init(|| {
        let dll = fixture_dir().join("bin/Release/net10.0/ProfileFixtures.dll");
        let built = Command::new("dotnet")
            .args(["build", "-c", "Release", "--nologo", "-v", "q"])
            .current_dir(fixture_dir())
            .env("TMP", scratch_temp())
            .env("TEMP", scratch_temp())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if built && dll.exists() {
            Some(dll)
        } else {
            assert!(std::env::var("SA_PROFILES_REQUIRE_CSHARP").is_err(), "C# profile fixture could not be built");
            eprintln!("SKIPPED: C# profile fixture not available (no .NET SDK?)");
            None
        }
    })
    .clone()
}

/// A private temp folder for the C# processes (broker files, diagnostic log), never the real `%TEMP%\SubtitleAdder`.
fn scratch_temp() -> PathBuf {
    static T: OnceLock<tempfile::TempDir> = OnceLock::new();
    T.get_or_init(|| tempfile::tempdir().expect("temp")).path().to_path_buf()
}

fn csharp(dll: &Path, args: &[&str]) -> String {
    let out = Command::new("dotnet")
        .arg(dll)
        .args(args)
        .env("TMP", scratch_temp())
        .env("TEMP", scratch_temp())
        .output()
        .expect("run dotnet");
    assert!(
        out.status.success(),
        "C# fixture {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

fn csharp_dump(dll: &Path, data: &Path, creds: &[(&str, &str)]) -> Value {
    let mut args: Vec<String> = vec!["dump".into(), data.to_string_lossy().into_owned()];
    for (u, p) in creds {
        args.push((*u).into());
        args.push((*p).into());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    serde_json::from_str(&csharp(dll, &refs)).expect("dump json")
}

// ---------------------------------------------------------------------------------------------------------------
// Shared scenario (mirrors Program.cs)

const A: &str = "Yaz Tatili Ç.mp4";
const B: &str = "missing_episode.mkv";
const C: &str = "belgesel-uzay.mp4";
const D: &str = "zeynep_klip.mp4";

/// Program.cs `Bytes`: xorshift32.
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

/// `<data>\..\media`, so a copied data folder keeps pointing at the same files.
fn media(data: &Path, create: bool) -> Media {
    let dir = data.parent().expect("parent").join("media");
    let p = |n: &str| dir.join(n).to_string_lossy().into_owned();
    let m = Media { a: p(A), b: p(B), c: p(C), d: p(D) };
    if create {
        std::fs::create_dir_all(&dir).expect("media dir");
        std::fs::write(&m.a, bytes(5000, 1)).expect("a");
        std::fs::write(&m.c, bytes(70000, 2)).expect("c");
        std::fs::write(&m.d, bytes(1234, 3)).expect("d");
    }
    m
}

fn open(data: &Path) -> Arc<WinProfileStore> {
    let bus = InProcessBus::new();
    let broker: Arc<dyn ProfileWriteBroker> = bus;
    WinProfileStore::open(data.join("profiles.json"), broker)
}

fn rust_generate(data: &Path) {
    std::fs::create_dir_all(data).expect("data dir");
    let m = media(data, true);
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
    let m = media(data, false);
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
// Rust dump (mirrors Program.cs `Dump`)

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

/// Copies `profiles.json`, `profiles.db` (+ WAL files) and the backup into a fresh folder next to `data`.
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

/// Dumps a copy with each side (a dump writes: it logs in, which clears a remembered session).
fn compare_both(dll: &Path, data: &Path, creds: &[(&str, &str)], what: &str, fp_upgrade: bool) -> (Value, Value) {
    let cs = csharp_dump(dll, &copy_data(data, "dump-cs"), creds);
    let rs = rust_dump(&copy_data(data, "dump-rs"), creds);
    assert_same(what, &cs, &rs, fp_upgrade);
    (cs, rs)
}

const CREDS: [(&str, &str); 2] = [("Emre", "pässwörd-1"), ("zeynep", "z-pass")];

fn header(data: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(data.join("profiles.json")).expect("header")).expect("header json")
}

// ---------------------------------------------------------------------------------------------------------------
// Tests

#[test]
fn csharp_written_profiles_read_identically_in_rust_and_rust_writes_on_top_read_back_in_csharp() {
    let Some(dll) = fixture() else { return };
    let root = tempfile::tempdir().expect("temp");
    let data = root.path().join("data");
    csharp(&dll, &["generate", &data.to_string_lossy()]);

    // C# → Rust: the remembered session (DPAPI, written by C#) restores in Rust too.
    let (cs, _) = compare_both(&dll, &data, &CREDS, "C# generate", false);
    assert_eq!(cs["restoredUser"], json!("zeynep"));
    assert_eq!(cs["profiles"]["Emre"]["lists"].as_array().map(Vec::len), Some(2));
    assert_eq!(cs["profiles"]["Emre"]["videos"][2]["viewCount"], json!(2));

    // Rust writes on the C# data → C# reads it (view-count deltas, HMAC list-item removal, renames, thumbnails).
    rust_mutate(&data);
    let (cs, _) = compare_both(&dll, &data, &CREDS, "C# generate + Rust mutate", false);
    let emre = &cs["profiles"]["Emre"];
    let a = emre["videos"].as_array().unwrap().iter().find(|v| v["fileName"] == json!(A)).expect("a");
    assert_eq!(a["viewCount"], json!(3));
    assert_eq!(a["isLiked"], json!(false));
    let names: Vec<&str> = emre["lists"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Uzay Belgeselleri") && names.contains(&"Yeni Liste"), "{names:?}");
    assert_eq!(cs["restoredUser"], json!("zeynep"), "Rust re-wrote the remembered session C# can unwrap");
    assert_eq!(cs["restored"]["totals"]["watched"], json!(0), "history cleared by Rust");
}

#[test]
fn rust_written_profiles_read_identically_in_csharp_and_csharp_writes_on_top_read_back_in_rust() {
    let Some(dll) = fixture() else { return };
    let root = tempfile::tempdir().expect("temp");
    let data = root.path().join("data");
    rust_generate(&data);

    let h = header(&data);
    assert_eq!(h["version"], json!(3));
    assert_eq!(h["rememberMeOptIn"], json!(true));
    assert!(h["rememberedProtectedKeyBase64"].as_str().unwrap().starts_with("dpapi1:"));

    // Rust → C#: the DPAPI remember-me written by Rust restores in C#.
    let (cs, _) = compare_both(&dll, &data, &CREDS, "Rust generate", false);
    assert_eq!(cs["restoredUser"], json!("zeynep"));
    assert_eq!(cs["restored"]["totals"], json!({"watched": 1, "views": 1, "liked": 0, "rated": 1}));

    // C# writes on the Rust data → Rust reads it.
    csharp(&dll, &["mutate", &data.to_string_lossy()]);
    compare_both(&dll, &data, &CREDS, "Rust generate + C# mutate", false);
}

#[test]
fn both_apps_write_equivalent_data_for_the_same_scenario() {
    let Some(dll) = fixture() else { return };
    let root_cs = tempfile::tempdir().expect("temp");
    let root_rs = tempfile::tempdir().expect("temp");
    let data_cs = root_cs.path().join("data");
    let data_rs = root_rs.path().join("data");
    csharp(&dll, &["generate", &data_cs.to_string_lossy()]);
    rust_generate(&data_rs);
    let cs = csharp_dump(&dll, &copy_data(&data_cs, "dump"), &CREDS);
    let rs = csharp_dump(&dll, &copy_data(&data_rs, "dump"), &CREDS);
    // Everything except paths (different roots), times and list ids must match.
    fn strip(v: &Value, root: &str) -> Value {
        match v {
            Value::String(s) => Value::String(s.replace(root, "<root>")),
            Value::Array(a) => Value::Array(a.iter().map(|x| strip(x, root)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .filter(|(k, _)| !k.ends_with("Ticks") && k.as_str() != "id")
                    .map(|(k, v)| (k.clone(), strip(v, root)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    let mut a = strip(&canon(&cs), &root_cs.path().to_string_lossy());
    let mut b = strip(&canon(&rs), &root_rs.path().to_string_lossy());
    // List order follows the random list ids.
    for v in [&mut a, &mut b] {
        if let Some(l) = v["profiles"]["Emre"]["lists"].as_array_mut() {
            l.sort_by_key(|x| x["name"].as_str().unwrap_or("").to_owned());
        }
    }
    assert_eq!(a, b);
}

#[test]
fn legacy_version_2_vaults_migrate_to_the_same_data_on_both_sides() {
    let Some(dll) = fixture() else { return };
    let root = tempfile::tempdir().expect("temp");
    let data = root.path().join("data");
    csharp(&dll, &["legacy", &data.to_string_lossy()]);
    let creds = [("eski", "eski-şifre"), ("bos", "bos-pw")];

    let cs_dir = copy_data(&data, "legacy-cs");
    let rs_dir = copy_data(&data, "legacy-rs");
    let cs = csharp_dump(&dll, &cs_dir, &creds);
    let rs = rust_dump(&rs_dir, &creds);
    assert_same("legacy migration", &cs, &rs, true);
    assert_eq!(rs["restoredUser"], Value::Null, "a pre-opt-in remembered session is dropped");
    assert_eq!(rs["lastUsername"], json!("eski"));
    let eski = &rs["profiles"]["eski"];
    assert_eq!(eski["lists"][0]["name"], json!("Eski Liste"));
    assert_eq!(eski["lists"][0]["videoPaths"].as_array().map(Vec::len), Some(2), "deduplicated, blank dropped");

    // Rust migrated: the header carries markers, a backup exists without a remembered key, and C# reads the result.
    let h = header(&rs_dir);
    assert_eq!(h["version"], json!(3));
    for p in h["profiles"].as_array().unwrap() {
        assert_eq!(p["encryptedVaultBase64"].as_str().map(str::len), Some(68), "vault marker");
    }
    assert!(h.get("rememberedProtectedKeyBase64").is_none() && h["rememberMe"] == json!(false));
    let bak = std::fs::read_to_string(rs_dir.join("profiles.json.bak")).expect("backup");
    assert!(!bak.contains("rememberedProtectedKeyBase64"));
    let (again, _) = compare_both(&dll, &rs_dir, &creds, "C# reading the Rust migration", false);
    assert_eq!(again["lastUsername"], json!("bos"), "the Rust dump logged in eski, then bos");
    assert_same("the migrated profiles", &again["profiles"], &rs["profiles"], false);
}

#[test]
fn csharp_fingerprints_match_rust_for_overlapping_slices() {
    let Some(dll) = fixture() else { return };
    let root = tempfile::tempdir().expect("temp");
    for (len, seed) in [(5usize, 7u32), (65_536, 8), (70_000, 9), (131_073, 10), (200_000, 11)] {
        let p = root.path().join(format!("f{len}.bin"));
        std::fs::write(&p, bytes(len, seed)).expect("write");
        let cs = csharp(&dll, &["fingerprint", &p.to_string_lossy()]);
        assert_eq!(sa_profiles::fingerprint::compute(&p).as_deref(), Some(cs.as_str()), "len {len}");
    }
}

#[test]
fn csharp_crypto_vectors_match_rust() {
    let Some(dll) = fixture() else { return };
    let v: Value = serde_json::from_str(&csharp(&dll, &["vectors"])).expect("vectors json");
    let s = |k: &str| v[k].as_str().unwrap_or_else(|| panic!("{k}")).to_owned();
    let unhex = |h: &str| (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
    let b64 = base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

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
    assert_eq!(v["vaultMarkerPlainLength"], json!(sa_profiles::crypto::VAULT_MARKER.len()));
    assert_eq!(v["pad300"], json!(sa_profiles::win::pad_to_bucket(vec![0; 300], 256, b' ').len()));
    assert_eq!(v["pad1025"], json!(sa_profiles::win::pad_to_bucket(vec![0; 1025], 1024, b' ').len()));
}

#[test]
fn sealed_broker_events_cross_between_the_csharp_and_rust_apps() {
    // A C# window and a Rust window on one broker channel: watch-progress events sealed by either side open on the
    // other (same IPC key derivation and AAD), so live sync keeps working while both apps run.
    let Some(dll) = fixture() else { return };
    let root = tempfile::tempdir().expect("temp");
    let data = root.path().join("data");
    rust_generate(&data);
    let m = media(&data, false);

    let evt_json = csharp(&dll, &["seal-event", &data.to_string_lossy(), "Emre", "pässwörd-1", &m.a]);
    let evt = sa_sync::wire::parse_event(&evt_json).expect("C# event parses");
    assert!(evt.video_path.is_none() && evt.position_seconds.is_none() && evt.sealed_payload_base64.is_some());
    let bus = InProcessBus::new();
    let broker: Arc<dyn ProfileWriteBroker> = bus.clone();
    let s = WinProfileStore::open(data.join("profiles.json"), broker);
    s.login_user("Emre", "pässwörd-1", false).expect("login");
    assert_eq!(s.get_stats_fast_in_memory(&m.a).map(|v| v.last_position_seconds), Some(55.0));
    sa_sync::SyncBus::publish(bus.as_ref(), sa_sync::Topic::WatchProgress, evt);
    assert_eq!(s.get_stats_fast_in_memory(&m.a).map(|v| v.last_position_seconds), Some(61.5), "C# event applied in Rust");

    let mine = s.create_broker_event_for_tests(
        sa_sync::Topic::WatchProgress,
        sa_sync::EventKind::ProgressUpdated,
        Some(&m.a),
        Some(42.25),
        Some(100.0),
        None,
    );
    let file = root.path().join("event.json");
    std::fs::write(&file, sa_sync::wire::event_line(&mine).expect("line")).expect("event file");
    s.dispose();
    let applied = csharp(&dll, &["apply-event", &data.to_string_lossy(), "Emre", "pässwörd-1", &file.to_string_lossy(), &m.a]);
    assert_eq!(applied, "42.25", "Rust event applied in C#");
}
