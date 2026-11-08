//! Ports of the C# profile-store tests (apps/windows/tests/SubtitleAdder.Core.Tests: `LocalProfileTests.cs`,
//! `ProfilePrivacyTests.cs`, the SQLite/memory/IPC parts of `WatchHistoryPrivacyTests.cs` and the profile part of
//! `EditingAndCatalogTests.MultiInstanceConcurrentStoresAndCleanupNeverThrowOrCorrupt`), driven at the store level
//! (view-model assertions live in sa-viewmodels), plus the Rust-side decisions of UNIFIED.md §3.11/§3.12/§4.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use sa_core::secrets::SecretString;
use sa_profiles::win::{SqliteProfileStore, WinProfileStore, WriteItem, WriteKind};
use sa_profiles::{FeedQuery, ProfileChange, ProfileError, ProfileStore, VideoFeedFilter};
use sa_sync::{EventKind, InProcessBus, NamedPipeBus, ProfileWriteBroker, StateSyncEvent, StateSyncWriteRequest, SyncBus, Topic};
use serde_json::Value;

const FLUSH: Duration = Duration::from_secs(5);
const NEVER: fn() -> bool = || true;

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Self { dir: tempfile::tempdir().expect("temp dir") }
    }
    fn profiles(&self) -> PathBuf {
        self.dir.path().join("profiles.json")
    }
    fn path(&self, name: &str) -> String {
        self.dir.path().join(name).to_string_lossy().into_owned()
    }
    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let p = self.path(name);
        std::fs::write(&p, bytes).expect("write file");
        p
    }
    fn json(&self) -> String {
        std::fs::read_to_string(self.profiles()).expect("profiles.json")
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let p = self.profiles();
        if let Ok(meta) = std::fs::metadata(&p) {
            let mut perm = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(&p, perm);
        }
    }
}

fn bus() -> Arc<dyn ProfileWriteBroker> {
    InProcessBus::new()
}

fn scoped(scope: &str) -> Arc<dyn ProfileWriteBroker> {
    InProcessBus::with_scope(scope)
}

fn mgr(path: &Path, broker: &Arc<dyn ProfileWriteBroker>) -> Arc<WinProfileStore> {
    WinProfileStore::open(path.to_path_buf(), broker.clone())
}

fn feed(m: &WinProfileStore, filter: VideoFeedFilter) -> Vec<sa_profiles::VideoUserStats> {
    m.get_feed(&filter, None, None, &NEVER)
}

fn feed_paths(m: &WinProfileStore, filter: VideoFeedFilter) -> Vec<String> {
    feed(m, filter).into_iter().map(|v| v.video_path).collect()
}

fn guid() -> String {
    sa_core::text::guid_n()
}

fn set_readonly(p: &Path, ro: bool) {
    let mut perm = std::fs::metadata(p).expect("meta").permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(ro);
    std::fs::set_permissions(p, perm).expect("set permissions");
}

fn other_event(target: &WinProfileStore, topic: Topic, kind: EventKind) -> StateSyncEvent {
    StateSyncEvent::new(topic, kind, &format!("other-window-{}", guid()), &target.file_path().to_string_lossy())
}

fn wait_until(what: &str, timeout: Duration, f: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(f(), "timed out waiting for {what}");
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

// =============================================================================================================
// LocalProfileTests.cs

#[test]
fn register_hashes_password_with_pbkdf2_never_stores_plaintext_on_disk() {
    let env = Env::new();
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    assert!(!m.is_logged_in());
    assert_eq!(m.current_username(), None);

    let secret = "MySuperSecretPassword!2026";
    m.register_user("emre", secret, false).expect("register");
    assert!(m.is_logged_in());
    assert_eq!(m.current_username().as_deref(), Some("emre"));
    assert!(m.registered_usernames().contains(&"emre".to_owned()));
    let raw = env.json();
    assert!(!raw.to_lowercase().contains(&secret.to_lowercase()));
    assert!(raw.contains("saltBase64") && raw.contains("passwordHashBase64") && raw.contains("100000"));
    // C#-identical shape: CRLF, no BOM, no trailing newline, version 3.
    assert!(raw.contains("\r\n") && !raw.starts_with('\u{feff}') && !raw.ends_with('\n'));
    assert!(raw.starts_with("{\r\n  \"version\": 3,\r\n  \"rememberMe\": false,"));
}

#[test]
fn login_and_logout_enforce_password_and_session_state() {
    let env = Env::new();
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "CorrectHorseBatteryStaple", false).expect("register");
    m.logout_user();
    assert!(!m.is_logged_in());
    assert_eq!(m.current_username(), None);

    assert_eq!(m.login_user("alice", "WrongPassword", false), Err(ProfileError::InvalidCredentials));
    assert!(!m.is_logged_in());
    assert!(m.last_error().is_some());
    // Unknown users get the same message, so the error does not reveal which names exist.
    assert_eq!(m.login_user("unknown_user", "CorrectHorseBatteryStaple", false), Err(ProfileError::InvalidCredentials));
    assert_eq!(m.login_user("  ", "x", false), Err(ProfileError::MissingCredentials));
    assert_eq!(m.login_user("alice", "", false), Err(ProfileError::MissingCredentials));
    assert!(!m.is_logged_in());

    m.login_user("ALICE", "CorrectHorseBatteryStaple", false).expect("login");
    assert!(m.is_logged_in());
    assert_eq!(m.current_username().as_deref(), Some("alice"));
    assert_eq!(m.last_error(), None);

    // Register validation and duplicates.
    assert_eq!(m.register_user("   ", "pw", false), Err(ProfileError::EmptyUsername));
    assert_eq!(m.register_user("bob", "", false), Err(ProfileError::EmptyPassword));
    assert_eq!(m.register_user(" Alice ", "pw", false), Err(ProfileError::UsernameTaken));
    // The password is used exactly as typed (not trimmed).
    m.register_user("spaces", " pw ", false).expect("register spaces");
    m.logout_user();
    assert!(m.login_user("spaces", "pw", false).is_err());
    m.login_user("spaces", " pw ", false).expect("untrimmed password");
}

#[test]
fn logged_out_state_does_not_record_watch_history_or_expose_stats() {
    let env = Env::new();
    let video = env.path("lecture.mp4");
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pw123", false).expect("register");
    m.record_watch_with(&video, 42.0, 120.0, None, None);
    m.set_liked_path(&video, true);
    m.set_rating_path(&video, 5);
    m.logout_user();

    assert!(m.get_stats(&video).is_none());
    assert!(feed(&m, VideoFeedFilter::All).is_empty());
    assert!(feed(&m, VideoFeedFilter::History).is_empty());
    assert_eq!(m.totals(), Default::default());
    assert!(m.record_watch_with(&video, 99.0, 120.0, None, None).is_none());
    assert!(!m.toggle_like(&video));
    assert!(m.set_rating_path(&video, 1).is_none());
    assert!(m.create_custom_list("Test List").is_none());
    assert!(m.update_playback_progress_with(&video, 5.0, 120.0, None).is_none());
    assert!(!m.set_thumbnail_with(&video, &[1, 2, 3], None));
    assert!(!m.clear_history());
    assert!(m.get_custom_lists().is_empty());
    assert!(m.active_vault_key().is_none());
    // Logged out no file is read: an existing video is not even fingerprinted.
    let real = env.file("real.mp4", &[1, 2, 3, 4, 5, 6]);
    assert!(m.record_watch_with(&real, 1.0, 10.0, None, None).is_none());
    assert!(!m.set_thumbnail_with(&real, &[1], None));
    assert!(sa_profiles::fingerprint::cached(Path::new(&real)).is_none());

    m.login_user("alice", "pw123", false).expect("login");
    let s = m.get_stats(&video).expect("stats");
    assert_eq!(s.view_count, 1);
    assert!(s.is_liked);
    assert_eq!(s.rating, 5);
    assert_eq!(s.last_position_seconds, 42.0);
}

#[test]
fn persistence_reloads_view_count_likes_ratings_and_last_watched_from_disk() {
    let env = Env::new();
    let video = env.path("movie.mkv");
    let b = bus();
    let first = mgr(&env.profiles(), &b);
    first.register_user("cinema_fan", "pass99", false).expect("register");
    first.record_watch_with(&video, 15.0, 300.0, None, None);
    first.record_watch_with(&video, 180.0, 300.0, None, None);
    first.update_playback_progress_with(&video, 210.0, 300.0, None);
    first.set_liked_path(&video, true);
    first.set_rating_path(&video, 4);

    let second = mgr(&env.profiles(), &b);
    assert!(!second.is_logged_in());
    assert!(second.registered_usernames().contains(&"cinema_fan".to_owned()));
    second.login_user("cinema_fan", "pass99", false).expect("login");
    let r = second.get_stats(&video).expect("stats");
    assert_eq!((r.view_count, r.is_liked, r.rating), (2, true, 4));
    assert_eq!((r.last_position_seconds, r.duration_seconds), (210.0, 300.0));
    assert!(r.last_watched_at.is_some());
}

#[test]
fn remember_me_opt_in_auto_restores_encrypted_session_across_new_instances_until_logout() {
    let env = Env::new();
    let video = env.path("remembered_show.mp4");
    let b = bus();
    let first = mgr(&env.profiles(), &b);
    first.register_user("emre", "pass2026", true).expect("register");
    assert!(first.remember_me());
    first.record_watch_with(&video, 50.0, 200.0, None, None);
    first.set_liked_path(&video, true);

    let second = mgr(&env.profiles(), &b);
    assert!(second.is_logged_in());
    assert_eq!(second.current_username().as_deref(), Some("emre"));
    assert_eq!(second.last_username().as_deref(), Some("emre"));
    let s = second.get_stats(&video).expect("stats");
    assert_eq!(s.view_count, 1);
    assert!(s.is_liked);

    second.logout_user();
    let third = mgr(&env.profiles(), &b);
    assert!(!third.is_logged_in());
    assert_eq!(third.last_username().as_deref(), Some("emre"));
}

