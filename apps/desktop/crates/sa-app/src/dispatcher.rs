//! `UiDispatcher` over the Slint event loop (win-core §5 `UiThreadDispatcher`).

use std::sync::Arc;
use std::thread::ThreadId;

use sa_core::dispatch::UiDispatcher;

/// Must be created on the UI thread.
pub struct SlintDispatcher {
    ui_thread: ThreadId,
}

impl SlintDispatcher {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { ui_thread: std::thread::current().id() })
    }
}

impl UiDispatcher for SlintDispatcher {
    fn has_thread_access(&self) -> bool {
        std::thread::current().id() == self.ui_thread
    }
    fn post(&self, f: Box<dyn FnOnce() + Send>) {
        // After the event loop quit the post is dropped silently (like a closed window's dispatcher).
        let _ = slint::invoke_from_event_loop(f);
    }
}
