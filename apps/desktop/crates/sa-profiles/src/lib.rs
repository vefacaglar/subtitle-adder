//! `sa-profiles`: the profile store (UNIFIED.md §5.1, §3.11, §4).
//!
//! One trait, [`ProfileStore`], two backends compiled per platform:
//! * [`win::WinProfileStore`] (`cfg(windows)`): `profiles.json` v3 header + `profiles.db` (SQLite WAL) + broker write
//!   queue, byte-compatible with the C# app (win-profiles).
//! * [`mac::MacProfileStore`] (used on macOS; compiled everywhere for its tests): LibraryKit `profiles.json` v2 with the embedded
//!   encrypted vault, flock, folder watcher, encrypted thumbnail cache (mac-librarykit).
//!
//! Shared: [`crypto`] (PBKDF2-HMAC-SHA256 100 000 / 64 bytes, AES-256-GCM `nonce‖tag‖ct`, no AAD; golden vectors
//! from both specs) and [`fingerprint`] (`fp:<len>:<24 hex>`).
//!
//! Privacy (rule 6): logged out, every mutator is a no-op that touches no file and takes no lock, every read returns
//! empty/zero, and no file is fingerprinted. Logout/switch/delete drops and zeroizes the vault key and contents and
//! clears the fingerprint cache.

use std::path::{Path, PathBuf};

use sa_core::secrets::{SecretKey32, SecretString};
use sa_core::subscription::Subscription;

pub mod crypto;
pub mod fingerprint;

#[cfg(windows)]
pub mod win;

/// Compiled on every OS (pure Rust; platform calls are `cfg`-gated inside) so its tests run on Windows too. Only the
/// macOS build uses it.
pub mod mac;

/// UTC timestamp in .NET ticks (100 ns since 0001-01-01T00:00:00Z); both formats convert to/from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ticks(pub i64);

impl Ticks {
    /// Ticks at 1970-01-01T00:00:00Z.
    pub const UNIX_EPOCH: i64 = 621_355_968_000_000_000;
    pub const MIN: Ticks = Ticks(0);

    pub fn now() -> Self {
        let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        Ticks(Self::UNIX_EPOCH + (d.as_nanos() / 100) as i64)
    }
}

/// Feed filters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum VideoFeedFilter {
    #[default]
    All,
    History,
    Liked,
    MostWatched,
    TopRated,
    /// A custom list (`None`/blank id on Windows = the first list).
    CustomList(Option<String>),
}

/// How much file access a lookup may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FingerprintMode {
    /// Disk reads (background lanes only).
    #[default]
    Compute,
    /// One `stat` per file.
    CachedOnly,
    /// No file access (UI-thread first pass).
    PathOnly,
}

/// A custom list. `id` = GUID "N" (32 lowercase hex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserVideoList {
    pub id: String,
    pub name: String,
    /// Normalized paths, unique case-insensitively, ordered.
    pub video_paths: Vec<String>,
    pub created_at: Ticks,
    pub updated_at: Ticks,
}

/// Per-video engagement.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoUserStats {
    /// `fp:<len>:<24hex>` or None.
    pub content_fingerprint: Option<String>,
    pub video_path: String,
    pub file_name: String,
    pub directory_path: String,
    pub view_count: i64,
    pub is_liked: bool,
    /// 0..=5, 0 = none.
    pub rating: i64,
    pub last_position_seconds: f64,
    pub duration_seconds: f64,
    pub last_watched_at: Option<Ticks>,
    pub updated_at: Ticks,
    /// Not stored; filled by `stats`/`feed` (JPEG/PNG/BMP bytes).
    pub thumbnail: Option<Vec<u8>>,
}

impl VideoUserStats {
    pub fn has_watched(&self) -> bool {
        self.view_count > 0 || self.last_watched_at.is_some()
    }
    /// `clamp(pos / dur, 0, 1)` when both are finite and `dur > 0`, else 0.
    pub fn progress_ratio(&self) -> f64 {
        let (p, d) = (self.last_position_seconds, self.duration_seconds);
        if d > 0.0 && d.is_finite() && p.is_finite() { (p / d).clamp(0.0, 1.0) } else { 0.0 }
    }
}

/// Totals for the hub header (all zero logged out).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ProfileCounts {
    /// `view_count > 0`.
    pub watched: i64,
    /// Σ view_count.
    pub views: i64,
    pub liked: i64,
    /// `rating > 0`.
    pub rated: i64,
}

/// Store errors; each maps to a catalog key ([`ProfileError::message_key`]). Each platform produces the cases its
/// app produced.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProfileError {
    EmptyUsername,
    EmptyPassword,
    MissingCredentials,
    UsernameTaken,
    InvalidCredentials,
    VaultUnreadable,
    EmptyListName,
    DuplicateListName,
    LockUnavailable,
    WriteFailed { errno: i32 },
    CorruptFileNotPreserved,
    UnsupportedFileVersion(i64),
    FileUnreadable,
    CryptoFailure,
    ListNotFound,
}