#[test]
fn encrypted_vault_and_thumbnails_never_expose_plaintext_video_names_lists_or_thumbnails_on_disk() {
    let env = Env::new();
    let secret_video = env.path("TopSecret_Project_Alpha_2026.mkv");
    let secret_list = "Gizli Özel Koleksiyonum";
    let thumb = b"FAKE_PNG_THUMBNAIL_PAYLOAD_987654321".to_vec();
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("vault_user", "UltraStrongVaultPass!99", false).expect("register");
    m.record_watch_with(&secret_video, 33.0, 100.0, None, None);
    m.set_liked_path(&secret_video, true);
    m.set_rating_path(&secret_video, 5);
    let list = m.create_custom_list(secret_list).expect("list");
    assert!(m.add_video_to_custom_list(&list.id, &secret_video));
    assert!(m.set_thumbnail_with(&secret_video, &thumb, None));

    let raw = env.json();
    assert!(raw.contains("encryptedVaultBase64"));
    assert!(!raw.to_lowercase().contains("topsecret_project_alpha_2026"));
    assert!(!raw.contains(secret_list));
    assert!(!raw.contains(&b64(&thumb)));

    let reloaded = mgr(&env.profiles(), &b);
    assert!(!reloaded.is_logged_in());
    assert!(reloaded.get_thumbnail(&secret_video).is_none());
    reloaded.login_user("vault_user", "UltraStrongVaultPass!99", false).expect("login");
    let s = reloaded.get_stats(&secret_video).expect("stats");
    assert_eq!((s.view_count, s.is_liked, s.rating), (1, true, 5));
    assert_eq!(s.thumbnail.as_deref(), Some(thumb.as_slice()));
    assert_eq!(reloaded.get_thumbnail(&secret_video).as_deref(), Some(thumb.as_slice()));
    let history = feed(&reloaded, VideoFeedFilter::History);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].thumbnail.as_deref(), Some(thumb.as_slice()));

    // Neither the database nor its WAL holds the file name, the list name or the thumbnail bytes.
    assert!(reloaded.flush_pending_writes(FLUSH));
    for f in [reloaded.sqlite_db_path(), PathBuf::from(format!("{}-wal", reloaded.sqlite_db_path().display()))] {
        if let Ok(bytes) = std::fs::read(&f) {
            for needle in [b"TopSecret_Project_Alpha_2026".as_slice(), secret_list.as_bytes(), thumb.as_slice()] {
                assert!(!bytes.windows(needle.len()).any(|w| w == needle), "{} leaks plaintext", f.display());
            }
        }
    }
}

#[test]
fn cross_instance_live_sync_two_concurrent_instances_merge_history_likes_ratings_and_lists() {
    let env = Env::new();
    let v1 = env.path("Instance1_Episode01.mp4");
    let v2 = env.path("Instance2_Episode02.mp4");
    let b = bus();
    let i1 = mgr(&env.profiles(), &b);
    i1.register_user("shared_user", "shared_pass_123", true).expect("register");
    let i2 = mgr(&env.profiles(), &b);
    assert!(i2.is_logged_in());
    assert_eq!(i2.current_username().as_deref(), Some("shared_user"));

    i1.record_watch_with(&v1, 30.0, 120.0, None, None);
    i1.set_liked_path(&v1, true);
    i2.record_watch_with(&v2, 90.0, 180.0, None, None);
    i2.set_rating_path(&v2, 5);
    let list = i2.create_custom_list("İki Pencere Ortak Liste").expect("list");
    assert!(i2.add_video_to_custom_list(&list.id, &v2));
    i1.record_watch_with(&v1, 60.0, 120.0, None, None);
    assert!(i1.add_video_to_custom_list(&list.id, &v1));

    assert_eq!(feed(&i1, VideoFeedFilter::History).len(), 2);
    assert_eq!(feed(&i2, VideoFeedFilter::History).len(), 2);
    let a = i2.get_stats(&v1).expect("v1 on 2");
    assert_eq!((a.view_count, a.is_liked), (2, true));
    let c = i1.get_stats(&v2).expect("v2 on 1");
    assert_eq!((c.view_count, c.rating), (1, 5));
    let l1 = i1.get_custom_lists();
    let l2 = i2.get_custom_lists();
    assert_eq!((l1.len(), l2.len()), (1, 1));
    assert_eq!((l1[0].video_paths.len(), l2[0].video_paths.len()), (2, 2));
}

#[test]
fn silent_background_updates_playback_progress_and_thumbnails_never_raise_changes() {
    let env = Env::new();
    let video = env.path("Playing_Movie.mp4");
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("emre", "pass123", false).expect("register");
    m.record_watch_with(&video, 0.0, 0.0, None, None);
    let discovered = vec![PathBuf::from(&video)];
    let q = FeedQuery { filter: VideoFeedFilter::All, discovered: discovered.clone(), ..Default::default() };
    let before = m.feed(&q, &NEVER).len();

    let changes = Arc::new(AtomicUsize::new(0));
    let c2 = changes.clone();
    let _sub = m.subscribe_changes(Box::new(move |_| {
        c2.fetch_add(1, Ordering::SeqCst);
    }));
    m.update_playback_progress_with(&video, 15.0, 300.0, None);
    m.update_playback_progress_with(&video, 30.0, 300.0, None);
    m.update_playback_progress_with(&video, 45.0, 300.0, None);
    let thumb = b"SAMPLE_THUMBNAIL_BYTES".to_vec();
    assert!(m.set_thumbnail_with(&video, &thumb, None));
    assert!(m.set_thumbnail_with(&video, &thumb, None));
    assert_eq!(m.feed(&q, &NEVER).len(), before);
    std::thread::sleep(Duration::from_millis(120));

    assert_eq!(changes.load(Ordering::SeqCst), 0);
    assert_eq!(m.get_stats(&video).map(|s| s.last_position_seconds), Some(45.0));
    assert_eq!(m.get_thumbnail(&video), Some(thumb));
}

#[test]
fn five_instances_over_the_named_pipe_broker_share_logins_history_progress_and_lists() {
    let env = Env::new();
    let scope = format!("rs-profiles-5-{}", guid());
    let buses: Vec<Arc<NamedPipeBus>> = (0..5).map(|_| NamedPipeBus::new(Some(&scope))).collect();
    let managers: Vec<Arc<WinProfileStore>> = buses
        .iter()
        .map(|b| {
            let broker: Arc<dyn ProfileWriteBroker> = b.clone();
            mgr(&env.profiles(), &broker)
        })
        .collect();
    let ep1 = env.path("Episode_01.mp4");
    let ep2 = env.path("Episode_02.mp4");
    let t = Duration::from_secs(10);

    managers[0].register_user("five_window_user", "Pass!12345", true).expect("register");
    wait_until("everyone logged in", t, || {
        managers.iter().all(|m| m.is_logged_in() && m.current_username().as_deref() == Some("five_window_user"))
    });

    managers[2].record_watch_with(&ep1, 10.0, 100.0, None, None);
    managers[2].set_liked_path(&ep1, true);
    wait_until("watch + like everywhere", t, || {
        managers.iter().all(|m| m.get_stats(&ep1).is_some_and(|s| s.view_count == 1 && s.is_liked))
    });

    let changes = Arc::new(AtomicUsize::new(0));
    let subs: Vec<_> = managers
        .iter()
        .map(|m| {
            let c = changes.clone();
            m.subscribe_changes(Box::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
            }))
        })
        .collect();
    let thumb = b"ENCRYPTED_THUMB_FROM_INSTANCE_3".to_vec();
    managers[3].update_playback_progress_with(&ep1, 77.0, 100.0, None);
    managers[3].set_thumbnail_with(&ep1, &thumb, None);
    wait_until("silent progress + thumbnail everywhere", t, || {
        managers.iter().all(|m| {
            m.get_stats_fast_in_memory(&ep1).is_some_and(|s| s.last_position_seconds == 77.0)
                && m.get_thumbnail(&ep1).is_some_and(|b| !b.is_empty())
        })
    });
    assert_eq!(changes.load(Ordering::SeqCst), 0);
    drop(subs);

    managers[0].dispose();
    buses[0].dispose();
    managers[2].record_watch_with(&ep2, 25.0, 120.0, None, None);
    let shared = managers[2].create_custom_list("Beş Pencere Ortak Liste").expect("list");
    assert!(managers[2].add_video_to_custom_list(&shared.id, &ep2));
    wait_until("the remaining four see the new watch and list", t, || {
        managers[1..].iter().all(|m| {
            m.get_stats(&ep2).is_some_and(|s| s.view_count == 1)
                && m.get_custom_lists().iter().any(|l| l.name == "Beş Pencere Ortak Liste" && l.video_paths.contains(&ep2))
        })
    });
    for (m, b) in managers.iter().zip(&buses).skip(1) {
        m.dispose();
        b.dispose();
    }
}

#[test]
fn multiple_profiles_have_completely_isolated_history_likes_ratings_and_lists() {
    let env = Env::new();
    let a = env.path("ep1.mp4");
    let bv = env.path("ep2.mp4");
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("user_one", "pass1", false).expect("u1");
    for _ in 0..3 {
        m.record_watch_with(&a, 0.0, 0.0, None, None);
    }
    m.set_liked_path(&a, true);
    m.set_rating_path(&a, 5);
    let l1 = m.create_custom_list("Bilim Kurgu").expect("l1");
    assert!(m.add_video_to_custom_list(&l1.id, &a));
    m.logout_user();

    m.register_user("user_two", "pass2", false).expect("u2");
    assert!(m.get_stats(&a).is_none());
    assert!(m.get_custom_lists().is_empty());
    m.record_watch_with(&a, 0.0, 0.0, None, None);
    m.set_rating_path(&a, 2);
    m.record_watch_with(&bv, 0.0, 0.0, None, None);
    m.record_watch_with(&bv, 0.0, 0.0, None, None);
    m.set_liked_path(&bv, true);
    m.set_rating_path(&bv, 4);
    let l2 = m.create_custom_list("Komedi").expect("l2");
    assert!(m.add_video_to_custom_list(&l2.id, &bv));
    m.logout_user();

    let r = mgr(&env.profiles(), &b);
    r.login_user("user_one", "pass1", false).expect("login 1");
    let s = r.get_stats(&a).expect("a");
    assert_eq!((s.view_count, s.is_liked, s.rating), (3, true, 5));
    assert!(r.get_stats(&bv).is_none());
    let lists = r.get_custom_lists();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].name, "Bilim Kurgu");
    assert_eq!(lists[0].video_paths, vec![a.clone()]);

    r.login_user("user_two", "pass2", false).expect("login 2");
    let sa = r.get_stats(&a).expect("a2");
    let sb = r.get_stats(&bv).expect("b2");
    assert_eq!((sa.view_count, sa.is_liked, sa.rating), (1, false, 2));
    assert_eq!((sb.view_count, sb.is_liked, sb.rating), (2, true, 4));
    let lists = r.get_custom_lists();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].name, "Komedi");
    assert_eq!(lists[0].video_paths, vec![bv]);
}

