//! Windows backend: `profiles.json` v3 + `profiles.db` + broker write queue, C#-compatible (a port of the current
//! `LocalProfileManager.cs` / `SqliteProfileStore.cs`, including the "no watch history without a logged-in profile"
//! changes: keyed-hash row keys `hk:`, owner tags `ot:`, padded row blobs, sealed broker payloads, DPAPI-wrapped
//! remember-me key with the legacy user/machine wrap still readable).
//!
//! Decisions: UNIFIED.md §3.11 (Q1 fix, Q2 fix, Q3 keep, Q5/Q6/Q8 keep, Q7 logged-out feed `[]`, Q9 fix) and §4
//! item 7 (fingerprint before creating a row, any size; migrate path-keyed rows to `fp:` keys on login).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Weak};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use sa_core::secrets::{SecretKey32, SecretString};
use sa_core::subscription::{Subscribers, Subscription};
use sa_sync::{EventKind, ProfileWriteBroker, StateSyncEvent, StateSyncWriteRequest, Topic, TopicFilter};

use crate::crypto;
use crate::fingerprint;
use crate::{
    FeedQuery, FingerprintMode, ProfileChange, ProfileCounts, ProfileError, ProfileStore, Ticks, UserVideoList,
    VideoFeedFilter, VideoUserStats,
};

pub mod json;
pub mod netcompat;
pub mod paths;
pub mod sqlite;
mod upper_table;

use json::{ListHeaderIn, ProgressRowIn, SealedPayloadIn, VideoRowIn};
use netcompat::{CiMap, CiSet, culture_cmp_ci, culture_contains_ci, culture_eq_ci, eq_ci, upper_invariant};
pub use sqlite::{
    HASHED_VIDEO_KEY_PREFIX, ListItemRow, OPAQUE_LEGACY_VIDEO_KEY_PREFIX, OWNER_TAG_PREFIX, SqliteProfileStore, WriteItem,
    WriteKind, flush_all_for_path, flush_for_path, set_before_flush_path_hook,
};

/// The bundled SQLite library version (startup diagnostics; proves rusqlite `bundled` links).
pub fn sqlite_version() -> &'static str {
    rusqlite::version()
}

const PBKDF2_ITERATIONS: i32 = 100_000;
const VIDEO_ROW_BLOB_BUCKET: usize = 1024;
const SMALL_ROW_BLOB_BUCKET: usize = 256;
const LIST_ITEM_PATH_BUCKET: usize = 512;
const DPAPI_REMEMBERED_KEY_PREFIX: &str = "dpapi1:";
const PROGRESS_EPSILON: f64 = 0.5;

static GLOBAL_FILE_WRITE_GENERATION: LazyLock<Mutex<HashMap<String, i64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers [`SqliteProfileStore::factory`] as the process-wide data worker factory (once; the C# broker engine
/// creates its `SqliteProfileStore` itself, the Rust broker cannot depend on this crate).
pub fn register_data_worker_factory(broker: &Arc<dyn ProfileWriteBroker>) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| broker.set_data_worker_factory(SqliteProfileStore::factory()));
}

/// `%LOCALAPPDATA%\SubtitleAdder\profiles.json`: C# `Environment.GetFolderPath(LocalApplicationData)` is
/// `SHGetKnownFolderPath(FOLDERID_LocalAppData, KF_FLAG_DEFAULT)`; the `LOCALAPPDATA` variable (then the temp folder)
/// is only a fallback when the shell call fails.
pub fn default_profiles_path() -> PathBuf {
    local_app_data().unwrap_or_else(std::env::temp_dir).join("SubtitleAdder").join("profiles.json")
}

fn local_app_data() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: the returned PWSTR is owned by the caller and freed with CoTaskMemFree after copying.
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) {
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            if let Some(s) = s.filter(|s| !s.is_empty()) {
                return Some(PathBuf::from(s));
            }
        }
    }
    std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()).map(PathBuf::from)
}

// ---------------------------------------------------------------------------------------------------------------
// Key derivations and blob encodings (byte-compatible with the C# app)

/// `StorageVideoKey`: `hk:` + 40 hex of `HMAC-SHA256(vault_key, "SubtitleAdder.VideoKey.v1\n" + upper(trim(key)))`.
pub fn storage_video_key(vault_key: &SecretKey32, logical_key: &str) -> String {
    let input = format!("SubtitleAdder.VideoKey.v1\n{}", upper_invariant(logical_key.trim_matches(char::is_whitespace)));
    let mac = crypto::hmac_sha256(vault_key.expose(), input.as_bytes());
    format!("{HASHED_VIDEO_KEY_PREFIX}{}", crypto::lower_hex(&mac[..20]))
}

/// `StorageOwnerTag`: `ot:` + 40 hex of `HMAC-SHA256(vault_key, "SubtitleAdder.StorageOwner.v1")`.
pub fn storage_owner_tag(vault_key: &SecretKey32) -> String {
    let mac = crypto::hmac_sha256(vault_key.expose(), b"SubtitleAdder.StorageOwner.v1");
    format!("{OWNER_TAG_PREFIX}{}", crypto::lower_hex(&mac[..20]))
}

/// `ComputeListItemHash`.
pub fn list_item_hash(username: &str, list_id: &str, normalized_path: &str, vault_key: &SecretKey32) -> String {
    let input = format!("{}\n{}\n{}", upper_invariant(username), upper_invariant(list_id), upper_invariant(normalized_path));
    crypto::hmac_hex32(vault_key, input.as_bytes())
}

/// `PadToBucket`: at least `min` bytes, else the next power of two.
pub fn pad_to_bucket(mut plain: Vec<u8>, min: usize, pad: u8) -> Vec<u8> {
    let target = min.max(plain.len().max(1).next_power_of_two());
    if target > plain.len() {
        plain.resize(target, pad);
    }
    plain
}

fn enc_video(s: &VideoUserStats, k: &SecretKey32) -> Vec<u8> {
    crypto::seal(&pad_to_bucket(json::video_row_json(s), VIDEO_ROW_BLOB_BUCKET, b' '), k)
}
fn enc_progress(s: &VideoUserStats, k: &SecretKey32) -> Vec<u8> {
    crypto::seal(&pad_to_bucket(json::progress_row_json(s), SMALL_ROW_BLOB_BUCKET, b' '), k)
}
fn enc_list_header(l: &UserVideoList, k: &SecretKey32) -> Vec<u8> {
    crypto::seal(&pad_to_bucket(json::list_header_json(l), SMALL_ROW_BLOB_BUCKET, b' '), k)
}
fn enc_list_path(path: &str, k: &SecretKey32) -> Vec<u8> {
    crypto::seal(&pad_to_bucket(path.as_bytes().to_vec(), LIST_ITEM_PATH_BUCKET, 0), k)
}
fn dec_video(b: &[u8], k: &SecretKey32) -> Option<VideoRowIn> {
    serde_json::from_slice(&crypto::open(b, k)?).ok()
}
fn dec_progress(b: &[u8], k: &SecretKey32) -> Option<ProgressRowIn> {
    serde_json::from_slice(&crypto::open(b, k)?).ok()
}
fn dec_list_header(b: &[u8], k: &SecretKey32) -> Option<ListHeaderIn> {
    serde_json::from_slice(&crypto::open(b, k)?).ok()
}

/// `EncryptVaultMarker`.
pub fn vault_marker(k: &SecretKey32) -> String {
    json::b64_encode(&crypto::seal(crypto::VAULT_MARKER.as_bytes(), k))
}

fn remembered_key_entropy(salt: &[u8]) -> Vec<u8> {
    let mut v = b"SubtitleAdder.RememberKey.v2".to_vec();
    v.extend_from_slice(salt);
    v
}

/// `LegacyRememberWrapKey`: PBKDF2 over `SubtitleAdder.RememberKey.<UserName>.<MachineName>`, 10 000 rounds.
pub fn legacy_remember_wrap_key(salt: &[u8], user_name: &str, machine_name: &str) -> SecretKey32 {
    let entropy = format!("SubtitleAdder.RememberKey.{user_name}.{machine_name}");
    let mut out = [0u8; 32];
    crypto::pbkdf2_sha256(entropy.as_bytes(), salt, crypto::REMEMBER_ITERATIONS, &mut out);
    SecretKey32::new(out)
}

fn local_legacy_wrap_key(salt: &[u8]) -> SecretKey32 {
    legacy_remember_wrap_key(salt, &netcompat::environment_user_name(), &netcompat::environment_machine_name())
}

/// `ProtectVaultKey`: DPAPI (current user) with `dpapi1:`; the legacy user/machine wrap if DPAPI fails.
pub fn protect_vault_key(k: &SecretKey32, salt: &[u8]) -> String {
    if let Some(wrapped) = netcompat::dpapi_protect(k.expose(), &remembered_key_entropy(salt)) {
        return format!("{DPAPI_REMEMBERED_KEY_PREFIX}{}", json::b64_encode(&wrapped));
    }
    json::b64_encode(&crypto::seal(k.expose(), &local_legacy_wrap_key(salt)))
}

/// `UnprotectVaultKey` → `(key, legacy_wrapping)`.
pub fn unprotect_vault_key(protected_b64: Option<&str>, salt: &[u8]) -> Option<(SecretKey32, bool)> {
    let p = protected_b64?;
    if p.trim().is_empty() {
        return None;
    }
    if let Some(rest) = p.strip_prefix(DPAPI_REMEMBERED_KEY_PREFIX) {
        let wrapped = json::b64_decode(rest)?;
        let mut plain = netcompat::dpapi_unprotect(&wrapped, &remembered_key_entropy(salt))?;
        let key = SecretKey32::from_slice(&plain);
        zeroize::Zeroize::zeroize(&mut plain);
        return key.map(|k| (k, false));
    }
    let combined = json::b64_decode(p)?;
    let mut plain = crypto::open(&combined, &local_legacy_wrap_key(salt))?;
    let key = SecretKey32::from_slice(&plain);
    zeroize::Zeroize::zeroize(&mut plain);
    key.map(|k| (k, true))
}

fn event_kind_name(kind: EventKind) -> String {
    match kind {
        EventKind::ProfileLogin => "ProfileLogin".into(),
        EventKind::ProfileLogout => "ProfileLogout".into(),
        EventKind::ProfileDeleted => "ProfileDeleted".into(),
        EventKind::VideoWatched => "VideoWatched".into(),
        EventKind::ProgressUpdated => "ProgressUpdated".into(),
        EventKind::VideoLiked => "VideoLiked".into(),
        EventKind::VideoRated => "VideoRated".into(),
        EventKind::HistoryCleared => "HistoryCleared".into(),
        EventKind::ListChanged => "ListChanged".into(),
        EventKind::ThumbnailCaptured => "ThumbnailCaptured".into(),
        EventKind::MediaIndexed => "MediaIndexed".into(),
        EventKind::ProvidersUpdated => "ProvidersUpdated".into(),
        EventKind::SettingsUpdated => "SettingsUpdated".into(),
        EventKind::Other(v) => v.to_string(),
    }
}

fn ipc_key(vault_key: &SecretKey32) -> SecretKey32 {
    SecretKey32::new(crypto::hmac_sha256(vault_key.expose(), b"SubtitleAdder.StateSync.SealedEvent.v1"))
}

fn event_aad(evt: &StateSyncEvent) -> Vec<u8> {
    format!("{}|{}|{}|{}", evt.event_id, evt.topic, event_kind_name(evt.kind), upper_invariant(&evt.scope_path)).into_bytes()
}

fn sanitize_seconds(candidate: f64, fallback: f64) -> f64 {
    if candidate.is_finite() && candidate > 0.0 {
        candidate
    } else if fallback.is_finite() && fallback > 0.0 {
        fallback
    } else {
        0.0
    }
}

fn norm(raw: &str) -> String {
    paths::normalize_video_path(raw)
}

fn cached_fp(norm: &str) -> Option<String> {
    if norm.is_empty() { None } else { fingerprint::cached(Path::new(norm)) }
}

fn compute_fp(norm: &str) -> Option<String> {
    if norm.is_empty() { None } else { fingerprint::fingerprint(Path::new(norm)) }
}

fn create_default(norm_path: &str, now: Ticks, fp: Option<String>, compute_from_disk: bool) -> VideoUserStats {
    let fp = fp.or_else(|| if compute_from_disk { compute_fp(norm_path) } else { cached_fp(norm_path) });
    VideoUserStats {
        content_fingerprint: fp,
        video_path: norm_path.to_owned(),
        file_name: paths::file_name(norm_path),
        directory_path: paths::directory_name(norm_path),
        view_count: 0,
        is_liked: false,
        rating: 0,
        last_position_seconds: 0.0,
        duration_seconds: 0.0,
        last_watched_at: None,
        updated_at: now,
        thumbnail: None,
    }
}

/// The logged-in user's profile in the line format of the former C# `E2EData dump` tool (`Console.WriteLine`, bools
/// as `True`/`False`): `user=<name>`, then `history|<file name>|views=<n>|liked=<bool>|rating=<n>` in history order,
/// then `list|<name>` per list. The `win_profile_dump` example prints it for `scripts/e2e-windows.ps1`; sa-app's
/// `csharp_e2e_data` test pins it against the frozen C# output.
#[doc(hidden)]
pub fn e2e_dump_lines(store: &dyn ProfileStore) -> Vec<String> {
    let b = |v: bool| if v { "True" } else { "False" };
    let mut out = vec![format!("user={}", store.current_username().unwrap_or_default())];
    let query = FeedQuery { filter: VideoFeedFilter::History, ..FeedQuery::default() };
    for v in store.feed(&query, &|| true) {
        out.push(format!("history|{}|views={}|liked={}|rating={}", v.file_name, v.view_count, b(v.is_liked), v.rating));
    }
    out.extend(store.lists().into_iter().map(|l| format!("list|{}", l.name)));
    out
}

/// `MatchesSearchQuery`: whole query, then every token (`' '`, `\t`, `_`, `-`, `.`) in name, folder or path.
pub fn matches_search_query(video: &VideoUserStats, query: &str) -> bool {
    let trimmed = query.trim_matches(char::is_whitespace);
    if trimmed.is_empty() {
        return true;
    }
    if culture_contains_ci(&video.file_name, trimmed)
        || culture_contains_ci(&video.directory_path, trimmed)
        || culture_contains_ci(&video.video_path, trimmed)
    {
        return true;
    }
    let tokens: Vec<&str> = trimmed
        .split([' ', '\t', '_', '-', '.'])
        .map(|t| t.trim_matches(char::is_whitespace))
        .filter(|t| !t.is_empty())
        .collect();
    tokens.iter().all(|t| {
        culture_contains_ci(&video.file_name, t)
            || culture_contains_ci(&video.directory_path, t)
            || culture_contains_ci(&video.video_path, t)
            || netcompat::contains_ci(&video.file_name, t)
            || netcompat::contains_ci(&video.directory_path, t)
    })
}

// ---------------------------------------------------------------------------------------------------------------
// State

struct ProfileEntry {
    username: String,
    salt_b64: String,
    hash_b64: String,
    iterations: i32,
    created: Ticks,
    encrypted_vault: Option<String>,
    videos: CiMap<VideoUserStats>,
    custom_lists: Vec<UserVideoList>,
    decoded_thumbnails: CiMap<Vec<u8>>,
    missing_thumbnails: CiSet,
}

impl ProfileEntry {
    fn clear_decrypted(&mut self) {
        self.videos.clear();
        self.custom_lists.clear();
        self.decoded_thumbnails.clear();
        self.missing_thumbnails.clear();
    }
}

#[derive(Default)]
struct State {
    profiles: CiMap<ProfileEntry>,
    session_keys: CiMap<SecretKey32>,
    processed_request_ids: HashSet<String>,
    active_profile: Option<String>,
    active_username: Option<String>,
    remember_me: bool,
    last_username: Option<String>,
    remembered_username: Option<String>,
    remembered_key: Option<String>,
    last_synced_json: Option<String>,
    last_synced_generation: i64,
    user_logged_out_this_session: bool,
    unpersisted_logout_key: Option<String>,
    session_token: i64,
    submitted_rewrites: CiSet,
    last_error: Option<String>,
}

