//! `BufferedVideoStream`: port of the C# stream of the same name (win-media §3).
//!
//! Small random reads of the demuxer are served from large sequential chunk reads of the source, a RAM window of
//! recently used chunks, a few chunks of read-ahead and the encrypted SSD cache ([`VideoCacheStore`]), so seeks and
//! reopening need no reads from the source disk. The SSD cache belongs to the session that was active when the
//! stream opened; logged out, nothing is cached on disk (RAM read-ahead only).
//!
//! Locks: `io` (the source stream) before `cache` (RAM chunks + persisted set); no I/O under `cache`.
//!
//! The stream is shared between the player's I/O thread and the UI (buffer bar): it implements `Read`/`Seek` both on
//! the value and on `&BufferedVideoStream`, so an `Arc<BufferedVideoStream>` can feed an FFmpeg AVIO context while
//! the UI polls [`BufferedVideoStream::buffered_ratio`]. Dropping it drains pending SSD writes synchronously; the player
//! drops it off the UI thread.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};

use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;

use super::cache::{CacheSession, VideoCacheStore};
use super::fsio::FILE_FLAG_SEQUENTIAL_SCAN;

/// `Io::pos` when the source position is not known (never a chunk start).
const UNKNOWN_POSITION: u64 = u64::MAX;

/// A readable, seekable byte source.
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

#[derive(Clone)]
struct Chunk {
    data: Arc<[u8]>,
    len: usize,
    tick: u64,
}

struct Io {
    inner: Option<Box<dyn ReadSeek>>,
    pos: u64,
}

#[derive(Default)]
struct CacheState {
    chunks: HashMap<i64, Chunk>,
    persisted: HashSet<i64>,
}

struct Inner {
    io: Mutex<Io>,
    cache_state: Mutex<CacheState>,
    pending: Mutex<VecDeque<(i64, Arc<[u8]>, usize)>>,
    length: i64,
    chunk_size: i32,
    max_cached: usize,
    read_ahead: i64,
    total_chunks: i64,
    store: Option<Arc<VideoCacheStore>>,
    session: Option<Arc<CacheSession>>,
    identity: Option<String>,
    owns: bool,
    async_persistence: bool,
    file_path: Option<String>,
    position: AtomicI64,
    last_read_chunk: AtomicI64,
    access_counter: AtomicU64,
    underlying_read_count: AtomicI32,
    underlying_bytes_read: AtomicI64,
    encrypted_cache_read_count: AtomicI32,
    foreground_waiters: AtomicI32,
    target_read_ahead: AtomicI64,
    read_ahead_running: AtomicBool,
    ssd_writer_running: AtomicBool,
    disposed: AtomicBool,
    cancel: CancellationToken,
}

/// Read-ahead stream over a video file (win-media §3); `Read + Seek` for the FFmpeg AVIO bridge.
pub struct BufferedVideoStream {
    inner: Arc<Inner>,
}

