//! macOS platform services. Only the `objc2` runtime and Core Foundation are used (messages by selector, classes
//! looked up by name), never framework binding crates, so this module type-checks for aarch64 on the Windows dev
//! machine (ARCHITECTURE.md §11); the pure rules live in [`crate::mac_rules`] and are tested on every platform.
//!
//! * [`MacShell`]: Application Support, preferred languages (global `AppleLanguages`), per-app `AppleLanguages`,
//!   `NSFontManager` families, quick locations (Movies / Downloads / Desktop / volumes), volume checks, NSWorkspace
//!   reveal/open, physical memory, rfd dialogs and the alert fallback.
//! * [`defaults::UserDefaultsSettingsStore`]: CFPreferences on the Swift app's domain.
//! * [`open_documents::OpenDocumentsHandler`]: Apple Event `odoc` → [`crate::coalescer::CoalescingDelivery`].
//! * [`appkit`]: process preparation, `NSApp.appearance`, title-bar double click, full screen, standard menu actions
//!   and the Services / Window / Help menu roles.
//! * [`symbols`]: SF Symbols rasterizer; [`thumbnails`]: AVAssetImageGenerator card pictures.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sa_core::diagnostics::DiagnosticSink;
use sa_core::engine_types::SubtitleFormat;
use sa_core::future::LocalBoxFuture;
use sa_core::l10n::UiLanguage;
use sa_core::platform::{Alert, AlertResponse, FileFilter, PlatformShell, QuickLocation};

pub mod appkit;
pub mod defaults;
pub mod ffi;
pub mod folders;
pub mod open_documents;
pub mod symbols;
pub mod thumbnails;

pub use thumbnails::AssetThumbnailGenerator;

/// The packaged bundle id = the Swift app's UserDefaults domain and Application Support identity.
pub const BUNDLE_ID: &str = "app.subtitleadder.mac";
/// The domain of an unbundled development run (the Swift dev build's executable name).
pub const DEV_DOMAIN: &str = "SubtitleAdderApp";

/// The macOS shell.
pub struct MacShell {
    diagnostics: Arc<dyn DiagnosticSink>,
}

impl MacShell {
    pub fn new(diagnostics: Arc<dyn DiagnosticSink>) -> Self {
        Self { diagnostics }
    }

    /// `<X>.app` when the exe runs from `<X>.app/Contents/MacOS/`.
    pub fn app_bundle() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let macos = exe.parent()?;
        let contents = macos.parent()?;
        let bundle = contents.parent()?;
        (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && bundle.extension()? == "app")
            .then(|| bundle.to_path_buf())
    }

    /// Defaults domain: `app.subtitleadder.mac` when bundled, `SubtitleAdderApp` for an unbundled dev run.
    pub fn defaults_domain() -> &'static str {
        if Self::app_bundle().is_some() { BUNDLE_ID } else { DEV_DOMAIN }
    }

    /// SIGPIPE ignored, activation policy regular, app activated (mac-ui §2.1 steps 1–2). Main thread.
    pub fn prepare_process() {
        appkit::prepare_process();
    }

    /// `NSApp.appearance` for `appTheme` (`system|light|dark`, unknown → system). Main thread.
    pub fn apply_app_theme(theme: Option<&str>) {
        appkit::apply_appearance(theme);
    }
}

impl PlatformShell for MacShell {
    fn app_data_dir(&self) -> PathBuf {
        folders::application_support().join("SubtitleAdder")
    }
    fn diagnostics(&self) -> &dyn DiagnosticSink {
        self.diagnostics.as_ref()
    }
    fn system_preferred_languages(&self) -> Vec<String> {
        // EngineKit `systemPreferredLanguages`: the global domain's list, else `Locale.preferredLanguages`.
        let global = defaults::global_apple_languages();
        if !global.is_empty() {
            return global;
        }
        objc2::rc::autoreleasepool(|_| unsafe {
            let list: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> =
                objc2::msg_send![objc2::class!(NSLocale), preferredLanguages];
            list.map(|l| ffi::array_items(&l).iter().map(|s| ffi::rust_string(s)).collect()).unwrap_or_default()
        })
    }
    fn apply_process_ui_language(&self, language: Option<UiLanguage>) {
        // Explicit choice: per-app `AppleLanguages = [code]`; System: removed (system menus/panels follow on relaunch).
        let codes = language.map(|l| vec![l.code().to_owned()]);
        defaults::set_app_string_array(Self::defaults_domain(), "AppleLanguages", codes.as_deref());
    }
    fn installed_font_families(&self) -> Vec<String> {
        appkit::font_families()
    }
    fn quick_locations(&self) -> Vec<QuickLocation> {
        folders::quick_locations()
    }
    fn storage_available(&self, path: &Path) -> bool {
        crate::mac_rules::storage_available(&path.to_string_lossy(), &folders::is_volume_mounted)
    }
    fn reveal_in_file_manager(&self, path: &Path) {
        appkit::reveal_in_finder(path);
    }
    fn open_folder(&self, path: &Path) {
        appkit::open_url(path);
    }
    fn total_physical_memory(&self) -> u64 {
        appkit::physical_memory()
    }
    fn other_app_instances_running(&self) -> bool {
        // Launch Services keeps one process per bundle; the temp-cleanup rule is Windows-only.
        false
    }
    fn pick_open_file(&self, filter: FileFilter) -> LocalBoxFuture<'static, Option<PathBuf>> {
        Box::pin(crate::dialogs::pick_open_file(filter, None))
    }
    fn pick_open_files(&self, filter: FileFilter) -> LocalBoxFuture<'static, Vec<PathBuf>> {
        Box::pin(crate::dialogs::pick_open_files(filter, None))
    }
    fn pick_save_file(&self, suggested_name: &str, format: SubtitleFormat) -> LocalBoxFuture<'static, Option<PathBuf>> {
        let name = suggested_name.to_owned();
        Box::pin(async move { crate::dialogs::pick_save_file(&name, format).await })
    }
    fn confirm(&self, alert: Alert) -> LocalBoxFuture<'static, AlertResponse> {
        // An app-installed presenter wins; otherwise an rfd alert (a sheet on the parented key window).
        crate::dialogs::confirm(alert)
    }
}