impl State {
    /// The active entry (resolved by name) and its session key.
    fn active(&mut self) -> Option<(&mut ProfileEntry, Option<SecretKey32>)> {
        let name = self.active_profile.clone()?;
        let key = self.session_keys.get(&name).cloned();
        self.profiles.get_mut(&name).map(|e| (e, key))
    }
    fn active_key(&self) -> Option<SecretKey32> {
        let name = self.active_profile.as_deref()?;
        self.profiles.get(name)?;
        self.session_keys.get(name).cloned()
    }
}

/// The store state behind its mutex, plus lock-free copies of the two values the UI thread reads while a video
/// plays (`is_logged_in`, `session_generation`). Workers hold the mutex across disk syncs (file reads with retry
/// sleeps, JSON parsing) and fingerprinting, so the UI thread must never wait for it; the copies are refreshed by
/// every [`StateGuard`] before it unlocks, so they always equal the state as of the last unlock.
struct StateCell {
    state: Mutex<State>,
    session_token: AtomicI64,
    logged_in: AtomicBool,
    /// Session-changing operations under way ([`StateCell::begin_session_change`]).
    session_changes: AtomicU32,
}

/// Marks a session-changing operation as under way until dropped (after its state guard, so after the new token is
/// published).
struct SessionChange<'a>(&'a AtomicU32);

impl Drop for SessionChange<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl StateCell {
    fn new(state: State) -> Self {
        let (token, logged_in) = (state.session_token, state.active_profile.is_some());
        Self {
            state: Mutex::new(state),
            session_token: AtomicI64::new(token),
            logged_in: AtomicBool::new(logged_in),
            session_changes: AtomicU32::new(0),
        }
    }

    /// Raised at the start of an operation that may bump the session token, before it waits for the state lock
    /// (it can then hold it for seconds: the cross-process mutex wait and the header save). Keep the returned guard
    /// alive past the state guard.
    fn begin_session_change(&self) -> SessionChange<'_> {
        self.session_changes.fetch_add(1, Ordering::AcqRel);
        SessionChange(&self.session_changes)
    }

    /// True while a session-changing operation is under way (never blocks).
    fn session_change_in_progress(&self) -> bool {
        self.session_changes.load(Ordering::Acquire) > 0
    }

    fn lock(&self) -> StateGuard<'_> {
        StateGuard { guard: self.state.lock(), cell: self }
    }

    /// `session_token` as of the last unlock (never blocks).
    fn session_token(&self) -> i64 {
        self.session_token.load(Ordering::Acquire)
    }

    /// `active_profile.is_some()` as of the last unlock (never blocks).
    fn logged_in(&self) -> bool {
        self.logged_in.load(Ordering::Acquire)
    }
}

/// The locked state; publishes the lock-free copies on drop, while still holding the mutex.
struct StateGuard<'a> {
    guard: parking_lot::MutexGuard<'a, State>,
    cell: &'a StateCell,
}

impl std::ops::Deref for StateGuard<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        &self.guard
    }
}

impl std::ops::DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.guard
    }
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        // Runs before the `guard` field is dropped, so the copies change under the lock, in unlock order.
        self.cell.session_token.store(self.guard.session_token, Ordering::Release);
        self.cell.logged_in.store(self.guard.active_profile.is_some(), Ordering::Release);
    }
}

type AvailabilityFilter = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// `LocalProfileManager` port.
pub struct WinProfileStore {
    file_path: String,
    db_path: String,
    backup_path: String,
    mutex_name: String,
    instance_id: String,
    broker: Arc<dyn ProfileWriteBroker>,
    store: Mutex<Arc<SqliteProfileStore>>,
    state: StateCell,
    dirty: AtomicBool,
    changes: Subscribers<ProfileChange>,
    subscription: Mutex<Option<Subscription>>,
    availability_filter: RwLock<Option<AvailabilityFilter>>,
}

impl std::fmt::Debug for WinProfileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinProfileStore").field("file_path", &self.file_path).finish()
    }
}

/// Row counts of the five data tables for the active profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TableRowCounts {
    pub videos: i64,
    pub watch_progress: i64,
    pub custom_lists: i64,
    pub custom_list_items: i64,
    pub thumbnails: i64,
}

impl WinProfileStore {
    /// `file` default `%LOCALAPPDATA%\SubtitleAdder\profiles.json` (`GetFullPathNameW`); DB = same path with `.db`.
    /// Opens the DB through the broker and subscribes to it; no file is read until [`ProfileStore::start`] or the
    /// first call.
    pub fn new(file: PathBuf, broker: Arc<dyn ProfileWriteBroker>) -> Arc<Self> {
        let raw = file.to_string_lossy().into_owned();
        let file_path = if raw.trim().is_empty() {
            default_profiles_path().to_string_lossy().into_owned()
        } else {
            paths::get_full_path(&raw).unwrap_or(raw)
        };
        let db_path = paths::change_extension(&file_path, ".db");
        let backup_path = format!("{file_path}.bak");
        let hash = crypto::lower_hex(&sha2_256(upper_invariant(&file_path).as_bytes()));
        let mutex_name = format!("Local\\SubtitleAdder.Profiles.{}", &hash[..16]);
        register_data_worker_factory(&broker);
        let store = Self::resolve_store(&broker, &db_path);
        let me = Arc::new(Self {
            file_path,
            db_path,
            backup_path,
            mutex_name,
            instance_id: sa_core::text::guid_n(),
            broker: broker.clone(),
            store: Mutex::new(store),
            state: StateCell::new(State { last_synced_generation: -1, ..State::default() }),
            dirty: AtomicBool::new(false),
            changes: Subscribers::new(),
            subscription: Mutex::new(None),
            availability_filter: RwLock::new(None),
        });
        let weak: Weak<Self> = Arc::downgrade(&me);
        let sub = broker.subscribe(
            TopicFilter::All,
            Box::new(move |evt: &StateSyncEvent| {
                if let Some(m) = weak.upgrade() {
                    m.on_broker_event(evt);
                }
            }),
        );
        *me.subscription.lock() = Some(sub);
        me
    }

    /// [`WinProfileStore::new`] followed by [`ProfileStore::start`] (the C# constructor).
    pub fn open(file: PathBuf, broker: Arc<dyn ProfileWriteBroker>) -> Arc<Self> {
        let me = Self::new(file, broker);
        me.start();
        me
    }

    fn resolve_store(broker: &Arc<dyn ProfileWriteBroker>, db_path: &str) -> Arc<SqliteProfileStore> {
        let worker = broker.data_worker_store(Path::new(db_path));
        SqliteProfileStore::from_worker(&worker)
            .or_else(|| SqliteProfileStore::find_live(Path::new(db_path)))
            .unwrap_or_else(|| SqliteProfileStore::new(Path::new(db_path)))
    }

    /// `SqlStore`: the broker's store for this DB (re-fetched once the previous one was disposed).
    pub fn sql_store(&self) -> Arc<SqliteProfileStore> {
        let mut g = self.store.lock();
        if g.is_disposed() {
            *g = Self::resolve_store(&self.broker, &self.db_path);
        }
        g.clone()
    }

    pub fn file_path(&self) -> &Path {
        Path::new(&self.file_path)
    }
    pub fn sqlite_db_path(&self) -> PathBuf {
        PathBuf::from(&self.db_path)
    }
    pub fn backup_file_path(&self) -> PathBuf {
        PathBuf::from(&self.backup_path)
    }
    /// The component id this manager stamps on its events and requests.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
    /// Already-localized text of the last auth error.
    pub fn last_error(&self) -> Option<String> {
        self.state.lock().last_error.clone()
    }
    pub fn flush_pending_writes(&self, timeout: Duration) -> bool {
        self.broker.flush_pending_writes(Path::new(&self.db_path), Some(timeout))
    }
    /// `true` once the user logged out in this manager (logout always wins over remembered restores).
    pub fn user_logged_out_this_session(&self) -> bool {
        self.state.lock().user_logged_out_this_session
    }
    pub fn pending_sqlite_write_count(&self) -> i64 {
        self.sql_store().pending_write_count()
    }
    pub fn total_sqlite_batches_committed(&self) -> i64 {
        self.sql_store().total_batches_committed()
    }
    pub fn total_sqlite_operations_committed(&self) -> i64 {
        self.sql_store().total_operations_committed()
    }
    pub fn total_watch_progress_sqlite_upserts(&self) -> i64 {
        self.sql_store().total_watch_progress_upserts()
    }
    pub fn last_storage_write_error(&self) -> Option<String> {
        self.sql_store().last_write_error()
    }

    /// Test hook: a path is available only if its root is ready **and** this returns true.
    pub fn set_storage_availability_filter(&self, filter: Option<Box<dyn Fn(&str) -> bool + Send + Sync>>) {
        *self.availability_filter.write() = filter.map(Arc::from);
    }

    /// `IsVideoStorageAvailable` (root readiness only; never touches files).
    pub fn is_video_storage_available(&self, path: &str) -> bool {
        self.storage_available(&norm(path), &mut HashMap::new())
    }

    fn storage_available(&self, normalized: &str, cache: &mut HashMap<String, bool>) -> bool {
        if normalized.trim().is_empty() {
            return false;
        }
        let filter = self.availability_filter.read().clone();
        if let Some(f) = filter {
            if !f(normalized) {
                return false;
            }
        }
        let root = paths::path_root(normalized);
        if root.trim().is_empty() {
            return true;
        }
        let key = upper_invariant(&root);
        if let Some(&v) = cache.get(&key) {
            return v;
        }
        // C#: a root starting with two backslashes (UNC, `\\?\`, `\\.\`) is checked with `Directory.Exists(root)`, a
        // drive root with `DriveInfo.IsReady`; both amount to "the root is a ready directory".
        let ready = paths::is_root_ready(&root);
        cache.insert(key, ready);
        ready
    }

    /// `ComputeVideoId`: the fingerprint, or the normalized path; `""` for an empty path.
    pub fn compute_video_id(path: &str) -> String {
        let n = norm(path);
        if n.is_empty() {
            return String::new();
        }
        compute_fp(&n).unwrap_or(n)
    }

    // ----- generation / sync

    fn current_generation(&self) -> i64 {
        GLOBAL_FILE_WRITE_GENERATION.lock().get(&upper_invariant(&self.file_path)).copied().unwrap_or(0)
    }

    fn bump(&self, st: &mut State) {
        let mut g = GLOBAL_FILE_WRITE_GENERATION.lock();
        let v = g.entry(upper_invariant(&self.file_path)).or_insert(0);
        *v += 1;
        st.last_synced_generation = *v;
    }

    fn ensure_synced_if_changed(&self, st: &mut State) {
        let generation = self.current_generation();
        if !self.dirty.load(Ordering::SeqCst) && st.last_synced_json.is_some() && generation == st.last_synced_generation {
            return;
        }
        self.sync_from_disk(st, true);
    }

    /// Logged out (after the first read): every mutator returns at once, before the store is drained or any file
    /// is touched (UNIFIED.md §4 item 1). A login or remembered restore elsewhere reaches this store through the
    /// broker subscription, which resyncs immediately. A header written by another manager of the same file in this
    /// process bumps the process-wide generation; like C# `EnsureSyncedIfChangedLocked`, that forces the resync
    /// (which may restore a remembered session) instead of the early return.
    fn locked_out(&self) -> bool {
        let st = self.state.lock();
        st.active_profile.is_none()
            && st.last_synced_json.is_some()
            && !self.dirty.load(Ordering::SeqCst)
            && self.current_generation() == st.last_synced_generation
    }

    fn drain(&self) {
        self.sql_store().try_drain_when_healthy_non_blocking();
    }

    fn end_session(&self, st: &mut State, username: &str) {
        let owner = st.session_keys.remove(username).map(|k| storage_owner_tag(&k));
        if let Some(e) = st.profiles.get_mut(username) {
            e.clear_decrypted();
        }
        let store = self.sql_store();
        store.forget_cached_thumbnails(Some(username));
        if let Some(o) = owner {
            store.forget_cached_thumbnails(Some(&o));
        }
    }

    fn end_all_sessions(&self, st: &mut State) {
        let users: Vec<String> = st.session_keys.keys().map(str::to_owned).collect();
        for u in users {
            self.end_session(st, &u);
        }
        for (_, e) in st.profiles.iter_mut() {
            e.clear_decrypted();
        }
        st.processed_request_ids.clear();
        fingerprint::clear_cache();
    }

