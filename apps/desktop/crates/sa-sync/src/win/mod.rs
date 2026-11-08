//! Windows named-pipe broker, wire- and file-compatible with the C# `StateSyncBroker` (win-sync §2–§10) with the
//! approved fixes F1–F5 and the required F6.
//!
//! * One *engine* per scope per process (process-wide map): it arbitrates hosting through the exclusive engine lock
//!   file, hosts the pipe server (two accept loops) while it holds the lock, owns the data-worker stores and counts
//!   instances through slot files.
//! * Every broker connects to the pipe as a client (also in the hosting process) and relays its publishes through
//!   it; the server relays each un-prefixed line to every *other* client verbatim.
//! * F1: post-commit events of a non-hosting process leave it through one of its connected brokers (the host relays
//!   them); F2: the last in-process broker flushes and disposes this process's stores even if other processes keep
//!   the channel alive; F3: 25 ms backoff when a server instance cannot be created; F4: `FlushBarrier`/undefined
//!   kinds rejected; F5: per-connection bounded outbound queues; F6: user-only pipe DACL + server owner check.

mod files;
mod pipe;
mod scope;
mod security;

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use sa_core::subscription::Subscription;

use crate::core::{
    self, AckFn, BrokerNode, DEFAULT_FLUSH, LocalDispatch, OutboundQueue, PendingWrites, WorkerHub, full_path_key,
    register_node, unregister_node,
};
use crate::lines::{LineDecoder, encode_line, is_blank};
use crate::wire::{self, ACK_PREFIX, StateSyncEvent, StateSyncWriteAck, StateSyncWriteRequest, WRITE_PREFIX};
use crate::{DataWorkerFactory, DataWorkerStore, ProfileWriteBroker, SyncBus, SyncHandler, Topic, TopicFilter};

use pipe::{Connection, Event, Pipe};

pub(crate) use scope::default_scope;

/// Client connect window per attempt.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(150);
/// Delay after any disconnect / failed attempt.
const RECONNECT_DELAY: Duration = Duration::from_millis(15);
/// Standby poll while another process hosts.
const STANDBY_POLL: Duration = Duration::from_millis(25);
/// F3 backoff after a failed server-instance creation.
const CREATE_BACKOFF: Duration = Duration::from_millis(25);
/// `WaitUntilReady` poll.
const READY_POLL: Duration = Duration::from_millis(10);
/// Read buffer.
const READ_BUFFER: usize = 4096;
/// Longest wait in `dispose` for lines already published to leave through the pipe (C# writes them synchronously).
const DISPOSE_DRAIN: Duration = Duration::from_millis(500);

fn engines() -> &'static Mutex<HashMap<String, Arc<Engine>>> {
    static E: OnceLock<Mutex<HashMap<String, Arc<Engine>>>> = OnceLock::new();
    E.get_or_init(|| Mutex::new(HashMap::new()))
}

fn resolve_scope(scope: Option<&str>) -> String {
    match scope.map(str::trim) {
        Some(s) if !s.is_empty() => s.to_owned(),
        _ => default_scope(),
    }
}

/// True when this process runs an engine for that scope (`None` = default scope).
pub fn is_channel_engine_running(scope: Option<&str>) -> bool {
    let hash = crate::scope_hash(&resolve_scope(scope));
    engines().lock().get(&hash).is_some_and(|e| e.running.load(Ordering::SeqCst))
}

/// `SubtitleAdder.StateBroker.<hash16>` (without the `\\.\pipe\` prefix).
pub fn pipe_name(scope: &str) -> String {
    format!("SubtitleAdder.StateBroker.{}", crate::scope_hash(scope))
}

// ---------------------------------------------------------------------------------------------------------------
// Engine

struct Lifecycle {
    /// Brokers of this process registered on the engine.
    registered: HashMap<String, Weak<BrokerInner>>,
}

struct Engine {
    hash: String,
    pipe_path: Vec<u16>,
    base_dir: PathBuf,
    instances_dir: PathBuf,
    running: AtomicBool,
    hosting: AtomicBool,
    cancel: Arc<Event>,
    lifecycle: Mutex<Lifecycle>,
    lock_file: Mutex<Option<File>>,
    server_streams: Mutex<HashMap<u64, Arc<Pipe>>>,
    clients: Mutex<HashMap<u64, Arc<Connection>>>,
    next_stream: AtomicU64,
    hub: WorkerHub,
    /// F6 descriptor; `None` (cannot build it) → this process never hosts.
    security: Option<security::ServerSecurity>,
    thread_done: (Mutex<bool>, Condvar),
}

