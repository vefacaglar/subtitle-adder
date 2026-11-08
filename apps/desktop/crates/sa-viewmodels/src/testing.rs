//! Test support shared with other crates' tests: a UI dispatcher whose "UI thread" is the thread that created it and
//! whose queue is pumped explicitly (the C# `UiThread` helper / Swift main actor in tests).

use std::sync::Arc;
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use sa_core::dispatch::UiDispatcher;

type Job = Box<dyn FnOnce() + Send>;

/// A pumped dispatcher: `post` queues, `pump` runs the queue on the creating thread.
pub struct QueueDispatcher {
    thread: ThreadId,
    tx: Sender<Job>,
    rx: Receiver<Job>,
}

impl QueueDispatcher {
    /// The current thread becomes the UI thread.
    pub fn new() -> Arc<Self> {
        let (tx, rx) = unbounded();
        Arc::new(Self { thread: std::thread::current().id(), tx, rx })
    }

    /// Runs every queued closure (and the ones they post) once; returns how many ran.
    pub fn pump(&self) -> usize {
        assert_eq!(std::thread::current().id(), self.thread, "pump on the UI thread");
        let mut n = 0;
        while let Ok(job) = self.rx.try_recv() {
            job();
            n += 1;
        }
        n
    }

    /// Pumps until `cond()` holds or `timeout` passes.
    pub fn pump_until(&self, timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump();
            if cond() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            if let Ok(job) = self.rx.recv_timeout(Duration::from_millis(5)) {
                job();
            }
        }
    }

    /// Pumps for a while (lets workers run into their next blocking point).
    pub fn pump_for(&self, d: Duration) {
        let deadline = Instant::now() + d;
        while Instant::now() < deadline {
            self.pump();
            if let Ok(job) = self.rx.recv_timeout(Duration::from_millis(2)) {
                job();
            }
        }
    }
}

impl UiDispatcher for QueueDispatcher {
    fn has_thread_access(&self) -> bool {
        std::thread::current().id() == self.thread
    }
    fn post(&self, f: Box<dyn FnOnce() + Send>) {
        let _ = self.tx.send(f);
    }
}
