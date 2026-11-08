//! The three serial store lanes (feed / actions / thumbnails; mac-ui §9.7), used on **both** platforms
//! (UNIFIED.md §1.3): the UI never blocks on SQLite, PBKDF2 or disk. Results come back through the dispatcher and are
//! applied only if their session generation is still current (checked by the caller's `apply`).

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Sender, unbounded};
use sa_core::dispatch::UiDispatcher;

/// Which lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    Feed,
    Actions,
    Thumbnails,
}

type Job = Box<dyn FnOnce() + Send>;

/// Three single-thread FIFO executors.
pub struct StoreLanes {
    ui: Arc<dyn UiDispatcher>,
    senders: [Sender<Job>; 3],
}

impl StoreLanes {
    pub fn new(ui: Arc<dyn UiDispatcher>) -> Arc<Self> {
        let make = |name: &str| {
            let (tx, rx) = unbounded::<Job>();
            let _ = std::thread::Builder::new().name(format!("sa-store-{name}")).spawn(move || {
                while let Ok(job) = rx.recv() {
                    // A panicking store call must not kill the lane.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                }
            });
            tx
        };
        Arc::new(Self { ui, senders: [make("feed"), make("actions"), make("thumbnails")] })
    }

    fn sender(&self, lane: Lane) -> &Sender<Job> {
        match lane {
            Lane::Feed => &self.senders[0],
            Lane::Actions => &self.senders[1],
            Lane::Thumbnails => &self.senders[2],
        }
    }

    /// Runs `work` on `lane` (after everything submitted before it), then `apply(result)` on the UI thread.
    pub fn run<R: Send + 'static>(
        &self,
        lane: Lane,
        work: impl FnOnce() -> R + Send + 'static,
        apply: impl FnOnce(R) + Send + 'static,
    ) {
        let ui = self.ui.clone();
        let _ = self.sender(lane).send(Box::new(move || {
            let r = work();
            ui.post(Box::new(move || apply(r)));
        }));
    }

    /// Runs `work` on `lane` without a UI continuation.
    pub fn spawn(&self, lane: Lane, work: impl FnOnce() + Send + 'static) {
        let _ = self.sender(lane).send(Box::new(work));
    }

    /// Waits up to `timeout` for `lane` to drain everything submitted so far (macOS `prepare_for_quit` ≤ 2 s).
    pub fn drain(&self, lane: Lane, timeout: Duration) -> bool {
        let (tx, rx) = crossbeam_channel::bounded::<()>(1);
        if self
            .sender(lane)
            .send(Box::new(move || {
                let _ = tx.send(());
            }))
            .is_err()
        {
            return false;
        }
        rx.recv_timeout(timeout).is_ok()
    }
}