/// Decrements the foreground waiter count on drop.
struct WaiterGuard<'a>(&'a AtomicI32);

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl BufferedVideoStream {
    /// Default burst chunk size (8 MiB); part of the cache identity (`cs:`), so it must stay the C# value.
    pub const DEFAULT_CHUNK: i32 = 8 * 1024 * 1024;
    /// Default RAM window (32 chunks = 256 MiB).
    pub const DEFAULT_MAX_CACHED_CHUNKS: i32 = 32;
    /// Default read-ahead (4 chunks).
    pub const DEFAULT_READ_AHEAD_CHUNKS: i32 = 4;
    pub const MIN_CHUNK: i32 = 64 * 1024;

    /// Opens `path` read-only (share Read|Write|Delete, sequential-scan hint, no user-space buffering); owns the file,
    /// persists chunks in the background and loads the persisted chunk set in the background.
    pub fn open_file(path: &Path, chunk: i32, max_cached: i32, read_ahead: i32, cache: Option<Arc<VideoCacheStore>>) -> std::io::Result<Self> {
        let file = OpenOptions::new().read(true).share_mode(0x7).custom_flags(FILE_FLAG_SEQUENTIAL_SCAN).open(path)?;
        Self::from_stream(Box::new(file), true, chunk, max_cached, read_ahead, cache, None, Some(path.to_string_lossy().into_owned()))
    }

    /// `owns = true` closes `inner` on dispose and switches SSD persistence and the persisted-set load to background
    /// threads; `owns = false` (tests) does both synchronously.
    #[allow(clippy::too_many_arguments)]
    pub fn from_stream(
        mut inner: Box<dyn ReadSeek>,
        owns: bool,
        chunk: i32,
        max_cached: i32,
        read_ahead: i32,
        cache: Option<Arc<VideoCacheStore>>,
        video_identity: Option<String>,
        file_path: Option<String>,
    ) -> std::io::Result<Self> {
        let pos = inner.stream_position()?;
        let length = inner.seek(SeekFrom::End(0))?;
        inner.seek(SeekFrom::Start(pos))?;
        let length = i64::try_from(length).unwrap_or(i64::MAX);
        let chunk_size = chunk.max(Self::MIN_CHUNK);
        let max_cached = max_cached.max(2);
        let read_ahead = read_ahead.clamp(0, max_cached - 1);
        let total_chunks = if length <= 0 { 0 } else { (length - 1) / i64::from(chunk_size) + 1 };
        // The SSD cache belongs to the profile active now; logged out nothing is cached on disk.
        let session = cache.as_ref().and_then(|c| c.try_begin_session());
        let store = if session.is_some() { cache } else { None };
        let identity = match (&store, video_identity.filter(|v| !v.trim().is_empty())) {
            (Some(_), Some(id)) => Some(id),
            (Some(_), None) => file_path
                .as_deref()
                .filter(|p| !p.trim().is_empty())
                .map(|p| VideoCacheStore::compute_video_identity(Some(p), length, chunk_size)),
            (None, _) => None,
        };
        let inner = Arc::new(Inner {
            io: Mutex::new(Io { inner: Some(inner), pos }),
            cache_state: Mutex::new(CacheState::default()),
            pending: Mutex::new(VecDeque::new()),
            length,
            chunk_size,
            max_cached: max_cached as usize,
            read_ahead: i64::from(read_ahead),
            total_chunks,
            store,
            session,
            identity,
            owns,
            async_persistence: owns,
            file_path,
            position: AtomicI64::new(0),
            last_read_chunk: AtomicI64::new(-1),
            access_counter: AtomicU64::new(0),
            underlying_read_count: AtomicI32::new(0),
            underlying_bytes_read: AtomicI64::new(0),
            encrypted_cache_read_count: AtomicI32::new(0),
            foreground_waiters: AtomicI32::new(0),
            target_read_ahead: AtomicI64::new(-1),
            read_ahead_running: AtomicBool::new(false),
            ssd_writer_running: AtomicBool::new(false),
            disposed: AtomicBool::new(false),
            cancel: CancellationToken::new(),
        });
        if inner.store.is_some() && inner.identity.is_some() && inner.total_chunks > 0 {
            if inner.async_persistence {
                let worker = Arc::clone(&inner);
                let _ = std::thread::Builder::new().name("vcache-index".into()).spawn(move || worker.load_persisted());
            } else {
                inner.load_persisted();
            }
        }
        Ok(Self { inner })
    }

    pub fn length(&self) -> i64 {
        self.inner.length
    }

    pub fn file_path(&self) -> Option<&str> {
        self.inner.file_path.as_deref()
    }

    pub fn chunk_size_bytes(&self) -> i32 {
        self.inner.chunk_size
    }

    /// Burst reads of the source.
    pub fn underlying_read_count(&self) -> i32 {
        self.inner.underlying_read_count.load(Ordering::Acquire)
    }

    pub fn underlying_bytes_read(&self) -> i64 {
        self.inner.underlying_bytes_read.load(Ordering::Acquire)
    }

    /// Chunks served from the encrypted SSD cache.
    pub fn encrypted_cache_read_count(&self) -> i32 {
        self.inner.encrypted_cache_read_count.load(Ordering::Acquire)
    }

    /// Chunks resident in RAM.
    pub fn cached_chunk_count(&self) -> usize {
        self.inner.cache_state.lock().chunks.len()
    }

    /// Current position.
    pub fn position(&self) -> u64 {
        self.inner.position.load(Ordering::Acquire).max(0) as u64
    }

    /// Buffer bar value (win-media §3.7): the share of the file buffered in RAM or the SSD cache at and ahead of
    /// `playback_ratio`.
    pub fn buffered_ratio(&self, playback_ratio: f64) -> f64 {
        self.inner.buffered_ratio(playback_ratio)
    }

    /// Writes the pending SSD chunks now (C# `Flush`).
    pub fn flush_pending(&self) {
        self.inner.drain_pending();
    }

    /// Reads at the current position (shared access: the read-ahead worker and the UI use the same stream).
    pub fn read_shared(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }

    /// Seeks (pure, no I/O). Seeking past the end is allowed; a negative target is an error.
    pub fn seek_shared(&self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }

    /// True when every chunk overlapping `[pos, pos + len)` is in RAM (C# `ReadAsync` completes synchronously then).
    pub fn is_range_in_ram(&self, len: usize) -> bool {
        let pos = self.inner.position.load(Ordering::Acquire);
        if len == 0 || pos >= self.inner.length {
            return true;
        }
        let cs = i64::from(self.inner.chunk_size);
        let start = pos / cs;
        let end = (self.inner.total_chunks - 1).min((pos + len as i64 - 1) / cs);
        let st = self.inner.cache_state.lock();
        (start..=end).all(|c| st.chunks.contains_key(&c))
    }
}