    fn sync_from_disk(&self, st: &mut State, force: bool) -> bool {
        self.dirty.store(false, Ordering::SeqCst);
        let generation = self.current_generation();
        if !force && st.last_synced_json.is_some() && generation == st.last_synced_generation {
            return false;
        }
        let Some(text) = netcompat::read_all_text_shared(Path::new(&self.file_path), 5) else {
            if force {
                for (_, e) in st.profiles.iter_mut() {
                    e.clear_decrypted();
                }
                st.profiles.clear();
                st.last_synced_json = None;
                // Q9 fix: a vanished header also ends the active session (privacy-safe).
                if st.active_profile.is_some() || st.active_username.is_some() {
                    st.active_profile = None;
                    st.active_username = None;
                    self.end_all_sessions(st);
                    st.session_token += 1;
                }
            }
            return false;
        };
        let Ok(doc) = serde_json::from_str::<json::HeaderDocIn>(&text) else { return false };
        let Some(dtos) = doc.profiles else { return false };

        let previous_active = st.active_username.clone();
        for (_, e) in st.profiles.iter_mut() {
            e.clear_decrypted();
        }
        st.profiles.clear();
        st.remember_me = doc.remember_me;
        if let Some(lu) = doc.last_username.as_deref().filter(|s| !s.trim().is_empty()) {
            st.last_username = Some(lu.trim_matches(char::is_whitespace).to_owned());
        }
        st.remembered_username = doc.remembered_username.as_deref().filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_owned());
        st.remembered_key = doc.remembered_protected_key_base64.as_deref().filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_owned());

        let mut rewrite_header = false;
        if doc.remember_me_opt_in != Some(true) && (st.remember_me || st.remembered_username.is_some() || st.remembered_key.is_some()) {
            st.remember_me = false;
            st.remembered_username = None;
            st.remembered_key = None;
            rewrite_header = true;
        }
        if let Some(stale) = st.unpersisted_logout_key.clone() {
            if st.remembered_key.as_deref() == Some(stale.as_str()) {
                st.remember_me = false;
                st.remembered_username = None;
                st.remembered_key = None;
                rewrite_header = true;
            } else {
                st.unpersisted_logout_key = None;
            }
        }

        let mut any_migrated = false;
        for dto in dtos {
            let (Some(user), Some(salt), Some(hash)) = (dto.username, dto.salt_base64, dto.password_hash_base64) else { continue };
            if user.trim().is_empty() || salt.trim().is_empty() || hash.trim().is_empty() {
                continue;
            }
            let mut entry = ProfileEntry {
                username: user.trim_matches(char::is_whitespace).to_owned(),
                salt_b64: salt,
                hash_b64: hash,
                iterations: if dto.iterations > 0 { dto.iterations } else { PBKDF2_ITERATIONS },
                created: dto.created_at_utc,
                encrypted_vault: dto.encrypted_vault_base64,
                videos: CiMap::new(),
                custom_lists: Vec::new(),
                decoded_thumbnails: CiMap::new(),
                missing_thumbnails: CiSet::new(),
            };
            let u = entry.username.clone();
            if !st.user_logged_out_this_session
                && !st.session_keys.contains_key(&u)
                && st.active_username.as_deref().is_none_or(|a| eq_ci(a, &u))
                && st.remember_me
                && st.remembered_username.as_deref().is_some_and(|r| eq_ci(r, &u))
                && st.remembered_key.as_deref().is_some_and(|k| !k.trim().is_empty())
            {
                if let Some(salt_bytes) = json::b64_decode(&entry.salt_b64) {
                    if let Some((key, legacy)) = unprotect_vault_key(st.remembered_key.as_deref(), &salt_bytes) {
                        if legacy {
                            st.remembered_key = Some(protect_vault_key(&key, &salt_bytes));
                            rewrite_header = true;
                        }
                        st.session_keys.insert(&u, key);
                        if st.active_username.is_none() {
                            st.active_username = Some(u.clone());
                        }
                    }
                }
            }
            if let Some(key) = st.session_keys.get(&u).cloned() {
                if self.decrypt_into_entry(&mut entry, &key, &mut st.submitted_rewrites) {
                    any_migrated = true;
                }
            }
            st.profiles.insert(&u, entry);
        }

        match st.active_username.clone() {
            Some(a) => match st.profiles.get_key_value(&a) {
                Some((k, _)) => st.active_profile = Some(k.to_owned()),
                None => {
                    st.active_profile = None;
                    st.active_username = None;
                }
            },
            None => st.active_profile = None,
        }
        let keep = st.active_username.clone();
        let users: Vec<String> = st.session_keys.keys().map(str::to_owned).collect();
        for u in users {
            if !keep.as_deref().is_some_and(|a| eq_ci(a, &u)) {
                self.end_session(st, &u);
            }
        }
        if !netcompat::opt_eq_ci(previous_active.as_deref(), st.active_username.as_deref()) {
            st.session_token += 1;
            if previous_active.is_some() {
                // The session ended elsewhere (profile deleted or switched in another window): forget the watched
                // paths of the process-wide fingerprint cache too (UNIFIED.md §3.12 FIX).
                fingerprint::clear_cache();
            }
        }
        st.last_synced_json = Some(text);
        st.last_synced_generation = self.current_generation();
        if any_migrated || rewrite_header {
            self.save_auth_header(st);
        }
        true
    }

    fn save_auth_header(&self, st: &mut State) -> bool {
        for attempt in 1..=3u64 {
            if self.try_write_auth_header(st) {
                st.unpersisted_logout_key = None;
                return true;
            }
            if attempt < 3 {
                std::thread::sleep(Duration::from_millis(25 * attempt));
            }
        }
        false
    }

    fn try_write_auth_header(&self, st: &mut State) -> bool {
        if let Some(dir) = Path::new(&self.file_path).parent() {
            if std::fs::create_dir_all(dir).is_err() {
                return false;
            }
        }
        let keys: Vec<(String, SecretKey32)> = st.session_keys.iter().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        for (u, k) in &keys {
            if let Some(e) = st.profiles.get_mut(u) {
                if e.encrypted_vault.as_deref().is_none_or(|v| v.trim().is_empty()) {
                    e.encrypted_vault = Some(vault_marker(k));
                }
            }
        }
        let mut entries: Vec<&ProfileEntry> = st.profiles.values().collect();
        entries.sort_by(|a, b| netcompat::cmp_ci(&a.username, &b.username));
        let opt_in = st.remember_me && st.remembered_username.is_some() && st.remembered_key.is_some();
        let doc = json::HeaderDocOut {
            remember_me: st.remember_me,
            remember_me_opt_in: opt_in.then_some(true),
            last_username: st.last_username.as_deref(),
            remembered_username: if st.remember_me { st.remembered_username.as_deref() } else { None },
            remembered_protected_key_base64: if st.remember_me { st.remembered_key.as_deref() } else { None },
            profiles: entries
                .iter()
                .map(|p| json::ProfileDtoOut {
                    username: &p.username,
                    salt_base64: &p.salt_b64,
                    password_hash_base64: &p.hash_b64,
                    iterations: p.iterations,
                    created_at_utc: p.created,
                    encrypted_vault_base64: p.encrypted_vault.as_deref(),
                })
                .collect(),
        };
        let text = doc.to_json();
        if !netcompat::write_all_text_atomic(Path::new(&self.file_path), &text, 5) {
            return false;
        }
        st.last_synced_json = Some(text);
        self.bump(st);
        true
    }

    // ----- requests and events

    fn request(&self, kind: WriteKind) -> StateSyncWriteRequest {
        let mut r = StateSyncWriteRequest::new(&self.instance_id, &self.db_path, sa_sync::WriteKind::UpsertVideo);
        r.kind = kind.name().to_owned();
        r
    }

    fn submit(&self, r: StateSyncWriteRequest) {
        self.broker.submit_write_request(r);
    }

    /// `CreateBrokerEvent`: profile events carry nothing personal; watch-progress / thumbnail events carry their data
    /// only sealed with the sender profile's key.
    fn make_event(
        &self,
        topic: Topic,
        kind: EventKind,
        seal_key: Option<&SecretKey32>,
        video_path: Option<&str>,
        position: Option<f64>,
        duration: Option<f64>,
        payload_b64: Option<&str>,
    ) -> StateSyncEvent {
        let mut evt = StateSyncEvent::new(topic, kind, &self.instance_id, &self.file_path);
        if !matches!(topic, Topic::WatchProgress | Topic::Thumbnail) {
            return evt;
        }
        let (Some(key), Some(path)) = (seal_key, video_path.filter(|p| !p.is_empty())) else { return evt };
        let payload = json::sealed_payload_json(path, position, duration, payload_b64);
        let ik = ipc_key(key);
        let sealed = crypto::seal_raw(&payload, ik.expose(), &event_aad(&evt));
        evt.sealed_payload_base64 = Some(json::b64_encode(&sealed));
        evt
    }

    fn profiles_event(&self, kind: EventKind) -> StateSyncEvent {
        self.make_event(Topic::Profiles, kind, None, None, None, None, None)
    }

    fn unseal(&self, st: &State, evt: &StateSyncEvent) -> Option<SealedPayloadIn> {
        let key = st.active_key()?;
        let combined = json::try_b64(evt.sealed_payload_base64.as_deref())?;
        if combined.len() <= crypto::BLOB_OVERHEAD {
            return None;
        }
        let ik = ipc_key(&key);
        let plain = crypto::open_raw(&combined, ik.expose(), &event_aad(evt))?;
        serde_json::from_slice(&plain).ok()
    }

    /// Builds a broker event exactly as this window would publish it (privacy tests).
    #[doc(hidden)]
    pub fn create_broker_event_for_tests(
        &self,
        topic: Topic,
        kind: EventKind,
        video_path: Option<&str>,
        position: Option<f64>,
        duration: Option<f64>,
        payload_b64: Option<&str>,
    ) -> StateSyncEvent {
        let st = self.state.lock();
        let key = st.active_username.as_deref().and_then(|u| st.session_keys.get(u).cloned());
        let p = video_path.map(norm);
        self.make_event(topic, kind, key.as_ref(), p.as_deref(), position, duration, payload_b64)
    }

    fn enqueue_video_and_progress(
        &self,
        key: Option<&SecretKey32>,
        logical_key: &str,
        stats: &VideoUserStats,
        include_progress: bool,
        request_id: Option<&str>,
        events: Option<Vec<StateSyncEvent>>,
    ) {
        let Some(k) = key else { return };
        let skey = storage_video_key(k, logical_key);
        let owner = storage_owner_tag(k);
        let req_id = request_id.filter(|r| !r.trim().is_empty()).map(str::to_owned).unwrap_or_else(sa_core::text::guid_n);
        let mut r = self.request(WriteKind::UpsertVideo);
        r.request_id = req_id.clone();
        r.username = owner.clone();
        r.key = skey.clone();
        r.blob = Some(json::b64_encode(&enc_video(stats, k)));
        r.post_commit_events = if include_progress { None } else { events.clone() };
        self.submit(r);
        if include_progress {
            let mut p = self.request(WriteKind::UpsertWatchProgress);
            p.request_id = format!("{req_id}:progress");
            p.username = owner;
            p.key = skey;
            p.blob = Some(json::b64_encode(&enc_progress(stats, k)));
            p.post_commit_events = events;
            self.submit(p);
        }
    }

    fn enqueue_progress_only(&self, key: Option<&SecretKey32>, logical_key: &str, stats: &VideoUserStats, events: Option<Vec<StateSyncEvent>>) {
        let Some(k) = key else { return };
        let mut r = self.request(WriteKind::UpsertWatchProgress);
        r.username = storage_owner_tag(k);
        r.key = storage_video_key(k, logical_key);
        r.blob = Some(json::b64_encode(&enc_progress(stats, k)));
        r.post_commit_events = events;
        self.submit(r);
    }

    fn enqueue_list_header(&self, key: Option<&SecretKey32>, list: &UserVideoList, events: Option<Vec<StateSyncEvent>>) {
        let Some(k) = key else { return };
        let mut r = self.request(WriteKind::UpsertCustomList);
        r.username = storage_owner_tag(k);
        r.key = list.id.clone();
        r.blob = Some(json::b64_encode(&enc_list_header(list, k)));
        r.post_commit_events = events;
        self.submit(r);
    }

    fn list_item_row(username: &str, list_id: &str, ord: i32, path: &str, k: &SecretKey32) -> ListItemRow {
        ListItemRow {
            list_id: list_id.to_owned(),
            item_hash: list_item_hash(username, list_id, path, k),
            ord,
            encrypted_path_blob: enc_list_path(path, k),
        }
    }

    fn list_item_rows(username: &str, list_id: &str, paths_: &[String], k: &SecretKey32) -> Vec<ListItemRow> {
        paths_
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let n = norm(p);
                (!n.is_empty()).then(|| Self::list_item_row(username, list_id, i as i32, &n, k))
            })
            .collect()
    }

    fn replace_list_items_request(&self, owner: &str, list_id: &str, rows: &[ListItemRow]) {
        let mut r = self.request(WriteKind::ReplaceCustomListItems);
        r.username = owner.to_owned();
        r.key = list_id.to_owned();
        r.batch_list_items = Some(
            rows.iter()
                .map(|li| sa_sync::BatchListItem {
                    list_id: Some(li.list_id.clone()),
                    item_hash: Some(li.item_hash.clone()),
                    ord: li.ord,
                    encrypted_path_blob: Some(json::b64_encode(&li.encrypted_path_blob)),
                })
                .collect(),
        );
        self.submit(r);
    }

    fn enqueue_list_item_add(&self, key: Option<&SecretKey32>, username: &str, list_id: &str, ord: i32, path: &str, events: Option<Vec<StateSyncEvent>>) {
        let Some(k) = key else { return };
        let row = Self::list_item_row(username, list_id, ord, path, k);
        let mut r = self.request(WriteKind::UpsertCustomListItem);
        r.username = storage_owner_tag(k);
        r.key = list_id.to_owned();
        r.secondary_key = row.item_hash;
        r.ord = row.ord;
        r.blob = Some(json::b64_encode(&row.encrypted_path_blob));
        r.post_commit_events = events;
        self.submit(r);
    }

    fn enqueue_list_item_remove(&self, key: Option<&SecretKey32>, username: &str, list_id: &str, path: &str, events: Option<Vec<StateSyncEvent>>) {
        let Some(k) = key else { return };
        let mut r = self.request(WriteKind::DeleteCustomListItem);
        r.username = storage_owner_tag(k);
        r.key = list_id.to_owned();
        r.secondary_key = list_item_hash(username, list_id, path, k);
        r.post_commit_events = events;
        self.submit(r);
    }

    fn migrate_video_key_request(&self, k: &SecretKey32, old_storage_key: String, new_storage_key: String, stats: &VideoUserStats, progress: bool) {
        let mut r = self.request(WriteKind::MigrateVideoKey);
        r.username = storage_owner_tag(k);
        r.key = old_storage_key;
        r.secondary_key = new_storage_key;
        r.blob = Some(json::b64_encode(&enc_video(stats, k)));
        r.secondary_blob = progress.then(|| json::b64_encode(&enc_progress(stats, k)));
        self.submit(r);
    }

    // ----- identity resolution

    /// `ResolveAndMigrateVideoKeyLocked` → `(found, key, stats, migrated)`.
    fn resolve_and_migrate(&self, e: &mut ProfileEntry, key: Option<&SecretKey32>, normalized: &str) -> (bool, String, Option<VideoUserStats>, bool) {
        if let Some(s) = e.videos.get(normalized) {
            return (true, normalized.to_owned(), Some(s.clone()), false);
        }
        let cached = cached_fp(normalized);
        if let Some(fp) = &cached {
            if let Some(s) = e.videos.get(fp) {
                if eq_ci(&s.video_path, normalized) {
                    return (true, fp.clone(), Some(s.clone()), false);
                }
            }
        }
        if let Some((k, v)) = e.videos.iter().find(|(_, v)| eq_ci(&v.video_path, normalized)) {
            return (true, k.to_owned(), Some(v.clone()), false);
        }
        let mut fingerprint = cached;
        if fingerprint.is_none() && !e.videos.is_empty() {
            if let Ok(meta) = std::fs::metadata(normalized) {
                if meta.is_file() && meta.len() > 0 {
                    let prefix = format!("fp:{}:", meta.len());
                    if e.videos.values().any(|v| v.content_fingerprint.as_deref().is_some_and(|fp| netcompat::starts_with_ci(fp, &prefix))) {
                        fingerprint = compute_fp(normalized);
                    }
                }
            }
        }
        let candidate = fingerprint.clone().unwrap_or_else(|| normalized.to_owned());
        let mut found: Option<(String, VideoUserStats)> = None;
        if let Some(fp) = &fingerprint {
            if let Some((k, v)) = e.videos.get_key_value(fp) {
                found = Some((k.to_owned(), v.clone()));
            } else if let Some((k, v)) = e.videos.iter().find(|(_, v)| netcompat::opt_eq_ci(v.content_fingerprint.as_deref(), Some(fp))) {
                found = Some((k.to_owned(), v.clone()));
            }
        }
        let Some((found_key, found_stats)) = found else { return (false, candidate, None, false) };
        let canonical = fingerprint.clone().or_else(|| found_stats.content_fingerprint.clone()).unwrap_or_else(|| found_key.clone());
        let path_changed = !eq_ci(&found_stats.video_path, normalized);
        let key_changed = !eq_ci(&found_key, &canonical);
        let fp_added = fingerprint.is_some() && !netcompat::opt_eq_ci(found_stats.content_fingerprint.as_deref(), fingerprint.as_deref());
        if !(path_changed || key_changed || fp_added) {
            return (true, found_key, Some(found_stats), false);
        }
        let old_path = found_stats.video_path.clone();
        let mut updated = found_stats.clone();
        updated.content_fingerprint = fingerprint.clone().or(found_stats.content_fingerprint.clone());
        updated.video_path = normalized.to_owned();
        updated.file_name = paths::file_name(normalized);
        updated.directory_path = paths::directory_name(normalized);
        if key_changed {
            e.videos.remove(&found_key);
            if let Some(t) = e.decoded_thumbnails.remove(&found_key) {
                e.decoded_thumbnails.insert(&canonical, t);
            }
            e.missing_thumbnails.remove(&found_key);
        }
        e.videos.insert(&canonical, updated.clone());
        if let Some(k) = key {
            self.migrate_video_key_request(k, storage_video_key(k, &found_key), storage_video_key(k, &canonical), &updated, true);
        }
        if path_changed && !old_path.is_empty() {
            self.migrate_custom_list_paths(e, key, &old_path, normalized);
        }
        (true, canonical, Some(updated), true)
    }

    fn migrate_custom_list_paths(&self, e: &mut ProfileEntry, key: Option<&SecretKey32>, old: &str, new: &str) {
        let username = e.username.clone();
        for i in 0..e.custom_lists.len() {
            if !e.custom_lists[i].video_paths.iter().any(|p| eq_ci(p, old)) {
                continue;
            }
            let mut seen = CiSet::new();
            let mut updated_paths = Vec::with_capacity(e.custom_lists[i].video_paths.len());
            for p in &e.custom_lists[i].video_paths {
                let replacement = if eq_ci(p, old) { new.to_owned() } else { p.clone() };
                if seen.insert(&replacement) {
                    updated_paths.push(replacement);
                }
            }
            e.custom_lists[i].video_paths = updated_paths;
            e.custom_lists[i].updated_at = Ticks::now();
            let list = e.custom_lists[i].clone();
            self.enqueue_list_header(key, &list, None);
            if let Some(k) = key {
                let rows = Self::list_item_rows(&username, &list.id, &list.video_paths, k);
                self.replace_list_items_request(&storage_owner_tag(k), &list.id, &rows);
            }
        }
    }

    /// `GetOrCreateStatsLocked`. The fingerprint is computed before a row is created, whatever the file size
    /// (UNIFIED.md §3.12 FIX-W).
    fn get_or_create(&self, e: &mut ProfileEntry, key: Option<&SecretKey32>, normalized: &str, now: Ticks, persist: bool) -> (String, VideoUserStats) {
        let (found, k, s, _) = self.resolve_and_migrate(e, key, normalized);
        if found {
            if let Some(s) = s {
                return (k, s);
            }
        }
        let mut fp = cached_fp(normalized);
        if fp.is_none() {
            if let Ok(meta) = std::fs::metadata(normalized) {
                if meta.is_file() && meta.len() > 0 {
                    fp = compute_fp(normalized);
                }
            }
        }
        let new_key = fp.clone().unwrap_or_else(|| normalized.to_owned());
        let created = create_default(normalized, now, fp, false);
        e.videos.insert(&new_key, created.clone());
        if persist {
            self.enqueue_video_and_progress(key, &new_key, &created, false, None, None);
        }
        (new_key, created)
    }

    /// Re-keys reachable path-keyed videos to their `fp:` key (UNIFIED.md §4 item 7).
    fn migrate_path_keyed_videos(&self, e: &mut ProfileEntry, key: Option<&SecretKey32>) -> bool {
        let Some(k) = key else { return false };
        let candidates: Vec<(String, VideoUserStats)> = e
            .videos
            .iter()
            .filter(|(lk, v)| v.content_fingerprint.is_none() && !lk.starts_with("fp:") && eq_ci(lk, &v.video_path))
            .map(|(lk, v)| (lk.to_owned(), v.clone()))
            .collect();
        let mut roots = HashMap::new();
        let mut changed = false;
        for (old_key, stats) in candidates {
            if !self.storage_available(&stats.video_path, &mut roots) {
                continue;
            }
            let Some(fp) = compute_fp(&stats.video_path) else { continue };
            if e.videos.contains_key(&fp) {
                continue;
            }
            let mut updated = stats.clone();
            updated.content_fingerprint = Some(fp.clone());
            e.videos.remove(&old_key);
            if let Some(t) = e.decoded_thumbnails.remove(&old_key) {
                e.decoded_thumbnails.insert(&fp, t);
            }
            e.missing_thumbnails.remove(&old_key);
            e.videos.insert(&fp, updated.clone());
            let progress = updated.last_position_seconds > 0.0 || updated.duration_seconds > 0.0;
            self.migrate_video_key_request(k, storage_video_key(k, &old_key), storage_video_key(k, &fp), &updated, progress);
            changed = true;
        }
        changed
    }

    // ----- thumbnails

    fn thumbnail_by_key(&self, e: &mut ProfileEntry, key: Option<&SecretKey32>, logical: &str, normalized: &str) -> Option<Vec<u8>> {
        if let Some(t) = e.decoded_thumbnails.get(logical) {
            return Some(t.clone());
        }
        if e.missing_thumbnails.contains(logical) {
            return None;
        }
        let k = key?;
        let store = self.sql_store();
        let owner = storage_owner_tag(k);
        let blob = store.try_get_thumbnail_blob(&owner, &storage_video_key(k, logical)).or_else(|| {
            if !eq_ci(logical, normalized) { store.try_get_thumbnail_blob(&owner, &storage_video_key(k, normalized)) } else { None }
        });
        if let Some(plain) = blob.filter(|b| !b.is_empty()).and_then(|b| crypto::open(&b, k)).filter(|p| !p.is_empty()) {
            e.decoded_thumbnails.insert(logical, plain.clone());
            return Some(plain);
        }
        e.missing_thumbnails.insert(logical);
        None
    }

    fn thumbnail_for_path(&self, e: &mut ProfileEntry, key: Option<&SecretKey32>, normalized: &str) -> Option<Vec<u8>> {
        let (found, k, _, _) = self.resolve_and_migrate(e, key, normalized);
        if found {
            if let Some(t) = self.thumbnail_by_key(e, key, &k, normalized) {
                return Some(t);
            }
        }
        if let Some(fp) = cached_fp(normalized) {
            if let Some(t) = self.thumbnail_by_key(e, key, &fp, normalized) {
                return Some(t);
            }
        }
        self.thumbnail_by_key(e, key, normalized, normalized)
    }

    // ----- decryption from disk

    fn try_decrypt_legacy_vault(b64: Option<&str>, key: &SecretKey32) -> Option<json::LegacyVault> {
        let text = b64?;
        if text.trim().is_empty() {
            return None;
        }
        let combined = json::b64_decode(text)?;
        let plain = crypto::open(&combined, key)?;
        if plain.is_empty() || plain == crypto::VAULT_MARKER.as_bytes() {
            return None;
        }
        json::parse_legacy_vault(&plain)
    }

    /// `DecryptIntoProfileEntryLocked` → whether a legacy vault was migrated (the header must be rewritten).
    fn decrypt_into_entry(&self, entry: &mut ProfileEntry, key: &SecretKey32, rewrites: &mut CiSet) -> bool {
        entry.clear_decrypted();
        let store = self.sql_store();
        let mut migrated_legacy = false;
        let owner = storage_owner_tag(key);

        let has_legacy_owner_rows = store.has_rows_owned_by(&entry.username);
        if has_legacy_owner_rows {
            let mut r = self.request(WriteKind::RetagOwner);
            r.username = entry.username.clone();
            r.secondary_key = owner.clone();
            self.submit(r);
        }

        if let Some(vault) = Self::try_decrypt_legacy_vault(entry.encrypted_vault.as_deref(), key) {
            let has_data = vault.videos.as_ref().is_some_and(|v| !v.is_empty())
                || vault.custom_lists.as_ref().is_some_and(|v| !v.is_empty())
                || vault.thumbnails.as_ref().is_some_and(|v| !v.is_empty());
            if has_data {
                migrated_legacy = self.import_legacy_vault(entry, key, &owner, vault);
            } else {
                entry.encrypted_vault = Some(vault_marker(key));
                migrated_legacy = true;
            }
        }

        let load_both = |f: &dyn Fn(&str) -> Vec<(String, Vec<u8>)>| -> Vec<(String, Vec<u8>)> {
            let mut v = f(&owner);
            if has_legacy_owner_rows {
                v.extend(f(&entry.username));
            }
            v
        };

        let mut db_videos = store.load_videos_with_view_counts(&owner);
        if has_legacy_owner_rows {
            db_videos.extend(store.load_videos_with_view_counts(&entry.username));
        }
        let mut logical_by_storage: CiMap<String> = CiMap::new();
        let mut has_unhashed = false;
        let mut unpadded = CiSet::new();
        for (skey, blob, db_vc) in db_videos {
            if blob.len() < crypto::BLOB_OVERHEAD + VIDEO_ROW_BLOB_BUCKET {
                unpadded.insert(&skey);
            }
            if !skey.starts_with(HASHED_VIDEO_KEY_PREFIX) {
                has_unhashed = true;
            }
            let Some(dto) = dec_video(&blob, key) else { continue };
            let n = norm(dto.video_path.as_deref().unwrap_or(""));
            if n.is_empty() {
                continue;
            }
            let fp = dto.content_fingerprint.as_deref().filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_owned()).or_else(|| cached_fp(&n));
            let canonical = fp.clone().unwrap_or_else(|| n.clone());
            logical_by_storage.insert(&skey, canonical.clone());
            let view_count = if db_vc >= 0 { db_vc } else { i64::from(dto.view_count.max(0)) };
            entry.videos.insert(
                &canonical,
                VideoUserStats {
                    content_fingerprint: fp,
                    video_path: n.clone(),
                    file_name: dto.file_name.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| paths::file_name(&n)),
                    directory_path: dto.directory_path.unwrap_or_else(|| paths::directory_name(&n)),
                    view_count,
                    is_liked: dto.is_liked,
                    rating: i64::from(dto.rating.clamp(0, 5)),
                    last_position_seconds: 0.0,
                    duration_seconds: 0.0,
                    last_watched_at: dto.last_watched_at_utc,
                    updated_at: dto.updated_at_utc,
                    thumbnail: None,
                },
            );
        }

        for (skey, blob) in load_both(&|o| store.load_watch_progress(o)) {
            if !skey.starts_with(HASHED_VIDEO_KEY_PREFIX) {
                has_unhashed = true;
            }
            if blob.len() < crypto::BLOB_OVERHEAD + SMALL_ROW_BLOB_BUCKET {
                unpadded.insert(&skey);
            }
            let Some(p) = dec_progress(&blob, key) else { continue };
            if let Some(lk) = logical_by_storage.get(&skey).cloned() {
                if let Some(v) = entry.videos.get_mut(&lk) {
                    v.last_position_seconds = p.last_position_seconds.max(0.0);
                    v.duration_seconds = p.duration_seconds.max(0.0);
                    if p.updated_at_utc > v.updated_at {
                        v.updated_at = p.updated_at_utc;
                    }
                }
            }
        }

        self.rewrite_unhashed_video_keys(entry, key, &logical_by_storage, has_unhashed, rewrites);
        self.repad_legacy_video_blobs(entry, key, &owner, &logical_by_storage, &unpadded, rewrites);

        let mut items_by_list: CiMap<Vec<String>> = CiMap::new();
        let mut unpadded_lists = CiSet::new();
        for (list_id, path_blob) in load_both(&|o| store.load_custom_list_items(o)) {
            if path_blob.len() < crypto::BLOB_OVERHEAD + LIST_ITEM_PATH_BUCKET {
                unpadded_lists.insert(&list_id);
            }
            let Some(bytes) = crypto::open(&path_blob, key).filter(|b| !b.is_empty()) else { continue };
            let text = String::from_utf8_lossy(&bytes);
            let n = norm(text.trim_end_matches('\0'));
            if n.is_empty() {
                continue;
            }
            if !items_by_list.contains_key(&list_id) {
                items_by_list.insert(&list_id, Vec::new());
            }
            let list = items_by_list.get_mut(&list_id).expect("inserted");
            if !list.iter().any(|p| eq_ci(p, &n)) {
                list.push(n.clone());
            }
            if !entry.videos.contains_key(&n) {
                let fp = cached_fp(&n).or_else(|| compute_fp(&n));
                let mem_key = fp.clone().unwrap_or_else(|| n.clone());
                if !entry.videos.contains_key(&mem_key) && !entry.videos.values().any(|v| eq_ci(&v.video_path, &n)) {
                    entry.videos.insert(&mem_key, create_default(&n, Ticks::now(), fp, false));
                }
            }
        }

        let db_lists = load_both(&|o| store.load_custom_lists(o));
        if !db_lists.is_empty() {
            entry.custom_lists.clear();
            for (_list_id, blob) in db_lists {
                let Some(h) = dec_list_header(&blob, key) else { continue };
                let (Some(id), Some(name)) = (h.id, h.name) else { continue };
                if id.trim().is_empty() || name.trim().is_empty() {
                    continue;
                }
                let paths_ = items_by_list.get(&id).cloned().unwrap_or_default();
                entry.custom_lists.push(UserVideoList {
                    id: id.clone(),
                    name: name.trim_matches(char::is_whitespace).to_owned(),
                    video_paths: paths_.clone(),
                    created_at: h.created_at_utc,
                    updated_at: h.updated_at_utc,
                });
                if unpadded_lists.contains(&id) && !paths_.is_empty() {
                    let rows = Self::list_item_rows(&entry.username, &id, &paths_, key);
                    self.replace_list_items_request(&owner, &id, &rows);
                }
            }
        }
        migrated_legacy
    }

    fn import_legacy_vault(&self, entry: &mut ProfileEntry, key: &SecretKey32, owner: &str, vault: json::LegacyVault) -> bool {
        if Path::new(&self.file_path).exists() && !Path::new(&self.backup_path).exists() {
            if let Some(header) = netcompat::read_all_text_shared(Path::new(&self.file_path), 5) {
                // The backup keeps the encrypted legacy vault, never a remembered (auto-unlock) key.
                let _ = netcompat::write_all_text_atomic(Path::new(&self.backup_path), &json::strip_remembered_session(&header), 5);
            }
        }
        // Rows deduplicated by key (last wins) so verification can succeed (Q2 fix).
        let mut video_rows: CiMap<sqlite::BlobRow> = CiMap::new();
        let mut progress_rows: CiMap<sqlite::BlobRow> = CiMap::new();
        let mut list_rows: CiMap<sqlite::BlobRow> = CiMap::new();
        let mut item_rows: CiMap<ListItemRow> = CiMap::new();
        let mut thumb_rows: CiMap<sqlite::BlobRow> = CiMap::new();

        for v in vault.videos.into_iter().flatten() {
            let n = norm(v.video_path.as_deref().unwrap_or(""));
            if n.is_empty() {
                continue;
            }
            let fp = v.content_fingerprint.as_deref().filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_owned()).or_else(|| cached_fp(&n));
            let lk = fp.clone().unwrap_or_else(|| n.clone());
            let s = VideoUserStats {
                content_fingerprint: fp,
                video_path: n.clone(),
                file_name: v.file_name.clone().filter(|f| !f.trim().is_empty()).unwrap_or_else(|| paths::file_name(&n)),
                directory_path: v.directory_path.clone().unwrap_or_else(|| paths::directory_name(&n)),
                view_count: i64::from(v.view_count.max(0)),
                is_liked: v.is_liked,
                rating: i64::from(v.rating.clamp(0, 5)),
                last_position_seconds: v.last_position_seconds,
                duration_seconds: v.duration_seconds,
                last_watched_at: v.last_watched_at,
                updated_at: v.updated_at,
                thumbnail: None,
            };
            let skey = storage_video_key(key, &lk);
            video_rows.remove(&skey);
            video_rows.insert(&skey, (skey.clone(), enc_video(&s, key), 0));
            if s.last_position_seconds > 0.0 || s.duration_seconds > 0.0 {
                progress_rows.remove(&skey);
                progress_rows.insert(&skey, (skey.clone(), enc_progress(&s, key), 0));
            } else {
                progress_rows.remove(&skey);
            }
            entry.videos.insert(&lk, s);
        }
        for l in vault.custom_lists.into_iter().flatten() {
            let (Some(id), Some(name)) = (l.id.clone(), l.name.clone()) else { continue };
            if id.trim().is_empty() || name.trim().is_empty() {
                continue;
            }
            let mut seen = CiSet::new();
            let mut paths_ = Vec::new();
            for p in l.video_paths.iter().flatten() {
                let n = norm(p.as_deref().unwrap_or(""));
                if !n.is_empty() && seen.insert(&n) {
                    paths_.push(n);
                }
            }
            let list = UserVideoList {
                id: id.clone(),
                name: name.trim_matches(char::is_whitespace).to_owned(),
                video_paths: paths_.clone(),
                created_at: l.created_at,
                updated_at: l.updated_at,
            };
            list_rows.remove(&id);
            list_rows.insert(&id, (id.clone(), enc_list_header(&list, key), 0));
            for row in Self::list_item_rows(&entry.username, &id, &paths_, key) {
                let k = format!("{}\n{}", row.list_id, row.item_hash);
                item_rows.remove(&k);
                item_rows.insert(&k, row);
            }
            entry.custom_lists.push(list);
        }
        for (k, b64) in vault.thumbnails.into_iter().flatten() {
            if k.trim().is_empty() {
                continue;
            }
            let Some(bytes) = json::try_b64(b64.as_deref()).filter(|b| !b.is_empty()) else { continue };
            let skey = storage_video_key(key, &k);
            thumb_rows.remove(&skey);
            thumb_rows.insert(&skey, (skey.clone(), crypto::seal(&bytes, key), 0));
            entry.decoded_thumbnails.insert(&k, bytes);
        }
        let store = self.sql_store();
        store.enqueue_upsert_profile(&entry.username, &entry.salt_b64, &entry.hash_b64, entry.iterations, entry.created);
        let v: Vec<_> = video_rows.values().cloned().collect();
        let p: Vec<_> = progress_rows.values().cloned().collect();
        let l: Vec<_> = list_rows.values().cloned().collect();
        let i: Vec<_> = item_rows.values().cloned().collect();
        let t: Vec<_> = thumb_rows.values().cloned().collect();
        if store.import_legacy_vault_batch_and_verify(owner, &v, &p, &l, &i, &t) {
            entry.encrypted_vault = Some(vault_marker(key));
            return true;
        }
        false
    }

    fn rewrite_unhashed_video_keys(&self, entry: &ProfileEntry, key: &SecretKey32, logical_by_storage: &CiMap<String>, has_unhashed: bool, rewrites: &mut CiSet) {
        for (skey, logical) in logical_by_storage.iter() {
            let expected = storage_video_key(key, logical);
            if eq_ci(skey, &expected) {
                continue;
            }
            let Some(stats) = entry.videos.get(logical) else { continue };
            if !rewrites.insert(&format!("{}|{skey}|{expected}", entry.username)) {
                continue;
            }
            let progress = stats.last_position_seconds > 0.0 || stats.duration_seconds > 0.0;
            self.migrate_video_key_request(key, skey.to_owned(), expected, stats, progress);
        }
        if has_unhashed && rewrites.insert(&format!("{}|purge", entry.username)) {
            let mut r = self.request(WriteKind::PurgeLegacyVideoKeys);
            r.username = storage_owner_tag(key);
            self.submit(r);
        }
    }

    fn repad_legacy_video_blobs(
        &self,
        entry: &ProfileEntry,
        key: &SecretKey32,
        owner: &str,
        logical_by_storage: &CiMap<String>,
        unpadded: &CiSet,
        rewrites: &mut CiSet,
    ) {
        for (skey, logical) in logical_by_storage.iter() {
            if !unpadded.contains(skey) || !skey.starts_with(HASHED_VIDEO_KEY_PREFIX) {
                continue;
            }
            if !eq_ci(&storage_video_key(key, logical), skey) {
                continue;
            }
            let Some(stats) = entry.videos.get(logical) else { continue };
            if !rewrites.insert(&format!("{owner}|repad|{skey}")) {
                continue;
            }
            let mut r = self.request(WriteKind::UpsertVideo);
            r.username = owner.to_owned();
            r.key = skey.to_owned();
            r.blob = Some(json::b64_encode(&enc_video(stats, key)));
            r.secondary_blob = (stats.last_position_seconds > 0.0 || stats.duration_seconds > 0.0)
                .then(|| json::b64_encode(&enc_progress(stats, key)));
            self.submit(r);
        }
    }

    fn load_from_disk(&self) {
        self.drain();
        let mut st = self.state.lock();
        self.scrub_backup_file();
        self.sync_from_disk(&mut st, true);
    }

    /// `ScrubBackupFile`: drops a remembered key from `profiles.json.bak` once.
    fn scrub_backup_file(&self) {
        let p = Path::new(&self.backup_path);
        if !p.exists() {
            return;
        }
        let Some(backup) = netcompat::read_all_text_shared(p, 5) else { return };
        if !backup.contains("rememberedProtectedKeyBase64") {
            return;
        }
        let _ = netcompat::write_all_text_atomic(p, &json::strip_remembered_session(&backup), 5);
    }

    fn notify(&self, change: ProfileChange) {
        self.changes.notify(&change);
    }

    // ----- broker events

    fn on_broker_event(&self, evt: &StateSyncEvent) {
        if evt.sender_instance_id == self.instance_id || !eq_ci(&evt.scope_path, &self.file_path) {
            return;
        }
        if evt.topic.eq_ignore_ascii_case(Topic::WatchProgress.wire()) {
            if evt.sealed_payload_base64.as_deref().is_none_or(|s| s.trim().is_empty()) {
                return;
            }
            let mut st = self.state.lock();
            if st.active_profile.is_none() {
                return;
            }
            let Some(payload) = self.unseal(&st, evt) else { return };
            let n = norm(payload.video_path.as_deref().unwrap_or(""));
            if n.is_empty() {
                return;
            }
            let ts = parse_ts(&evt.timestamp_utc);
            let Some((e, key)) = st.active() else { return };
            let (k, existing) = self.get_or_create(e, key.as_ref(), &n, Ticks::now(), false);
            let dur = sanitize_seconds(payload.duration_seconds.unwrap_or(0.0), existing.duration_seconds);
            let mut pos = payload.position_seconds.unwrap_or(existing.last_position_seconds).max(0.0);
            if dur > 0.0 && pos > dur {
                pos = dur;
            }
            let mut updated = existing;
            updated.last_position_seconds = pos;
            updated.duration_seconds = dur;
            updated.updated_at = ts;
            e.videos.insert(&k, updated);
            return;
        }
        if evt.topic.eq_ignore_ascii_case(Topic::Thumbnail.wire()) {
            if evt.sealed_payload_base64.as_deref().is_none_or(|s| s.trim().is_empty()) {
                return;
            }
            let mut st = self.state.lock();
            if st.active_profile.is_none() {
                return;
            }
            let Some(payload) = self.unseal(&st, evt) else { return };
            let Some(bytes) = json::try_b64(payload.thumbnail_base64.as_deref()).filter(|b| !b.is_empty()) else { return };
            let n = norm(payload.video_path.as_deref().unwrap_or(""));
            if n.is_empty() {
                return;
            }
            let Some((e, key)) = st.active() else { return };
            let (k, _) = self.get_or_create(e, key.as_ref(), &n, Ticks::now(), false);
            e.decoded_thumbnails.insert(&k, bytes);
            e.missing_thumbnails.remove(&k);
            return;
        }
        if evt.topic.eq_ignore_ascii_case(Topic::Profiles.wire()) {
            let mut updated;
            // Another instance logged out or deleted a profile: the resync below may end this process's session.
            // (A login elsewhere only restores a session here while none is active, and list events change no
            // session, so streams keep their key through those.)
            let _change =
                matches!(evt.kind, EventKind::ProfileLogout | EventKind::ProfileDeleted).then(|| self.state.begin_session_change());
            {
                let mut st = self.state.lock();
                self.dirty.store(true, Ordering::SeqCst);
                updated = self.sync_from_disk(&mut st, true);
                if evt.kind == EventKind::ProfileLogout && st.remembered_username.is_none() && st.active_username.is_some() {
                    self.end_all_sessions(&mut st);
                    st.active_profile = None;
                    st.active_username = None;
                    st.session_token += 1;
                    updated = true;
                }
            }
            if updated {
                self.notify(ProfileChange::External);
            }
        }
    }

    // ----- public C#-parity API

    /// `Register`.
    pub fn register_user(&self, username: &str, password: &str, remember_me: bool) -> Result<(), ProfileError> {
        let u = username.trim_matches(char::is_whitespace).to_owned();
        if u.is_empty() {
            return Err(self.fail(ProfileError::EmptyUsername));
        }
        if password.is_empty() {
            return Err(self.fail(ProfileError::EmptyPassword));
        }
        // PBKDF2 runs before the locks are taken (it does not depend on the state), so the cross-process mutex is
        // held only for the header edit; the C# app derives inside, which can outlast the 2 s wait of other writers.
        let salt = crypto::random_salt();
        let (hash, key) = crypto::derive(password, &salt, PBKDF2_ITERATIONS as u32);
        self.drain();
        let _change = self.state.begin_session_change();
        {
            let mut st = self.state.lock();
            let _m = netcompat::CrossProcessMutex::acquire(&self.mutex_name, 2000);
            self.sync_from_disk(&mut st, false);
            if st.profiles.contains_key(&u) {
                let err = ProfileError::UsernameTaken;
                st.last_error = Some(err.message());
                return Err(err);
            }
            let entry = ProfileEntry {
                username: u.clone(),
                salt_b64: json::b64_encode(&salt),
                hash_b64: json::b64_encode(&hash),
                iterations: PBKDF2_ITERATIONS,
                created: Ticks::now(),
                encrypted_vault: Some(vault_marker(&key)),
                videos: CiMap::new(),
                custom_lists: Vec::new(),
                decoded_thumbnails: CiMap::new(),
                missing_thumbnails: CiSet::new(),
            };
            self.end_all_sessions(&mut st);
            st.session_keys.insert(&u, key.clone());
            let (salt_b64, hash_b64, created) = (entry.salt_b64.clone(), entry.hash_b64.clone(), entry.created);
            st.profiles.insert(&u, entry);
            let stored = st.profiles.get_key_value(&u).map(|(k, _)| k.to_owned()).unwrap_or(u.clone());
            st.active_profile = Some(stored.clone());
            st.active_username = Some(u.clone());
            st.session_token += 1;
            st.last_username = Some(u.clone());
            st.remember_me = remember_me;
            st.user_logged_out_this_session = false;
            st.unpersisted_logout_key = None;
            if remember_me {
                st.remembered_username = Some(u.clone());
                st.remembered_key = Some(protect_vault_key(&key, &salt));
            } else {
                st.remembered_username = None;
                st.remembered_key = None;
            }
            st.last_error = None;
            self.save_auth_header(&mut st);
            let mut r = self.request(WriteKind::UpsertProfile);
            r.username = u.clone();
            r.salt_base64 = Some(salt_b64);
            r.password_hash_base64 = Some(hash_b64);
            r.iterations = PBKDF2_ITERATIONS;
            r.created_at_utc = json::format_json_date(created);
            r.post_commit_events = Some(vec![self.profiles_event(EventKind::ProfileLogin), self.profiles_event(EventKind::ListChanged)]);
            self.submit(r);
        }
        self.notify(ProfileChange::Local);
        Ok(())
    }

    fn fail(&self, err: ProfileError) -> ProfileError {
        self.state.lock().last_error = Some(err.message());
        err
    }

    fn verify(entry: &ProfileEntry, password: &str) -> Option<SecretKey32> {
        Self::verify_creds(&entry.salt_b64, &entry.hash_b64, entry.iterations, password)
    }

    fn verify_creds(salt_b64: &str, hash_b64: &str, iterations: i32, password: &str) -> Option<SecretKey32> {
        let salt = json::b64_decode(salt_b64)?;
        let hash = json::b64_decode(hash_b64)?;
        let it = if iterations > 0 { iterations } else { PBKDF2_ITERATIONS };
        crypto::verify(password, &salt, &hash, it as u32)
    }

    /// Verifies the password against the entry as currently known, outside the cross-process mutex (PBKDF2 is
    /// slow). The caller uses the key only if the entry still has the same salt and hash under the locks.
    fn preverify(&self, username: &str, password: &str) -> Option<(String, String, SecretKey32)> {
        let (salt, hash, iterations) = {
            let mut st = self.state.lock();
            self.sync_from_disk(&mut st, false);
            let e = st.profiles.get(username)?;
            (e.salt_b64.clone(), e.hash_b64.clone(), e.iterations)
        };
        let key = Self::verify_creds(&salt, &hash, iterations, password)?;
        Some((salt, hash, key))
    }

    /// The vault key for `username` under the locks: the pre-verified one when the entry is unchanged, else a
    /// fresh verification.
    fn verified_key(st: &State, username: &str, password: &str, pre: Option<(String, String, SecretKey32)>) -> Option<SecretKey32> {
        let e = st.profiles.get(username)?;
        match pre {
            Some((salt, hash, key)) if salt == e.salt_b64 && hash == e.hash_b64 => Some(key),
            _ => Self::verify(e, password),
        }
    }

    /// `Login`.
    pub fn login_user(&self, username: &str, password: &str, remember_me: bool) -> Result<(), ProfileError> {
        let u = username.trim_matches(char::is_whitespace).to_owned();
        if u.is_empty() || password.is_empty() {
            return Err(self.fail(ProfileError::MissingCredentials));
        }
        self.drain();
        let pre = self.preverify(&u, password);
        let _change = self.state.begin_session_change();
        {
            let mut st = self.state.lock();
            let _m = netcompat::CrossProcessMutex::acquire(&self.mutex_name, 2000);
            self.sync_from_disk(&mut st, false);
            let Some(key) = Self::verified_key(&st, &u, password, pre) else {
                let err = ProfileError::InvalidCredentials;
                st.last_error = Some(err.message());
                return Err(err);
            };
            self.end_all_sessions(&mut st);
            let name = st.profiles.get(&u).map(|e| e.username.clone()).unwrap_or(u.clone());
            let stored = st.profiles.get_key_value(&u).map(|(k, _)| k.to_owned()).unwrap_or(u.clone());
            st.session_keys.insert(&name, key.clone());
            {
                let State { profiles, submitted_rewrites, .. } = &mut *st;
                if let Some(e) = profiles.get_mut(&u) {
                    self.decrypt_into_entry(e, &key, submitted_rewrites);
                    // UNIFIED.md §4 item 7: re-key reachable path-keyed rows to their fingerprint.
                    self.migrate_path_keyed_videos(e, Some(&key));
                }
            }
            st.active_profile = Some(stored);
            st.active_username = Some(name.clone());
            st.session_token += 1;
            st.last_username = Some(name.clone());
            st.remember_me = remember_me;
            st.user_logged_out_this_session = false;
            st.unpersisted_logout_key = None;
            let salt = st.profiles.get(&u).and_then(|e| json::b64_decode(&e.salt_b64)).unwrap_or_default();
            if remember_me {
                st.remembered_username = Some(name.clone());
                st.remembered_key = Some(protect_vault_key(&key, &salt));
            } else {
                st.remembered_username = None;
                st.remembered_key = None;
            }
            st.last_error = None;
            self.save_auth_header(&mut st);
        }
        self.broker.publish(Topic::Profiles, self.profiles_event(EventKind::ProfileLogin));
        self.broker.publish(Topic::Profiles, self.profiles_event(EventKind::ListChanged));
        self.notify(ProfileChange::Local);
        Ok(())
    }

    /// `Logout`.
    pub fn logout_user(&self) {
        self.drain();
        let _change = self.state.begin_session_change();
        let changed;
        {
            let mut st = self.state.lock();
            let _m = netcompat::CrossProcessMutex::acquire(&self.mutex_name, 2000);
            self.sync_from_disk(&mut st, false);
            changed = st.active_profile.is_some() || st.active_username.is_some() || st.remembered_username.is_some();
            if changed {
                let removed = st.remembered_key.take();
                st.active_profile = None;
                st.active_username = None;
                st.remember_me = false;
                st.remembered_username = None;
                if !self.save_auth_header(&mut st) && removed.is_some() {
                    st.unpersisted_logout_key = removed;
                }
            }
            self.end_all_sessions(&mut st);
            st.session_token += 1;
            st.user_logged_out_this_session = true;
            st.last_error = None;
        }
        if changed {
            self.broker.publish(Topic::Profiles, self.profiles_event(EventKind::ProfileLogout));
            self.notify(ProfileChange::Local);
        }
    }

    /// `DeleteProfile`.
    pub fn delete_user(&self, username: &str, password: &str) -> Result<(), ProfileError> {
        let u = username.trim_matches(char::is_whitespace).to_owned();
        if u.is_empty() || password.is_empty() {
            return Err(self.fail(ProfileError::MissingCredentials));
        }
        self.drain();
        let pre = self.preverify(&u, password);
        let _change = self.state.begin_session_change();
        {
            let mut st = self.state.lock();
            let _m = netcompat::CrossProcessMutex::acquire(&self.mutex_name, 2000);
            self.sync_from_disk(&mut st, false);
            let Some(key) = Self::verified_key(&st, &u, password, pre) else {
                let err = ProfileError::InvalidCredentials;
                st.last_error = Some(err.message());
                return Err(err);
            };
            let deleted_owner = storage_owner_tag(&key);
            drop(key);
            let name = st.profiles.get(&u).map(|e| e.username.clone()).unwrap_or(u.clone());
            self.end_session(&mut st, &name);
            st.profiles.remove(&u);
            if st.active_profile.as_deref().is_some_and(|a| eq_ci(a, &name)) {
                st.active_profile = None;
                st.active_username = None;
                st.session_token += 1;
                // Deleting the active profile ends its session: nothing of it stays in memory, including the
                // watched paths of the process-wide fingerprint cache (UNIFIED.md §3.12 FIX).
                self.end_all_sessions(&mut st);
            }
            if st.remembered_username.as_deref().is_some_and(|r| eq_ci(r, &name)) {
                st.remembered_username = None;
                st.remembered_key = None;
            }
            st.last_error = None;
            self.save_auth_header(&mut st);
            let mut r = self.request(WriteKind::DeleteProfileCascade);
            r.username = name;
            r.secondary_key = deleted_owner;
            r.post_commit_events = Some(vec![self.profiles_event(EventKind::ProfileDeleted), self.profiles_event(EventKind::ListChanged)]);
            self.submit(r);
        }
        self.notify(ProfileChange::Local);
        Ok(())
    }

    /// `SessionToken`: changes on every login, logout, switch or restore.
    pub fn session_token(&self) -> i64 {
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        st.session_token
    }

    fn expected_session(st: &State, expected: Option<i64>) -> bool {
        expected.is_none_or(|e| e == st.session_token)
    }

    /// `RecordWatch(path, position, duration, requestId, expectedSessionToken)`.
    pub fn record_watch_with(&self, path: &str, position: f64, duration: f64, request_id: Option<&str>, expected_token: Option<i64>) -> Option<VideoUserStats> {
        let n = norm(path);
        if n.is_empty() {
            return None;
        }
        let supplied = request_id.filter(|r| !r.trim().is_empty()).map(|r| r.trim().to_owned());
        let req = supplied.clone().unwrap_or_else(sa_core::text::guid_n);
        if self.locked_out() {
            return None;
        }
        self.drain();
        let updated;
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            if st.active_profile.is_none() || !Self::expected_session(&st, expected_token) {
                return None;
            }
            let now = Ticks::now();
            let fresh = st.processed_request_ids.insert(req.clone());
            let store = self.sql_store();
            let (e, key) = st.active()?;
            let (lk, existing) = self.get_or_create(e, key.as_ref(), &n, now, false);
            if supplied.is_some() && (!fresh || store.is_request_processed(&req)) {
                self.enqueue_video_and_progress(key.as_ref(), &lk, &existing, false, Some(&req), None);
                return Some(existing);
            }
            let dur = sanitize_seconds(duration, existing.duration_seconds);
            let mut pos = sanitize_seconds(position, existing.last_position_seconds);
            if dur > 0.0 && pos > dur {
                pos = dur;
            }
            let mut u = existing;
            u.view_count += 1;
            u.last_position_seconds = pos;
            u.duration_seconds = dur;
            u.last_watched_at = Some(now);
            u.updated_at = now;
            e.videos.insert(&lk, u.clone());
            let events = vec![self.profiles_event(EventKind::VideoWatched), self.profiles_event(EventKind::ListChanged)];
            self.enqueue_video_and_progress(key.as_ref(), &lk, &u, pos > 0.0 || dur > 0.0, Some(&req), Some(events));
            self.bump(&mut st);
            updated = u;
        }
        self.notify(ProfileChange::Local);
        Some(updated)
    }

    /// `UpdatePlaybackProgress(path, position, duration, expectedSessionToken)`; never raises a change.
    pub fn update_playback_progress_with(&self, path: &str, position: f64, duration: f64, expected_token: Option<i64>) -> Option<VideoUserStats> {
        let n = norm(path);
        if n.is_empty() {
            return None;
        }
        if self.locked_out() {
            return None;
        }
        self.drain();
        let mut silent_event = None;
        let updated;
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            if st.active_profile.is_none() || !Self::expected_session(&st, expected_token) {
                return None;
            }
            let now = Ticks::now();
            let (e, key) = st.active()?;
            let (lk, existing) = self.get_or_create(e, key.as_ref(), &n, now, true);
            let dur = sanitize_seconds(duration, existing.duration_seconds);
            let mut pos = (if position.is_finite() { position } else { existing.last_position_seconds }).max(0.0);
            if dur > 0.0 && pos > dur {
                pos = dur;
            }
            let significant = (pos - existing.last_position_seconds).abs() >= PROGRESS_EPSILON
                || (dur - existing.duration_seconds).abs() >= PROGRESS_EPSILON;
            let mut u = existing.clone();
            u.last_position_seconds = pos;
            u.duration_seconds = dur;
            u.updated_at = if significant { now } else { existing.updated_at };
            e.videos.insert(&lk, u.clone());
            let evt = self.make_event(Topic::WatchProgress, EventKind::ProgressUpdated, key.as_ref(), Some(&n), Some(pos), Some(dur), None);
            if significant {
                self.enqueue_progress_only(key.as_ref(), &lk, &u, Some(vec![evt]));
                self.bump(&mut st);
            } else {
                silent_event = Some(evt);
            }
            updated = u;
        }
        if let Some(evt) = silent_event {
            self.broker.publish(Topic::WatchProgress, evt);
        }
        Some(updated)
    }

    /// `ClearWatchHistory`.
    pub fn clear_history(&self) -> bool {
        if self.locked_out() {
            return false;
        }
        self.drain();
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            let Some((e, key)) = st.active() else { return false };
            let now = Ticks::now();
            let mut listed = CiSet::new();
            for l in &e.custom_lists {
                for p in &l.video_paths {
                    listed.insert(p);
                }
            }
            let keys: Vec<String> = e.videos.keys().map(str::to_owned).collect();
            for k in keys {
                let cur = e.videos.get(&k).cloned().expect("key from map");
                if !cur.is_liked && cur.rating <= 0 && !listed.contains(&cur.video_path) {
                    e.videos.remove(&k);
                    e.decoded_thumbnails.remove(&k);
                    e.missing_thumbnails.remove(&k);
                } else {
                    let v = e.videos.get_mut(&k).expect("key from map");
                    v.view_count = 0;
                    v.last_watched_at = None;
                    v.last_position_seconds = 0.0;
                    v.updated_at = now;
                }
            }
            if let Some(k) = key.as_ref() {
                let mut videos = Vec::new();
                let mut progress = Vec::new();
                for (lk, v) in e.videos.iter() {
                    let skey = storage_video_key(k, lk);
                    videos.push(sa_sync::BatchRow { key: Some(skey.clone()), encrypted_blob: Some(json::b64_encode(&enc_video(v, k))), updated_ticks: 0 });
                    if v.last_position_seconds > 0.0 || v.duration_seconds > 0.0 {
                        progress.push(sa_sync::BatchRow { key: Some(skey), encrypted_blob: Some(json::b64_encode(&enc_progress(v, k))), updated_ticks: 0 });
                    }
                }
                let mut r = self.request(WriteKind::ReplaceAllVideosAndProgress);
                r.username = storage_owner_tag(k);
                r.batch_videos = Some(videos);
                r.batch_progress = Some(progress);
                r.post_commit_events = Some(vec![self.profiles_event(EventKind::ListChanged)]);
                self.submit(r);
            }
            self.bump(&mut st);
        }
        self.notify(ProfileChange::Local);
        true
    }

    fn mutate_video(&self, path: &str, f: impl FnOnce(&mut VideoUserStats), with_watch_event: bool) -> Option<VideoUserStats> {
        let n = norm(path);
        if n.is_empty() {
            return None;
        }
        if self.locked_out() {
            return None;
        }
        self.drain();
        let updated;
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            let now = Ticks::now();
            let (e, key) = st.active()?;
            let (lk, existing) = self.get_or_create(e, key.as_ref(), &n, now, false);
            let mut u = existing;
            f(&mut u);
            u.updated_at = now;
            e.videos.insert(&lk, u.clone());
            let mut events = Vec::new();
            if with_watch_event {
                events.push(self.profiles_event(EventKind::VideoWatched));
            }
            events.push(self.profiles_event(EventKind::ListChanged));
            self.enqueue_video_and_progress(key.as_ref(), &lk, &u, false, None, Some(events));
            self.bump(&mut st);
            updated = u;
        }
        self.notify(ProfileChange::Local);
        Some(updated)
    }

    /// `ToggleLike` → the new state (false when logged out).
    pub fn toggle_like(&self, path: &str) -> bool {
        self.mutate_video(path, |v| v.is_liked = !v.is_liked, false).is_some_and(|v| v.is_liked)
    }

    /// `SetLiked`.
    pub fn set_liked_path(&self, path: &str, liked: bool) -> Option<VideoUserStats> {
        self.mutate_video(path, |v| v.is_liked = liked, true)
    }

    /// `SetRating` (clamped to 0..=5).
    pub fn set_rating_path(&self, path: &str, rating: i64) -> Option<VideoUserStats> {
        let clamped = rating.clamp(0, 5);
        self.mutate_video(path, |v| v.rating = clamped, true)
    }

    /// `GetStatsFastInMemory`: no drain, no sync, no disk access, no thumbnail.
    pub fn get_stats_fast_in_memory(&self, path: &str) -> Option<VideoUserStats> {
        let n = norm(path);
        if n.is_empty() {
            return None;
        }
        let st = self.state.lock();
        let name = st.active_profile.as_deref()?;
        let e = st.profiles.get(name)?;
        if let Some(v) = e.videos.get(&n) {
            return Some(v.clone());
        }
        if let Some(fp) = cached_fp(&n) {
            if let Some(v) = e.videos.get(&fp) {
                if eq_ci(&v.video_path, &n) {
                    return Some(v.clone());
                }
            }
        }
        e.videos.values().find(|v| eq_ci(&v.video_path, &n)).cloned()
    }

    /// `GetStats`.
    pub fn get_stats(&self, path: &str) -> Option<VideoUserStats> {
        let n = norm(path);
        if n.is_empty() {
            return None;
        }
        self.drain();
        let result;
        let migrated;
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            let (e, key) = st.active()?;
            let (found, k, s, m) = self.resolve_and_migrate(e, key.as_ref(), &n);
            let mut s = s.filter(|_| found)?;
            if let Some(t) = self.thumbnail_by_key(e, key.as_ref(), &k, &n) {
                s.thumbnail = Some(t);
            }
            migrated = m;
            if m {
                self.bump(&mut st);
            }
            result = s;
        }
        if migrated {
            self.notify(ProfileChange::Local);
        }
        Some(result)
    }

    /// `SetThumbnail(path, bytes, expectedSessionToken)`; never raises a change.
    pub fn set_thumbnail_with(&self, path: &str, bytes: &[u8], expected_token: Option<i64>) -> bool {
        let n = norm(path);
        if n.is_empty() || bytes.is_empty() {
            return false;
        }
        if self.locked_out() {
            return false;
        }
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        if st.active_profile.is_none() || !Self::expected_session(&st, expected_token) {
            return false;
        }
        let Some((e, key)) = st.active() else { return false };
        let (lk, _) = self.get_or_create(e, key.as_ref(), &n, Ticks::now(), true);
        if e.decoded_thumbnails.get(&lk).is_some_and(|b| b.as_slice() == bytes) {
            return true;
        }
        e.decoded_thumbnails.insert(&lk, bytes.to_vec());
        e.missing_thumbnails.remove(&lk);
        if let Some(k) = key.as_ref() {
            let evt = self.make_event(Topic::Thumbnail, EventKind::ThumbnailCaptured, Some(k), Some(&n), None, None, Some(&json::b64_encode(bytes)));
            let mut r = self.request(WriteKind::UpsertThumbnail);
            r.username = storage_owner_tag(k);
            r.key = storage_video_key(k, &lk);
            r.blob = Some(json::b64_encode(&crypto::seal(bytes, k)));
            r.post_commit_events = Some(vec![evt]);
            self.submit(r);
        }
        self.bump(&mut st);
        true
    }

    /// `GetThumbnail`.
    pub fn get_thumbnail(&self, path: &str) -> Option<Vec<u8>> {
        let n = norm(path);
        if n.is_empty() {
            return None;
        }
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        let (e, key) = st.active()?;
        self.thumbnail_for_path(e, key.as_ref(), &n)
    }

    /// `CreateCustomList` (`None` for blank, logged out or a duplicate name under CurrentCultureIgnoreCase).
    pub fn create_custom_list(&self, name: &str) -> Option<UserVideoList> {
        let clean = name.trim_matches(char::is_whitespace).to_owned();
        if clean.is_empty() {
            return None;
        }
        if self.locked_out() {
            return None;
        }
        self.drain();
        let created;
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            let (e, key) = st.active()?;
            if e.custom_lists.iter().any(|l| culture_eq_ci(&l.name, &clean)) {
                return None;
            }
            let now = Ticks::now();
            let list = UserVideoList { id: sa_core::text::guid_n(), name: clean, video_paths: Vec::new(), created_at: now, updated_at: now };
            e.custom_lists.push(list.clone());
            self.enqueue_list_header(key.as_ref(), &list, Some(vec![self.profiles_event(EventKind::ListChanged)]));
            self.bump(&mut st);
            created = list;
        }
        self.notify(ProfileChange::Local);
        Some(created)
    }

    /// `RenameCustomList`.
    pub fn rename_custom_list(&self, id: &str, new_name: &str) -> bool {
        let clean = new_name.trim_matches(char::is_whitespace).to_owned();
        if id.trim().is_empty() || clean.is_empty() {
            return false;
        }
        if self.locked_out() {
            return false;
        }
        self.drain();
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            let Some((e, key)) = st.active() else { return false };
            let Some(idx) = e.custom_lists.iter().position(|l| eq_ci(&l.id, id)) else { return false };
            if e.custom_lists.iter().enumerate().any(|(i, l)| i != idx && culture_eq_ci(&l.name, &clean)) {
                return false;
            }
            e.custom_lists[idx].name = clean;
            e.custom_lists[idx].updated_at = Ticks::now();
            let list = e.custom_lists[idx].clone();
            self.enqueue_list_header(key.as_ref(), &list, Some(vec![self.profiles_event(EventKind::ListChanged)]));
            self.bump(&mut st);
        }
        self.notify(ProfileChange::Local);
        true
    }

    /// `DeleteCustomList`.
    pub fn delete_custom_list(&self, id: &str) -> bool {
        if id.trim().is_empty() {
            return false;
        }
        if self.locked_out() {
            return false;
        }
        self.drain();
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            let Some((e, key)) = st.active() else { return false };
            let before = e.custom_lists.len();
            e.custom_lists.retain(|l| !eq_ci(&l.id, id));
            if e.custom_lists.len() == before {
                return false;
            }
            let owner = key.as_ref().map(storage_owner_tag).unwrap_or_else(|| e.username.clone());
            let mut r = self.request(WriteKind::DeleteCustomList);
            r.username = owner;
            r.key = id.to_owned();
            r.post_commit_events = Some(vec![self.profiles_event(EventKind::ListChanged)]);
            self.submit(r);
            self.bump(&mut st);
        }
        self.notify(ProfileChange::Local);
        true
    }

    /// `AddVideoToCustomList` (true also when already present).
    pub fn add_video_to_custom_list(&self, id: &str, path: &str) -> bool {
        let n = norm(path);
        if id.trim().is_empty() || n.is_empty() {
            return false;
        }
        if self.locked_out() {
            return false;
        }
        self.drain();
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            let Some((e, key)) = st.active() else { return false };
            if !e.custom_lists.iter().any(|l| eq_ci(&l.id, id)) {
                return false;
            }
            let _ = self.get_or_create(e, key.as_ref(), &n, Ticks::now(), false);
            let Some(idx) = e.custom_lists.iter().position(|l| eq_ci(&l.id, id)) else { return false };
            if e.custom_lists[idx].video_paths.iter().any(|p| eq_ci(p, &n)) {
                return true;
            }
            e.custom_lists[idx].video_paths.push(n.clone());
            e.custom_lists[idx].updated_at = Ticks::now();
            let list = e.custom_lists[idx].clone();
            let username = e.username.clone();
            self.enqueue_list_header(key.as_ref(), &list, None);
            self.enqueue_list_item_add(key.as_ref(), &username, &list.id, list.video_paths.len() as i32 - 1, &n, Some(vec![self.profiles_event(EventKind::ListChanged)]));
            self.bump(&mut st);
        }
        self.notify(ProfileChange::Local);
        true
    }

    /// `RemoveVideoFromCustomList`.
    pub fn remove_video_from_custom_list(&self, id: &str, path: &str) -> bool {
        let n = norm(path);
        if id.trim().is_empty() || n.is_empty() {
            return false;
        }
        if self.locked_out() {
            return false;
        }
        self.drain();
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            let Some((e, key)) = st.active() else { return false };
            let _ = self.resolve_and_migrate(e, key.as_ref(), &n);
            let Some(idx) = e.custom_lists.iter().position(|l| eq_ci(&l.id, id)) else { return false };
            let before = e.custom_lists[idx].video_paths.len();
            e.custom_lists[idx].video_paths.retain(|p| !eq_ci(p, &n));
            if e.custom_lists[idx].video_paths.len() == before {
                return false;
            }
            e.custom_lists[idx].updated_at = Ticks::now();
            let list = e.custom_lists[idx].clone();
            let username = e.username.clone();
            self.enqueue_list_header(key.as_ref(), &list, None);
            self.enqueue_list_item_remove(key.as_ref(), &username, &list.id, &n, Some(vec![self.profiles_event(EventKind::ListChanged)]));
            self.bump(&mut st);
        }
        self.notify(ProfileChange::Local);
        true
    }

    /// `ToggleVideoInCustomList` → true when now in the list.
    pub fn toggle_video_in_custom_list(&self, id: &str, path: &str) -> bool {
        let n = norm(path);
        if id.trim().is_empty() || n.is_empty() {
            return false;
        }
        if self.locked_out() {
            return false;
        }
        self.drain();
        let now_in;
        {
            let mut st = self.state.lock();
            self.ensure_synced_if_changed(&mut st);
            let Some((e, key)) = st.active() else { return false };
            let _ = self.get_or_create(e, key.as_ref(), &n, Ticks::now(), false);
            let Some(idx) = e.custom_lists.iter().position(|l| eq_ci(&l.id, id)) else { return false };
            let username = e.username.clone();
            let list = &mut e.custom_lists[idx];
            if let Some(m) = list.video_paths.iter().position(|p| eq_ci(p, &n)) {
                list.video_paths.remove(m);
                now_in = false;
            } else {
                list.video_paths.push(n.clone());
                now_in = true;
            }
            list.updated_at = Ticks::now();
            let list = list.clone();
            self.enqueue_list_header(key.as_ref(), &list, None);
            let events = Some(vec![self.profiles_event(EventKind::ListChanged)]);
            if now_in {
                self.enqueue_list_item_add(key.as_ref(), &username, &list.id, list.video_paths.len() as i32 - 1, &n, events);
            } else {
                self.enqueue_list_item_remove(key.as_ref(), &username, &list.id, &n, events);
            }
            self.bump(&mut st);
        }
        self.notify(ProfileChange::Local);
        now_in
    }

    /// `GetCustomLists`.
    pub fn get_custom_lists(&self) -> Vec<UserVideoList> {
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        st.active().map(|(e, _)| e.custom_lists.clone()).unwrap_or_default()
    }

    /// `GetCustomListsContaining`.
    pub fn get_custom_lists_containing(&self, path: &str) -> Vec<UserVideoList> {
        let n = norm(path);
        if n.is_empty() {
            return Vec::new();
        }
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        let Some((e, key)) = st.active() else { return Vec::new() };
        let (found, _, _, migrated) = self.resolve_and_migrate(e, key.as_ref(), &n);
        let lists: Vec<UserVideoList> = e.custom_lists.iter().filter(|l| l.video_paths.iter().any(|p| eq_ci(p, &n))).cloned().collect();
        if found && migrated {
            self.bump(&mut st);
        }
        lists
    }

    /// `IsVideoInCustomList`.
    pub fn is_video_in_custom_list(&self, id: &str, path: &str) -> bool {
        let n = norm(path);
        if id.trim().is_empty() || n.is_empty() {
            return false;
        }
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        let Some((e, key)) = st.active() else { return false };
        let (found, _, _, migrated) = self.resolve_and_migrate(e, key.as_ref(), &n);
        let result = e.custom_lists.iter().find(|l| eq_ci(&l.id, id)).is_some_and(|l| l.video_paths.iter().any(|p| eq_ci(p, &n)));
        if found && migrated {
            self.bump(&mut st);
        }
        result
    }

    /// `GetFeed`. Logged out → `[]` for every filter (rule 6, Q7).
    pub fn get_feed(&self, filter: &VideoFeedFilter, search: Option<&str>, discovered: Option<&[String]>, should_continue: &dyn Fn() -> bool) -> Vec<VideoUserStats> {
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        let now = Ticks::now();
        let mut roots: HashMap<String, bool> = HashMap::new();
        if st.active_profile.is_none() {
            return Vec::new();
        }
        let mut any_migrated = false;
        let mut merged: CiMap<VideoUserStats> = CiMap::new();
        let mut disc: Vec<String> = Vec::new();
        {
            let Some((e, key)) = st.active() else { return Vec::new() };
            let mut known = CiSet::new();
            let mut trackable = false;
            for v in e.videos.values() {
                if !v.video_path.is_empty() {
                    known.insert(&v.video_path);
                    if v.content_fingerprint.as_deref().is_some_and(|f| !f.is_empty()) {
                        trackable = true;
                    }
                }
            }
            for raw in discovered.into_iter().flatten() {
                if !should_continue() {
                    return Vec::new();
                }
                let n = norm(raw);
                if n.is_empty() || !self.storage_available(&n, &mut roots) {
                    continue;
                }
                disc.push(n.clone());
                if trackable && !known.contains(&n) {
                    let (found, _, _, migrated) = self.resolve_and_migrate(e, key.as_ref(), &n);
                    if found && migrated {
                        any_migrated = true;
                        known.insert(&n);
                    }
                }
            }
            let entries: Vec<(String, VideoUserStats)> = e.videos.iter().map(|(k, v)| (k.to_owned(), v.clone())).collect();
            for (k, mut v) in entries {
                if !v.video_path.is_empty() && self.storage_available(&v.video_path, &mut roots) {
                    if let Some(t) = self.thumbnail_by_key(e, key.as_ref(), &k, &v.video_path) {
                        v.thumbnail = Some(t);
                    }
                    let p = v.video_path.clone();
                    merged.insert(&p, v);
                }
            }
            for n in &disc {
                if !merged.contains_key(n) {
                    merged.insert(n, create_default(n, now, None, false));
                }
            }
            for l in &e.custom_lists {
                for raw in &l.video_paths {
                    let n = norm(raw);
                    if !n.is_empty() && !merged.contains_key(&n) && self.storage_available(&n, &mut roots) {
                        merged.insert(&n, create_default(&n, now, None, false));
                    }
                }
            }
        }
        if any_migrated {
            self.bump(&mut st);
        }
        let Some((e, _)) = st.active() else { return Vec::new() };
        let mut items: Vec<VideoUserStats> = if let VideoFeedFilter::CustomList(list_id) = filter {
            let target = match list_id.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(id) => e.custom_lists.iter().find(|l| eq_ci(&l.id, id)),
                None => e.custom_lists.first(),
            };
            let Some(target) = target else { return Vec::new() };
            let mut out = Vec::new();
            for raw in &target.video_paths {
                let n = norm(raw);
                if n.is_empty() || !self.storage_available(&n, &mut roots) {
                    continue;
                }
                out.push(merged.get(&n).cloned().unwrap_or_else(|| create_default(&n, now, None, false)));
            }
            out
        } else {
            let mut q: Vec<VideoUserStats> = merged.values().cloned().collect();
            let last_or_updated = |v: &VideoUserStats| v.last_watched_at.unwrap_or(v.updated_at);
            match filter {
                VideoFeedFilter::History => {
                    q.retain(|v| v.view_count > 0 || v.last_watched_at.is_some());
                    q.sort_by(|a, b| {
                        last_or_updated(b)
                            .cmp(&last_or_updated(a))
                            .then(b.view_count.cmp(&a.view_count))
                            .then_with(|| culture_cmp_ci(&a.file_name, &b.file_name))
                    });
                }
                VideoFeedFilter::Liked => {
                    q.retain(|v| v.is_liked);
                    q.sort_by(|a, b| {
                        b.updated_at.cmp(&a.updated_at).then(b.view_count.cmp(&a.view_count)).then_with(|| culture_cmp_ci(&a.file_name, &b.file_name))
                    });
                }
                VideoFeedFilter::MostWatched => {
                    q.retain(|v| v.view_count > 0);
                    q.sort_by(|a, b| {
                        b.view_count
                            .cmp(&a.view_count)
                            .then(last_or_updated(b).cmp(&last_or_updated(a)))
                            .then_with(|| culture_cmp_ci(&a.file_name, &b.file_name))
                    });
                }
                VideoFeedFilter::TopRated => {
                    q.retain(|v| v.rating > 0);
                    q.sort_by(|a, b| {
                        b.rating
                            .cmp(&a.rating)
                            .then(b.view_count.cmp(&a.view_count))
                            .then(b.updated_at.cmp(&a.updated_at))
                            .then_with(|| culture_cmp_ci(&a.file_name, &b.file_name))
                    });
                }
                _ => {
                    let engaged = |v: &VideoUserStats| v.last_watched_at.is_some() || v.is_liked || v.rating > 0;
                    q.sort_by(|a, b| {
                        engaged(b)
                            .cmp(&engaged(a))
                            .then(b.last_watched_at.unwrap_or(Ticks::MIN).cmp(&a.last_watched_at.unwrap_or(Ticks::MIN)))
                            .then(b.view_count.cmp(&a.view_count))
                            .then_with(|| culture_cmp_ci(&a.file_name, &b.file_name))
                    });
                }
            }
            q
        };
        if let Some(s) = search.filter(|s| !s.trim().is_empty()) {
            items.retain(|v| matches_search_query(v, s));
        }
        items
    }

    /// `GetSqliteTableRowCounts` (all zero logged out).
    pub fn get_sqlite_table_row_counts(&self) -> TableRowCounts {
        let owner = {
            let st = self.state.lock();
            st.active_key().map(|k| storage_owner_tag(&k))
        };
        let Some(owner) = owner else { return TableRowCounts::default() };
        let (videos, watch_progress, custom_lists, custom_list_items, thumbnails) = self.sql_store().get_table_row_counts(&owner);
        TableRowCounts { videos, watch_progress, custom_lists, custom_list_items, thumbnails }
    }

    /// `TryGetActiveProfileVaultKey`.
    pub fn try_get_active_profile_vault_key(&self) -> Option<(String, SecretKey32)> {
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        let name = st.active_profile.clone()?;
        let e = st.profiles.get(&name)?;
        let key = st.session_keys.get(&e.username)?.clone();
        Some((e.username.clone(), key))
    }

    /// Profiles whose vault key is held (privacy tests).
    #[doc(hidden)]
    pub fn unlocked_profile_count_for_tests(&self) -> usize {
        self.state.lock().session_keys.len()
    }

    /// Decrypted videos, lists and thumbnails held across all profiles (privacy tests).
    #[doc(hidden)]
    pub fn decrypted_item_count_for_tests(&self) -> usize {
        self.state.lock().profiles.values().map(|e| e.videos.len() + e.custom_lists.len() + e.decoded_thumbnails.len()).sum()
    }

    /// Unsubscribes from the broker (C# `Dispose`); the store belongs to the broker.
    pub fn dispose(&self) {
        self.subscription.lock().take();
    }
}

