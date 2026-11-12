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
//! * Full screen ([`WindowChrome::set_full_screen`]), done here rather than through winit's borderless mode: winit keeps
//!   `WS_MAXIMIZE` on a maximized window and changes the styles after the size messages, so the handlers below could
//!   not tell full screen from maximized (the maximized top inset stayed, 9 px of the monitor were not client area).
//!   The chrome sets its own full-screen flag before it touches the window, so every handler knows the state: the
//!   caption and sizing frame are dropped (`WS_MAXIMIZE` and Windows' own restore / snap bookkeeping are left alone),
//!   the client area is the whole window (no inset), hit testing is plain client area, the size is the monitor's
//!   (`WM_GETMINMAXINFO` does not clamp it; `WM_WINDOWPOSCHANGING` keeps it on the monitor the window is on through
//!   re-maximizing, monitor moves, resolution and DPI changes), corners are square and the DWM border is off, and the
//!   shell is told it is a full-screen window (`ITaskbarList2::MarkFullscreenWindow`) so the taskbar stays below.
//!   Leaving restores the frame styles and the previous rectangle ([`restore_rect`]).

use std::cell::{Cell, RefCell};

use sa_core::settings::SettingsStore;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMWA_BORDER_COLOR, DWMWA_COLOR_DEFAULT, DWMWA_COLOR_NONE, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND, DWMWINDOWATTRIBUTE,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromRect,
    MonitorFromWindow, ScreenToClient,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::{DefSubclassProc, ITaskbarList2, RemoveWindowSubclass, SetWindowSubclass, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetClientRect, GetWindowLongPtrW, GetWindowPlacement, GetWindowRect, HICON, HTCAPTION, HTCLIENT,
    HTMAXBUTTON, HTTOP, ICON_BIG, ICON_SMALL, IMAGE_ICON, IsIconic, IsZoomed, LR_LOADFROMFILE, LoadImageW, MINMAXINFO,
    NCCALCSIZE_PARAMS, PostMessageW, RegisterWindowMessageW, SC_CLOSE, SC_MINIMIZE, SET_WINDOW_POS_FLAGS, SM_CXFRAME, SM_CXICON,
    SM_CXPADDEDBORDER, SM_CXSMICON, SM_CYFRAME, SW_MAXIMIZE, SW_RESTORE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    IsWindowVisible, SW_HIDE, SW_SHOWMAXIMIZED, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, STYLESTRUCT, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    SendMessageW, SetWindowLongPtrW, SetWindowPlacement, SetWindowPos, ShowWindow, WINDOWPLACEMENT, WM_STYLECHANGING,
    WPF_RESTORETOMAXIMIZED,
    WINDOWPOS, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_DWMCOLORIZATIONCOLORCHANGED, WM_GETMINMAXINFO, WM_MOUSEMOVE, WM_NCCALCSIZE,
    WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_SETICON,
    WM_SETTINGCHANGE, WM_SYSCOMMAND, WM_THEMECHANGED, WM_WINDOWPOSCHANGED, WM_WINDOWPOSCHANGING, WS_CAPTION, WS_EX_CLIENTEDGE,
    WS_EX_DLGMODALFRAME, WS_EX_STATICEDGE, WS_EX_WINDOWEDGE, WS_THICKFRAME,
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

/// The window's frame state as the non-client handlers see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameState {
    Restored,
    Maximized,
    FullScreen,
}

/// `(left, top, right, bottom)`, as in a Win32 `RECT`.
pub type Edges = (i32, i32, i32, i32);

/// The client rectangle `WM_NCCALCSIZE` answers. `window` is the proposed window rectangle, `default` what the
/// default procedure made of it (frame on every side, caption at the top). The caption is dropped: restored windows
/// keep the side and bottom resize borders and start the client area at the window top; a maximized window hangs
/// `frame` px over every monitor edge, so the top is inset by that much; a full-screen window has no frame at all,
/// the client area is the whole window.
pub fn client_edges(window: Edges, default: Edges, state: FrameState, frame: i32) -> Edges {
    match state {
        FrameState::FullScreen => window,
        FrameState::Restored => (default.0, window.1, default.2, default.3),
        FrameState::Maximized => (default.0, window.1 + frame, default.2, default.3),
    }
}

/// [`WindowChrome::geometry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    pub window: Rect,
    pub client: Rect,
    pub monitor: Rect,
    pub maximized: bool,
    pub full_screen: bool,
}

impl WindowGeometry {
    /// `window=x,y,WxH client=x,y,WxH monitor=x,y,WxH maximized=b` (the `[player-stats]` fields).
    pub fn stats_fields(&self) -> String {
        let r = |r: Rect| format!("{},{},{}x{}", r.0, r.1, r.2, r.3);
        format!("window={} client={} monitor={} maximized={}", r(self.window), r(self.client), r(self.monitor), self.maximized)
    }
}

/// The window before it entered full screen (what leaving restores).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedWindow {
    pub style: u32,
    pub ex_style: u32,
    /// Window rectangle (screen coordinates, physical px).
    pub rect: Rect,
    pub maximized: bool,
    /// The monitor and work area the window was on, and its DPI.
    pub monitor: Rect,
    pub work: Rect,
    pub dpi: u32,
    /// `WINDOWPLACEMENT::rcNormalPosition` as it was (workspace coordinates, unconverted): put back on the way out,
    /// because a Win+arrow while full screen makes Windows record the monitor rectangle there.
    pub placement_normal: Edges,
}