impl Engine {
    fn start(hash: &str, pipe_name: &str, base_dir: &Path, instances_dir: &Path) -> Arc<Engine> {
        let _ = std::fs::create_dir_all(base_dir);
        let engine = Arc::new_cyclic(|weak: &Weak<Engine>| {
            let weak = weak.clone();
            Engine {
                hash: hash.to_owned(),
                pipe_path: pipe::pipe_path(pipe_name),
                base_dir: base_dir.to_owned(),
                instances_dir: instances_dir.to_owned(),
                running: AtomicBool::new(true),
                hosting: AtomicBool::new(false),
                cancel: Arc::new(Event::new().expect("CreateEventW")),
                lifecycle: Mutex::new(Lifecycle { registered: HashMap::new() }),
                lock_file: Mutex::new(None),
                server_streams: Mutex::new(HashMap::new()),
                clients: Mutex::new(HashMap::new()),
                next_stream: AtomicU64::new(1),
                hub: WorkerHub::new(Arc::new(move |events: Vec<StateSyncEvent>| {
                    if let Some(e) = weak.upgrade() {
                        e.broadcast_post_commit(events);
                    }
                })),
                security: security::ServerSecurity::new().ok(),
                thread_done: (Mutex::new(false), Condvar::new()),
            }
        });
        let e = engine.clone();
        let spawned = std::thread::Builder::new().name("sa-sync-engine".into()).spawn(move || {
            e.run_engine_loop();
            let (m, cv) = &e.thread_done;
            *m.lock() = true;
            cv.notify_all();
        });
        if spawned.is_err() {
            *engine.thread_done.0.lock() = true;
        }
        engine
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_set()
    }

    // --- lifecycle (§7.1, §7.6, §7.7) ---

    fn try_register(&self, id: &str, broker: Weak<BrokerInner>) -> bool {
        let mut g = self.lifecycle.lock();
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        g.registered.insert(id.to_owned(), broker);
        true
    }

    fn count_slots_or(&self, registered: usize) -> usize {
        files::count_active_slots(&self.instances_dir).unwrap_or(registered)
    }

    fn active_instance_count(&self) -> usize {
        let g = self.lifecycle.lock();
        if !self.running.load(Ordering::SeqCst) {
            return 0;
        }
        let reg = g.registered.len();
        reg.max(self.count_slots_or(reg))
    }

    fn registered_count(&self) -> usize {
        self.lifecycle.lock().registered.len()
    }

    fn registered_brokers(&self) -> Vec<Arc<BrokerInner>> {
        let g = self.lifecycle.lock();
        g.registered.values().filter_map(Weak::upgrade).filter(|b| !b.disposed.load(Ordering::SeqCst)).collect()
    }

    /// True when this call shut the engine down (it was the last instance on the channel).
    fn unregister_and_shutdown_if_last(&self, id: &str) -> bool {
        let release_stores;
        {
            let mut g = self.lifecycle.lock();
            g.registered.remove(id);
            if !self.running.load(Ordering::SeqCst) {
                return false;
            }
            let reg = g.registered.len();
            let remaining = reg.max(self.count_slots_or(reg));
            if remaining > 0 {
                // F2: other processes keep the channel alive, but this process has no broker left: commit and close
                // its stores now (the process is about to exit).
                release_stores = reg == 0;
            } else {
                self.running.store(false, Ordering::SeqCst);
                self.hosting.store(false, Ordering::SeqCst);
                release_stores = false;
            }
        }
        if release_stores {
            self.hub.shutdown();
            return false;
        }
        if self.running.load(Ordering::SeqCst) {
            return false;
        }
        self.shutdown_core();
        true
    }

