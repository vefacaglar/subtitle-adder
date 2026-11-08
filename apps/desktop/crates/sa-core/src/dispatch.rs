//! UI-thread dispatching (win-core §5). Core crates never reference Slint: they receive an
//! `Arc<dyn UiDispatcher>`; `sa-app` implements it with `slint::invoke_from_event_loop`.

use std::sync::Arc;

/// Marshals work onto the UI thread.
///
/// Contract relied on everywhere: every observable notification of the view models and the model catalog is raised
/// on the UI thread, including from cancelled and stale jobs.
pub trait UiDispatcher: Send + Sync {
    /// The caller is on the UI thread.
    fn has_thread_access(&self) -> bool;
    /// Queues `f` on the UI thread (FIFO; never inline, also when already on the UI thread). Dropped silently after
    /// the event loop quit.
    fn post(&self, f: Box<dyn FnOnce() + Send>);
}

/// Extension helpers over any dispatcher.
pub trait UiDispatcherExt: UiDispatcher {
    /// Runs `f` inline when on the UI thread, else posts it and **blocks** the calling worker until it ran
    /// (engine-event back-pressure, win-viewmodels §1.2). Returns `None` when the event loop is gone.
    fn invoke_blocking<R: Send + 'static>(&self, f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
        if self.has_thread_access() {
            return Some(f());
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.post(Box::new(move || {
            let _ = tx.send(f());
        }));
        rx.recv().ok()
    }
}

impl<T: UiDispatcher + ?Sized> UiDispatcherExt for T {}

/// Tests and design time: always "on the UI thread", `post` runs inline.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImmediateDispatcher;

impl ImmediateDispatcher {
    pub fn shared() -> Arc<dyn UiDispatcher> {
        Arc::new(ImmediateDispatcher)
    }
}

impl UiDispatcher for ImmediateDispatcher {
    fn has_thread_access(&self) -> bool {
        true
    }
    fn post(&self, f: Box<dyn FnOnce() + Send>) {
        f();
    }
}

/// A UI-thread stand-in for tests and tools (the C# `UiThread` test helper): the thread that created it is the "UI
/// thread"; `post` queues (FIFO, never inline) and the owner runs the queue with [`run_pending`](Self::run_pending)
/// or [`run_until`](Self::run_until). Posts after [`close`](Self::close) are dropped silently, like a quit event loop.
pub struct QueueDispatcher {
    owner: std::thread::ThreadId,
    queue: parking_lot::Mutex<std::collections::VecDeque<Box<dyn FnOnce() + Send>>>,
    ready: parking_lot::Condvar,
    closed: std::sync::atomic::AtomicBool,
}

impl QueueDispatcher {
    /// The calling thread becomes the UI thread.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            owner: std::thread::current().id(),
            queue: parking_lot::Mutex::new(std::collections::VecDeque::new()),
            ready: parking_lot::Condvar::new(),
            closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Runs everything queued so far (and what those closures post); returns how many ran.
    pub fn run_pending(&self) -> usize {
        let mut ran = 0;
        loop {
            let next = self.queue.lock().pop_front();
            match next {
                Some(f) => {
                    f();
                    ran += 1;
                }
                None => return ran,
            }
        }
    }

    /// Pumps the queue until `done()` holds or `timeout` passed; returns `done()`.
    pub fn run_until(&self, timeout: std::time::Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.run_pending();
            if done() {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return done();
            }
            let mut q = self.queue.lock();
            if q.is_empty() {
                let wait = (deadline - now).min(std::time::Duration::from_millis(20));
                let _ = self.ready.wait_for(&mut q, wait);
            }
        }
    }

    /// Drops queued work and every later post.
    pub fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::Release);
        self.queue.lock().clear();
    }
}

impl UiDispatcher for QueueDispatcher {
    fn has_thread_access(&self) -> bool {
        std::thread::current().id() == self.owner
    }
    fn post(&self, f: Box<dyn FnOnce() + Send>) {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        self.queue.lock().push_back(f);
        self.ready.notify_all();
    }
}