impl Inner {
    fn is_disposed(&self) -> bool {
        self.disposed.load(Ordering::Acquire)
    }

    fn load_persisted(&self) {
        if self.is_disposed() {
            return;
        }
        let (Some(store), Some(id)) = (&self.store, &self.identity) else { return };
        let existing = store.cached_chunk_indices_for(self.session.as_deref(), id, self.total_chunks, self.chunk_size, self.length);
        if existing.is_empty() || self.is_disposed() {
            return;
        }
        self.cache_state.lock().persisted.extend(existing);
    }

    fn next_tick(&self) -> u64 {
        self.access_counter.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn chunk_buffered(st: &CacheState, c: i64) -> bool {
        st.chunks.contains_key(&c) || st.persisted.contains(&c)
    }

    fn buffered_ratio(&self, playback_ratio: f64) -> f64 {
        if self.is_disposed() || self.length <= 0 || self.total_chunks <= 0 {
            return 0.0;
        }
        let p = if playback_ratio.is_finite() { playback_ratio } else { 0.0 }.clamp(0.0, 1.0);
        let st = self.cache_state.lock();
        if st.chunks.is_empty() && st.persisted.is_empty() {
            return p;
        }
        let total = self.total_chunks;
        let all = if st.chunks.len() as i64 >= total || st.persisted.len() as i64 >= total {
            true
        } else if (st.chunks.len() + st.persisted.len()) as i64 >= total {
            (0..total).all(|c| Self::chunk_buffered(&st, c))
        } else {
            false
        };
        if all {
            return 1.0;
        }
        let cs = i64::from(self.chunk_size);
        let approx = (p * self.length as f64) as i64;
        let start = (approx / cs).clamp(0, total - 1);
        let mut end_byte = approx;
        for c in start..total {
            if !Self::chunk_buffered(&st, c) {
                break;
            }
            end_byte = self.length.min((c + 1) * cs);
        }
        let r_offset = (end_byte as f64 / self.length as f64).clamp(p, 1.0);
        let lr = self.last_read_chunk.load(Ordering::Acquire);
        if (0..total).contains(&lr) && Self::chunk_buffered(&st, lr) {
            let mut n = 0i64;
            for c in lr..total {
                if !Self::chunk_buffered(&st, c) {
                    break;
                }
                n += 1;
            }
            if lr + n >= total {
                return 1.0;
            }
            let r_last = (p + (n * cs) as f64 / self.length as f64).clamp(p, 1.0);
            return r_offset.max(r_last);
        }
        r_offset
    }

    fn seek(&self, pos: SeekFrom) -> std::io::Result<u64> {
        if self.is_disposed() {
            return Ok(self.position.load(Ordering::Acquire).max(0) as u64);
        }
        let next = match pos {
            SeekFrom::Start(o) => i64::try_from(o).unwrap_or(i64::MAX),
            SeekFrom::Current(o) => self.position.load(Ordering::Acquire).saturating_add(o),
            SeekFrom::End(o) => self.length.saturating_add(o),
        };
        if next < 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Cannot seek before the beginning of the stream."));
        }
        self.position.store(next, Ordering::Release);
        Ok(next as u64)
    }