impl ProfileError {
    /// The catalog key (Turkish source).
    pub const fn message_key(&self) -> &'static str {
        match self {
            ProfileError::EmptyUsername => "Lütfen bir kullanıcı adı girin.",
            ProfileError::EmptyPassword => "Lütfen bir şifre belirleyin.",
            ProfileError::MissingCredentials => "Kullanıcı adı ve şifre gereklidir.",
            ProfileError::UsernameTaken => "Bu kullanıcı adı zaten kayıtlı.",
            ProfileError::InvalidCredentials => "Kullanıcı adı veya şifre hatalı.",
            ProfileError::VaultUnreadable => {
                "Şifre doğru, ancak profilin şifreli verisi okunamadı. Dosya değiştirilmedi; başka bir sürümle açmayı deneyin."
            }
            ProfileError::EmptyListName => "Lütfen bir liste adı girin.",
            ProfileError::DuplicateListName => "Bu adda bir liste zaten var.",
            ProfileError::LockUnavailable => {
                "Profil dosyası şu an başka bir işlem tarafından kullanılıyor. Biraz sonra tekrar deneyin."
            }
            ProfileError::WriteFailed { .. } => "Profil dosyası yazılamadı (hata %lld).",
            ProfileError::CorruptFileNotPreserved => "Profil dosyası bozuk ve yedeği alınamadığı için üzerine yazılmadı.",
            ProfileError::UnsupportedFileVersion(_) => {
                "Profil dosyası uygulamanın daha yeni veya başka bir sürümü tarafından yazılmış (sürüm %lld); burada açılamaz ve değiştirilmeyecek."
            }
            ProfileError::FileUnreadable => "Profil dosyası okunamıyor; değişiklikler kaydedilmedi.",
            ProfileError::CryptoFailure => "Şifreleme işlemi başarısız oldu.",
            ProfileError::ListNotFound => "Bu liste artık yok.",
        }
    }

    /// Localized text in the current UI language.
    pub fn message(&self) -> String {
        match self {
            ProfileError::WriteFailed { errno } => sa_core::l!(self.message_key(), i64::from(*errno)),
            ProfileError::UnsupportedFileVersion(v) => sa_core::l!(self.message_key(), *v),
            _ => sa_core::l(self.message_key()),
        }
    }
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ProfileError {}

/// A store change worth a redraw. Progress-only, thumbnail-only and migration-only writes never raise `Local`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileChange {
    Local,
    /// Another instance/process changed the data (or a remembered login was restored).
    External,
}

/// Feed request.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FeedQuery {
    pub filter: VideoFeedFilter,
    pub search: Option<String>,
    /// Indexed paths merged into Keşfet (never populated logged out).
    pub discovered: Vec<PathBuf>,
    pub mode: FingerprintMode,
}

/// The common profile store (UNIFIED.md §5.1). Blocking methods are called on store lanes only; the getters marked
/// UI-safe touch memory (+ at most one `stat`).
pub trait ProfileStore: Send + Sync {
    // ----- session
    /// UI-safe.
    fn is_logged_in(&self) -> bool;
    /// Registered casing. UI-safe.
    fn current_username(&self) -> Option<String>;
    /// Platform sort (Windows CurrentCultureIgnoreCase; macOS localized case-insensitive).
    fn registered_usernames(&self) -> Vec<String>;
    fn last_username(&self) -> Option<String>;
    /// True only while a remembered session exists.
    fn remember_me(&self) -> bool;
    /// Bumps on login/logout/switch/delete/restore; snapshots built under an older generation are never applied.
    /// UI-safe (memory only on both platforms).
    ///
    /// Contract: every change of the active user's identity or vault key (what [`Self::active_vault_key`] returns)
    /// bumps it, and it never goes back. The encrypted video cache relies on this: an open stream keeps its captured
    /// key only while the generation is unchanged.
    fn session_generation(&self) -> u64;

    /// Lock-free: true from the start of an operation that may change the session (login, register, logout, delete;
    /// raised before it waits for the store lock) until its new generation is published. A consumer holding a
    /// captured key ([`Self::active_vault_key_in_session`]) does not use it meanwhile; if the operation then fails,
    /// the generation is unchanged and the consumer may use its key again. Default: never (test doubles).
    fn session_change_in_progress(&self) -> bool {
        false
    }

    /// Lock-free: the current generation, or `None` while a session change is in progress. The flag is read before
    /// the generation, so a change that starts and finishes between the two reads still shows up as a new
    /// generation. A per-read check compares this with the generation its key was captured under.
    fn settled_session_generation(&self) -> Option<u64> {
        if self.session_change_in_progress() {
            return None;
        }
        Some(self.session_generation())
    }
    /// First read + remembered restore (background lane).
    fn start(&self);
    fn register(&self, username: &str, password: &SecretString, remember: bool) -> Result<(), ProfileError>;
    fn login(&self, username: &str, password: &SecretString, remember: bool) -> Result<(), ProfileError>;
    /// Local logout always happens; clears remember-me for every instance; blocks restore for the rest of the run.
    fn logout(&self) -> Result<(), ProfileError>;
    fn delete_profile(&self, username: &str, password: &SecretString) -> Result<(), ProfileError>;