/// The session token a `session_generation()` value stands for (`token.max(0) as u64`; the token starts at 0 and
/// only grows, so the mapping is exact; an out-of-range value matches no session).
fn token_of(generation: u64) -> i64 {
    i64::try_from(generation).unwrap_or(-1)
}

fn parse_ts(text: &str) -> Ticks {
    json::parse_date(text).unwrap_or_else(Ticks::now)
}

fn sha2_256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let d = sha2::Sha256::digest(data);
    let mut a = [0u8; 32];
    a.copy_from_slice(&d);
    a
}

impl ProfileStore for WinProfileStore {
    /// UI-safe: memory only (no drain, no file read) and lock-free (a worker may hold the state mutex across a disk
    /// sync). Logins, logouts and restores elsewhere reach this store through its broker subscription, which resyncs
    /// at once.
    fn is_logged_in(&self) -> bool {
        self.state.logged_in()
    }
    /// UI-safe (memory only), registered casing.
    fn current_username(&self) -> Option<String> {
        let st = self.state.lock();
        let name = st.active_profile.as_deref()?;
        st.profiles.get(name).map(|e| e.username.clone())
    }
    fn registered_usernames(&self) -> Vec<String> {
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        let mut names: Vec<String> = st.profiles.values().map(|e| e.username.clone()).collect();
        names.sort_by(|a, b| culture_cmp_ci(a, b));
        names
    }
    fn last_username(&self) -> Option<String> {
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        st.last_username.clone()
    }
    fn remember_me(&self) -> bool {
        self.state.lock().remember_me
    }
    /// UI-safe (memory only, like `is_logged_in`): the hub compares it on the UI thread before applying a snapshot,
    /// so it must not drain the write queue or reread `profiles.json` there. Every session change in this process
    /// (local calls, broker-driven resyncs) bumps the in-memory token at once; [`WinProfileStore::session_token`]
    /// is the C# `SessionToken` with its drain and resync.
    ///
    /// Lock-free: the value as of the last state unlock. The hub reads it on the UI thread at every playback report,
    /// and a worker may hold the state mutex across a disk sync (or a whole login switch) for tens of milliseconds;
    /// a change still being made under the lock shows up once it is done. A captured generation can therefore be
    /// stale by the time a worker writes: writes gated on one go through the `*_in_session` methods, which compare
    /// it with the token under the lock (`expected_session`), in the same critical section as the write.
    fn session_generation(&self) -> u64 {
        self.state.session_token().max(0) as u64
    }
    /// Lock-free: raised by `register_user`, `login_user`, `logout_user`, `delete_user` and the resync after another
    /// instance's logout / delete, from before they take the state lock until their token is published. A failed
    /// operation (wrong password, taken name) raises it too: a stream then continues uncached from its source.
    fn session_change_in_progress(&self) -> bool {
        self.state.session_change_in_progress()
    }
    fn start(&self) {
        let was_logged_in = self.state.lock().active_profile.is_some();
        self.load_from_disk();
        let restored = {
            let mut st = self.state.lock();
            if let Some((e, key)) = st.active() {
                if self.migrate_path_keyed_videos(e, key.as_ref()) {
                    self.bump(&mut st);
                }
            }
            !was_logged_in && st.active_profile.is_some()
        };
        if restored {
            self.notify(ProfileChange::External);
        }
    }
    fn register(&self, username: &str, password: &SecretString, remember: bool) -> Result<(), ProfileError> {
        self.register_user(username, password.expose(), remember)
    }
    fn login(&self, username: &str, password: &SecretString, remember: bool) -> Result<(), ProfileError> {
        self.login_user(username, password.expose(), remember)
    }
    fn logout(&self) -> Result<(), ProfileError> {
        self.logout_user();
        Ok(())
    }
    fn delete_profile(&self, username: &str, password: &SecretString) -> Result<(), ProfileError> {
        self.delete_user(username, password.expose())
    }
    fn record_watch(&self, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        self.record_watch_with(&path.to_string_lossy(), position, duration, None, None)
    }
    fn update_playback_progress(&self, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        self.update_playback_progress_with(&path.to_string_lossy(), position, duration, None)
    }
    fn update_playback_progress_in_session(&self, generation: u64, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        self.update_playback_progress_with(&path.to_string_lossy(), position, duration, Some(token_of(generation)))
    }
    fn record_watch_in_session(&self, generation: u64, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        self.record_watch_with(&path.to_string_lossy(), position, duration, None, Some(token_of(generation)))
    }
    fn set_thumbnail_in_session(&self, generation: u64, path: &Path, bytes: &[u8]) -> bool {
        self.set_thumbnail_with(&path.to_string_lossy(), bytes, Some(token_of(generation)))
    }
    fn flush_playback_progress(&self) -> bool {
        self.flush_pending_writes(Duration::from_secs(5))
    }
    fn set_liked(&self, path: &Path, liked: bool) -> Option<VideoUserStats> {
        self.set_liked_path(&path.to_string_lossy(), liked)
    }
    fn set_rating(&self, path: &Path, stars: u8) -> Option<VideoUserStats> {
        self.set_rating_path(&path.to_string_lossy(), i64::from(stars))
    }
    fn stats(&self, path: &Path, mode: FingerprintMode) -> Option<VideoUserStats> {
        match mode {
            FingerprintMode::Compute => self.get_stats(&path.to_string_lossy()),
            FingerprintMode::CachedOnly | FingerprintMode::PathOnly => self.get_stats_fast_in_memory(&path.to_string_lossy()),
        }
    }
    fn stats_fast_in_memory(&self, path: &Path) -> Option<VideoUserStats> {
        self.get_stats_fast_in_memory(&path.to_string_lossy())
    }
    fn clear_watch_history(&self) -> bool {
        self.clear_history()
    }
    fn thumbnail(&self, path: &Path) -> Option<Vec<u8>> {
        self.get_thumbnail(&path.to_string_lossy())
    }
    fn set_thumbnail(&self, path: &Path, bytes: &[u8]) -> bool {
        self.set_thumbnail_with(&path.to_string_lossy(), bytes, None)
    }
    fn lists(&self) -> Vec<UserVideoList> {
        self.get_custom_lists()
    }
    fn create_list(&self, name: &str) -> Result<Option<UserVideoList>, ProfileError> {
        Ok(self.create_custom_list(name))
    }
    fn rename_list(&self, id: &str, name: &str) -> Result<bool, ProfileError> {
        Ok(self.rename_custom_list(id, name))
    }
    fn delete_list(&self, id: &str) -> bool {
        self.delete_custom_list(id)
    }
    fn set_in_list(&self, id: &str, path: &Path, is_in: bool) -> bool {
        let p = path.to_string_lossy();
        if is_in { self.add_video_to_custom_list(id, &p) } else { self.remove_video_from_custom_list(id, &p) }
    }
    fn toggle_in_list(&self, id: &str, path: &Path) -> bool {
        self.toggle_video_in_custom_list(id, &path.to_string_lossy())
    }
    /// Ids of the lists containing `path`, in list order (the hub matches them against `lists()`; same as macOS).
    fn lists_containing(&self, path: &Path) -> Vec<String> {
        self.get_custom_lists_containing(&path.to_string_lossy()).into_iter().map(|l| l.id).collect()
    }
    fn feed(&self, query: &FeedQuery, should_continue: &dyn Fn() -> bool) -> Vec<VideoUserStats> {
        let discovered: Vec<String> = query.discovered.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        self.get_feed(&query.filter, query.search.as_deref(), Some(&discovered), should_continue)
    }
    fn totals(&self) -> ProfileCounts {
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        let Some((e, _)) = st.active() else { return ProfileCounts::default() };
        ProfileCounts {
            watched: e.videos.values().filter(|v| v.view_count > 0).count() as i64,
            views: e.videos.values().map(|v| v.view_count).sum(),
            liked: e.videos.values().filter(|v| v.is_liked).count() as i64,
            rated: e.videos.values().filter(|v| v.rating > 0).count() as i64,
        }
    }
    fn persist_pending_migrations(&self) -> bool {
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        let Some((e, key)) = st.active() else { return false };
        let changed = self.migrate_path_keyed_videos(e, key.as_ref());
        if changed {
            self.bump(&mut st);
        }
        changed
    }
    fn subscribe_changes(&self, handler: Box<dyn Fn(ProfileChange) + Send + Sync>) -> Subscription {
        self.changes.subscribe(move |c: &ProfileChange| handler(*c))
    }
    fn active_vault_key(&self) -> Option<(String, SecretKey32)> {
        self.try_get_active_profile_vault_key()
    }
    /// The key and the session token from one critical section (the token is published at this unlock, so it is
    /// exactly what `session_generation()` reports until the next session change).
    fn active_vault_key_in_session(&self) -> Option<(u64, String, SecretKey32)> {
        self.drain();
        let mut st = self.state.lock();
        self.ensure_synced_if_changed(&mut st);
        let name = st.active_profile.clone()?;
        let e = st.profiles.get(&name)?;
        let key = st.session_keys.get(&e.username)?.clone();
        Some((st.session_token.max(0) as u64, e.username.clone(), key))
    }
}