#[test]
fn custom_lists_and_clear_watch_history_preserve_favorites_ratings_and_lists() {
    let env = Env::new();
    let v1 = env.path("Sezon_1_Bolum_01.mkv");
    let v2 = env.path("Sezon_2_Bolum_05.mkv");
    let v3 = env.path("Belgesel_Uzay.mp4");
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("emre", "pw123", false).expect("register");

    let later = m.create_custom_list("Daha Sonra İzle").expect("later");
    let best = m.create_custom_list("En İyi Bölümler").expect("best");
    // CurrentCultureIgnoreCase (tr-TR on this machine, like the C# test): "izle" collides with "İzle".
    assert!(m.create_custom_list("daha sonra izle").is_none());
    assert!(m.create_custom_list("   ").is_none());
    assert!(!m.rename_custom_list(&best.id, "DAHA SONRA İZLE"));

    assert!(m.toggle_video_in_custom_list(&later.id, &v2));
    assert!(m.toggle_video_in_custom_list(&best.id, &v1));
    assert!(m.toggle_video_in_custom_list(&best.id, &v2));

    m.record_watch_with(&v1, 0.0, 0.0, None, None);
    assert!(m.toggle_like(&v1));
    m.record_watch_with(&v2, 0.0, 0.0, None, None);
    m.set_rating_path(&v2, 5);
    m.record_watch_with(&v3, 0.0, 0.0, None, None);

    let list_feed = feed(&m, VideoFeedFilter::CustomList(Some(best.id.clone())));
    assert_eq!(list_feed.len(), 2);
    let card = list_feed.iter().find(|c| c.video_path == v2).expect("v2 card");
    assert_eq!(card.rating, 5);
    // The trait returns list ids (the hub matches them against `lists()`), in list order.
    assert_eq!(m.lists_containing(Path::new(&v2)), vec![later.id.clone(), best.id.clone()]);
    // A blank list id = the first list.
    assert_eq!(feed(&m, VideoFeedFilter::CustomList(None)).len(), 1);

    assert!(m.clear_history());
    let t = m.totals();
    assert_eq!((t.watched, t.liked, t.rated), (0, 1, 1));
    assert!(feed(&m, VideoFeedFilter::History).is_empty());
    let liked = feed(&m, VideoFeedFilter::Liked);
    assert_eq!(liked.len(), 1);
    assert_eq!((liked[0].video_path.as_str(), liked[0].view_count), (v1.as_str(), 0));
    let top = feed(&m, VideoFeedFilter::TopRated);
    assert_eq!(top.len(), 1);
    assert_eq!((top[0].video_path.as_str(), top[0].rating), (v2.as_str(), 5));
    assert_eq!(feed(&m, VideoFeedFilter::CustomList(Some(best.id.clone()))).len(), 2);

    // Rename and delete.
    assert!(m.rename_custom_list(&later.id, "  Sonra  "));
    assert_eq!(m.get_custom_lists().iter().find(|l| l.id == later.id).map(|l| l.name.as_str()), Some("Sonra"));
    assert!(m.delete_custom_list(&later.id.to_uppercase()));
    assert!(!m.delete_custom_list(&later.id));
    assert_eq!(m.get_custom_lists().len(), 1);
    assert!(!m.toggle_video_in_custom_list(&best.id, &v1), "toggling a member removes it");
    assert!(!m.is_video_in_custom_list(&best.id, &v1));

    let r = mgr(&env.profiles(), &b);
    r.login_user("emre", "pw123", false).expect("login");
    let lists = r.get_custom_lists();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].video_paths, vec![v2.clone()]);
    assert_eq!(r.get_stats(&v1).map(|s| (s.view_count, s.is_liked)), Some((0, true)));
}

#[test]
fn feed_filters_multi_token_search_and_logged_out_lock() {
    let env = Env::new();
    let v1 = env.path("Alpha_Tutorial.mp4");
    let v2 = env.path("Beta_Documentary.mkv");
    let v3 = env.path("Gamma_Clip.mov");
    let v4 = env.path("Sezon_2_Bolum_05.mp4");
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("emre", "secret123", false).expect("register");
    let discovered: Vec<String> = vec![v1.clone(), v2.clone(), v3.clone(), v4.clone()];

    m.record_watch_with(&v1, 0.0, 0.0, None, None);
    assert!(m.toggle_like(&v1));
    m.set_rating_path(&v1, 3);
    for _ in 0..3 {
        m.record_watch_with(&v2, 0.0, 0.0, None, None);
    }
    m.set_rating_path(&v2, 5);
    m.record_watch_with(&v3, 0.0, 0.0, None, None);
    m.record_watch_with(&v3, 0.0, 0.0, None, None);
    assert!(m.toggle_like(&v3));
    m.set_rating_path(&v3, 4);

    assert_eq!(feed_paths(&m, VideoFeedFilter::MostWatched), vec![v2.clone(), v3.clone(), v1.clone()]);
    assert_eq!(feed_paths(&m, VideoFeedFilter::TopRated), vec![v2.clone(), v3.clone(), v1.clone()]);
    let liked = feed_paths(&m, VideoFeedFilter::Liked);
    assert_eq!(liked.len(), 2);
    assert!(liked.contains(&v1) && liked.contains(&v3));
    let history = feed_paths(&m, VideoFeedFilter::History);
    assert_eq!(history.len(), 3);
    assert!(!history.contains(&v4));
    let all = m.get_feed(&VideoFeedFilter::All, None, Some(&discovered), &NEVER);
    assert_eq!(all.len(), 4);
    let found = m.get_feed(&VideoFeedFilter::All, Some("sezon 2 bolum"), Some(&discovered), &NEVER);
    assert_eq!(found.iter().map(|v| v.video_path.clone()).collect::<Vec<_>>(), vec![v4.clone()]);
    assert_eq!(m.get_feed(&VideoFeedFilter::All, Some(""), Some(&discovered), &NEVER).len(), 4);
    // A cancelled feed build returns nothing.
    assert!(m.get_feed(&VideoFeedFilter::All, None, Some(&discovered), &|| false).is_empty());

    // Logged out every tab is locked, including the indexed files on "All" and in search (rule 6, Q7).
    m.logout_user();
    for f in [VideoFeedFilter::All, VideoFeedFilter::History, VideoFeedFilter::MostWatched, VideoFeedFilter::Liked] {
        assert!(m.get_feed(&f, None, Some(&discovered), &NEVER).is_empty());
        assert!(m.get_feed(&f, Some("sezon 2 bolum"), Some(&discovered), &NEVER).is_empty());
    }
    assert_eq!(m.totals(), Default::default());
}

#[test]
fn logged_out_fresh_store_shows_no_indexed_videos_on_any_tab() {
    let env = Env::new();
    let watched = env.file("Watched_Before.mp4", &[1, 2, 3]);
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    let q = |filter, search: Option<&str>| FeedQuery {
        filter,
        search: search.map(str::to_owned),
        discovered: vec![PathBuf::from(&watched)],
        ..Default::default()
    };
    for f in [VideoFeedFilter::All, VideoFeedFilter::History, VideoFeedFilter::MostWatched] {
        assert!(m.feed(&q(f, None), &NEVER).is_empty());
    }
    assert!(m.feed(&q(VideoFeedFilter::All, Some("watched")), &NEVER).is_empty());
    // Logged out nothing is fingerprinted (no file is read).
    assert!(sa_profiles::fingerprint::cached(Path::new(&watched)).is_none());
}

#[test]
fn moving_or_renaming_video_file_preserves_history_likes_ratings_and_lists() {
    let env = Env::new();
    let original = env.file("video_v1.mp4", &(0..150_000u32).map(|i| (i * 7 + i / 13) as u8).collect::<Vec<_>>());
    let expected = WinProfileStore::compute_video_id(&original);
    assert!(expected.starts_with("fp:150000:"));
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("emre", "pass123", false).expect("register");
    m.record_watch_with(&original, 65.0, 200.0, None, None);
    m.record_watch_with(&original, 120.0, 200.0, None, None);
    m.set_liked_path(&original, true);
    m.set_rating_path(&original, 5);
    let fav = m.create_custom_list("Arşivim").expect("list");
    assert!(m.add_video_to_custom_list(&fav.id, &original));

    std::fs::create_dir_all(env.dir.path().join("subfolder")).expect("subfolder");
    let moved = env.path("subfolder\\renamed_video.mp4");
    std::fs::rename(&original, &moved).expect("move");
    assert_eq!(WinProfileStore::compute_video_id(&moved), expected);

    let s = m.get_stats(&moved).expect("migrated");
    assert_eq!(s.content_fingerprint.as_deref(), Some(expected.as_str()));
    assert_eq!(s.video_path, moved);
    assert_eq!(s.file_name, "renamed_video.mp4");
    assert_eq!((s.view_count, s.is_liked, s.rating, s.last_position_seconds), (2, true, 5, 120.0));
    assert!(m.is_video_in_custom_list(&fav.id, &moved));
    let lf = feed(&m, VideoFeedFilter::CustomList(Some(fav.id.clone())));
    assert_eq!(lf.len(), 1);
    assert_eq!(lf[0].video_path, moved);
    assert_eq!((lf[0].view_count, lf[0].is_liked, lf[0].rating), (2, true, 5));

    // The move survives a reload (the migration was persisted, including the list item).
    let r = mgr(&env.profiles(), &b);
    r.login_user("emre", "pass123", false).expect("login");
    assert_eq!(r.get_stats_fast_in_memory(&moved).map(|s| s.view_count), Some(2));
    assert_eq!(r.get_custom_lists()[0].video_paths, vec![moved.clone()]);
}

