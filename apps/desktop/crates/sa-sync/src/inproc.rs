//! In-process transport (win-sync §14): the macOS bus, and a pipe-free bus for tests on every platform.
//!
//! Buses that share a *channel scope* form one channel inside the process (the in-process analogue of the C# pipe
//! relay between brokers): a publish is dispatched synchronously to the publisher's own subscribers, then to every
//! other live bus of the channel; `eventId` dedupe applies per bus. The channel owns the data-worker hub (one store
//! per DB path, post-commit events dispatched to every bus of the channel); the last bus to dispose flushes and
//! disposes the stores ("last window closes the engine"). No pipe, no slot or lock files, no `W:`/`A:` traffic.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use sa_core::subscription::Subscription;

use crate::core::{
    self, AckFn, BrokerNode, DEFAULT_FLUSH, LocalDispatch, PendingWrites, WorkerHub, full_path_key, register_node,
    unregister_node,
};
use crate::wire::{StateSyncEvent, StateSyncWriteAck, StateSyncWriteRequest};
use crate::{DataWorkerFactory, DataWorkerStore, ProfileWriteBroker, SyncBus, SyncHandler, Topic, TopicFilter};

struct Channel {
    scope: String,
    members: Mutex<Vec<(String, Weak<Inner>)>>,
    running: AtomicBool,
    hub: WorkerHub,
}

impl Channel {
    fn live_members(&self) -> Vec<Arc<Inner>> {
        self.members.lock().iter().filter_map(|(_, w)| w.upgrade()).filter(|m| !m.disposed.load(Ordering::SeqCst)).collect()
    }
}

fn channels() -> &'static Mutex<HashMap<String, Arc<Channel>>> {
    static C: OnceLock<Mutex<HashMap<String, Arc<Channel>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn new_channel(scope: &str) -> Arc<Channel> {
    Arc::new_cyclic(|weak: &Weak<Channel>| {
        let weak = weak.clone();
        Channel {
            scope: scope.to_owned(),
            members: Mutex::new(Vec::new()),
            running: AtomicBool::new(true),
            hub: WorkerHub::new(Arc::new(move |events: Vec<StateSyncEvent>| {
                if let Some(ch) = weak.upgrade() {
                    for e in &events {
                        for m in ch.live_members() {
                            m.local.dispatch(e);
                        }
                    }
                }
            })),
        }
    })
}

struct Inner {
    id: String,
    channel: Arc<Channel>,
    local: LocalDispatch,
    pending: PendingWrites,
    disposed: AtomicBool,
    closed_channel_on_dispose: AtomicBool,
}

impl Inner {
    fn handle_ack(&self, ack: &StateSyncWriteAck) {
        self.pending.apply_ack(ack);
    }

    fn ack_fn(self: &Arc<Self>) -> AckFn {
        let weak = Arc::downgrade(self);
        Arc::new(move |ack: &StateSyncWriteAck| {
            if let Some(me) = weak.upgrade() {
                me.handle_ack(ack);
            }
        })
    }

    fn push_pending(self: &Arc<Self>, full_key: &str) {
        for req in self.pending.in_submission_order() {
            if full_path_key(Path::new(&req.db_path)) == full_key && !self.channel.hub.has_enqueued(&req.request_id) {
                self.channel.hub.enqueue(req, Some(self.ack_fn()), true);
            }
        }
    }
}

struct Node(Weak<Inner>);

impl BrokerNode for Node {
    fn node_handle_write_ack(&self, ack: &StateSyncWriteAck) {
        if let Some(i) = self.0.upgrade() {
            i.handle_ack(ack);
        }
    }
    fn node_push_pending_for_db(&self, full_key: &str) {
        if let Some(i) = self.0.upgrade() {
            i.push_pending(full_key);
        }
    }
    fn node_is_disposed(&self) -> bool {
        self.0.upgrade().is_none_or(|i| i.disposed.load(Ordering::SeqCst))
    }
}

/// In-process transport (macOS; also tests on every platform).
pub struct InProcessBus {
    inner: Arc<Inner>,
    // Keeps the registry entry alive while the bus lives.
    _node: Arc<Node>,
}

impl InProcessBus {
    /// A bus on a private channel of its own (isolated from every other bus).
    pub fn new() -> Arc<Self> {
        Self::join(&format!("inproc-{}", sa_core::text::guid_n()))
    }

    /// A bus on the named channel (trimmed; blank → [`crate::default_scope`]); buses with the same scope reach
    /// each other.
    pub fn with_scope(scope: &str) -> Arc<Self> {
        let s = scope.trim();
        if s.is_empty() { Self::join(&crate::default_scope()) } else { Self::join(s) }
    }

