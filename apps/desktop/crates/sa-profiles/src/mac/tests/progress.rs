//! `ProfileProgressTests` (12): playback progress is applied in memory at once but written at most every 15 s and on
//! an explicit flush.

use super::*;
use crate::mac::fileio::FileLock;
use crate::{FingerprintMode, ProfileError, ProfileStore};

struct Fixture {
    folder: TestFolder,
    clock: ManualClock,
    secrets: Arc<TestSecrets>,
}

impl Fixture {
    fn new() -> Self {
        Fixture { folder: TestFolder::new(), clock: ManualClock::new(), secrets: TestSecrets::new() }
    }
    fn video(&self) -> PathBuf {
        self.folder.video("movie.mp4")
    }
    fn make_store(&self, lock_timeout: Duration) -> Arc<MacProfileStore> {
        self.folder.store(StoreOpts {
            secrets: self.secrets.clone(),
            lock_timeout,
            clock: Some(self.clock.clock()),
            ..StoreOpts::default()
        })
    }
    fn store(&self) -> Arc<MacProfileStore> {
        self.make_store(Duration::from_secs(5))
    }
    fn logged_in(&self) -> Arc<MacProfileStore> {
        let store = self.store();
        store.register_user("emre", "pw", true).unwrap();
        store
    }
    fn data(&self) -> Vec<u8> {
        self.folder.file_data()
    }
    fn position(&self, store: &MacProfileStore) -> Option<f64> {
        store.stats_mode(&self.video(), FingerprintMode::Compute).map(|s| s.last_position_seconds)
    }
}

#[test]
fn progress_is_written_at_most_every_fifteen_seconds() {
    let f = Fixture::new();
    let store = f.logged_in();
    store.try_record_watch(&f.video(), 1.0, 600.0).unwrap();
    let mut previous = f.data();
    let mut writes = Vec::new();
    for second in 1..=44 {
        f.clock.advance(1.0);
        store.try_update_playback_progress(&f.video(), 1.0 + f64::from(second), 600.0).unwrap();
        let current = f.data();
        if current != previous {
            writes.push(second);
            previous = current;
        }
        assert_eq!(f.position(&store), Some(1.0 + f64::from(second)), "readers see every tick at once");
    }
    assert_eq!(writes, [15, 30], "one write per 15 seconds, not one per tick");
}

#[test]
fn the_first_progress_of_a_fresh_session_is_written_at_once_then_throttled() {
    let f = Fixture::new();
    let _ = f.logged_in();
    let fresh = f.store();
    assert!(fresh.is_logged_in());
    let before = f.data();
    fresh.try_update_playback_progress(&f.video(), 20.0, 600.0).unwrap();
    let first = f.data();
    assert_ne!(first, before);
    f.clock.advance(1.0);
    fresh.try_update_playback_progress(&f.video(), 25.0, 600.0).unwrap();
    assert_eq!(f.data(), first);
}

#[test]
fn flush_writes_the_latest_position_at_once_and_a_second_flush_is_a_no_op() {
    let f = Fixture::new();
    let store = f.logged_in();
    store.try_record_watch(&f.video(), 5.0, 600.0).unwrap();
    let saved = f.data();
    assert!(!store.has_unsaved_changes());
    f.clock.advance(1.0);
    store.try_update_playback_progress(&f.video(), 42.0, 600.0).unwrap();
    assert_eq!(f.data(), saved, "within the interval nothing is written");
    assert!(store.has_unsaved_changes());
    assert!(store.try_flush_playback_progress().unwrap());
    assert!(!store.has_unsaved_changes());
    let flushed = f.data();
    assert_ne!(flushed, saved);
    assert!(!store.try_flush_playback_progress().unwrap());
    assert_eq!(f.data(), flushed);
    let reopened = f.store();
    assert!(reopened.is_logged_in());
    assert_eq!(f.position(&reopened), Some(42.0));
    assert_eq!(reopened.stats_mode(&f.video(), FingerprintMode::Compute).unwrap().view_count, 1);
}

#[test]
fn progress_and_flushes_never_raise_the_local_signal() {
    let f = Fixture::new();
    let store = f.logged_in();
    store.try_record_watch(&f.video(), 5.0, 600.0).unwrap();
    let log = ChangeLog::new(&store);
    for second in 1..=20 {
        f.clock.advance(1.0);
        store.try_update_playback_progress(&f.video(), 5.0 + f64::from(second), 600.0).unwrap();
    }
    store.try_flush_playback_progress().unwrap();
    store.try_persist_pending_migrations().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert!(log.events().is_empty());
}

#[test]
fn any_other_write_carries_the_unsaved_progress_along() {
    let f = Fixture::new();
    let store = f.logged_in();
    store.try_record_watch(&f.video(), 5.0, 600.0).unwrap();
    f.clock.advance(1.0);
    store.try_update_playback_progress(&f.video(), 300.0, 600.0).unwrap();
    assert!(store.has_unsaved_changes());
    store.try_set_rating(&f.folder.video("other.mp4"), 4).unwrap();
    assert!(!store.has_unsaved_changes());
    assert_eq!(f.position(&f.store()), Some(300.0));
}

