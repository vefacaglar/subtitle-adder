//! `SqliteProfileStore`: the broker engine's data worker for one `profiles.db` (C# `SqliteProfileStore.cs`), with the
//! same schema, SQL text, write queue, coalescing, idempotency (`processed_requests`), retry loop and the one-time
//! privacy scrub + VACUUM of plaintext history columns. Opened with the bundled SQLite (same file and WAL format as
//! the OS `winsqlite3.dll` the C# app uses).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Condvar, Mutex, ReentrantMutex, RwLock};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags, params};
use sa_sync::{DataWorkerStore, StateSyncEvent, StateSyncWriteAck, StateSyncWriteRequest};

use super::json::{b64_decode, format_round_trip, parse_date};
use super::netcompat::{eq_ci, upper_invariant};
use crate::Ticks;

/// Prefix of every `video_key` written by the current schema (keyed hash of the logical key).
pub const HASHED_VIDEO_KEY_PREFIX: &str = "hk:";
/// Prefix of opaque random keys that replaced legacy plaintext keys before the profile logged in again.
pub const OPAQUE_LEGACY_VIDEO_KEY_PREFIX: &str = "rk:";
/// Prefix of the owner tag stored in the data tables' `username` column.
pub const OWNER_TAG_PREFIX: &str = "ot:";

const PRIVACY_SCHEMA_META_KEY: &str = "privacy_schema";
const PRIVACY_SCHEMA_VERSION: &str = "1";
const PRIVACY_VACUUM_META_KEY: &str = "privacy_vacuum";
const PRIVACY_VACUUM_VERSION: &str = "1";
const MAX_PROCESSED_REQUEST_ROWS: usize = 4096;
const MAX_RETRY_ATTEMPTS: u32 = 60;
const OWNED_DATA_TABLES: [&str; 5] = ["thumbnails", "custom_list_items", "custom_lists", "watch_progress", "videos"];

/// C# `SqliteProfileStore.WriteKind` (names travel on the broker wire; ordinals are the C# enum values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteKind {
    UpsertProfile,
    DeleteProfileCascade,
    UpsertVideo,
    DeleteVideo,
    ReplaceAllVideosAndProgress,
    UpsertWatchProgress,
    UpsertCustomList,
    DeleteCustomList,
    UpsertCustomListItem,
    DeleteCustomListItem,
    ReplaceCustomListItems,
    UpsertThumbnail,
    MigrateVideoKey,
    PurgeLegacyVideoKeys,
    RetagOwner,
    FlushBarrier,
}

impl WriteKind {
    pub const ALL: [WriteKind; 16] = [
        WriteKind::UpsertProfile,
        WriteKind::DeleteProfileCascade,
        WriteKind::UpsertVideo,
        WriteKind::DeleteVideo,
        WriteKind::ReplaceAllVideosAndProgress,
        WriteKind::UpsertWatchProgress,
        WriteKind::UpsertCustomList,
        WriteKind::DeleteCustomList,
        WriteKind::UpsertCustomListItem,
        WriteKind::DeleteCustomListItem,
        WriteKind::ReplaceCustomListItems,
        WriteKind::UpsertThumbnail,
        WriteKind::MigrateVideoKey,
        WriteKind::PurgeLegacyVideoKeys,
        WriteKind::RetagOwner,
        WriteKind::FlushBarrier,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            WriteKind::UpsertProfile => "UpsertProfile",
            WriteKind::DeleteProfileCascade => "DeleteProfileCascade",
            WriteKind::UpsertVideo => "UpsertVideo",
            WriteKind::DeleteVideo => "DeleteVideo",
            WriteKind::ReplaceAllVideosAndProgress => "ReplaceAllVideosAndProgress",
            WriteKind::UpsertWatchProgress => "UpsertWatchProgress",
            WriteKind::UpsertCustomList => "UpsertCustomList",
            WriteKind::DeleteCustomList => "DeleteCustomList",
            WriteKind::UpsertCustomListItem => "UpsertCustomListItem",
            WriteKind::DeleteCustomListItem => "DeleteCustomListItem",
            WriteKind::ReplaceCustomListItems => "ReplaceCustomListItems",
            WriteKind::UpsertThumbnail => "UpsertThumbnail",
            WriteKind::MigrateVideoKey => "MigrateVideoKey",
            WriteKind::PurgeLegacyVideoKeys => "PurgeLegacyVideoKeys",
            WriteKind::RetagOwner => "RetagOwner",
            WriteKind::FlushBarrier => "FlushBarrier",
        }
    }

    /// `Enum.TryParse(kind, ignoreCase: true)` restricted to defined members; `FlushBarrier` is never accepted from
    /// a request.
    pub fn parse_request(kind: &str) -> Option<WriteKind> {
        let k = kind.trim();
        let parsed = if let Ok(n) = k.parse::<i64>() {
            usize::try_from(n).ok().and_then(|i| Self::ALL.get(i).copied())
        } else {
            Self::ALL.iter().copied().find(|w| w.name().eq_ignore_ascii_case(k))
        };
        parsed.filter(|w| *w != WriteKind::FlushBarrier)
    }
}

/// `(key, encrypted_blob, updated_ticks)`.
pub type BlobRow = (String, Vec<u8>, i64);

/// `SqliteProfileStore.ListItemRow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItemRow {
    pub list_id: String,
    pub item_hash: String,
    pub ord: i32,
    pub encrypted_path_blob: Vec<u8>,
}

/// Completion callback `(committed, is_duplicate, error)`.
pub type OnCompleted = Box<dyn FnOnce(bool, bool, Option<String>) + Send>;

/// `SqliteProfileStore.WriteItem`.
pub struct WriteItem {
    pub kind: WriteKind,
    pub username: String,
    pub key: String,
    pub secondary_key: String,
    pub ord: i32,
    pub blob: Option<Vec<u8>>,
    pub secondary_blob: Option<Vec<u8>>,
    pub updated_ticks: i64,
    pub view_count: i32,
    pub view_count_delta: i32,
    pub request_id: String,
    pub salt_base64: Option<String>,
    pub password_hash_base64: Option<String>,
    pub iterations: i32,
    pub created_at_utc: Ticks,
    pub batch_videos: Option<Vec<BlobRow>>,
    pub batch_progress: Option<Vec<BlobRow>>,
    pub batch_list_items: Option<Vec<ListItemRow>>,
    pub post_commit_events: Option<Vec<StateSyncEvent>>,
    pub on_completed: Option<OnCompleted>,
    barrier: Option<Sender<bool>>,
}

impl WriteItem {
    /// An item with the C# record defaults (`view_count` -1).
    pub fn new(kind: WriteKind) -> Self {
        Self {
            kind,
            username: String::new(),
            key: String::new(),
            secondary_key: String::new(),
            ord: 0,
            blob: None,
            secondary_blob: None,
            updated_ticks: 0,
            view_count: -1,
            view_count_delta: 0,
            request_id: String::new(),
            salt_base64: None,
            password_hash_base64: None,
            iterations: 0,
            created_at_utc: Ticks(0),
            batch_videos: None,
            batch_progress: None,
            batch_list_items: None,
            post_commit_events: None,
            on_completed: None,
            barrier: None,
        }
    }

    pub fn with_user_key(mut self, username: &str, key: &str) -> Self {
        self.username = username.to_owned();
        self.key = key.to_owned();
        self
    }

    pub fn with_blob(mut self, blob: Vec<u8>) -> Self {
        self.blob = Some(blob);
        self
    }

    pub fn with_request_id(mut self, id: &str) -> Self {
        self.request_id = id.to_owned();
        self
    }

    /// Converts a broker request (base64 blobs); `Err` = negative-ack text.
    pub fn from_request(req: StateSyncWriteRequest) -> Result<Self, String> {
        let Some(kind) = WriteKind::parse_request(&req.kind) else {
            return Err(format!("Unknown WriteKind '{}'.", req.kind));
        };
        let dec = |b: Option<String>| -> Result<Option<Vec<u8>>, String> {
            match b {
                None => Ok(None),
                Some(s) => b64_decode(&s).map(Some).ok_or_else(|| "Invalid base64 payload.".to_owned()),
            }
        };
        let rows = |v: Option<Vec<sa_sync::BatchRow>>| -> Result<Option<Vec<BlobRow>>, String> {
            match v {
                Some(list) if !list.is_empty() => {
                    let mut out = Vec::with_capacity(list.len());
                    for r in list {
                        let blob = dec(r.encrypted_blob)?.unwrap_or_default();
                        out.push((r.key.unwrap_or_default(), blob, r.updated_ticks));
                    }
                    Ok(Some(out))
                }
                _ => Ok(None),
            }
        };
        let items = match req.batch_list_items {
            Some(list) if !list.is_empty() => {
                let mut out = Vec::with_capacity(list.len());
                for li in list {
                    out.push(ListItemRow {
                        list_id: li.list_id.unwrap_or_default(),
                        item_hash: li.item_hash.unwrap_or_default(),
                        ord: li.ord,
                        encrypted_path_blob: dec(li.encrypted_path_blob)?.unwrap_or_default(),
                    });
                }
                Some(out)
            }
            _ => None,
        };
        Ok(Self {
            kind,
            username: req.username,
            key: req.key,
            secondary_key: req.secondary_key,
            ord: req.ord,
            blob: dec(req.blob)?,
            secondary_blob: dec(req.secondary_blob)?,
            updated_ticks: req.updated_ticks,
            view_count: req.view_count,
            view_count_delta: req.view_count_delta,
            request_id: req.request_id,
            salt_base64: req.salt_base64,
            password_hash_base64: req.password_hash_base64,
            iterations: req.iterations,
            created_at_utc: parse_date(&req.created_at_utc).unwrap_or(Ticks(0)),
            batch_videos: rows(req.batch_videos)?,
            batch_progress: rows(req.batch_progress)?,
            batch_list_items: items,
            post_commit_events: req.post_commit_events,
            on_completed: None,
            barrier: None,
        })
    }
}

