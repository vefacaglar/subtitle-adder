//! UI-thread object registry: workers never hold `Rc`s. They carry a `Copy + Send` [`UiKey`] and post closures that
//! look the object up in a thread-local table of the UI thread; on any other thread the lookup fails (no-op).

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sa_core::dispatch::{UiDispatcher, UiDispatcherExt};

thread_local! {
    static REGISTRY: RefCell<HashMap<u64, Box<dyn Any>>> = RefCell::new(HashMap::new());
}

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A `Send + Sync + Copy` handle to a UI-thread object.
pub struct UiKey<T: 'static> {
    id: u64,
    _t: PhantomData<fn() -> T>,
}

impl<T> Clone for UiKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for UiKey<T> {}

impl<T> std::fmt::Debug for UiKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UiKey({})", self.id)
    }
}

impl<T: 'static> UiKey<T> {
    /// Registers `rc` (weakly) on the current (UI) thread.
    pub fn register(rc: &Rc<T>) -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let weak: Weak<T> = Rc::downgrade(rc);
        REGISTRY.with(|r| r.borrow_mut().insert(id, Box::new(weak)));
        Self { id, _t: PhantomData }
    }

    /// The object, when called on its UI thread and still alive.
    pub fn get(&self) -> Option<Rc<T>> {
        REGISTRY.with(|r| r.borrow().get(&self.id).and_then(|b| b.downcast_ref::<Weak<T>>()).and_then(Weak::upgrade))
    }

    pub fn unregister(&self) {
        let _ = REGISTRY.try_with(|r| r.borrow_mut().remove(&self.id));
    }

    /// Posts `f` to the UI thread (FIFO).
    pub fn post(&self, ui: &Arc<dyn UiDispatcher>, f: impl FnOnce(&Rc<T>) + Send + 'static) {
        let key = *self;
        ui.post(Box::new(move || {
            if let Some(rc) = key.get() {
                f(&rc);
            }
        }));
    }

    /// Runs `f` on the UI thread and waits (worker back-pressure). `None` when the object or the loop is gone.
    pub fn invoke<R: Send + 'static>(&self, ui: &Arc<dyn UiDispatcher>, f: impl FnOnce(&Rc<T>) -> R + Send + 'static) -> Option<R> {
        let key = *self;
        ui.invoke_blocking(move || key.get().map(|rc| f(&rc))).flatten()
    }
}
