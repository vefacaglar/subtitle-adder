//! Observer lists and RAII subscriptions used by every observable in the workspace.
//!
//! Handlers are invoked on the notifying thread, **outside** any internal lock (C# `PropertyChanged` after the lock
//! is released). Dropping the [`Subscription`] unsubscribes; [`Subscription::detach`] keeps the handler for the
//! lifetime of the source.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;

type Handler<T> = Arc<dyn Fn(&T) + Send + Sync>;
type List<T> = Mutex<(u64, Vec<(u64, Handler<T>)>)>;

/// A thread-safe list of handlers for events of type `T`.
pub struct Subscribers<T: 'static> {
    list: Arc<List<T>>,
}

impl<T: 'static> Default for Subscribers<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: 'static> Subscribers<T> {
    pub fn new() -> Self {
        Self { list: Arc::new(Mutex::new((0, Vec::new()))) }
    }

    /// Adds a handler; it stays registered while the returned [`Subscription`] lives.
    pub fn subscribe(&self, handler: impl Fn(&T) + Send + Sync + 'static) -> Subscription {
        let id = {
            let mut g = self.list.lock();
            g.0 += 1;
            let id = g.0;
            g.1.push((id, Arc::new(handler)));
            id
        };
        let weak: Weak<List<T>> = Arc::downgrade(&self.list);
        Subscription::new(move || {
            if let Some(list) = weak.upgrade() {
                list.lock().1.retain(|(i, _)| *i != id);
            }
        })
    }

    /// Calls every handler (snapshot taken under the lock, handlers run outside it).
    pub fn notify(&self, event: &T) {
        let handlers: Vec<Handler<T>> = self.list.lock().1.iter().map(|(_, h)| h.clone()).collect();
        for h in handlers {
            h(event);
        }
    }

    /// Number of registered handlers.
    pub fn len(&self) -> usize {
        self.list.lock().1.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Unsubscribes when dropped.
#[must_use = "dropping a Subscription unsubscribes immediately"]
pub struct Subscription {
    cancel: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Subscription {
    /// A subscription that runs `cancel` once when dropped.
    pub fn new(cancel: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self { cancel: Some(Box::new(cancel)) }
    }

    /// A subscription that does nothing (for sources without events).
    pub fn empty() -> Self {
        Self { cancel: None }
    }

    /// Keeps the handler registered for the lifetime of the source.
    pub fn detach(mut self) {
        self.cancel = None;
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(c) = self.cancel.take() {
            c();
        }
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription").field("active", &self.cancel.is_some()).finish()
    }
}
