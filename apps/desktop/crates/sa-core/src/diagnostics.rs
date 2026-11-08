//! Diagnostic log (win-core §6, mac-enginekit §9, UNIFIED.md §2 "Diagnostics").
//!
//! Never blocks the caller: lines are formatted immediately and appended by one background consumer (Windows: bounded
//! queue of 1024 lines, drop-oldest, like the C# channel; macOS: unbounded, like the Swift serial queue). Rotation
//! at 2 MiB to `<file>.old`. Messages are scrubbed of configured API keys and
//! `(?i)(bearer\s+)[^\s"',;]+` → `$1[redacted]` on **both** platforms (UNIFIED.md FIX-W).
//!
//! The Windows format additionally keeps the C# app's privacy rules (`DiagnosticLog.cs`): every absolute Windows
//! path or `file:` URI in a message becomes `<path>`, and the existing log files (`…diagnostics.log`, `.old`, the
//! crash log) are scrubbed the same way by the consumer before the first new line; the legacy player log is deleted.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Weak};

use parking_lot::{Condvar, Mutex};
use regex::Regex;

/// Where diagnostic lines go. Implemented by [`DiagnosticLog`]; tests use [`NullDiagnostics`].
pub trait DiagnosticSink: Send + Sync {
    /// Formats and enqueues one line. Must never be given subtitle text, prompts or secrets.
    fn write(&self, category: &str, message: &str);
}

/// Line format and location per platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticFormat {
    /// `%TEMP%\SubtitleAdder-diagnostics.log`; `"{O-time 7 digits ±HH:MM} [T{tid}] [{cat}] {msg}\r\n"`.
    Windows,
    /// `~/Library/Application Support/SubtitleAdder/diagnostics.log`; `"{ISO8601 UTC} [{cat}] {msg}\n"`, newlines in
    /// the message → spaces, at most 2000 characters.
    Mac,
}

impl DiagnosticFormat {
    /// The format of the platform this binary is built for.
    pub const fn native() -> Self {
        if cfg!(target_os = "macos") { Self::Mac } else { Self::Windows }
    }
}

type Secrets = Box<dyn Fn() -> Vec<String> + Send + Sync>;

#[derive(Default)]
struct Queue {
    lines: VecDeque<(u64, String)>,
    /// Sequence number of the last line handed to the writer (written or discarded).
    done: u64,
    worker_started: bool,
    /// The consumer finished its startup work (the Windows scrub) and is appending.
    scrubbed: bool,
    shutdown: bool,
}

struct Shared {
    path: PathBuf,
    format: DiagnosticFormat,
    max_bytes: u64,
    queue: Mutex<Queue>,
    ready: Condvar,
    idle: Condvar,
    next_seq: AtomicU64,
}

/// The diagnostic log.
pub struct DiagnosticLog {
    shared: Arc<Shared>,
    secrets: Secrets,
}

impl DiagnosticLog {
    /// Queue capacity (drop-oldest).
    pub const QUEUE_CAPACITY: usize = 1024;
    /// Rotation threshold.
    pub const ROTATE_BYTES: u64 = 2 * 1024 * 1024;
    /// macOS message length limit.
    pub const MAC_MAX_MESSAGE_CHARS: usize = 2000;
    /// Prefix of the engine's stderr lines copied into the log (mac-enginekit §9.5).
    pub const TRANSLATION_PREFIX: &'static str = "[translation] ";
    /// At most this many bytes from the end of an engine stderr file are scanned.
    pub const MAX_SCANNED_BYTES: u64 = 4 * 1024 * 1024;
    /// At most this many matching lines are copied from one run.
    pub const MAX_ENGINE_LINES: usize = 500;
    /// Placeholder written instead of a file path (Windows format).
    pub const REDACTED_PATH: &'static str = "<path>";

    /// A log writing to `path` in `format`. `secrets` returns the API keys to redact at write time (the active
    /// provider key from `EngineLaunchState`); it is evaluated for every line.
    pub fn new(path: PathBuf, format: DiagnosticFormat, secrets: Box<dyn Fn() -> Vec<String> + Send + Sync>) -> Arc<Self> {
        Self::with_max_bytes(path, format, Self::ROTATE_BYTES, secrets)
    }