type Hook = Arc<dyn Fn(Vec<StateSyncEvent>) + Send + Sync>;
type PathHook = Arc<dyn Fn(&str) + Send + Sync>;

static BEFORE_FLUSH_PATH_HOOK: LazyLock<RwLock<Option<PathHook>>> = LazyLock::new(|| RwLock::new(None));
static ACTIVE_STORES: LazyLock<Mutex<Vec<(String, Weak<SqliteProfileStore>)>>> = LazyLock::new(|| Mutex::new(Vec::new()));

static DIAGNOSTICS: LazyLock<RwLock<Option<Arc<dyn sa_core::diagnostics::DiagnosticSink>>>> = LazyLock::new(|| RwLock::new(None));

/// C# `DiagnosticLog.Write` target for the store's SQLite messages (none by default).
pub fn set_diagnostics(sink: Option<Arc<dyn sa_core::diagnostics::DiagnosticSink>>) {
    *DIAGNOSTICS.write() = sink;
}

fn diag(category: &str, message: &str) {
    let sink = DIAGNOSTICS.read().clone();
    if let Some(s) = sink {
        s.write(category, message);
    }
}

/// C# `BeforeFlushPathHook`: the broker pushes its own pending IPC requests for a DB path into the store.
pub fn set_before_flush_path_hook(hook: Option<Box<dyn Fn(&str) + Send + Sync>>) {
    *BEFORE_FLUSH_PATH_HOOK.write() = hook.map(Arc::from);
}

/// Without an override this is `sa_sync::push_in_process_pending_writes` (the C# broker installs the same hook in
/// its static constructor), so a flush or drain first receives every in-process broker's pending requests for `db`.
fn run_before_flush_hook(db: &str) {
    let hook = BEFORE_FLUSH_PATH_HOOK.read().clone();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match hook {
        Some(h) => h(db),
        None => sa_sync::push_in_process_pending_writes(Path::new(db)),
    }));
}

fn live_stores_for(db: &str) -> Vec<Arc<SqliteProfileStore>> {
    let key = upper_invariant(db);
    let weaks: Vec<Weak<SqliteProfileStore>> = {
        let mut g = ACTIVE_STORES.lock();
        g.retain(|(_, w)| w.strong_count() > 0);
        g.iter().filter(|(k, _)| *k == key).map(|(_, w)| w.clone()).collect()
    };
    // Upgraded outside the registry lock: dropping a last reference runs `dispose`, which takes the lock.
    weaks.into_iter().filter_map(|w| w.upgrade()).collect()
}

fn full_path(db: &str) -> String {
    super::paths::get_full_path(db).unwrap_or_else(|| db.to_owned())
}

struct DrainState {
    retry_items: Vec<WriteItem>,
}

type Callback = (OnCompleted, bool, bool, Option<String>);

struct DrainOutcome {
    barriers: Vec<Sender<bool>>,
    barrier_result: bool,
    callbacks: Vec<Callback>,
    events: Vec<StateSyncEvent>,
}

struct Inner {
    db_path: String,
    db: Mutex<Option<Connection>>,
    queue: Mutex<VecDeque<WriteItem>>,
    queue_cv: Condvar,
    drain: ReentrantMutex<RefCell<DrainState>>,
    recent_thumbs: Mutex<HashMap<(String, String), Vec<u8>>>,
    disposed: AtomicBool,
    pending: AtomicI64,
    consecutive_failures: AtomicU32,
    last_batch_succeeded: AtomicBool,
    total_batches: AtomicI64,
    total_ops: AtomicI64,
    total_progress: AtomicI64,
    total_duplicates: AtomicI64,
    last_write_error: Mutex<Option<String>>,
    permanent_error: AtomicBool,
    hook: RwLock<Option<Hook>>,
}

/// `SqliteProfileStore`: the broker engine's data worker for one DB (win-profiles §7–§8).
pub struct SqliteProfileStore {
    inner: Arc<Inner>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for SqliteProfileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteProfileStore").field("db_path", &self.inner.db_path).finish()
    }
}

fn sqlite_error_text(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(err, msg) => {
            let rc = err.extended_code & 0xFF;
            match msg {
                Some(m) if !m.trim().is_empty() => format!("{m} (rc={rc})"),
                _ => format!("sqlite3_exec failed with rc={rc}"),
            }
        }
        other => other.to_string(),
    }
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(err, _) if matches!(err.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked))
}

/// `TryExecNonQueryLocked`: up to 3 attempts on BUSY/LOCKED (10 ms apart).
fn try_exec(conn: &Connection, sql: &str) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..3 {
        match conn.execute_batch(sql) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = sqlite_error_text(&e);
                if !is_busy(&e) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    Err(last)
}

/// `StepWithRetry` around one statement execution (10 × 5 ms, then a last attempt).
fn step_retry<T>(mut f: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    for _ in 0..10 {
        match f() {
            Err(e) if is_busy(&e) => std::thread::sleep(Duration::from_millis(5)),
            r => return r,
        }
    }
    f()
}

fn exec_or_throw(conn: &Connection, op: &str, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Result<(), String> {
    let mut stmt = conn.prepare_cached(sql).map_err(|_| format!("Failed to prepare {op} statement."))?;
    step_retry(|| stmt.execute(p)).map(|_| ()).map_err(|e| match e {
        rusqlite::Error::SqliteFailure(err, _) => format!("{op} sqlite3_step failed with rc={}", err.extended_code & 0xFF),
        other => format!("{op} sqlite3_step failed: {other}"),
    })
}

fn text_of(v: ValueRef<'_>) -> String {
    match v {
        ValueRef::Text(b) | ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(r) => r.to_string(),
        ValueRef::Null => String::new(),
    }
}

fn blob_of(v: ValueRef<'_>) -> Option<Vec<u8>> {
    match v {
        ValueRef::Text(b) | ValueRef::Blob(b) if !b.is_empty() => Some(b.to_vec()),
        _ => None,
    }
}

fn int_of(v: ValueRef<'_>) -> i64 {
    match v {
        ValueRef::Integer(i) => i,
        ValueRef::Real(r) => r as i64,
        ValueRef::Text(b) => std::str::from_utf8(b).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0),
        _ => 0,
    }
}

