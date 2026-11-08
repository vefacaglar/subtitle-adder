//! `ProfileRobustnessTests` (17): damaged files, unreadable vaults, failing disks and what gets written when.

use base64::Engine as _;
use serde_json::Value;

use super::*;
use crate::mac::fileio::FileLock;
use crate::{FingerprintMode, ProfileCounts, ProfileError, ProfileStore, VideoFeedFilter as F};

fn random_b64(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    crate::crypto::random_fill(&mut bytes);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn set_vault(folder: &TestFolder, index: usize, vault: &str) {
    let mut root = folder.profiles_object();
    let profiles = root.get_mut("profiles").unwrap().as_array_mut().unwrap();
    profiles[index].as_object_mut().unwrap().insert("encryptedVaultBase64".into(), Value::String(vault.into()));
    folder.write_profiles_object(&root);
}

#[test]
fn a_corrupt_profiles_file_is_moved_aside_before_anything_new_is_written() {
    let folder = TestFolder::new();
    let garbage = b"{ this is not json";
    std::fs::write(folder.profiles_file(), garbage).unwrap();
    let store = folder.make_store();
    assert!(store.registered_usernames().is_empty());
    assert_eq!(store.login_user("nobody", "pw", true), Err(ProfileError::InvalidCredentials));
    assert_eq!(folder.file_data(), garbage, "merely looking never touches the file");
    assert!(folder.aside_files().is_empty());

    store.register_user("emre", "pw", true).unwrap();
    let aside = folder.aside_files();
    assert_eq!(aside.len(), 1);
    assert_eq!(std::fs::read(folder.url.join(&aside[0])).unwrap(), garbage);
    let name = &aside[0];
    let stamp = name.strip_prefix("profiles.corrupt-").unwrap().strip_suffix(".json").unwrap();
    assert!(stamp.len() == 15 && stamp.as_bytes()[8] == b'-' && stamp.replace('-', "").chars().all(|c| c.is_ascii_digit()), "{name}");
    assert_eq!(folder.profiles_object()["version"], 2);
    assert_eq!(folder.make_store().registered_usernames(), ["emre"]);
}

#[test]
fn an_empty_file_counts_as_corrupt_too() {
    let folder = TestFolder::new();
    std::fs::write(folder.profiles_file(), b"").unwrap();
    folder.make_store().register_user("emre", "pw", true).unwrap();
    assert_eq!(folder.aside_files().len(), 1);
}

#[test]
fn each_damaged_generation_gets_its_own_aside_file() {
    let folder = TestFolder::new();
    let clock = ManualClock::new();
    for round in 1..=2 {
        std::fs::write(folder.profiles_file(), format!("broken {round}")).unwrap();
        let store = folder.store(StoreOpts { clock: Some(clock.clock()), ..StoreOpts::default() });
        store.register_user(&format!("user{round}"), "pw", true).unwrap();
    }
    let aside = folder.aside_files();
    assert_eq!(aside.len(), 2);
    assert!(aside.iter().any(|n| n.ends_with("-1.json")), "same clock → numbered: {aside:?}");
    let mut contents: Vec<String> =
        aside.iter().map(|n| String::from_utf8(std::fs::read(folder.url.join(n)).unwrap()).unwrap()).collect();
    contents.sort();
    assert_eq!(contents, ["broken 1", "broken 2"]);
}

#[test]
fn a_running_store_keeps_its_state_and_restores_the_file_after_someone_corrupts_it() {
    let folder = TestFolder::new();
    let a = folder.video("a.mp4");
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    store.try_record_watch(&a, 0.0, 0.0).unwrap();
    std::fs::write(folder.profiles_file(), b"garbage").unwrap();
    assert!(store.is_logged_in(), "a reload failure must not log the user out");
    store.try_set_liked(&a, true).unwrap();
    assert_eq!(folder.aside_files().len(), 1);
    let fresh = folder.make_store();
    fresh.login_user("emre", "pw", true).unwrap();
    assert!(fresh.stats_mode(&a, FingerprintMode::Compute).unwrap().is_liked);
}

#[test]
fn a_newer_file_version_is_left_alone() {
    let folder = TestFolder::new();
    let future = br#"{"version": 99, "profiles": []}"#;
    std::fs::write(folder.profiles_file(), future).unwrap();
    let store = folder.make_store();
    assert_eq!(store.register_user("emre", "pw", true), Err(ProfileError::UnsupportedFileVersion(99)));
    assert_eq!(folder.file_data(), future);
    assert!(folder.aside_files().is_empty());
    assert!(!store.is_logged_in());
}

#[test]
fn a_missing_file_is_fine() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    assert!(store.registered_usernames().is_empty());
    assert!(!folder.profiles_file().exists());
    assert_eq!(store.totals(), ProfileCounts::default());
    assert!(feed(&store, F::All).is_empty());
    store.logout_user().unwrap();
    assert!(!folder.profiles_file().exists());
}

