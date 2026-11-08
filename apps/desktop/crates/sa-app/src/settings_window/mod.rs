//! The Settings window (owner: ui-settings; ARCHITECTURE.md §6.5): Windows NavigationView with Genel / Görünüm /
//! Modeller (win-ui-hub-settings §6–§7), macOS preferences tabs Genel / Çeviri / Görünüm / Modeller with the provider
//! editor sheet (mac-ui §3.5, §11). One model ([`model::SettingsModel`]) serves both layouts; [`host`] is the Slint
//! glue.
//!
//! Wiring (app shell): build a [`deps::SettingsDeps`] once, create [`SettingsWindowHost::new`], call
//! `host.show(None)` from `AppBridge.open-settings` / Ctrl+, / ⌘, (the translation picker's "manage providers"
//! passes `Some("general")` on Windows and `Some("translation")` on macOS, `PlatformPolicy::settings_manage_providers_page`),
//! and `host.close()` when the main window closes.

#[cfg(all(windows, sa_ui = "win"))]
mod chrome;
pub mod deps;
pub mod host;
pub mod model;
pub mod providers;
#[doc(hidden)]
pub mod shot;
#[doc(hidden)]
pub mod testing;

pub use deps::{SettingsDeps, SettingsVm};
pub use host::SettingsWindowHost;
pub use model::{Flavor, SettingsModel, View};