const INIT_SQL: &str = "PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA temp_store=MEMORY;
PRAGMA secure_delete=ON;
PRAGMA journal_size_limit=0;
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT
);
CREATE TABLE IF NOT EXISTS profiles (
    username TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
    display_username TEXT NOT NULL,
    salt_b64 TEXT NOT NULL,
    password_hash_b64 TEXT NOT NULL,
    iterations INTEGER NOT NULL,
    created_at_utc TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS videos (
    username TEXT NOT NULL COLLATE NOCASE,
    video_key TEXT NOT NULL COLLATE NOCASE,
    encrypted_blob BLOB NOT NULL,
    updated_at_ticks INTEGER NOT NULL,
    view_count INTEGER NOT NULL DEFAULT -1,
    PRIMARY KEY (username, video_key)
);
CREATE TABLE IF NOT EXISTS watch_progress (
    username TEXT NOT NULL COLLATE NOCASE,
    video_key TEXT NOT NULL COLLATE NOCASE,
    encrypted_blob BLOB NOT NULL,
    updated_at_ticks INTEGER NOT NULL,
    PRIMARY KEY (username, video_key)
);
CREATE TABLE IF NOT EXISTS custom_lists (
    username TEXT NOT NULL COLLATE NOCASE,
    list_id TEXT NOT NULL COLLATE NOCASE,
    encrypted_blob BLOB NOT NULL,
    updated_at_ticks INTEGER NOT NULL,
    PRIMARY KEY (username, list_id)
);
CREATE TABLE IF NOT EXISTS custom_list_items (
    username TEXT NOT NULL COLLATE NOCASE,
    list_id TEXT NOT NULL COLLATE NOCASE,
    item_hash TEXT NOT NULL COLLATE NOCASE,
    ord INTEGER NOT NULL,
    encrypted_path_blob BLOB NOT NULL,
    PRIMARY KEY (username, list_id, item_hash)
);
CREATE INDEX IF NOT EXISTS idx_custom_list_items_order
    ON custom_list_items (username, list_id, ord);
CREATE TABLE IF NOT EXISTS thumbnails (
    username TEXT NOT NULL COLLATE NOCASE,
    video_key TEXT NOT NULL COLLATE NOCASE,
    encrypted_blob BLOB NOT NULL,
    updated_at_ticks INTEGER NOT NULL,
    PRIMARY KEY (username, video_key)
);
CREATE TABLE IF NOT EXISTS processed_requests (
    request_id TEXT PRIMARY KEY NOT NULL,
    username TEXT NOT NULL DEFAULT '' COLLATE NOCASE,
    processed_at_ticks INTEGER NOT NULL
);";

const SQL_UPSERT_PROGRESS: &str = "INSERT INTO watch_progress (username, video_key, encrypted_blob, updated_at_ticks)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT(username, video_key) DO UPDATE SET
    encrypted_blob = excluded.encrypted_blob,
    updated_at_ticks = excluded.updated_at_ticks;";

const SQL_UPSERT_LIST: &str = "INSERT INTO custom_lists (username, list_id, encrypted_blob, updated_at_ticks)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT(username, list_id) DO UPDATE SET
    encrypted_blob = excluded.encrypted_blob,
    updated_at_ticks = excluded.updated_at_ticks;";

const SQL_INSERT_IGNORE_VIDEO: &str = "INSERT OR IGNORE INTO videos (username, video_key, encrypted_blob, updated_at_ticks, view_count)
VALUES (?1, ?2, ?3, ?4, -1);";

/// The custom-list item query (must use `idx_custom_list_items_order`).
pub const SQL_LOAD_LIST_ITEMS: &str =
    "SELECT list_id, encrypted_path_blob FROM custom_list_items WHERE username = ?1 ORDER BY list_id, ord ASC;";

fn open_connection(db_path: &str) -> Option<Connection> {
    if let Some(dir) = Path::new(db_path).parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_FULL_MUTEX;
    let conn = Connection::open_with_flags(db_path, flags).ok()?;
    let _ = conn.busy_timeout(Duration::from_millis(150));
    let _ = try_exec(&conn, INIT_SQL);
    let _ = try_exec(&conn, "ALTER TABLE videos ADD COLUMN view_count INTEGER NOT NULL DEFAULT -1;");
    migrate_plaintext_history_columns(&conn);
    Some(conn)
}

fn read_meta(conn: &Connection, key: &str) -> Option<String> {
    let mut stmt = conn.prepare_cached("SELECT value FROM meta WHERE key = ?1 LIMIT 1;").ok()?;
    let r = step_retry(|| {
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(text_of(row.get_ref(0)?))),
            None => Ok(None),
        }
    });
    r.ok().flatten()
}

/// One-time scrub of plaintext history columns written by earlier versions, then the privacy VACUUM.
fn migrate_plaintext_history_columns(conn: &Connection) {
    if scrub_plaintext_history_columns(conn) {
        let _ = try_exec(conn, "UPDATE processed_requests SET username = '' WHERE username <> '';");
        ensure_privacy_vacuum(conn);
    }
}

fn ensure_privacy_vacuum(conn: &Connection) {
    if read_meta(conn, PRIVACY_VACUUM_META_KEY).as_deref() == Some(PRIVACY_VACUUM_VERSION) {
        return;
    }
    if let Err(e) = try_exec(conn, "VACUUM;") {
        diag("SQLite", &format!("Privacy VACUUM deferred: {e}"));
        return;
    }
    let _ = try_exec(conn, "PRAGMA wal_checkpoint(TRUNCATE);");
    let _ = try_exec(
        conn,
        &format!("INSERT OR REPLACE INTO meta (key, value) VALUES ('{PRIVACY_VACUUM_META_KEY}', '{PRIVACY_VACUUM_VERSION}');"),
    );
}

fn scrub_plaintext_history_columns(conn: &Connection) -> bool {
    if read_meta(conn, PRIVACY_SCHEMA_META_KEY).as_deref() == Some(PRIVACY_SCHEMA_VERSION) {
        return true;
    }
    if try_exec(conn, "BEGIN IMMEDIATE;").is_err() {
        return false;
    }
    if read_meta(conn, PRIVACY_SCHEMA_META_KEY).as_deref() == Some(PRIVACY_SCHEMA_VERSION) {
        let _ = try_exec(conn, "COMMIT;");
        return true;
    }
    let legacy_filter =
        format!("substr(video_key, 1, 3) NOT IN ('{HASHED_VIDEO_KEY_PREFIX}', '{OPAQUE_LEGACY_VIDEO_KEY_PREFIX}')");
    let sql = format!(
        "DROP TABLE IF EXISTS temp.legacy_video_keys;
CREATE TEMP TABLE legacy_video_keys (
    username TEXT NOT NULL COLLATE NOCASE,
    old_key TEXT NOT NULL COLLATE NOCASE,
    new_key TEXT NOT NULL,
    PRIMARY KEY (username, old_key)
);
INSERT OR IGNORE INTO temp.legacy_video_keys (username, old_key, new_key)
    SELECT username, video_key, '{rk}' || lower(hex(randomblob(16)))
    FROM (
        SELECT username, video_key FROM videos
        UNION SELECT username, video_key FROM watch_progress
        UNION SELECT username, video_key FROM thumbnails
    )
    WHERE {f};
UPDATE videos SET video_key = (
    SELECT m.new_key FROM temp.legacy_video_keys m
    WHERE m.username = videos.username AND m.old_key = videos.video_key)
WHERE {f};
UPDATE watch_progress SET video_key = (
    SELECT m.new_key FROM temp.legacy_video_keys m
    WHERE m.username = watch_progress.username AND m.old_key = watch_progress.video_key)
WHERE {f};
UPDATE thumbnails SET video_key = (
    SELECT m.new_key FROM temp.legacy_video_keys m
    WHERE m.username = thumbnails.username AND m.old_key = thumbnails.video_key)
WHERE {f};
DROP TABLE temp.legacy_video_keys;
UPDATE videos SET updated_at_ticks = 0, view_count = -1;
UPDATE watch_progress SET updated_at_ticks = 0;
UPDATE thumbnails SET updated_at_ticks = 0;
UPDATE custom_lists SET updated_at_ticks = 0;
DELETE FROM processed_requests;
DELETE FROM meta WHERE key = '{vk}';
INSERT OR REPLACE INTO meta (key, value) VALUES ('{sk}', '{sv}');",
        rk = OPAQUE_LEGACY_VIDEO_KEY_PREFIX,
        f = legacy_filter,
        vk = PRIVACY_VACUUM_META_KEY,
        sk = PRIVACY_SCHEMA_META_KEY,
        sv = PRIVACY_SCHEMA_VERSION
    );
    let result = try_exec(conn, &sql).and_then(|_| try_exec(conn, "COMMIT;"));
    if let Err(e) = result {
        diag("SQLite", &format!("Privacy schema migration failed: {e}"));
        let _ = try_exec(conn, "ROLLBACK;");
        return false;
    }
    true
}

impl Inner {
    fn set_error(&self, e: Option<String>) {
        *self.last_write_error.lock() = e;
    }

    fn has_error(&self) -> bool {
        self.last_write_error.lock().is_some()
    }

    /// Runs `f` with an open connection (`ensure_connection`); `None` when the DB cannot be opened or the store is
    /// disposed.
    fn with_db<R>(&self, f: impl FnOnce(&Connection) -> R) -> Option<R> {
        let mut g = self.db.lock();
        if self.disposed.load(Ordering::SeqCst) {
            return None;
        }
        if g.is_none() {
            *g = open_connection(&self.db_path);
        }
        g.as_ref().map(f)
    }

    fn push(&self, item: WriteItem) {
        let mut q = self.queue.lock();
        q.push_back(item);
        self.queue_cv.notify_all();
    }

    fn forget_cached_thumbnails(&self, username: Option<&str>) {
        let mut g = self.recent_thumbs.lock();
        match username {
            Some(u) if !u.trim().is_empty() => {
                let up = upper_invariant(u.trim());
                g.retain(|(user, _), _| *user != up);
            }
            _ => g.clear(),
        }
    }

    fn release_cached_thumbnail(&self, item: &WriteItem) {
        let mut g = self.recent_thumbs.lock();
        if item.kind == WriteKind::UpsertThumbnail {
            if let Some(blob) = &item.blob {
                let k = (upper_invariant(&item.username), upper_invariant(&item.key));
                if g.get(&k) == Some(blob) {
                    g.remove(&k);
                }
            }
        } else if item.kind == WriteKind::MigrateVideoKey && !item.secondary_key.is_empty() {
            g.remove(&(upper_invariant(&item.username), upper_invariant(&item.secondary_key)));
        }
    }