    fn read(self: &Arc<Self>, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.is_disposed() || buf.is_empty() {
            return Ok(0);
        }
        let mut pos = self.position.load(Ordering::Acquire);
        if pos >= self.length {
            return Ok(0);
        }
        let cs = i64::from(self.chunk_size);
        let mut copied = 0usize;
        let mut last_chunk = -1i64;
        while copied < buf.len() && pos < self.length && !self.is_disposed() {
            let ci = pos / cs;
            let off = (pos % cs) as usize;
            let chunk = self.get_or_load_chunk(ci, true)?;
            if chunk.len <= off {
                break;
            }
            let n = (buf.len() - copied).min(chunk.len - off);
            buf[copied..copied + n].copy_from_slice(&chunk.data[off..off + n]);
            copied += n;
            pos += n as i64;
            last_chunk = ci;
        }
        self.position.store(pos, Ordering::Release);
        if last_chunk >= 0 {
            let prev = self.last_read_chunk.swap(last_chunk, Ordering::AcqRel);
            let sequential = prev < 0 || last_chunk == prev || last_chunk == prev + 1;
            if self.read_ahead > 0 && sequential && !self.is_disposed() {
                self.schedule_read_ahead(last_chunk + 1);
            }
        }
        Ok(copied)
    }

    fn evict_locked(&self, st: &mut CacheState, protected: i64) {
        while st.chunks.len() >= self.max_cached {
            let victim = st.chunks.iter().filter(|(k, _)| **k != protected).min_by_key(|(_, c)| c.tick).map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    st.chunks.remove(&k);
                }
                None => break,
            }
        }
    }

    fn get_or_load_chunk(self: &Arc<Self>, ci: i64, foreground: bool) -> std::io::Result<Chunk> {
        // 1. RAM.
        {
            let mut st = self.cache_state.lock();
            if let Some(c) = st.chunks.get_mut(&ci) {
                c.tick = self.next_tick();
                return Ok(c.clone());
            }
        }
        let cs = i64::from(self.chunk_size);
        let start = ci * cs;
        let expected = cs.min((self.length - start).max(0)) as usize;
        let cached = self.store.is_some() && self.identity.is_some();

        // 2. Encrypted SSD cache, without the source lock (never waits on the source disk).
        if expected > 0
            && let (Some(store), Some(id)) = (&self.store, &self.identity)
        {
            if let Some(data) = store.try_read_chunk_for(self.session.as_deref(), id, self.chunk_size, ci, expected as i32)
                && data.len() == expected
            {
                self.encrypted_cache_read_count.fetch_add(1, Ordering::AcqRel);
                let chunk = Chunk { data: Arc::from(data), len: expected, tick: self.next_tick() };
                let mut st = self.cache_state.lock();
                if let Some(existing) = st.chunks.get_mut(&ci) {
                    existing.tick = self.next_tick();
                    return Ok(existing.clone());
                }
                self.evict_locked(&mut st, ci);
                st.chunks.insert(ci, chunk.clone());
                st.persisted.insert(ci);
                return Ok(chunk);
            }
            self.cache_state.lock().persisted.remove(&ci);
        }

        // 3. Source read.
        let _guard = foreground.then(|| {
            self.foreground_waiters.fetch_add(1, Ordering::AcqRel);
            WaiterGuard(&self.foreground_waiters)
        });
        let loaded = {
            let mut io = self.io.lock();
            {
                let mut st = self.cache_state.lock();
                if let Some(c) = st.chunks.get_mut(&ci) {
                    c.tick = self.next_tick();
                    return Ok(c.clone());
                }
            }
            if self.is_disposed() {
                return Ok(Chunk { data: Arc::from(Vec::new()), len: 0, tick: 0 });
            }
            let mut data = vec![0u8; expected];
            let mut got = 0usize;
            if expected > 0 {
                let io = &mut *io;
                let Some(inner) = io.inner.as_mut() else {
                    return Ok(Chunk { data: Arc::from(Vec::new()), len: 0, tick: 0 });
                };
                // C# compares with the live `Position`; after a failed seek or read the tracked position is unknown,
                // so the next load seeks again.
                if io.pos != start as u64 {
                    io.pos = UNKNOWN_POSITION;
                    io.pos = inner.seek(SeekFrom::Start(start as u64))?;
                }
                while got < expected {
                    match inner.read(&mut data[got..]) {
                        Ok(0) => break,
                        Ok(n) => {
                            got += n;
                            io.pos += n as u64;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            io.pos = UNKNOWN_POSITION;
                            return Err(e);
                        }
                    }
                }
                self.underlying_read_count.fetch_add(1, Ordering::AcqRel);
                self.underlying_bytes_read.fetch_add(got as i64, Ordering::AcqRel);
            }
            let chunk = Chunk { data: Arc::from(data), len: got, tick: self.next_tick() };
            let mut st = self.cache_state.lock();
            self.evict_locked(&mut st, ci);
            st.chunks.insert(ci, chunk.clone());
            if got > 0 && cached {
                // Optimistic: counted as persisted before the SSD write is done.
                st.persisted.insert(ci);
            }
            chunk
        };

        if loaded.len > 0
            && let (Some(store), Some(id)) = (&self.store, &self.identity)
        {
            if self.async_persistence {
                self.enqueue_write(ci, Arc::clone(&loaded.data), loaded.len);
            } else {
                store.try_write_chunk_for(self.session.as_deref(), id, self.chunk_size, ci, &loaded.data[..loaded.len]);
            }
        }
        Ok(loaded)
    }

    fn schedule_read_ahead(self: &Arc<Self>, start: i64) {
        if start < 0 || start >= self.total_chunks || self.is_disposed() {
            return;
        }
        self.target_read_ahead.store(start, Ordering::Release);
        if self.read_ahead_running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            let worker = Arc::clone(self);
            if std::thread::Builder::new().name("vstream-readahead".into()).spawn(move || worker.read_ahead_loop()).is_err() {
                self.read_ahead_running.store(false, Ordering::Release);
            }
        }
    }

    fn read_ahead_loop(self: &Arc<Self>) {
        struct Reset<'a>(&'a AtomicBool);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _reset = Reset(&self.read_ahead_running);
        while !self.cancel.is_cancelled() && !self.is_disposed() {
            if self.foreground_waiters.load(Ordering::Acquire) > 0 {
                std::thread::yield_now();
                continue;
            }
            let s = self.target_read_ahead.swap(-1, Ordering::AcqRel);
            if s < 0 {
                break;
            }
            let end = (self.total_chunks - 1).min(s + self.read_ahead - 1);
            let mut idx = s;
            while idx <= end && !self.cancel.is_cancelled() && !self.is_disposed() {
                if self.foreground_waiters.load(Ordering::Acquire) > 0 {
                    // Yield to the foreground read; keep the interrupted position unless a newer target was set.
                    let _ = self.target_read_ahead.compare_exchange(-1, idx, Ordering::AcqRel, Ordering::Acquire);
                    break;
                }
                let in_ram = self.cache_state.lock().chunks.contains_key(&idx);
                if !in_ram && self.get_or_load_chunk(idx, false).is_err() {
                    return;
                }
                idx += 1;
            }
        }
    }

    fn enqueue_write(self: &Arc<Self>, index: i64, data: Arc<[u8]>, len: usize) {
        if self.is_disposed() || self.store.is_none() || self.identity.is_none() {
            return;
        }
        self.pending.lock().push_back((index, data, len));
        self.spawn_writer();
    }

    fn spawn_writer(self: &Arc<Self>) {
        if self.ssd_writer_running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            let worker = Arc::clone(self);
            if std::thread::Builder::new()
                .name("vstream-ssd".into())
                .spawn(move || {
                    worker.drain_pending();
                    worker.ssd_writer_running.store(false, Ordering::Release);
                    if !worker.pending.lock().is_empty() {
                        worker.spawn_writer();
                    }
                })
                .is_err()
            {
                self.ssd_writer_running.store(false, Ordering::Release);
            }
        }
    }

    fn drain_pending(&self) {
        let (Some(store), Some(id)) = (&self.store, &self.identity) else {
            self.pending.lock().clear();
            return;
        };
        loop {
            let Some((index, data, len)) = self.pending.lock().pop_front() else { break };
            store.try_write_chunk_for(self.session.as_deref(), id, self.chunk_size, index, &data[..len]);
        }
    }

    fn dispose(&self) {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.cancel.cancel();
        self.cache_state.lock().chunks.clear();
        self.drain_pending();
        let mut io = self.io.lock();
        if self.owns {
            io.inner = None;
        }
    }
}

