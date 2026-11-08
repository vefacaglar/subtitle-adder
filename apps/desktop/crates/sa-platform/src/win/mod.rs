//! Windows platform services (`windows` crate): known folders, user/machine names, preferred UI languages, the
//! process UI language override, DirectWrite font families, quick locations (Videolar / İndirilenler / Masaüstü /
//! drives), drive readiness, Explorer, memory, process checks, crash log, shell thumbnails, plus the single-instance
//! coordinator, HKCU file associations and the window chrome.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sa_core::diagnostics::DiagnosticSink;
use sa_core::engine_types::SubtitleFormat;
use sa_core::future::LocalBoxFuture;
use sa_core::l10n::UiLanguage;
use sa_core::platform::{Alert, AlertResponse, FileAssociations, FileFilter, PlatformShell, QuickLocation};

pub mod associations;
pub mod chrome;
pub mod crash;
pub mod folders;
pub mod fonts;
pub mod process;
pub mod single_instance;
pub mod thumbnails;
mod wide;

pub use thumbnails::ShellThumbnailGenerator;

/// The Windows shell.
pub struct WinShell {
    diagnostics: Arc<dyn DiagnosticSink>,
    associations: associations::FileAssociationRegistrar,
}

impl WinShell {
    pub fn new(diagnostics: Arc<dyn DiagnosticSink>) -> Self {
        Self { diagnostics, associations: associations::FileAssociationRegistrar::new() }
    }

    /// `SHGetKnownFolderPath(FOLDERID_LocalAppData)` (fallback `%LOCALAPPDATA%`).
    pub fn local_app_data() -> PathBuf {
        folders::local_app_data()
    }

    /// `SHGetKnownFolderPath(FOLDERID_Videos)` (dialog start folder, explorer default).
    pub fn videos_folder() -> Option<PathBuf> {
        folders::videos_folder()
    }

    /// .NET `Environment.UserName` (`GetUserNameExW(NameSamCompatible)` after the last `\`).
    pub fn user_name() -> String {
        folders::user_name()
    }

    /// .NET `Environment.MachineName` (`GetComputerNameW`).
    pub fn machine_name() -> String {
        folders::machine_name()
    }

    /// Panic hook → `%TEMP%\SubtitleAdder-crash.log` (paths redacted, never secrets).
    pub fn install_crash_log() {
        crash::install();
    }
}

impl PlatformShell for WinShell {
    fn app_data_dir(&self) -> PathBuf {
        Self::local_app_data().join("SubtitleAdder")
    }
    fn diagnostics(&self) -> &dyn DiagnosticSink {
        self.diagnostics.as_ref()
    }
    fn system_preferred_languages(&self) -> Vec<String> {
        folders::preferred_ui_languages()
    }
    fn apply_process_ui_language(&self, language: Option<UiLanguage>) {
        let language = language.unwrap_or_else(|| UiLanguage::system(&self.system_preferred_languages()));
        folders::set_process_ui_language(language);
    }
    fn installed_font_families(&self) -> Vec<String> {
        fonts::families().to_vec()
    }
    fn quick_locations(&self) -> Vec<QuickLocation> {
        folders::quick_locations()
    }
    fn storage_available(&self, path: &Path) -> bool {
        folders::storage_available(path)
    }
    fn reveal_in_file_manager(&self, path: &Path) {
        process::open_in_explorer(path, true);
    }
    fn open_folder(&self, path: &Path) {
        process::open_in_explorer(path, false);
    }
    fn total_physical_memory(&self) -> u64 {
        process::total_physical_memory()
    }
    fn other_app_instances_running(&self) -> bool {
        process::other_app_instances_running()
    }
    fn pick_open_file(&self, filter: FileFilter) -> LocalBoxFuture<'static, Option<PathBuf>> {
        Box::pin(crate::dialogs::pick_open_file(filter, Self::videos_folder()))
    }
    fn pick_open_files(&self, filter: FileFilter) -> LocalBoxFuture<'static, Vec<PathBuf>> {
        Box::pin(crate::dialogs::pick_open_files(filter, Self::videos_folder()))
    }
    fn pick_save_file(&self, suggested_name: &str, format: SubtitleFormat) -> LocalBoxFuture<'static, Option<PathBuf>> {
        let name = suggested_name.to_owned();
        Box::pin(async move { crate::dialogs::pick_save_file_in(&name, format, Self::videos_folder()).await })
    }
    fn confirm(&self, alert: Alert) -> LocalBoxFuture<'static, AlertResponse> {
        // The ContentDialog-style overlay is drawn by the Slint view, installed with
        // `dialogs::set_alert_presenter`; an rfd message box is the fallback.
        crate::dialogs::confirm(alert)
    }
    fn file_associations(&self) -> Option<&dyn FileAssociations> {
        Some(&self.associations)
    }
}