    fn enqueue(&self, mut item: WriteItem) {
        if self.disposed.load(Ordering::SeqCst) {
            if let Some(cb) = item.on_completed.take() {
                cb(false, false, Some("Store is disposed.".into()));
            }
            return;
        }
        match item.kind {
            WriteKind::UpsertThumbnail => {
                if let Some(blob) = item.blob.as_ref().filter(|b| !b.is_empty()) {
                    self.recent_thumbs
                        .lock()
                        .insert((upper_invariant(&item.username), upper_invariant(&item.key)), blob.clone());
                }
            }
            WriteKind::MigrateVideoKey if !eq_ci(&item.key, &item.secondary_key) => {
                let mut g = self.recent_thumbs.lock();
                let user = upper_invariant(&item.username);
                if let Some(moved) = g.remove(&(user.clone(), upper_invariant(&item.key))) {
                    g.insert((user, upper_invariant(&item.secondary_key)), moved);
                }
            }
            WriteKind::DeleteProfileCascade
            | WriteKind::ReplaceAllVideosAndProgress
            | WriteKind::PurgeLegacyVideoKeys
            | WriteKind::RetagOwner => {
                self.forget_cached_thumbnails(Some(&item.username));
                if !item.secondary_key.trim().is_empty()
                    && matches!(item.kind, WriteKind::DeleteProfileCascade | WriteKind::RetagOwner)
                {
                    self.forget_cached_thumbnails(Some(&item.secondary_key));
                }
            }
            _ => {}
        }
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.push(item);
    }

    fn take_all_queued(&self) -> Vec<WriteItem> {
        self.queue.lock().drain(..).collect()
    }

    fn drain_available_batch(self: &Arc<Self>, state: &RefCell<DrainState>) -> DrainOutcome {
        let mut batch: Vec<WriteItem> = std::mem::take(&mut state.borrow_mut().retry_items);
        let mut barriers = Vec::new();
        for item in self.take_all_queued() {
            if item.kind == WriteKind::FlushBarrier {
                if let Some(b) = item.barrier {
                    barriers.push(b);
                }
            } else {
                batch.push(item);
            }
        }
        let mut callbacks = Vec::new();
        let mut events = Vec::new();
        if !batch.is_empty() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.try_execute_coalesced_batch(&batch)));
            let (ok, progress_count, dup_count, dup_flags) = match result {
                Ok(Ok((p, d, flags))) => (true, p, d, flags),
                Ok(Err(())) => (false, 0, 0, Vec::new()),
                Err(_) => {
                    self.set_error(Some("Unhandled batch write exception.".into()));
                    diag("SQLite", "Unhandled batch write exception.");
                    (false, 0, 0, Vec::new())
                }
            };
            self.last_batch_succeeded.store(ok, Ordering::SeqCst);
            if ok {
                let n = batch.len() as i64;
                self.consecutive_failures.store(0, Ordering::SeqCst);
                self.set_error(None);
                self.total_batches.fetch_add(1, Ordering::SeqCst);
                self.total_ops.fetch_add(n, Ordering::SeqCst);
                self.total_progress.fetch_add(progress_count, Ordering::SeqCst);
                self.total_duplicates.fetch_add(dup_count, Ordering::SeqCst);
                self.pending.fetch_sub(n, Ordering::SeqCst);
                for (i, mut item) in batch.into_iter().enumerate() {
                    self.release_cached_thumbnail(&item);
                    let is_dup = dup_flags.get(i).copied().unwrap_or(false);
                    if let Some(cb) = item.on_completed.take() {
                        callbacks.push((cb, true, is_dup, None));
                    }
                    if !is_dup {
                        if let Some(evts) = item.post_commit_events.take() {
                            events.extend(evts);
                        }
                    }
                }
            } else {
                let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
                if failures >= MAX_RETRY_ATTEMPTS || self.disposed.load(Ordering::SeqCst) {
                    self.permanent_error.store(true, Ordering::SeqCst);
                    self.pending.fetch_sub(batch.len() as i64, Ordering::SeqCst);
                    let err = self.last_write_error.lock().clone();
                    for mut item in batch {
                        self.release_cached_thumbnail(&item);
                        if let Some(cb) = item.on_completed.take() {
                            callbacks.push((cb, false, false, err.clone()));
                        }
                    }
                } else {
                    state.borrow_mut().retry_items = batch;
                    let weak = Arc::downgrade(self);
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(100));
                        if let Some(inner) = weak.upgrade() {
                            if !inner.disposed.load(Ordering::SeqCst) {
                                inner.push(WriteItem::new(WriteKind::FlushBarrier));
                            }
                        }
                    });
                }
            }
        }
        let barrier_result =
            self.last_batch_succeeded.load(Ordering::SeqCst) && !self.permanent_error.load(Ordering::SeqCst);
        DrainOutcome { barriers, barrier_result, callbacks, events }
    }

    fn dispatch(&self, out: DrainOutcome) {
        for (cb, committed, dup, err) in out.callbacks {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(committed, dup, err)));
        }
        if !out.events.is_empty() {
            let hook = self.hook.read().clone();
            if let Some(h) = hook {
                let events = out.events;
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h(events)));
            }
        }
        for b in out.barriers {
            let _ = b.send(out.barrier_result);
        }
    }

    /// `TryExecuteCoalescedBatch` → `(progress upserts, duplicates, duplicate flags)`.
    fn try_execute_coalesced_batch(&self, batch: &[WriteItem]) -> Result<(i64, i64, Vec<bool>), ()> {
        let mut g = self.db.lock();
        if self.disposed.load(Ordering::SeqCst) {
            self.set_error(Some("Database connection is not open.".into()));
            return Err(());
        }
        if g.is_none() {
            *g = open_connection(&self.db_path);
        }
        let Some(conn) = g.as_ref() else {
            self.set_error(Some("Database connection is not open.".into()));
            return Err(());
        };
        if let Err(e) = try_exec(conn, "BEGIN IMMEDIATE;") {
            let msg = if e.is_empty() { "BEGIN IMMEDIATE failed.".to_owned() } else { e };
            diag("SQLite", &format!("BEGIN IMMEDIATE failed on '{}': {msg}", self.db_path));
            self.set_error(Some(msg));
            return Err(());
        }
        match execute_batch_body(conn, batch) {
            Ok(r) => Ok(r),
            Err(msg) => {
                diag("SQLite", &format!("Batch rollback on '{}': {msg}", self.db_path));
                self.set_error(Some(msg));
                let _ = try_exec(conn, "ROLLBACK;");
                Err(())
            }
        }
    }
}

fn is_request_processed_conn(conn: &Connection, request_id: &str) -> Result<bool, String> {
    if request_id.trim().is_empty() {
        return Ok(false);
    }
    let mut stmt = conn
        .prepare_cached("SELECT 1 FROM processed_requests WHERE request_id = ?1 LIMIT 1;")
        .map_err(|_| "Failed to prepare IsRequestProcessed statement.".to_owned())?;
    step_retry(|| stmt.exists(params![request_id])).map_err(|e| sqlite_error_text(&e))
}

fn record_processed_request(conn: &Connection, request_id: &str) -> Result<(), String> {
    if request_id.trim().is_empty() {
        return Ok(());
    }
    exec_or_throw(
        conn,
        "RecordProcessedRequest",
        "INSERT OR IGNORE INTO processed_requests (request_id, username, processed_at_ticks)
VALUES (?1, ?2, ?3);",
        &[&request_id, &"", &0i64],
    )
}

fn upsert_keyed_blob(conn: &Connection, sql: &str, user: &str, key: &str, blob: &[u8]) -> Result<(), String> {
    // Timestamps live inside the encrypted blobs only; the plaintext column is always 0.
    exec_or_throw(conn, "UpsertKeyedBlob", sql, &[&user, &key, &blob, &0i64])
}

fn upsert_video_row(conn: &Connection, user: &str, key: &str, blob: &[u8]) -> Result<(), String> {
    exec_or_throw(
        conn,
        "UpsertVideoRow",
        "INSERT INTO videos (username, video_key, encrypted_blob, updated_at_ticks, view_count)
VALUES (?1, ?2, ?3, 0, -1)
ON CONFLICT(username, video_key) DO UPDATE SET
    encrypted_blob = excluded.encrypted_blob,
    updated_at_ticks = 0,
    view_count = -1;",
        &[&user, &key, &blob],
    )
}

fn upsert_list_item(conn: &Connection, user: &str, list_id: &str, hash: &str, ord: i32, blob: &[u8]) -> Result<(), String> {
    exec_or_throw(
        conn,
        "UpsertCustomListItem",
        "INSERT INTO custom_list_items (username, list_id, item_hash, ord, encrypted_path_blob)
VALUES (?1, ?2, ?3, ?4, ?5)
ON CONFLICT(username, list_id, item_hash) DO UPDATE SET
    ord = excluded.ord,
    encrypted_path_blob = excluded.encrypted_path_blob;",
        &[&user, &list_id, &hash, &i64::from(ord), &blob],
    )
}

fn delete_by_user(conn: &Connection, sql: &str, user: &str) -> Result<(), String> {
    exec_or_throw(conn, "DeleteByUsername", sql, &[&user])
}

fn delete_keyed(conn: &Connection, sql: &str, user: &str, key: &str) -> Result<(), String> {
    exec_or_throw(conn, "DeleteKeyedRow", sql, &[&user, &key])
}

