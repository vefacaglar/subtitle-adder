//! The end-to-end data folder written by the C# app's own classes (frozen in `tests/fixtures/csharp/e2e-data`, see
//! its README) read and rewritten by the Rust stores the app composes (`app::win_parts`). `scripts/e2e-windows.ps1`
//! runs the real app on a copy of the same folder; these tests pin what it relies on without a UI:
//!
//! * C# → Rust: the profile "Emre" (history, likes, ratings, thumbnails, a list), settings.json, media-index.json
//!   and the plaintext API-key provider read back as the C# app wrote them, and the Rust reader prints exactly what
//!   the C# reader (`E2EData dump`, frozen as `csharp-dump.txt`) printed.
//! * Rust → C#: data the Rust stores rewrote (a login/logout cycle, the provider key moved into the DPAPI sidecar)
//!   keeps the C# file shapes and reads back through Rust like the frozen C# data.
#![cfg(windows)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sa_core::secrets::SecretString;
use sa_core::settings::{JsonFileSettingsStore, SettingsStore};
use sa_media::win::WinMediaIndex;
use sa_profiles::win::WinProfileStore;
use sa_profiles::{FeedQuery, ProfileStore, VideoFeedFilter, VideoUserStats};
use sa_providers::win::WinProviderManager;
use sa_providers::{ProviderKeyRead, ProviderStore};
use sa_sync::{InProcessBus, ProfileWriteBroker};
use serde_json::Value;
use sha2::{Digest, Sha256};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/csharp/e2e-data");
/// The folder the clips were in when the C# tool wrote the data (`e2e-data.ps1 -Root C:\SubtitleAdderE2E\template`).
const RECORDED_MEDIA: &str = r"C:\SubtitleAdderE2E\template\media";
const USER: &str = "Emre";
const PASSWORD: &str = "e2e-parola-1";
const PROVIDER_NAME: &str = "C# anahtarlı sağlayıcı";
const PROVIDER_KEY: &str = "sk-e2e-csharp-0123456789abcdef";
const FILES: [&str; 5] = ["profiles.json", "profiles.db", "settings.json", "media-index.json", "translation-providers.json"];

/// A private copy of the frozen data folder (the fixture itself is never written).
fn copy_data() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("SubtitleAdder");
    std::fs::create_dir_all(&data).unwrap();
    for f in FILES {
        std::fs::copy(Path::new(FIXTURE).join("data").join(f), data.join(f)).unwrap();
    }
    (dir, data)
}

fn fixture_text(name: &str) -> String {
    std::fs::read_to_string(Path::new(FIXTURE).join(name)).unwrap()
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture_text(&format!("data/{name}"))).unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object().expect("object").keys().cloned().collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// `media.sha256`: relative path (forward slashes) → SHA-256 of the clips and thumbnails the C# tool was given.
fn media_hash(rel: &str) -> String {
    fixture_text("media.sha256")
        .lines()
        .filter_map(|l| l.split_once("  "))
        .find(|(_, name)| *name == rel)
        .map(|(h, _)| h.to_owned())
        .unwrap_or_else(|| panic!("{rel} not in media.sha256"))
}

fn broker() -> Arc<dyn ProfileWriteBroker> {
    InProcessBus::with_scope(&format!("e2e-data-test-{}", sa_core::text::guid_n()))
}

fn open_logged_in(data: &Path) -> Arc<WinProfileStore> {
    let store = WinProfileStore::open(data.join("profiles.json"), broker());
    store.login(USER, &SecretString::new(PASSWORD), false).expect("the C# password unlocks the C# profile");
    store
}

fn close(store: &WinProfileStore) {
    let _ = store.logout();
    assert!(store.flush_pending_writes(Duration::from_secs(10)), "profile writes flushed");
    store.dispose();
}

fn feed(store: &WinProfileStore, filter: VideoFeedFilter) -> Vec<VideoUserStats> {
    store.feed(&FeedQuery { filter, ..FeedQuery::default() }, &|| true)
}

/// The lines the `win_profile_dump` example (the reader `scripts/e2e-windows.ps1` runs) prints: the same function.
fn dump(store: &WinProfileStore) -> Vec<String> {
    sa_profiles::win::e2e_dump_lines(store)
}

fn csharp_dump() -> Vec<String> {
    fixture_text("csharp-dump.txt").lines().map(|l| l.trim_end_matches('\r').to_owned()).collect()
}

fn recorded(name: &str) -> String {
    format!(r"{RECORDED_MEDIA}\{name}")
}

#[test]
fn the_rust_reader_prints_exactly_what_the_csharp_reader_printed() {
    let (_dir, data) = copy_data();
    let store = open_logged_in(&data);
    assert_eq!(dump(&store), csharp_dump());
    close(&store);
}

