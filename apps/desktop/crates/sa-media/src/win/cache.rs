//! `VideoCacheStore`: port of the current C# `EncryptedVideoCacheStore` (layout `v3`).
//!
//! `%LOCALAPPDATA%\SubtitleAdder\VideoCache\v3\<tag>.vcache`, one flat folder for every profile and video:
//!
//! * `profile_tag = HMAC-SHA256(vault_key, "SubtitleAdder.VideoCache.ProfileDir.v2")` as lowercase hex, first 32
//!   characters; `tag = HMAC(vault_key, "SubtitleAdder.VideoCache.ChunkFile.v3|{profile_tag}|{identity}|{index}")`
//!   likewise. Without the profile key nobody can tell which profile, video or position a file belongs to.
//! * File = `"SAVC0003"`, `i32 LE chunk_size`, 12-byte nonce, 16-byte tag, then the AES-256-GCM ciphertext of the
//!   chunk **padded to the full chunk size**; key `HMAC(vault_key, "SubtitleAdder.VideoCache.ChunkKey.v3|{profile_tag}|
//!   {identity}|{chunk_size}|{index}")`, AAD `"SAVC3|{profile_tag}|{identity}|{chunk_size}|{index}|{plaintext_len}"`
//!   (the real length is authenticated).
//! * Every file and the layout folder get the fixed time 2001-01-01T00:00:00Z; reads never touch the access time and
//!   there is no LRU touch. Eviction follows the (pseudo-random) file names.
//! * Nothing is cached while no profile is logged in ([`VideoCacheStore::try_begin_session`] is `None`), and a stream
//!   opened under one session never reads or writes under another: a [`CacheSession`] ends (its key copy zeroed) as
//!   soon as its profile is no longer the active, unlocked one.
//! * Everything else under the cache folder is a legacy layout (unkeyed v1 / keyed v2 folders, anonymous chunks) and
//!   is purged once at startup ([`VideoCacheStore::purge_legacy_layout`]).

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::AeadInOut;
use aes_gcm::{Aes256Gcm, KeyInit};
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use sa_core::secrets::SecretKey32;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

use super::fsio;
use super::paths;
use crate::text::{eq_ordinal_ignore_case, upper_invariant};

/// Supplies the active profile's vault key (`ProfileStore::active_vault_key`: username as stored and a copy of the
/// 32-byte key), `None` logged out or while the key is not unlocked in this session. A store built with
/// [`VideoCacheStore::new`] calls it for every chunk read and write; the app builds the store with
/// [`VideoCacheStore::with_session_keys`] instead, which calls its key source only when a stream opens.
pub type VaultKeyProvider = Arc<dyn Fn() -> Option<(String, SecretKey32)> + Send + Sync>;

/// The active profile's vault key with the session generation it belongs to, read consistently
/// (`ProfileStore::active_vault_key_in_session`); `None` logged out. May block (the profile store lock).
pub type SessionKeyProvider = Arc<dyn Fn() -> Option<(u64, String, SecretKey32)> + Send + Sync>;

/// The current session generation, or `None` while a session change is in progress
/// (`ProfileStore::settled_session_generation`). Must never block: it runs on every chunk read of a playback stream
/// (the demux thread). A `None` ends the stream's cache session like a new generation does.
pub type SessionGenerationProvider = Arc<dyn Fn() -> Option<u64> + Send + Sync>;

/// Where a store gets its key from.
enum KeySource {
    /// Looked up for every chunk (tests and tools whose provider takes no lock).
    PerChunk(VaultKeyProvider),
    /// Captured once per stream with its session generation; per chunk only the lock-free generation is compared.
    Session { keys: SessionKeyProvider, generation: SessionGenerationProvider },
}

impl KeySource {
    /// The current owner, as a full (possibly blocking) lookup: `(generation, username, key)`.
    fn lookup(&self) -> Option<(Option<u64>, String, SecretKey32)> {
        match self {
            KeySource::PerChunk(keys) => keys().map(|(u, k)| (None, u, k)),
            KeySource::Session { keys, .. } => keys().map(|(g, u, k)| (Some(g), u, k)),
        }
    }
}

const GIB: i64 = 1024 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"SAVC0003";

