//! macOS backend: LibraryKit `profiles.json` v2 + encrypted vault + flock + folder watcher + encrypted thumbnail
//! cache (mac-librarykit §5–§12). Remember-me is opt-in (UNIFIED.md §3.11 ADD-M): the remembered vault key lives in
//! `profile-remember-keys.json` through the [`SecretStore`] (never the Keychain).
//!
//! The module is pure Rust over `std` (plus kqueue on macOS), so it compiles and its tests run on every OS.
//!
//! **Concurrency** (§9.14). All state is one value behind one mutex; every method is synchronous. The mutex is held
//! only for memory work, one `stat`, a re-read after an outside change, or one vault write — never while waiting for
//! the file lock, while calling the secret store, during PBKDF2 or while fingerprinting. Lock order: file lock, then
//! the state mutex. Change notifications are delivered after the mutex is released.
//!
//! **Privacy** (rule 6, UNIFIED.md §4): logged out, every mutator returns the logged-out value before any probe is
//! built (no file is fingerprinted, no lock taken, nothing written), and `feed` returns `[]` for every filter. A
//! session end drops and zeroizes the vault key and contents and clears the fingerprint memo. An explicit
//! [`ProfileStore::logout`] also blocks the automatic restore of a remembered login until this store logs in or
//! registers again ("logout always wins", UNIFIED.md §3.11; Windows `user_logged_out_this_session`).

use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Condvar, Mutex, MutexGuard};
use sa_core::secrets::{SecretKey32, SecretRead, SecretStore, SecretString};
use sa_core::subscription::{Subscribers, Subscription};

use crate::{
    FeedQuery, FingerprintMode, ProfileChange, ProfileCounts, ProfileError, ProfileStore, Ticks, UserVideoList,
    VideoUserStats,
};

mod auth;
pub mod dates;
pub mod document;
pub mod feed;
pub mod fileio;
pub mod fingerprints;
pub mod json;
mod lists;
pub mod paths;
mod state;
pub mod thumbnails;
pub mod vault;
mod videos;

#[cfg(test)]
mod tests;

use fileio::{FileLock, FileSignature, FolderWatcher, Snapshot};
use fingerprints::FingerprintCache;
use state::{Effect, FileProblem, ProfileRecord, ProfileState};
use thumbnails::{ThumbnailCache, ThumbnailCacheLimits};
use vault::{VaultContents, VideoProbe};

/// The store's time source (injectable for tests).
pub type Clock = Arc<dyn Fn() -> Ticks + Send + Sync>;

/// Construction options (mac-librarykit §9.1).
pub struct MacProfileStoreOptions {
    /// Default `~/Library/Application Support/SubtitleAdder/profiles.json`.
    pub file: Option<PathBuf>,
    pub watches_file: bool,
    pub lock_timeout: Duration,
    pub thumbnails_folder: Option<PathBuf>,
    /// The app passes false and calls `start()` on a background lane.
    pub reads_file_immediately: bool,
}

impl Default for MacProfileStoreOptions {
    fn default() -> Self {
        Self { file: None, watches_file: true, lock_timeout: Duration::from_secs(5), thumbnails_folder: None, reads_file_immediately: true }
    }
}

/// Less common construction parameters (tests, tools).
#[derive(Default)]
pub struct StoreExtras {
    /// Default: [`Ticks::now`].
    pub clock: Option<Clock>,
    /// Default: the process-wide [`FingerprintCache::global`].
    pub fingerprints: Option<Arc<FingerprintCache>>,
    pub thumbnail_limits: ThumbnailCacheLimits,
}

/// What the state mutex guards besides the cloneable state.
struct Inner {
    state: ProfileState,
    /// A remembered login is being restored (the secret store is asked outside the mutex).
    restore_in_flight: bool,
    /// [`ProfileStore::logout`] was called: no automatic restore until the next successful login or registration.
    user_logged_out: bool,
}

/// Fan-out of [`ProfileChange`] to handlers and unbounded channels; no replay.
#[derive(Default)]
struct ChangeHub {
    subscribers: Subscribers<ProfileChange>,
    streams: Mutex<Vec<Sender<ProfileChange>>>,
}