impl Drop for WinProfileStore {
    fn drop(&mut self) {
        self.subscription.get_mut().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The UI thread reads `is_logged_in` and `session_generation` while a video plays; a worker can hold the state
    /// mutex across a disk sync, so neither may wait for it, and both must still follow every session change.
    #[test]
    fn session_reads_never_wait_for_the_state_mutex() {
        let dir = tempfile::tempdir().unwrap();
        let store = WinProfileStore::open(dir.path().join("profiles.json"), sa_sync::InProcessBus::with_scope(&sa_core::text::guid_n()));
        let logged_out = store.session_generation();
        assert!(!store.is_logged_in());
        store.register_user("ali", "pw-1234", false).unwrap();
        assert!(store.is_logged_in());
        let generation = store.session_generation();
        assert_ne!(generation, logged_out);
        assert_eq!(generation, store.session_token().max(0) as u64, "the copy equals the locked value");

        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        std::thread::scope(|s| {
            let holder = &store;
            s.spawn(move || {
                // Stands in for a worker inside `ensure_synced_if_changed` (file reads, retry sleeps).
                let _st = holder.state.state.lock();
                held_tx.send(()).unwrap();
                // Released on request, or after 2 s so a blocking reader fails instead of hanging.
                let _ = release_rx.recv_timeout(Duration::from_secs(2));
            });
            held_rx.recv().unwrap();
            let t = std::time::Instant::now();
            let (logged_in, read) = (store.is_logged_in(), store.session_generation());
            let elapsed = t.elapsed();
            let _ = release_tx.send(());
            assert!(logged_in);
            assert_eq!(read, generation);
            assert!(elapsed < Duration::from_millis(500), "took {elapsed:?} with the state mutex held elsewhere");
        });

        store.logout_user();
        assert!(!store.is_logged_in());
        assert_ne!(store.session_generation(), generation);
    }

    /// A playback stream captures the key with its generation once and then only compares the lock-free generation:
    /// the pair must match `session_generation()` exactly, and every logout / switch must change it.
    #[test]
    fn vault_key_in_session_pairs_the_key_with_the_lock_free_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = WinProfileStore::open(dir.path().join("profiles.json"), sa_sync::InProcessBus::with_scope(&sa_core::text::guid_n()));
        assert!(store.active_vault_key_in_session().is_none(), "logged out: no key");
        store.register_user("ali", "pw-1234", false).unwrap();
        let (generation, user, key) = store.active_vault_key_in_session().expect("logged in");
        assert_eq!(user, "ali");
        assert_eq!(generation, store.session_generation());
        let (_, plain) = store.active_vault_key().unwrap();
        assert_eq!(key.expose(), plain.expose(), "the same key as active_vault_key");
        // Reading the key is no session change.
        assert_eq!(store.session_generation(), generation);

        store.logout_user();
        assert!(store.active_vault_key_in_session().is_none());
        assert_ne!(store.session_generation(), generation, "a stream opened under ali sees the logout lock-free");

        store.register_user("veli", "pw-5678", false).unwrap();
        let (veli, user, _) = store.active_vault_key_in_session().unwrap();
        assert_eq!(user, "veli");
        assert_ne!(veli, generation);
        store.login_user("ali", "pw-1234", false).unwrap();
        let (again, _, again_key) = store.active_vault_key_in_session().unwrap();
        assert!(again != generation && again != veli, "a new login is a new session, also for the same user");
        assert_eq!(again_key.expose(), key.expose());
        assert_eq!(again, store.session_generation());
    }