    // ----- engagement (logged out: None/false, no I/O)
    fn record_watch(&self, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats>;
    /// Never raises `Local`.
    fn update_playback_progress(&self, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats>;
    fn flush_playback_progress(&self) -> bool;
    fn set_liked(&self, path: &Path, liked: bool) -> Option<VideoUserStats>;
    fn set_rating(&self, path: &Path, stars: u8) -> Option<VideoUserStats>;
    fn stats(&self, path: &Path, mode: FingerprintMode) -> Option<VideoUserStats>;
    /// No I/O at all.
    fn stats_fast_in_memory(&self, path: &Path) -> Option<VideoUserStats>;
    fn clear_watch_history(&self) -> bool;
    fn thumbnail(&self, path: &Path) -> Option<Vec<u8>>;
    fn set_thumbnail(&self, path: &Path, bytes: &[u8]) -> bool;

    // ----- writes gated on a session generation captured earlier
    //
    // `session_generation()` is lock-free, so a value captured on the UI thread can be stale by the time a worker
    // writes (a login switch in between). These variants write only while the session is still `generation`,
    // compared under the store's own lock in the same critical section as the write, so nothing of one profile
    // lands in the next. The defaults (test doubles) check, then act; both real stores override them.

    /// [`Self::update_playback_progress`] for session `generation` only.
    fn update_playback_progress_in_session(&self, generation: u64, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        if self.session_generation() != generation {
            return None;
        }
        self.update_playback_progress(path, position, duration)
    }
    /// [`Self::record_watch`] for session `generation` only.
    fn record_watch_in_session(&self, generation: u64, path: &Path, position: f64, duration: f64) -> Option<VideoUserStats> {
        if self.session_generation() != generation {
            return None;
        }
        self.record_watch(path, position, duration)
    }
    /// [`Self::set_thumbnail`] for session `generation` only.
    fn set_thumbnail_in_session(&self, generation: u64, path: &Path, bytes: &[u8]) -> bool {
        if self.session_generation() != generation {
            return false;
        }
        self.set_thumbnail(path, bytes)
    }

    // ----- lists
    fn lists(&self) -> Vec<UserVideoList>;
    /// `Ok(None)` = Windows silent duplicate/blank; macOS returns errors.
    fn create_list(&self, name: &str) -> Result<Option<UserVideoList>, ProfileError>;
    fn rename_list(&self, id: &str, name: &str) -> Result<bool, ProfileError>;
    fn delete_list(&self, id: &str) -> bool;
    fn set_in_list(&self, id: &str, path: &Path, is_in: bool) -> bool;
    /// True = now in the list.
    fn toggle_in_list(&self, id: &str, path: &Path) -> bool;
    fn lists_containing(&self, path: &Path) -> Vec<String>;

    // ----- feed
    /// `[]` when logged out, for every filter.
    fn feed(&self, query: &FeedQuery, should_continue: &dyn Fn() -> bool) -> Vec<VideoUserStats>;
    fn totals(&self) -> ProfileCounts;
    /// Windows: re-key path-keyed rows to `fp:` keys when reachable (UNIFIED.md §4 item 7).
    fn persist_pending_migrations(&self) -> bool;
    /// Change notifications; other profiles' events are filtered out.
    fn subscribe_changes(&self, handler: Box<dyn Fn(ProfileChange) + Send + Sync>) -> Subscription;
    /// Windows VideoCache key derivation; macOS ThumbnailCache internal. Blocking (takes the store lock): call it when
    /// a consumer starts, never per read.
    fn active_vault_key(&self) -> Option<(String, SecretKey32)>;

    /// [`Self::active_vault_key`] together with the [`Self::session_generation`] it belongs to, read consistently:
    /// a long-lived consumer (a playback stream) captures the key once and afterwards only compares the lock-free
    /// generation, so it never takes the store lock again and still stops the moment the session changes. `None`
    /// logged out. The default (test doubles) brackets the key read with two generation reads and gives up when they
    /// differ (generations only grow); both real stores read the key and the generation under one lock.
    ///
    /// The default is only sound when the store publishes a new generation no earlier than the state change it
    /// describes (by the time the new generation is readable, `active_vault_key` must already return the new key).
    /// A generation published early could be paired with the old key; one published late only pairs the old
    /// generation with the new key, which the consumer's next check rejects (fails closed). Both real stores publish
    /// at the unlock of the critical section that made the change.
    fn active_vault_key_in_session(&self) -> Option<(u64, String, SecretKey32)> {
        let before = self.session_generation();
        let (username, key) = self.active_vault_key()?;
        (self.session_generation() == before).then_some((before, username, key))
    }
}
