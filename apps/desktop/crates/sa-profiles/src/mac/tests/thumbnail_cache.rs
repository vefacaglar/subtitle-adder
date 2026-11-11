//! `ThumbnailCacheTests` (store-level cases; the cache-only ones live in `mac::thumbnails`).

use base64::Engine as _;
use serde_json::Value;

use super::file_format::hand_sealed;
use super::*;
use crate::mac::auth::derive_checked;
use crate::mac::fileio::signature;
use crate::mac::thumbnails::ThumbnailCache;
use crate::{FingerprintMode, ProfileCounts, ProfileStore, VideoFeedFilter as F};

fn picture() -> Vec<u8> {
    content(2000, 251, 0)
}

#[test]
fn storing_pictures_never_rewrites_the_vault_or_creates_records() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    let before = folder.file_data();
    let sig = signature(&folder.profiles_file());
    let mut videos = Vec::new();
    for index in 0..40 {
        let video = folder.video(&format!("v{index}.mp4"));
        std::fs::write(&video, format!("content {index}")).unwrap();
        assert!(store.try_set_thumbnail(&video, &picture()).unwrap());
        videos.push(video);
    }
    assert_eq!(folder.file_data(), before);
    assert_eq!(signature(&folder.profiles_file()), sig);
    assert_eq!(store.totals(), ProfileCounts::default());
    assert!(feed(&store, F::History).is_empty());
    assert!(videos.iter().all(|v| store.stats_mode(v, FingerprintMode::Compute).is_none()));
    assert!(!store.has_unsaved_changes());
    assert_eq!(folder.cache_files().len(), 40);
    assert!(videos.iter().all(|v| store.thumbnail(v) == Some(picture())));
}

#[test]
fn pictures_are_encrypted_under_the_profiles_key_with_opaque_names() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("Emre Gizli", "pw", false).unwrap();
    let video = folder.video("Cok_Gizli_Film.mp4");
    assert!(store.try_set_thumbnail(&video, &picture()).unwrap());
    let file = folder.cache_files().pop().unwrap();
    let bytes = std::fs::read(&file).unwrap();
    assert!(!bytes.windows(64).any(|w| w == &picture()[..64]));
    assert_eq!(file.extension().unwrap(), "bin");
    let names = format!(
        "{}{}",
        file.parent().unwrap().file_name().unwrap().to_string_lossy(),
        file.file_name().unwrap().to_string_lossy()
    )
    .to_lowercase();
    assert!(!names.contains("emre") && !names.contains("film") && !names.contains("mp4"));
    assert_eq!(bytes.len(), picture().len() + 28);
    let (_, key) = store.active_vault_key().unwrap();
    let mut wrong = [0u8; 32];
    crate::crypto::random_fill(&mut wrong);
    let wrong = sa_core::secrets::SecretKey32::new(wrong);
    assert_ne!(ThumbnailCache::file_name("k", &wrong), ThumbnailCache::file_name("k", &key));
}

#[test]
fn logged_out_nothing_is_stored_or_read() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    let video = folder.video("a.mp4");
    assert!(!store.try_set_thumbnail(&video, &picture()).unwrap());
    assert_eq!(store.thumbnail(&video), None);
    assert!(folder.cache_files().is_empty());
}

#[test]
fn each_profile_has_its_own_pictures() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    let video = folder.video("a.mp4");
    store.register_user("ali", "pw", false).unwrap();
    assert!(store.try_set_thumbnail(&video, &picture()).unwrap());
    store.logout_user().unwrap();
    store.register_user("veli", "pw", false).unwrap();
    assert_eq!(store.thumbnail(&video), None);
    store.logout_user().unwrap();
    store.login_user("ali", "pw", false).unwrap();
    assert_eq!(store.thumbnail(&video), Some(picture()));
}

#[test]
fn a_picture_follows_the_file_when_it_moves() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    let original = folder.video("original.mp4");
    std::fs::write(&original, vec![9u8; 5000]).unwrap();
    assert!(store.try_set_thumbnail(&original, &picture()).unwrap());
    let moved = folder.video("renamed.mp4");
    std::fs::rename(&original, &moved).unwrap();
    assert_eq!(store.thumbnail(&moved), Some(picture()));
}