    /// [`new`](Self::new) with an explicit rotation threshold (tests).
    ///
    /// Windows: the consumer starts at once, like the C# static constructor (`DiagnosticLog.Initialize()` at app
    /// start), so the scrub of path-bearing logs from earlier versions runs at startup even if nothing is logged.
    pub fn with_max_bytes(path: PathBuf, format: DiagnosticFormat, max_bytes: u64, secrets: Secrets) -> Arc<Self> {
        let log = Arc::new(Self {
            shared: Arc::new(Shared {
                path,
                format,
                max_bytes,
                queue: Mutex::new(Queue::default()),
                ready: Condvar::new(),
                idle: Condvar::new(),
                next_seq: AtomicU64::new(0),
            }),
            secrets,
        });
        if format == DiagnosticFormat::Windows {
            log.start_worker(&mut log.shared.queue.lock());
        }
        log
    }

    /// Starts the single consumer once (under the queue lock).
    fn start_worker(&self, q: &mut Queue) {
        if q.worker_started {
            return;
        }
        q.worker_started = true;
        let weak = Arc::downgrade(&self.shared);
        let spawned = std::thread::Builder::new().name("diagnostic-log".into()).spawn(move || consume(weak));
        if spawned.is_err() {
            // No consumer: lines are dropped (a diagnostic log must never get in the way).
            q.shutdown = true;
            q.lines.clear();
        }
    }

    /// Waits until the Windows startup scrub finished (tests).
    pub fn wait_for_startup_scrub(&self) {
        let mut q = self.shared.queue.lock();
        while q.worker_started && !q.scrubbed && !q.shutdown {
            self.shared.idle.wait(&mut q);
        }
    }

    /// The platform default path (`std::env::temp_dir()` on Windows, the app data folder on macOS).
    pub fn default_path(format: DiagnosticFormat, app_data_dir: &Path) -> PathBuf {
        match format {
            DiagnosticFormat::Windows => std::env::temp_dir().join("SubtitleAdder-diagnostics.log"),
            DiagnosticFormat::Mac => app_data_dir.join("diagnostics.log"),
        }
    }

    /// Where the log is written.
    pub fn path(&self) -> &Path {
        &self.shared.path
    }

    /// The rotated copy next to the log (`<file>.old`).
    pub fn rotated_path(&self) -> PathBuf {
        rotated(&self.shared.path)
    }

    /// Windows: the crash log next to the diagnostics log (`SubtitleAdder-crash.log`).
    pub fn crash_log_path(&self) -> PathBuf {
        sibling(&self.shared.path, "SubtitleAdder-crash.log")
    }

    /// Windows: the player log of earlier versions (full video paths), deleted by the startup scrub.
    pub fn legacy_player_log_path(&self) -> PathBuf {
        sibling(&self.shared.path, "SubtitleAdder-player.log")
    }

    /// Removes every secret (exact substring, non-empty, trimmed) and redacts bearer tokens.
    pub fn sanitize(message: &str, secrets: &[String]) -> String {
        static BEARER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)(bearer\s+)[^\s"',;]+"#).expect("bearer regex"));
        let mut text = message.to_owned();
        for secret in secrets {
            let key = secret.trim();
            if !key.is_empty() {
                text = text.replace(key, "[redacted]");
            }
        }
        BEARER.replace_all(&text, "${1}[redacted]").into_owned()
    }

    /// macOS message rules on top of [`sanitize`](Self::sanitize): newline runs → one space (empty pieces dropped),
    /// at most 2000 characters.
    pub fn sanitize_mac(message: &str, secrets: &[String]) -> String {
        let text = Self::sanitize(message, secrets);
        let joined = text.split(is_newline).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
        joined.chars().take(Self::MAC_MAX_MESSAGE_CHARS).collect()
    }