fn execute_batch_body(conn: &Connection, batch: &[WriteItem]) -> Result<(i64, i64, Vec<bool>), String> {
    let mut dup_flags = vec![false; batch.len()];
    let mut seen: HashSet<&str> = HashSet::new();
    let mut dup_count = 0i64;
    for (i, item) in batch.iter().enumerate() {
        let id = item.request_id.as_str();
        if !id.is_empty() && (!seen.insert(id) || is_request_processed_conn(conn, id)?) {
            dup_flags[i] = true;
            dup_count += 1;
        }
    }

    type Pair = (String, String);
    let mut last_video: HashMap<Pair, usize> = HashMap::new();
    let mut acc_delta: HashMap<Pair, i32> = HashMap::new();
    let mut last_prog_blob: HashMap<Pair, usize> = HashMap::new();
    let mut last_progress: HashMap<Pair, usize> = HashMap::new();
    let mut last_thumb: HashMap<Pair, usize> = HashMap::new();
    let pairs: Vec<Pair> = batch.iter().map(|it| (upper_invariant(&it.username), upper_invariant(&it.key))).collect();
    for (i, item) in batch.iter().enumerate() {
        if dup_flags[i] {
            continue;
        }
        let pair = pairs[i].clone();
        match item.kind {
            WriteKind::UpsertVideo => {
                last_video.insert(pair.clone(), i);
                if item.view_count_delta > 0 {
                    *acc_delta.entry(pair.clone()).or_insert(0) += item.view_count_delta;
                }
                if item.secondary_blob.is_some() {
                    last_prog_blob.insert(pair, i);
                }
            }
            WriteKind::UpsertWatchProgress => {
                last_progress.insert(pair, i);
            }
            WriteKind::UpsertThumbnail => {
                last_thumb.insert(pair, i);
            }
            WriteKind::DeleteVideo
            | WriteKind::ReplaceAllVideosAndProgress
            | WriteKind::MigrateVideoKey
            | WriteKind::DeleteProfileCascade
            | WriteKind::PurgeLegacyVideoKeys
            | WriteKind::RetagOwner => {
                last_video.clear();
                acc_delta.clear();
                last_prog_blob.clear();
                last_progress.clear();
            }
            _ => {}
        }
    }

    let mut progress_count = 0i64;
    let mut recorded = false;
    for (i, item) in batch.iter().enumerate() {
        if dup_flags[i] {
            continue;
        }
        let pair = &pairs[i];
        let superseded = match item.kind {
            WriteKind::UpsertVideo => last_video.get(pair).is_some_and(|&j| j != i),
            WriteKind::UpsertWatchProgress => last_progress.get(pair).is_some_and(|&j| j != i),
            WriteKind::UpsertThumbnail => last_thumb.get(pair).is_some_and(|&j| j != i),
            _ => false,
        };
        if superseded {
            if !item.request_id.is_empty() {
                record_processed_request(conn, &item.request_id)?;
                recorded = true;
            }
            continue;
        }
        let eff_delta =
            if item.kind == WriteKind::UpsertVideo { acc_delta.get(pair).copied().unwrap_or(item.view_count_delta) } else { item.view_count_delta };
        let eff_secondary: Option<&[u8]> = if item.kind == WriteKind::UpsertVideo && item.secondary_blob.is_none() {
            last_prog_blob.get(pair).and_then(|&j| batch[j].secondary_blob.as_deref())
        } else {
            item.secondary_blob.as_deref()
        };
        execute_single_write(conn, item, eff_delta, eff_secondary)?;
        if !item.request_id.is_empty() {
            record_processed_request(conn, &item.request_id)?;
            recorded = true;
        }
        if (item.kind == WriteKind::UpsertWatchProgress && item.blob.is_some())
            || (item.kind == WriteKind::UpsertVideo && eff_secondary.is_some())
        {
            progress_count += 1;
        }
    }
    if recorded {
        try_exec(
            conn,
            &format!(
                "DELETE FROM processed_requests WHERE rowid NOT IN (SELECT rowid FROM processed_requests ORDER BY rowid DESC LIMIT {MAX_PROCESSED_REQUEST_ROWS});"
            ),
        )
        .map_err(|e| if e.is_empty() { "Pruning processed_requests failed.".to_owned() } else { e })?;
    }
    try_exec(conn, "COMMIT;").map_err(|e| if e.is_empty() { "COMMIT failed.".to_owned() } else { e })?;
    Ok((progress_count, dup_count, dup_flags))
}

fn execute_single_write(conn: &Connection, item: &WriteItem, _eff_delta: i32, eff_secondary: Option<&[u8]>) -> Result<(), String> {
    let user = item.username.as_str();
    let key = item.key.as_str();
    match item.kind {
        WriteKind::UpsertProfile => exec_or_throw(
            conn,
            "UpsertProfile",
            "INSERT INTO profiles (username, display_username, salt_b64, password_hash_b64, iterations, created_at_utc)
VALUES (?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT(username) DO UPDATE SET
    display_username = excluded.display_username,
    salt_b64 = excluded.salt_b64,
    password_hash_b64 = excluded.password_hash_b64,
    iterations = excluded.iterations,
    created_at_utc = excluded.created_at_utc;",
            &[&user, &user, &item.salt_base64, &item.password_hash_base64, &i64::from(item.iterations), &format_round_trip(item.created_at_utc)],
        )?,
        WriteKind::DeleteProfileCascade => {
            for owner in [user, item.secondary_key.as_str()] {
                if owner.trim().is_empty() {
                    continue;
                }
                for table in OWNED_DATA_TABLES {
                    delete_by_user(conn, &format!("DELETE FROM {table} WHERE username = ?1;"), owner)?;
                }
            }
            delete_by_user(conn, "DELETE FROM profiles WHERE username = ?1;", user)?;
        }
        WriteKind::RetagOwner => {
            if !user.trim().is_empty() && item.secondary_key.starts_with(OWNER_TAG_PREFIX) {
                for table in OWNED_DATA_TABLES {
                    delete_keyed(conn, &format!("UPDATE OR IGNORE {table} SET username = ?2 WHERE username = ?1;"), user, &item.secondary_key)?;
                    delete_by_user(conn, &format!("DELETE FROM {table} WHERE username = ?1;"), user)?;
                }
            }
        }
        WriteKind::UpsertVideo => {
            if let Some(b) = &item.blob {
                upsert_video_row(conn, user, key, b)?;
            }
            if let Some(p) = eff_secondary {
                upsert_keyed_blob(conn, SQL_UPSERT_PROGRESS, user, key, p)?;
            }
        }
        WriteKind::UpsertWatchProgress => {
            if let Some(v) = eff_secondary {
                upsert_keyed_blob(conn, SQL_INSERT_IGNORE_VIDEO, user, key, v)?;
            }
            if let Some(b) = &item.blob {
                upsert_keyed_blob(conn, SQL_UPSERT_PROGRESS, user, key, b)?;
            }
        }
        WriteKind::DeleteVideo => {
            delete_keyed(conn, "DELETE FROM watch_progress WHERE username = ?1 AND video_key = ?2;", user, key)?;
            delete_keyed(conn, "DELETE FROM videos WHERE username = ?1 AND video_key = ?2;", user, key)?;
        }
        WriteKind::ReplaceAllVideosAndProgress => {
            delete_by_user(conn, "DELETE FROM watch_progress WHERE username = ?1;", user)?;
            delete_by_user(conn, "DELETE FROM videos WHERE username = ?1;", user)?;
            for (k, b, _) in item.batch_videos.iter().flatten() {
                upsert_keyed_blob(
                    conn,
                    "INSERT OR REPLACE INTO videos (username, video_key, encrypted_blob, updated_at_ticks, view_count)
VALUES (?1, ?2, ?3, ?4, -1);",
                    user,
                    k,
                    b,
                )?;
            }
            for (k, b, _) in item.batch_progress.iter().flatten() {
                upsert_keyed_blob(
                    conn,
                    "INSERT OR REPLACE INTO watch_progress (username, video_key, encrypted_blob, updated_at_ticks)
VALUES (?1, ?2, ?3, ?4);",
                    user,
                    k,
                    b,
                )?;
            }
            delete_by_user(
                conn,
                "DELETE FROM thumbnails WHERE username = ?1 AND video_key NOT IN (SELECT video_key FROM videos WHERE username = ?1);",
                user,
            )?;
        }
        WriteKind::PurgeLegacyVideoKeys => {
            for table in ["watch_progress", "videos", "thumbnails"] {
                delete_by_user(
                    conn,
                    &format!("DELETE FROM {table} WHERE username = ?1 AND substr(video_key, 1, 3) <> '{HASHED_VIDEO_KEY_PREFIX}';"),
                    user,
                )?;
            }
        }
        WriteKind::UpsertCustomList => {
            if let Some(b) = &item.blob {
                upsert_keyed_blob(conn, SQL_UPSERT_LIST, user, key, b)?;
            }
        }
        WriteKind::DeleteCustomList => {
            delete_keyed(conn, "DELETE FROM custom_list_items WHERE username = ?1 AND list_id = ?2;", user, key)?;
            delete_keyed(conn, "DELETE FROM custom_lists WHERE username = ?1 AND list_id = ?2;", user, key)?;
        }
        WriteKind::UpsertCustomListItem => {
            if let Some(h) = eff_secondary {
                upsert_keyed_blob(conn, SQL_UPSERT_LIST, user, key, h)?;
            }
            if let Some(b) = &item.blob {
                upsert_list_item(conn, user, key, &item.secondary_key, item.ord, b)?;
            }
        }
        WriteKind::DeleteCustomListItem => {
            if let Some(h) = eff_secondary {
                upsert_keyed_blob(conn, SQL_UPSERT_LIST, user, key, h)?;
            }
            exec_or_throw(
                conn,
                "DeleteCustomListItem",
                "DELETE FROM custom_list_items WHERE username = ?1 AND list_id = ?2 AND item_hash = ?3;",
                &[&user, &key, &item.secondary_key],
            )?;
        }
        WriteKind::ReplaceCustomListItems => {
            if let Some(h) = eff_secondary {
                upsert_keyed_blob(conn, SQL_UPSERT_LIST, user, key, h)?;
            }
            delete_keyed(conn, "DELETE FROM custom_list_items WHERE username = ?1 AND list_id = ?2;", user, key)?;
            for li in item.batch_list_items.iter().flatten() {
                upsert_list_item(conn, user, &li.list_id, &li.item_hash, li.ord, &li.encrypted_path_blob)?;
            }
        }
        WriteKind::UpsertThumbnail => {
            if let Some(v) = eff_secondary {
                upsert_keyed_blob(conn, SQL_INSERT_IGNORE_VIDEO, user, key, v)?;
            }
            if let Some(b) = &item.blob {
                upsert_keyed_blob(
                    conn,
                    "INSERT INTO thumbnails (username, video_key, encrypted_blob, updated_at_ticks)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT(username, video_key) DO UPDATE SET
    encrypted_blob = excluded.encrypted_blob,
    updated_at_ticks = excluded.updated_at_ticks;",
                    user,
                    key,
                    b,
                )?;
            }
        }
        WriteKind::MigrateVideoKey => {
            let new_key = item.secondary_key.as_str();
            if !eq_ci(key, new_key) {
                delete_keyed(conn, "DELETE FROM watch_progress WHERE username = ?1 AND video_key = ?2;", user, key)?;
                delete_keyed(conn, "DELETE FROM videos WHERE username = ?1 AND video_key = ?2;", user, key)?;
                exec_or_throw(
                    conn,
                    "MigrateVideoKey",
                    "UPDATE OR REPLACE thumbnails
SET video_key = ?3
WHERE username = ?1 AND video_key = ?2;",
                    &[&user, &key, &new_key],
                )?;
            }
            if let Some(b) = &item.blob {
                upsert_video_row(conn, user, new_key, b)?;
            }
            if let Some(p) = eff_secondary {
                upsert_keyed_blob(
                    conn,
                    "INSERT OR REPLACE INTO watch_progress (username, video_key, encrypted_blob, updated_at_ticks)
VALUES (?1, ?2, ?3, ?4);",
                    user,
                    new_key,
                    p,
                )?;
            }
        }
        WriteKind::FlushBarrier => {}
    }
    Ok(())
}

fn count_rows(conn: &Connection, sql: &str, user: &str) -> i64 {
    let Ok(mut stmt) = conn.prepare_cached(sql) else { return 0 };
    step_retry(|| stmt.query_row(params![user], |r| Ok(int_of(r.get_ref(0)?)))).unwrap_or(0)
}

fn load_keyed(conn: &Connection, sql: &str, user: &str) -> Vec<(String, Vec<u8>)> {
    let Ok(mut stmt) = conn.prepare_cached(sql) else { return Vec::new() };
    let r = step_retry(|| {
        let mut rows = stmt.query(params![user])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let key = text_of(row.get_ref(0)?);
            if let Some(blob) = blob_of(row.get_ref(1)?) {
                if !key.is_empty() {
                    out.push((key, blob));
                }
            }
        }
        Ok(out)
    });
    r.unwrap_or_default()
}