/// One cache owner captured when a stream opens: the profile, a copy of its key and (with a session key source) the
/// session generation it was read under. Ended (key zeroed) as soon as that profile is no longer the active, unlocked
/// profile; it never becomes current again.
pub struct CacheSession {
    username: String,
    profile_tag: String,
    generation: Option<u64>,
    gate: Mutex<SessionKey>,
}

struct SessionKey {
    ended: bool,
    key: SecretKey32,
}

impl CacheSession {
    /// True once the session ended.
    pub fn is_ended(&self) -> bool {
        self.gate.lock().ended
    }

    fn end_locked(state: &mut SessionKey) {
        if !state.ended {
            state.ended = true;
            // The old key is zeroized when it is dropped.
            state.key = SecretKey32::new([0u8; 32]);
        }
    }

    /// Ends the session now (idempotent).
    pub fn end(&self) {
        Self::end_locked(&mut self.gate.lock());
    }
}

impl std::fmt::Debug for CacheSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheSession").field("ended", &self.is_ended()).finish_non_exhaustive()
    }
}

/// Everything one chunk read or write needs, derived under the session gate; the I/O runs without it.
struct ChunkMaterial {
    path: PathBuf,
    key: Zeroizing<[u8; 32]>,
    aad: Vec<u8>,
}

/// The encrypted SSD chunk cache.
pub struct VideoCacheStore {
    cache_dir: PathBuf,
    keys: KeySource,
    max_bytes: AtomicI64,
    writes_since_quota_check: AtomicU64,
    quota_lock: Mutex<()>,
    sessions: Mutex<Vec<Weak<CacheSession>>>,
    /// True while the session watcher thread runs (see [`VideoCacheStore::SESSION_WATCH_INTERVAL`]).
    watching: AtomicBool,
    me: Weak<VideoCacheStore>,
}

fn keyed_tag(key: &[u8; 32], input: &str) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(input.as_bytes());
    let digest = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(32);
    for b in &digest[..16] {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

fn hmac32(key: &[u8; 32], input: &str) -> Zeroizing<[u8; 32]> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(input.as_bytes());
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// The fixed time of every cache file and folder (2001-01-01T00:00:00Z).
pub fn fixed_file_time() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(978_307_200)
}

impl VideoCacheStore {
    pub const DEFAULT_MAX_GB: i64 = 20;
    pub const MIN_MAX_GB: i64 = 1;
    pub const MAX_MAX_GB: i64 = 500;
    /// Subfolder holding the current layout; everything else under the cache folder is legacy.
    pub const LAYOUT_FOLDER: &'static str = "v3";
    /// 8 magic + 4 padded length + 12 nonce + 16 tag.
    pub const HEADER_SIZE: i64 = 40;
    /// Setter floor of the byte limit.
    pub const MIN_MAX_BYTES: i64 = 64 * 1024;
    /// Folder of the old C# logged-out ("anonymous") chunks; part of the legacy layout that
    /// [`Self::purge_legacy_layout`] deletes.
    pub const ANONYMOUS_FOLDER: &'static str = "06b631aa99fabdf205dd7684223ae0d6";
    /// While any handed-out session is live, a watcher ends stale ones this often. The C# store ends them on
    /// `LocalProfileManager.StateChanged`; the watcher gives the same guarantee (an open, paused stream does not keep
    /// a logged-out profile's key copy in memory) without depending on the app forwarding every profile change.
    /// [`Self::end_stale_sessions`] still ends them at once when called.
    pub const SESSION_WATCH_INTERVAL: Duration = Duration::from_millis(250);

    /// `cache_dir` = `%LOCALAPPDATA%\SubtitleAdder\VideoCache` in production. `max_bytes` overrides the 20 GiB default
    /// when > 0. Purges the legacy layout once, in the background.
    /// A blank `cache_dir` is [`Self::default_directory`] (C# `IsNullOrWhiteSpace(cacheDirectory)`).
    ///
    /// `keys` is called for every chunk read and write (and by the session watcher), so it must not block; the app
    /// uses [`Self::with_session_keys`].
    pub fn new(cache_dir: PathBuf, keys: VaultKeyProvider, max_bytes: Option<i64>) -> Arc<Self> {
        Self::build(cache_dir, KeySource::PerChunk(keys), max_bytes)
    }