#[test]
fn an_unreadable_vault_makes_login_fail_with_vault_unreadable_and_leaves_the_ciphertext_alone() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("alice", "pw-alice", false).unwrap();
    store.try_record_watch(&folder.video("a.mp4"), 0.0, 0.0).unwrap();
    store.logout_user().unwrap();
    let damaged = random_b64(80);
    set_vault(&folder, 0, &damaged);
    let before = folder.file_data();

    let reopened = folder.make_store();
    assert_eq!(reopened.login_user("alice", "pw-alice", true), Err(ProfileError::VaultUnreadable));
    assert!(!reopened.is_logged_in());
    assert_eq!(reopened.login_user("alice", "wrong", true), Err(ProfileError::InvalidCredentials));
    assert_eq!(folder.file_data(), before);

    reopened.register_user("bob", "pw-bob", true).unwrap();
    let root = folder.profiles_object();
    let alice = root["profiles"].as_array().unwrap().iter().find(|p| p["username"] == "alice").unwrap();
    assert_eq!(alice["encryptedVaultBase64"], Value::String(damaged));
    assert_eq!(reopened.registered_usernames(), ["alice", "bob"]);
}

#[test]
fn a_remembered_login_with_an_unreadable_vault_does_not_log_in() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let store = folder.store_with(&secrets);
    store.register_user("alice", "pw", true).unwrap();
    set_vault(&folder, 0, &random_b64(64));
    assert!(!folder.store_with(&secrets).is_logged_in());
    assert!(!store.is_logged_in(), "a running session whose vault was replaced by garbage is locked, not kept");
}

#[test]
fn an_unknown_user_still_pays_for_one_pbkdf2() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("alice", "pw", true).unwrap();
    store.logout_user().unwrap();
    let started = Instant::now();
    assert_eq!(store.login_user("nobody", "pw", true), Err(ProfileError::InvalidCredentials));
    let unknown = started.elapsed();
    let started = Instant::now();
    assert_eq!(store.login_user("", "pw", true), Err(ProfileError::MissingCredentials));
    let blank = started.elapsed();
    assert!(unknown > Duration::from_millis(5), "an unknown user must cost a key derivation ({unknown:?})");
    assert!(blank < unknown);
}

#[cfg(unix)]
#[test]
fn write_failures_are_reported_and_leave_the_state_as_it_was() {
    use std::os::unix::fs::PermissionsExt;
    if !permissions_apply() {
        return;
    }
    let folder = TestFolder::new();
    let video = folder.video("a.mp4");
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    store.try_record_watch(&video, 0.0, 0.0).unwrap();
    std::fs::set_permissions(&folder.url, std::fs::Permissions::from_mode(0o555)).unwrap();
    assert_eq!(store.try_set_liked(&video, true), Err(ProfileError::WriteFailed { errno: 13 }));
    assert!(!store.stats_mode(&video, FingerprintMode::Compute).unwrap().is_liked, "a failed save is not applied");
    assert!(store.try_record_watch(&video, 0.0, 0.0).is_err());
    std::fs::set_permissions(&folder.url, std::fs::Permissions::from_mode(0o755)).unwrap();
    store.try_set_liked(&video, true).unwrap();
    let stats = store.stats_mode(&video, FingerprintMode::Compute).unwrap();
    assert!(stats.is_liked);
    assert_eq!(stats.view_count, 1);
}

#[test]
fn a_held_file_lock_fails_fast_instead_of_proceeding_unlocked() {
    let folder = TestFolder::new();
    let video = folder.video("a.mp4");
    let store = folder.store(StoreOpts { lock_timeout: Duration::from_millis(200), ..StoreOpts::default() });
    store.register_user("emre", "pw", false).unwrap();
    let mut held = FileLock::acquire(store.lock_path(), Duration::from_secs(1)).unwrap();
    assert_eq!(store.try_record_watch(&video, 0.0, 0.0), Err(ProfileError::LockUnavailable));
    assert_eq!(store.try_create_list("x"), Err(ProfileError::LockUnavailable));
    assert!(store.is_logged_in());
    assert_eq!(store.logout_user(), Err(ProfileError::LockUnavailable));
    assert!(!store.is_logged_in());
    held.release();
    store.login_user("emre", "pw", false).unwrap();
    store.try_record_watch(&video, 0.0, 0.0).unwrap();
    assert_eq!(store.stats_mode(&video, FingerprintMode::Compute).unwrap().view_count, 1);
    // RUST: the trait swallows the error and keeps it for the caller.
    let mut held = FileLock::acquire(store.lock_path(), Duration::from_secs(1)).unwrap();
    assert_eq!(store.record_watch(&video, 0.0, 0.0), None);
    assert_eq!(store.last_error(), Some(ProfileError::LockUnavailable));
    held.release();
}

#[test]
fn a_logged_out_store_does_not_take_the_lock_or_write_anything() {
    let folder = TestFolder::new();
    let store = folder.store(StoreOpts { lock_timeout: Duration::from_millis(100), ..StoreOpts::default() });
    let mut held = FileLock::acquire(store.lock_path(), Duration::from_secs(1)).unwrap();
    let a = folder.video("a.mp4");
    std::fs::write(&a, b"bytes").unwrap();
    assert_eq!(store.try_record_watch(&a, 0.0, 0.0).unwrap(), None);
    assert_eq!(store.try_update_playback_progress(&a, 5.0, 10.0).unwrap(), None);
    assert!(!store.try_set_thumbnail(&a, b"x").unwrap());
    assert!(!folder.profiles_file().exists());
    assert!(folder.cache.is_empty(), "PRIVACY OVERRIDE: no file is fingerprinted while logged out");
    held.release();
}