impl SqliteProfileStore {
    /// Opens (creating if needed) the DB at `db` and starts the writer thread. Each call creates a new store with its
    /// own connection; the broker engine keeps one per DB path.
    pub fn new(db: &Path) -> Arc<Self> {
        let db_path = full_path(&db.to_string_lossy());
        let inner = Arc::new(Inner {
            db_path: db_path.clone(),
            db: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            queue_cv: Condvar::new(),
            drain: ReentrantMutex::new(RefCell::new(DrainState { retry_items: Vec::new() })),
            recent_thumbs: Mutex::new(HashMap::new()),
            disposed: AtomicBool::new(false),
            pending: AtomicI64::new(0),
            consecutive_failures: AtomicU32::new(0),
            last_batch_succeeded: AtomicBool::new(true),
            total_batches: AtomicI64::new(0),
            total_ops: AtomicI64::new(0),
            total_progress: AtomicI64::new(0),
            total_duplicates: AtomicI64::new(0),
            last_write_error: Mutex::new(None),
            permanent_error: AtomicBool::new(false),
            hook: RwLock::new(None),
        });
        {
            let mut g = inner.db.lock();
            *g = open_connection(&db_path);
        }
        let loop_inner = inner.clone();
        let handle = std::thread::Builder::new()
            .name("sa-profiles-sqlite-writer".into())
            .spawn(move || writer_loop(loop_inner))
            .ok();
        let store = Arc::new(Self { inner, writer: Mutex::new(handle) });
        ACTIVE_STORES.lock().push((upper_invariant(&db_path), Arc::downgrade(&store)));
        store
    }

    /// Alias of [`SqliteProfileStore::new`] (C# `new SqliteProfileStore(dbPath)`).
    pub fn open(db: &Path) -> Arc<Self> {
        Self::new(db)
    }

    /// Factory to register with `ProfileWriteBroker::set_data_worker_factory`.
    pub fn factory() -> sa_sync::DataWorkerFactory {
        Box::new(|db| SqliteProfileStore::new(db) as Arc<dyn DataWorkerStore>)
    }

    /// The concrete store behind a broker's `Arc<dyn DataWorkerStore>` (pointer identity), when it is one.
    pub fn from_worker(worker: &Arc<dyn DataWorkerStore>) -> Option<Arc<Self>> {
        let ptr = Arc::as_ptr(worker) as *const ();
        let weak = {
            let g = ACTIVE_STORES.lock();
            g.iter().find(|(_, w)| Weak::as_ptr(w) as *const () == ptr).map(|(_, w)| w.clone())
        };
        weak.and_then(|w| w.upgrade())
    }

    /// A live store for `db` registered in this process (any), if one exists.
    pub fn find_live(db: &Path) -> Option<Arc<Self>> {
        live_stores_for(&full_path(&db.to_string_lossy())).into_iter().find(|s| !s.is_disposed())
    }

    pub fn db_path(&self) -> &str {
        &self.inner.db_path
    }
    pub fn is_disposed(&self) -> bool {
        self.inner.disposed.load(Ordering::SeqCst)
    }
    pub fn pending_write_count(&self) -> i64 {
        self.inner.pending.load(Ordering::SeqCst)
    }
    pub fn total_batches_committed(&self) -> i64 {
        self.inner.total_batches.load(Ordering::SeqCst)
    }
    pub fn total_operations_committed(&self) -> i64 {
        self.inner.total_ops.load(Ordering::SeqCst)
    }
    pub fn total_watch_progress_upserts(&self) -> i64 {
        self.inner.total_progress.load(Ordering::SeqCst)
    }
    pub fn total_duplicate_requests_ignored(&self) -> i64 {
        self.inner.total_duplicates.load(Ordering::SeqCst)
    }
    pub fn last_write_error(&self) -> Option<String> {
        self.inner.last_write_error.lock().clone()
    }
    pub fn has_permanent_write_error(&self) -> bool {
        self.inner.permanent_error.load(Ordering::SeqCst)
    }

    /// C# `PostCommitBroadcastHandler`.
    pub fn set_post_commit_broadcast_handler(&self, hook: Option<Box<dyn Fn(Vec<StateSyncEvent>) + Send + Sync>>) {
        *self.inner.hook.write() = hook.map(Arc::from);
    }

    /// `EnqueueItem`.
    pub fn enqueue_item(&self, item: WriteItem) {
        self.inner.enqueue(item);
    }

    /// `EnqueueUpsertProfile`.
    pub fn enqueue_upsert_profile(&self, username: &str, salt_b64: &str, hash_b64: &str, iterations: i32, created: Ticks) {
        let mut it = WriteItem::new(WriteKind::UpsertProfile);
        it.username = username.to_owned();
        it.salt_base64 = Some(salt_b64.to_owned());
        it.password_hash_base64 = Some(hash_b64.to_owned());
        it.iterations = iterations;
        it.created_at_utc = created;
        self.inner.enqueue(it);
    }

    /// `EnqueueUpsertVideo` (raw, for tests and tools).
    pub fn enqueue_upsert_video(&self, username: &str, key: &str, blob: Vec<u8>, ticks: i64) {
        let mut it = WriteItem::new(WriteKind::UpsertVideo).with_user_key(username, key).with_blob(blob);
        it.updated_ticks = ticks;
        self.inner.enqueue(it);
    }

    /// `ForgetCachedThumbnails` (blank = all).
    pub fn forget_cached_thumbnails(&self, username: Option<&str>) {
        self.inner.forget_cached_thumbnails(username);
    }

    /// Thumbnail blobs held for not-yet-read writes (diagnostics and tests).
    pub fn cached_thumbnail_blob_count(&self) -> usize {
        self.inner.recent_thumbs.lock().len()
    }