#[test]
fn unplugged_drive_videos_are_hidden_and_reappear_with_full_stats() {
    let env = Env::new();
    let internal = env.file("internal_ssd_movie.mp4", &[1, 2, 3, 4]);
    let external = env.file("external_hdd_movie.mkv", &[5, 6, 7, 8]);
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("emre", "pass123", false).expect("register");
    m.record_watch_with(&internal, 0.0, 0.0, None, None);
    m.update_playback_progress_with(&internal, 45.0, 120.0, None);
    assert!(m.toggle_like(&internal));
    m.set_rating_path(&internal, 4);
    m.record_watch_with(&external, 0.0, 0.0, None, None);
    m.update_playback_progress_with(&external, 90.0, 300.0, None);
    assert!(m.toggle_like(&external));
    m.set_rating_path(&external, 5);
    let list = m.create_custom_list("HDD Arşivi").expect("list");
    assert!(m.toggle_video_in_custom_list(&list.id, &internal));
    assert!(m.toggle_video_in_custom_list(&list.id, &external));
    assert_eq!(feed(&m, VideoFeedFilter::History).len(), 2);

    let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let c = connected.clone();
    let ext = external.clone();
    m.set_storage_availability_filter(Some(Box::new(move |p: &str| c.load(Ordering::SeqCst) || !p.eq_ignore_ascii_case(&ext))));
    for f in [VideoFeedFilter::History, VideoFeedFilter::Liked, VideoFeedFilter::MostWatched, VideoFeedFilter::TopRated, VideoFeedFilter::All] {
        assert_eq!(feed_paths(&m, f), vec![internal.clone()]);
    }
    assert_eq!(feed_paths(&m, VideoFeedFilter::CustomList(Some(list.id.clone()))), vec![internal.clone()]);

    connected.store(true, Ordering::SeqCst);
    let history = feed(&m, VideoFeedFilter::History);
    assert_eq!(history.len(), 2);
    let r = history.iter().find(|v| v.video_path == external).expect("restored");
    assert_eq!((r.view_count, r.is_liked, r.rating, r.last_position_seconds), (1, true, 5, 90.0));
    assert!(m.lists_containing(Path::new(&external)).contains(&list.id));
    assert_eq!(feed(&m, VideoFeedFilter::CustomList(Some(list.id.clone()))).len(), 2);
}

#[test]
fn sqlite_backend_seven_tables_coalesced_progress_order_index_and_db_loss_survival() {
    let env = Env::new();
    let v1 = env.file("movie_1.mp4", &[10, 20, 30, 40, 50]);
    let v2 = env.file("movie_2.mkv", &[60, 70, 80, 90, 100]);
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("emre", "pass123", false).expect("register");

    let plan = m.sql_store().explain_query_plan(sa_profiles::win::sqlite::SQL_LOAD_LIST_ITEMS);
    assert!(!plan.is_empty());
    assert!(plan.iter().any(|l| l.to_lowercase().contains("idx_custom_list_items_order")), "{plan:?}");
    assert!(!plan.iter().any(|l| l.to_uppercase().contains("USE TEMP B-TREE")), "{plan:?}");

    m.record_watch_with(&v1, 10.0, 200.0, None, None);
    m.set_liked_path(&v1, true);
    m.set_rating_path(&v1, 5);
    let list = m.create_custom_list("Favori Filmler").expect("list");
    assert!(m.add_video_to_custom_list(&list.id, &v1));
    assert!(m.add_video_to_custom_list(&list.id, &v2));
    let thumb: Vec<u8> = (0..4096u32).map(|i| (i * 31) as u8).collect();
    assert!(m.set_thumbnail_with(&v1, &thumb, None));
    let upserts_before = m.total_watch_progress_sqlite_upserts();
    for i in 1..=10 {
        m.update_playback_progress_with(&v1, 10.0 + f64::from(i) * 5.0, 200.0, None);
    }
    assert!(m.sql_store().flush(FLUSH));
    assert_eq!(m.sql_store().last_write_error(), None);
    assert!(m.total_watch_progress_sqlite_upserts() - upserts_before <= 10);

    let c = m.get_sqlite_table_row_counts();
    assert_eq!((c.videos, c.watch_progress, c.custom_lists, c.custom_list_items, c.thumbnails), (1, 1, 1, 2, 1));
    let size = std::fs::metadata(env.profiles()).expect("size").len();
    assert!(size < 2048, "profiles.json is {size} bytes");

    // The profile survives losing the database (the header keeps the auth data).
    let legacy_dir = env.dir.path().join("legacy_vault_test");
    std::fs::create_dir_all(&legacy_dir).expect("dir");
    let legacy_json = legacy_dir.join("profiles.json");
    let lb = bus();
    let seed = mgr(&legacy_json, &lb);
    seed.register_user("legacy_user", "secret_pw", false).expect("seed");
    assert!(seed.flush_pending_writes(FLUSH));
    seed.dispose();
    drop(seed);
    lb.dispose();
    sa_profiles::win::flush_all_for_path(&legacy_dir.join("profiles.db"));
    for e in std::fs::read_dir(&legacy_dir).expect("ls") {
        let p = e.expect("e").path();
        if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("profiles.db")) {
            std::fs::remove_file(&p).unwrap_or_else(|err| panic!("delete {}: {err}", p.display()));
        }
    }
    let lb2 = bus();
    let migrated = mgr(&legacy_json, &lb2);
    migrated.login_user("legacy_user", "secret_pw", false).expect("login");
    migrated.record_watch_with(&v1, 33.0, 100.0, None, None);
    assert!(migrated.sql_store().flush(FLUSH));
    let c = migrated.get_sqlite_table_row_counts();
    assert_eq!((c.videos, c.watch_progress), (1, 1));
}

fn blocker(db: &Path) -> rusqlite::Connection {
    let c = rusqlite::Connection::open(db).expect("blocker connection");
    c.execute_batch("BEGIN EXCLUSIVE;").expect("BEGIN EXCLUSIVE");
    c
}

#[test]
fn sqlite_backend_detects_exclusive_database_lock_and_returns_false() {
    let env = Env::new();
    let db = env.dir.path().join("lock_test").join("locked.db");
    std::fs::create_dir_all(db.parent().unwrap()).expect("dir");
    let store = SqliteProfileStore::open(&db);
    store.enqueue_upsert_video("emre", "v1", vec![1, 2, 3], sa_profiles::Ticks::now().0);
    assert!(store.flush(Duration::from_secs(2)));
    assert_eq!(store.total_batches_committed(), 1);

    let blk = blocker(&db);
    let ok = store.import_legacy_vault_batch_and_verify("emre", &[("v2".into(), vec![4, 5, 6], 123)], &[("v2".into(), vec![7, 8, 9], 123)], &[], &[], &[]);
    assert!(!ok);
    assert!(store.last_write_error().is_some());
    let before = store.total_batches_committed();
    store.enqueue_upsert_video("emre", "v3", vec![9, 9, 9], 456);
    assert!(!store.flush(Duration::from_millis(500)));
    assert_eq!(store.total_batches_committed(), before);
    blk.execute_batch("ROLLBACK;").expect("rollback");
    drop(blk);
    // The failed batch is retried after the lock is gone.
    wait_until("retried batch", Duration::from_secs(5), || store.flush(Duration::from_millis(500)));
    assert_eq!(store.get_table_row_counts("emre").0, 2);
    store.dispose();
}

#[test]
fn sqlite_backend_preserves_failed_batch_during_temporary_lock_and_commits_both_once_unlocked() {
    let env = Env::new();
    let dir = env.dir.path().join("lock_recovery_test");
    std::fs::create_dir_all(&dir).expect("dir");
    let json = dir.join("profiles.json");
    let v1 = dir.join("ep1.mp4").to_string_lossy().into_owned();
    let v2 = dir.join("ep2.mp4").to_string_lossy().into_owned();
    std::fs::write(&v1, [1, 2, 3, 4]).expect("v1");
    std::fs::write(&v2, [5, 6, 7, 8]).expect("v2");
    let b = bus();
    let m = mgr(&json, &b);
    m.register_user("emre", "pass123", false).expect("register");
    assert!(m.flush_pending_writes(FLUSH));

    {
        let blk = blocker(&m.sqlite_db_path());
        m.record_watch_with(&v1, 25.0, 100.0, None, None);
        assert!(!m.flush_pending_writes(FLUSH));
        assert!(m.last_storage_write_error().is_some());
        blk.execute_batch("ROLLBACK;").expect("rollback");
    }
    m.record_watch_with(&v2, 50.0, 200.0, None, None);
    assert!(m.flush_pending_writes(FLUSH));
    assert_eq!(m.last_storage_write_error(), None);

    let r = mgr(&json, &b);
    r.login_user("emre", "pass123", false).expect("login");
    let s1 = r.get_stats(&v1).expect("v1");
    let s2 = r.get_stats(&v2).expect("v2");
    assert_eq!((s1.view_count, s1.last_position_seconds), (1, 25.0));
    assert_eq!((s2.view_count, s2.last_position_seconds), (1, 50.0));
}

