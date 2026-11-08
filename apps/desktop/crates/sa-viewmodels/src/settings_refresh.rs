//! `SettingsRefreshSubscription` (win-core §3): the Settings window never rebuilds its controls on job progress;
//! only settings-relevant changes, coalesced into one deferred refresh.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::types::SubtitleChange;

#[derive(Default)]
struct State {
    pending: bool,
    pending_refill: bool,
    disposed: bool,
}

/// Coalesces refresh requests (thread-safe).
pub struct SettingsRefreshSubscription {
    state: Arc<Mutex<State>>,
    enqueue: Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>,
    refresh: Arc<dyn Fn(bool) + Send + Sync>,
}

impl SettingsRefreshSubscription {
    /// `enqueue` posts to the UI loop; `refresh(refill)` re-reads controls (`refill` also rebuilds item lists).
    pub fn new(
        enqueue: impl Fn(Box<dyn FnOnce() + Send>) + Send + Sync + 'static,
        refresh: impl Fn(bool) + Send + Sync + 'static,
    ) -> Self {
        Self { state: Arc::new(Mutex::new(State::default())), enqueue: Arc::new(enqueue), refresh: Arc::new(refresh) }
    }

    /// Filter of view-model changes: `All` → refill, settings/models/providers → refresh, progress → ignored.
    pub fn on_view_model_change(&self, change: SubtitleChange) {
        match change {
            SubtitleChange::All => self.request_refresh(true),
            SubtitleChange::Settings | SubtitleChange::Models | SubtitleChange::Providers => self.request_refresh(false),
            _ => {}
        }
    }

    /// Thread-safe: coalesces into one deferred `refresh`; `refill` accumulates.
    pub fn request_refresh(&self, refill: bool) {
        {
            let mut g = self.state.lock();
            if g.disposed {
                return;
            }
            g.pending_refill |= refill;
            if g.pending {
                return;
            }
            g.pending = true;
        }
        let state = self.state.clone();
        let refresh = self.refresh.clone();
        (self.enqueue)(Box::new(move || {
            let refill = {
                let mut g = state.lock();
                if g.disposed {
                    return;
                }
                let r = g.pending_refill;
                g.pending_refill = false;
                g.pending = false;
                r
            };
            refresh(refill);
        }));
    }

    pub fn dispose(&self) {
        self.state.lock().disposed = true;
    }
}