#[test]
fn the_csharp_profile_reads_back_with_engagement_thumbnails_and_the_list() {
    let (_dir, data) = copy_data();
    let store = open_logged_in(&data);

    let all = feed(&store, VideoFeedFilter::All);
    let paths: BTreeSet<&str> = all.iter().map(|v| v.video_path.as_str()).collect();
    let expected: BTreeSet<String> = ["altyazi-testi.mp4", "belgesel-uzay.mp4", "yaz-tatili.mp4"].map(recorded).into();
    assert_eq!(paths, expected.iter().map(String::as_str).collect());
    for v in &all {
        let fp = v.content_fingerprint.as_deref().unwrap_or_default();
        assert!(fp.starts_with("fp:"), "{}: C# keyed the row by its content fingerprint, got {fp:?}", v.file_name);
        assert_eq!(v.directory_path, RECORDED_MEDIA);
    }

    let by_name = |name: &str| all.iter().find(|v| v.file_name == name).unwrap();
    let first = by_name("altyazi-testi.mp4");
    assert_eq!((first.view_count, first.is_liked, first.rating), (2, true, 5));
    assert_eq!((first.last_position_seconds, first.duration_seconds), (6.0, 8.0), "UpdatePlaybackProgress(6, 8)");
    assert!(first.thumbnail.is_none(), "no thumbnail was given for the TTS clip");
    let second = by_name("belgesel-uzay.mp4");
    assert_eq!((second.view_count, second.is_liked, second.rating), (1, false, 3));
    let third = by_name("yaz-tatili.mp4");
    assert_eq!((third.view_count, third.is_liked, third.rating), (0, false, 0));
    // The thumbnails are the PNG bytes the C# app was given, stored verbatim (encrypted) in profiles.db.
    for (v, rel) in [(second, "thumbs/belgesel-uzay.png"), (third, "thumbs/yaz-tatili.png")] {
        let png = v.thumbnail.as_deref().unwrap_or_else(|| panic!("{}: thumbnail", v.file_name));
        assert_eq!(sha256_hex(png), media_hash(rel), "{}", v.file_name);
    }

    // The hub tab counts the e2e clicks: "İzleme geçmişi (2)", "Favorilerim (1)".
    assert_eq!(feed(&store, VideoFeedFilter::History).len(), 2);
    let liked: Vec<String> = feed(&store, VideoFeedFilter::Liked).into_iter().map(|v| v.file_name).collect();
    assert_eq!(liked, ["altyazi-testi.mp4"]);
    let rated: Vec<String> = feed(&store, VideoFeedFilter::TopRated).into_iter().map(|v| v.file_name).collect();
    assert_eq!(rated, ["altyazi-testi.mp4", "belgesel-uzay.mp4"]);

    let lists = store.lists();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].name, "İzlenecekler");
    assert_eq!(lists[0].video_paths, [recorded("belgesel-uzay.mp4")]);
    let listed: Vec<String> = feed(&store, VideoFeedFilter::CustomList(Some(lists[0].id.clone()))).into_iter().map(|v| v.file_name).collect();
    assert_eq!(listed, ["belgesel-uzay.mp4"]);
    close(&store);
}

#[test]
fn a_rust_login_logout_cycle_keeps_the_csharp_profile_file_and_data() {
    let (_dir, data) = copy_data();
    let store = open_logged_in(&data);
    close(&store);

    // profiles.json after the Rust cycle: the C# v3 shape, and the account material is untouched.
    let csharp = fixture_json("profiles.json");
    let rust = read_json(&data.join("profiles.json"));
    assert_eq!(keys(&rust), keys(&csharp));
    assert_eq!(rust["version"], csharp["version"]);
    assert_eq!(rust["rememberMe"], Value::Bool(false));
    assert_eq!(rust["lastUsername"], csharp["lastUsername"]);
    let (rp, cp) = (rust["profiles"].as_array().unwrap(), csharp["profiles"].as_array().unwrap());
    assert_eq!(rp.len(), cp.len());
    assert_eq!(keys(&rp[0]), keys(&cp[0]));
    for field in ["username", "saltBase64", "passwordHashBase64", "iterations", "createdAtUtc", "encryptedVaultBase64"] {
        assert_eq!(rp[0][field], cp[0][field], "{field}");
    }

    // A fresh reader (new instance, new broker scope) still sees exactly the C# data.
    let again = open_logged_in(&data);
    assert_eq!(dump(&again), csharp_dump());
    close(&again);
}

