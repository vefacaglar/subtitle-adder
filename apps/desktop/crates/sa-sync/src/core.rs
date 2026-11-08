//! Transport-independent broker pieces shared by [`crate::InProcessBus`] and the Windows `NamedPipeBus`:
//! local dispatch with `eventId` dedupe (win-sync §7.9), the outbound queue (§7.2), pending write requests in
//! submission order (§8.2) and the data-worker hub (§8.2–§8.5).

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use sa_core::subscription::Subscription;

use crate::wire::{StateSyncEvent, StateSyncWriteAck, StateSyncWriteRequest, WriteKind};
use crate::{DataWorkerFactory, DataWorkerStore, TopicFilter};

/// Outbound queue cap while disconnected (drop oldest).
#[cfg(windows)]
pub(crate) const OUTBOUND_CAP: usize = 256;
/// Event-id dedupe window per broker.
pub(crate) const DEDUPE_WINDOW: usize = 2048;
/// Store flush on shutdown.
pub(crate) const SHUTDOWN_FLUSH: Duration = Duration::from_secs(5);
/// Default flush timeout of `flush_pending_writes`.
pub(crate) const DEFAULT_FLUSH: Duration = Duration::from_secs(5);

type Handler = Arc<dyn Fn(&StateSyncEvent) + Send + Sync>;

/// `"*"` or a topic wire name.
pub(crate) fn filter_text(filter: TopicFilter) -> String {
    match filter {
        TopicFilter::All => "*".into(),
        TopicFilter::Only(t) => t.wire().into(),
    }
}

/// C# `Subscribe(topic)`: trimmed, blank → `*`.
pub(crate) fn normalize_subscription_topic(topic: &str) -> String {
    let t = topic.trim();
    if t.is_empty() { "*".into() } else { t.into() }
}

/// C# `Publish(topic, evt)`: trimmed, blank → `profiles`.
pub(crate) fn normalize_publish_topic(topic: &str) -> String {
    let t = topic.trim();
    if t.is_empty() { "profiles".into() } else { t.into() }
}

fn topic_matches(sub: &str, event_topic: &str) -> bool {
    sub == "*" || eq_ordinal_ignore_case(sub, event_topic)
}

/// .NET's ordinal casing of one char, which `StringComparison.OrdinalIgnoreCase` compares (checked against .NET 10 by
/// the C# interop tests): the simple (one-to-one) uppercase mapping; chars whose full uppercase is not a single char
/// (`ß`) stay as they are, and so do non-ASCII chars that would map into ASCII (`ı`, `ſ`).
fn simple_upper(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_uppercase();
    }
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(u), None) if !u.is_ascii() => u,
        _ => c,
    }
}

/// `string.Equals(a, b, StringComparison.OrdinalIgnoreCase)`.
pub(crate) fn eq_ordinal_ignore_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.chars().map(simple_upper).eq(b.chars().map(simple_upper))
}

/// A key equal for two strings exactly when they are equal under `OrdinalIgnoreCase`.
pub(crate) fn ordinal_ignore_case_key(s: &str) -> String {
    s.chars().map(simple_upper).collect()
}

// ---------------------------------------------------------------------------------------------------------------

struct SubList {
    next: u64,
    entries: Vec<(u64, String, Handler)>,
}

struct Seen {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

/// Subscribers of one broker plus its dedupe window.
pub(crate) struct LocalDispatch {
    subs: Arc<Mutex<SubList>>,
    seen: Mutex<Seen>,
}

impl LocalDispatch {
    pub(crate) fn new() -> Self {
        Self {
            subs: Arc::new(Mutex::new(SubList { next: 0, entries: Vec::new() })),
            seen: Mutex::new(Seen { ids: HashSet::new(), order: VecDeque::new() }),
        }
    }

    pub(crate) fn subscribe(&self, topic: &str, handler: Handler) -> Subscription {
        let topic = normalize_subscription_topic(topic);
        let id = {
            let mut g = self.subs.lock();
            g.next += 1;
            let id = g.next;
            g.entries.push((id, topic, handler));
            id
        };
        let weak: Weak<Mutex<SubList>> = Arc::downgrade(&self.subs);
        Subscription::new(move || {
            if let Some(list) = weak.upgrade() {
                list.lock().entries.retain(|(i, _, _)| *i != id);
            }
        })
    }

