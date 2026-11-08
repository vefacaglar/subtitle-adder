//! `sa-platform`: OS integration behind `sa_core::platform::PlatformShell` (UNIFIED.md §5.6, §5.8, §5.10, §5.11).
//!
//! * [`win`] (`cfg(windows)`, `windows` crate): known folders, single instance (named mutex
//!   `Local\SubtitleAdder.SingleInstance.<user>` + pipe, byte-exact with C#), HKCU file associations, DirectWrite font
//!   families, preferred UI languages and the process language override, WinUI-like window chrome (Mica,
//!   `WM_NCCALCSIZE`/`WM_NCHITTEST` incl. the maximize button for Snap Layouts), crash log, shell thumbnails.
//! * [`mac`] (`cfg(target_os = "macos")`, `objc2` runtime + Core Foundation only): CFPreferences settings store
//!   (domain `app.subtitleadder.mac`), Apple Event `odoc` handler + coalescer, app-bundle paths, NSWorkspace,
//!   `NSApp.appearance`, standard menu actions and roles, SF Symbols rasterizer, AVFoundation thumbnails, global
//!   `AppleLanguages`.
//! * Shared: rfd-based dialogs and the alert fallback ([`dialogs`]), the open-documents coalescer ([`coalescer`]),
//!   the pure macOS rules ([`mac_rules`], tested on every platform).

pub mod coalescer;
pub mod dialogs;
pub mod icon;
pub mod mac_rules;

#[cfg(windows)]
pub mod win;

#[cfg(target_os = "macos")]
pub mod mac;

/// The shell of this platform.
pub fn platform_shell(diagnostics: std::sync::Arc<dyn sa_core::diagnostics::DiagnosticSink>) -> std::sync::Arc<dyn sa_core::platform::PlatformShell> {
    #[cfg(windows)]
    {
        std::sync::Arc::new(win::WinShell::new(diagnostics))
    }
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(mac::MacShell::new(diagnostics))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = diagnostics;
        unimplemented!("only Windows and macOS are supported")
    }
}

/// The card-picture generator of this platform (shell thumbnails on Windows, AVFoundation on macOS).
pub fn thumbnail_generator() -> std::sync::Arc<dyn sa_core::platform::ThumbnailGenerator> {
    #[cfg(windows)]
    {
        std::sync::Arc::new(win::ShellThumbnailGenerator::new())
    }
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(mac::AssetThumbnailGenerator::new())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        unimplemented!("only Windows and macOS are supported")
    }
}

/// The external-open source of this platform (single-instance pipe on Windows, Apple Events on macOS). macOS: call
/// on the main thread before the event loop starts.
pub fn external_open_source() -> Box<dyn sa_core::platform::ExternalOpenSource> {
    #[cfg(windows)]
    {
        Box::new(win::single_instance::SingleInstanceCoordinator::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(mac::open_documents::OpenDocumentsHandler::install())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        unimplemented!("only Windows and macOS are supported")
    }
}
