//! OS services the core and the views need (UNIFIED.md §5.6, §5.10). Pure contract; implementations live in
//! `sa-platform` (`WinShell`, `MacShell`). Never business logic.

use std::path::{Path, PathBuf};

use crate::diagnostics::DiagnosticSink;
use crate::future::LocalBoxFuture;
use crate::l10n::UiLanguage;

/// A sidebar quick location (Windows: Videolar/İndirilenler/Masaüstü/drives `"{label} (C:)"`; macOS:
/// Filmler/İndirilenler/Masaüstü/mounted volumes).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QuickLocation {
    /// Display title (already localized for known folders; volume/drive labels verbatim).
    pub title: String,
    pub path: PathBuf,
    pub kind: QuickLocationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuickLocationKind {
    Videos,
    Downloads,
    Desktop,
    Home,
    Drive,
    Volume,
}

/// Open/save dialog filters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileFilter {
    Videos,
    Subtitles,
    /// Export: one type (`srt`/`vtt`/`json`).
    Subtitle(crate::engine_types::SubtitleFormat),
    Any,
}

/// A confirmation/alert (NSAlert sheet on macOS; ContentDialog-style overlay on Windows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub title: String,
    pub message: String,
    /// Default (Return) button first.
    pub buttons: Vec<AlertButton>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertButton {
    pub title: String,
    pub role: AlertButtonRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertButtonRole {
    Default,
    Cancel,
    Destructive,
}

/// Index of the pressed button, or `None` when dismissed (Esc).
pub type AlertResponse = Option<usize>;

/// Windows file associations (HKCU, no elevation; win-ui-main §3).
pub trait FileAssociations: Send + Sync {
    fn register(&self) -> bool;
    fn unregister(&self) -> bool;
    fn is_registered(&self) -> bool;
    /// "Varsayılan uygulamalar…": opens the Windows Default Apps page.
    fn open_default_apps_settings(&self);
}

/// The OS services.
pub trait PlatformShell: Send + Sync {
    /// `%LOCALAPPDATA%\SubtitleAdder` / `~/Library/Application Support/SubtitleAdder`.
    fn app_data_dir(&self) -> PathBuf;
    /// `std::env::temp_dir()/SubtitleAdder`.
    fn engine_work_dir(&self) -> PathBuf {
        std::env::temp_dir().join("SubtitleAdder")
    }
    fn diagnostics(&self) -> &dyn DiagnosticSink;
    /// BCP-47 tags: Windows `GetUserPreferredUILanguages`; macOS global-domain `AppleLanguages`.
    fn system_preferred_languages(&self) -> Vec<String>;
    /// Windows `SetProcessPreferredUILanguages` (startup); macOS per-app `AppleLanguages` (`None` for System).
    fn apply_process_ui_language(&self, language: Option<UiLanguage>);
    /// DirectWrite / CTFontManager family names (sorted as the app showed them).
    fn installed_font_families(&self) -> Vec<String>;
    fn quick_locations(&self) -> Vec<QuickLocation>;
    /// Drive root ready / volume mounted — never touches files.
    fn storage_available(&self, path: &Path) -> bool;
    /// Explorer `/select,` / `activateFileViewerSelectingURLs`.
    fn reveal_in_file_manager(&self, path: &Path);
    fn open_folder(&self, path: &Path);
    fn total_physical_memory(&self) -> u64;
    /// Windows temp-cleanup rule.
    fn other_app_instances_running(&self) -> bool;
    /// rfd dialogs, parented to the key window. Windows starts in the Videos known folder.
    fn pick_open_file(&self, filter: FileFilter) -> LocalBoxFuture<'static, Option<PathBuf>>;
    fn pick_open_files(&self, filter: FileFilter) -> LocalBoxFuture<'static, Vec<PathBuf>>;
    /// macOS keeps the exact suggested name; Windows appends the extension when missing.
    fn pick_save_file(&self, suggested_name: &str, format: crate::engine_types::SubtitleFormat) -> LocalBoxFuture<'static, Option<PathBuf>>;
    fn confirm(&self, alert: Alert) -> LocalBoxFuture<'static, AlertResponse>;
    /// `Some` on Windows only.
    fn file_associations(&self) -> Option<&dyn FileAssociations> {
        None
    }
}

/// Hub card pictures (UNIFIED.md §5.8): Windows shell thumbnail 240 px (1 at a time); macOS 384×216 JPEG 0.75 at
/// 10 % / ≤ 30 s (2 at a time). **Never called logged out.**
pub trait ThumbnailGenerator: Send + Sync {
    fn generate(&self, path: &Path, cancel: &crate::cancel::CancellationToken) -> Option<Vec<u8>>;
    fn max_concurrency(&self) -> usize;
}

/// Outcome of the single-instance check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceRole {
    /// This process owns the UI.
    Primary,
    /// The paths were forwarded to the primary instance: exit now (Windows).
    Exit,
}

/// External "open these files" delivery (Windows: mutex + pipe server; macOS: Apple Event `odoc` +
/// `ExternalOpenCoalescer` 250 ms quiet / 1 s max).
pub trait ExternalOpenSource {
    /// Windows: returns `Exit` when the paths were forwarded to the primary instance (1200 ms timeout).
    fn acquire_or_forward(&mut self, initial_paths: &[PathBuf]) -> InstanceRole;
    /// Posted to the UI thread; buffered until set.
    fn set_sink(&mut self, sink: Box<dyn FnMut(Vec<PathBuf>) + Send>);
}
