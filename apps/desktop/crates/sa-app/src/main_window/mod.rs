//! The main window (ARCHITECTURE.md §6.5, area ui-main): wiring of `MainBridge`, `ExplorerBridge`, `AppBridge` and
//! `MenuBridge` to the view models and the player, plus the pure presentation logic both trees share.
//!
//! * [`controller::MainWindowController`] — attaches one `MainWindow` to the view models, its player and the shell.
//! * Pure, unit-tested pieces: [`layout`] (workspace layout), [`hud`] (control visibility), [`transport`]
//!   (transport texts/ratios), [`player`] (player controller over `MediaPlayerBackend`), [`rows`] (keyed row
//!   models, selection), [`menus`] (menu builders), [`status`] (error bar, playback notice), [`input`] (keys, drops).
//! * [`chrome`] — native window chrome (Windows caption/Snap/size persistence, macOS unified title bar attributes).
//! * [`translation_picker`] — the toolbar translation-model flyout (Windows) / popover (macOS).
//! * [`demo`] fills the bridges with representative content for `sa-shot` look checks.

pub mod chrome;
pub mod controller;
pub mod demo;
pub mod hud;
pub mod input;
pub mod layout;
pub mod menus;
pub mod player;
pub mod rows;
pub mod status;
pub mod translation_picker;
pub mod transport;

pub use controller::{MainWindowController, MainWindowDeps, MainWindowHooks, PreviewMaker};