    /// The process-wide bus (default scope). After [`SyncBus::dispose`] it is a disposed no-op object.
    pub fn shared() -> &'static Arc<InProcessBus> {
        static S: OnceLock<Arc<InProcessBus>> = OnceLock::new();
        S.get_or_init(|| Self::join(&crate::default_scope()))
    }

    fn join(scope: &str) -> Arc<Self> {
        let id = sa_core::text::guid_n();
        loop {
            let channel = channels().lock().entry(scope.to_owned()).or_insert_with(|| new_channel(scope)).clone();
            let inner = Arc::new(Inner {
                id: id.clone(),
                channel: channel.clone(),
                local: LocalDispatch::new(),
                pending: PendingWrites::default(),
                disposed: AtomicBool::new(false),
                closed_channel_on_dispose: AtomicBool::new(false),
            });
            {
                let mut members = channel.members.lock();
                if channel.running.load(Ordering::SeqCst) {
                    members.push((id.clone(), Arc::downgrade(&inner)));
                    drop(members);
                    let node = Arc::new(Node(Arc::downgrade(&inner)));
                    let weak_node: Weak<dyn BrokerNode> = Arc::downgrade(&node) as Weak<dyn BrokerNode>;
                    register_node(&id, weak_node);
                    return Arc::new(Self { inner, _node: node });
                }
            }
            // A stopped channel: drop it from the map (only if still mapped) and retry.
            let mut map = channels().lock();
            if map.get(scope).is_some_and(|c| Arc::ptr_eq(c, &channel)) {
                map.remove(scope);
            }
        }
    }

    /// The resolved channel scope.
    pub fn channel_scope(&self) -> &str {
        &self.inner.channel.scope
    }

    /// C# `Publish(evt)`: publishes with the event's own topic, unchanged.
    pub fn publish_event(&self, event: StateSyncEvent) {
        if self.inner.disposed.load(Ordering::SeqCst) {
            return;
        }
        self.inner.local.dispatch(&event);
        for m in self.inner.channel.live_members() {
            if m.id != self.inner.id {
                m.local.dispatch(&event);
            }
        }
    }

    /// C# `Publish(topic, evt)`: the event with `topic` trimmed (blank → `profiles`).
    pub fn publish_to(&self, topic: &str, mut event: StateSyncEvent) {
        event.topic = core::normalize_publish_topic(topic);
        self.publish_event(event);
    }

    /// C# `Subscribe(topic, handler)` with a raw topic string (trimmed, blank → `*`, case-insensitive match).
    pub fn subscribe_topic(&self, topic: &str, handler: SyncHandler) -> Subscription {
        self.inner.local.subscribe(topic, Arc::from(handler))
    }

    /// True iff this bus's dispose shut the channel down (it was the last bus).
    pub fn closed_channel_on_dispose(&self) -> bool {
        self.inner.closed_channel_on_dispose.load(Ordering::SeqCst)
    }

    /// `!disposed && active_instance_count() <= 1`.
    pub fn is_last_instance(&self) -> bool {
        !self.inner.disposed.load(Ordering::SeqCst) && self.active_instance_count() <= 1
    }

    pub fn is_disposed(&self) -> bool {
        self.inner.disposed.load(Ordering::SeqCst)
    }
}

impl Drop for InProcessBus {
    fn drop(&mut self) {
        self.dispose();
    }
}

impl SyncBus for InProcessBus {
    fn publish(&self, topic: Topic, mut event: StateSyncEvent) {
        event.topic = topic.wire().into();
        self.publish_event(event);
    }

    fn subscribe(&self, filter: TopicFilter, handler: SyncHandler) -> Subscription {
        self.inner.local.subscribe(&core::filter_text(filter), Arc::from(handler))
    }

    fn active_instance_count(&self) -> usize {
        if !self.inner.channel.running.load(Ordering::SeqCst) {
            return 0;
        }
        self.inner.channel.live_members().len()
    }

    fn has_other_active_instances(&self) -> bool {
        let floor = if self.inner.disposed.load(Ordering::SeqCst) { 0 } else { 1 };
        self.active_instance_count() > floor
    }

    fn instance_id(&self) -> &str {
        &self.inner.id
    }

    fn dispose(&self) {
        let inner = &self.inner;
        if inner.disposed.load(Ordering::SeqCst) {
            return;
        }
        // Hand every not-yet-enqueued pending request to the hub before leaving.
        for req in inner.pending.in_submission_order() {
            if !inner.channel.hub.has_enqueued(&req.request_id) {
                inner.channel.hub.enqueue(req, None, true);
            }
        }
        if inner.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        unregister_node(&inner.id);
        let last = {
            let mut members = inner.channel.members.lock();
            members.retain(|(id, w)| *id != inner.id && w.upgrade().is_some_and(|m| !m.disposed.load(Ordering::SeqCst)));
            if members.is_empty() && inner.channel.running.swap(false, Ordering::SeqCst) {
                true
            } else {
                false
            }
        };
        if last {
            {
                let mut map = channels().lock();
                if map.get(&inner.channel.scope).is_some_and(|c| Arc::ptr_eq(c, &inner.channel)) {
                    map.remove(&inner.channel.scope);
                }
            }
            inner.channel.hub.shutdown();
            inner.closed_channel_on_dispose.store(true, Ordering::SeqCst);
        }
        inner.local.clear();
    }
}

impl ProfileWriteBroker for InProcessBus {
    fn submit_write_request(&self, mut request: StateSyncWriteRequest) {
        if self.inner.disposed.load(Ordering::SeqCst) {
            return;
        }
        if request.request_id.trim().is_empty() {
            request.request_id = sa_core::text::guid_n();
        }
        self.inner.pending.insert(&request);
        self.inner.channel.hub.enqueue(request, Some(self.inner.ack_fn()), false);
    }

    fn flush_pending_writes(&self, db: &Path, timeout: Option<Duration>) -> bool {
        if self.inner.disposed.load(Ordering::SeqCst) {
            return false;
        }
        self.inner.push_pending(&full_path_key(db));
        self.inner.channel.hub.store(db).flush(timeout.unwrap_or(DEFAULT_FLUSH))
    }

    fn data_worker_store(&self, db: &Path) -> Arc<dyn DataWorkerStore> {
        self.inner.channel.hub.store(db)
    }

    fn set_data_worker_factory(&self, factory: DataWorkerFactory) {
        core::set_factory(factory);
    }

    fn pending_write_request_count(&self) -> usize {
        self.inner.pending.len()
    }
}