    /// Like [`Self::new`], for a key source that may block (the profile store lock): `keys` runs only when a stream
    /// opens (and on the session watcher thread); a chunk read or write compares the lock-free `generation` with the
    /// one its session was opened under, so the player's demux thread never waits for the profile store, and a
    /// logout or user switch ends an open stream's session at its next chunk — already when the change begins
    /// (`generation` is `None` while it is in progress). A change the profile store only learns about by re-reading
    /// its file (another in-process store of the same file, another instance without a broker event) is noticed by
    /// the session watcher's full lookup within [`Self::SESSION_WATCH_INTERVAL`], not at the next chunk.
    pub fn with_session_keys(cache_dir: PathBuf, keys: SessionKeyProvider, generation: SessionGenerationProvider, max_bytes: Option<i64>) -> Arc<Self> {
        Self::build(cache_dir, KeySource::Session { keys, generation }, max_bytes)
    }

    fn build(cache_dir: PathBuf, keys: KeySource, max_bytes: Option<i64>) -> Arc<Self> {
        let cache_dir = if cache_dir.as_os_str().to_string_lossy().trim().is_empty() {
            Self::default_directory()
        } else {
            std::path::absolute(&cache_dir).unwrap_or(cache_dir)
        };
        let store = Arc::new_cyclic(|me| Self {
            cache_dir,
            keys,
            max_bytes: AtomicI64::new(max_bytes.filter(|m| *m > 0).unwrap_or(Self::DEFAULT_MAX_GB * GIB)),
            writes_since_quota_check: AtomicU64::new(0),
            quota_lock: Mutex::new(()),
            sessions: Mutex::new(Vec::new()),
            watching: AtomicBool::new(false),
            me: me.clone(),
        });
        let weak = Arc::downgrade(&store);
        let _ = std::thread::Builder::new().name("video-cache-purge".into()).spawn(move || {
            if let Some(store) = weak.upgrade() {
                store.purge_legacy_layout();
            }
        });
        store
    }

    /// `%LOCALAPPDATA%\SubtitleAdder\VideoCache`.
    pub fn default_directory() -> PathBuf {
        fsio::local_app_data().unwrap_or_default().join("SubtitleAdder").join("VideoCache")
    }

    pub fn cache_directory(&self) -> &Path {
        &self.cache_dir
    }

    fn layout_dir(&self) -> PathBuf {
        self.cache_dir.join(Self::LAYOUT_FOLDER)
    }

    pub fn max_cache_size_bytes(&self) -> i64 {
        self.max_bytes.load(Ordering::Acquire)
    }

    /// Stores `max(64 KiB, bytes)` at once; quota enforcement runs on a background thread.
    pub fn set_max_cache_size_bytes(&self, bytes: i64) {
        let clamped = bytes.max(Self::MIN_MAX_BYTES);
        self.max_bytes.store(clamped, Ordering::Release);
        let me = self.me.clone();
        let _ = std::thread::Builder::new().name("video-cache-quota".into()).spawn(move || {
            if let Some(store) = me.upgrade() {
                store.enforce_quota(Some(clamped));
            }
        });
    }

    /// `clamp(max_bytes / GiB, 1, 500)`.
    pub fn max_cache_size_gb(&self) -> i64 {
        (self.max_cache_size_bytes() / GIB).clamp(Self::MIN_MAX_GB, Self::MAX_MAX_GB)
    }

    /// Stores the limit at once; eviction runs on a background thread.
    pub fn set_max_cache_size_gb(&self, gb: i64) {
        self.set_max_cache_size_bytes(gb.clamp(Self::MIN_MAX_GB, Self::MAX_MAX_GB) * GIB);
    }

    /// `"{UPPER(normalize_video_path(path))}|len:{len}|cs:{chunk}"`, or `"stream|len:…|cs:…"` without a path. No
    /// I/O. It is never written to disk as is (file names are keyed hashes of it). The identity ignores mtime (C#).
    pub fn compute_video_identity(path: Option<&str>, len: i64, chunk_size: i32) -> String {
        if let Some(p) = path.filter(|p| !p.trim().is_empty()) {
            let normalized = paths::normalize_video_path(p);
            if !normalized.trim().is_empty() {
                return format!("{}|len:{len}|cs:{chunk_size}", upper_invariant(&normalized));
            }
        }
        format!("stream|len:{len}|cs:{chunk_size}")
    }