#[test]
fn centralized_data_worker_shared_store_idempotent_request_ids_and_fast_in_memory_stats() {
    let env = Env::new();
    let dir = env.dir.path().join("centralized_worker_test");
    std::fs::create_dir_all(&dir).expect("dir");
    let json = dir.join("profiles.json");
    let video = dir.join("episode_01.mp4").to_string_lossy().into_owned();
    std::fs::write(&video, [1, 2, 3, 4, 5, 6, 7, 8]).expect("video");
    let scope = format!("centralized-worker-{}", guid());
    let b1 = scoped(&scope);
    let b2 = scoped(&scope);
    let w1 = mgr(&json, &b1);
    let w2 = mgr(&json, &b2);
    let shared = w1.sql_store();
    assert!(Arc::ptr_eq(&shared, &w2.sql_store()));
    assert!(!shared.is_disposed());

    w1.register_user("emre", "pass123", false).expect("register");
    assert!(w1.flush_pending_writes(FLUSH));
    w2.login_user("emre", "pass123", false).expect("login");

    let dup = format!("watch-req-{}", guid());
    let first = w1.record_watch_with(&video, 15.0, 120.0, Some(&dup), None).expect("first");
    let again = w1.record_watch_with(&video, 15.0, 120.0, Some(&dup), None).expect("dup");
    assert_eq!((first.view_count, again.view_count), (1, 1));

    let mut raw = StateSyncWriteRequest::new(b2.instance_id(), &w2.sqlite_db_path().to_string_lossy(), sa_sync::WriteKind::UpsertVideo);
    raw.request_id = dup.clone();
    raw.username = "emre".into();
    raw.key = WinProfileStore::compute_video_id(&video);
    raw.blob = Some(b64(&[1, 2, 3]));
    raw.updated_ticks = sa_profiles::Ticks::now().0;
    raw.view_count = 99;
    raw.view_count_delta = 1;
    b2.submit_write_request(raw);
    assert!(w1.flush_pending_writes(FLUSH));
    assert!(shared.total_duplicate_requests_ignored() >= 1);
    assert!(shared.is_request_processed(&dup));

    let second = w1.record_watch_with(&video, 45.0, 120.0, Some(&format!("watch-req-{}", guid())), None).expect("second");
    assert_eq!(second.view_count, 2);
    assert_eq!(w1.get_stats_fast_in_memory(&video).map(|s| s.view_count), Some(2));

    w1.dispose();
    b1.dispose();
    assert!(!shared.is_disposed());
    w2.record_watch_with(&video, 90.0, 120.0, None, None).expect("third");
    assert!(w2.flush_pending_writes(FLUSH));
    w2.dispose();
    b2.dispose();
    assert!(shared.is_disposed());

    let vb = scoped(&scope);
    let v = mgr(&json, &vb);
    v.login_user("emre", "pass123", false).expect("verify login");
    let s = v.get_stats(&video).expect("final");
    assert_eq!((s.view_count, s.last_position_seconds), (3, 90.0));
}

// =============================================================================================================
// ProfilePrivacyTests.cs

#[test]
fn remember_me_is_off_by_default_so_a_restart_starts_logged_out() {
    let env = Env::new();
    let video = env.path("private.mp4");
    let b = bus();
    {
        let first = mgr(&env.profiles(), &b);
        assert!(!first.remember_me());
        first.register_user("emre", "pass123", false).expect("register");
        assert!(!first.remember_me());
        first.record_watch_with(&video, 10.0, 100.0, None, None);
        assert!(first.flush_pending_writes(FLUSH));
        let json = env.json();
        assert!(!json.contains("rememberedProtectedKeyBase64") && !json.contains("rememberMeOptIn"));
        first.dispose();
    }
    let restarted = mgr(&env.profiles(), &b);
    assert!(!restarted.is_logged_in());
    assert!(!restarted.remember_me());
    assert!(feed(&restarted, VideoFeedFilter::History).is_empty());
    assert!(restarted.get_stats(&video).is_none());
}

#[test]
fn remember_me_ticked_by_user_writes_opt_in_marker_and_restores() {
    let env = Env::new();
    let b = bus();
    let first = mgr(&env.profiles(), &b);
    first.register_user("emre", "pass123", true).expect("register");
    let json: Value = serde_json::from_str(&env.json()).expect("json");
    assert_eq!(json["rememberMeOptIn"], Value::Bool(true));
    first.dispose();

    let restarted = mgr(&env.profiles(), &b);
    assert!(restarted.is_logged_in());
    assert!(restarted.remember_me());
    assert_eq!(restarted.current_username().as_deref(), Some("emre"));
}

#[test]
fn legacy_document_without_opt_in_marker_drops_remembered_session_once_and_persists() {
    let env = Env::new();
    let video = env.path("legacy.mp4");
    let b = bus();
    {
        let seed = mgr(&env.profiles(), &b);
        seed.register_user("emre", "pass123", true).expect("register");
        seed.record_watch_with(&video, 5.0, 50.0, None, None);
        assert!(seed.flush_pending_writes(FLUSH));
        seed.dispose();
    }
    let mut node: serde_json::Map<String, Value> = serde_json::from_str(&env.json()).expect("json");
    assert!(node.remove("rememberMeOptIn").is_some());
    assert!(node.contains_key("rememberedProtectedKeyBase64"));
    std::fs::write(env.profiles(), serde_json::to_string(&node).unwrap()).expect("rewrite");

    {
        let upgraded = mgr(&env.profiles(), &b);
        assert!(!upgraded.is_logged_in());
        assert!(!upgraded.remember_me());
        assert!(feed(&upgraded, VideoFeedFilter::History).is_empty());
        assert_eq!(upgraded.last_username().as_deref(), Some("emre"));
        upgraded.dispose();
    }
    let cleaned: Value = serde_json::from_str(&env.json()).expect("json");
    assert!(cleaned.get("rememberedUsername").is_none());
    assert!(cleaned.get("rememberedProtectedKeyBase64").is_none());
    assert_eq!(cleaned["rememberMe"], Value::Bool(false));

    let again = mgr(&env.profiles(), &b);
    assert!(!again.is_logged_in());
    again.login_user("emre", "pass123", false).expect("login");
    assert_eq!(again.get_stats(&video).map(|s| s.view_count), Some(1));
}

#[test]
fn logout_resets_remember_me() {
    let env = Env::new();
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    assert!(!m.remember_me());
    m.register_user("emre", "pass123", true).expect("register");
    assert!(m.remember_me());
    m.logout().expect("logout");
    assert!(!m.remember_me());
    let restarted = mgr(&env.profiles(), &b);
    assert!(!restarted.is_logged_in());
}

#[test]
fn logout_blocks_remembered_session_restore_for_the_rest_of_the_session() {
    let env = Env::new();
    let scope = format!("privacy-two-windows-{}", guid());
    let ba = scoped(&scope);
    let bb = scoped(&scope);
    let a = mgr(&env.profiles(), &ba);
    let bw = mgr(&env.profiles(), &bb);
    a.register_user("emre", "pass123", true).expect("register");
    a.logout_user();
    assert!(!a.is_logged_in());
    assert!(a.user_logged_out_this_session());

    bw.login_user("emre", "pass123", true).expect("login b");
    let mut evt = other_event(&a, Topic::Profiles, EventKind::ProfileLogin);
    evt.username = Some("emre".into());
    ba.publish(Topic::Profiles, evt);
    assert!(!a.is_logged_in());
    assert!(feed(&a, VideoFeedFilter::History).is_empty());

    a.login_user("emre", "pass123", false).expect("explicit login");
    assert!(a.is_logged_in());
}

#[test]
fn logout_when_header_write_fails_never_restores_and_persists_cleared_header_later() {
    let env = Env::new();
    let video = env.path("secret.mp4");
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("emre", "pass123", true).expect("register");
    m.record_watch_with(&video, 3.0, 30.0, None, None);
    assert!(m.flush_pending_writes(FLUSH));

    set_readonly(&env.profiles(), true);
    m.logout_user();
    assert!(!m.is_logged_in());
    assert!(env.json().contains("rememberedProtectedKeyBase64"));

    b.publish(Topic::Profiles, other_event(&m, Topic::Profiles, EventKind::ListChanged));
    assert!(!m.is_logged_in());
    assert!(m.get_stats(&video).is_none());

    set_readonly(&env.profiles(), false);
    b.publish(Topic::Profiles, other_event(&m, Topic::Profiles, EventKind::ListChanged));
    assert!(!m.is_logged_in());
    assert!(!env.json().contains("rememberedProtectedKeyBase64"));
    let restarted = mgr(&env.profiles(), &b);
    assert!(!restarted.is_logged_in());
}

