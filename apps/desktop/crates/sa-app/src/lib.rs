//! `sa-app`: the Slint front end and the wiring of every crate (ARCHITECTURE.md §6).
//!
//! The compiled UI tree follows the target (`ui/win` fluent, `ui/mac` cupertino; see build.rs). Both trees export
//! the same windows and globals, so this crate's Rust code is shared; tree-specific glue uses `cfg(sa_ui = "…")`.

pub mod app;
pub mod dispatcher;
pub mod hub;
pub mod loc;
pub mod main_window;
pub mod settings_window;
pub mod symbols;

/// Generated Slint types (`MainWindow`, `SettingsWindow`, the bridge globals, `Palette`).
#[allow(clippy::all)]
pub mod ui {
    slint::include_modules!();
}

/// Which UI tree and style this binary was built with.
pub const UI_TREE: &str = env!("SA_UI_TREE");
pub const UI_STYLE: &str = env!("SA_UI_STYLE");

/// Selects the catalog chain matching the compiled tree (only differs from native in cross-platform previews).
pub fn select_catalog_chain() {
    #[cfg(sa_ui = "mac")]
    sa_core::l10n::set_active_chain(sa_core::l10n::CatalogChain::Mac);
    #[cfg(sa_ui = "win")]
    sa_core::l10n::set_active_chain(sa_core::l10n::CatalogChain::Windows);
}

/// Prints what the native stack reports (FFmpeg, audio host, SQLite) — the `--probe` switch.
pub fn probe_native_stack() -> String {
    let info = sa_player::ffmpeg_backend::probe();
    let mut out = format!(
        "ui tree: {UI_TREE} ({UI_STYLE})\nffmpeg avformat: {}\naudio host: {}\naudio output: {}\n",
        info.ffmpeg_avformat_version,
        info.audio_host,
        info.audio_output_device.as_deref().unwrap_or("(none)")
    );
    #[cfg(windows)]
    out.push_str(&format!("sqlite: {}\n", sa_profiles::win::sqlite_version()));
    out
}

/// The app: composition, the main window(s), the event loop and the shutdown order ([`app::run`]).
pub fn run() -> Result<(), slint::PlatformError> {
    app::run()
}
