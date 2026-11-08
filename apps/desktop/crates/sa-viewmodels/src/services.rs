//! The dependencies every view model receives (constructed once by `sa-app`, per platform).

use std::sync::Arc;

use sa_core::diagnostics::DiagnosticSink;
use sa_core::dispatch::UiDispatcher;
use sa_core::platform::{PlatformShell, ThumbnailGenerator};
use sa_core::settings::SettingsStore;
use sa_enginekit::client::EngineClient;
use sa_enginekit::launch::EngineLaunchState;
use sa_media::MediaIndex;
use sa_profiles::ProfileStore;
use sa_providers::ProviderStore;
use sa_sync::SyncBus;

use crate::policy::PlatformPolicy;

/// Shared services. All are `Send + Sync`; blocking calls go to [`crate::lanes::StoreLanes`] or worker threads.
#[derive(Clone)]
pub struct AppServices {
    pub policy: Arc<PlatformPolicy>,
    pub settings: Arc<dyn SettingsStore>,
    pub ui: Arc<dyn UiDispatcher>,
    /// `None` when no engine was located (jobs fail with the localized not-found message).
    pub engine: Option<Arc<dyn EngineClient>>,
    pub launch_state: Arc<EngineLaunchState>,
    pub profiles: Arc<dyn ProfileStore>,
    pub media_index: Arc<dyn MediaIndex>,
    pub providers: Arc<dyn ProviderStore>,
    pub shell: Arc<dyn PlatformShell>,
    pub bus: Arc<dyn SyncBus>,
    pub diagnostics: Arc<dyn DiagnosticSink>,
    pub thumbnails: Arc<dyn ThumbnailGenerator>,
}