#[test]
fn a_vault_with_pictures_is_migrated_on_its_first_save() {
    let folder = TestFolder::new();
    let password = "pw";
    let salt: Vec<u8> = (0..16).collect();
    let (hash, key) = derive_checked(password, &salt, 100_000).unwrap();
    let record = |path: &str, views: i64, liked: bool, position: f64| {
        let watched = if views > 0 { ",\"lastWatchedAtUtc\":\"2026-05-01T10:20:30Z\"" } else { "" };
        let name = path.rsplit('/').next().unwrap();
        format!(
            "{{\"videoPath\":\"{path}\",\"fileName\":\"{name}\",\"directoryPath\":\"/Movies\",\"viewCount\":{views},\"isLiked\":{liked},\"rating\":0,\"lastPositionSeconds\":{position},\"durationSeconds\":0,\"updatedAtUtc\":\"2026-05-01T10:20:30Z\"{watched}}}"
        )
    };
    let image = base64::engine::general_purpose::STANDARD.encode(picture());
    let records = [
        record("/Movies/watched.mkv", 2, false, 0.0),
        record("/Movies/browsed.mkv", 0, false, 0.0),
        record("/Movies/listed.mkv", 0, false, 0.0),
        record("/Movies/resumed.mkv", 0, false, 30.0),
        record("/Movies/liked.mkv", 0, true, 0.0),
    ]
    .join(",");
    let mut pictures: Vec<String> =
        ["watched", "browsed", "listed", "resumed", "liked"].iter().map(|n| format!("\"/movies/{n}.mkv\":\"{image}\"")).collect();
    pictures.push(format!("\"fp:9:orphan\":\"{image}\""));
    let vault_json = format!(
        "{{\"videos\":[{records}],\"customLists\":[{{\"id\":\"0123456789abcdef0123456789abcdef\",\"name\":\"Liste\",\"videoPaths\":[\"/Movies/listed.mkv\"],\"createdAtUtc\":\"2026-05-01T07:00:00Z\",\"updatedAtUtc\":\"2026-05-01T07:00:00Z\"}}],\"thumbnails\":{{{}}}}}",
        pictures.join(",")
    );
    let vault = base64::engine::general_purpose::STANDARD.encode(hand_sealed(vault_json.as_bytes(), &key));
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let file = format!(
        "{{\"version\":2,\"rememberMe\":false,\"profiles\":[{{\"username\":\"Ayşe\",\"saltBase64\":\"{}\",\"passwordHashBase64\":\"{}\",\"iterations\":100000,\"createdAtUtc\":\"2026-04-30T08:00:00Z\",\"encryptedVaultBase64\":\"{vault}\"}}]}}",
        b64(&salt),
        b64(&hash)
    );
    std::fs::write(folder.profiles_file(), &file).unwrap();
    let size_before = file.len();

    let store = folder.make_store();
    store.login_user("Ayşe", password, false).unwrap();
    assert_eq!(store.thumbnail(Path::new("/Movies/browsed.mkv")), Some(picture()));
    assert!(folder.cache_files().is_empty());

    store.try_set_rating(&folder.video("other.mp4"), 3).unwrap();
    assert_eq!(folder.cache_files().len(), 6);
    for name in ["watched", "browsed", "listed", "resumed", "liked"] {
        assert_eq!(store.thumbnail(Path::new(&format!("/Movies/{name}.mkv"))), Some(picture()), "{name}");
    }
    assert!(folder.file_data().len() < size_before);

    let root = folder.profiles_object();
    let encoded = root["profiles"][0]["encryptedVaultBase64"].as_str().unwrap();
    let blob = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
    let plain = crate::crypto::open(&blob, &key).unwrap();
    let payload: Value = serde_json::from_slice(&plain).unwrap();
    assert!(payload["thumbnails"].as_object().unwrap().is_empty());
    let mut kept: Vec<String> =
        payload["videos"].as_array().unwrap().iter().map(|v| v["videoPath"].as_str().unwrap().to_string()).collect();
    kept.sort();
    let mut expected = vec![
        "/Movies/watched.mkv".to_string(),
        "/Movies/listed.mkv".to_string(),
        "/Movies/resumed.mkv".to_string(),
        "/Movies/liked.mkv".to_string(),
        normalized(&folder.video("other.mp4")),
    ];
    expected.sort();
    assert_eq!(kept, expected, "the record that only held a picture is gone");

    let again = folder.store(StoreOpts { secrets: TestSecrets::new(), ..StoreOpts::default() });
    again.login_user("Ayşe", password, false).unwrap();
    assert_eq!(again.thumbnail(Path::new("/Movies/browsed.mkv")), Some(picture()));
}

