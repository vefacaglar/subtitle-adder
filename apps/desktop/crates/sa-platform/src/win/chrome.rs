//! WinUI-like window chrome on a winit HWND (win-ui-main §4; UNIFIED.md §5.11; C# `WindowChrome`).
//!
//! * The client area extends into the title bar: a WndProc subclass answers `WM_NCCALCSIZE` by keeping the side and
//!   bottom resize borders and dropping the caption (maximized windows are inset by the frame so nothing is cut off).
//!   The window keeps its standard frame style, so Snap, Aero Shake, the DWM shadow and the system menu stay native.
//!   Create the Slint window **with** decorations (`no-frame: false`).
//! * `WM_NCHITTEST`: the Slint title bar reports its layout ([`CaptionLayout`], physical px, client coordinates).
//!   Empty title-bar space is `HTCAPTION` (drag, double-click maximize, right-click system menu), the top edge is
//!   `HTTOP` when restored, the Slint-drawn maximize button is `HTMAXBUTTON` so Windows 11 shows the Snap Layouts flyout
//!   on hover. Minimize/close and every other control in the title bar stay client area (Slint handles the clicks).
//!   Because the maximize button is non-client, Slint gets no pointer events over it: the chrome reports hover/press
//!   through [`WindowChrome::set_caption_hover_handler`] and performs the maximize/restore itself on click.
//! * Mica (`DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_MAINWINDOW`, frame extended into the whole client area) through
//!   [`WindowChrome::set_mica`], which reports whether DWM draws it: it only shows through a transparent Slint
//!   background, so the caller makes the background transparent only then; rounded corners;
//!   immersive dark mode follows the content theme ([`WindowChrome::set_dark`]).
//! * Size: min size `min_dip × scale` (scale at attach time, like C#), first-run default size centered on the work area,
//!   restored size persisted in physical px (`mainWindowWidth/Height`).

use std::cell::{Cell, RefCell};

use sa_core::settings::SettingsStore;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow, ScreenToClient};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetWindowLongPtrW, GetWindowRect, HICON, HTCAPTION, HTCLIENT, HTMAXBUTTON, HTTOP, ICON_BIG, ICON_SMALL, IMAGE_ICON,
    IsIconic, IsZoomed, LR_LOADFROMFILE, LoadImageW, MINMAXINFO, NCCALCSIZE_PARAMS, PostMessageW, SC_CLOSE, SC_MINIMIZE,
    SM_CXICON, SM_CXPADDEDBORDER, SM_CXSMICON, SM_CYFRAME, SW_MAXIMIZE, SW_RESTORE, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SendMessageW, SetWindowPos, ShowWindow, WM_GETMINMAXINFO, WM_MOUSEMOVE,
    WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE,
    WM_NCMOUSEMOVE, WM_SETICON, WM_SETTINGCHANGE, WM_SYSCOMMAND, WM_THEMECHANGED, WS_THICKFRAME, WM_DWMCOLORIZATIONCOLORCHANGED,
};

use super::wide::Wide;

/// Main window (win-ui-main §4.1): min 760×580, default 1240×840 DIP, size key `mainWindow`.
pub const MAIN_MIN_DIP: (u32, u32) = (760, 580);
pub const MAIN_DEFAULT_DIP: (u32, u32) = (1240, 840);
pub const MAIN_SIZE_KEY: &str = "mainWindow";
/// Settings window (win-ui-hub-settings §7): min 640×520, default 860×760 DIP, size key `settingsWindow`.
pub const SETTINGS_MIN_DIP: (u32, u32) = (640, 520);
pub const SETTINGS_DEFAULT_DIP: (u32, u32) = (860, 760);
pub const SETTINGS_SIZE_KEY: &str = "settingsWindow";
/// Title bar height of both windows (TitleBarHeightOption.Tall).
pub const TITLE_BAR_HEIGHT_DIP: u32 = 48;

/// `(x, y, width, height)`.
pub type Rect = (i32, i32, i32, i32);

/// Rectangles (physical px, client coordinates) the Slint title bar reports on every layout change.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CaptionLayout {
    pub title_bar_height: i32,
    pub minimize: Rect,
    pub maximize: Rect,
    pub close: Rect,
    /// Interactive controls inside the title bar (not draggable).
    pub interactive: [Rect; 8],
    pub interactive_count: usize,
}