/// `rcNormalPosition` of a `WINDOWPLACEMENT` (workspace coordinates: relative to the work area of the monitor, so
/// they differ from screen coordinates when a taskbar sits at the left or top) as a screen rectangle.
pub fn workspace_to_screen(r: Edges, monitor: Rect, work: Rect) -> Rect {
    let (ox, oy) = (work.0 - monitor.0, work.1 - monitor.1);
    (r.0 + ox, r.1 + oy, r.2 - r.0, r.3 - r.1)
}

/// Where Windows puts a maximized window: the work area plus the frame overhang on every side.
pub fn maximized_rect(work: Rect, frame: (i32, i32)) -> Rect {
    (work.0 - frame.0, work.1 - frame.1, work.2 + 2 * frame.0, work.3 + 2 * frame.1)
}

/// The window rectangle a `WM_WINDOWPOSCHANGING` proposes (`flags`: `SWP_NOMOVE` / `SWP_NOSIZE` take the current
/// position / size), or `None` when it neither moves nor sizes the window, or minimizes it (-32000).
pub fn proposed_window_pos(flags: u32, pos: Rect, current: Rect) -> Option<Rect> {
    let both = SWP_NOMOVE.0 | SWP_NOSIZE.0;
    if flags & both == both {
        return None;
    }
    let (x, y) = if flags & SWP_NOMOVE.0 != 0 { (current.0, current.1) } else { (pos.0, pos.1) };
    let (w, h) = if flags & SWP_NOSIZE.0 != 0 { (current.2, current.3) } else { (pos.2, pos.3) };
    (x > -32000 && y > -32000).then_some((x, y, w, h))
}

/// `v` kept within `lo..=hi`; `lo` wins when the range is empty (never panics, unlike `i32::clamp`).
fn clamp_low(v: i32, lo: i32, hi: i32) -> i32 {
    v.min(hi).max(lo)
}

/// The display the window is on when it leaves full screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Display {
    pub monitor: Rect,
    pub work: Rect,
    pub dpi: u32,
    /// Maximized frame overhang at `dpi` (horizontal, vertical).
    pub frame: (i32, i32),
}

/// Where a window leaving full screen goes. Unchanged display: exactly the saved rectangle (maximized, snapped or
/// normal alike; the style keeps `WS_MAXIMIZE` and Windows keeps its own restore and snap state, since entering
/// never restored or moved the window through the system). A maximized window on a changed display (another
/// monitor, resolution, DPI or taskbar) takes the new work area with the frame overhang; a restored one keeps its
/// place relative to the work area, scaled by the DPI change and kept inside the new work area. `maximized` is the
/// state now (it can change while in full screen, e.g. `Win+↓`); `normal` is the restored rectangle Windows keeps
/// (`GetWindowPlacement`, screen coordinates), used when a window saved maximized is no longer maximized.
pub fn restore_rect(saved: &SavedWindow, maximized: bool, normal: Rect, now: &Display) -> Rect {
    let same = saved.monitor == now.monitor && saved.work == now.work && saved.dpi == now.dpi;
    if maximized {
        if saved.maximized && same {
            return saved.rect;
        }
        return maximized_rect(now.work, now.frame);
    }
    let base = if saved.maximized { normal } else { saved.rect };
    if same {
        return base;
    }
    let k = now.dpi as f64 / saved.dpi.max(1) as f64;
    let w = clamp_low((base.2 as f64 * k).round() as i32, 1, now.work.2);
    let h = clamp_low((base.3 as f64 * k).round() as i32, 1, now.work.3);
    let dx = ((base.0 - saved.work.0) as f64 * k).round() as i32;
    let dy = ((base.1 - saved.work.1) as f64 * k).round() as i32;
    let x = clamp_low(now.work.0 + dx, now.work.0, now.work.0 + now.work.2 - w);
    let y = clamp_low(now.work.1 + dy, now.work.1, now.work.1 + now.work.3 - h);
    (x, y, w, h)
}

/// Frame bits a full-screen window drops (`WS_CAPTION` = border + dialog frame, `WS_THICKFRAME`) and the edge
/// extended styles.
const FULL_SCREEN_DROPPED_STYLE: u32 = WS_CAPTION.0 | WS_THICKFRAME.0;
const FULL_SCREEN_DROPPED_EX_STYLE: u32 = WS_EX_DLGMODALFRAME.0 | WS_EX_WINDOWEDGE.0 | WS_EX_CLIENTEDGE.0 | WS_EX_STATICEDGE.0;

/// The styles in full screen (`entering`) or back out of it: only the frame bits change, every other bit stays what
/// it is now (winit may have changed some while the window was full screen).
pub fn full_screen_styles(current: (u32, u32), saved: (u32, u32), entering: bool) -> (u32, u32) {
    let (s, e) = (current.0 & !FULL_SCREEN_DROPPED_STYLE, current.1 & !FULL_SCREEN_DROPPED_EX_STYLE);
    if entering { (s, e) } else { (s | (saved.0 & FULL_SCREEN_DROPPED_STYLE), e | (saved.1 & FULL_SCREEN_DROPPED_EX_STYLE)) }
}

