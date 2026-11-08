//! Future aliases. The app has no async runtime: UI-side futures run on the Slint event loop
//! (`slint::spawn_local`), work runs on threads and reports back through [`crate::dispatch::UiDispatcher`].

use std::future::Future;
use std::pin::Pin;

/// A boxed future polled on the UI thread (rfd dialogs, alerts).
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// A boxed `Send` future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