#[test]
fn deleting_a_profile_deletes_its_pictures() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("ali", "pw", false).unwrap();
    let video = folder.video("a.mp4");
    assert!(store.try_set_thumbnail(&video, &picture()).unwrap());
    store.logout_user().unwrap();
    store.register_user("veli", "pw", false).unwrap();
    assert!(store.try_set_thumbnail(&video, &picture()).unwrap());
    assert_eq!(folder.cache_files().len(), 2);
    store.delete_user("ali", "pw").unwrap();
    assert_eq!(folder.cache_files().len(), 1);
    assert_eq!(store.thumbnail(&video), Some(picture()));
    store.delete_user("veli", "pw").unwrap();
    assert!(folder.cache_files().is_empty());
}

#[test]
fn clearing_the_history_forgets_the_pictures_of_the_videos_it_drops() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("ali", "pw", false).unwrap();
    let (dropped, kept, browsed) = (folder.video("dropped.mp4"), folder.video("kept.mp4"), folder.video("browsed.mp4"));
    store.try_record_watch(&dropped, 0.0, 0.0).unwrap();
    store.try_record_watch(&kept, 0.0, 0.0).unwrap();
    store.try_set_liked(&kept, true).unwrap();
    for video in [&dropped, &kept, &browsed] {
        assert!(store.try_set_thumbnail(video, &picture()).unwrap());
    }
    assert!(store.try_clear_watch_history().unwrap());
    assert_eq!(store.thumbnail(&dropped), None);
    assert_eq!(store.thumbnail(&kept), Some(picture()));
    assert_eq!(store.thumbnail(&browsed), Some(picture()), "a picture of a video that never had a record stays");
}

#[test]
fn the_session_generation_changes_with_every_login_and_logout() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    let start = store.session_generation();
    store.register_user("ali", "pw", false).unwrap();
    let registered = store.session_generation();
    assert_ne!(registered, start);
    store.try_set_liked(&folder.video("a.mp4"), true).unwrap();
    assert_eq!(store.session_generation(), registered);
    store.logout_user().unwrap();
    let logged_out = store.session_generation();
    assert_ne!(logged_out, registered);
    store.logout_user().unwrap();
    assert_eq!(store.session_generation(), logged_out);
    store.login_user("ali", "pw", false).unwrap();
    assert_ne!(store.session_generation(), logged_out);
}

/// The UI thread reads the generation at every playback report; it must not wait for a worker that holds the state
/// mutex (a re-read of `profiles.json`, a vault decrypt).
#[test]
fn the_session_generation_never_waits_for_the_state_mutex() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("ali", "pw", false).unwrap();
    let expected = store.inner.lock().state.session_generation;
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    std::thread::scope(|s| {
        let holder = &store;
        s.spawn(move || {
            let _l = holder.inner.lock();
            held_tx.send(()).unwrap();
            // Released on request, or after 2 s so a blocking reader fails instead of hanging.
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
        });
        held_rx.recv().unwrap();
        let t = std::time::Instant::now();
        let generation = store.session_generation();
        let elapsed = t.elapsed();
        let _ = release_tx.send(());
        assert_eq!(generation, expected);
        assert!(elapsed < std::time::Duration::from_millis(500), "took {elapsed:?} with the mutex held elsewhere");
    });
}