    fn shutdown_core(&self) {
        self.cancel.set();
        self.hub.shutdown();
        self.lock_file.lock().take();
        let streams: Vec<Arc<Pipe>> = self.server_streams.lock().drain().map(|(_, p)| p).collect();
        for p in &streams {
            p.cancel_io();
        }
        drop(streams);
        let clients: Vec<Arc<Connection>> = self.clients.lock().drain().map(|(_, c)| c).collect();
        for c in &clients {
            c.close();
        }
        drop(clients);
        // Let the engine thread finish (it releases its references to the pipe instances), bounded.
        {
            let (m, cv) = &self.thread_done;
            let mut done = m.lock();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !*done {
                if cv.wait_until(&mut done, deadline).timed_out() {
                    break;
                }
            }
        }
        self.lock_file.lock().take();
        let lock = files::lock_path(&self.base_dir, &self.hash);
        if lock.exists() {
            let _ = std::fs::remove_file(&lock);
        }
        let is_empty = |p: &Path| std::fs::read_dir(p).is_ok_and(|mut rd| rd.next().is_none());
        if is_empty(&self.instances_dir) {
            let _ = std::fs::remove_dir(&self.instances_dir);
        }
        if is_empty(&self.base_dir) {
            let _ = std::fs::remove_dir(&self.base_dir);
        }
    }

    // --- hosting (§7.4, §7.5) ---

    fn run_engine_loop(self: &Arc<Self>) {
        while !self.is_cancelled() && self.running.load(Ordering::SeqCst) {
            let lock = if self.security.is_some() { files::try_acquire_lock(&self.base_dir, &self.hash) } else { None };
            match lock {
                Some(f) if self.running.load(Ordering::SeqCst) && !self.is_cancelled() => {
                    *self.lock_file.lock() = Some(f);
                    self.hosting.store(true, Ordering::SeqCst);
                    let a = self.clone();
                    let b = self.clone();
                    let t1 = std::thread::Builder::new().name("sa-sync-accept".into()).spawn(move || a.accept_loop());
                    let t2 = std::thread::Builder::new().name("sa-sync-accept".into()).spawn(move || b.accept_loop());
                    if let Ok(t) = t1 {
                        let _ = t.join();
                    }
                    if let Ok(t) = t2 {
                        let _ = t.join();
                    }
                    self.hosting.store(false, Ordering::SeqCst);
                    self.lock_file.lock().take();
                }
                Some(f) => drop(f),
                None => {
                    self.hosting.store(false, Ordering::SeqCst);
                    if self.cancel.wait(STANDBY_POLL) {
                        break;
                    }
                }
            }
        }
        self.hosting.store(false, Ordering::SeqCst);
    }

