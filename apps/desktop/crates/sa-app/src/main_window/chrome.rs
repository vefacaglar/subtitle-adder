//! Native chrome of the main window.
//!
//! * Windows (win-ui-main §4, C# `WindowChrome.Setup`): content extended into the 48 px title bar with the Slint
//!   caption buttons, Snap Layouts on the maximize button (non-client, so its hover/press comes back through
//!   `AppBridge.maximize-hover`), rounded corners, frame dark mode following the content theme, min size 760×580 DIP,
//!   saved size (`mainWindowWidth/Height`, physical px, written on close only when restored) clamped to the work area
//!   and centered on it. In full screen the title bar row is 0, so nothing is reported as caption. Mica behind the
//!   window where DWM supports it (`AppBridge.mica-backdrop` makes the Slint background transparent).
//! * macOS (mac-ui §3.2, ARCHITECTURE §6.6): transparent title bar + full-size content view with a hidden title, so the
//!   52 pt toolbar is the unified title bar; set through the winit window attributes before the window exists
//!   ([`winit_attributes`], for `slint::BackendSelector::with_winit_window_attributes_hook`).
//!
//! Only on a real winit window: `sa-shot` and the tests (software renderer) have no native window and keep the plain
//! path (the controller's own minimize/maximize/drag fallbacks).

use slint::winit_030::winit;

/// Width of one Slint caption button (DIP, win-ui-main §6.1: 3 buttons in the 140 px reserve).
pub const CAPTION_BUTTON_DIP: f32 = 46.0;

/// Window attributes of the main window before creation. Windows keeps the standard frame (the chrome subclass needs
/// it, `sa_platform::win::chrome`); macOS gets the unified-toolbar look.
pub fn winit_attributes(attrs: winit::window::WindowAttributes) -> winit::window::WindowAttributes {
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::WindowAttributesExtMacOS;
        attrs.with_titlebar_transparent(true).with_fullsize_content_view(true).with_title_hidden(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        attrs
    }
}

/// Whether the renderer that really draws `window` delivers per-pixel alpha, which Mica needs. femtovg (OpenGL)
/// does; the software renderer presents through GDI, where a transparent pixel is black. The software renderer is
/// what `SLINT_BACKEND=winit-software` / `winit-sw` selects, and also what winit falls back to when the GPU renderer
/// cannot be created (no usable OpenGL, e.g. in some VMs or RDP sessions), so the environment cannot tell: the
/// window's renderer is asked instead. Only the GPU renderers support a rendering notifier; the probe leaves a no-op
/// one installed. A window that already has a notifier passes what installing it said as `known` instead: setting
/// another one would replace it (femtovg keeps only the last), which would switch off the main window's GPU video
/// path (`video_gl`). Call after the window was shown.
pub fn mica_renderer(window: &slint::Window, known: Option<bool>) -> bool {
    if let Some(gpu) = known {
        return gpu;
    }
    match window.set_rendering_notifier(|_, _| {}) {
        Ok(()) | Err(slint::SetRenderingNotifierError::AlreadySet) => true,
        Err(_) => false,
    }
}

/// The title-bar rectangles in physical client pixels: caption buttons at the right edge (3 × 46 DIP); no caption at
/// all in full screen (row height 0).
#[cfg(windows)]
pub fn caption_layout(width_px: u32, scale: f32, full_screen: bool) -> sa_platform::win::chrome::CaptionLayout {
    use sa_platform::win::chrome::{CaptionLayout, TITLE_BAR_HEIGHT_DIP};
    if full_screen {
        return CaptionLayout::default();
    }
    let bar = (TITLE_BAR_HEIGHT_DIP as f32 * scale).round() as i32;
    let btn = (CAPTION_BUTTON_DIP * scale).round() as i32;
    let w = width_px as i32;
    CaptionLayout {
        title_bar_height: bar,
        close: (w - btn, 0, btn, bar),
        maximize: (w - 2 * btn, 0, btn, bar),
        minimize: (w - 3 * btn, 0, btn, bar),
        ..Default::default()
    }
}

#[cfg(windows)]
pub use win::{MainChrome, attach};

#[cfg(windows)]
mod win {
    use std::rc::Rc;

    use sa_core::settings::SettingsStore;
    use sa_platform::win::chrome::{CaptionHover, MAIN_DEFAULT_DIP, MAIN_MIN_DIP, MAIN_SIZE_KEY, WindowChrome, saved_size};
    use slint::ComponentHandle;
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::WinitWindowAccessor;