/// `WM_GETMINMAXINFO` track sizes: the minimum is raised to `min_px`; in full screen (`monitor`) neither limit may
/// keep the window from covering the monitor exactly (a monitor smaller than the minimum size is still covered).
pub fn min_max_track(min: (i32, i32), max: (i32, i32), min_px: (i32, i32), monitor: Option<Rect>) -> ((i32, i32), (i32, i32)) {
    let min = (min.0.max(min_px.0), min.1.max(min_px.1));
    match monitor {
        None => (min, max),
        Some(m) => ((min.0.min(m.2), min.1.min(m.3)), (max.0.max(m.2), max.1.max(m.3))),
    }
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
    /// `Some` while the window is full screen (set before the window is touched on the way in, cleared before it is
    /// touched on the way out, so every handler sees the state the window is going to).
    full: Cell<Option<SavedWindow>>,
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
            full: Cell::new(None),
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

    /// Window rectangle, client rectangle (screen coordinates) and the rectangle of the monitor the window is mostly
    /// on, physical px (diagnostics: the `[player-stats]` line).
    pub fn geometry(&self) -> WindowGeometry {
        let h = self.state.hwnd;
        unsafe {
            let mut r = RECT::default();
            let _ = GetWindowRect(h, &mut r);
            let mut c = RECT::default();
            let _ = GetClientRect(h, &mut c);
            let mut origin = POINT { x: 0, y: 0 };
            let _ = ClientToScreen(h, &mut origin);
            let (monitor, _) = monitor_rects(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST));
            WindowGeometry {
                window: (r.left, r.top, r.right - r.left, r.bottom - r.top),
                client: (origin.x, origin.y, c.right - c.left, c.bottom - c.top),
                monitor,
                maximized: IsZoomed(h).as_bool(),
                full_screen: self.is_full_screen(),
            }
        }
    }

    /// Whether the window is full screen ([`WindowChrome::set_full_screen`]).
    pub fn is_full_screen(&self) -> bool {
        self.state.full.get().is_some()
    }

    /// Enters or leaves full screen (see the module docs): on the way in the window covers the monitor it is mostly
    /// on, with no frame, border, rounded corners or non-client area, above the taskbar; on the way out it gets its
    /// frame and previous rectangle back ([`restore_rect`]). Nothing happens when the state is already `on`.
    pub fn set_full_screen(&self, on: bool) {
        let h = self.state.hwnd;
        if on == self.is_full_screen() {
            return;
        }
        unsafe {
            let current = (GetWindowLongPtrW(h, GWL_STYLE) as u32, GetWindowLongPtrW(h, GWL_EXSTYLE) as u32);
            let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            let _ = GetWindowPlacement(h, &mut wp);
            let n = wp.rcNormalPosition;
            let placement_normal = (n.left, n.top, n.right, n.bottom);
            let iconic = IsIconic(h).as_bool();
            if on {
                let (monitor, work) = monitor_rects(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST));
                let dpi = dpi_of(h);
                // A minimized window's rectangle is the off-screen icon's: what it restores to is saved instead.
                let (rect, maximized) = if iconic {
                    if wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0 {
                        (maximized_rect(work, frame_overhang(dpi)), true)
                    } else {
                        (workspace_to_screen(placement_normal, monitor, work), false)
                    }
                } else {
                    let mut r = RECT::default();
                    let _ = GetWindowRect(h, &mut r);
                    ((r.left, r.top, r.right - r.left, r.bottom - r.top), IsZoomed(h).as_bool())
                };
                let saved = SavedWindow {
                    style: current.0,
                    ex_style: current.1,
                    rect,
                    maximized,
                    monitor,
                    work,
                    dpi,
                    placement_normal,
                };
                self.state.full.set(Some(saved));
                let (s, e) = full_screen_styles(current, (saved.style, saved.ex_style), true);
                SetWindowLongPtrW(h, GWL_STYLE, s as isize);
                SetWindowLongPtrW(h, GWL_EXSTYLE, e as isize);
                set_frame_look(h, true);
                mark_full_screen(h, true);
                if iconic {
                    // Stays minimized; restoring it ends on the monitor (WM_WINDOWPOSCHANGED -> refit).
                    let _ = SetWindowPos(h, None, 0, 0, 0, 0, FULL_SCREEN_SWP | SWP_NOMOVE | SWP_NOSIZE);
                } else {
                    let _ = SetWindowPos(h, None, monitor.0, monitor.1, monitor.2, monitor.3, FULL_SCREEN_SWP);
                }
            } else {
                let Some(saved) = self.state.full.take() else { return };
                let (s, e) = full_screen_styles(current, (saved.style, saved.ex_style), false);
                SetWindowLongPtrW(h, GWL_STYLE, s as isize);
                SetWindowLongPtrW(h, GWL_EXSTYLE, e as isize);
                let (monitor, work) = monitor_rects(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST));
                let dpi = dpi_of(h);
                let now = Display { monitor, work, dpi, frame: frame_overhang(dpi) };
                let zoomed = IsZoomed(h).as_bool();
                let normal = workspace_to_screen(saved.placement_normal, saved.monitor, saved.work);
                let (x, y, w, hh) = restore_rect(&saved, zoomed, normal, &now);
                set_frame_look(h, false);
                mark_full_screen(h, false);
                // Win+Up / Win+Left / Right while full screen made Windows record the monitor rectangle as the
                // restore (pre-snap) rectangle: put the one from before back, keeping the show state.
                if placement_normal != saved.placement_normal {
                    let (l, t, r, b) = saved.placement_normal;
                    let show = if !IsWindowVisible(h).as_bool() {
                        SW_HIDE
                    } else if iconic {
                        SW_SHOWMINNOACTIVE
                    } else if zoomed {
                        SW_SHOWMAXIMIZED
                    } else {
                        SW_SHOWNOACTIVATE
                    };
                    let fixed = WINDOWPLACEMENT {
                        showCmd: show.0 as u32,
                        rcNormalPosition: RECT { left: l, top: t, right: r, bottom: b },
                        ..wp
                    };
                    let _ = SetWindowPlacement(h, &fixed);
                }
                if iconic {
                    let _ = SetWindowPos(h, None, 0, 0, 0, 0, FULL_SCREEN_SWP | SWP_NOMOVE | SWP_NOSIZE);
                } else {
                    let _ = SetWindowPos(h, None, x, y, w, hh, FULL_SCREEN_SWP);
                }
            }
        }
    }

    /// Physical outer size when restored (saved on close), `None` when maximized, minimized or full screen from a
    /// maximized window; full screen from a restored window reports the size it had before.
    pub fn restored_size(&self) -> Option<(u32, u32)> {
        let h = self.state.hwnd;
        if let Some(saved) = self.state.full.get() {
            return (!saved.maximized).then_some((saved.rect.2.max(0) as u32, saved.rect.3.max(0) as u32));
        }
        unsafe {
            if IsZoomed(h).as_bool() || IsIconic(h).as_bool() {
                return None;
            }
            // winit's borderless full screen (only used before the chrome is attached) drops the sizing frame; the
            // chrome's own full screen is answered above, and `WM_STYLECHANGING` keeps the frame off only while it is.
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

/// `SetWindowPos` flags of the full-screen transitions (the frame changes, the z-order and activation do not).
const FULL_SCREEN_SWP: SET_WINDOW_POS_FLAGS =
    SET_WINDOW_POS_FLAGS(SWP_FRAMECHANGED.0 | SWP_NOZORDER.0 | SWP_NOOWNERZORDER.0 | SWP_NOACTIVATE.0);

/// Monitor and work area of `mon` (a 1280×800 primary fallback).
fn monitor_rects(mon: HMONITOR) -> (Rect, Rect) {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        let (m, w) = (mi.rcMonitor, mi.rcWork);
        ((m.left, m.top, m.right - m.left, m.bottom - m.top), (w.left, w.top, w.right - w.left, w.bottom - w.top))
    } else {
        ((0, 0, 1280, 800), (0, 0, 1280, 800))
    }
}

/// How far a maximized window hangs over the work area at `dpi` (horizontal, vertical).
fn frame_overhang(dpi: u32) -> (i32, i32) {
    unsafe {
        let pad = GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
        (GetSystemMetricsForDpi(SM_CXFRAME, dpi) + pad, GetSystemMetricsForDpi(SM_CYFRAME, dpi) + pad)
    }
}

/// Square corners and no DWM border in full screen; the rounded corners and the default border otherwise.
fn set_frame_look(hwnd: HWND, full_screen: bool) {
    let corner = if full_screen { DWMWCP_DONOTROUND } else { DWMWCP_ROUND };
    let border: u32 = if full_screen { DWMWA_COLOR_NONE } else { DWMWA_COLOR_DEFAULT };
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as *const _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as *const _, 4);
    }
}