impl ChangeHub {
    fn notify(&self, change: ProfileChange) {
        self.subscribers.notify(&change);
        self.streams.lock().retain(|s| s.send(change).is_ok());
    }
}

/// The state mutex held: dereferences to [`Inner`]; on drop releases the mutex, then clears the fingerprint memo if
/// a session ended and delivers the queued change signals.
struct Locked<'a> {
    store: &'a MacProfileStore,
    guard: ManuallyDrop<MutexGuard<'a, Inner>>,
    events: Vec<ProfileChange>,
    initial_generation: u64,
    initial_had_session: bool,
}

impl std::ops::Deref for Locked<'_> {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.guard
    }
}

impl std::ops::DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.guard
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        let generation = self.guard.state.session_generation;
        // Published while the mutex is still held, so the copy follows the state in unlock order.
        self.store.session_generation.store(generation, Ordering::Release);
        let ended = self.initial_had_session && generation != self.initial_generation;
        // SAFETY: the guard is dropped exactly once, here, and never used afterwards.
        unsafe { ManuallyDrop::drop(&mut self.guard) };
        if ended {
            self.store.fingerprints.clear();
        }
        for event in std::mem::take(&mut self.events) {
            self.store.hub.notify(event);
        }
    }
}

/// A remembered login to try (§9.6.2).
struct RestoreCandidate {
    name: String,
    record: ProfileRecord,
}

/// LibraryKit `ProfileStore` port.
pub struct MacProfileStore {
    file: PathBuf,
    lock_path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    clock: Clock,
    lock_timeout: Duration,
    thumbnails: ThumbnailCache,
    fingerprints: Arc<FingerprintCache>,
    inner: Mutex<Inner>,
    /// `state.session_generation` as of the last [`Locked`] unlock: the UI thread reads it at every playback
    /// report, and the mutex can be held across a re-read of `profiles.json` and a vault decrypt.
    session_generation: AtomicU64,
    restore_done: Condvar,
    hub: ChangeHub,
    watcher: Mutex<Option<FolderWatcher>>,
    last_error: Mutex<Option<ProfileError>>,
}

impl Drop for MacProfileStore {
    fn drop(&mut self) {
        if let Some(mut watcher) = self.watcher.lock().take() {
            watcher.stop();
        }
        self.hub.streams.lock().clear();
    }
}

impl MacProfileStore {
    pub const PROGRESS_SAVE_INTERVAL: Duration = Duration::from_secs(15);
    /// Default iterations of a new profile.
    pub const DEFAULT_ITERATIONS: u32 = 100_000;
    /// A damaged or hostile file must not make a login spin for minutes.
    pub const MAX_ITERATIONS: i64 = 10_000_000;

    pub fn new(options: MacProfileStoreOptions, secrets: Arc<dyn SecretStore>) -> Arc<Self> {
        Self::with_extras(options, secrets, StoreExtras::default())
    }

    pub fn with_extras(options: MacProfileStoreOptions, secrets: Arc<dyn SecretStore>, extras: StoreExtras) -> Arc<Self> {
        let raw = options.file.unwrap_or_else(|| paths::app_data_file("profiles.json", None));
        let normalized = paths::normalized_path(&raw);
        let file = if normalized.is_empty() { raw } else { PathBuf::from(normalized) };
        let mut lock_name = file.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        lock_name.push(".lock");
        let lock_path = file.with_file_name(lock_name);
        let folder = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let thumbnails_root = options.thumbnails_folder.unwrap_or_else(|| folder.join("thumbnails"));
        let _ = std::fs::create_dir_all(&folder);

        let store = Arc::new(MacProfileStore {
            file: file.clone(),
            lock_path,
            secrets,
            clock: extras.clock.unwrap_or_else(|| Arc::new(Ticks::now)),
            lock_timeout: options.lock_timeout,
            thumbnails: ThumbnailCache::new(thumbnails_root, extras.thumbnail_limits),
            fingerprints: extras.fingerprints.unwrap_or_else(FingerprintCache::global),
            inner: Mutex::new(Inner { state: ProfileState::default(), restore_in_flight: false, user_logged_out: false }),
            session_generation: AtomicU64::new(ProfileState::default().session_generation),
            restore_done: Condvar::new(),
            hub: ChangeHub::default(),
            watcher: Mutex::new(None),
            last_error: Mutex::new(None),
        });
        if options.reads_file_immediately {
            let mut l = store.lock();
            store.sync_locked(&mut l, true);
        }
        if options.watches_file {
            let weak = Arc::downgrade(&store);
            let watcher = FolderWatcher::start(
                file,
                Duration::from_millis(300),
                Box::new(move || {
                    if let Some(store) = weak.upgrade() {
                        store.refresh();
                    }
                }),
            );
            *store.watcher.lock() = watcher;
        }
        store
    }