    /// C# `DiagnosticLog.Redact`: absolute Windows paths (`C:\…`, `C:/…`, `\\server\…`) and `file:` URIs →
    /// `<path>`; a path runs until a quote, a line break or the end.
    pub fn redact_paths(message: &str) -> String {
        static PATH: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"(?:file:/{2,3}|\b[A-Za-z]:[\\/]|\\\\[^\\\s'"]+\\)[^'"\r\n]*"#).expect("path regex")
        });
        if message.is_empty() {
            return String::new();
        }
        PATH.replace_all(message, DiagnosticLog::REDACTED_PATH).into_owned()
    }

    /// Formats one line for `format` (timestamp taken now).
    pub fn format_line(format: DiagnosticFormat, category: &str, message: &str, secrets: &[String]) -> String {
        match format {
            DiagnosticFormat::Windows => {
                let now = chrono::Local::now();
                let ts = format!(
                    "{}.{:07}{}",
                    now.format("%Y-%m-%dT%H:%M:%S"),
                    now.timestamp_subsec_nanos().min(999_999_999) / 100,
                    now.format("%:z")
                );
                let msg = Self::redact_paths(&Self::sanitize(message, secrets));
                format!("{ts} [T{}] [{category}] {msg}\r\n", thread_number())
            }
            DiagnosticFormat::Mac => {
                let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
                format!("{ts} [{category}] {}\n", Self::sanitize_mac(message, secrets))
            }
        }
    }

    /// `write` with extra secrets (the key an engine process was launched with).
    pub fn write_redacting(&self, category: &str, message: &str, extra_secrets: &[String]) {
        let mut secrets = (self.secrets)();
        secrets.extend(extra_secrets.iter().cloned());
        let line = Self::format_line(self.shared.format, category, message, &secrets);
        self.enqueue(vec![line]);
    }

    /// Queues the engine's `[translation] ` stderr lines as category `engine` (one batch, one timestamp).
    pub fn write_engine_lines(&self, lines: &[String], extra_secrets: &[String]) {
        if lines.is_empty() {
            return;
        }
        let mut secrets = (self.secrets)();
        secrets.extend(extra_secrets.iter().cloned());
        let formatted = lines.iter().map(|l| Self::format_line(self.shared.format, "engine", l, &secrets)).collect();
        self.enqueue(formatted);
    }

    /// The `[translation] ` lines of an engine stderr file (its last 4 MiB, lossy UTF-8, split on `\n` or `\r`),
    /// in order, at most the last 500 (mac-enginekit §9.5).
    pub fn translation_lines(log_path: &Path) -> Vec<String> {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut file) = std::fs::File::open(log_path) else { return Vec::new() };
        let Ok(size) = file.seek(SeekFrom::End(0)) else { return Vec::new() };
        let start = size.saturating_sub(Self::MAX_SCANNED_BYTES);
        if file.seek(SeekFrom::Start(start)).is_err() {
            return Vec::new();
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<String> = text
            .split(['\n', '\r'])
            .filter(|l| !l.is_empty() && l.starts_with(Self::TRANSLATION_PREFIX))
            .map(str::to_owned)
            .collect();
        let skip = lines.len().saturating_sub(Self::MAX_ENGINE_LINES);
        lines.into_iter().skip(skip).collect()
    }

    /// Windows: appends already-redacted `text` to the crash log; `false` when it could not be written.
    pub fn append_crash(&self, text: &str) -> bool {
        let _guard = crash_lock().lock();
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.crash_log_path())
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .is_ok()
    }

    /// Waits until every queued line was written (tests, exit).
    pub fn flush(&self) {
        let target = self.shared.next_seq.load(Ordering::Acquire);
        let mut q = self.shared.queue.lock();
        if !q.worker_started {
            return;
        }
        while q.done < target && !q.shutdown {
            self.shared.idle.wait(&mut q);
        }
    }

    fn enqueue(&self, lines: Vec<String>) {
        let mut q = self.shared.queue.lock();
        for line in lines {
            let seq = self.shared.next_seq.fetch_add(1, Ordering::AcqRel) + 1;
            // Windows: the C# bounded channel (1024, drop-oldest). macOS: a serial dispatch queue, unbounded.
            if self.shared.format == DiagnosticFormat::Windows && q.lines.len() >= Self::QUEUE_CAPACITY {
                q.lines.pop_front();
            }
            q.lines.push_back((seq, line));
        }
        self.start_worker(&mut q);
        drop(q);
        self.shared.ready.notify_one();
    }
}

impl Drop for DiagnosticLog {
    fn drop(&mut self) {
        self.flush();
        self.shared.queue.lock().shutdown = true;
        self.shared.ready.notify_all();
    }
}

impl DiagnosticSink for DiagnosticLog {
    fn write(&self, category: &str, message: &str) {
        self.write_redacting(category, message, &[]);
    }
}

/// Discards everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullDiagnostics;

impl DiagnosticSink for NullDiagnostics {
    fn write(&self, _category: &str, _message: &str) {}
}

/// Collects lines in memory (tests of callers).
#[derive(Debug, Default)]
pub struct MemoryDiagnostics {
    lines: Mutex<Vec<(String, String)>>,
}