impl CaptionLayout {
    /// Adds an interactive rectangle (ignored beyond 8).
    pub fn push_interactive(&mut self, r: Rect) {
        if self.interactive_count < self.interactive.len() {
            self.interactive[self.interactive_count] = r;
            self.interactive_count += 1;
        }
    }
}

/// What a point of the window is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    Client,
    Caption,
    Top,
    MaximizeButton,
}

fn contains(r: Rect, x: i32, y: i32) -> bool {
    r.2 > 0 && r.3 > 0 && x >= r.0 && y >= r.1 && x < r.0 + r.2 && y < r.1 + r.3
}

/// Pure hit test of a client point (physical px). `top_border` = resize band at the top when restored.
pub fn hit_test(layout: &CaptionLayout, x: i32, y: i32, maximized: bool, top_border: i32) -> HitTarget {
    if !maximized && y >= 0 && y < top_border && !contains(layout.close, x, y) && !contains(layout.minimize, x, y) {
        // The maximize button keeps its flyout even in the top band, like the native caption buttons.
        if !contains(layout.maximize, x, y) {
            return HitTarget::Top;
        }
    }
    if y < 0 || y >= layout.title_bar_height {
        return HitTarget::Client;
    }
    if contains(layout.maximize, x, y) {
        return HitTarget::MaximizeButton;
    }
    if contains(layout.minimize, x, y) || contains(layout.close, x, y) {
        return HitTarget::Client;
    }
    if layout.interactive[..layout.interactive_count.min(8)].iter().any(|r| contains(*r, x, y)) {
        return HitTarget::Client;
    }
    HitTarget::Caption
}

/// Initial outer size and position (win-ui-main §4.1): a saved size (both > 0) at least the minimum, else the
/// default; each clamped to the work area, centered in it.
pub fn compute_placement(saved: Option<(u32, u32)>, min_dip: (u32, u32), default_dip: (u32, u32), scale: f64, work: Rect) -> Rect {
    // C# `(int)(minWidth * scale)`: truncated physical pixels.
    let min_px = ((min_dip.0 as f64 * scale) as i32, (min_dip.1 as f64 * scale) as i32);
    compute_placement_px(saved, min_px, default_dip, scale, work)
}

/// [`compute_placement`] with the minimum already in physical pixels (the chrome keeps the value it computed at
/// attach time, so no DIP round trip can change it by a pixel).
pub fn compute_placement_px(saved: Option<(u32, u32)>, min_px: (i32, i32), default_dip: (u32, u32), scale: f64, work: Rect) -> Rect {
    let (min_w, min_h) = min_px;
    let (w, h) = match saved {
        // Settings ints are 32-bit in C#; anything larger is clamped by the work area below anyway.
        Some((w, h)) if w > 0 && h > 0 => ((w.min(i32::MAX as u32) as i32).max(min_w), (h.min(i32::MAX as u32) as i32).max(min_h)),
        _ => ((default_dip.0 as f64 * scale) as i32, (default_dip.1 as f64 * scale) as i32),
    };
    let w = w.min(work.2);
    let h = h.min(work.3);
    (work.0 + (work.2 - w) / 2, work.1 + (work.3 - h) / 2, w, h)
}

/// `<key>Width` / `<key>Height` from settings (C# `GetInt`, positive values only).
pub fn saved_size(settings: &dyn SettingsStore, key: &str) -> Option<(u32, u32)> {
    let w = settings.get_int(&format!("{key}Width"))?;
    let h = settings.get_int(&format!("{key}Height"))?;
    (w > 0 && h > 0).then(|| (w.min(u32::MAX as i64) as u32, h.min(u32::MAX as i64) as u32))
}

/// Hover / press state of the (non-client) maximize button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionHover {
    None,
    MaximizeHover,
    MaximizePressed,
}

struct State {
    hwnd: HWND,
    layout: Cell<CaptionLayout>,
    min_px: (i32, i32),
    hover: Cell<CaptionHover>,
    tracking: Cell<bool>,
    hover_handler: RefCell<Option<Box<dyn Fn(CaptionHover)>>>,
    system_change_handler: RefCell<Option<Box<dyn Fn()>>>,
    attached: Cell<bool>,
}

