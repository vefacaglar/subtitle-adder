//! Windows chrome of the Settings window (win-ui-hub-settings §6.1, C# `WindowChrome.Setup`): content extended into
//! the 48 px title bar with the Slint caption buttons, Snap Layouts on the maximize button, rounded corners, the
//! frame's dark mode following the app theme, min size 640×520 DIP, saved size (`settingsWindowWidth/Height`,
//! physical px, written on close only when restored) clamped to the work area and centered on it.
//!
//! Only on a real winit window: `sa-shot` and the tests (software renderer) have no HWND and keep the plain path.

use std::rc::Rc;

use sa_core::settings::SettingsStore;
use sa_platform::win::chrome::{
    CaptionHover, CaptionLayout, SETTINGS_DEFAULT_DIP, SETTINGS_MIN_DIP, SETTINGS_SIZE_KEY, TITLE_BAR_HEIGHT_DIP, WindowChrome,
    saved_size,
};
use slint::ComponentHandle;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::{EventResult, WinitWindowAccessor};

use crate::ui;

/// Width of one Slint caption button (DIP).
const CAPTION_BUTTON_DIP: f32 = 46.0;

fn hwnd_of(window: &ui::SettingsWindow) -> Option<isize> {
    window
        .window()
        .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        })
        .flatten()
}

/// The title-bar rectangles in physical client pixels (caption buttons at the right edge, 3 × 46 DIP).
fn layout_for(width_px: u32, scale: f32) -> CaptionLayout {
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

fn sync(chrome: &WindowChrome, window: &ui::SettingsWindow) {
    let w = window.window();
    chrome.set_caption_layout(layout_for(w.size().width, w.scale_factor()));
    let b = window.global::<ui::SettingsBridge>();
    let max = chrome.is_maximized();
    if b.get_is_maximized() != max {
        b.set_is_maximized(max);
    }
}

/// Attaches the chrome after the window was shown; `None` without a native window.
/// `on_theme` runs when the OS theme changes (the host re-resolves `appTheme = system`).
pub fn attach(window: &ui::SettingsWindow, settings: &dyn SettingsStore, on_theme: Box<dyn Fn()>) -> Option<Rc<WindowChrome>> {
    let hwnd = hwnd_of(window)?;
    let chrome = Rc::new(WindowChrome::attach(hwnd, SETTINGS_MIN_DIP));
    chrome.place(saved_size(settings, SETTINGS_SIZE_KEY), SETTINGS_DEFAULT_DIP);
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let _ = chrome.set_icon_file(&dir.join("Assets").join("AppIcon.ico"));
    }
    {
        let weak = window.as_weak();
        chrome.set_caption_hover_handler(Box::new(move |h| {
            if let Some(w) = weak.upgrade() {
                w.global::<ui::SettingsBridge>().set_maximize_hover(match h {
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
        window.window().on_winit_window_event(move |_, event| {
            let (Some(w), Some(chrome)) = (weak.upgrade(), c.upgrade()) else { return EventResult::Propagate };
            match event {
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    // Slint updates its size after this filter: compute from the event's window state on the next turn.
                    let weak = w.as_weak();
                    let c = Rc::downgrade(&chrome);
                    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                        if let (Some(w), Some(chrome)) = (weak.upgrade(), c.upgrade()) {
                            sync(&chrome, &w);
                        }
                    });
                }
                WindowEvent::Focused(active) => w.global::<ui::SettingsBridge>().set_is_active(*active),
                WindowEvent::ThemeChanged(_) => {
                    on_theme();
                    set_dark(&chrome, &w);
                }
                _ => {}
            }
            EventResult::Propagate
        });
    }
    sync(&chrome, window);
    set_dark(&chrome, window);
    Some(chrome)
}

/// The frame follows the content theme (System = the OS scheme Slint resolved).
pub fn set_dark(chrome: &WindowChrome, window: &ui::SettingsWindow) {
    let scheme = window.global::<ui::Palette>().get_color_scheme();
    let dark = match scheme {
        slint::language::ColorScheme::Dark => true,
        slint::language::ColorScheme::Light => false,
        _ => window.window().with_winit_window(|w| w.theme()).flatten() == Some(slint::winit_030::winit::window::Theme::Dark),
    };
    chrome.set_dark(dark);
}

/// Saves the restored size on close (`settingsWindowWidth/Height`).
pub fn save_size(chrome: &WindowChrome, settings: &dyn SettingsStore) {
    chrome.save_size(settings, SETTINGS_SIZE_KEY);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_buttons_sit_at_the_right_edge_in_physical_pixels() {
        let l = layout_for(1290, 1.5);
        assert_eq!(l.title_bar_height, 72);
        assert_eq!(l.close, (1290 - 69, 0, 69, 72));
        assert_eq!(l.maximize, (1290 - 138, 0, 69, 72));
        assert_eq!(l.minimize, (1290 - 207, 0, 69, 72));
    }
}
