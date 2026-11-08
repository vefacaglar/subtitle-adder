//! Cross-implementation check of the shared version-2 format (no Swift toolchain here, so the C# app stands in for a
//! second, independent implementation of it): the C# fixture (`tools/fixtures/ProfileFixtures legacy`) writes a
//! `profiles.json` v2 with monolithic encrypted vaults through .NET's own PBKDF2, AES-GCM, `System.Text.Json` and
//! `DateTimeOffset.ToString("O")`; the macOS backend must log in, repair and serve it exactly as LibraryKit would
//! (mac-librarykit §5, §6, §8.1, §11), and the C# reader must read back what the macOS backend writes.
//!
//! Windows only (the fixture is .NET on this machine); without a .NET SDK the test prints SKIPPED and passes unless
//! `SA_PROFILES_REQUIRE_CSHARP=1` is set.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;

use super::*;
use crate::mac::{fingerprints, thumbnails};
use crate::{FingerprintMode, ProfileStore, VideoFeedFilter};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/ProfileFixtures")
}

/// A private temp folder for the C# processes (broker files, diagnostic log), never the real `%TEMP%\SubtitleAdder`.
fn scratch_temp() -> PathBuf {
    static T: OnceLock<tempfile::TempDir> = OnceLock::new();
    T.get_or_init(|| tempfile::tempdir().expect("temp")).path().to_path_buf()
}

/// The built fixture DLL (built once); `None` = skip.
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
    let Some(dll) = fixture() else { return };
    let folder = TestFolder::new();
    let data = folder.url.join("data");
    csharp(&dll, &["legacy", &data.to_string_lossy()]);
    let media = folder.url.join("media");
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

    // C# reads what the macOS backend wrote (on a copy: the C# app migrates the folder to version 3).
    let cs_data = folder.url.join("cs-data");
    copy_dir(&data, &cs_data);
    std::fs::remove_dir_all(cs_data.join("thumbnails")).ok();
    let dump: Value =
        serde_json::from_str(&csharp(&dll, &["dump", &cs_data.to_string_lossy(), "eski", "eski-şifre", "bos", "bos-pw"]))
            .expect("dump json");
    assert_eq!(dump["registered"], serde_json::json!(["bos", "eski"]));
    assert_eq!(dump["restoredUser"], Value::Null);
    let eski = &dump["profiles"]["eski"];
    let videos = eski["videos"].as_array().unwrap();
    let video_a = videos.iter().find(|v| v["contentFingerprint"] == fp_a.as_str()).expect("a read by C#");
    assert_eq!(video_a["viewCount"], 3);
    assert_eq!(video_a["isLiked"], true);
    assert_eq!(video_a["rating"], 5);
    assert_eq!(video_a["lastPositionSeconds"], 33.25);
    assert_eq!(video_a["durationSeconds"], 100.0);
    assert_eq!(video_a["lastWatchedTicks"], t0.0 + 5 * MINUTE);
    let video_c = videos.iter().find(|v| v["fileName"] == "belgesel-uzay.mp4").expect("c read by C#");
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