impl State {
    fn set_hover(&self, h: CaptionHover) {
        if self.hover.replace(h) != h
            && let Some(f) = self.hover_handler.borrow().as_ref()
        {
            f(h);
        }
    }
}

const SUBCLASS_ID: usize = 0x5341_4348; // "SACH"

/// Installed on the main/settings windows (UI thread). Dropping it removes the subclass.
pub struct WindowChrome {
    state: Box<State>,
    scale: f64,
}

/// The frame extension that puts Mica behind the whole client area without DWM's own caption buttons. A "sheet of
/// glass" (`-1`) or any top margin makes DWM draw its minimize/maximize/close glyphs in the extended frame, and with
/// the caption turned into client area they show through the transparent Slint title bar under the Slint buttons
/// (two sets, offset). Extending from the bottom edge by more than any window is tall covers the same client area
/// and DWM draws no caption buttons there.
pub fn mica_margins() -> MARGINS {
    MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 0, cyBottomHeight: i16::MAX as i32 }
}

/// `DWMWA_MICA_EFFECT` (Windows 11 21H2 only; not in the SDK headers).
const DWMWA_MICA_EFFECT: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(1029);

/// Build 22000+ (`RtlGetVersion` is not subject to the manifest-based version lie of `GetVersionEx`).
fn is_windows_11() -> bool {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OSVERSIONINFOW) -> i32;
    }
    let mut info = OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    // SAFETY: `info` is a properly sized OSVERSIONINFOW with its size field set.
    unsafe { RtlGetVersion(&mut info) == 0 && info.dwMajorVersion >= 10 && info.dwBuildNumber >= 22000 }
}

fn dpi_of(hwnd: HWND) -> u32 {
    let d = unsafe { GetDpiForWindow(hwnd) };
    if d == 0 { 96 } else { d }
}