/// Tells the shell the window is (no longer) full screen, so the taskbar goes below it even when the window is not
/// the foreground window (`ITaskbarList2::MarkFullscreenWindow`; COM is initialized on the UI thread by winit).
fn mark_full_screen(hwnd: HWND, on: bool) {
    unsafe {
        if let Ok(list) = CoCreateInstance::<_, ITaskbarList2>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
            && list.HrInit().is_ok()
        {
            let _ = list.MarkFullscreenWindow(hwnd, on);
        }
    }
}

/// Private message: put the full-screen window back over its monitor (after a display, DPI or work-area change, or
/// a restore from minimized that left it elsewhere).
/// 0 when the message could not be registered: then no refit is posted (the `WM_WINDOWPOSCHANGING` rule still holds).
fn refit_message() -> u32 {
    static MSG: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(windows::core::w!("SubtitleAdder.FullScreenRefit")) })
}

fn post_refit(hwnd: HWND) {
    let m = refit_message();
    if m != 0 {
        let _ = unsafe { PostMessageW(Some(hwnd), m, WPARAM(0), LPARAM(0)) };
    }
}

/// Moves a full-screen window over the monitor it is on when it is anywhere else (not while minimized).
fn refit_full_screen(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            return;
        }
        let (m, _) = monitor_rects(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST));
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        if (r.left, r.top, r.right - r.left, r.bottom - r.top) != m {
            let _ = SetWindowPos(hwnd, None, m.0, m.1, m.2, m.3, SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE);
        }
    }
}

fn toggle_maximize(hwnd: HWND) {
    unsafe {
        let cmd = if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE };
        let _ = ShowWindow(hwnd, cmd);
    }
}

/// The rectangle `WM_NCCALCSIZE` works on: `rgrc[0]` of the `NCCALCSIZE_PARAMS` when `wParam` is TRUE, else the
/// single `RECT` `lParam` points to.
///
/// # Safety
/// `lparam` must be the `lParam` of a `WM_NCCALCSIZE` with this `wparam`.
unsafe fn nccalcsize_rect<'a>(wparam: WPARAM, lparam: LPARAM) -> &'a mut RECT {
    unsafe {
        if wparam.0 != 0 { &mut (*(lparam.0 as *mut NCCALCSIZE_PARAMS)).rgrc[0] } else { &mut *(lparam.0 as *mut RECT) }
    }
}

fn lparam_point(lparam: LPARAM) -> POINT {
    POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 }
}

unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, data: usize) -> LRESULT {
    let state = unsafe { &*(data as *const State) };
    let full = state.full.get().is_some();
    match msg {
        // `wParam` TRUE: `NCCALCSIZE_PARAMS` (first rectangle = proposed window -> client); FALSE: a single `RECT`,
        // only taken over in full screen (otherwise the default answer stands, as before).
        WM_NCCALCSIZE if wparam.0 != 0 || full => unsafe {
            let rect = nccalcsize_rect(wparam, lparam);
            let window = (rect.left, rect.top, rect.right, rect.bottom);
            let frame_state = if full {
                FrameState::FullScreen
            } else if IsZoomed(hwnd).as_bool() {
                FrameState::Maximized
            } else {
                FrameState::Restored
            };
            // Full screen: nothing for the default procedure to subtract.
            let default = if full {
                window
            } else {
                let r = DefSubclassProc(hwnd, msg, wparam, lparam);
                if r.0 != 0 {
                    return r;
                }
                let d = nccalcsize_rect(wparam, lparam);
                (d.left, d.top, d.right, d.bottom)
            };
            let (l, t, rr, b) = client_edges(window, default, frame_state, frame_thickness(dpi_of(hwnd)));
            *nccalcsize_rect(wparam, lparam) = RECT { left: l, top: t, right: rr, bottom: b };
            LRESULT(0)
        },
        // winit rewrites GWL_STYLE from its own flags (a min / max size change, any window-flag change) and does
        // not know the window is full screen: keep the frame off until the chrome leaves full screen.
        WM_STYLECHANGING if full => unsafe {
            let ss = &mut *(lparam.0 as *mut STYLESTRUCT);
            if wparam.0 as i32 == GWL_STYLE.0 {
                ss.styleNew &= !FULL_SCREEN_DROPPED_STYLE;
            } else if wparam.0 as i32 == GWL_EXSTYLE.0 {
                ss.styleNew &= !FULL_SCREEN_DROPPED_EX_STYLE;
            }
            DefSubclassProc(hwnd, msg, wparam, lparam)
        },
        // Full screen: no resize borders, no caption, no caption buttons.
        WM_NCHITTEST if full => LRESULT(HTCLIENT as isize),
        WM_WINDOWPOSCHANGING if full => unsafe {
            let r = DefSubclassProc(hwnd, msg, wparam, lparam);
            let wp = &mut *(lparam.0 as *mut WINDOWPOS);
            // Minimizing moves the window to -32000; anything else that moves or sizes it (re-maximizing after a
            // work-area change, Win+Shift+arrow to another monitor, a DPI change) ends on the whole monitor it lands on.
            let mut cur = RECT::default();
            let _ = GetWindowRect(hwnd, &mut cur);
            let current = (cur.left, cur.top, cur.right - cur.left, cur.bottom - cur.top);
            if !IsIconic(hwnd).as_bool()
                && let Some((x, y, cw, ch)) = proposed_window_pos(wp.flags.0, (wp.x, wp.y, wp.cx, wp.cy), current)
            {
                let proposed = RECT { left: x, top: y, right: x + cw, bottom: y + ch };
                let (m, _) = monitor_rects(MonitorFromRect(&proposed, MONITOR_DEFAULTTONEAREST));
                wp.x = m.0;
                wp.y = m.1;
                wp.cx = m.2;
                wp.cy = m.3;
                wp.flags = SET_WINDOW_POS_FLAGS(wp.flags.0 & !(SWP_NOMOVE.0 | SWP_NOSIZE.0));
            }
            r
        },
        WM_WINDOWPOSCHANGED | WM_DISPLAYCHANGE | WM_DPICHANGED if full => unsafe {
            let r = DefSubclassProc(hwnd, msg, wparam, lparam);
            // After the system's (and winit's) own handling: a restore from minimized can put the window at its
            // normal rectangle, a display change can leave it at the old resolution.
            post_refit(hwnd);
            r
        },
        m if m != 0 && m == refit_message() => {
            if full {
                refit_full_screen(hwnd);
            }
            LRESULT(0)
        }
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
            if full {
                // A taskbar change (size, auto-hide) moves the work area and re-maximizes maximized windows.
                post_refit(hwnd);
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_GETMINMAXINFO => unsafe {
            let r = DefSubclassProc(hwnd, msg, wparam, lparam);
            let info = &mut *(lparam.0 as *mut MINMAXINFO);
            let monitor = full.then(|| monitor_rects(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)).0);
            let (min, max) = min_max_track(
                (info.ptMinTrackSize.x, info.ptMinTrackSize.y),
                (info.ptMaxTrackSize.x, info.ptMaxTrackSize.y),
                state.min_px,
                monitor,
            );
            (info.ptMinTrackSize.x, info.ptMinTrackSize.y) = min;
            (info.ptMaxTrackSize.x, info.ptMaxTrackSize.y) = max;
            if let Some(m) = monitor {
                // Maximizing while full screen (a restore from minimized) covers the monitor, not the work area.
                info.ptMaxPosition = POINT { x: 0, y: 0 };
                info.ptMaxSize = POINT { x: m.2, y: m.3 };
            }
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
        CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG,
        PM_REMOVE, PeekMessageW, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
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

    #[test]
    fn the_client_area_drops_the_caption_and_in_full_screen_every_frame() {
        for dpi in [96, 120, 144, 168, 192] {
            let f = frame_thickness(dpi);
            assert!(f > 0, "{dpi}");
            // Restored 1550×1050 at (505,165): the default procedure leaves f on the sides and bottom and a caption.
            let window = (505, 165, 2055, 1215);
            let default = (505 + f, 165 + f + 31, 2055 - f, 1215 - f);
            assert_eq!(client_edges(window, default, FrameState::Restored, f), (505 + f, 165, 2055 - f, 1215 - f), "{dpi}");
            // Maximized: the window hangs f over every monitor edge; the top is inset by as much as the others.
            let window = (-f, -f, 2560 + f, 1380 + f);
            let default = (0, 31, 2560, 1380);
            assert_eq!(client_edges(window, default, FrameState::Maximized, f), (0, 0, 2560, 1380), "{dpi}");
            // Full screen: the whole window (the monitor) is client area, whatever the default made of it.
            let window = (0, 0, 2560, 1440);
            assert_eq!(client_edges(window, (0, f, 2560, 1440 - f), FrameState::FullScreen, f), window, "{dpi}");
            // A second monitor left of the primary one.
            let window = (-1920, 0, 0, 1080);
            assert_eq!(client_edges(window, (-1920 + f, f, -f, 1080 - f), FrameState::FullScreen, f), window, "{dpi}");
        }
    }

    fn saved(rect: Rect, maximized: bool) -> SavedWindow {
        SavedWindow {
            style: 0x16CF_0000,
            ex_style: 0x0004_0110,
            rect,
            maximized,
            monitor: (0, 0, 2560, 1440),
            work: (0, 0, 2560, 1380),
            dpi: 120,
            placement_normal: (rect.0, rect.1, rect.0 + rect.2, rect.1 + rect.3),
        }
    }

    #[test]
    fn placement_coordinates_window_positions_and_empty_ranges() {
        // Workspace coordinates are relative to the work area: equal to screen coordinates with the taskbar at the
        // bottom, shifted by its width / height with the taskbar at the left / top, also on a second monitor.
        let mon = (0, 0, 2560, 1440);
        assert_eq!(workspace_to_screen((505, 165, 2055, 1215), mon, (0, 0, 2560, 1380)), (505, 165, 1550, 1050));
        assert_eq!(workspace_to_screen((505, 165, 2055, 1215), mon, (60, 0, 2500, 1440)), (565, 165, 1550, 1050));
        assert_eq!(workspace_to_screen((-1920, 0, -1120, 600), (-1920, 0, 1920, 1080), (-1920, 48, 1920, 1032)), (-1920, 48, 800, 600));
        assert_eq!(maximized_rect((0, 0, 2560, 1380), (9, 9)), (-9, -9, 2578, 1398));
        // WM_WINDOWPOSCHANGING: the current position / size fills in for SWP_NOMOVE / SWP_NOSIZE.
        let cur = (0, 0, 2560, 1440);
        assert_eq!(proposed_window_pos(0, (10, 20, 300, 200), cur), Some((10, 20, 300, 200)));
        assert_eq!(proposed_window_pos(SWP_NOMOVE.0, (10, 20, 300, 200), cur), Some((0, 0, 300, 200)));
        assert_eq!(proposed_window_pos(SWP_NOSIZE.0, (-1900, 20, 0, 0), cur), Some((-1900, 20, 2560, 1440)));
        assert_eq!(proposed_window_pos(SWP_NOMOVE.0 | SWP_NOSIZE.0, (10, 20, 300, 200), cur), None);
        assert_eq!(proposed_window_pos(0, (-32000, -32000, 160, 28), cur), None, "minimizing");
        // An empty work area never panics.
        assert_eq!(clamp_low(5, 10, 0), 10);
        let gone = Display { monitor: (0, 0, 0, 0), work: (0, 0, 0, 0), dpi: 96, frame: (8, 8) };
        let _ = restore_rect(&saved((505, 165, 1550, 1050), false), false, (0, 0, 0, 0), &gone);
    }

    #[test]
    fn leaving_full_screen_restores_the_previous_rectangle() {
        let here = Display { monitor: (0, 0, 2560, 1440), work: (0, 0, 2560, 1380), dpi: 120, frame: (9, 9) };
        let none = (0, 0, 0, 0);
        // Normal, maximized and snapped (left half) windows come back exactly where they were.
        let normal = saved((505, 165, 1550, 1050), false);
        assert_eq!(restore_rect(&normal, false, none, &here), (505, 165, 1550, 1050));
        let max = saved((-9, -9, 2578, 1398), true);
        assert_eq!(restore_rect(&max, true, (505, 165, 1550, 1050), &here), (-9, -9, 2578, 1398));
        let snapped = saved((-7, 0, 1294, 1387), false);
        assert_eq!(restore_rect(&snapped, false, none, &here), (-7, 0, 1294, 1387));
        // Maximized on a changed display (here a taskbar that grew): the new work area plus the frame overhang.
        let taller = Display { work: (0, 0, 2560, 1340), ..here };
        assert_eq!(restore_rect(&max, true, none, &taller), (-9, -9, 2578, 1358));
        // Un-maximized while full screen (Win+Down): Windows' restored rectangle.
        assert_eq!(restore_rect(&max, false, (400, 100, 1500, 1000), &here), (400, 100, 1500, 1000));
        // Moved to a 1080p monitor at 100 % on the left: same place relative to the work area, scaled, kept inside.
        let left = Display { monitor: (-1920, 0, 1920, 1080), work: (-1920, 0, 1920, 1040), dpi: 96, frame: (8, 8) };
        assert_eq!(restore_rect(&normal, false, none, &left), (-1920 + 404, 132, 1240, 840));
        let big = saved((100, 50, 2400, 1300), false);
        assert_eq!(restore_rect(&big, false, none, &left), (-1920, 0, 1920, 1040));
        assert_eq!(restore_rect(&max, true, none, &left), (-1928, -8, 1936, 1056));
    }

    #[test]
    fn full_screen_drops_only_the_frame_styles() {
        let style = 0x16CF_0000u32; // WS_OVERLAPPEDWINDOW | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN
        let ex = 0x0004_0110u32 | WS_EX_WINDOWEDGE.0;
        let (s, e) = full_screen_styles((style, ex), (style, ex), true);
        assert_eq!(s & (WS_CAPTION.0 | WS_THICKFRAME.0), 0);
        assert_eq!(s, style & !(WS_CAPTION.0 | WS_THICKFRAME.0));
        assert_eq!(e & WS_EX_WINDOWEDGE.0, 0);
        assert_eq!(full_screen_styles((s, e), (style, ex), false), (style, ex));
        // A maximized window keeps WS_MAXIMIZE through full screen, and bits changed meanwhile survive leaving.
        let maximized = style | 0x0100_0000;
        let (s, e) = full_screen_styles((maximized, ex), (maximized, ex), true);
        assert_ne!(s & 0x0100_0000, 0);
        assert_eq!(full_screen_styles((s & !0x0100_0000, e), (maximized, ex), false).0, style);
    }

    #[test]
    fn full_screen_track_sizes_never_clamp_the_monitor() {
        assert_eq!(min_max_track((136, 39), (2576, 1456), (950, 725), None), ((950, 725), (2576, 1456)));
        assert_eq!(min_max_track((136, 39), (2576, 1456), (950, 725), Some((0, 0, 3840, 2160))), ((950, 725), (3840, 2160)));
        // A monitor smaller than the minimum window size is still covered exactly.
        assert_eq!(min_max_track((136, 39), (1040, 784), (1330, 1015), Some((0, 0, 1024, 768))), ((1024, 768), (1040, 784)));
    }

    fn window_rect(hwnd: HWND) -> Rect {
        let mut r = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut r).unwrap() };
        (r.left, r.top, r.right - r.left, r.bottom - r.top)
    }

    unsafe extern "system" fn plain_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    fn test_window(name: windows::core::PCWSTR) -> HWND {
        unsafe {
            let inst = GetModuleHandleW(None).unwrap();
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(plain_proc),
                hInstance: inst.into(),
                lpszClassName: name,
                ..Default::default()
            };
            RegisterClassW(&class);
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                name,
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
            .unwrap()
        }
    }

    /// Dispatches the posted messages (the full-screen refit is posted).
    fn pump() {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn placement(hwnd: HWND) -> WINDOWPLACEMENT {
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        unsafe { GetWindowPlacement(hwnd, &mut wp).unwrap() };
        wp
    }

    /// Never shown (no `WS_VISIBLE`, nothing that shows or activates it), so it runs in every `cargo test` without
    /// touching the desktop. The maximize / minimize parts need a visible window: see
    /// `a_visible_window_maximizes_minimizes_and_goes_full_screen` (`WINDOW_E2E=1`).
    #[test]
    fn a_hidden_window_gets_the_extended_client_area_hit_tests_and_full_screen() {
        unsafe {
            let hwnd = test_window(w!("SaChromeTestHidden"));
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
            SendMessageW(hwnd, WM_NCMOUSELEAVE, None, None);
            let changes = std::rc::Rc::new(Cell::new(0));
            let c2 = changes.clone();
            chrome.set_system_change_handler(Box::new(move || c2.set(c2.get() + 1)));
            SendMessageW(hwnd, WM_SETTINGCHANGE, None, None);
            SendMessageW(hwnd, WM_THEMECHANGED, None, None);
            assert_eq!(changes.get(), 2);
            assert_eq!(*hovers.borrow(), vec![CaptionHover::MaximizeHover, CaptionHover::MaximizePressed, CaptionHover::None]);

            // Full screen from a restored window.
            let before = (window_rect(hwnd), GetWindowLongPtrW(hwnd, GWL_STYLE), placement(hwnd).rcNormalPosition);
            let restored = chrome.restored_size();
            chrome.set_full_screen(true);
            assert!(chrome.is_full_screen());
            let g = chrome.geometry();
            assert_eq!(g.client, g.monitor, "restored -> full screen: client == monitor");
            assert_eq!(g.window, g.monitor);
            assert_eq!(at(150, 1), HTCLIENT);
            assert_eq!(at(310, 30), HTCLIENT);
            assert_eq!(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & (WS_CAPTION.0 | WS_THICKFRAME.0), 0);
            assert_eq!(chrome.restored_size(), restored, "the size before full screen is what is stored");
            chrome.set_full_screen(true); // no-op
            // WM_NCCALCSIZE with wParam FALSE: the whole window is client area.
            let mut r = RECT { left: 10, top: 20, right: 810, bottom: 620 };
            SendMessageW(hwnd, WM_NCCALCSIZE, Some(WPARAM(0)), Some(LPARAM(&mut r as *mut RECT as isize)));
            assert_eq!((r.left, r.top, r.right, r.bottom), (10, 20, 810, 620));
            // winit rewriting the style puts no frame back while full screen.
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            SetWindowLongPtrW(hwnd, GWL_STYLE, (style | WS_CAPTION.0 | WS_THICKFRAME.0) as isize);
            assert_eq!(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & (WS_CAPTION.0 | WS_THICKFRAME.0), 0, "re-stripped");
            // A move, a resize (SWP_NOMOVE only), a move (SWP_NOSIZE only): all end on the monitor.
            let m = g.monitor;
            let _ = SetWindowPos(hwnd, None, m.0 + 50, m.1 + 50, 640, 480, SWP_NOZORDER | SWP_NOACTIVATE);
            assert_eq!(chrome.geometry().client, m, "kept on the monitor");
            let _ = SetWindowPos(hwnd, None, 0, 0, 640, 480, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE);
            assert_eq!(chrome.geometry().client, m, "a resize alone");
            let _ = SetWindowPos(hwnd, None, m.0 + 30, m.1 + 30, 0, 0, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSIZE);
            assert_eq!(chrome.geometry().client, m, "a move alone");
            // The shell recording the monitor rectangle as the restore rectangle (Win+Up / Win+Left in full screen).
            let mut wp = placement(hwnd);
            wp.rcNormalPosition = RECT { left: m.0, top: m.1, right: m.0 + m.2, bottom: m.1 + m.3 };
            wp.showCmd = SW_HIDE.0 as u32;
            SetWindowPlacement(hwnd, &wp).unwrap();
            pump();
            assert_eq!(chrome.geometry().client, m);
            chrome.set_full_screen(false);
            pump();
            let after = (window_rect(hwnd), GetWindowLongPtrW(hwnd, GWL_STYLE), placement(hwnd).rcNormalPosition);
            assert_eq!(after, before, "restored again, with the restore rectangle from before");
            assert_eq!(at(150, 1), HTTOP, "the frame is back");
            assert!(!IsWindowVisible(hwnd).as_bool(), "never shown");

            let settings = sa_core::settings::InMemorySettingsStore::new();
            chrome.save_size(settings.as_ref(), MAIN_SIZE_KEY);
            assert!(saved_size(settings.as_ref(), MAIN_SIZE_KEY).is_some());
            chrome.set_dark(true);
            chrome.set_mica(true);
            drop(chrome);
            let _ = DestroyWindow(hwnd);
        }
    }

    /// Shows, maximizes and minimizes a window that covers a whole monitor (taskbar included) and can take the
    /// focus, so it only runs with `WINDOW_E2E=1`:
    /// `WINDOW_E2E=1 cargo test -p sa-platform a_visible_window -- --nocapture` (PowerShell: `$env:WINDOW_E2E=1`).
    #[test]
    fn a_visible_window_maximizes_minimizes_and_goes_full_screen() {
        if std::env::var_os("WINDOW_E2E").is_none_or(|v| v != "1") {
            eprintln!("skipped: set WINDOW_E2E=1 to run the visible-window test");
            return;
        }
        unsafe {
            let hwnd = test_window(w!("SaChromeTestVisible"));
            let chrome = WindowChrome::attach(hwnd.0 as isize, MAIN_MIN_DIP);
            chrome.place(None, MAIN_DEFAULT_DIP);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let normal = (window_rect(hwnd), GetWindowLongPtrW(hwnd, GWL_STYLE), placement(hwnd).rcNormalPosition);

            // The Slint-drawn maximize button (non-client) maximizes on click.
            let hovers = std::rc::Rc::new(RefCell::new(Vec::new()));
            let h2 = hovers.clone();
            chrome.set_caption_hover_handler(Box::new(move |h| h2.borrow_mut().push(h)));
            SendMessageW(hwnd, WM_NCMOUSEMOVE, Some(WPARAM(HTMAXBUTTON as usize)), Some(LPARAM(0)));
            SendMessageW(hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(HTMAXBUTTON as usize)), Some(LPARAM(0)));
            SendMessageW(hwnd, WM_NCLBUTTONUP, Some(WPARAM(HTMAXBUTTON as usize)), Some(LPARAM(0)));
            assert!(chrome.is_maximized());
            assert_eq!(chrome.restored_size(), None);
            SendMessageW(hwnd, WM_MOUSEMOVE, None, None);
            assert_eq!(
                *hovers.borrow(),
                vec![CaptionHover::MaximizeHover, CaptionHover::MaximizePressed, CaptionHover::MaximizeHover, CaptionHover::None]
            );

            // Full screen from maximized.
            let before = (window_rect(hwnd), GetWindowLongPtrW(hwnd, GWL_STYLE));
            chrome.set_full_screen(true);
            let g = chrome.geometry();
            assert_eq!(g.client, g.monitor, "maximized -> full screen: client == monitor");
            assert_eq!(chrome.restored_size(), None, "maximized before: nothing to store");
            // Minimize and restore while full screen: back on the monitor.
            let _ = ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
            pump();
            assert!(IsIconic(hwnd).as_bool());
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            pump();
            assert_eq!(chrome.geometry().client, g.monitor, "restored from minimized onto the monitor");
            chrome.set_full_screen(false);
            pump();
            assert_eq!((window_rect(hwnd), GetWindowLongPtrW(hwnd, GWL_STYLE)), before, "maximized again");
            assert!(chrome.is_maximized());

            // Entered while minimized (from a restored window): it stays minimized and restores onto the monitor;
            // leaving puts it back where it was before it was minimized, not at the off-screen icon.
            chrome.toggle_maximize();
            pump();
            assert_eq!((window_rect(hwnd), GetWindowLongPtrW(hwnd, GWL_STYLE), placement(hwnd).rcNormalPosition), normal);
            let _ = ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
            pump();
            chrome.set_full_screen(true);
            pump();
            assert!(IsIconic(hwnd).as_bool(), "still minimized");
            assert_eq!(chrome.restored_size(), Some(((normal.0).2 as u32, (normal.0).3 as u32)));
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            pump();
            assert_eq!(chrome.geometry().client, g.monitor, "restored onto the monitor");
            chrome.set_full_screen(false);
            pump();
            assert_eq!((window_rect(hwnd), GetWindowLongPtrW(hwnd, GWL_STYLE), placement(hwnd).rcNormalPosition), normal);

            drop(chrome);
            let _ = DestroyWindow(hwnd);
        }
    }

}