#[test]
fn unsaved_progress_sits_on_top_of_another_instances_write() {
    let f = Fixture::new();
    let a = f.logged_in();
    a.try_record_watch(&f.video(), 5.0, 600.0).unwrap();
    let b = f.store();
    assert!(b.is_logged_in());
    f.clock.advance(1.0);
    a.try_update_playback_progress(&f.video(), 200.0, 600.0).unwrap();
    assert!(a.has_unsaved_changes());
    f.clock.advance(1.0);
    b.try_set_liked(&f.video(), true).unwrap();
    let seen = a.stats_mode(&f.video(), FingerprintMode::Compute).unwrap();
    assert!(seen.is_liked, "the other instance's change arrives");
    assert_eq!(seen.last_position_seconds, 200.0, "and does not roll the unsaved resume point back");
    assert!(a.has_unsaved_changes());
    a.try_flush_playback_progress().unwrap();
    let from_b = b.stats_mode(&f.video(), FingerprintMode::Compute).unwrap();
    assert_eq!(from_b.last_position_seconds, 200.0);
    assert!(from_b.is_liked);
}

#[test]
fn a_failed_throttled_write_keeps_the_progress_and_backs_off_for_the_interval() {
    let f = Fixture::new();
    let store = f.make_store(Duration::from_millis(100));
    store.register_user("emre", "pw", true).unwrap();
    store.try_record_watch(&f.video(), 1.0, 600.0).unwrap();
    f.clock.advance(20.0);
    let before = f.data();
    let mut held = FileLock::acquire(store.lock_path(), Duration::from_secs(1)).unwrap();
    assert_eq!(store.try_update_playback_progress(&f.video(), 50.0, 600.0), Err(ProfileError::LockUnavailable));
    assert_eq!(f.position(&store), Some(50.0), "the progress is kept in memory");
    assert!(store.has_unsaved_changes());
    for tick in 1..=5 {
        f.clock.advance(1.0);
        store.try_update_playback_progress(&f.video(), 50.0 + f64::from(tick), 600.0).unwrap();
    }
    held.release();
    assert_eq!(f.data(), before);
    f.clock.advance(15.0);
    store.try_update_playback_progress(&f.video(), 90.0, 600.0).unwrap();
    assert_ne!(f.data(), before);
    assert_eq!(f.position(&f.store()), Some(90.0));
}

#[test]
fn logging_out_writes_the_progress_first() {
    let f = Fixture::new();
    let store = f.logged_in();
    store.try_record_watch(&f.video(), 1.0, 600.0).unwrap();
    f.clock.advance(1.0);
    store.try_update_playback_progress(&f.video(), 333.0, 600.0).unwrap();
    store.logout_user().unwrap();
    assert!(!store.has_unsaved_changes());
    store.login_user("emre", "pw", true).unwrap();
    assert_eq!(f.position(&store), Some(333.0));
}

#[test]
fn switching_profiles_writes_the_progress_of_the_one_that_was_logged_in() {
    let f = Fixture::new();
    let store = f.logged_in();
    store.register_user("other", "pw2", false).unwrap();
    store.login_user("emre", "pw", false).unwrap();
    store.try_record_watch(&f.video(), 1.0, 600.0).unwrap();
    f.clock.advance(1.0);
    store.try_update_playback_progress(&f.video(), 77.0, 600.0).unwrap();
    assert!(store.has_unsaved_changes());
    store.login_user("other", "pw2", false).unwrap();
    assert!(!store.has_unsaved_changes());
    f.clock.advance(1.0);
    store.login_user("emre", "pw", false).unwrap();
    assert_eq!(f.position(&store), Some(77.0));
    f.clock.advance(1.0);
    store.try_update_playback_progress(&f.video(), 88.0, 600.0).unwrap();
    store.register_user("third", "pw3", false).unwrap();
    store.login_user("emre", "pw", false).unwrap();
    assert_eq!(f.position(&store), Some(88.0));
}

#[test]
fn progress_that_was_never_flushed_is_not_in_the_file() {
    let f = Fixture::new();
    let store = f.logged_in();
    store.try_record_watch(&f.video(), 5.0, 600.0).unwrap();
    f.clock.advance(1.0);
    store.try_update_playback_progress(&f.video(), 99.0, 600.0).unwrap();
    assert_eq!(f.position(&f.store()), Some(5.0));
}

#[test]
fn a_step_under_half_a_second_changes_nothing_even_after_the_interval() {
    let f = Fixture::new();
    let store = f.logged_in();
    store.try_record_watch(&f.video(), 10.0, 100.0).unwrap();
    let saved = f.data();
    f.clock.advance(60.0);
    store.try_update_playback_progress(&f.video(), 10.2, 100.0).unwrap();
    assert!(!store.has_unsaved_changes());
    assert_eq!(f.data(), saved);
}

#[test]
fn flushing_while_logged_out_does_nothing() {
    let f = Fixture::new();
    let store = f.store();
    assert!(!store.try_flush_playback_progress().unwrap());
    assert!(!store.try_persist_pending_migrations().unwrap());
    assert!(!f.folder.profiles_file().exists());
    assert!(!ProfileStore::flush_playback_progress(&*store));
}