impl Drop for BufferedVideoStream {
    fn drop(&mut self) {
        self.inner.dispose();
    }
}

impl Read for BufferedVideoStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for BufferedVideoStream {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl Read for &BufferedVideoStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for &BufferedVideoStream {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// `BufferedVideoStream.GetMimeType`: the container MIME type by extension (`video/mp4` when unknown). The extension
/// is lower-cased like `ToLowerInvariant`; the only non-ASCII character that lower-cases into one of the ASCII keys is
/// the Kelvin sign U+212A (to `k`, as in `.mkv`).
pub fn mime_type(path: Option<&str>) -> &'static str {
    let ext: String = path
        .map(super::paths::extension)
        .unwrap_or("")
        .chars()
        .map(|c| if c == '\u{212A}' { 'k' } else { c.to_ascii_lowercase() })
        .collect();
    match ext.as_str() {
        ".mp4" | ".m4v" => "video/mp4",
        ".mkv" => "video/x-matroska",
        ".webm" => "video/webm",
        ".mov" => "video/quicktime",
        ".avi" => "video/x-msvideo",
        ".wmv" => "video/x-ms-wmv",
        ".mpg" | ".mpeg" => "video/mpeg",
        ".ts" | ".mts" | ".m2ts" => "video/mp2t",
        ".3gp" => "video/3gpp",
        ".flv" => "video/x-flv",
        ".mp3" => "audio/mpeg",
        ".wav" => "audio/wav",
        ".m4a" => "audio/mp4",
        ".aac" => "audio/aac",
        ".flac" => "audio/flac",
        ".ogg" | ".opus" => "audio/ogg",
        ".wma" => "audio/x-ms-wma",
        _ => "video/mp4",
    }
}
