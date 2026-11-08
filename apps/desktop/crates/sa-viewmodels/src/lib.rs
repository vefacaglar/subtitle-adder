//! `sa-viewmodels`: the view-model core shared by both front ends (UNIFIED.md §1.1 `app-core`, §6).
//!
//! No Slint, no OS bindings: views (`sa-app`) read state and call commands; every notification is raised on the UI
//! thread. Platform differences are injected (traits in `sa-core::platform` & co., knobs in [`policy`]).

pub mod explorer;
pub mod hub;
pub mod jobs;
pub mod lanes;
pub mod policy;
pub mod providers_ui;
pub mod services;
pub mod settings_refresh;
pub mod shortcuts;
pub mod subtitle;
pub mod types;
pub mod uiref;
#[doc(hidden)]
pub mod testing;

pub use policy::PlatformPolicy;
pub use services::AppServices;
pub use subtitle::SubtitleViewModel;
