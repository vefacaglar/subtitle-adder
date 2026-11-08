//! `sa-core`: the foundation crate of the Subtitle Adder desktop app (ARCHITECTURE.md §3).
//!
//! No UI toolkit, no OS bindings: localization (complete), settings contract, appearance and overlay text, UI-thread
//! dispatch contract, diagnostics, undo, the engine wire data model, subtitle import and the model catalog.

pub mod appearance;
pub mod atomic_file;
pub mod cancel;
pub mod diagnostics;
pub mod dispatch;
pub mod engine_types;
pub mod folding;
pub mod future;
pub mod l10n;
pub mod models;
pub mod platform;
pub mod secrets;
pub mod settings;
pub mod subscription;
pub mod subtitle_import;
pub mod text;
pub mod undo;

pub use cancel::CancellationToken;
pub use l10n::{Arg, LocalizedMessage, UiLanguage, k, l, l_args};
pub use subscription::{Subscribers, Subscription};