    /// `FlushAsync`: a receiver that yields the barrier result (`false` at once when disposed).
    pub fn flush_async(&self) -> Receiver<bool> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        if self.is_disposed() {
            let _ = tx.send(false);
            return rx;
        }
        run_before_flush_hook(&self.inner.db_path);
        let mut item = WriteItem::new(WriteKind::FlushBarrier);
        item.barrier = Some(tx);
        self.inner.push(item);
        rx
    }

    /// `Flush(timeout)`: true only if the last batch succeeded and there is no permanent write error.
    pub fn flush(&self, timeout: Duration) -> bool {
        if self.is_disposed() {
            return false;
        }
        if self.inner.drain.is_owned_by_current_thread() {
            // A post-commit handler on the draining thread (the Rust buses deliver events synchronously, the C#
            // broker asynchronously): the barrier could only be served by this thread, so report the last result
            // instead of waiting out the timeout.
            return self.inner.last_batch_succeeded.load(Ordering::SeqCst) && !self.inner.permanent_error.load(Ordering::SeqCst);
        }
        self.flush_async().recv_timeout(timeout).unwrap_or(false)
    }

    /// `TryDrainWhenHealthyNonBlocking`: commits queued writes on the calling thread (read-your-writes).
    pub fn try_drain_when_healthy_non_blocking(&self) {
        let inner = &self.inner;
        if inner.disposed.load(Ordering::SeqCst) || inner.drain.is_owned_by_current_thread() {
            return;
        }
        run_before_flush_hook(&inner.db_path);
        if inner.has_error() {
            return;
        }
        let guard = inner.drain.lock();
        if inner.pending.load(Ordering::SeqCst) <= 0 || inner.has_error() || inner.disposed.load(Ordering::SeqCst) {
            return;
        }
        let mut all = DrainOutcome { barriers: Vec::new(), barrier_result: true, callbacks: Vec::new(), events: Vec::new() };
        while guard.borrow().retry_items.is_empty()
            && inner.pending.load(Ordering::SeqCst) > 0
            && !inner.has_error()
            && !inner.disposed.load(Ordering::SeqCst)
        {
            let out = inner.drain_available_batch(&guard);
            all.barriers.extend(out.barriers);
            all.barrier_result = all.barrier_result && out.barrier_result;
            all.callbacks.extend(out.callbacks);
            all.events.extend(out.events);
            if !out.barrier_result {
                break;
            }
        }
        if !all.barriers.is_empty() || !all.callbacks.is_empty() || !all.events.is_empty() {
            inner.dispatch(all);
        }
        drop(guard);
    }

    /// `IsRequestProcessed`.
    pub fn is_request_processed(&self, request_id: &str) -> bool {
        if request_id.trim().is_empty() {
            return false;
        }
        self.inner.with_db(|c| is_request_processed_conn(c, request_id.trim()).unwrap_or(false)).unwrap_or(false)
    }

    /// `HasRowsOwnedBy` (no flush).
    pub fn has_rows_owned_by(&self, owner: &str) -> bool {
        if owner.trim().is_empty() {
            return false;
        }
        self.inner
            .with_db(|c| {
                OWNED_DATA_TABLES.iter().any(|t| {
                    count_rows(c, &format!("SELECT COUNT(*) FROM (SELECT 1 FROM {t} WHERE username = ?1 LIMIT 1);"), owner) > 0
                })
            })
            .unwrap_or(false)
    }

    /// `LoadVideosWithViewCounts`.
    pub fn load_videos_with_view_counts(&self, user: &str) -> Vec<(String, Vec<u8>, i64)> {
        self.inner
            .with_db(|c| {
                let Ok(mut stmt) = c.prepare_cached("SELECT video_key, encrypted_blob, view_count FROM videos WHERE username = ?1;")
                else {
                    return Vec::new();
                };
                step_retry(|| {
                    let mut rows = stmt.query(params![user])?;
                    let mut out = Vec::new();
                    while let Some(row) = rows.next()? {
                        let key = text_of(row.get_ref(0)?);
                        let blob = blob_of(row.get_ref(1)?);
                        let vc = int_of(row.get_ref(2)?);
                        if let Some(b) = blob {
                            if !key.is_empty() {
                                out.push((key, b, vc));
                            }
                        }
                    }
                    Ok(out)
                })
                .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    pub fn load_watch_progress(&self, user: &str) -> Vec<(String, Vec<u8>)> {
        self.inner
            .with_db(|c| load_keyed(c, "SELECT video_key, encrypted_blob FROM watch_progress WHERE username = ?1;", user))
            .unwrap_or_default()
    }

    pub fn load_custom_lists(&self, user: &str) -> Vec<(String, Vec<u8>)> {
        self.inner
            .with_db(|c| load_keyed(c, "SELECT list_id, encrypted_blob FROM custom_lists WHERE username = ?1;", user))
            .unwrap_or_default()
    }

    pub fn load_custom_list_items(&self, user: &str) -> Vec<(String, Vec<u8>)> {
        self.inner.with_db(|c| load_keyed(c, SQL_LOAD_LIST_ITEMS, user)).unwrap_or_default()
    }

    /// `TryGetThumbnailBlob` (recent in-memory writes first).
    pub fn try_get_thumbnail_blob(&self, user: &str, key: &str) -> Option<Vec<u8>> {
        if let Some(b) = self.inner.recent_thumbs.lock().get(&(upper_invariant(user), upper_invariant(key))) {
            return Some(b.clone());
        }
        self.inner
            .with_db(|c| {
                let mut stmt =
                    c.prepare_cached("SELECT encrypted_blob FROM thumbnails WHERE username = ?1 AND video_key = ?2 LIMIT 1;").ok()?;
                step_retry(|| {
                    let mut rows = stmt.query(params![user, key])?;
                    match rows.next()? {
                        Some(row) => Ok(blob_of(row.get_ref(0)?)),
                        None => Ok(None),
                    }
                })
                .ok()
                .flatten()
            })
            .flatten()
    }

    /// `ImportLegacyVaultBatchAndVerify` (bypasses the queue).
    pub fn import_legacy_vault_batch_and_verify(
        &self,
        user: &str,
        videos: &[BlobRow],
        progress: &[BlobRow],
        lists: &[BlobRow],
        list_items: &[ListItemRow],
        thumbnails: &[BlobRow],
    ) -> bool {
        flush_for_path(Path::new(&self.inner.db_path));
        let inner = &self.inner;
        let r = inner.with_db(|conn| -> bool {
            if let Err(e) = try_exec(conn, "BEGIN IMMEDIATE;") {
                let msg = if e.is_empty() { "BEGIN IMMEDIATE failed during legacy import.".to_owned() } else { e };
                diag("SQLite", &format!("ImportLegacyVaultBatchAndVerify BEGIN IMMEDIATE failed: {msg}"));
                inner.set_error(Some(msg));
                return false;
            }
            let body = || -> Result<(), String> {
                for (k, b, _) in videos {
                    upsert_keyed_blob(
                        conn,
                        "INSERT OR REPLACE INTO videos (username, video_key, encrypted_blob, updated_at_ticks, view_count)
VALUES (?1, ?2, ?3, ?4, -1);",
                        user,
                        k,
                        b,
                    )?;
                }
                for (k, b, _) in progress {
                    upsert_keyed_blob(
                        conn,
                        "INSERT OR REPLACE INTO watch_progress (username, video_key, encrypted_blob, updated_at_ticks)
VALUES (?1, ?2, ?3, ?4);",
                        user,
                        k,
                        b,
                    )?;
                }
                for (k, b, _) in lists {
                    upsert_keyed_blob(
                        conn,
                        "INSERT OR REPLACE INTO custom_lists (username, list_id, encrypted_blob, updated_at_ticks)
VALUES (?1, ?2, ?3, ?4);",
                        user,
                        k,
                        b,
                    )?;
                }
                for li in list_items {
                    upsert_list_item(conn, user, &li.list_id, &li.item_hash, li.ord, &li.encrypted_path_blob)?;
                }
                for (k, b, _) in thumbnails {
                    upsert_keyed_blob(
                        conn,
                        "INSERT OR REPLACE INTO thumbnails (username, video_key, encrypted_blob, updated_at_ticks)
VALUES (?1, ?2, ?3, ?4);",
                        user,
                        k,
                        b,
                    )?;
                }
                try_exec(conn, "COMMIT;").map_err(|e| if e.is_empty() { "COMMIT failed during legacy import.".into() } else { e })
            };
            if let Err(msg) = body() {
                diag("SQLite", &format!("ImportLegacyVaultBatchAndVerify rollback: {msg}"));
                inner.set_error(Some(msg));
                let _ = try_exec(conn, "ROLLBACK;");
                return false;
            }
            let c = |t: &str| count_rows(conn, &format!("SELECT COUNT(*) FROM {t} WHERE username = ?1;"), user);
            c("videos") >= videos.len() as i64
                && c("watch_progress") >= progress.len() as i64
                && c("custom_lists") >= lists.len() as i64
                && c("custom_list_items") >= list_items.len() as i64
                && c("thumbnails") >= thumbnails.len() as i64
        });
        match r {
            Some(v) => v,
            None => {
                inner.set_error(Some("Database connection is not open.".into()));
                false
            }
        }
    }

    /// `GetTableRowCounts(owner)`: (videos, watch_progress, custom_lists, custom_list_items, thumbnails).
    pub fn get_table_row_counts(&self, user: &str) -> (i64, i64, i64, i64, i64) {
        flush_for_path(Path::new(&self.inner.db_path));
        self.inner
            .with_db(|c| {
                let n = |t: &str| count_rows(c, &format!("SELECT COUNT(*) FROM {t} WHERE username = ?1;"), user);
                (n("videos"), n("watch_progress"), n("custom_lists"), n("custom_list_items"), n("thumbnails"))
            })
            .unwrap_or((0, 0, 0, 0, 0))
    }

    /// `ExplainQueryPlan`: the `detail` column per row.
    pub fn explain_query_plan(&self, sql: &str) -> Vec<String> {
        self.inner
            .with_db(|c| {
                let Ok(mut stmt) = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")) else { return Vec::new() };
                let mut out = Vec::new();
                // `raw_query`: the plan of a statement with unbound parameters (`?1`), like `sqlite3_step` in C#.
                let mut rows = stmt.raw_query();
                while let Ok(Some(row)) = rows.next() {
                    out.push(row.get_ref(3).map(text_of).unwrap_or_default());
                }
                out
            })
            .unwrap_or_default()
    }

    /// `CloseConnectionIfIdle`.
    pub fn close_connection_if_idle(&self) {
        if self.pending_write_count() > 0 {
            return;
        }
        let mut g = self.inner.db.lock();
        if self.pending_write_count() == 0 {
            *g = None;
        }
    }

    /// `Dispose`: flush (5 s), stop the writer (2 s), unregister, close the DB. Idempotent.
    pub fn dispose(&self) {
        if self.is_disposed() {
            return;
        }
        self.flush(Duration::from_secs(5));
        self.inner.disposed.store(true, Ordering::SeqCst);
        {
            let _q = self.inner.queue.lock();
            self.inner.queue_cv.notify_all();
        }
        if let Some(h) = self.writer.lock().take() {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !h.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if h.is_finished() {
                let _ = h.join();
            }
        }
        {
            let me = self as *const Self as *const ();
            ACTIVE_STORES.lock().retain(|(_, w)| Weak::as_ptr(w) as *const () != me && w.strong_count() > 0);
        }
        *self.inner.db.lock() = None;
    }

    // ----- test and diagnostics helpers (C# `internal ... ForTests`)

    /// Every `(table, username, video_key, updated_at_ticks, view_count)` row of videos / watch_progress / thumbnails.
    #[doc(hidden)]
    pub fn dump_plaintext_video_columns(&self) -> Vec<(String, String, String, i64, i64)> {
        flush_for_path(Path::new(&self.inner.db_path));
        self.inner
            .with_db(|c| {
                let mut out = Vec::new();
                for (table, view_col) in [("videos", "view_count"), ("watch_progress", "-1"), ("thumbnails", "-1")] {
                    let Ok(mut stmt) = c.prepare(&format!("SELECT username, video_key, updated_at_ticks, {view_col} FROM {table};"))
                    else {
                        continue;
                    };
                    if let Ok(mut rows) = stmt.query([]) {
                        while let Ok(Some(r)) = rows.next() {
                            out.push((
                                table.to_owned(),
                                r.get_ref(0).map(text_of).unwrap_or_default(),
                                r.get_ref(1).map(text_of).unwrap_or_default(),
                                r.get_ref(2).map(int_of).unwrap_or(0),
                                r.get_ref(3).map(int_of).unwrap_or(0),
                            ));
                        }
                    }
                }
                out
            })
            .unwrap_or_default()
    }

    /// Every `(username, processed_at_ticks)` row of `processed_requests`.
    #[doc(hidden)]
    pub fn dump_processed_requests(&self) -> Vec<(String, i64)> {
        flush_for_path(Path::new(&self.inner.db_path));
        self.inner
            .with_db(|c| {
                let mut out = Vec::new();
                if let Ok(mut stmt) = c.prepare("SELECT username, processed_at_ticks FROM processed_requests;") {
                    if let Ok(mut rows) = stmt.query([]) {
                        while let Ok(Some(r)) = rows.next() {
                            out.push((r.get_ref(0).map(text_of).unwrap_or_default(), r.get_ref(1).map(int_of).unwrap_or(0)));
                        }
                    }
                }
                out
            })
            .unwrap_or_default()
    }

    /// `(table, owner, length(blob))` of every data row.
    #[doc(hidden)]
    pub fn dump_owners_and_blob_lengths(&self) -> Vec<(String, String, i64)> {
        flush_for_path(Path::new(&self.inner.db_path));
        self.inner
            .with_db(|c| {
                let mut out = Vec::new();
                for table in OWNED_DATA_TABLES {
                    let col = if table == "custom_list_items" { "encrypted_path_blob" } else { "encrypted_blob" };
                    let Ok(mut stmt) = c.prepare(&format!("SELECT username, length({col}) FROM {table};")) else { continue };
                    if let Ok(mut rows) = stmt.query([]) {
                        while let Ok(Some(r)) = rows.next() {
                            out.push((
                                table.to_owned(),
                                r.get_ref(0).map(text_of).unwrap_or_default(),
                                r.get_ref(1).map(int_of).unwrap_or(0),
                            ));
                        }
                    }
                }
                out
            })
            .unwrap_or_default()
    }

    /// Runs SQL directly (tests seeding legacy rows).
    #[doc(hidden)]
    pub fn execute_raw_for_tests(&self, sql: &str) -> bool {
        self.inner.with_db(|c| try_exec(c, sql).is_ok()).unwrap_or(false)
    }

    #[doc(hidden)]
    pub fn run_startup_privacy_steps_for_tests(&self) {
        self.inner.with_db(migrate_plaintext_history_columns);
    }

    #[doc(hidden)]
    pub fn rerun_privacy_migration_for_tests(&self) {
        self.inner.with_db(|c| {
            let _ = try_exec(c, &format!("DELETE FROM meta WHERE key = '{PRIVACY_SCHEMA_META_KEY}';"));
            migrate_plaintext_history_columns(c);
        });
    }

    #[doc(hidden)]
    pub fn is_privacy_vacuum_done_for_tests(&self) -> bool {
        self.inner
            .with_db(|c| read_meta(c, PRIVACY_VACUUM_META_KEY).as_deref() == Some(PRIVACY_VACUUM_VERSION))
            .unwrap_or(false)
    }
}

impl Drop for SqliteProfileStore {
    fn drop(&mut self) {
        self.dispose();
    }
}

fn writer_loop(inner: Arc<Inner>) {
    loop {
        {
            let mut q = inner.queue.lock();
            while q.is_empty() && !inner.disposed.load(Ordering::SeqCst) {
                inner.queue_cv.wait(&mut q);
            }
            if q.is_empty() && inner.disposed.load(Ordering::SeqCst) {
                break;
            }
        }
        let guard = inner.drain.lock();
        let out = inner.drain_available_batch(&guard);
        inner.dispatch(out);
        drop(guard);
    }
    // Resolve every leftover barrier.
    for item in inner.take_all_queued() {
        if let Some(b) = item.barrier {
            let _ = b.send(false);
        }
    }
}

/// `FlushForPath`: the broker hook, then `flush(5 s)` on every live store of `db` with pending writes.
pub fn flush_for_path(db: &Path) {
    let full = full_path(&db.to_string_lossy());
    run_before_flush_hook(&full);
    for s in live_stores_for(&full) {
        if !s.is_disposed() && s.pending_write_count() > 0 {
            s.flush(Duration::from_secs(5));
        }
    }
}

/// `FlushAllForPath`: [`flush_for_path`], then closes idle connections (tests use it before deleting DB files).
pub fn flush_all_for_path(db: &Path) {
    let full = full_path(&db.to_string_lossy());
    flush_for_path(Path::new(&full));
    for s in live_stores_for(&full) {
        if !s.is_disposed() && s.pending_write_count() == 0 {
            s.close_connection_if_idle();
        }
    }
}

impl DataWorkerStore for SqliteProfileStore {
    fn enqueue(&self, request: StateSyncWriteRequest, on_ack: Box<dyn FnOnce(StateSyncWriteAck) + Send>) {
        let request_id = request.request_id.clone();
        match WriteItem::from_request(request) {
            Ok(mut item) => {
                let id = request_id.clone();
                item.on_completed = Some(Box::new(move |committed, dup, err| {
                    on_ack(StateSyncWriteAck { request_id: id, committed, is_duplicate: dup, error: err });
                }));
                self.inner.enqueue(item);
            }
            Err(error) => on_ack(StateSyncWriteAck { request_id, committed: false, is_duplicate: false, error: Some(error) }),
        }
    }
    fn flush(&self, timeout: Duration) -> bool {
        SqliteProfileStore::flush(self, timeout)
    }
    fn set_post_commit_hook(&self, hook: Box<dyn Fn(Vec<StateSyncEvent>) + Send + Sync>) {
        self.set_post_commit_broadcast_handler(Some(hook));
    }
    /// Engine shutdown ("the last window closes the engine"): the C# engine disposes its store the same way.
    fn dispose(&self) {
        SqliteProfileStore::dispose(self);
    }
}