    /// The cache's contract on `session_generation`: whenever `active_vault_key()` changes (user or key), the token
    /// changed too, across every session-changing operation.
    #[test]
    fn the_token_changes_whenever_the_active_vault_key_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = WinProfileStore::open(dir.path().join("profiles.json"), sa_sync::InProcessBus::with_scope(&sa_core::text::guid_n()));
        let snapshot = |s: &WinProfileStore| (s.session_generation(), s.active_vault_key().map(|(u, k)| (u, *k.expose())));
        let mut last = snapshot(&store);
        let mut check = |what: &str, s: &WinProfileStore| {
            let now = snapshot(s);
            if now.1 != last.1 {
                assert_ne!(now.0, last.0, "{what}: the key changed but the token did not");
            }
            last = now;
        };
        store.register_user("ali", "pw-1", false).unwrap();
        check("register", &store);
        store.register_user("veli", "pw-2", false).unwrap();
        check("register another (switch)", &store);
        store.login_user("ali", "pw-1", false).unwrap();
        check("login switch", &store);
        assert!(store.login_user("veli", "wrong", false).is_err());
        check("failed login", &store);
        store.logout_user();
        check("logout", &store);
        store.login_user("veli", "pw-2", true).unwrap();
        check("login", &store);
        store.delete_user("veli", "pw-2").unwrap();
        check("delete the active profile", &store);
        store.login_user("ali", "pw-1", false).unwrap();
        check("login after delete", &store);
        // Another in-process store of the same file logs out: this one follows at its next locked read.
        let other = WinProfileStore::open(dir.path().join("profiles.json"), sa_sync::InProcessBus::with_scope(&sa_core::text::guid_n()));
        other.logout_user();
        let _ = store.active_vault_key();
        check("logout elsewhere", &store);
    }

    /// While a logout waits inside its critical section, the lock-free signal is up (so a stream stops using its
    /// captured key before the new token is published); afterwards it is down and the token is new.
    #[test]
    fn a_session_change_in_progress_is_visible_lock_free() {
        let dir = tempfile::tempdir().unwrap();
        let store = WinProfileStore::open(dir.path().join("profiles.json"), sa_sync::InProcessBus::with_scope(&sa_core::text::guid_n()));
        store.register_user("ali", "pw-1234", false).unwrap();
        let generation = store.session_generation();
        assert_eq!(store.settled_session_generation(), Some(generation));
        std::thread::scope(|s| {
            let held = store.state.state.lock(); // a worker inside a disk sync
            let logout = s.spawn(|| store.logout_user());
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !store.session_change_in_progress() && std::time::Instant::now() < deadline {
                std::thread::yield_now();
            }
            assert!(store.session_change_in_progress(), "raised before the logout got the lock");
            assert_eq!(store.settled_session_generation(), None);
            assert_eq!(store.session_generation(), generation, "nothing published yet");
            drop(held);
            logout.join().unwrap();
        });
        assert!(!store.session_change_in_progress());
        let settled = store.settled_session_generation().unwrap();
        assert_ne!(settled, generation);
        // A failed login raises it only while it runs, and changes nothing.
        assert!(store.login_user("ali", "wrong", false).is_err());
        assert_eq!(store.settled_session_generation(), Some(settled));
    }

    #[test]
    fn padding_buckets() {
        assert_eq!(pad_to_bucket(vec![1; 10], 256, b' ').len(), 256);
        assert_eq!(pad_to_bucket(vec![1; 300], 256, b' ').len(), 512);
        assert_eq!(pad_to_bucket(vec![1; 1024], 1024, b' ').len(), 1024);
        assert_eq!(pad_to_bucket(vec![1; 1025], 1024, b' ').len(), 2048);
    }

    /// Golden vectors computed by the C# primitives (the same values as `tests/fixtures/csharp/vectors.json`, which
    /// `tests/win_fixtures.rs` checks in full).
    pub(crate) mod golden {
        pub const PASSWORD: &str = "pässwörd-1 İı";
        pub const SALT_HEX: &str = "031425364758697a8b9cadbecfe0f102";
        pub const HASH_B64: &str = "fa3eK5WLQiwCLn/Hi7YjBWYKD1ETMo2rXEotXCfp/04=";
        pub const KEY_HEX: &str = "7aede8c97bbb25af7103589bbf7a9353c85579e1dd600a88739e0e3de4ee5d68";
        pub const TURKISH_PATH: &str = r"C:\Videolar\İzmir ılık Ödev ß.mp4";
        pub const STORAGE_VIDEO_KEY: &str = "hk:b588df6998e8de8cb65daa883ea18f8acd05a252";
        pub const STORAGE_VIDEO_KEY_FP: &str = "hk:d0dd3b781f82da4cb7bed6d70d0425da3a032c39";
        pub const OWNER_TAG: &str = "ot:de4d730198d70efbb58bea484d55ffc1d312f213";
        pub const LIST_ITEM_HASH: &str = "711a302a9c4e7c0ac89963aacc91783c";
        pub const UPPER_INPUT: &str = "ıiİß ǅ ﬃ ΐ ſ µ ẞ ǰ";
        pub const UPPER_INVARIANT: &str = "ıIİß Ǆ ﬃ ΐ S Μ ẞ ǰ";
        pub const CSHARP_MARKER_BLOB_B64: &str = "vnLzWeVA/8u6BIxm1isvT56d97Kifn1BNJ1ac5iedIxUjeMVyB32J2Xqf97zCmNfdyTe";
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn golden_vectors_from_the_csharp_primitives() {
        use golden::*;
        let salt = unhex(SALT_HEX);
        let (hash, key) = crypto::derive(PASSWORD, &salt, 100_000);
        assert_eq!(json::b64_encode(&hash), HASH_B64);
        assert_eq!(crypto::lower_hex(key.expose()), KEY_HEX);
        assert!(crypto::verify(PASSWORD, &salt, &hash, 100_000).is_some());
        assert_eq!(storage_video_key(&key, &format!("  {TURKISH_PATH} ")), STORAGE_VIDEO_KEY);
        assert_eq!(storage_video_key(&key, "fp:5000:3ab99b16d24511552b0d2f52"), STORAGE_VIDEO_KEY_FP);
        assert_eq!(storage_owner_tag(&key), OWNER_TAG);
        assert_eq!(list_item_hash("Emre", "0123456789ABCDEFabcdef0123456789", TURKISH_PATH, &key), LIST_ITEM_HASH);
        assert_eq!(upper_invariant(UPPER_INPUT), UPPER_INVARIANT);
        let blob = json::b64_decode(CSHARP_MARKER_BLOB_B64).unwrap();
        assert_eq!(blob.len(), 28 + 23);
        assert_eq!(crypto::open(&blob, &key).as_deref(), Some(crypto::VAULT_MARKER.as_bytes()));
        assert_eq!(vault_marker(&key).len(), 68);
        assert_eq!(pad_to_bucket(vec![0; 300], 256, b' ').len(), 512);
        assert_eq!(pad_to_bucket(vec![0; 1025], 1024, b' ').len(), 2048);
    }

    #[test]
    fn default_path_is_the_known_local_app_data_folder() {
        // Only the path is computed; nothing is read or written there.
        let p = default_profiles_path();
        assert!(p.ends_with(Path::new("SubtitleAdder").join("profiles.json")));
        if let Some(env) = std::env::var_os("LOCALAPPDATA") {
            assert!(eq_ci(&p.parent().and_then(Path::parent).unwrap().to_string_lossy(), &env.to_string_lossy()));
        }
    }

    #[test]
    fn storage_keys_have_prefixes() {
        let k = SecretKey32::new([9u8; 32]);
        let a = storage_video_key(&k, " c:\\a.mp4 ");
        assert_eq!(a, storage_video_key(&k, "C:\\A.MP4"));
        assert!(a.starts_with("hk:") && a.len() == 43);
        assert!(storage_owner_tag(&k).starts_with("ot:"));
    }
}