#[test]
fn progress_is_kept_only_when_it_moved_half_a_second() {
    let folder = TestFolder::new();
    let video = folder.video("a.mp4");
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    store.try_record_watch(&video, 10.0, 100.0).unwrap();
    let saved = folder.file_data();
    let small = store.try_update_playback_progress(&video, 10.3, 100.0).unwrap().unwrap();
    assert_eq!(small.last_position_seconds, 10.0);
    assert!(!store.has_unsaved_changes());
    assert!(!store.try_flush_playback_progress().unwrap());
    assert_eq!(folder.file_data(), saved);
    let real = store.try_update_playback_progress(&video, 10.5, 100.0).unwrap().unwrap();
    assert_eq!(real.last_position_seconds, 10.5);
    assert_eq!(real.view_count, 1, "progress never counts a view");
    assert_eq!(store.stats_mode(&video, FingerprintMode::Compute).unwrap().last_position_seconds, 10.5);
    assert!(store.has_unsaved_changes());
    assert!(store.try_flush_playback_progress().unwrap());
    assert_ne!(folder.file_data(), saved);
    let cut = store.try_update_playback_progress(&video, 500.0, 100.0).unwrap().unwrap();
    assert_eq!(cut.last_position_seconds, 100.0);
}

#[test]
fn a_position_of_zero_in_record_watch_keeps_the_old_one() {
    let folder = TestFolder::new();
    let video = folder.video("a.mp4");
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    store.try_record_watch(&video, 42.0, 120.0).unwrap();
    let again = store.try_record_watch(&video, 0.0, 0.0).unwrap().unwrap();
    assert_eq!((again.last_position_seconds, again.duration_seconds, again.view_count), (42.0, 120.0, 2));
    let nan = store.try_record_watch(&video, f64::NAN, f64::INFINITY).unwrap().unwrap();
    assert_eq!((nan.last_position_seconds, nan.duration_seconds), (42.0, 120.0));
    let over = store.try_record_watch(&video, 999.0, 0.0).unwrap().unwrap();
    assert_eq!(over.last_position_seconds, 120.0);
}

#[test]
fn progress_on_an_untouched_video_creates_it_only_when_it_matters() {
    let folder = TestFolder::new();
    let video = folder.video("a.mp4");
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    let tiny = store.try_update_playback_progress(&video, 0.2, 0.0).unwrap().unwrap();
    assert_eq!(tiny.view_count, 0);
    assert_eq!(store.stats_mode(&video, FingerprintMode::Compute), None);
    store.try_update_playback_progress(&video, 20.0, 100.0).unwrap();
    let stored = store.stats_mode(&video, FingerprintMode::Compute).unwrap();
    assert_eq!(stored.last_position_seconds, 20.0);
    assert!(!stored.has_watched());
    assert_eq!(stored.progress_ratio(), 0.2);
    assert!(feed(&store, F::History).is_empty());
}

#[test]
fn thumbnails_over_512_kb_are_ignored() {
    let folder = TestFolder::new();
    let video = folder.video("a.mp4");
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    store.try_record_watch(&video, 0.0, 0.0).unwrap();
    let saved = folder.file_data();
    assert!(!store.try_set_thumbnail(&video, &vec![1u8; 512 * 1024 + 1]).unwrap());
    assert!(!store.try_set_thumbnail(&video, &[]).unwrap());
    assert_eq!(store.thumbnail(&video), None);
    assert_eq!(folder.file_data(), saved);
    let exactly = vec![2u8; 512 * 1024];
    assert!(store.try_set_thumbnail(&video, &exactly).unwrap());
    assert_eq!(store.thumbnail(&video).as_deref(), Some(exactly.as_slice()));
    let reopened = folder.make_store();
    reopened.login_user("emre", "pw", true).unwrap();
    assert_eq!(reopened.thumbnail(&video).as_deref(), Some(exactly.as_slice()));
}

#[test]
fn ratings_are_clamped_and_zero_clears() {
    let folder = TestFolder::new();
    let video = folder.video("a.mp4");
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    assert_eq!(store.try_set_rating(&video, 9).unwrap().unwrap().rating, 5);
    assert_eq!(store.try_set_rating(&video, -3).unwrap().unwrap().rating, 0);
    assert_eq!(store.try_set_rating(&video, 3).unwrap().unwrap().rating, 3);
    assert_eq!(store.totals().rated, 1);
    assert_eq!(store.try_set_rating(&video, 0).unwrap().unwrap().rating, 0);
    assert_eq!(store.totals().rated, 0);
    assert!(store.try_toggle_like(&video).unwrap());
    assert!(!store.try_toggle_like(&video).unwrap());
    assert_eq!(store.set_rating(&video, 4).unwrap().rating, 4, "the trait takes 0..=5 stars");
}