fn frame_thickness(dpi: u32) -> i32 {
    unsafe { GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
}

impl WindowChrome {
    /// `hwnd` from `slint::Window::window_handle()` (raw-window-handle 0.6 `Win32WindowHandle::hwnd`).
    pub fn attach(hwnd: isize, min_size_dip: (u32, u32)) -> Self {
        let h = HWND(hwnd as *mut _);
        let scale = dpi_of(h) as f64 / 96.0;
        let state = Box::new(State {
            hwnd: h,
            layout: Cell::new(CaptionLayout::default()),
            min_px: ((min_size_dip.0 as f64 * scale) as i32, (min_size_dip.1 as f64 * scale) as i32),
            hover: Cell::new(CaptionHover::None),
            tracking: Cell::new(false),
            hover_handler: RefCell::new(None),
            system_change_handler: RefCell::new(None),
            attached: Cell::new(false),
        });
        unsafe {
            let ok = SetWindowSubclass(h, Some(subclass_proc), SUBCLASS_ID, &*state as *const State as usize).as_bool();
            state.attached.set(ok);
            let round = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const _, 4);
            // Re-run WM_NCCALCSIZE with the subclass in place.
            let _ = SetWindowPos(h, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        Self { state, scale }
    }

    pub fn hwnd(&self) -> isize {
        self.state.hwnd.0 as isize
    }

    /// DPI scale at attach time (`GetDpiForWindow / 96`).
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// The current DPI scale.
    pub fn current_scale(&self) -> f64 {
        dpi_of(self.state.hwnd) as f64 / 96.0
    }

    pub fn set_caption_layout(&self, layout: CaptionLayout) {
        self.state.layout.set(layout);
    }

    pub fn caption_layout(&self) -> CaptionLayout {
        self.state.layout.get()
    }

    /// Called on the UI thread whenever the maximize button's hover/press state changes.
    pub fn set_caption_hover_handler(&self, handler: Box<dyn Fn(CaptionHover)>) {
        *self.state.hover_handler.borrow_mut() = Some(handler);
    }

    /// Called on the UI thread when the system theme, accent or other settings change (`WM_SETTINGCHANGE`,
    /// `WM_THEMECHANGED`, `WM_DWMCOLORIZATIONCOLORCHANGED`): re-read the accent palette and, for `appTheme = system`,
    /// the theme, then re-apply the caption colors (win-ui-main §4.1, §5).
    pub fn set_system_change_handler(&self, handler: Box<dyn Fn()>) {
        *self.state.system_change_handler.borrow_mut() = Some(handler);
    }

    /// Light/dark: immersive dark mode for the frame, border and Mica tint (the caption glyphs are Slint's).
    pub fn set_dark(&self, dark: bool) {
        let v: i32 = dark as i32;
        unsafe {
            let _ = DwmSetWindowAttribute(self.state.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &v as *const _ as *const _, 4);
        }
    }

    /// Mica behind a transparent Slint background (C# `window.SystemBackdrop = new MicaBackdrop()`):
    /// `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_MAINWINDOW` on Windows 11 22H2+, the older `DWMWA_MICA_EFFECT` on 21H2, with
    /// the frame extended into the whole client area ([`mica_margins`]: from the bottom edge, never the top). Returns whether DWM draws Mica; `false` (Windows 10, or `on ==
    /// false`) leaves the frame unextended, and the caller keeps an opaque background (MicaBackdrop's solid fallback).
    pub fn set_mica(&self, on: bool) -> bool {
        let hwnd = self.state.hwnd;
        unsafe {
            let kind = if on { DWMSBT_MAINWINDOW } else { DWMSBT_NONE };
            let mut ok = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as *const _, 4).is_ok();
            if on && !ok {
                // Windows 11 21H2 (build 22000): the undocumented attribute 1029 that preceded the backdrop type.
                let enable: i32 = 1;
                ok = DwmSetWindowAttribute(hwnd, DWMWA_MICA_EFFECT, &enable as *const _ as *const _, 4).is_ok() && is_windows_11();
            }
            let mica = on && ok;
            let m = if mica { mica_margins() } else { MARGINS::default() };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
            mica
        }
    }

    /// Window icon from an `.ico` file (`<app dir>\Assets\AppIcon.ico`); `false` when it could not be loaded.
    pub fn set_icon_file(&self, path: &std::path::Path) -> bool {
        let w = Wide::path(path);
        let dpi = dpi_of(self.state.hwnd);
        let mut any = false;
        for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
            let size = unsafe { GetSystemMetricsForDpi(metric, dpi) };
            if let Ok(h) = unsafe { LoadImageW(None, w.pcwstr(), IMAGE_ICON, size, size, LR_LOADFROMFILE) } {
                unsafe { SendMessageW(self.state.hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(HICON(h.0).0 as isize))) };
                any = true;
            }
        }
        any
    }

    /// Centers on the work area at the saved size (or the default size on first run), win-ui-main §4.1.
    pub fn place(&self, size_physical: Option<(u32, u32)>, default_dip: (u32, u32)) {
        let work = self.work_area();
        let (x, y, w, h) = compute_placement_px(size_physical, self.state.min_px, default_dip, self.scale, work);
        unsafe {
            let _ = SetWindowPos(self.state.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    /// The work area of the window's display (primary display fallback).
    pub fn work_area(&self) -> Rect {
        unsafe {
            let mon = MonitorFromWindow(self.state.hwnd, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let r = mi.rcWork;
                (r.left, r.top, r.right - r.left, r.bottom - r.top)
            } else {
                (0, 0, 1280, 800)
            }
        }
    }

    pub fn is_maximized(&self) -> bool {
        unsafe { IsZoomed(self.state.hwnd).as_bool() }
    }

    /// Physical outer size when restored (saved on close), `None` when maximized, minimized or full screen.
    pub fn restored_size(&self) -> Option<(u32, u32)> {
        let h = self.state.hwnd;
        unsafe {
            if IsZoomed(h).as_bool() || IsIconic(h).as_bool() {
                return None;
            }
            // winit's borderless full screen drops the sizing frame.
            if GetWindowLongPtrW(h, GWL_STYLE) as u32 & WS_THICKFRAME.0 == 0 {
                return None;
            }
            let mut r = RECT::default();
            GetWindowRect(h, &mut r).ok()?;
            Some(((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32))
        }
    }

    /// Saves `<key>Width/Height` when restored (window close, win-ui-main §4.3).
    pub fn save_size(&self, settings: &dyn SettingsStore, key: &str) {
        if let Some((w, h)) = self.restored_size() {
            settings.set_int(&format!("{key}Width"), w as i64);
            settings.set_int(&format!("{key}Height"), h as i64);
        }
    }

    pub fn minimize(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.state.hwnd), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0));
        }
    }

    pub fn toggle_maximize(&self) {
        toggle_maximize(self.state.hwnd);
    }

    /// Asks the window to close (`SC_CLOSE`, goes through the normal close handling).
    pub fn close(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.state.hwnd), WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0));
        }
    }

    /// Brings the window to the front (second-instance callback, win-ui-main §2.3).
    pub fn bring_to_foreground(&self) {
        super::process::bring_to_foreground(self.hwnd());
    }
}