    fn accept_loop(self: &Arc<Self>) {
        let Some(sec) = self.security.as_ref() else { return };
        while !self.is_cancelled() && self.running.load(Ordering::SeqCst) {
            let server = match pipe::create_server_instance(&self.pipe_path, sec) {
                Ok(p) => Arc::new(p),
                Err(_) => {
                    if self.cancel.wait(CREATE_BACKOFF) {
                        break;
                    }
                    continue;
                }
            };
            let id = self.next_stream.fetch_add(1, Ordering::SeqCst);
            self.server_streams.lock().insert(id, server.clone());
            match server.accept(&[&self.cancel]) {
                Ok(()) => {
                    if self.is_cancelled() || !self.running.load(Ordering::SeqCst) {
                        self.server_streams.lock().remove(&id);
                        break;
                    }
                    // Register the session before its reader thread starts, so a line relayed right after the
                    // client sees itself connected already reaches it (the C# host registers on its own task, later).
                    let Ok(conn) = Connection::start(server) else {
                        self.server_streams.lock().remove(&id);
                        continue;
                    };
                    self.clients.lock().insert(id, conn.clone());
                    let e = self.clone();
                    let c = conn.clone();
                    let spawned = std::thread::Builder::new()
                        .name("sa-sync-session".into())
                        .spawn(move || e.handle_connected_client(id, c));
                    if spawned.is_err() {
                        self.clients.lock().remove(&id);
                        self.server_streams.lock().remove(&id);
                        conn.close();
                    }
                }
                Err(_) => {
                    self.server_streams.lock().remove(&id);
                    if self.is_cancelled() || !self.running.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
        }
    }

    fn handle_connected_client(self: &Arc<Self>, id: u64, conn: Arc<Connection>) {
        let mut decoder = LineDecoder::new();
        let mut buf = vec![0u8; READ_BUFFER];
        let mut lines = Vec::new();
        let mut eof = false;
        'read: while !eof && !self.is_cancelled() && self.running.load(Ordering::SeqCst) {
            lines.clear();
            match conn.read(&mut buf, &self.cancel) {
                Err(_) => break,
                // End of stream: an unterminated last line still counts (`ReadLine`).
                Ok(0) => {
                    eof = true;
                    lines.extend(decoder.finish_stream());
                }
                Ok(n) => {
                    if decoder.push(&buf[..n], &mut lines).is_err() {
                        break;
                    }
                }
            }
            for line in lines.drain(..) {
                if is_blank(&line) {
                    continue;
                }
                if let Some(json) = line.strip_prefix(WRITE_PREFIX) {
                    if let Some(req) = wire::parse_write_request(json) {
                        let weak = Arc::downgrade(self);
                        let on_ack: AckFn = Arc::new(move |ack: &StateSyncWriteAck| {
                            if let Some(e) = weak.upgrade() {
                                e.send_line_to_client(id, &wire::ack_line(ack));
                            }
                        });
                        self.hub.enqueue(req, Some(on_ack), false);
                    }
                    continue;
                }
                self.broadcast(&line, Some(id));
                if self.is_cancelled() {
                    break 'read;
                }
            }
        }
        self.clients.lock().remove(&id);
        self.server_streams.lock().remove(&id);
        conn.close();
    }

    fn send_line_to_client(&self, id: u64, line: &str) {
        let Some(conn) = self.clients.lock().get(&id).cloned() else { return };
        if !conn.try_send(Arc::from(encode_line(line))) {
            self.clients.lock().remove(&id);
        }
    }

    /// Broadcast to every connected client (`exclude` = the sender's session).
    fn broadcast(&self, line: &str, exclude: Option<u64>) {
        let bytes: Arc<[u8]> = Arc::from(encode_line(line));
        let targets: Vec<(u64, Arc<Connection>)> =
            self.clients.lock().iter().filter(|(k, _)| Some(**k) != exclude).map(|(k, c)| (*k, c.clone())).collect();
        for (k, c) in targets {
            if !c.try_send(bytes.clone()) {
                self.clients.lock().remove(&k);
            }
        }
    }

    /// §8.5 + F1.
    fn broadcast_post_commit(&self, events: Vec<StateSyncEvent>) {
        for evt in &events {
            let brokers = self.registered_brokers();
            for b in &brokers {
                b.local.dispatch(evt);
            }
            let Some(line) = wire::event_line(evt) else { continue };
            if self.hosting.load(Ordering::SeqCst) {
                self.broadcast(&line, None);
            } else if !brokers.iter().any(|b| b.try_send_line(&line)) {
                // F1: no connected broker right now — queue on one so it leaves on reconnect.
                if let Some(b) = brokers.first() {
                    b.outbound.push_back(line);
                }
            }
        }
    }

    fn connected_client_count(&self) -> usize {
        self.clients.lock().len()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Broker

struct BrokerInner {
    id: String,
    scope: String,
    pipe_path: Vec<u16>,
    engine: Arc<Engine>,
    local: LocalDispatch,
    outbound: OutboundQueue,
    pending: PendingWrites,
    /// `client_write_lock` + active stream.
    client: Mutex<Option<Arc<Connection>>>,
    connected: AtomicBool,
    disposed: AtomicBool,
    closed_engine_on_dispose: AtomicBool,
    cancel: Event,
    slot: Mutex<Option<File>>,
}

impl BrokerInner {
    fn is_disposed(&self) -> bool {
        self.disposed.load(Ordering::SeqCst)
    }

    /// C# `TrySendToEnginePipe` (F5: queued on the connection).
    fn try_send_line(&self, line: &str) -> bool {
        let g = self.client.lock();
        let Some(conn) = g.as_ref() else { return false };
        if !self.connected.load(Ordering::SeqCst) || !conn.is_open() {
            return false;
        }
        if conn.try_send(Arc::from(encode_line(line))) {
            true
        } else {
            self.connected.store(false, Ordering::SeqCst);
            false
        }
    }

    fn publish_event(&self, event: StateSyncEvent) {
        if self.is_disposed() {
            return;
        }
        self.local.dispatch(&event);
        if let Some(line) = wire::event_line(&event)
            && !self.try_send_line(&line)
        {
            self.outbound.push_back(line);
        }
    }

    fn handle_write_ack(&self, ack: &StateSyncWriteAck) {
        self.pending.apply_ack(ack);
    }

    fn ack_fn(self: &Arc<Self>) -> AckFn {
        let weak = Arc::downgrade(self);
        Arc::new(move |ack: &StateSyncWriteAck| {
            if let Some(me) = weak.upgrade() {
                me.handle_write_ack(ack);
            }
        })
    }

    fn resend_pending_write_requests(&self) {
        for req in self.pending.in_submission_order() {
            let Some(line) = wire::write_request_line(&req) else { break };
            if !self.try_send_line(&line) {
                break;
            }
        }
    }

    fn flush_pending_outbound(&self) {
        self.resend_pending_write_requests();
        while let Some(line) = self.outbound.pop_front() {
            if !self.try_send_line(&line) {
                self.outbound.push_back(line);
                break;
            }
        }
    }

    fn push_pending_for_db(self: &Arc<Self>, full_key: &str) {
        for req in self.pending.in_submission_order() {
            if full_path_key(Path::new(&req.db_path)) == full_key && !self.engine.hub.has_enqueued(&req.request_id) {
                self.engine.hub.enqueue(req, Some(self.ack_fn()), true);
            }
        }
    }

    fn submit_write_request(self: &Arc<Self>, mut request: StateSyncWriteRequest) {
        if self.is_disposed() {
            return;
        }
        if request.request_id.trim().is_empty() {
            request.request_id = sa_core::text::guid_n();
        }
        self.pending.insert(&request);
        let e = &self.engine;
        let in_process = e.running.load(Ordering::SeqCst)
            && (e.hosting.load(Ordering::SeqCst) || e.registered_count() >= 1 || e.active_instance_count() >= 1);
        if in_process {
            e.hub.enqueue(request, Some(self.ack_fn()), false);
            return;
        }
        if let Some(line) = wire::write_request_line(&request) {
            let _ = self.try_send_line(&line);
        }
    }

    /// §7.3, run on the broker's own thread until disposed.
    fn run_client_loop(self: Arc<Self>) {
        let mut buf = vec![0u8; READ_BUFFER];
        while !self.is_disposed() && !self.cancel.is_set() {
            if let Ok(p) = pipe::connect_client(&self.pipe_path, CONNECT_TIMEOUT, &self.cancel)
                && security::server_owned_by_current_user(p.raw())
                && let Ok(conn) = Connection::start(Arc::new(p))
            {
                let accepted = {
                    let mut g = self.client.lock();
                    if self.is_disposed() || self.cancel.is_set() {
                        conn.close();
                        false
                    } else {
                        *g = Some(conn.clone());
                        self.connected.store(true, Ordering::SeqCst);
                        true
                    }
                };
                if !accepted {
                    break;
                }
                self.flush_pending_outbound();
                self.read_until_disconnect(&conn, &mut buf);
                {
                    let mut g = self.client.lock();
                    if g.as_ref().is_some_and(|c| Arc::ptr_eq(c, &conn)) {
                        *g = None;
                    }
                    self.connected.store(false, Ordering::SeqCst);
                }
                conn.close();
            }
            if self.is_disposed() || self.cancel.wait(RECONNECT_DELAY) {
                break;
            }
        }
    }

    fn read_until_disconnect(&self, conn: &Connection, buf: &mut [u8]) {
        let mut decoder = LineDecoder::new();
        let mut lines = Vec::new();
        let mut eof = false;
        while !eof && !self.is_disposed() && conn.is_open() {
            lines.clear();
            match conn.read(buf, &self.cancel) {
                Err(_) => return,
                // End of stream: an unterminated last line still counts (`ReadLine`).
                Ok(0) => {
                    eof = true;
                    lines.extend(decoder.finish_stream());
                }
                Ok(n) => {
                    if decoder.push(&buf[..n], &mut lines).is_err() {
                        return;
                    }
                }
            }
            for line in lines.drain(..) {
                if is_blank(&line) {
                    continue;
                }
                if let Some(json) = line.strip_prefix(ACK_PREFIX) {
                    if let Some(ack) = wire::parse_ack(json) {
                        self.handle_write_ack(&ack);
                    }
                    continue;
                }
                if let Some(evt) = wire::parse_event(&line)
                    && evt.sender_instance_id != self.id
                {
                    self.local.dispatch(&evt);
                }
            }
        }
    }

    fn dispose(&self) {
        if self.is_disposed() {
            return;
        }
        for req in self.pending.in_submission_order() {
            if !self.engine.hub.has_enqueued(&req.request_id) {
                self.engine.hub.enqueue(req, None, true);
            }
        }
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        unregister_node(&self.id);
        // Lines this broker already handed to its connection still reach the pipe (bounded), before the client loop
        // is cancelled (it closes the connection on its way out).
        let client = {
            let mut g = self.client.lock();
            self.connected.store(false, Ordering::SeqCst);
            g.take()
        };
        if let Some(c) = client {
            c.close_after_drain(DISPOSE_DRAIN);
        }
        self.cancel.set();
        self.slot.lock().take();
        let closed = self.engine.unregister_and_shutdown_if_last(&self.id);
        if closed {
            let mut map = engines().lock();
            if map.get(&self.engine.hash).is_some_and(|e| Arc::ptr_eq(e, &self.engine)) {
                map.remove(&self.engine.hash);
            }
        }
        self.closed_engine_on_dispose.store(closed, Ordering::SeqCst);
        self.local.clear();
        self.outbound.clear();
    }
}

struct Node(Weak<BrokerInner>);

impl BrokerNode for Node {
    fn node_handle_write_ack(&self, ack: &StateSyncWriteAck) {
        if let Some(b) = self.0.upgrade() {
            b.handle_write_ack(ack);
        }
    }
    fn node_push_pending_for_db(&self, full_key: &str) {
        if let Some(b) = self.0.upgrade()
            && !b.is_disposed()
        {
            b.push_pending_for_db(full_key);
        }
    }
    fn node_is_disposed(&self) -> bool {
        self.0.upgrade().is_none_or(|b| b.is_disposed())
    }
}

/// Windows named-pipe broker, C#-compatible (win-sync §3–§8).
pub struct NamedPipeBus {
    inner: Arc<BrokerInner>,
    _node: Arc<Node>,
}

impl NamedPipeBus {
    /// `scope = None` (or blank) → [`crate::default_scope`]. Creates the slot file, joins (or starts) this process's
    /// engine for the scope and starts the client loop.
    pub fn new(scope: Option<&str>) -> Arc<Self> {
        let id = sa_core::text::guid_n();
        let scope = resolve_scope(scope);
        let hash = crate::scope_hash(&scope);
        let name = format!("SubtitleAdder.StateBroker.{hash}");
        let base = crate::broker_dir(&std::env::temp_dir(), &scope);
        let instances = base.join("instances");
        let _ = std::fs::create_dir_all(&instances);
        let slot = files::create_slot(&instances, &id);

        let inner = Arc::new_cyclic(|weak: &Weak<BrokerInner>| {
            let engine = loop {
                let engine = engines()
                    .lock()
                    .entry(hash.clone())
                    .or_insert_with(|| Engine::start(&hash, &name, &base, &instances))
                    .clone();
                if engine.try_register(&id, weak.clone()) {
                    break engine;
                }
                let mut map = engines().lock();
                if map.get(&hash).is_some_and(|e| Arc::ptr_eq(e, &engine)) {
                    map.remove(&hash);
                }
            };
            BrokerInner {
                id: id.clone(),
                scope: scope.clone(),
                pipe_path: pipe::pipe_path(&name),
                engine,
                local: LocalDispatch::new(),
                outbound: OutboundQueue::default(),
                pending: PendingWrites::default(),
                client: Mutex::new(None),
                connected: AtomicBool::new(false),
                disposed: AtomicBool::new(false),
                closed_engine_on_dispose: AtomicBool::new(false),
                cancel: Event::new().expect("CreateEventW"),
                slot: Mutex::new(slot),
            }
        });
        let node = Arc::new(Node(Arc::downgrade(&inner)));
        register_node(&id, Arc::downgrade(&node) as Weak<dyn BrokerNode>);
        let runner = inner.clone();
        let _ = std::thread::Builder::new().name("sa-sync-client".into()).spawn(move || runner.run_client_loop());
        Arc::new(Self { inner, _node: node })
    }

    /// The process-wide broker (default scope). The app disposes it when the main window closes; afterwards it is a
    /// disposed no-op object.
    pub fn shared() -> &'static Arc<NamedPipeBus> {
        static S: OnceLock<Arc<NamedPipeBus>> = OnceLock::new();
        S.get_or_init(|| NamedPipeBus::new(None))
    }

    /// `!disposed && engine running && connected`.
    pub fn is_ready(&self) -> bool {
        !self.inner.is_disposed() && self.inner.engine.running.load(Ordering::SeqCst) && self.inner.connected.load(Ordering::SeqCst)
    }

    /// Polls [`Self::is_ready`] every 10 ms until `timeout` or dispose; returns the final value.
    pub fn wait_until_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.inner.is_disposed() && Instant::now() < deadline {
            if self.is_ready() {
                return true;
            }
            std::thread::sleep(READY_POLL);
        }
        self.is_ready()
    }

    /// The resolved channel scope.
    pub fn channel_scope(&self) -> &str {
        &self.inner.scope
    }

    /// The engine of this broker runs (also after this broker's dispose).
    pub fn is_engine_running(&self) -> bool {
        self.inner.engine.running.load(Ordering::SeqCst)
    }

    /// This process's engine hosts the pipe server.
    pub fn is_leader(&self) -> bool {
        !self.inner.is_disposed() && self.inner.engine.hosting.load(Ordering::SeqCst)
    }

    pub fn is_connected_to_leader(&self) -> bool {
        !self.inner.is_disposed() && self.inner.connected.load(Ordering::SeqCst)
    }

    /// Client sessions hosted by this process's engine (0 when another process hosts).
    pub fn connected_client_count(&self) -> usize {
        self.inner.engine.connected_client_count()
    }

    /// `!disposed && active_instance_count() <= 1`.
    pub fn is_last_instance(&self) -> bool {
        !self.inner.is_disposed() && self.active_instance_count() <= 1
    }

    /// True iff this broker's dispose shut the engine down.
    pub fn closed_engine_on_dispose(&self) -> bool {
        self.inner.closed_engine_on_dispose.load(Ordering::SeqCst)
    }

    pub fn is_disposed(&self) -> bool {
        self.inner.is_disposed()
    }

    /// Resends every unacknowledged request as `W:` lines in submission order.
    pub fn resend_pending_write_requests(&self) {
        self.inner.resend_pending_write_requests();
    }

    /// C# `Publish(evt)`: the event's own topic, unchanged.
    pub fn publish_event(&self, event: StateSyncEvent) {
        self.inner.publish_event(event);
    }

    /// C# `Publish(topic, evt)`: the event with `topic` trimmed (blank → `profiles`).
    pub fn publish_to(&self, topic: &str, mut event: StateSyncEvent) {
        event.topic = core::normalize_publish_topic(topic);
        self.inner.publish_event(event);
    }

    /// C# `Subscribe(topic, handler)` with a raw topic (trimmed, blank → `*`, case-insensitive match).
    pub fn subscribe_topic(&self, topic: &str, handler: SyncHandler) -> Subscription {
        self.inner.local.subscribe(topic, Arc::from(handler))
    }

    /// Lines waiting for a connection (diagnostics/tests).
    pub fn queued_outbound_count(&self) -> usize {
        self.inner.outbound.len()
    }

    /// `<temp>\SubtitleAdder\broker-<hash16>`.
    pub fn base_dir(&self) -> &Path {
        &self.inner.engine.base_dir
    }
}

impl Drop for NamedPipeBus {
    fn drop(&mut self) {
        self.inner.dispose();
    }
}

impl SyncBus for NamedPipeBus {
    fn publish(&self, topic: Topic, mut event: StateSyncEvent) {
        event.topic = topic.wire().into();
        self.inner.publish_event(event);
    }