    use crate::ui::{self, AppBridge, MainWindow};

    /// The attached chrome of one main window. Slint keeps one winit event filter per window, so the controller's
    /// filter re-syncs it ([`MainChrome::sync`]) on resize, scale and theme changes.
    pub struct MainChrome {
        chrome: WindowChrome,
    }

    impl MainChrome {
        pub fn minimize(&self) {
            self.chrome.minimize();
        }
        pub fn toggle_maximize(&self) {
            self.chrome.toggle_maximize();
        }
        pub fn is_maximized(&self) -> bool {
            self.chrome.is_maximized()
        }
        /// Saves the restored size on close (`mainWindowWidth/Height`, win-ui-main §4.3).
        pub fn save_size(&self, settings: &dyn SettingsStore) {
            self.chrome.save_size(settings, MAIN_SIZE_KEY);
        }
        /// Re-reports the caption layout (resize, scale change, full screen on/off) and the maximized state.
        pub fn sync(&self, window: &MainWindow) {
            let w = window.window();
            let app = window.global::<AppBridge>();
            self.chrome.set_caption_layout(super::caption_layout(w.size().width, w.scale_factor(), app.get_is_full_screen()));
            let max = self.chrome.is_maximized();
            if app.get_is_maximized() != max {
                app.set_is_maximized(max);
            }
        }
        /// The frame follows the content theme (System = the OS scheme Slint resolved).
        pub fn sync_theme(&self, window: &MainWindow) {
            let dark = match window.global::<ui::Palette>().get_color_scheme() {
                slint::language::ColorScheme::Dark => true,
                slint::language::ColorScheme::Light => false,
                _ => window.window().with_winit_window(|w| w.theme()).flatten() == Some(slint::winit_030::winit::window::Theme::Dark),
            };
            self.chrome.set_dark(dark);
        }
    }

    fn hwnd_of(window: &MainWindow) -> Option<isize> {
        window
            .window()
            .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
                _ => None,
            })
            .flatten()
    }

    /// Attaches the chrome after the window was shown (placement from the saved size, icon, hover of the maximize
    /// button, theme); `None` without a native window.
    /// `gpu_renderer`: whether the window's renderer is a GPU one, when already known (see `mica_renderer`).
    pub fn attach(window: &MainWindow, settings: &dyn SettingsStore, gpu_renderer: Option<bool>) -> Option<Rc<MainChrome>> {
        let hwnd = hwnd_of(window)?;
        let chrome = Rc::new(MainChrome { chrome: WindowChrome::attach(hwnd, MAIN_MIN_DIP) });
        chrome.chrome.place(saved_size(settings, MAIN_SIZE_KEY), MAIN_DEFAULT_DIP);
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            let _ = chrome.chrome.set_icon_file(&dir.join("Assets").join("AppIcon.ico"));
        }
        {
            let weak = window.as_weak();
            chrome.chrome.set_caption_hover_handler(Box::new(move |h| {
                if let Some(w) = weak.upgrade() {
                    w.global::<AppBridge>().set_maximize_hover(match h {
                        CaptionHover::None => 0,
                        CaptionHover::MaximizeHover => 1,
                        CaptionHover::MaximizePressed => 2,
                    });
                }
            }));
        }
        {
            let weak = window.as_weak();
            let c = Rc::downgrade(&chrome);
            chrome.chrome.set_system_change_handler(Box::new(move || {
                if let (Some(w), Some(c)) = (weak.upgrade(), c.upgrade()) {
                    c.sync_theme(&w);
                }
            }));
        }
        chrome.sync(window);
        chrome.sync_theme(window);
        // C# `window.SystemBackdrop = new MicaBackdrop()`: the Slint background turns transparent only where DWM
        // really draws Mica (Windows 11); elsewhere it stays the solid fallback color.
        let mica = super::mica_renderer(window.window(), gpu_renderer) && chrome.chrome.set_mica(true);
        window.global::<AppBridge>().set_mica_backdrop(mica);
        Some(chrome)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn caption_buttons_sit_at_the_right_edge_and_vanish_in_full_screen() {
        let l = caption_layout(1860, 1.5, false);
        assert_eq!(l.title_bar_height, 72);
        assert_eq!(l.close, (1860 - 69, 0, 69, 72));
        assert_eq!(l.maximize, (1860 - 138, 0, 69, 72));
        assert_eq!(l.minimize, (1860 - 207, 0, 69, 72));
        assert_eq!(caption_layout(1860, 1.5, true).title_bar_height, 0);
    }
}