    /// Re-sync from disk (watcher callback, start). When it returns, a remembered login another thread started
    /// restoring is done too (plain readers do not wait for that).
    pub fn refresh(&self) {
        self.sync_and_restore();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut l = self.lock();
        while l.restore_in_flight {
            if self.restore_done.wait_until(&mut *l.guard, deadline).timed_out() {
                break;
            }
        }
    }

    pub fn file_path(&self) -> &Path {
        &self.file
    }

    /// `profiles.json.lock`.
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// The picture cache of this store.
    pub fn thumbnail_cache(&self) -> &ThumbnailCache {
        &self.thumbnails
    }

    /// The fingerprint memo this store uses.
    pub fn fingerprint_cache(&self) -> &Arc<FingerprintCache> {
        &self.fingerprints
    }

    /// A new unbounded stream of change signals (no replay); closed when the store is dropped.
    pub fn changes(&self) -> Receiver<ProfileChange> {
        let (tx, rx) = crossbeam_channel::unbounded();
        self.hub.streams.lock().push(tx);
        rx
    }

    /// The last error a [`ProfileStore`] method had to swallow (its signature returns `Option`/`bool`).
    pub fn last_error(&self) -> Option<ProfileError> {
        self.last_error.lock().clone()
    }

    /// The header flag `rememberMe` of `profiles.json` (the last remember-me choice; Swift `rememberMe`).
    pub fn remember_me_flag(&self) -> bool {
        self.read(|s| s.remember_me)
    }

    /// The username a new instance would log in as without a password, if any.
    pub fn remembered_username(&self) -> Option<String> {
        self.read(|s| s.effective_remembered().map(str::to_string))
    }

    /// Playback progress or a followed move is held in memory and not yet in the file.
    pub fn has_unsaved_changes(&self) -> bool {
        self.read(|s| s.session.as_ref().is_some_and(|x| x.dirty))
    }

    /// A followed move is held in memory and not yet in the file (progress alone does not count).
    pub fn has_unsaved_migrations(&self) -> bool {
        self.read(|s| s.session.as_ref().is_some_and(|x| x.migrated))
    }

    // ----- internals

