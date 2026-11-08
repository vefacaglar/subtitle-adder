//! `ProfileRestoreTests` (1): `refresh()` waits for a restore another thread has under way; plain reads do not.

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::ProfileStore;

/// A secret store that takes its time to answer.
struct SlowSecrets {
    inner: Arc<TestSecrets>,
    delay: Duration,
    slow: AtomicBool,
}

impl SecretStore for SlowSecrets {
    fn read(&self, account: &str) -> SecretRead {
        if self.slow.load(Ordering::SeqCst) {
            std::thread::sleep(self.delay);
        }
        self.inner.read(account)
    }
    fn set(&self, bytes: &[u8], account: &str) -> bool {
        self.inner.set(bytes, account)
    }
    fn remove(&self, account: &str) {
        self.inner.remove(account)
    }
}

#[test]
fn refresh_waits_for_a_restore_another_thread_has_under_way() {
    let folder = TestFolder::new();
    let secrets =
        Arc::new(SlowSecrets { inner: TestSecrets::new(), delay: Duration::from_millis(400), slow: AtomicBool::new(false) });
    let first = folder.store(StoreOpts { secrets: secrets.clone(), ..StoreOpts::default() });
    first.register_user("emre", "pw", true).unwrap();

    secrets.slow.store(true, Ordering::SeqCst);
    let second = folder.store(StoreOpts { secrets: secrets.clone(), ..StoreOpts::default() });
    let reader = {
        let second = second.clone();
        std::thread::spawn(move || second.current_username())
    };
    std::thread::sleep(Duration::from_millis(100));
    assert!(!second.is_logged_in(), "a plain read meanwhile does not wait: it sees nobody yet");
    second.refresh();
    assert!(second.is_logged_in());
    assert_eq!(reader.join().unwrap().as_deref(), Some("emre"));
}

#[test]
fn start_restores_the_remembered_login() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    folder.store_with(&secrets).register_user("emre", "pw", true).unwrap();
    let lazy = MacProfileStore::with_extras(
        MacProfileStoreOptions {
            file: Some(folder.profiles_file()),
            watches_file: false,
            reads_file_immediately: false,
            ..MacProfileStoreOptions::default()
        },
        secrets.clone(),
        StoreExtras { fingerprints: Some(folder.cache.clone()), ..StoreExtras::default() },
    );
    lazy.start();
    assert!(lazy.inner.lock().state.session.is_some(), "start() is the background first read + restore");
}
