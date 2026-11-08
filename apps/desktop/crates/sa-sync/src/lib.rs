//! `sa-sync`: cross-instance state sync (win-sync; UNIFIED.md §5.4, §3.19).
//!
//! * Windows: [`NamedPipeBus`], wire-compatible with the C# `StateSyncBroker` (same scope hash, pipe name, temp
//!   paths, slot/lock files, enum numbers, `W:`/`A:` lines) so a C# instance and the Rust app form one channel.
//!   UNIFIED.md approves win-sync §13 F1–F5 and **requires** F6 (pipe DACL = current user + SYSTEM,
//!   `PIPE_REJECT_REMOTE_CLIENTS`, the client verifies the server's owner SID).
//! * macOS (and tests everywhere): [`InProcessBus`] (LibraryKit keeps its own flock/watcher cross-process mechanism).
//!
//! Handlers run synchronously on the delivering thread; UI consumers re-post to the event loop. Consumers ignore
//! events whose `sender_instance_id` is their own or whose `scope_path` differs (case-insensitive).
//!
//! The profile store (`sa-profiles::win::SqliteProfileStore`) registers itself through
//! [`ProfileWriteBroker::set_data_worker_factory`] and calls [`push_in_process_pending_writes`] at the start of its
//! flush and drain paths (C# `SqliteProfileStore.BeforeFlushPathHook`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sa_core::subscription::Subscription;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod core;
mod inproc;
pub mod lines;
#[cfg(windows)]
mod win;
pub mod wire;

pub use core::{push_in_process_pending_writes, total_active_in_process_brokers};
pub use inproc::InProcessBus;
#[cfg(windows)]
pub use win::{NamedPipeBus, is_channel_engine_running, pipe_name};
#[cfg(windows)]
#[doc(hidden)]
pub use win::testing as win_testing;
pub use wire::{BatchListItem, BatchRow, EventKind, StateSyncEvent, StateSyncWriteAck, StateSyncWriteRequest, WriteKind};

/// Topics (wire values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Topic {
    Profiles,
    WatchProgress,
    Thumbnail,
    MediaIndex,
    Providers,
    Settings,
}

impl Topic {
    pub const ALL: [Topic; 6] =
        [Topic::Profiles, Topic::WatchProgress, Topic::Thumbnail, Topic::MediaIndex, Topic::Providers, Topic::Settings];

    pub const fn wire(self) -> &'static str {
        match self {
            Topic::Profiles => "profiles",
            Topic::WatchProgress => "watch-progress",
            Topic::Thumbnail => "thumbnail",
            Topic::MediaIndex => "media-index",
            Topic::Providers => "providers",
            Topic::Settings => "settings",
        }
    }

    /// Exact wire value; see [`Topic::matches`] for the routing comparison.
    pub fn parse(wire: &str) -> Option<Self> {
        Topic::ALL.into_iter().find(|t| t.wire() == wire)
    }

    /// True when an event's topic routes to this topic (C# `OrdinalIgnoreCase`).
    pub fn matches(self, event_topic: &str) -> bool {
        core::eq_ordinal_ignore_case(self.wire(), event_topic)
    }
}

/// Subscription filter (`*` = every topic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TopicFilter {
    All,
    Only(Topic),
}

/// Event handler.
pub type SyncHandler = Box<dyn Fn(&StateSyncEvent) + Send + Sync>;

/// The bus contract shared by both transports.
pub trait SyncBus: Send + Sync {
    /// Never blocks on a missing peer (outbound queue of 256 lines, drop oldest). Local subscribers are called
    /// synchronously before the call returns. The event's `topic` is replaced by `topic`.
    fn publish(&self, topic: Topic, event: StateSyncEvent);
    /// The returned token unsubscribes when dropped.
    fn subscribe(&self, filter: TopicFilter, handler: SyncHandler) -> Subscription;
    /// Live instances (slot files on Windows; in-process brokers of the channel on macOS).
    fn active_instance_count(&self) -> usize;
    /// Temp cleanup policy: skip broker files and recent files while others run.
    fn has_other_active_instances(&self) -> bool;
    /// 32 lowercase hex.
    fn instance_id(&self) -> &str;
    /// Idempotent shutdown ("last window closes the engine").
    fn dispose(&self);
}

/// The SQLite data worker the broker engine hosts (implemented by `sa_profiles::win::SqliteProfileStore`; registered
/// with [`ProfileWriteBroker::set_data_worker_factory`] to avoid a crate cycle).
pub trait DataWorkerStore: Send + Sync {
    /// Enqueues a write (non-blocking); the ack is delivered through `on_ack` after commit (or failure/duplicate).
    /// The broker has already validated `request.kind` ([`StateSyncWriteRequest::write_kind`] is `Some`) and turned
    /// empty batch lists into `None`.
    fn enqueue(&self, request: StateSyncWriteRequest, on_ack: Box<dyn FnOnce(StateSyncWriteAck) + Send>);
    /// Barrier: true only if the last batch succeeded and no permanent write error.
    fn flush(&self, timeout: Duration) -> bool;
    /// Post-commit events are published through this hook (installed once, right after the factory created the
    /// store; called on the store's drain thread with the committed, non-duplicate events of one batch in order).
    fn set_post_commit_hook(&self, hook: Box<dyn Fn(Vec<StateSyncEvent>) + Send + Sync>);
    /// Engine shutdown (after a 5 s flush): release the database; later `enqueue` calls ack
    /// `{committed: false, error: "Store is disposed."}`. Default: nothing to release.
    fn dispose(&self) {}
}

/// Creates the data worker store for a DB path (full path). Called once per engine and DB path; it must return a new
/// store (the engine installs its own post-commit hook and disposes the store at shutdown) and must not call back
/// into the broker.
pub type DataWorkerFactory = Box<dyn Fn(&Path) -> Arc<dyn DataWorkerStore> + Send + Sync>;

/// Write path of the profile store (win-sync §8). Implemented by [`NamedPipeBus`] (Windows) and [`InProcessBus`].
pub trait ProfileWriteBroker: SyncBus {
    /// Fire-and-forget; tracked by `request_id` until a committed ack (resent as `W:` on reconnect). A blank id gets
    /// a fresh one.
    fn submit_write_request(&self, request: StateSyncWriteRequest);
    /// Push this broker's pending requests for `db`, then flush the store (default 5 s). Disposed → false.
    fn flush_pending_writes(&self, db: &Path, timeout: Option<Duration>) -> bool;
    /// The engine's store for `db` (full path, case-insensitive key), created on demand.
    fn data_worker_store(&self, db: &Path) -> Arc<dyn DataWorkerStore>;
    /// Process-wide: every engine created afterwards (and every store not yet created) uses `factory`.
    fn set_data_worker_factory(&self, factory: DataWorkerFactory);
    fn pending_write_request_count(&self) -> usize;
}

/// Default channel scope: .NET `$"{UserDomainName}\{UserName}"` = SAM-compatible `DOMAIN\user` (Windows); `$USER`
/// elsewhere.
pub fn default_scope() -> String {
    #[cfg(windows)]
    {
        win::default_scope()
    }
    #[cfg(not(windows))]
    {
        std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_else(|_| "user".into())
    }
}

/// `lowercase_hex(SHA256(UTF8(scope)))[0..16]`.
pub fn scope_hash(scope: &str) -> String {
    let digest = Sha256::digest(scope.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// `<temp>\SubtitleAdder\broker-<hash16>`.
pub fn broker_dir(temp: &Path, scope: &str) -> PathBuf {
    temp.join("SubtitleAdder").join(format!("broker-{}", scope_hash(scope)))
}
