//! `ProfileLockingTests` (4): readers never wait for a writer's file lock; the secret store is never asked with a
//! lock held; a failed login puts the secret store back.

use std::sync::Weak;

use super::*;
use crate::mac::fileio::FileLock;
use crate::{FingerprintMode, ProfileCounts, ProfileError, ProfileStore};

/// Checks on every call that the watched store holds neither its state mutex nor the file lock.
struct ProbingSecrets {
    inner: Arc<TestSecrets>,
    target: Mutex<Option<Weak<MacProfileStore>>>,
    seen: Mutex<Vec<String>>,
    count: Mutex<usize>,
}

impl ProbingSecrets {
    fn new(inner: Arc<TestSecrets>) -> Arc<Self> {
        Arc::new(Self { inner, target: Mutex::new(None), seen: Mutex::new(Vec::new()), count: Mutex::new(0) })
    }
    fn watch(&self, store: &Arc<MacProfileStore>) {
        *self.target.lock() = Some(Arc::downgrade(store));
    }
    fn violations(&self) -> Vec<String> {
        self.seen.lock().clone()
    }
    fn calls(&self) -> usize {
        *self.count.lock()
    }
    fn check(&self, what: &str) {
        let Some(store) = self.target.lock().as_ref().and_then(Weak::upgrade) else { return };
        *self.count.lock() += 1;
        if store.inner.try_lock().is_none() {
            self.seen.lock().push(format!("{what} ran with the in-process lock held"));
        }
        match FileLock::acquire(store.lock_path(), Duration::ZERO) {
            Ok(mut probe) => probe.release(),
            Err(_) => self.seen.lock().push(format!("{what} ran with the file lock held")),
        }
    }
}

impl SecretStore for ProbingSecrets {
    fn read(&self, account: &str) -> SecretRead {
        self.check(&format!("read of {account}"));
        self.inner.read(account)
    }
    fn set(&self, bytes: &[u8], account: &str) -> bool {
        self.check(&format!("write of {account}"));
        self.inner.set(bytes, account)
    }
    fn remove(&self, account: &str) {
        self.check(&format!("removal of {account}"));
        self.inner.remove(account)
    }
}

#[test]
fn getters_are_not_held_up_while_a_writer_polls_for_the_file_lock() {
    let folder = TestFolder::new();
    let video = folder.video("a.mp4");
    let store = folder.store(StoreOpts { lock_timeout: Duration::from_secs(3), ..StoreOpts::default() });
    store.register_user("emre", "pw", false).unwrap();
    store.try_create_list("Liste").unwrap();

    let mut held = FileLock::acquire(store.lock_path(), Duration::from_secs(1)).unwrap();
    let writer = {
        let store = store.clone();
        let video = video.clone();
        std::thread::spawn(move || store.try_record_watch(&video, 0.0, 0.0))
    };
    std::thread::sleep(Duration::from_millis(200));
    let started = Instant::now();
    assert!(store.is_logged_in());
    assert_eq!(store.current_username().as_deref(), Some("emre"));
    assert_eq!(store.registered_usernames(), ["emre"]);
    assert_eq!(store.totals(), ProfileCounts::default());
    assert_eq!(store.lists().into_iter().map(|l| l.name).collect::<Vec<_>>(), ["Liste"]);
    assert_eq!(store.stats_mode(&video, FingerprintMode::Compute), None);
    assert!(!store.has_unsaved_changes());
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_millis(500), "readers must not wait for the writer's file lock (took {elapsed:?})");
    held.release();
    writer.join().unwrap().unwrap();
    assert_eq!(store.stats_mode(&video, FingerprintMode::Compute).unwrap().view_count, 1);
}

#[test]
fn a_writer_waiting_for_the_file_lock_does_not_block_another_store_of_the_same_file() {
    let folder = TestFolder::new();
    let video = folder.video("a.mp4");
    let secrets = TestSecrets::new();
    let opts = || StoreOpts { secrets: secrets.clone(), lock_timeout: Duration::from_secs(3), ..StoreOpts::default() };
    let a = folder.store(opts());
    a.register_user("emre", "pw", true).unwrap();
    let b = folder.store(opts());
    assert!(b.is_logged_in());
    let mut held = FileLock::acquire(a.lock_path(), Duration::from_secs(1)).unwrap();
    let writer = {
        let a = a.clone();
        let video = video.clone();
        std::thread::spawn(move || a.try_record_watch(&video, 0.0, 0.0))
    };
    std::thread::sleep(Duration::from_millis(150));
    let started = Instant::now();
    assert_eq!(b.current_username().as_deref(), Some("emre"));
    assert_eq!(b.totals(), ProfileCounts::default());
    assert!(started.elapsed() < Duration::from_millis(500));
    held.release();
    writer.join().unwrap().unwrap();
    assert_eq!(b.stats_mode(&video, FingerprintMode::Compute).unwrap().view_count, 1);
}

#[test]
fn the_secret_store_is_never_asked_while_a_lock_is_held() {
    let folder = TestFolder::new();
    let shared = TestSecrets::new();
    let first = ProbingSecrets::new(shared.clone());
    let a = folder.store(StoreOpts { secrets: first.clone(), ..StoreOpts::default() });
    first.watch(&a);
    a.register_user("alice", "pw1", true).unwrap();
    a.register_user("bob", "pw2", true).unwrap();
    a.login_user("alice", "pw1", true).unwrap();
    a.login_user("bob", "pw2", false).unwrap();
    a.login_user("alice", "pw1", true).unwrap();
    a.logout_user().unwrap();
    assert_eq!(a.login_user("alice", "wrong", true), Err(ProfileError::InvalidCredentials));
    a.login_user("alice", "pw1", true).unwrap();

    let second = ProbingSecrets::new(shared.clone());
    let b = folder.store(StoreOpts { secrets: second.clone(), ..StoreOpts::default() });
    second.watch(&b);
    assert_eq!(b.current_username().as_deref(), Some("alice"));
    b.refresh();
    a.logout_user().unwrap();
    b.refresh();
    a.login_user("bob", "pw2", true).unwrap();
    b.delete_user("bob", "pw2").unwrap();

    assert!(first.calls() > 8, "{}", first.calls());
    assert!(second.calls() > 0);
    assert!(first.violations().is_empty(), "{:?}", first.violations());
    assert!(second.violations().is_empty(), "{:?}", second.violations());
}

#[test]
fn a_login_that_cannot_be_written_puts_the_secret_store_back_as_it_was() {
    let folder = TestFolder::new();
    let shared = TestSecrets::new();
    let store =
        folder.store(StoreOpts { secrets: shared.clone(), lock_timeout: Duration::from_millis(100), ..StoreOpts::default() });
    store.register_user("alice", "pw", true).unwrap();

    shared.put("profile-remember.alice", &[7u8; 32]);
    let mut held = FileLock::acquire(store.lock_path(), Duration::from_secs(1)).unwrap();
    assert_eq!(store.login_user("alice", "pw", true), Err(ProfileError::LockUnavailable));
    held.release();
    assert_eq!(shared.data("profile-remember.alice"), Some(vec![7u8; 32]));

    shared.remove("profile-remember.alice");
    let mut held = FileLock::acquire(store.lock_path(), Duration::from_secs(1)).unwrap();
    assert_eq!(store.login_user("alice", "pw", true), Err(ProfileError::LockUnavailable));
    held.release();
    assert_eq!(shared.data("profile-remember.alice"), None, "no key is left behind by a login that failed");
}