    // ---- sessions ----------------------------------------------------------------------------------------------

    /// Captures the active profile as the owner of a playback stream's cache; `None` when no profile is logged in
    /// (logged-out playback is never cached on disk).
    pub fn try_begin_session(&self) -> Option<Arc<CacheSession>> {
        let (generation, username, key) = self.keys.lookup()?;
        if username.trim().is_empty() {
            return None;
        }
        // A session change under way (or finished since the lookup) would end the session at its first chunk.
        if let KeySource::Session { generation: current, .. } = &self.keys
            && current() != generation
        {
            return None;
        }
        let session = Arc::new(CacheSession {
            username: username.trim().to_owned(),
            profile_tag: keyed_tag(key.expose(), "SubtitleAdder.VideoCache.ProfileDir.v2"),
            generation,
            gate: Mutex::new(SessionKey { ended: false, key }),
        });
        {
            let mut sessions = self.sessions.lock();
            sessions.retain(|w| w.upgrade().is_some_and(|s| !s.is_ended()));
            sessions.push(Arc::downgrade(&session));
        }
        self.ensure_session_watcher();
        Some(session)
    }

    /// Starts the watcher unless it runs; it exits once no live session is left (and never keeps the store alive).
    fn ensure_session_watcher(&self) {
        if self.watching.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }
        let me = self.me.clone();
        let spawned = std::thread::Builder::new().name("video-cache-sessions".into()).spawn(move || {
            loop {
                std::thread::sleep(Self::SESSION_WATCH_INTERVAL);
                let Some(store) = me.upgrade() else { return };
                store.end_stale_sessions();
                if !store.sessions.lock().is_empty() {
                    continue;
                }
                store.watching.store(false, Ordering::Release);
                // A session handed out between the check and the reset restarts the loop here.
                let restarted = !store.sessions.lock().is_empty()
                    && store.watching.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok();
                if !restarted {
                    return;
                }
            }
        });
        if spawned.is_err() {
            self.watching.store(false, Ordering::Release);
        }
    }

    /// The lock-free part of a currency check: with a session key source, the generation the session was opened
    /// under must still be the current one (always true for a per-chunk source).
    fn generation_current(&self, session: &CacheSession) -> bool {
        match &self.keys {
            KeySource::PerChunk(_) => true,
            KeySource::Session { generation, .. } => session.generation.is_some() && generation() == session.generation,
        }
    }

    /// The full check against an owner looked up beforehand (never under the gate: the lookup may block).
    fn matches_owner(&self, session: &CacheSession, state: &SessionKey, owner: Option<&(Option<u64>, String, SecretKey32)>) -> bool {
        let Some((generation, username, key)) = owner else { return false };
        !state.ended
            && self.generation_current(session)
            && (generation.is_none() || *generation == session.generation)
            && eq_ordinal_ignore_case(username.trim(), &session.username)
            && bool::from(key.expose().ct_eq(state.key.expose()))
    }

    /// True while `session`'s profile is still the active, unlocked profile (a full lookup; may block on the
    /// profile store, so not for the read path).
    pub fn is_session_current(&self, session: &CacheSession) -> bool {
        let owner = self.keys.lookup();
        let state = session.gate.lock();
        self.matches_owner(session, &state, owner.as_ref())
    }

    /// Ends every handed-out session whose profile is no longer the active, unlocked one (C# `EndStaleSessions`, run
    /// on every `StateChanged`). The session watcher calls it while sessions are live; the app may call it on a
    /// profile state change to end them at once. The owner is looked up once, before any session gate is taken, so
    /// a slow profile store never holds up a stream's chunk read through the gate.
    pub fn end_stale_sessions(&self) {
        let live: Vec<Arc<CacheSession>> = {
            let mut sessions = self.sessions.lock();
            sessions.retain(|w| w.upgrade().is_some_and(|s| !s.is_ended()));
            sessions.iter().filter_map(Weak::upgrade).collect()
        };
        if live.is_empty() {
            return;
        }
        let owner = self.keys.lookup();
        for session in live {
            let mut state = session.gate.lock();
            if !self.matches_owner(&session, &state, owner.as_ref()) {
                CacheSession::end_locked(&mut state);
            }
        }
    }

    /// Runs `work` with the session's key under its gate; a stale session is ended. File I/O stays out of `work`.
    ///
    /// The per-chunk path: with a session key source it takes no profile store lock at all (only the session's own
    /// gate and the lock-free generation); a per-chunk source is looked up before the gate is taken.
    fn with_current_session<T>(&self, session: Option<&CacheSession>, work: impl FnOnce(&CacheSession, &[u8; 32]) -> T) -> Option<T> {
        let session = session?;
        let owner = match &self.keys {
            KeySource::PerChunk(_) => Some(self.keys.lookup()),
            KeySource::Session { .. } => None,
        };
        let mut state = session.gate.lock();
        if state.ended {
            return None;
        }
        let current = match &owner {
            Some(owner) => self.matches_owner(session, &state, owner.as_ref()),
            None => self.generation_current(session),
        };
        if !current {
            CacheSession::end_locked(&mut state);
            return None;
        }
        Some(work(session, state.key.expose()))
    }

    fn chunk_path(&self, session: &CacheSession, key: &[u8; 32], identity: &str, index: i64) -> PathBuf {
        let tag = keyed_tag(key, &format!("SubtitleAdder.VideoCache.ChunkFile.v3|{}|{identity}|{index}", session.profile_tag));
        self.layout_dir().join(format!("{tag}.vcache"))
    }

    fn derive_material(&self, session: Option<&CacheSession>, identity: &str, chunk_size: i32, index: i64, plaintext_len: usize) -> Option<ChunkMaterial> {
        self.with_current_session(session, |s, key| ChunkMaterial {
            path: self.chunk_path(s, key, identity, index),
            key: hmac32(key, &format!("SubtitleAdder.VideoCache.ChunkKey.v3|{}|{identity}|{chunk_size}|{index}", s.profile_tag)),
            aad: format!("SAVC3|{}|{identity}|{chunk_size}|{index}|{plaintext_len}", s.profile_tag).into_bytes(),
        })
    }

    // ---- chunks ------------------------------------------------------------------------------------------------

    /// Chunk indices `0..total_chunks` cached for the active profile; empty while logged out.
    /// Blocking: begins a session, i.e. takes the profile store lock with a session key source. For tests and
    /// tools, never the read path (streams use the `*_for` variants with their own session).
    pub fn cached_chunk_indices(&self, id: &str, total_chunks: i64, chunk_size: i32, total_len: i64) -> HashSet<i64> {
        let session = self.try_begin_session();
        self.cached_chunk_indices_for(session.as_deref(), id, total_chunks, chunk_size, total_len)
    }

    /// Chunk indices cached for `session`; empty once it ended. One attribute lookup per file (no listing).
    pub fn cached_chunk_indices_for(&self, session: Option<&CacheSession>, id: &str, total_chunks: i64, chunk_size: i32, total_len: i64) -> HashSet<i64> {
        if id.trim().is_empty() || total_chunks <= 0 || chunk_size <= 0 || total_len <= 0 {
            return HashSet::new();
        }
        let Some(paths) = self.with_current_session(session, |s, key| {
            (0..total_chunks).map(|i| self.chunk_path(s, key, id, i)).collect::<Vec<_>>()
        }) else {
            return HashSet::new();
        };
        let expected = (Self::HEADER_SIZE + i64::from(chunk_size)) as u64;
        paths
            .iter()
            .enumerate()
            // `FileInfo.Exists && Length`: the entry itself, a link is not followed (its own length never matches).
            .filter(|(_, p)| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file() && m.len() == expected))
            .map(|(i, _)| i as i64)
            .collect()
    }

    /// Reads and decrypts a chunk for the active profile; `None` logged out, on a miss, a foreign or damaged file.
    /// Blocking: begins a session, i.e. takes the profile store lock with a session key source. For tests and
    /// tools, never the read path (streams use the `*_for` variants with their own session).
    pub fn try_read_chunk(&self, id: &str, chunk_size: i32, index: i64, expected_len: i32) -> Option<Vec<u8>> {
        let session = self.try_begin_session();
        self.try_read_chunk_for(session.as_deref(), id, chunk_size, index, expected_len)
    }

    /// Reads a chunk cached for `session`; `None` once it ended. A file that fails authentication is deleted.
    pub fn try_read_chunk_for(&self, session: Option<&CacheSession>, id: &str, chunk_size: i32, index: i64, expected_len: i32) -> Option<Vec<u8>> {
        if id.trim().is_empty() || expected_len <= 0 || index < 0 || chunk_size <= 0 || expected_len > chunk_size {
            return None;
        }
        let material = self.derive_material(session, id, chunk_size, index, expected_len as usize)?;
        if !paths::file_exists(&material.path) {
            return None;
        }
        let total = Self::HEADER_SIZE as usize + chunk_size as usize;
        let mut packet = vec![0u8; total];
        {
            let mut f = fsio::open_for_read_without_access_time(&material.path).ok()?;
            if f.metadata().ok()?.len() != total as u64 {
                return None;
            }
            let mut read = 0;
            while read < total {
                match f.read(&mut packet[read..]) {
                    Ok(0) => break,
                    Ok(n) => read += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => return None,
                }
            }
            if read != total {
                return None;
            }
        }
        if &packet[..8] != MAGIC || i32::from_le_bytes([packet[8], packet[9], packet[10], packet[11]]) != chunk_size {
            return None;
        }
        let nonce: [u8; 12] = packet[12..24].try_into().ok()?;
        let tag: [u8; 16] = packet[24..40].try_into().ok()?;
        let mut body = packet.split_off(Self::HEADER_SIZE as usize);
        let cipher = Aes256Gcm::new(&(*material.key).into());
        if cipher.decrypt_inout_detached(&nonce.into(), &material.aad, body.as_mut_slice().into(), &tag.into()).is_err() {
            // Authentication failure (another profile's key, damage): the file is useless.
            body.zeroize();
            let _ = std::fs::remove_file(&material.path);
            return None;
        }
        // No "last used" touch: the file time would record when the video was played.
        body.truncate(expected_len as usize);
        Some(body)
    }

    /// Encrypts and writes a chunk for the active profile; `false` (nothing written) while logged out.
    /// Blocking: begins a session, i.e. takes the profile store lock with a session key source. For tests and
    /// tools, never the read path (streams use the `*_for` variants with their own session).
    pub fn try_write_chunk(&self, id: &str, chunk_size: i32, index: i64, plaintext: &[u8]) -> bool {
        let session = self.try_begin_session();
        self.try_write_chunk_for(session.as_deref(), id, chunk_size, index, plaintext)
    }

    /// Writes a chunk for `session`; refused once it ended (checked right before the write, so a chunk read under
    /// one session never lands in the next one).
    pub fn try_write_chunk_for(&self, session: Option<&CacheSession>, id: &str, chunk_size: i32, index: i64, plaintext: &[u8]) -> bool {
        if id.trim().is_empty() || plaintext.is_empty() || index < 0 || chunk_size <= 0 || plaintext.len() > chunk_size as usize {
            return false;
        }
        let max = self.max_cache_size_bytes();
        if Self::HEADER_SIZE + i64::from(chunk_size) > max {
            return false;
        }
        let Some(material) = self.derive_material(session, id, chunk_size, index, plaintext.len()) else { return false };
        self.write_chunk_core(&material, chunk_size as usize, plaintext, max)
    }

    fn write_chunk_core(&self, material: &ChunkMaterial, chunk_size: usize, plaintext: &[u8], max: i64) -> bool {
        let layout = self.layout_dir();
        if std::fs::create_dir_all(&layout).is_err() {
            return false;
        }
        // Padded to the full chunk size: a short last chunk would confirm a candidate file by its length.
        let header = Self::HEADER_SIZE as usize;
        let mut packet = vec![0u8; header + chunk_size];
        packet[..8].copy_from_slice(MAGIC);
        packet[8..12].copy_from_slice(&(chunk_size as i32).to_le_bytes());
        let mut nonce = [0u8; 12];
        if getrandom::fill(&mut nonce).is_err() {
            return false;
        }
        packet[12..24].copy_from_slice(&nonce);
        packet[header..header + plaintext.len()].copy_from_slice(plaintext);
        let cipher = Aes256Gcm::new(&(*material.key).into());
        let Ok(tag) = cipher.encrypt_inout_detached(&nonce.into(), &material.aad, (&mut packet[header..]).into()) else {
            packet.zeroize();
            return false;
        };
        packet[24..40].copy_from_slice(&tag);

        let temp = layout.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
        let written = fsio::write_through(&temp, &packet).and_then(|_| std::fs::rename(&temp, &material.path));
        if temp.exists() {
            let _ = std::fs::remove_file(&temp);
        }
        if written.is_err() {
            return false;
        }
        fsio::set_fixed_times(&material.path, false, fixed_file_time());
        fsio::set_fixed_times(&layout, true, fixed_file_time());
        if self.writes_since_quota_check.fetch_add(1, Ordering::AcqRel) % 4 == 0 {
            self.enforce_quota(Some(max));
        }
        true
    }

    // ---- maintenance -------------------------------------------------------------------------------------------

    /// Bytes of every `*.vcache` file below the cache folder (all profiles, legacy layouts included).
    pub fn total_used_bytes(&self) -> i64 {
        if !self.cache_dir.is_dir() {
            return 0;
        }
        let mut files = Vec::new();
        fsio::walk_files(&self.cache_dir, Some(".vcache"), &mut files);
        files.iter().map(|(_, m)| m.len() as i64).sum()
    }

    /// Deletes every file below the cache folder, then every top-level folder (all profiles). Errors are ignored.
    pub fn clear_all(&self) {
        let _quota = self.quota_lock.lock();
        if !self.cache_dir.is_dir() {
            return;
        }
        let mut files = Vec::new();
        fsio::walk_files(&self.cache_dir, None, &mut files);
        for (p, _) in files {
            let _ = std::fs::remove_file(p);
        }
        for dir in fsio::top_level_entries(&self.cache_dir).0 {
            fsio::delete_directory_recursive(&dir);
        }
    }

    /// Deletes everything below the cache folder that is not the `v3` layout: folders named after hashes of profile
    /// names and video paths (unkeyed v1, keyed v2), unpadded last chunks, offset-numbered chunk files and the chunks
    /// old versions cached for logged-out playback (the anonymous folder).
    pub fn purge_legacy_layout(&self) {
        let _quota = self.quota_lock.lock();
        if !self.cache_dir.is_dir() {
            return;
        }
        let (dirs, files) = fsio::top_level_entries(&self.cache_dir);
        for dir in dirs {
            if dir.file_name().is_some_and(|n| eq_ordinal_ignore_case(&n.to_string_lossy(), Self::LAYOUT_FOLDER)) {
                continue;
            }
            fsio::delete_directory_recursive(&dir);
        }
        for file in files {
            let _ = std::fs::remove_file(file);
        }
    }

    /// Startup (rule 6): kept for API compatibility; the anonymous folder is part of the legacy layout, so this runs
    /// [`Self::purge_legacy_layout`].
    pub fn delete_anonymous_folder(&self) {
        self.purge_legacy_layout();
    }

    /// Deletes chunk files (ordinal order of their keyed names) until the total is at or below the limit.
    pub fn enforce_quota(&self, max_bytes: Option<i64>) {
        let limit = max_bytes.unwrap_or_else(|| self.max_cache_size_bytes());
        let _quota = self.quota_lock.lock();
        if !self.cache_dir.is_dir() {
            return;
        }
        let mut files = Vec::new();
        fsio::walk_files(&self.cache_dir, Some(".vcache"), &mut files);
        let mut total: i64 = files.iter().map(|(_, m)| m.len() as i64).sum();
        if total <= limit {
            return;
        }
        files.sort_by(|(a, _), (b, _)| {
            let a = a.file_name().map(|n| n.to_string_lossy().encode_utf16().collect::<Vec<_>>()).unwrap_or_default();
            let b = b.file_name().map(|n| n.to_string_lossy().encode_utf16().collect::<Vec<_>>()).unwrap_or_default();
            a.cmp(&b)
        });
        for (path, meta) in files {
            if total <= limit {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= meta.len() as i64;
            }
        }
    }
}