impl MemoryDiagnostics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    /// `(category, message)` pairs in write order.
    pub fn lines(&self) -> Vec<(String, String)> {
        self.lines.lock().clone()
    }
}

impl DiagnosticSink for MemoryDiagnostics {
    fn write(&self, category: &str, message: &str) {
        self.lines.lock().push((category.to_owned(), message.to_owned()));
    }
}

fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

fn rotated(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".old");
    PathBuf::from(s)
}

fn sibling(path: &Path, name: &str) -> PathBuf {
    path.parent().map(|p| p.join(name)).unwrap_or_else(|| PathBuf::from(name))
}

/// Serializes crash-log appends with the startup scrub (process-wide, like the C# static lock).
fn crash_lock() -> &'static Mutex<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    &LOCK
}

/// A small per-thread sequential id (stands in for the .NET managed thread id).
fn thread_number() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static ID: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    ID.with(|id| *id)
}

/// The single consumer: scrub (Windows) once, then append batches until the log is dropped.
fn consume(weak: Weak<Shared>) {
    if let Some(shared) = weak.upgrade() {
        if shared.format == DiagnosticFormat::Windows {
            scrub_existing_logs(&shared.path);
        }
        shared.queue.lock().scrubbed = true;
        shared.idle.notify_all();
    }
    loop {
        let Some(shared) = weak.upgrade() else { return };
        let batch: Vec<(u64, String)> = {
            let mut q = shared.queue.lock();
            while q.lines.is_empty() && !q.shutdown {
                // Wake up now and then so a dropped log lets the thread end.
                let _ = shared.ready.wait_for(&mut q, std::time::Duration::from_millis(500));
                if q.lines.is_empty() && Arc::strong_count(&shared) == 1 {
                    return;
                }
            }
            if q.lines.is_empty() && q.shutdown {
                shared.idle.notify_all();
                return;
            }
            q.lines.drain(..).collect()
        };
        let last = batch.last().map(|(s, _)| *s).unwrap_or(0);
        append(&shared, batch.into_iter().map(|(_, l)| l));
        {
            let mut q = shared.queue.lock();
            q.done = q.done.max(last);
        }
        shared.idle.notify_all();
    }
}

fn append(shared: &Shared, lines: impl Iterator<Item = String>) {
    let path = &shared.path;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > shared.max_bytes
    {
        let old = rotated(path);
        if shared.format == DiagnosticFormat::Mac {
            // Swift: `try? removeItem(.old)`, then `try moveItem`; a failed move drops the batch.
            let _ = std::fs::remove_file(&old);
            if std::fs::rename(path, &old).is_err() {
                return;
            }
        } else {
            // C#: `File.Move(overwrite: true)` in a catch-all; the batch is appended either way.
            let _ = std::fs::rename(path, &old);
        }
    }
    let text: String = lines.collect();
    // Any error drops the batch (no spin, no retry).
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(text.as_bytes()));
}

/// Windows startup scrub (C# `ScrubExistingLogs`): delete the legacy player log, rewrite the log, its `.old` copy and
/// the crash log without paths. A file that grew meanwhile is retried (3 attempts); on I/O failure the original stays.
fn scrub_existing_logs(log_path: &Path) {
    let _ = std::fs::remove_file(sibling(log_path, "SubtitleAdder-player.log"));
    for path in [log_path.to_path_buf(), rotated(log_path), sibling(log_path, "SubtitleAdder-crash.log")] {
        let _guard = crash_lock().lock();
        scrub_file(&path);
    }
}

fn scrub_file(path: &Path) {
    for _ in 0..3 {
        let Ok(bytes) = std::fs::read(path) else { return };
        let length = bytes.len() as u64;
        let text = crate::atomic_file::decode_with_bom(&bytes);
        let scrubbed = text.split('\n').map(DiagnosticLog::redact_paths).collect::<Vec<_>>().join("\n");
        if scrubbed == text {
            return;
        }
        let mut temp = path.as_os_str().to_os_string();
        temp.push(format!(".{}.scrub.tmp", std::process::id()));
        let temp = PathBuf::from(temp);
        if std::fs::write(&temp, scrubbed.as_bytes()).is_err() {
            let _ = std::fs::remove_file(&temp);
            return;
        }
        if std::fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX) != length {
            let _ = std::fs::remove_file(&temp);
            continue;
        }
        if std::fs::rename(&temp, path).is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        return;
    }
}