#[test]
fn the_csharp_settings_read_back() {
    let (_dir, data) = copy_data();
    let settings = JsonFileSettingsStore::open(data.join("settings.json"), Duration::from_millis(1), None);
    for (key, value) in [("fileAssociationsDisabled", true), ("autoPlayVideo", false), ("showFileExplorer", false)] {
        assert_eq!(settings.get_bool(key), Some(value), "{key}");
    }
    for (key, value) in [
        ("uiLanguage", "tr"),
        ("selectedLanguage", "tr"),
        ("selectedModel", "base"),
        ("translationModel", "madlad-3b"),
        ("translationTarget", "en"),
        ("appTheme", "light"),
    ] {
        assert_eq!(settings.get_string(key).as_deref(), Some(value), "{key}");
    }
    assert_eq!(keys(&fixture_json("settings.json")).len(), 9, "the fixture holds exactly the values checked here");
    settings.dispose();
}

#[test]
fn the_csharp_media_index_reads_back() {
    let (_dir, data) = copy_data();
    let index = WinMediaIndex::new(Some(WinMediaIndex::default_path(&data)), None);
    assert_eq!(index.indexed_directories(), [RECORDED_MEDIA]);
    assert_eq!(index.cached_subdirectories(RECORDED_MEDIA), [format!(r"{RECORDED_MEDIA}\thumbs")]);

    let csharp = fixture_json("media-index.json");
    let entries = csharp["directories"][0]["mediaFiles"].as_array().unwrap();
    let files = index.cached_directory(RECORDED_MEDIA);
    assert_eq!(files.len(), entries.len());
    for (f, e) in files.iter().zip(entries) {
        assert_eq!(f.full_path, e["fullPath"].as_str().unwrap());
        assert_eq!(f.file_name, e["fileName"].as_str().unwrap());
        assert_eq!(f.directory_path, RECORDED_MEDIA);
        assert_eq!(f.size_bytes, e["sizeBytes"].as_i64().unwrap());
        assert!(!f.has_sidecar_subtitle && f.sidecar_extension.is_none());
        assert_eq!(f.indexed_at_utc, 0, "the Windows app writes 0001-01-01");
    }
    let found: Vec<String> = index.search_files("uzay", 10).into_iter().map(|f| f.file_name).collect();
    assert_eq!(found, ["belgesel-uzay.mp4"]);
    // Nothing on disk changed just by reading it.
    assert_eq!(std::fs::read(WinMediaIndex::default_path(&data)).unwrap(), std::fs::read(Path::new(FIXTURE).join("data/media-index.json")).unwrap());
}

#[test]
fn the_csharp_plaintext_provider_key_moves_into_the_sidecar_and_keeps_the_csharp_shape() {
    let (_dir, data) = copy_data();
    let csharp = fixture_json("translation-providers.json");
    let csharp_provider = &csharp["providers"][0];
    assert_eq!(csharp_provider["apiKey"], PROVIDER_KEY, "the C# app keeps keys in plaintext");

    let providers = WinProviderManager::open(&data, None);
    let list = providers.providers();
    assert_eq!(list.len(), 1);
    let p = &list[0];
    assert_eq!(p.id.to_string(), csharp_provider["id"].as_str().unwrap());
    assert_eq!(p.name, PROVIDER_NAME);
    assert_eq!(p.preset_id, "custom");
    assert_eq!(p.base_url, "https://127.0.0.1:9/v1");
    assert_eq!(p.model, "e2e-model");
    assert!(!p.async_translation && p.discovered_models.is_empty());
    assert_eq!(providers.active_provider_id(), None);
    match providers.read_key(p.id) {
        ProviderKeyRead::Found(k) => assert_eq!(k.expose(), PROVIDER_KEY),
        other => panic!("key: {other:?}"),
    }
    let id = p.id;
    drop(providers);

    // The file the Rust store rewrote: no plaintext key anywhere in the folder, same C# shape.
    let text = std::fs::read_to_string(data.join("translation-providers.json")).unwrap();
    assert!(!text.contains(PROVIDER_KEY), "plaintext key left in translation-providers.json");
    for entry in std::fs::read_dir(&data).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(PROVIDER_KEY.len()).any(|w| w == PROVIDER_KEY.as_bytes()), "plaintext key in {}", path.display());
    }
    let rust: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(keys(&rust), keys(&csharp));
    assert_eq!(rust["activeProviderId"], csharp["activeProviderId"]);
    let rust_provider = &rust["providers"][0];
    assert_eq!(keys(rust_provider), keys(csharp_provider));
    for field in ["id", "name", "presetId", "baseUrl", "model", "asyncTranslation", "discoveredModels"] {
        assert_eq!(rust_provider[field], csharp_provider[field], "{field}");
    }
    assert_eq!(rust_provider["apiKey"], "");

    // After a restart the key still decrypts from the sidecar (e2e step 8).
    let reopened = WinProviderManager::open(&data, None);
    match reopened.read_key(id) {
        ProviderKeyRead::Found(k) => assert_eq!(k.expose(), PROVIDER_KEY),
        other => panic!("key after reopen: {other:?}"),
    }
}