    fn subscribe(&self, filter: TopicFilter, handler: SyncHandler) -> Subscription {
        self.inner.local.subscribe(&core::filter_text(filter), Arc::from(handler))
    }

    fn active_instance_count(&self) -> usize {
        self.inner.engine.active_instance_count()
    }

    fn has_other_active_instances(&self) -> bool {
        let floor = if self.inner.is_disposed() { 0 } else { 1 };
        self.active_instance_count() > floor
    }

    fn instance_id(&self) -> &str {
        &self.inner.id
    }

    fn dispose(&self) {
        self.inner.dispose();
    }
}

impl ProfileWriteBroker for NamedPipeBus {
    fn submit_write_request(&self, request: StateSyncWriteRequest) {
        self.inner.submit_write_request(request);
    }

    fn flush_pending_writes(&self, db: &Path, timeout: Option<Duration>) -> bool {
        if self.inner.is_disposed() {
            return false;
        }
        self.inner.push_pending_for_db(&full_path_key(db));
        self.inner.engine.hub.store(db).flush(timeout.unwrap_or(DEFAULT_FLUSH))
    }

    fn data_worker_store(&self, db: &Path) -> Arc<dyn DataWorkerStore> {
        self.inner.engine.hub.store(db)
    }

    fn set_data_worker_factory(&self, factory: DataWorkerFactory) {
        core::set_factory(factory);
    }