    fn lock(&self) -> Locked<'_> {
        let guard = self.inner.lock();
        let initial_generation = guard.state.session_generation;
        let initial_had_session = guard.state.session.is_some();
        Locked { store: self, guard: ManuallyDrop::new(guard), events: Vec::new(), initial_generation, initial_had_session }
    }

    fn now(&self) -> Ticks {
        (self.clock)()
    }

    fn swallow<T>(&self, result: Result<T, ProfileError>, fallback: T) -> T {
        match result {
            Ok(v) => v,
            Err(e) => {
                *self.last_error.lock() = Some(e);
                fallback
            }
        }
    }

    /// Brings the state up to date, then runs `f` on it under the mutex (memory only).
    fn read<T>(&self, f: impl FnOnce(&ProfileState) -> T) -> T {
        self.sync_and_restore();
        let l = self.lock();
        f(&l.state)
    }

    fn sync_and_restore(&self) {
        let candidate = {
            let mut l = self.lock();
            self.sync_locked(&mut l, false);
            self.restore_candidate(&mut l)
        };
        if let Some(candidate) = candidate {
            self.restore_remembered(&candidate);
            self.lock().restore_in_flight = false;
            self.restore_done.notify_all();
        }
    }

    /// §9.4: reloads `profiles.json` if its content changed since the last look. Returns whether anything a view
    /// would show changed (and then queues `External`).
    fn sync_locked(&self, l: &mut Locked<'_>, force: bool) -> bool {
        let signature = fileio::signature(&self.file);
        let retrying = l.state.last_snapshot == Some(Snapshot::Unreadable);
        if !force && !retrying && l.state.has_synced && signature == l.state.last_signature {
            return false;
        }
        let snapshot = fileio::snapshot(&self.file);
        if !force && l.state.has_synced && Some(&snapshot) == l.state.last_snapshot.as_ref() {
            l.state.last_signature = signature;
            return false;
        }
        let visible = self.apply(&mut l.state, snapshot, signature);
        if visible {
            l.events.push(ProfileChange::External);
        }
        visible
    }

    fn apply(&self, state: &mut ProfileState, snapshot: Snapshot, signature: Option<FileSignature>) -> bool {
        let old_remembered = state.effective_remembered().map(str::to_string);
        let old_remember_me = state.remember_me;
        let mut old_names: Vec<String> = state.profiles.keys().cloned().collect();
        old_names.sort();

        match &snapshot {
            Snapshot::Missing => {
                state.profiles.clear();
                state.file_problem = None;
            }
            Snapshot::Unreadable => state.file_problem = Some(FileProblem::Unreadable),
            Snapshot::Content(data) => match document::parse_profiles(data) {
                document::ParsedFile::Document(doc) => {
                    state.file_problem = None;
                    state.load(&doc);
                }
                document::ParsedFile::Unsupported(v) => state.file_problem = Some(FileProblem::UnsupportedVersion(v)),
                document::ParsedFile::Damaged => state.file_problem = Some(FileProblem::Corrupt),
            },
        }
        state.last_snapshot = Some(snapshot);
        state.last_signature = signature;
        state.has_synced = true;
        state.sync_generation += 1;

        let mut new_names: Vec<String> = state.profiles.keys().cloned().collect();
        new_names.sort();
        let mut visible = new_names != old_names || state.remember_me != old_remember_me;

        if let Some(session) = state.session.as_ref() {
            match state.profiles.get(&session.username_key) {
                Some(record) => {
                    if record.encrypted_vault_base64 != session.loaded_ciphertext {
                        match open_vault(record, &session.vault_key) {
                            Ok(mut contents) => {
                                let ciphertext = record.encrypted_vault_base64.clone();
                                let session = state.session.as_mut().expect("checked above");
                                if contents.differs_visibly(&session.contents) {
                                    visible = true;
                                }
                                reapply(&session.pending_progress, &mut contents);
                                session.contents = contents;
                                session.loaded_ciphertext = ciphertext;
                                session.dirty = !session.pending_progress.is_empty();
                                session.migrated = false;
                            }
                            Err(_) => {
                                state.end_session();
                                visible = true;
                            }
                        }
                    }
                }
                None => {
                    state.end_session();
                    visible = true;
                }
            }
        }

        if let Some(session) = state.session.as_ref()
            && session.remembered
            && state.effective_remembered().is_none()
            && old_remembered.as_deref().map(paths::ci).as_deref() == Some(session.username_key.as_str())
        {
            state.end_session();
            visible = true;
        }
        visible
    }

    /// §9.6.2 (mutex held): the remembered login to try now, at most once per state of the file.
    fn restore_candidate(&self, l: &mut Locked<'_>) -> Option<RestoreCandidate> {
        if l.user_logged_out || l.state.session.is_some() || l.state.restore_attempted_generation == l.state.sync_generation
        {
            return None;
        }
        let name = l.state.effective_remembered()?.to_string();
        let record = l.state.profiles.get(&paths::ci(&name))?.clone();
        l.state.restore_attempted_generation = l.state.sync_generation;
        if !has_vault(&record) {
            return None;
        }
        l.restore_in_flight = true;
        Some(RestoreCandidate { name, record })
    }

    /// Reads the key and opens the vault outside every lock, then takes the session if nothing changed meanwhile.
    fn restore_remembered(&self, candidate: &RestoreCandidate) {
        let SecretRead::Found(bytes) = self.secrets.read(&auth::remember_account(&candidate.name)) else { return };
        let Some(key) = SecretKey32::from_slice(bytes.expose()) else { return };
        let Ok(contents) = open_vault(&candidate.record, &key) else { return };
        let mut l = self.lock();
        let name_key = paths::ci(&candidate.name);
        if l.user_logged_out
            || l.state.session.is_some()
            || l.state.effective_remembered().map(paths::ci).as_deref() != Some(name_key.as_str())
        {
            return;
        }
        let Some(current) = l.state.profiles.get(&name_key).cloned() else { return };
        if current.encrypted_vault_base64 != candidate.record.encrypted_vault_base64 {
            return;
        }
        l.state.start_session(state::ProfileSession::new(
            paths::ci(&current.username),
            current.username.clone(),
            key,
            contents,
            current.encrypted_vault_base64.clone(),
            true,
        )
        .with_thumbnail_profile(&current.username));
        l.events.push(ProfileChange::External);
    }

    /// §9.6.5: encrypts the active vault if needed and writes `profiles.json`; nothing is written over a file that
    /// is newer or unreadable, and a damaged one is set aside first.
    fn save_locked(&self, working: &mut ProfileState, vault_changed: bool) -> Result<(), ProfileError> {
        match working.file_problem {
            Some(FileProblem::UnsupportedVersion(v)) => return Err(ProfileError::UnsupportedFileVersion(v)),
            Some(FileProblem::Unreadable) => return Err(ProfileError::FileUnreadable),
            _ => {}
        }
        let mut sealed = false;
        {
            let ProfileState { session, profiles, .. } = &mut *working;
            if let Some(session) = session.as_mut()
                && (vault_changed || session.dirty)
            {
                for (k, data) in session.contents.extract_thumbnails() {
                    self.thumbnails.store(&data, &session.thumbnail_profile, &k, &session.vault_key);
                }
                let mut plaintext = document::encode_vault(&session.contents.payload()).into_bytes();
                let blob = crate::crypto::seal(&plaintext, &session.vault_key);
                zeroize::Zeroize::zeroize(&mut plaintext);
                if blob.is_empty() {
                    return Err(ProfileError::CryptoFailure);
                }
                let encoded = base64::engine::general_purpose::STANDARD.encode(blob);
                if let Some(record) = profiles.get_mut(&session.username_key) {
                    record.encrypted_vault_base64 = Some(encoded.clone());
                }
                session.loaded_ciphertext = Some(encoded);
                session.dirty = false;
                session.migrated = false;
                session.pending_progress.clear();
                sealed = true;
            }
        }
        let data = document::encode_profiles(&working.document()).into_bytes();
        if working.file_problem == Some(FileProblem::Corrupt) {
            fileio::preserve_damaged(&self.file, self.now())?;
        } else if let Some(Snapshot::Content(current)) = &working.last_snapshot
            && **current == *data
        {
            return Ok(());
        }
        fileio::write_atomically(&data, &self.file)?;
        working.file_problem = None;
        working.last_snapshot = Some(Snapshot::Content(data.into()));
        working.last_signature = fileio::signature(&self.file);
        working.has_synced = true;
        if sealed {
            working.last_vault_save_attempt = Some(self.now());
        }
        Ok(())
    }

    /// §9.6.4: one read-modify-write. File lock first (waited for without the mutex), then the mutex, a reload if
    /// the file moved on, `body` on a copy, the save; the copy is committed only if the save succeeded.
    fn transact<T>(
        &self,
        requires_session: bool,
        fallback: impl FnOnce() -> T,
        body: impl FnOnce(&mut ProfileState) -> Result<(T, Effect), ProfileError>,
    ) -> Result<T, ProfileError> {
        if requires_session {
            self.sync_and_restore();
            if self.lock().state.session.is_none() {
                return Ok(fallback());
            }
        }
        let mut file_lock = FileLock::acquire(&self.lock_path, self.lock_timeout)?;
        let result = {
            let mut l = self.lock();
            self.sync_locked(&mut l, false);
            if requires_session && l.state.session.is_none() {
                Ok(fallback())
            } else {
                let mut working = l.state.clone();
                match body(&mut working) {
                    Err(e) => Err(e),
                    Ok((value, effect)) => {
                        let saved = if effect.needs_save() { self.save_locked(&mut working, effect.vault) } else { Ok(()) };
                        match saved {
                            Err(e) => Err(e),
                            Ok(()) => {
                                l.state = working;
                                if effect.needs_save() && !effect.silent {
                                    l.events.push(ProfileChange::Local);
                                }
                                Ok(value)
                            }
                        }
                    }
                }
            }
        };
        file_lock.release();
        result
    }

    /// §9.6.6: writes what is held in memory only (progress, followed moves); throttled by `min_interval`.
    fn persist_dirty(&self, min_interval: Duration) -> Result<bool, ProfileError> {
        let due = {
            let l = self.lock();
            match &l.state.session {
                Some(session) if session.dirty => match (min_interval.is_zero(), l.state.last_vault_save_attempt) {
                    (true, _) | (false, None) => true,
                    (false, Some(last)) => self.now().0 - last.0 >= dates::seconds(min_interval.as_secs_f64()),
                },
                _ => false,
            }
        };
        if !due {
            return Ok(false);
        }
        let result = self.transact(
            true,
            || false,
            |state| {
                if state.session.as_ref().is_some_and(|s| s.dirty) {
                    Ok((true, Effect::VAULT_SILENT))
                } else {
                    Ok((false, Effect::NONE))
                }
            },
        );
        if result.is_err() {
            let now = self.now();
            self.lock().state.last_vault_save_attempt = Some(now);
        }
        result
    }

    // ----- probes

    fn probe(&self, path: &Path, mode: FingerprintMode) -> Option<VideoProbe> {
        VideoProbe::new(path, mode, &self.fingerprints)
    }

    /// The lookup key for a file, fingerprinted as `mode` says and prepared for resolving.
    fn lookup_probe(&self, path: &Path, mode: FingerprintMode) -> Option<VideoProbe> {
        let probe = self.probe(path, mode)?;
        self.prepared(vec![probe], None).pop()
    }

    /// Fingerprints (outside every lock) the recorded paths of records these probes would take over.
    fn prepared(&self, probes: Vec<VideoProbe>, should_continue: Option<&dyn Fn() -> bool>) -> Vec<VideoProbe> {
        if !probes.iter().any(|p| p.fingerprint.is_some()) {
            return probes;
        }
        self.sync_and_restore();
        let Some(snapshot) = self.lock().state.session.as_ref().map(|s| s.contents.clone()) else { return probes };
        let mut stopped = false;
        probes
            .into_iter()
            .map(|probe| {
                if stopped || should_continue.is_some_and(|f| !f()) {
                    stopped = true;
                    return probe;
                }
                let Some(recorded) = snapshot.takeover_path(&probe) else { return probe };
                let fingerprint = match probe.mode {
                    FingerprintMode::Compute => self.fingerprints.fingerprint(&recorded),
                    _ => self.fingerprints.cached(&recorded),
                };
                match fingerprint {
                    Some(fp) => probe.with_recorded_fingerprint(fp, &recorded),
                    None => probe,
                }
            })
            .collect()
    }

    /// §9.6.8: the vault after following moved files among `probes` (in memory only), or `None` logged out.
    fn read_migrating(&self, probes: &[VideoProbe]) -> Option<VaultContents> {
        self.sync_and_restore();
        let now = self.now();
        let mut l = self.lock();
        let session = l.state.session.as_mut()?;
        let mut migrated = false;
        for probe in probes {
            if session.contents.resolve(probe, now).is_some_and(|r| r.migrated) {
                migrated = true;
            }
        }
        if migrated {
            session.dirty = true;
            session.migrated = true;
        }
        Some(session.contents.clone())
    }

    /// Memory + at most one `stat`; the privacy gate in front of every path-taking call.
    fn logged_in(&self) -> bool {
        self.read(|s| s.session.is_some())
    }

    fn totals_inner(&self) -> ProfileCounts {
        self.read(|state| {
            let Some(session) = &state.session else { return ProfileCounts::default() };
            let mut totals = ProfileCounts::default();
            for video in session.contents.videos().values() {
                if video.view_count > 0 {
                    totals.watched += 1;
                }
                totals.views += video.view_count;
                if video.is_liked {
                    totals.liked += 1;
                }
                if video.rating > 0 {
                    totals.rated += 1;
                }
            }
            totals
        })
    }
}

