//! Test doubles shared by the integration tests.
#![allow(dead_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sa_sync::{DataWorkerFactory, DataWorkerStore, StateSyncEvent, StateSyncWriteAck, StateSyncWriteRequest};

type Hook = Box<dyn Fn(Vec<StateSyncEvent>) + Send + Sync>;
type Ack = Box<dyn FnOnce(StateSyncWriteAck) + Send>;

/// An in-memory data worker mimicking `SqliteProfileStore`'s contract: `processed_requests` idempotency (a duplicate
/// is acked `committed + isDuplicate` and its post-commit events are dropped), post-commit events after the commit,
/// `"Store is disposed."` after dispose. With `hold`, commits wait for the next `flush`.
pub struct FakeStore {
    pub path: PathBuf,
    processed: Mutex<HashSet<String>>,
    pub committed: Mutex<Vec<StateSyncWriteRequest>>,
    held: Mutex<Vec<(StateSyncWriteRequest, Ack)>>,
    hook: Mutex<Option<Arc<Hook>>>,
    pub hold: AtomicBool,
    pub disposed: AtomicBool,
    pub duplicates: AtomicUsize,
    pub flushes: AtomicUsize,
}

impl FakeStore {
    pub fn new(path: &Path) -> Arc<Self> {
        Arc::new(Self {
            path: path.to_path_buf(),
            processed: Mutex::new(HashSet::new()),
            committed: Mutex::new(Vec::new()),
            held: Mutex::new(Vec::new()),
            hook: Mutex::new(None),
            hold: AtomicBool::new(false),
            disposed: AtomicBool::new(false),
            duplicates: AtomicUsize::new(0),
            flushes: AtomicUsize::new(0),
        })
    }

    pub fn is_processed(&self, id: &str) -> bool {
        self.processed.lock().contains(id)
    }

    fn commit(&self, req: StateSyncWriteRequest, ack: Ack) {
        let id = req.request_id.clone();
        let dup = !self.processed.lock().insert(id.clone());
        if dup {
            self.duplicates.fetch_add(1, Ordering::SeqCst);
            ack(StateSyncWriteAck::new(&id, true, true, None));
            return;
        }
        let events = req.post_commit_events.clone().unwrap_or_default();
        self.committed.lock().push(req);
        ack(StateSyncWriteAck::new(&id, true, false, None));
        if !events.is_empty() {
            let hook = self.hook.lock().clone();
            if let Some(h) = hook {
                h(events);
            }
        }
    }
}

impl DataWorkerStore for FakeStore {
    fn enqueue(&self, request: StateSyncWriteRequest, on_ack: Ack) {
        if self.disposed.load(Ordering::SeqCst) {
            on_ack(StateSyncWriteAck::new(&request.request_id, false, false, Some("Store is disposed.".into())));
            return;
        }
        if self.hold.load(Ordering::SeqCst) {
            self.held.lock().push((request, on_ack));
            return;
        }
        self.commit(request, on_ack);
    }

    fn flush(&self, _timeout: Duration) -> bool {
        self.flushes.fetch_add(1, Ordering::SeqCst);
        // The real store calls the broker's before-flush hook first.
        sa_sync::push_in_process_pending_writes(&self.path);
        self.hold.store(false, Ordering::SeqCst);
        let held: Vec<_> = self.held.lock().drain(..).collect();
        for (r, a) in held {
            self.commit(r, a);
        }
        !self.disposed.load(Ordering::SeqCst)
    }

    fn set_post_commit_hook(&self, hook: Hook) {
        *self.hook.lock() = Some(Arc::new(hook));
    }

    fn dispose(&self) {
        self.disposed.store(true, Ordering::SeqCst);
    }
}

/// Every store the factory created, by full path (tests look them up after the fact).
pub fn created_stores() -> &'static Mutex<Vec<Arc<FakeStore>>> {
    static S: OnceLock<Mutex<Vec<Arc<FakeStore>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn fake_factory() -> DataWorkerFactory {
    Box::new(|p: &Path| {
        let s = FakeStore::new(p);
        created_stores().lock().push(s.clone());
        s as Arc<dyn DataWorkerStore>
    })
}

/// Polls `cond` every 15 ms for up to `ms` (C# `WaitUntilAsync`).
pub fn wait_until(ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return cond();
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// A unique scope (never the default one).
pub fn test_scope(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// Records every delivered event.
#[derive(Clone, Default)]
pub struct Recorder(pub Arc<Mutex<Vec<StateSyncEvent>>>);

impl Recorder {
    pub fn handler(&self) -> sa_sync::SyncHandler {
        let v = self.0.clone();
        Box::new(move |e: &StateSyncEvent| v.lock().push(e.clone()))
    }
    pub fn len(&self) -> usize {
        self.0.lock().len()
    }
    pub fn ids(&self) -> Vec<String> {
        self.0.lock().iter().map(|e| e.event_id.clone()).collect()
    }
    pub fn count_id(&self, id: &str) -> usize {
        self.0.lock().iter().filter(|e| e.event_id == id).count()
    }
}