    fn pending_write_request_count(&self) -> usize {
        self.inner.pending.len()
    }
}

/// Test-only access to internals (F6 checks, raw pipe clients).
#[doc(hidden)]
pub mod testing {
    use super::*;

    /// SDDL (owner + DACL) of a server instance of this process's engine for `scope`, created on the fly.
    pub fn server_instance_sddl(scope: &str) -> std::io::Result<String> {
        let sec = security::ServerSecurity::new()?;
        let path = pipe::pipe_path(&format!("{}.sddl-probe.{}", pipe_name(scope), sa_core::text::guid_n()));
        let p = pipe::create_server_instance(&path, &sec)?;
        security::handle_sddl(p.raw())
    }

    /// The SDDL string used for server instances.
    pub fn expected_server_sddl() -> Option<String> {
        security::token_sids().map(security::server_sddl)
    }

    /// The token user SID (SDDL form).
    pub fn current_user_sid() -> Option<String> {
        security::token_sids().map(|s| s.user_sddl.clone())
    }

    /// A raw client connection to the channel's pipe (verifies the server owner like the broker does). A background
    /// thread keeps reading (a peer that stops reading would stall a C# host, whose writes are synchronous).
    pub struct RawClient {
        conn: Arc<Connection>,
        lines: crossbeam_channel::Receiver<String>,
    }