/// A profile has a vault when its string is present and not blank.
fn has_vault(record: &ProfileRecord) -> bool {
    record.encrypted_vault_base64.as_deref().is_some_and(|v| !paths::trim_ws_nl(v).is_empty())
}

/// No vault → empty contents; else base64 → open → JSON → contents; any failure → `VaultUnreadable`.
fn open_vault(record: &ProfileRecord, key: &SecretKey32) -> Result<VaultContents, ProfileError> {
    if !has_vault(record) {
        return Ok(VaultContents::new());
    }
    let encoded = record.encrypted_vault_base64.as_deref().unwrap_or_default();
    let blob = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| ProfileError::VaultUnreadable)?;
    let mut plaintext = crate::crypto::open(&blob, key).ok_or(ProfileError::VaultUnreadable)?;
    let payload = document::parse_vault(&plaintext);
    zeroize::Zeroize::zeroize(&mut plaintext);
    let payload = payload.ok_or(ProfileError::VaultUnreadable)?;
    Ok(VaultContents::from_payload(&payload))
}

/// Puts unsaved progress back on a vault just loaded from the file (the local resume point wins).
fn reapply(pending: &std::collections::HashMap<String, state::PendingProgress>, contents: &mut VaultContents) {
    for progress in pending.values() {
        let (k, existing, _) = contents.resolve_or_create(&progress.probe, progress.at);
        let mut updated = existing.clone();
        updated.last_position_seconds = progress.position;
        if progress.duration > 0.0 {
            updated.duration_seconds = progress.duration;
        }
        updated.updated_at = existing.updated_at.max(progress.at);
        contents.set_video(updated, &k);
    }
}

