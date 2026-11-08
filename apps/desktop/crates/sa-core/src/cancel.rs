//! Cooperative cancellation shared by jobs, scans, playlist runs and store lanes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

type Callback = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Inner {
    cancelled: AtomicBool,
    callbacks: Mutex<Vec<Callback>>,
}

/// A clonable cancellation token. Cancelling runs every registered callback once (outside the lock), e.g. to
/// terminate an engine child process.
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Idempotent.
    pub fn cancel(&self) {
        if self.inner.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let callbacks = std::mem::take(&mut *self.inner.callbacks.lock());
        for c in callbacks {
            c();
        }
    }

    /// Runs `f` on cancellation (immediately when already cancelled).
    pub fn on_cancel(&self, f: impl FnOnce() + Send + 'static) {
        if self.is_cancelled() {
            f();
            return;
        }
        let mut g = self.inner.callbacks.lock();
        if self.is_cancelled() {
            drop(g);
            f();
        } else {
            g.push(Box::new(f));
        }
    }

    /// A token cancelled when `self` is cancelled (or by itself).
    pub fn child(&self) -> CancellationToken {
        let child = CancellationToken::new();
        let c = child.clone();
        self.on_cancel(move || c.cancel());
        child
    }
}

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken").field("cancelled", &self.is_cancelled()).finish()
    }
}