    pub(crate) fn clear(&self) {
        self.subs.lock().entries.clear();
    }

    /// Records `eventId` (2048 most recent); false when this broker already delivered it.
    fn first_sighting(&self, event_id: &str) -> bool {
        if event_id.is_empty() {
            return true;
        }
        let mut g = self.seen.lock();
        if !g.ids.insert(event_id.to_owned()) {
            return false;
        }
        g.order.push_back(event_id.to_owned());
        while g.order.len() > DEDUPE_WINDOW {
            if let Some(old) = g.order.pop_front() {
                g.ids.remove(&old);
            }
        }
        true
    }

    /// Delivers `event` once per `eventId` to every topic-matching handler, outside every lock; panics are isolated
    /// per handler.
    pub(crate) fn dispatch(&self, event: &StateSyncEvent) {
        if !self.first_sighting(&event.event_id) {
            return;
        }
        let topic = event.routing_topic();
        let handlers: Vec<Handler> = self
            .subs
            .lock()
            .entries
            .iter()
            .filter(|(_, t, _)| topic_matches(t, topic))
            .map(|(_, _, h)| h.clone())
            .collect();
        for h in handlers {
            let _ = catch_unwind(AssertUnwindSafe(|| h(event)));
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------

/// Lines published while disconnected (256, drop oldest; pipe transport only).
#[cfg(windows)]
#[derive(Default)]
pub(crate) struct OutboundQueue {
    lines: Mutex<VecDeque<String>>,
}

#[cfg(windows)]
impl OutboundQueue {
    pub(crate) fn push_back(&self, line: String) {
        let mut g = self.lines.lock();
        while g.len() >= OUTBOUND_CAP {
            g.pop_front();
        }
        g.push_back(line);
    }

    pub(crate) fn pop_front(&self) -> Option<String> {
        self.lines.lock().pop_front()
    }

    pub(crate) fn len(&self) -> usize {
        self.lines.lock().len()
    }

    pub(crate) fn clear(&self) {
        self.lines.lock().clear();
    }
}

// ---------------------------------------------------------------------------------------------------------------

/// Pending (unacknowledged) write requests of one broker, replayed in submission order.
#[derive(Default)]
pub(crate) struct PendingWrites {
    inner: Mutex<(u64, HashMap<String, (u64, StateSyncWriteRequest)>)>,
}

impl PendingWrites {
    /// Existing id → keeps the first entry (C# `TryAdd`).
    pub(crate) fn insert(&self, request: &StateSyncWriteRequest) {
        let mut g = self.inner.lock();
        g.0 += 1;
        let seq = g.0;
        g.1.entry(request.request_id.clone()).or_insert_with(|| (seq, request.clone()));
    }

    pub(crate) fn remove(&self, id: &str) -> bool {
        self.inner.lock().1.remove(id).is_some()
    }

    pub(crate) fn len(&self) -> usize {
        self.inner.lock().1.len()
    }

    pub(crate) fn in_submission_order(&self) -> Vec<StateSyncWriteRequest> {
        let g = self.inner.lock();
        let mut v: Vec<&(u64, StateSyncWriteRequest)> = g.1.values().collect();
        v.sort_by_key(|(s, _)| *s);
        v.into_iter().map(|(_, r)| r.clone()).collect()
    }

    /// C# `HandleWriteAck`: committed → removed; otherwise stays pending (resent on the next reconnect).
    pub(crate) fn apply_ack(&self, ack: &StateSyncWriteAck) {
        if ack.request_id.is_empty() {
            return;
        }
        if ack.committed {
            self.remove(&ack.request_id);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Process-wide registry of live brokers (C# `ActiveBrokersById`), shared by both transports.

/// What the hub and the store hook need from a live broker.
pub(crate) trait BrokerNode: Send + Sync {
    fn node_handle_write_ack(&self, ack: &StateSyncWriteAck);
    fn node_push_pending_for_db(&self, full_key: &str);
    fn node_is_disposed(&self) -> bool;
}

fn registry() -> &'static Mutex<HashMap<String, Weak<dyn BrokerNode>>> {
    static R: OnceLock<Mutex<HashMap<String, Weak<dyn BrokerNode>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn register_node(id: &str, node: Weak<dyn BrokerNode>) {
    registry().lock().insert(id.to_owned(), node);
}

pub(crate) fn unregister_node(id: &str) {
    registry().lock().remove(id);
}

pub(crate) fn live_node(id: &str) -> Option<Arc<dyn BrokerNode>> {
    registry().lock().get(id).and_then(Weak::upgrade).filter(|n| !n.node_is_disposed())
}

fn live_nodes() -> Vec<Arc<dyn BrokerNode>> {
    registry().lock().values().filter_map(Weak::upgrade).filter(|n| !n.node_is_disposed()).collect()
}

/// C# `StateSyncBroker.TotalActiveInProcessBrokers`: constructed-not-disposed brokers of this process, of either
/// transport (temp cleanup policy, win-sync §3.4).
pub fn total_active_in_process_brokers() -> usize {
    live_nodes().len()
}

/// C# `SqliteProfileStore.BeforeFlushPathHook` = `FlushInProcessPendingWritesForDbPath`: every live broker of this
/// process hands its not-yet-enqueued pending requests for `db` to its engine. The profile store calls this at the
/// start of its flush paths and of its non-blocking drain.
pub fn push_in_process_pending_writes(db: &Path) {
    let key = full_path_key(db);
    for node in live_nodes() {
        node.node_push_pending_for_db(&key);
    }
}

/// `Path.GetFullPath(db)` compared `OrdinalIgnoreCase`.
pub(crate) fn full_path_key(db: &Path) -> String {
    let full = std::path::absolute(db).unwrap_or_else(|_| db.to_path_buf());
    ordinal_ignore_case_key(&full.to_string_lossy())
}

// ---------------------------------------------------------------------------------------------------------------
// Data worker factory (process-wide) and the hub.

fn factory_slot() -> &'static RwLock<Option<Arc<DataWorkerFactory>>> {
    static F: OnceLock<RwLock<Option<Arc<DataWorkerFactory>>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(None))
}

pub(crate) fn set_factory(factory: DataWorkerFactory) {
    *factory_slot().write() = Some(Arc::new(factory));
}

/// Used when no factory is registered: every write fails, flushes report failure.
struct UnavailableStore;

impl DataWorkerStore for UnavailableStore {
    fn enqueue(&self, request: StateSyncWriteRequest, on_ack: Box<dyn FnOnce(StateSyncWriteAck) + Send>) {
        on_ack(StateSyncWriteAck::new(&request.request_id, false, false, Some("No data worker store is registered.".into())));
    }
    fn flush(&self, _timeout: Duration) -> bool {
        false
    }
    fn set_post_commit_hook(&self, _hook: Box<dyn Fn(Vec<StateSyncEvent>) + Send + Sync>) {}
}

pub(crate) type AckFn = Arc<dyn Fn(&StateSyncWriteAck) + Send + Sync>;
pub(crate) type PostCommitFn = Arc<dyn Fn(Vec<StateSyncEvent>) + Send + Sync>;

/// The data-worker part of an engine (win-sync §8): stores per DB path and the request-id bookkeeping.
pub(crate) struct WorkerHub {
    stores: Mutex<HashMap<String, Arc<dyn DataWorkerStore>>>,
    enqueued: Mutex<HashSet<String>>,
    fallback_flushed: Mutex<HashSet<String>>,
    post_commit: PostCommitFn,
}

impl WorkerHub {
    pub(crate) fn new(post_commit: PostCommitFn) -> Self {
        Self {
            stores: Mutex::new(HashMap::new()),
            enqueued: Mutex::new(HashSet::new()),
            fallback_flushed: Mutex::new(HashSet::new()),
            post_commit,
        }
    }

    /// The store for `db` (full path, case-insensitive), created on demand through the registered factory.
    pub(crate) fn store(&self, db: &Path) -> Arc<dyn DataWorkerStore> {
        let key = full_path_key(db);
        // Created under the lock so one hub never owns two stores for one DB (the factory must not call back here).
        let mut g = self.stores.lock();
        if let Some(s) = g.get(&key) {
            return s.clone();
        }
        let full = std::path::absolute(db).unwrap_or_else(|_| db.to_path_buf());
        let factory = factory_slot().read().clone();
        let created: Arc<dyn DataWorkerStore> = match factory {
            Some(f) => f(&full),
            None => Arc::new(UnavailableStore),
        };
        let hook = self.post_commit.clone();
        created.set_post_commit_hook(Box::new(move |events| hook(events)));
        g.insert(key, created.clone());
        created
    }

    pub(crate) fn has_enqueued(&self, id: &str) -> bool {
        !id.is_empty() && self.enqueued.lock().contains(id)
    }

    /// C# `EnqueueWriteRequestOnEngine` (win-sync §8.2), with F4 (`FlushBarrier`/undefined kinds rejected).
    pub(crate) fn enqueue(&self, request: StateSyncWriteRequest, on_ack: Option<AckFn>, from_fallback: bool) {
        if request.db_path.trim().is_empty() {
            return;
        }
        let id = request.request_id.clone();
        if from_fallback {
            if !id.is_empty() {
                if !self.enqueued.lock().insert(id.clone()) {
                    return;
                }
                self.fallback_flushed.lock().insert(id.clone());
            }
        } else if !id.is_empty() {
            self.enqueued.lock().insert(id.clone());
            // The same request was already pushed by an explicit flush: ack this copy without executing it again.
            if self.fallback_flushed.lock().remove(&id) {
                if let Some(cb) = &on_ack {
                    cb(&StateSyncWriteAck::new(&id, true, false, None));
                }
                return;
            }
        }
        let store = self.store(Path::new(&request.db_path));
        if request.write_kind().is_none() {
            if let Some(cb) = &on_ack {
                cb(&StateSyncWriteAck::new(&id, false, false, Some(WriteKind::unknown_kind_error(&request.kind))));
            }
            return;
        }
        let mut request = request;
        // DTO lists are handed over only when non-empty.
        if request.batch_videos.as_ref().is_some_and(Vec::is_empty) {
            request.batch_videos = None;
        }
        if request.batch_progress.as_ref().is_some_and(Vec::is_empty) {
            request.batch_progress = None;
        }
        if request.batch_list_items.as_ref().is_some_and(Vec::is_empty) {
            request.batch_list_items = None;
        }
        let sender = request.sender_instance_id.clone();
        store.enqueue(
            request,
            Box::new(move |ack: StateSyncWriteAck| {
                if let Some(cb) = &on_ack {
                    cb(&ack);
                }
                if let Some(node) = live_node(&sender) {
                    node.node_handle_write_ack(&ack);
                }
            }),
        );
    }

    /// Flush (5 s) and dispose every store, then forget them (engine shutdown).
    pub(crate) fn shutdown(&self) {
        let stores: Vec<Arc<dyn DataWorkerStore>> = self.stores.lock().drain().map(|(_, s)| s).collect();
        for s in stores {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                s.flush(SHUTDOWN_FLUSH);
                s.dispose();
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WriteKind;

    /// C# `PendingWritesInSubmissionOrder`: resends, flush pushes and dispose hand-offs replay requests in the order
    /// they were submitted (a watch recorded before "clear history" is never applied after it), whatever the ids.
    #[test]
    fn pending_writes_replay_in_submission_order_and_keep_the_first_entry_per_id() {
        let p = PendingWrites::default();
        let mut ids = Vec::new();
        for i in 0..300 {
            let mut r = StateSyncWriteRequest::new("s", "db", WriteKind::UpsertVideo);
            r.request_id = format!("{:08x}", (i as u32).wrapping_mul(2_654_435_761));
            ids.push(r.request_id.clone());
            p.insert(&r);
        }
        let mut dup = StateSyncWriteRequest::new("s", "db", WriteKind::DeleteVideo);
        dup.request_id = ids[5].clone();
        p.insert(&dup);
        let order: Vec<StateSyncWriteRequest> = p.in_submission_order();
        assert_eq!(order.iter().map(|r| r.request_id.clone()).collect::<Vec<_>>(), ids);
        assert_eq!(order[5].kind, "UpsertVideo", "an existing id keeps its first entry (TryAdd)");

        p.apply_ack(&StateSyncWriteAck::new(&ids[0], false, false, Some("x".into())));
        assert_eq!(p.len(), 300, "a failed ack keeps the request pending");
        p.apply_ack(&StateSyncWriteAck::new(&ids[0], true, false, None));
        p.apply_ack(&StateSyncWriteAck::new("", true, false, None));
        assert_eq!(p.len(), 299);
        assert_eq!(p.in_submission_order()[0].request_id, ids[1]);
    }
}
