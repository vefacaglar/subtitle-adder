//! Cross-implementation check of the shared version-2 format (no Swift toolchain here, so the C# app stands in for a
//! second, independent implementation of it): a `profiles.json` v2 with monolithic encrypted vaults that the C# app
//! wrote through .NET's own PBKDF2, AES-GCM, `System.Text.Json` and `DateTimeOffset.ToString("O")` (frozen in
//! `tests/fixtures/csharp/legacy/data`, see the README there); the macOS backend must log in, repair and serve it
//! exactly as LibraryKit would (mac-librarykit §5, §6, §8.1, §11), and what the macOS backend writes back must read
//! the same through the Windows backend (the Rust port of the C# reader, whose reading of C# files is pinned by
//! `tests/win_fixtures.rs`).
//!
//! Windows only: the C# file names its media by absolute Windows paths under [`FIXTURE_ROOT`], recreated here.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;

use super::*;
use crate::mac::{fingerprints, thumbnails};
use crate::{FingerprintMode, ProfileStore, VideoFeedFilter};

/// Where the C# fixture kept its media (the same root as `tests/win_fixtures.rs`).
const FIXTURE_ROOT: &str = r"C:\ProgramData\SubtitleAdderTestFixtures\profiles";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/csharp")
}

/// Writes `content` to `path` unless it is already there (atomically: parallel test processes share the folder).
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

/// The media folder the C# file refers to (`Yaz Tatili Ç.mp4` and `belgesel-uzay.mp4` exist,
/// `missing_episode.mkv` never does).
fn media() -> PathBuf {
    static M: OnceLock<()> = OnceLock::new();
    let dir = Path::new(FIXTURE_ROOT).join("media");
    M.get_or_init(|| {
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("media folder {dir:?}: {e}"));
        ensure_file(&dir.join("Yaz Tatili Ç.mp4"), &bytes(5000, 1));
        ensure_file(&dir.join("belgesel-uzay.mp4"), &bytes(70000, 2));
        ensure_file(&dir.join("zeynep_klip.mp4"), &bytes(1234, 3));
        assert!(!dir.join("missing_episode.mkv").exists());
    });
    dir
}

/// What the Windows backend reads from `data` for each login: `(registered, restored user, per-user videos, lists)`.
fn windows_reader_dump(data: &Path, creds: &[(&str, &str)]) -> Value {
    let bus = sa_sync::InProcessBus::new();
    let broker: Arc<dyn sa_sync::ProfileWriteBroker> = bus;
    let s = crate::win::WinProfileStore::open(data.join("profiles.json"), broker);
    let mut profiles = serde_json::Map::new();
    let restored = s.current_username();
    for (u, p) in creds {
        s.login_user(u, p, false).unwrap_or_else(|e| panic!("login {u}: {e:?}"));
        let never = || true;
        let videos: Vec<Value> = s
            .get_feed(&VideoFeedFilter::All, None, None, &never)
            .iter()
            .map(|v| {
                serde_json::json!({
                    "fileName": v.file_name,
                    "contentFingerprint": v.content_fingerprint,
                    "viewCount": v.view_count,
                    "isLiked": v.is_liked,
                    "rating": v.rating,
                    "lastPositionSeconds": v.last_position_seconds,
                    "durationSeconds": v.duration_seconds,
                    "lastWatchedTicks": v.last_watched_at.map(|t| t.0),
                })
            })
            .collect();
        let lists: Vec<Value> = s
            .get_custom_lists()
            .iter()
            .map(|l| {
                serde_json::json!({
                    "id": l.id, "name": l.name, "videoPaths": l.video_paths, "createdTicks": l.created_at.0,
                })
            })
            .collect();
        profiles.insert((*u).into(), serde_json::json!({ "videos": videos, "lists": lists }));
    }
    assert!(s.flush_pending_writes(Duration::from_secs(10)), "flush");
    let dump = serde_json::json!({ "registered": s.registered_usernames(), "restoredUser": restored, "profiles": profiles });
    s.dispose();
    dump
}

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