#[test]
fn broker_watch_progress_and_thumbnail_events_of_another_profile_are_ignored() {
    let env = Env::new();
    let watched = env.path("watched.mp4");
    let unseen = env.path("unseen.mp4");
    let scope = format!("privacy-tests-{}", guid());
    let b = scoped(&scope);
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pass123", false).expect("alice");
    m.register_user("bob", "pass456", false).expect("bob");
    m.login_user("alice", "pass123", false).expect("login alice");
    m.record_watch_with(&watched, 10.0, 100.0, None, None);
    assert!(m.flush_pending_writes(FLUSH));

    let bob_bus = scoped(&format!("{scope}-bob"));
    let bob = mgr(&env.profiles(), &bob_bus);
    bob.login_user("bob", "pass456", false).expect("bob window");
    let other_thumb = b"BOB_THUMBNAIL".to_vec();
    b.publish(Topic::WatchProgress, bob.create_broker_event_for_tests(Topic::WatchProgress, EventKind::ProgressUpdated, Some(&watched), Some(99.0), Some(100.0), None));
    b.publish(Topic::WatchProgress, bob.create_broker_event_for_tests(Topic::WatchProgress, EventKind::ProgressUpdated, Some(&unseen), Some(50.0), Some(100.0), None));
    b.publish(Topic::Thumbnail, bob.create_broker_event_for_tests(Topic::Thumbnail, EventKind::ThumbnailCaptured, Some(&unseen), None, None, Some(&b64(&other_thumb))));

    // Plaintext (old wire format or forged) events are never trusted, even with alice's name on them.
    let plain = |topic: Topic, kind: EventKind, user: Option<&str>, path: &str, pos: Option<f64>, payload: Option<&[u8]>| {
        let mut e = other_event(&m, topic, kind);
        e.username = user.map(str::to_owned);
        e.video_path = Some(path.to_owned());
        e.position_seconds = pos;
        e.duration_seconds = pos.map(|_| 100.0);
        e.payload_base64 = payload.map(b64);
        e
    };
    b.publish(Topic::WatchProgress, plain(Topic::WatchProgress, EventKind::ProgressUpdated, Some("alice"), &watched, Some(77.0), None));
    b.publish(Topic::Thumbnail, plain(Topic::Thumbnail, EventKind::ThumbnailCaptured, Some("alice"), &watched, None, Some(&other_thumb)));
    b.publish(Topic::Thumbnail, plain(Topic::Thumbnail, EventKind::ThumbnailCaptured, None, &unseen, None, Some(&other_thumb)));

    assert_eq!(m.get_stats_fast_in_memory(&watched).map(|s| s.last_position_seconds), Some(10.0));
    assert!(m.get_stats_fast_in_memory(&unseen).is_none());
    assert!(m.get_thumbnail(&unseen).is_none());
    assert!(m.get_thumbnail(&watched).is_none());

    // Events sealed by another window unlocked with the same profile still apply silently.
    let alice_bus = scoped(&format!("{scope}-alice"));
    let alice = mgr(&env.profiles(), &alice_bus);
    alice.login_user("ALICE", "pass123", false).expect("alice window");
    assert!(m.get_thumbnail(&watched).is_none());
    let own = b"ALICE_THUMBNAIL".to_vec();
    b.publish(Topic::WatchProgress, alice.create_broker_event_for_tests(Topic::WatchProgress, EventKind::ProgressUpdated, Some(&watched), Some(42.0), Some(100.0), None));
    b.publish(Topic::Thumbnail, alice.create_broker_event_for_tests(Topic::Thumbnail, EventKind::ThumbnailCaptured, Some(&watched), None, None, Some(&b64(&own))));
    assert_eq!(m.get_stats_fast_in_memory(&watched).map(|s| s.last_position_seconds), Some(42.0));
    assert_eq!(m.get_thumbnail(&watched), Some(own));
}

// =============================================================================================================
// WatchHistoryPrivacyTests.cs (SQLite at rest, memory after logout/switch, IPC, plaintext files)

#[test]
fn sqlite_holds_no_plaintext_paths_counts_or_timestamps() {
    let env = Env::new();
    let token = format!("PrivateClip_{}", guid());
    let small = env.file(&format!("{token}.mp4"), &[0u8; 4096]);
    let missing = env.path(&format!("{token}_large.mkv"));
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pass123", false).expect("register");
    m.record_watch_with(&small, 10.0, 100.0, None, None);
    m.record_watch_with(&small, 20.0, 100.0, None, None);
    m.record_watch_with(&missing, 5.0, 50.0, None, None);
    m.update_playback_progress_with(&small, 33.0, 100.0, None);
    assert!(m.set_thumbnail_with(&missing, &[1, 2, 3, 4], None));
    assert!(m.flush_pending_writes(FLUSH));

    let store = m.sql_store();
    let rows = store.dump_plaintext_video_columns();
    assert!(!rows.is_empty());
    for (_, _, key, ticks, vc) in &rows {
        assert!(key.starts_with("hk:"), "{key}");
        assert!(!key.to_lowercase().contains(&token.to_lowercase()) && !key.contains("fp:"));
        assert_eq!((*ticks, *vc), (0, -1));
    }
    for (user, ticks) in store.dump_processed_requests() {
        assert_eq!((user.as_str(), ticks), ("", 0));
    }
    let owned = store.dump_owners_and_blob_lengths();
    assert!(!owned.is_empty());
    for (_, owner, _) in &owned {
        assert!(owner.starts_with("ot:") && !owner.to_lowercase().contains("alice"));
    }
    for table in ["videos", "watch_progress"] {
        let mut lens: Vec<i64> = owned.iter().filter(|r| r.0 == table).map(|r| r.2).collect();
        lens.dedup();
        assert_eq!(lens.len(), 1, "{table} blobs padded to one size: {lens:?}");
    }
    assert_eq!(store.cached_thumbnail_blob_count(), 0);
    for f in [m.sqlite_db_path(), PathBuf::from(format!("{}-wal", m.sqlite_db_path().display()))] {
        if let Ok(bytes) = std::fs::read(&f) {
            assert!(!bytes.windows(token.len()).any(|w| w == token.as_bytes()), "{} contains a plaintext path", f.display());
        }
    }

    m.logout_user();
    assert!(m.get_stats(&small).is_none());
    m.login_user("alice", "pass123", false).expect("login");
    assert_eq!(m.get_stats(&small).map(|s| (s.view_count, s.last_position_seconds)), Some((2, 33.0)));
    assert_eq!(m.get_stats(&missing).map(|s| s.view_count), Some(1));
}

#[test]
fn clear_watch_history_removes_thumbnails_and_keeps_only_ownerless_request_ids() {
    let env = Env::new();
    let video = env.file("cleared.mp4", &[0u8; 2048]);
    let card = env.path("only_shown_as_card.mp4");
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pass123", false).expect("register");
    m.record_watch_with(&video, 5.0, 50.0, None, None);
    for i in 1..=20 {
        m.update_playback_progress_with(&video, 5.0 + f64::from(i), 50.0, None);
    }
    assert!(m.set_thumbnail_with(&video, &[9, 9, 9], None));
    assert!(m.set_thumbnail_with(&card, &[8, 8, 8], None));
    assert!(m.flush_pending_writes(FLUSH));
    assert!(m.sql_store().dump_processed_requests().len() >= 20);

    assert!(m.clear_history());
    assert!(m.flush_pending_writes(FLUSH));
    let c = m.get_sqlite_table_row_counts();
    assert_eq!((c.thumbnails, c.videos, c.watch_progress), (0, 0, 0));
    for (user, ticks) in m.sql_store().dump_processed_requests() {
        assert_eq!((user.as_str(), ticks), ("", 0));
    }
    assert!(m.get_thumbnail(&video).is_none());
}

#[test]
fn resent_write_after_clear_history_is_still_a_duplicate() {
    let env = Env::new();
    let store = SqliteProfileStore::open(&env.dir.path().join("resend.db"));
    let owner = "ot:0123456789abcdef";
    let watch = || WriteItem::new(WriteKind::UpsertVideo).with_user_key(owner, "hk:1").with_blob(vec![1, 2, 3]).with_request_id("watch-1");
    store.enqueue_item(watch());
    assert!(store.flush(FLUSH));
    assert_eq!(store.get_table_row_counts(owner).0, 1);
    store.enqueue_item(WriteItem::new(WriteKind::ReplaceAllVideosAndProgress).with_user_key(owner, "").with_request_id("clear-1"));
    assert!(store.flush(FLUSH));
    assert_eq!(store.get_table_row_counts(owner).0, 0);
    store.enqueue_item(watch());
    assert!(store.flush(FLUSH));
    assert_eq!(store.get_table_row_counts(owner).0, 0);
    assert!(store.total_duplicate_requests_ignored() >= 1);
    store.dispose();
}

#[test]
fn privacy_vacuum_that_fails_on_a_locked_database_is_retried_at_the_next_open() {
    let env = Env::new();
    let db = env.dir.path().join("vacuum.db");
    let first = SqliteProfileStore::open(&db);
    let second = SqliteProfileStore::open(&db);
    assert!(first.is_privacy_vacuum_done_for_tests());
    assert!(first.execute_raw_for_tests("DELETE FROM meta WHERE key = 'privacy_vacuum';"));
    assert!(second.execute_raw_for_tests("BEGIN IMMEDIATE;"));
    first.run_startup_privacy_steps_for_tests();
    assert!(!first.is_privacy_vacuum_done_for_tests());
    assert!(second.execute_raw_for_tests("ROLLBACK;"));
    first.run_startup_privacy_steps_for_tests();
    assert!(first.is_privacy_vacuum_done_for_tests());
    first.dispose();
    second.dispose();
}

#[test]
fn rows_owned_by_a_plain_profile_name_are_retagged_at_login() {
    let env = Env::new();
    let video = env.file("legacy_owner.mp4", &[0u8; 2048]);
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pass123", false).expect("register");
    m.record_watch_with(&video, 10.0, 100.0, None, None);
    assert!(m.set_thumbnail_with(&video, &[5, 6, 7], None));
    assert!(m.flush_pending_writes(FLUSH));
    m.logout_user();
    assert!(m.sql_store().execute_raw_for_tests(
        "UPDATE videos SET username = 'alice'; UPDATE watch_progress SET username = 'alice'; UPDATE thumbnails SET username = 'alice';"
    ));

    m.login_user("alice", "pass123", false).expect("login");
    assert_eq!(m.get_stats(&video).map(|s| (s.view_count, s.last_position_seconds)), Some((1, 10.0)));
    assert!(m.flush_pending_writes(FLUSH));
    let owned = m.sql_store().dump_owners_and_blob_lengths();
    assert!(!owned.is_empty());
    assert!(owned.iter().all(|r| r.1.starts_with("ot:")), "{owned:?}");
    let c = m.get_sqlite_table_row_counts();
    assert_eq!((c.videos, c.thumbnails), (1, 1));
}

