//! Ports of `apps/macos/Tests/LibraryKitTests` (profile store, vault, thumbnails, fingerprints). Each Swift suite is
//! one submodule; test names keep the Swift names in snake case. Deviations are marked `PRIVACY OVERRIDE`
//! (UNIFIED.md §4) or `RUST` (an API difference).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use parking_lot::Mutex;
use sa_core::secrets::{SecretBytes, SecretRead, SecretStore};

use super::fingerprints::FingerprintCache;
use super::{Clock, MacProfileStore, MacProfileStoreOptions, StoreExtras, dates, paths};
use crate::{ProfileChange, Ticks};

mod crypto;
mod file_format;
mod file_safety;
mod fingerprint;
mod golden;
mod lists_and_feed;
mod locking;
mod multi_instance;
mod progress;
mod reads_never_write;
mod restore;
mod robustness;
mod store;
mod thumbnail_cache;
mod vault_contents;

/// A secret store with an `accounts()` listing (Swift `InMemorySecretStore`).
#[derive(Default)]
pub struct TestSecrets {
    map: Mutex<HashMap<String, Vec<u8>>>,
}

impl TestSecrets {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn accounts(&self) -> Vec<String> {
        let mut names: Vec<String> = self.map.lock().keys().cloned().collect();
        names.sort();
        names
    }
    pub fn data(&self, account: &str) -> Option<Vec<u8>> {
        self.map.lock().get(account).cloned()
    }
    pub fn put(&self, account: &str, bytes: &[u8]) {
        self.map.lock().insert(account.to_string(), bytes.to_vec());
    }
}

impl SecretStore for TestSecrets {
    fn read(&self, account: &str) -> SecretRead {
        match self.map.lock().get(account) {
            Some(v) => SecretRead::Found(SecretBytes::new(v.clone())),
            None => SecretRead::Absent,
        }
    }
    fn set(&self, bytes: &[u8], account: &str) -> bool {
        self.put(account, bytes);
        true
    }
    fn remove(&self, account: &str) {
        self.map.lock().remove(account);
    }
}

/// A clock the test advances by hand (starts at 1_800_000_000).
#[derive(Clone)]
pub struct ManualClock(Arc<Mutex<Ticks>>);

impl ManualClock {
    pub fn new() -> Self {
        ManualClock(Arc::new(Mutex::new(dates::from_unix_seconds(1_800_000_000.0))))
    }
    pub fn advance(&self, seconds: f64) {
        let mut now = self.0.lock();
        now.0 += dates::seconds(seconds);
    }
    pub fn clock(&self) -> Clock {
        let inner = self.0.clone();
        Arc::new(move || *inner.lock())
    }
}

/// Store construction knobs of `TestFolder.makeStore`.
pub struct StoreOpts {
    pub secrets: Arc<dyn SecretStore>,
    pub watches: bool,
    pub lock_timeout: Duration,
    pub clock: Option<Clock>,
}

impl Default for StoreOpts {
    fn default() -> Self {
        Self { secrets: TestSecrets::new(), watches: false, lock_timeout: Duration::from_secs(5), clock: None }
    }
}

/// A throwaway folder for one test.
pub struct TestFolder {
    _dir: tempfile::TempDir,
    pub url: PathBuf,
    /// The fingerprint memo shared by this test's stores (the process-wide one would leak between parallel tests).
    pub cache: Arc<FingerprintCache>,
}