fn mac_store(data: &Path, cache: &Arc<fingerprints::FingerprintCache>) -> Arc<MacProfileStore> {
    MacProfileStore::with_extras(
        MacProfileStoreOptions {
            file: Some(data.join("profiles.json")),
            watches_file: false,
            lock_timeout: Duration::from_secs(5),
            thumbnails_folder: None,
            reads_file_immediately: false,
        },
        TestSecrets::new(),
        StoreExtras { fingerprints: Some(cache.clone()), ..StoreExtras::default() },
    )
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

const MINUTE: i64 = 600_000_000;

#[test]
fn a_version_2_file_written_by_dotnet_opens_repairs_and_round_trips() {
    let folder = TestFolder::new();
    let data = folder.url.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::copy(fixtures().join("legacy/data/profiles.json"), data.join("profiles.json")).expect("C# legacy file");
    let media = media();
    let (a, c) = (media.join("Yaz Tatili Ç.mp4"), media.join("belgesel-uzay.mp4"));
    let t0 = dates::parse("2025-03-04T05:06:07.1234567Z").unwrap();
    let fp_a = crate::fingerprint::compute(&a).expect("fingerprint of a");

    // The C# file names a pre-ticked remembered login, but this machine's secret store holds no key for it.
    let store = mac_store(&data, &folder.cache);
    store.start();
    assert!(!store.is_logged_in());
    assert_eq!(store.remembered_username().as_deref(), Some("eski"));
    assert_eq!(store.last_username().as_deref(), Some("eski"));
    assert_eq!(store.registered_usernames(), ["bos", "eski"]);
    assert_eq!(store.try_create_list("x").unwrap(), None, "logged out: no list");

    // .NET PBKDF2 + AES-GCM + System.Text.Json, read by the LibraryKit rules.
    assert_eq!(store.login_user("eski", "wrong", false), Err(crate::ProfileError::InvalidCredentials));
    store.login_user("ESKI", "eski-şifre", false).expect("login with .NET-derived keys");
    assert_eq!(store.current_username().as_deref(), Some("eski"));

    let history = feed(&store, VideoFeedFilter::History);
    assert_eq!(history.len(), 2, "{history:?}");
    let first = &history[0];
    assert_eq!(first.content_fingerprint.as_deref(), Some(fp_a.as_str()), "C# and Rust fingerprints agree");
    assert_eq!((first.view_count, first.is_liked, first.rating), (3, true, 5), "rating 9 is clamped to 5");
    assert_eq!((first.last_position_seconds, first.duration_seconds), (33.25, 100.0));
    assert_eq!(first.last_watched_at, Some(Ticks(t0.0 + 5 * MINUTE)), "7-digit DateTimeOffset kept exactly");
    assert_eq!(first.updated_at, Ticks(t0.0 + 6 * MINUTE));
    assert_eq!(history[1].file_name, "belgesel-uzay.mp4");
    assert_eq!(history[1].last_watched_at, Some(Ticks(t0.0 + 2 * MINUTE)));
    let rated = feed(&store, VideoFeedFilter::TopRated);
    assert_eq!(rated.iter().map(|v| (v.rating, v.view_count)).collect::<Vec<_>>(), [(5, 3), (2, 0)], "negative views clamped");

    let lists = store.all_lists();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].id, "0123456789abcdef0123456789abcdef");
    assert_eq!(lists[0].name, "Eski Liste", "name trimmed");
    assert_eq!(lists[0].video_paths.len(), 2, "case-insensitive duplicate and blank entry dropped");
    assert_eq!(lists[0].created_at, t0);
    assert_eq!(lists[0].updated_at, Ticks(t0.0 + 3 * MINUTE));

    // A write: the vault is re-sealed, its pictures move to the encrypted cache, no path is probed.
    let created = store.try_create_list("Yeni Liste").unwrap().expect("list");
    let raw = std::fs::read_to_string(data.join("profiles.json")).unwrap();
    assert!(!raw.contains("Yeni Liste") && !raw.contains("Yaz Tatili"), "history stays encrypted");
    let root: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(root["version"], 2);
    assert!(root.get("rememberedUsername").is_none() && root["rememberMe"] == false);
    store.logout_user().unwrap();

    // The C# reader (its Rust port) reads what the macOS backend wrote (on a copy: it migrates the folder to
    // version 3).
    let cs_data = folder.url.join("cs-data");
    copy_dir(&data, &cs_data);
    std::fs::remove_dir_all(cs_data.join("thumbnails")).ok();
    let dump = windows_reader_dump(&cs_data, &[("eski", "eski-şifre"), ("bos", "bos-pw")]);
    assert_eq!(dump["registered"], serde_json::json!(["bos", "eski"]));
    assert_eq!(dump["restoredUser"], Value::Null);
    let eski = &dump["profiles"]["eski"];
    let videos = eski["videos"].as_array().unwrap();
    let video_a = videos.iter().find(|v| v["contentFingerprint"] == fp_a.as_str()).expect("a read by the C# reader");
    assert_eq!(video_a["viewCount"], 3);
    assert_eq!(video_a["isLiked"], true);
    assert_eq!(video_a["rating"], 5);
    assert_eq!(video_a["lastPositionSeconds"], 33.25);
    assert_eq!(video_a["durationSeconds"], 100.0);
    assert_eq!(video_a["lastWatchedTicks"], t0.0 + 5 * MINUTE);
    let video_c = videos.iter().find(|v| v["fileName"] == "belgesel-uzay.mp4").expect("c read by the C# reader");
    assert_eq!(video_c["viewCount"], 1);
    assert_eq!(video_c["lastWatchedTicks"], t0.0 + 2 * MINUTE);
    let cs_lists = eski["lists"].as_array().unwrap();
    let names: Vec<&str> = cs_lists.iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Eski Liste", "Yeni Liste"]);
    assert_eq!(cs_lists[1]["id"], created.id.as_str());
    assert_eq!(cs_lists[1]["createdTicks"], created.created_at.0);
    assert_eq!(cs_lists[0]["videoPaths"].as_array().unwrap().len(), 2);
    assert!(dump["profiles"]["bos"]["videos"].as_array().unwrap().is_empty());

    // The pictures the C# vault carried are now served from the encrypted cache (keyed by the C# fingerprint).
    let again = mac_store(&data, &folder.cache);
    again.login_user("eski", "eski-şifre", false).unwrap();
    assert_eq!(again.thumbnail_mode(&a, FingerprintMode::Compute), Some(bytes(900, 6)));
    assert_eq!(again.thumbnail_mode(&c, FingerprintMode::Compute), None);
    let profile_folder = data.join("thumbnails").join(thumbnails::ThumbnailCache::folder_name("eski"));
    let mut cached: Vec<String> =
        std::fs::read_dir(&profile_folder).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    cached.sort();
    let key = again.active_vault_key().unwrap().1;
    let mut expected = vec![
        thumbnails::ThumbnailCache::file_name(&fp_a, &key),
        thumbnails::ThumbnailCache::file_name(&media.join("missing_episode.mkv").to_string_lossy().replace('/', "\\"), &key),
        // LibraryKit keeps a blank picture key as it is (only an empty or undecodable image is dropped).
        thumbnails::ThumbnailCache::file_name("  ", &key),
    ];
    expected.sort();
    assert_eq!(cached, expected, "every picture the vault carried moved to its keyed cache file");
}