#[test]
fn legacy_plaintext_columns_are_scrubbed_without_a_key_and_rekeyed_at_next_login() {
    let env = Env::new();
    let token = format!("LegacyClip_{}", guid());
    let video = env.file(&format!("{token}.mp4"), &[0u8; 4096]);
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pass123", false).expect("register");
    m.record_watch_with(&video, 10.0, 100.0, None, None);
    m.record_watch_with(&video, 12.0, 100.0, None, None);
    assert!(m.flush_pending_writes(FLUSH));
    m.logout_user();

    let escaped = video.replace('\'', "''");
    assert!(m.sql_store().execute_raw_for_tests(&format!(
        "UPDATE videos SET video_key = '{escaped}', view_count = 2, updated_at_ticks = 638000000000000000;
         UPDATE watch_progress SET video_key = '{escaped}', updated_at_ticks = 638000000000000000;
         INSERT INTO processed_requests (request_id, username, processed_at_ticks) VALUES ('legacy', 'alice', 638000000000000000);"
    )));
    m.sql_store().rerun_privacy_migration_for_tests();
    for (_, _, key, ticks, vc) in m.sql_store().dump_plaintext_video_columns() {
        assert!(key.starts_with("rk:"), "{key}");
        assert!(!key.to_lowercase().contains(&token.to_lowercase()));
        assert_eq!((ticks, vc), (0, -1));
    }
    assert!(m.sql_store().dump_processed_requests().is_empty());
    assert!(m.sql_store().is_privacy_vacuum_done_for_tests());

    m.login_user("alice", "pass123", false).expect("login");
    assert_eq!(m.get_stats(&video).map(|s| (s.view_count, s.last_position_seconds)), Some((2, 12.0)));
    assert!(m.flush_pending_writes(FLUSH));
    let rekeyed = m.sql_store().dump_plaintext_video_columns();
    assert!(!rekeyed.is_empty());
    assert!(rekeyed.iter().all(|r| r.2.starts_with("hk:")), "{rekeyed:?}");
}

#[test]
fn logout_in_another_window_drops_every_decrypted_copy_and_key() {
    let env = Env::new();
    let video = env.path("two_windows.mp4");
    let b = bus();
    let a = mgr(&env.profiles(), &b);
    let bw = mgr(&env.profiles(), &b);
    a.register_user("alice", "pass123", false).expect("register");
    bw.login_user("alice", "pass123", false).expect("login");
    bw.record_watch_with(&video, 10.0, 100.0, None, None);
    assert!(bw.set_thumbnail_with(&video, &[1, 2, 3], None));
    assert!(bw.flush_pending_writes(FLUSH));
    assert!(bw.decrypted_item_count_for_tests() > 0);

    a.logout_user();
    assert!(!bw.is_logged_in());
    assert_eq!(bw.unlocked_profile_count_for_tests(), 0);
    assert_eq!(bw.decrypted_item_count_for_tests(), 0);
    assert_eq!(a.decrypted_item_count_for_tests(), 0);
    assert!(bw.get_stats(&video).is_none());
    assert_eq!(bw.sql_store().cached_thumbnail_blob_count(), 0);
}

#[test]
fn profile_switch_ends_the_previous_session_and_logout_forgets_watched_paths() {
    let env = Env::new();
    let video = env.file("fingerprinted.mp4", &[0u8; 8192]);
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("bob", "pass456", false).expect("bob");
    m.register_user("alice", "pass123", false).expect("alice");
    m.record_watch_with(&video, 10.0, 100.0, None, None);
    // (The cache is process-wide and other tests end sessions in parallel, so only its clearing is asserted.)
    assert!(m.get_stats(&video).and_then(|s| s.content_fingerprint).is_some());
    assert!(m.flush_pending_writes(FLUSH));
    let generation = m.session_generation();

    m.login_user("bob", "pass456", false).expect("switch");
    assert_ne!(m.session_generation(), generation);
    assert_eq!(m.unlocked_profile_count_for_tests(), 1);
    assert_eq!(m.decrypted_item_count_for_tests(), 0);
    assert!(m.get_stats(&video).is_none());

    m.logout_user();
    assert_eq!(m.unlocked_profile_count_for_tests(), 0);
    assert!(sa_profiles::fingerprint::cached(Path::new(&video)).is_none());
    m.login_user("alice", "pass123", false).expect("alice again");
    assert_eq!(m.get_stats(&video).map(|s| s.view_count), Some(1));
}

#[test]
fn work_captured_in_a_previous_session_never_lands_in_the_next_profile() {
    let env = Env::new();
    let x = env.path("opened_by_alice.mp4");
    let y = env.path("debounced.mp4");
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("bob", "pass456", false).expect("bob");
    m.register_user("alice", "pass123", false).expect("alice");
    let alice_token = m.session_token();
    m.logout_user();
    m.login_user("bob", "pass456", false).expect("bob login");

    assert!(m.record_watch_with(&y, 0.0, 0.0, None, Some(alice_token)).is_none());
    assert!(!m.set_thumbnail_with(&x, &[1, 2, 3], Some(alice_token)));
    assert!(m.update_playback_progress_with(&x, 31.0, 100.0, Some(alice_token)).is_none());
    assert!(m.get_stats(&x).is_none());
    assert!(m.get_stats(&y).is_none());

    let bob_token = m.session_token();
    assert_eq!(m.record_watch_with(&y, 0.0, 0.0, None, Some(bob_token)).map(|s| s.view_count), Some(1));
    assert_eq!(m.get_stats(&y).map(|s| s.view_count), Some(1));
}

#[test]
fn broker_events_carry_no_plaintext_path_position_or_username() {
    let env = Env::new();
    let token = format!("PipeClip_{}", guid());
    let video = env.path(&format!("{token}.mp4"));
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pass123", false).expect("register");
    let events = [
        m.create_broker_event_for_tests(Topic::WatchProgress, EventKind::ProgressUpdated, Some(&video), Some(1234.5), Some(5000.0), None),
        m.create_broker_event_for_tests(Topic::Thumbnail, EventKind::ThumbnailCaptured, Some(&video), None, None, Some(&b64(token.as_bytes()))),
        m.create_broker_event_for_tests(Topic::Profiles, EventKind::VideoWatched, Some(&video), None, None, None),
        m.create_broker_event_for_tests(Topic::Profiles, EventKind::ProfileLogin, None, None, None, None),
    ];
    for e in &events {
        assert!(e.video_path.is_none() && e.username.is_none() && e.position_seconds.is_none());
        assert!(e.duration_seconds.is_none() && e.view_count.is_none() && e.payload_base64.is_none());
        let json = serde_json::to_string(e).expect("json");
        assert!(!json.to_lowercase().contains(&token.to_lowercase()));
        assert!(!json.to_lowercase().contains("alice"));
        assert!(!json.contains("1234.5"));
    }
    assert!(events[0].sealed_payload_base64.is_some() && events[1].sealed_payload_base64.is_some());
}

#[test]
fn remember_me_is_wrapped_with_dpapi_not_with_user_and_machine_names() {
    let env = Env::new();
    let b = bus();
    {
        let m = mgr(&env.profiles(), &b);
        m.register_user("alice", "pass123", true).expect("register");
        m.dispose();
    }
    let header: Value = serde_json::from_str(&env.json()).expect("json");
    assert!(header["rememberedProtectedKeyBase64"].as_str().unwrap().starts_with("dpapi1:"));
    let restarted = mgr(&env.profiles(), &b);
    assert!(restarted.is_logged_in());
}

#[test]
fn a_legacy_user_and_machine_wrapped_remembered_key_restores_and_is_rewrapped_with_dpapi() {
    let env = Env::new();
    let b = bus();
    let salt;
    let key;
    {
        let m = mgr(&env.profiles(), &b);
        m.register_user("alice", "pass123", true).expect("register");
        key = m.active_vault_key().expect("key").1;
        m.dispose();
    }
    let mut doc: serde_json::Map<String, Value> = serde_json::from_str(&env.json()).expect("json");
    salt = base64::engine::general_purpose::STANDARD.decode(doc["profiles"][0]["saltBase64"].as_str().unwrap()).unwrap();
    let wrap = sa_profiles::win::legacy_remember_wrap_key(
        &salt,
        &sa_profiles::win::netcompat::environment_user_name(),
        &sa_profiles::win::netcompat::environment_machine_name(),
    );
    let legacy = b64(&sa_profiles::crypto::seal(key.expose(), &wrap));
    assert_eq!(legacy.len(), 80);
    doc.insert("rememberedProtectedKeyBase64".into(), Value::String(legacy));
    std::fs::write(env.profiles(), serde_json::to_string_pretty(&doc).unwrap()).expect("write");

    let restarted = mgr(&env.profiles(), &b);
    assert!(restarted.is_logged_in());
    let header: Value = serde_json::from_str(&env.json()).expect("json");
    assert!(header["rememberedProtectedKeyBase64"].as_str().unwrap().starts_with("dpapi1:"), "re-wrapped");
}

// =============================================================================================================
// EditingAndCatalogTests.MultiInstanceConcurrentStoresAndCleanupNeverThrowOrCorrupt (profile part)