#[test]
fn playback_progress_alone_is_not_an_unsaved_migration() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("ali", "pw", false).unwrap();
    let video = folder.video("a.mp4");
    std::fs::write(&video, vec![3u8; 4000]).unwrap();
    store.try_record_watch(&video, 0.0, 0.0).unwrap();
    store.try_update_playback_progress(&video, 20.0, 100.0).unwrap();
    assert!(store.has_unsaved_changes());
    assert!(!store.has_unsaved_migrations());
    let moved = folder.video("b.mp4");
    std::fs::rename(&video, &moved).unwrap();
    let _ = store.stats_mode(&moved, FingerprintMode::Compute);
    assert!(store.has_unsaved_migrations());
    store.try_persist_pending_migrations().unwrap();
    assert!(!store.has_unsaved_migrations());
    assert!(!store.has_unsaved_changes());
}

#[test]
fn a_store_can_be_built_without_reading_the_file() {
    let folder = TestFolder::new();
    folder.make_store().register_user("ali", "pw", false).unwrap();
    let lazy = MacProfileStore::with_extras(
        MacProfileStoreOptions {
            file: Some(folder.profiles_file()),
            watches_file: false,
            reads_file_immediately: false,
            ..MacProfileStoreOptions::default()
        },
        TestSecrets::new(),
        StoreExtras { fingerprints: Some(folder.cache.clone()), ..StoreExtras::default() },
    );
    assert!(!lazy.inner.lock().state.has_synced, "nothing is read until something asks");
    assert_eq!(lazy.registered_usernames(), ["ali"]);
    lazy.login_user("ali", "pw", false).unwrap();
    assert!(lazy.is_logged_in());
}

#[test]
fn a_session_end_forgets_the_fingerprinted_paths() {
    // UNIFIED.md §3.12 FIX: the memo of paths the profile looked at does not outlive the session.
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("ali", "pw", false).unwrap();
    let video = folder.video("a.mp4");
    std::fs::write(&video, vec![5u8; 1000]).unwrap();
    store.try_record_watch(&video, 0.0, 0.0).unwrap();
    assert!(!folder.cache.is_empty());
    store.logout_user().unwrap();
    assert!(folder.cache.is_empty());
    assert!(store.active_vault_key().is_none());
}

/// A login switch between the capture of the generation (UI thread) and the write (worker) records nothing of the
/// previous profile in the new one: the `*_in_session` writes compare the generation under the state mutex.
#[test]
fn a_user_switch_between_capture_and_write_records_nothing_in_the_new_profile() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    let video = folder.video("a.mp4");
    std::fs::write(&video, "content a").unwrap();
    store.register_user("bob", "pw", false).unwrap();
    store.logout_user().unwrap();
    store.register_user("alice", "pw", false).unwrap();
    let alice = store.session_generation();
    assert!(store.update_playback_progress_in_session(alice, &video, 12.0, 100.0).is_some(), "the current session writes");
    store.logout_user().unwrap();
    store.login_user("bob", "pw", false).unwrap();
    assert_ne!(store.session_generation(), alice);
    assert!(store.update_playback_progress_in_session(alice, &video, 42.0, 100.0).is_none());
    assert!(store.record_watch_in_session(alice, &video, 0.0, 0.0).is_none());
    assert!(!store.set_thumbnail_in_session(alice, &video, &picture()));
    store.flush_playback_progress();
    let stats = store.stats(&video, FingerprintMode::Compute);
    assert!(stats.as_ref().is_none_or(|s| s.view_count == 0 && s.last_position_seconds == 0.0), "nothing of alice in bob's profile: {stats:?}");
    assert_eq!(store.thumbnail(&video), None);
    let bob = store.session_generation();
    assert!(store.record_watch_in_session(bob, &video, 0.0, 0.0).is_some());
    assert!(store.set_thumbnail_in_session(bob, &video, &picture()));
    assert_eq!(store.stats(&video, FingerprintMode::Compute).map(|s| s.view_count), Some(1));
}