impl TestFolder {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("LibraryKitTests-").tempdir().expect("temp dir");
        let url = PathBuf::from(paths::normalized_path(dir.path()));
        TestFolder { _dir: dir, url, cache: Arc::new(FingerprintCache::default()) }
    }

    pub fn profiles_file(&self) -> PathBuf {
        self.url.join("profiles.json")
    }

    /// A path inside the folder (the file need not exist).
    pub fn video(&self, name: &str) -> PathBuf {
        self.url.join(name)
    }

    pub fn make_store(&self) -> Arc<MacProfileStore> {
        self.store(StoreOpts::default())
    }

    pub fn store_with(&self, secrets: &Arc<TestSecrets>) -> Arc<MacProfileStore> {
        self.store(StoreOpts { secrets: secrets.clone(), ..StoreOpts::default() })
    }

    pub fn store(&self, opts: StoreOpts) -> Arc<MacProfileStore> {
        MacProfileStore::with_extras(
            MacProfileStoreOptions {
                file: Some(self.profiles_file()),
                watches_file: opts.watches,
                lock_timeout: opts.lock_timeout,
                thumbnails_folder: None,
                reads_file_immediately: true,
            },
            opts.secrets,
            StoreExtras { clock: opts.clock, fingerprints: Some(self.cache.clone()), ..StoreExtras::default() },
        )
    }

    /// The files of the picture cache, at any depth.
    pub fn cache_files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.is_file() {
                    out.push(p);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.url.join("thumbnails"), &mut out);
        out
    }

    pub fn raw_profiles(&self) -> String {
        String::from_utf8_lossy(&std::fs::read(self.profiles_file()).expect("profiles.json")).into_owned()
    }

    pub fn file_data(&self) -> Vec<u8> {
        std::fs::read(self.profiles_file()).expect("profiles.json")
    }

    pub fn profiles_object(&self) -> serde_json::Map<String, serde_json::Value> {
        match serde_json::from_slice(&self.file_data()).expect("json") {
            serde_json::Value::Object(map) => map,
            _ => panic!("profiles.json is not an object"),
        }
    }

    pub fn write_profiles_object(&self, root: &serde_json::Map<String, serde_json::Value>) {
        std::fs::write(self.profiles_file(), serde_json::to_vec(root).unwrap()).unwrap();
    }

    /// Names in the folder starting with `profiles.corrupt-`.
    pub fn aside_files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.url)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("profiles.corrupt-"))
            .collect();
        names.sort();
        names
    }
}

impl Drop for TestFolder {
    fn drop(&mut self) {
        // A test may have made the folder read-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.url, std::fs::Permissions::from_mode(0o755));
        }
    }
}

/// The path a store reports for a file (normalized, absolute).
pub fn normalized(path: &Path) -> String {
    paths::normalized_path(path)
}

/// Collects what a store's change stream delivers.
pub struct ChangeLog {
    rx: Mutex<Option<Receiver<ProfileChange>>>,
    items: Mutex<Vec<ProfileChange>>,
}

impl ChangeLog {
    pub fn new(store: &MacProfileStore) -> Self {
        ChangeLog { rx: Mutex::new(Some(store.changes())), items: Mutex::new(Vec::new()) }
    }
    pub fn events(&self) -> Vec<ProfileChange> {
        if let Some(rx) = self.rx.lock().as_ref() {
            while let Ok(e) = rx.try_recv() {
                self.items.lock().push(e);
            }
        }
        self.items.lock().clone()
    }
    pub fn stop(&self) {
        let _ = self.events();
        self.rx.lock().take();
    }
}

/// Polls until `condition` holds or `timeout` (4 s) passes.
pub fn eventually(condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    condition()
}

/// Deterministic content (`(i + seed) % modulus`).
pub fn content(count: usize, modulus: usize, seed: usize) -> Vec<u8> {
    (0..count).map(|i| ((i + seed) % modulus) as u8).collect()
}

/// Whether permission tests can run (root ignores file modes; Windows has no POSIX modes).
#[cfg(unix)]
pub fn permissions_apply() -> bool {
    // SAFETY: getuid has no preconditions.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() != 0 }
}

/// `store.feed(filter)` with no search, no discovered files, `.compute`.
pub fn feed(store: &MacProfileStore, filter: crate::VideoFeedFilter) -> Vec<crate::VideoUserStats> {
    store.feed_with(&filter, None, &[], crate::FingerprintMode::Compute, None)
}

/// `store.feed(filter, search:, discovered:)`.
pub fn feed_in(
    store: &MacProfileStore,
    filter: crate::VideoFeedFilter,
    search: Option<&str>,
    discovered: &[PathBuf],
) -> Vec<crate::VideoUserStats> {
    store.feed_with(&filter, search, discovered, crate::FingerprintMode::Compute, None)
}

pub fn list(id: &str) -> crate::VideoFeedFilter {
    crate::VideoFeedFilter::CustomList(Some(id.to_string()))
}

pub fn paths_of(items: &[crate::VideoUserStats]) -> Vec<String> {
    items.iter().map(|v| v.video_path.clone()).collect()
}

pub fn names_of(items: &[crate::VideoUserStats]) -> Vec<String> {
    items.iter().map(|v| v.file_name.clone()).collect()
}