impl ProfileStore for MacProfileStore {
    fn is_logged_in(&self) -> bool {
        self.logged_in()
    }
    fn current_username(&self) -> Option<String> {
        self.read(|s| s.session.as_ref().map(|x| x.username.clone()))
    }
    fn registered_usernames(&self) -> Vec<String> {
        self.read(|s| {
            let mut names: Vec<String> = s.profiles.values().map(|r| r.username.clone()).collect();
            names.sort_by(|a, b| paths::localized_ci_cmp(a, b));
            names
        })
    }
    fn last_username(&self) -> Option<String> {
        self.read(|s| s.last_username.clone())
    }
    fn remember_me(&self) -> bool {
        // True only while a remembered session exists (the UI checkbox reflects it; UNIFIED.md §3.11).
        self.read(|s| s.session.as_ref().is_some_and(|x| x.remembered))
    }
    /// Lock-free (UI-safe): the value as of the last state unlock; a change still being made under the mutex shows
    /// up once it is done, so a captured value can be stale when a worker writes. Writes gated on a captured
    /// generation go through the `*_in_session` methods, which compare it again under the mutex.
    fn session_generation(&self) -> u64 {
        self.session_generation.load(Ordering::Acquire)
    }
    fn start(&self) {
        self.refresh();
    }
    fn register(&self, username: &str, password: &SecretString, remember: bool) -> Result<(), ProfileError> {
        self.register_user(username, password.expose(), remember)
    }
    fn login(&self, username: &str, password: &SecretString, remember: bool) -> Result<(), ProfileError> {
        self.login_user(username, password.expose(), remember)
    }
    fn logout(&self) -> Result<(), ProfileError> {
        self.logout_user()
    }
    fn delete_profile(&self, username: &str, password: &SecretString) -> Result<(), ProfileError> {
        self.delete_user(username, password.expose())
    }
    fn record_watch(&self, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        let r = self.try_record_watch(path, position, duration);
        self.swallow(r, None)
    }
    fn update_playback_progress(&self, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        let r = self.try_update_playback_progress(path, position, duration);
        self.swallow(r, None)
    }
    fn update_playback_progress_in_session(&self, generation: u64, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        let r = self.try_update_playback_progress_in(Some(generation), path, position, duration);
        self.swallow(r, None)
    }
    fn record_watch_in_session(&self, generation: u64, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        let r = self.try_record_watch_in(Some(generation), path, position, duration);
        self.swallow(r, None)
    }
    fn set_thumbnail_in_session(&self, generation: u64, path: &Path, bytes: &[u8]) -> bool {
        let r = self.try_set_thumbnail_in(Some(generation), path, bytes);
        self.swallow(r, false)
    }
    fn flush_playback_progress(&self) -> bool {
        let r = self.try_flush_playback_progress();
        self.swallow(r, false)
    }
    fn set_liked(&self, path: &Path, liked: bool) -> Option<VideoUserStats> {
        let r = self.try_set_liked(path, liked);
        self.swallow(r, None)
    }
    fn set_rating(&self, path: &Path, stars: u8) -> Option<VideoUserStats> {
        let r = self.try_set_rating(path, i64::from(stars));
        self.swallow(r, None)
    }
    fn stats(&self, path: &Path, mode: FingerprintMode) -> Option<VideoUserStats> {
        self.stats_mode(path, mode)
    }
    fn stats_fast_in_memory(&self, path: &Path) -> Option<VideoUserStats> {
        let path_key = paths::ci(&paths::normalized_path(path));
        if path_key.is_empty() {
            return None;
        }
        let l = self.lock();
        let contents = &l.state.session.as_ref()?.contents;
        contents.find_by_path_key(&path_key).and_then(|k| contents.stats_for_key(&k))
    }
    fn clear_watch_history(&self) -> bool {
        let r = self.try_clear_watch_history();
        self.swallow(r, false)
    }
    fn thumbnail(&self, path: &Path) -> Option<Vec<u8>> {
        self.thumbnail_mode(path, FingerprintMode::Compute)
    }
    fn set_thumbnail(&self, path: &Path, bytes: &[u8]) -> bool {
        let r = self.try_set_thumbnail(path, bytes);
        self.swallow(r, false)
    }
    fn lists(&self) -> Vec<UserVideoList> {
        self.all_lists()
    }
    fn create_list(&self, name: &str) -> Result<Option<UserVideoList>, ProfileError> {
        self.try_create_list(name)
    }
    fn rename_list(&self, id: &str, name: &str) -> Result<bool, ProfileError> {
        self.try_rename_list(id, name)
    }
    fn delete_list(&self, id: &str) -> bool {
        let r = self.try_delete_list(id);
        self.swallow(r, false)
    }
    fn set_in_list(&self, id: &str, path: &Path, is_in: bool) -> bool {
        let r = if is_in { self.try_add_video(path, id) } else { self.try_remove_video(path, id) };
        self.swallow(r, false)
    }
    fn toggle_in_list(&self, id: &str, path: &Path) -> bool {
        let r = self.try_toggle_video(path, id);
        self.swallow(r, false)
    }
    fn lists_containing(&self, path: &Path) -> Vec<String> {
        self.lists_containing_mode(path, FingerprintMode::Compute).into_iter().map(|l| l.id).collect()
    }
    fn feed(&self, query: &FeedQuery, should_continue: &dyn Fn() -> bool) -> Vec<VideoUserStats> {
        self.feed_with(&query.filter, query.search.as_deref(), &query.discovered, query.mode, Some(should_continue))
    }
    fn totals(&self) -> ProfileCounts {
        self.totals_inner()
    }
    fn persist_pending_migrations(&self) -> bool {
        let r = self.try_persist_pending_migrations();
        self.swallow(r, false)
    }
    fn subscribe_changes(&self, handler: Box<dyn Fn(ProfileChange) + Send + Sync>) -> Subscription {
        self.hub.subscribers.subscribe(move |c: &ProfileChange| handler(*c))
    }
    fn active_vault_key(&self) -> Option<(String, SecretKey32)> {
        let l = self.lock();
        l.state.session.as_ref().map(|s| (s.username_key.clone(), s.vault_key.clone()))
    }
}