#[test]
fn multi_instance_concurrent_registrations_never_corrupt_the_header() {
    let env = Env::new();
    let b = bus();
    let a = mgr(&env.profiles(), &b);
    let bm = mgr(&env.profiles(), &b);
    let handles: Vec<_> = (0..12)
        .map(|i| {
            let (a, bm) = (a.clone(), bm.clone());
            std::thread::spawn(move || {
                let _ = a.register_user(&format!("userA_{i}"), "pass1234", false);
                let _ = bm.register_user(&format!("userB_{i}"), "pass1234", false);
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread");
    }
    let doc: Value = serde_json::from_str(&env.json()).expect("valid JSON");
    assert_eq!(doc["version"], Value::from(3));
    let fresh = mgr(&env.profiles(), &bus());
    assert_eq!(fresh.registered_usernames().len(), 24);
}

// =============================================================================================================
// Rust-side decisions (UNIFIED.md §3.11, §3.12, §4)

#[test]
fn trait_surface_maps_onto_the_windows_store() {
    let env = Env::new();
    let video = env.file("trait.mp4", &[1, 2, 3, 4, 5]);
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    let store: &dyn ProfileStore = m.as_ref();
    let pw = SecretString::new("pw".to_owned());
    let changes = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let c2 = changes.clone();
    let _sub = store.subscribe_changes(Box::new(move |c| c2.lock().push(c)));
    store.register("trait_user", &pw, false).expect("register");
    assert_eq!(changes.lock().as_slice(), &[ProfileChange::Local]);
    assert_eq!(store.record_watch(Path::new(&video), 1.0, 10.0).map(|s| s.view_count), Some(1));
    assert_eq!(store.set_rating(Path::new(&video), 9).map(|s| s.rating), Some(5));
    assert!(store.set_liked(Path::new(&video), true).is_some_and(|s| s.is_liked));
    let list = store.create_list("Liste").expect("ok").expect("created");
    assert_eq!(store.create_list("liste").expect("ok"), None, "Windows: silent duplicate");
    assert!(store.set_in_list(&list.id, Path::new(&video), true));
    assert!(!store.toggle_in_list(&list.id, Path::new(&video)));
    assert!(store.lists_containing(Path::new(&video)).is_empty());
    assert!(store.rename_list(&list.id, "Yeni").expect("ok"));
    assert!(store.delete_list(&list.id));
    assert!(store.stats_fast_in_memory(Path::new(&video)).is_some());
    assert!(store.stats(Path::new(&video), sa_profiles::FingerprintMode::PathOnly).is_some());
    assert!(store.flush_playback_progress());
    assert_eq!(store.active_vault_key().map(|k| k.0), Some("trait_user".to_owned()));
    store.logout().expect("logout");
    assert!(store.active_vault_key().is_none());
    assert!(store.delete_profile("trait_user", &SecretString::new("bad".to_owned())).is_err());
    store.delete_profile("trait_user", &pw).expect("delete");
    assert!(store.registered_usernames().is_empty());
    assert_eq!(store.last_username().as_deref(), Some("trait_user"), "last username survives delete");
}

#[test]
fn delete_profile_removes_header_entry_and_every_row() {
    let env = Env::new();
    let video = env.file("deleted.mp4", &[1, 2, 3]);
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("gone", "pw", true).expect("register");
    m.record_watch_with(&video, 1.0, 10.0, None, None);
    assert!(m.set_thumbnail_with(&video, &[1], None));
    let list = m.create_custom_list("L").expect("list");
    assert!(m.add_video_to_custom_list(&list.id, &video));
    assert!(m.flush_pending_writes(FLUSH));
    let owner_rows = m.sql_store().dump_owners_and_blob_lengths().len();
    assert!(owner_rows >= 4);

    m.delete_user("GONE", "pw").expect("delete");
    assert!(!m.is_logged_in());
    assert!(m.flush_pending_writes(FLUSH));
    assert!(m.sql_store().dump_owners_and_blob_lengths().is_empty());
    let header: Value = serde_json::from_str(&env.json()).expect("json");
    assert_eq!(header["profiles"].as_array().map(Vec::len), Some(0));
    assert!(header.get("rememberedProtectedKeyBase64").is_none());
    m.register_user("gone", "pw2", false).expect("the name is free again");
    assert!(m.get_stats(&video).is_none(), "a new profile with the same name sees nothing");
}

#[test]
fn deleting_the_active_profile_here_or_in_another_window_forgets_watched_paths() {
    // UNIFIED.md §3.12: the process-wide fingerprint cache is cleared whenever a session ends, delete included.
    let env = Env::new();
    let video = env.file("deleted_fp.mp4", &[7u8; 4096]);
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    // C# checks roots starting with two backslashes with `Directory.Exists`, which is true for `\\?\C:\`.
    assert!(m.is_video_storage_available(r"\\?\C:\not-there.mp4"));
    assert!(m.is_video_storage_available(r"\\.\C:\not-there.mp4"));
    m.register_user("gone", "pw", false).expect("register");
    m.record_watch_with(&video, 1.0, 10.0, None, None);
    // (The cache is process-wide and other tests end sessions in parallel, so only its clearing is asserted.)
    m.delete_user("gone", "pw").expect("delete");
    assert!(!sa_profiles::fingerprint::is_cached(Path::new(&video)));
    assert_eq!(m.unlocked_profile_count_for_tests(), 0);
    assert_eq!(m.decrypted_item_count_for_tests(), 0);

    let other = env.file("deleted_elsewhere.mp4", &[8u8; 4096]);
    let a = mgr(&env.profiles(), &b);
    let w = mgr(&env.profiles(), &b);
    a.register_user("bye", "pw", false).expect("register");
    w.login_user("bye", "pw", false).expect("login in the second window");
    w.record_watch_with(&other, 1.0, 10.0, None, None);
    assert!(w.flush_pending_writes(FLUSH));
    a.delete_user("bye", "pw").expect("delete in the first window");
    assert!(a.flush_pending_writes(FLUSH));
    wait_until("the second window drops the deleted profile", Duration::from_secs(5), || !w.is_logged_in());
    assert!(!sa_profiles::fingerprint::is_cached(Path::new(&other)));
    assert_eq!(w.unlocked_profile_count_for_tests(), 0);
    assert_eq!(w.decrypted_item_count_for_tests(), 0);
}

#[test]
fn vanished_header_ends_the_session() {
    // Q9 fix: a missing profiles.json on a forced resync also clears the active profile.
    let env = Env::new();
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pw", false).expect("register");
    std::fs::remove_file(env.profiles()).expect("delete header");
    b.publish(Topic::Profiles, other_event(&m, Topic::Profiles, EventKind::ListChanged));
    assert!(!m.is_logged_in());
    assert!(m.active_vault_key().is_none());
    assert_eq!(m.decrypted_item_count_for_tests(), 0);
}

#[test]
fn path_keyed_rows_get_their_fingerprint_once_the_file_is_reachable() {
    // UNIFIED.md §4 item 7: rows of unreachable files stay path-keyed; at the next login with the file present they
    // are re-keyed to the fingerprint (MigrateVideoKey), and a moved file then keeps its history.
    let env = Env::new();
    let video = env.path("arrives_later.mp4");
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pw", false).expect("register");
    m.record_watch_with(&video, 7.0, 70.0, None, None);
    assert_eq!(m.get_stats(&video).and_then(|s| s.content_fingerprint), None);
    assert!(m.flush_pending_writes(FLUSH));
    m.logout_user();

    std::fs::write(&video, (0..3000u32).map(|i| i as u8).collect::<Vec<_>>()).expect("file arrives");
    m.login_user("alice", "pw", false).expect("login");
    let fp = WinProfileStore::compute_video_id(&video);
    assert!(fp.starts_with("fp:3000:"));
    assert_eq!(m.get_stats_fast_in_memory(&video).and_then(|s| s.content_fingerprint), Some(fp.clone()));
    assert!(m.flush_pending_writes(FLUSH));

    let moved = env.path("renamed_later.mp4");
    std::fs::rename(&video, &moved).expect("move");
    let r = mgr(&env.profiles(), &b);
    r.login_user("alice", "pw", false).expect("login again");
    let s = r.get_stats(&moved).expect("found by fingerprint");
    assert_eq!((s.view_count, s.last_position_seconds, s.content_fingerprint), (1, 7.0, Some(fp)));
    assert_eq!(r.get_sqlite_table_row_counts().videos, 1);
}

#[test]
fn large_files_are_fingerprinted_before_a_row_is_created() {
    // UNIFIED.md §3.12 FIX-W: no path-keyed row for a fingerprintable file, whatever its size (the C# app keyed
    // files over 64 MiB by path first).
    let env = Env::new();
    let big = env.path("big.mkv");
    let f = std::fs::File::create(&big).expect("create");
    f.set_len(65 * 1024 * 1024).expect("sparse size");
    drop(f);
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pw", false).expect("register");
    let s = m.record_watch_with(&big, 1.0, 100.0, None, None).expect("watch");
    assert!(s.content_fingerprint.as_deref().is_some_and(|fp| fp.starts_with(&format!("fp:{}:", 65 * 1024 * 1024))));
}

#[test]
fn record_watch_sanitizes_positions_and_never_resets_a_stored_position_with_zero() {
    let env = Env::new();
    let v = env.path("sanitize.mp4");
    let b = bus();
    let m = mgr(&env.profiles(), &b);
    m.register_user("alice", "pw", false).expect("register");
    let s = m.record_watch_with(&v, 150.0, 100.0, None, None).expect("clamped");
    assert_eq!((s.last_position_seconds, s.duration_seconds), (100.0, 100.0));
    let s = m.record_watch_with(&v, 0.0, f64::NAN, None, None).expect("fallback");
    assert_eq!((s.last_position_seconds, s.duration_seconds, s.view_count), (100.0, 100.0, 2));
    // Progress below the 0.5 s threshold is not persisted; above it is.
    m.update_playback_progress_with(&v, 40.0, 100.0, None);
    m.update_playback_progress_with(&v, 40.3, 100.0, None);
    assert!(m.flush_pending_writes(FLUSH));
    let r = mgr(&env.profiles(), &b);
    r.login_user("alice", "pw", false).expect("login");
    assert_eq!(r.get_stats(&v).map(|s| s.last_position_seconds), Some(40.0));
}

#[test]
fn search_query_matches_whole_query_or_every_token() {
    let mk = |path: &str| sa_profiles::VideoUserStats {
        content_fingerprint: None,
        video_path: path.into(),
        file_name: sa_profiles::win::paths::file_name(path),
        directory_path: sa_profiles::win::paths::directory_name(path),
        view_count: 0,
        is_liked: false,
        rating: 0,
        last_position_seconds: 0.0,
        duration_seconds: 0.0,
        last_watched_at: None,
        updated_at: sa_profiles::Ticks::now(),
        thumbnail: None,
    };
    let v = mk("C:\\Diziler\\Sezon_2_Bolum_05.mp4");
    let m = sa_profiles::win::matches_search_query;
    assert!(m(&v, "sezon 2 bolum"));
    assert!(m(&v, "  "));
    assert!(m(&v, "DİZİLER"), "Turkish culture casing on this machine");
    assert!(m(&v, "bolum_05"));
    assert!(!m(&v, "sezon 3"));
    assert!(m(&v, "- . _"), "no tokens = match");
}