impl Drop for WindowChrome {
    fn drop(&mut self) {
        if self.state.attached.replace(false) {
            unsafe {
                let _ = RemoveWindowSubclass(self.state.hwnd, Some(subclass_proc), SUBCLASS_ID);
            }
        }
    }
}

fn toggle_maximize(hwnd: HWND) {
    unsafe {
        let cmd = if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE };
        let _ = ShowWindow(hwnd, cmd);
    }
}

fn lparam_point(lparam: LPARAM) -> POINT {
    POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 }
}

unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, data: usize) -> LRESULT {
    let state = unsafe { &*(data as *const State) };
    match msg {
        WM_NCCALCSIZE if wparam.0 != 0 => unsafe {
            let params = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
            let top = params.rgrc[0].top;
            let r = DefSubclassProc(hwnd, msg, wparam, lparam);
            if r.0 != 0 {
                return r;
            }
            params.rgrc[0].top = top;
            if IsZoomed(hwnd).as_bool() {
                params.rgrc[0].top += frame_thickness(dpi_of(hwnd));
            }
            LRESULT(0)
        },
        WM_NCHITTEST => unsafe {
            let r = DefSubclassProc(hwnd, msg, wparam, lparam);
            if r.0 as u32 != HTCLIENT {
                return r;
            }
            let mut pt = lparam_point(lparam);
            let _ = ScreenToClient(hwnd, &mut pt);
            let maximized = IsZoomed(hwnd).as_bool();
            let top = frame_thickness(dpi_of(hwnd));
            let target = hit_test(&state.layout.get(), pt.x, pt.y, maximized, top);
            LRESULT(match target {
                HitTarget::Client => HTCLIENT,
                HitTarget::Caption => HTCAPTION,
                HitTarget::Top => HTTOP,
                HitTarget::MaximizeButton => HTMAXBUTTON,
            } as isize)
        },
        WM_NCMOUSEMOVE => {
            if wparam.0 as u32 == HTMAXBUTTON {
                if state.hover.get() != CaptionHover::MaximizePressed {
                    state.set_hover(CaptionHover::MaximizeHover);
                }
                if !state.tracking.replace(true) {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut t);
                    }
                }
            } else {
                state.set_hover(CaptionHover::None);
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_NCMOUSELEAVE => {
            state.tracking.set(false);
            state.set_hover(CaptionHover::None);
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_MOUSEMOVE => {
            state.set_hover(CaptionHover::None);
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wparam.0 as u32 == HTMAXBUTTON => {
            state.set_hover(CaptionHover::MaximizePressed);
            LRESULT(0)
        }
        WM_NCLBUTTONUP if wparam.0 as u32 == HTMAXBUTTON => {
            let was_pressed = state.hover.get() == CaptionHover::MaximizePressed;
            state.set_hover(CaptionHover::MaximizeHover);
            if was_pressed {
                toggle_maximize(hwnd);
            }
            LRESULT(0)
        }
        WM_NCLBUTTONUP => {
            if state.hover.get() == CaptionHover::MaximizePressed {
                state.set_hover(CaptionHover::None);
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_DWMCOLORIZATIONCOLORCHANGED => {
            if let Ok(h) = state.system_change_handler.try_borrow()
                && let Some(f) = h.as_ref()
            {
                f();
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_GETMINMAXINFO => unsafe {
            let r = DefSubclassProc(hwnd, msg, wparam, lparam);
            let info = &mut *(lparam.0 as *mut MINMAXINFO);
            info.ptMinTrackSize.x = info.ptMinTrackSize.x.max(state.min_px.0);
            info.ptMinTrackSize.y = info.ptMinTrackSize.y.max(state.min_px.1);
            r
        },
        WM_NCDESTROY => unsafe {
            state.attached.set(false);
            let _ = RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
            DefSubclassProc(hwnd, msg, wparam, lparam)
        },
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, RegisterClassW,
        WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
    };
    use windows::core::w;

    #[test]
    fn mica_extends_the_frame_from_the_bottom_only() {
        // A sheet of glass (-1) or a top margin makes DWM draw a second set of caption buttons (see mica_margins).
        let m = mica_margins();
        assert_eq!((m.cxLeftWidth, m.cxRightWidth, m.cyTopHeight), (0, 0, 0));
        assert!(m.cyBottomHeight >= 16_000, "taller than any window: the whole client area");
    }

    fn layout() -> CaptionLayout {
        let mut l = CaptionLayout {
            title_bar_height: 48,
            minimize: (1000, 0, 46, 48),
            maximize: (1046, 0, 46, 48),
            close: (1092, 0, 46, 48),
            ..Default::default()
        };
        l.push_interactive((10, 8, 200, 32));
        l
    }

    #[test]
    fn title_bar_hit_testing() {
        let l = layout();
        assert_eq!(hit_test(&l, 500, 20, false, 8), HitTarget::Caption);
        assert_eq!(hit_test(&l, 500, 3, false, 8), HitTarget::Top);
        assert_eq!(hit_test(&l, 500, 3, true, 8), HitTarget::Caption);
        assert_eq!(hit_test(&l, 1060, 20, false, 8), HitTarget::MaximizeButton);
        assert_eq!(hit_test(&l, 1060, 2, false, 8), HitTarget::MaximizeButton);
        assert_eq!(hit_test(&l, 1010, 20, false, 8), HitTarget::Client);
        assert_eq!(hit_test(&l, 1100, 2, false, 8), HitTarget::Client);
        assert_eq!(hit_test(&l, 50, 20, false, 8), HitTarget::Client);
        assert_eq!(hit_test(&l, 500, 48, false, 8), HitTarget::Client);
        assert_eq!(hit_test(&CaptionLayout::default(), 10, 20, false, 8), HitTarget::Client);
    }

    #[test]
    fn placement_restores_clamps_and_centers() {
        let work = (0, 0, 1920, 1040);
        assert_eq!(compute_placement(None, MAIN_MIN_DIP, MAIN_DEFAULT_DIP, 1.0, work), (340, 100, 1240, 840));
        assert_eq!(compute_placement(None, MAIN_MIN_DIP, MAIN_DEFAULT_DIP, 1.5, work), (30, 0, 1860, 1040));
        assert_eq!(compute_placement(Some((500, 400)), MAIN_MIN_DIP, MAIN_DEFAULT_DIP, 1.0, work), (580, 230, 760, 580));
        assert_eq!(compute_placement(Some((1400, 900)), MAIN_MIN_DIP, MAIN_DEFAULT_DIP, 1.0, (100, 50, 1000, 700)), (100, 50, 1000, 700));
        assert_eq!(compute_placement(Some((0, 900)), MAIN_MIN_DIP, MAIN_DEFAULT_DIP, 1.0, work), (340, 100, 1240, 840));
        // 175 %: the minimum is (int)(760 * 1.75) = 1330 px, never a pixel less after a DIP round trip.
        let wide = (0, 0, 3840, 2100);
        assert_eq!(compute_placement(Some((100, 100)), MAIN_MIN_DIP, MAIN_DEFAULT_DIP, 1.75, wide), (1255, 542, 1330, 1015));
        assert_eq!(compute_placement_px(Some((100, 100)), (1330, 1015), MAIN_DEFAULT_DIP, 1.75, wide), (1255, 542, 1330, 1015));
        // A huge saved size is clamped to the work area, not wrapped negative.
        assert_eq!(compute_placement(Some((u32::MAX, u32::MAX)), MAIN_MIN_DIP, MAIN_DEFAULT_DIP, 1.0, work), (0, 0, 1920, 1040));
    }

    #[test]
    fn saved_size_needs_both_positive_ints() {
        let s = sa_core::settings::InMemorySettingsStore::new();
        assert_eq!(saved_size(s.as_ref(), MAIN_SIZE_KEY), None);
        s.set_int("mainWindowWidth", 1300);
        assert_eq!(saved_size(s.as_ref(), MAIN_SIZE_KEY), None);
        s.set_int("mainWindowHeight", 0);
        assert_eq!(saved_size(s.as_ref(), MAIN_SIZE_KEY), None);
        s.set_int("mainWindowHeight", 900);
        assert_eq!(saved_size(s.as_ref(), MAIN_SIZE_KEY), Some((1300, 900)));
    }

    unsafe extern "system" fn plain_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    #[test]
    fn a_real_window_gets_the_extended_client_area_and_hit_tests() {
        unsafe {
            let inst = GetModuleHandleW(None).unwrap();
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(plain_proc),
                hInstance: inst.into(),
                lpszClassName: w!("SaChromeTest"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("SaChromeTest"),
                w!("t"),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                900,
                700,
                None,
                None,
                Some(inst.into()),
                None,
            )
            .unwrap();
            let chrome = WindowChrome::attach(hwnd.0 as isize, MAIN_MIN_DIP);
            // The caption is gone: the client area starts at the window top.
            let mut wr = RECT::default();
            let mut cr = RECT::default();
            GetWindowRect(hwnd, &mut wr).unwrap();
            GetClientRect(hwnd, &mut cr).unwrap();
            assert_eq!(cr.bottom - cr.top + frame_thickness(dpi_of(hwnd)), wr.bottom - wr.top, "only the bottom frame remains");

            chrome.place(None, MAIN_DEFAULT_DIP);
            let (w, h) = chrome.restored_size().unwrap();
            let work = chrome.work_area();
            assert!(w as i32 <= work.2 && h as i32 <= work.3);
            assert!(w as f64 >= (MAIN_MIN_DIP.0 as f64 * chrome.scale()).min(work.2 as f64));

            let mut l = CaptionLayout { title_bar_height: 48, ..Default::default() };
            l.maximize = (300, 0, 46, 48);
            chrome.set_caption_layout(l);
            GetWindowRect(hwnd, &mut wr).unwrap();
            let mut origin = POINT { x: 0, y: 0 };
            windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut origin).unwrap();
            let at = |x: i32, y: i32| {
                let lp = LPARAM((((origin.y + y) as i16 as u16 as isize) << 16) | ((origin.x + x) as i16 as u16 as isize));
                SendMessageW(hwnd, WM_NCHITTEST, None, Some(lp)).0 as u32
            };
            assert_eq!(at(150, 30), HTCAPTION);
            assert_eq!(at(310, 30), HTMAXBUTTON);
            assert_eq!(at(150, 100), HTCLIENT);
            assert_eq!(at(150, 1), HTTOP);

            let hovers = std::rc::Rc::new(RefCell::new(Vec::new()));
            let h2 = hovers.clone();
            chrome.set_caption_hover_handler(Box::new(move |h| h2.borrow_mut().push(h)));
            SendMessageW(hwnd, WM_NCMOUSEMOVE, Some(WPARAM(HTMAXBUTTON as usize)), Some(LPARAM(0)));
            SendMessageW(hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(HTMAXBUTTON as usize)), Some(LPARAM(0)));
            SendMessageW(hwnd, WM_NCLBUTTONUP, Some(WPARAM(HTMAXBUTTON as usize)), Some(LPARAM(0)));
            assert!(chrome.is_maximized());
            assert_eq!(chrome.restored_size(), None);
            SendMessageW(hwnd, WM_MOUSEMOVE, None, None);
            let changes = std::rc::Rc::new(Cell::new(0));
            let c2 = changes.clone();
            chrome.set_system_change_handler(Box::new(move || c2.set(c2.get() + 1)));
            SendMessageW(hwnd, WM_SETTINGCHANGE, None, None);
            SendMessageW(hwnd, WM_THEMECHANGED, None, None);
            assert_eq!(changes.get(), 2);
            assert_eq!(
                *hovers.borrow(),
                vec![CaptionHover::MaximizeHover, CaptionHover::MaximizePressed, CaptionHover::MaximizeHover, CaptionHover::None]
            );
            chrome.toggle_maximize();
            assert!(!chrome.is_maximized());

            let settings = sa_core::settings::InMemorySettingsStore::new();
            chrome.save_size(settings.as_ref(), MAIN_SIZE_KEY);
            assert!(saved_size(settings.as_ref(), MAIN_SIZE_KEY).is_some());
            chrome.set_dark(true);
            chrome.set_mica(true);
            drop(chrome);
            let _ = DestroyWindow(hwnd);
        }
    }
}