    impl RawClient {
        pub fn connect(scope: &str, timeout: Duration) -> std::io::Result<RawClient> {
            let cancel = Event::new()?;
            let path = pipe::pipe_path(&pipe_name(scope));
            let deadline = Instant::now() + timeout;
            let pipe = loop {
                match pipe::connect_client(&path, CONNECT_TIMEOUT, &cancel) {
                    Ok(p) => break p,
                    Err(e) if Instant::now() >= deadline => return Err(e),
                    Err(_) => std::thread::sleep(RECONNECT_DELAY),
                }
            };
            if !security::server_owned_by_current_user(pipe.raw()) {
                return Err(std::io::Error::other("pipe server not owned by the current user"));
            }
            let conn = Connection::start(Arc::new(pipe))?;
            let (tx, rx) = crossbeam_channel::unbounded();
            let reader = conn.clone();
            std::thread::spawn(move || {
                let mut decoder = LineDecoder::new();
                let mut buf = vec![0u8; READ_BUFFER];
                let mut out = Vec::new();
                while let Ok(n) = reader.read(&mut buf, &cancel) {
                    if n == 0 || decoder.push(&buf[..n], &mut out).is_err() {
                        break;
                    }
                    for l in out.drain(..) {
                        if !is_blank(&l) && tx.send(l).is_err() {
                            return;
                        }
                    }
                }
            });
            Ok(RawClient { conn, lines: rx })
        }

        pub fn send_line(&self, line: &str) -> bool {
            self.conn.try_send(Arc::from(encode_line(line)))
        }

        /// Sends `bytes` as they are (framing tests).
        pub fn send_raw(&self, bytes: &[u8]) -> bool {
            self.conn.try_send(Arc::from(bytes))
        }

        /// Next non-blank line, waiting up to `timeout`.
        pub fn read_line(&mut self, timeout: Duration) -> Option<String> {
            self.lines.recv_timeout(timeout).ok()
        }

        pub fn close(&self) {
            self.conn.close();
        }
    }
}
