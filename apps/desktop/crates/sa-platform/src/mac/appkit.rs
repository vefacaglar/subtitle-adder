//! AppKit glue that Slint/winit/rfd do not provide (main thread unless noted): process preparation, `NSApp`
//! appearance, Finder, fonts, memory, title-bar double click and full screen on a window, the standard menu actions
//! and the Services / Window / Help menu roles of the Slint `MenuBar` (mac-ui §2.1, §3.2, §4, §14.1).

use std::ffi::c_void;
use std::path::Path;

use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};

use super::defaults::copy_any_app_value;
use super::ffi::{array_items, file_url, ns_array, ns_string, responds, rust_string, shared_application};
use crate::mac_rules::{TitleBarDoubleClick, appearance_name, title_bar_double_click};

const NS_APPLICATION_ACTIVATION_POLICY_REGULAR: isize = 0;
const NS_WINDOW_STYLE_MASK_FULL_SCREEN: usize = 1 << 14;

/// mac-ui §2.1 steps 1–2: ignore SIGPIPE (an engine that exits early must not kill the app on a stdin write), then
/// a regular activation policy and activation (Dock icon and front even when run unbundled).
pub fn prepare_process() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    autoreleasepool(|_| unsafe {
        let app = shared_application();
        let _: bool = msg_send![&*app, setActivationPolicy: NS_APPLICATION_ACTIVATION_POLICY_REGULAR];
        if responds(&app, sel!(activate)) {
            let _: () = msg_send![&*app, activate];
        } else {
            let _: () = msg_send![&*app, activateIgnoringOtherApps: true];
        }
    });
}

/// `NSApp.appearance` = nil (system) / Aqua / DarkAqua; affects title bars, menus, alerts, panels.
pub fn apply_appearance(theme: Option<&str>) {
    autoreleasepool(|_| unsafe {
        let app = shared_application();
        match appearance_name(theme) {
            Some(name) => {
                let n = ns_string(name);
                let appearance: Option<Retained<AnyObject>> = msg_send![class!(NSAppearance), appearanceNamed: &*n];
                let _: () = msg_send![&*app, setAppearance: appearance.as_deref()];
            }
            None => {
                let none: Option<&AnyObject> = None;
                let _: () = msg_send![&*app, setAppearance: none];
            }
        }
    });
}

/// Whether the effective app appearance is dark (`NSApp.effectiveAppearance` best match).
pub fn is_dark_appearance() -> bool {
    autoreleasepool(|_| unsafe {
        let app = shared_application();
        let appearance: Option<Retained<AnyObject>> = msg_send![&*app, effectiveAppearance];
        let Some(appearance) = appearance else { return false };
        let aqua = ns_string("NSAppearanceNameAqua");
        let dark = ns_string("NSAppearanceNameDarkAqua");
        let names = ns_array(&[&*aqua, &*dark]);
        let best: Option<Retained<AnyObject>> = msg_send![&*appearance, bestMatchFromAppearancesWithNames: &*names];
        best.is_some_and(|b| rust_string(&b) == "NSAppearanceNameDarkAqua")
    })
}

/// `NSWorkspace activateFileViewerSelectingURLs:` (any thread).
pub fn reveal_in_finder(path: &Path) {
    autoreleasepool(|_| unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let url = file_url(path);
        let urls = ns_array(&[&*url]);
        let _: () = msg_send![&*ws, activateFileViewerSelectingURLs: &*urls];
    });
}

/// `NSWorkspace openURL:` (a folder opens in Finder).
pub fn open_url(path: &Path) {
    autoreleasepool(|_| unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let url = file_url(path);
        let _: bool = msg_send![&*ws, openURL: &*url];
    });
}

/// Opens a web URL with the default browser.
pub fn open_web_url(url: &str) {
    autoreleasepool(|_| unsafe {
        let s = ns_string(url);
        let u: Option<Retained<AnyObject>> = msg_send![class!(NSURL), URLWithString: &*s];
        if let Some(u) = u {
            let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
            let _: bool = msg_send![&*ws, openURL: &*u];
        }
    });
}

/// `NSProcessInfo.processInfo.physicalMemory`.
pub fn physical_memory() -> u64 {
    autoreleasepool(|_| unsafe {
        let info: Retained<AnyObject> = msg_send![class!(NSProcessInfo), processInfo];
        msg_send![&*info, physicalMemory]
    })
}

/// `NSFontManager.sharedFontManager.availableFontFamilies` (AppKit's order).
pub fn font_families() -> Vec<String> {
    autoreleasepool(|_| unsafe {
        let fm: Retained<AnyObject> = msg_send![class!(NSFontManager), sharedFontManager];
        let families: Option<Retained<AnyObject>> = msg_send![&*fm, availableFontFamilies];
        families.map(|f| array_items(&f).iter().map(|s| rust_string(s)).collect()).unwrap_or_default()
    })
}

/// The user's title-bar double-click action (global `AppleActionOnDoubleClick`).
pub fn title_bar_double_click_action() -> TitleBarDoubleClick {
    let action = copy_any_app_value("AppleActionOnDoubleClick").and_then(|v| v.as_string());
    let mini = copy_any_app_value("AppleMiniaturizeOnDoubleClick").and_then(|v| v.as_bool());
    title_bar_double_click(action.as_deref(), mini)
}

/// The `NSWindow` of an `NSView*` (raw-window-handle `AppKitWindowHandle::ns_view`).
pub unsafe fn window_of_view(ns_view: *mut c_void) -> Option<Retained<AnyObject>> {
    if ns_view.is_null() {
        return None;
    }
    let view = ns_view as *mut AnyObject;
    unsafe { msg_send![&*view, window] }
}

/// A double click on empty toolbar space: zoom / minimize / nothing per the user's setting.
pub unsafe fn perform_title_bar_double_click(ns_view: *mut c_void) {
    autoreleasepool(|_| unsafe {
        let Some(w) = window_of_view(ns_view) else { return };
        let none: Option<&AnyObject> = None;
        match title_bar_double_click_action() {
            TitleBarDoubleClick::Zoom => {
                let _: () = msg_send![&*w, performZoom: none];
            }
            TitleBarDoubleClick::Minimize => {
                let _: () = msg_send![&*w, performMiniaturize: none];
            }
            TitleBarDoubleClick::None => {}
        }
    });
}

/// Whether the window is in native full screen (style mask), to keep `isFullScreen` in step.
pub unsafe fn is_full_screen(ns_view: *mut c_void) -> bool {
    autoreleasepool(|_| unsafe {
        window_of_view(ns_view).is_some_and(|w| {
            let mask: usize = msg_send![&*w, styleMask];
            mask & NS_WINDOW_STYLE_MASK_FULL_SCREEN != 0
        })
    })
}

/// Native (Spaces) full screen on/off.
pub unsafe fn toggle_full_screen(ns_view: *mut c_void) {
    autoreleasepool(|_| unsafe {
        if let Some(w) = window_of_view(ns_view) {
            let none: Option<&AnyObject> = None;
            let _: () = msg_send![&*w, toggleFullScreen: none];
        }
    });
}

/// `NSWindow.title` (Mission Control, Window menu; the visible title is drawn by the toolbar).
pub unsafe fn set_window_title(ns_view: *mut c_void, title: &str) {
    autoreleasepool(|_| unsafe {
        if let Some(w) = window_of_view(ns_view) {
            let t = ns_string(title);
            let _: () = msg_send![&*w, setTitle: &*t];
        }
    });
}

/// Standard actions AppKit used to supply for the menu items the Rust app builds itself (mac-ui §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardAction {
    About,
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    Minimize,
    Zoom,
    ToggleFullScreen,
    BringAllToFront,
}

impl StandardAction {
    pub fn selector(self) -> Sel {
        match self {
            StandardAction::About => sel!(orderFrontStandardAboutPanel:),
            StandardAction::Hide => sel!(hide:),
            StandardAction::HideOthers => sel!(hideOtherApplications:),
            StandardAction::ShowAll => sel!(unhideAllApplications:),
            StandardAction::Quit => sel!(terminate:),
            StandardAction::Cut => sel!(cut:),
            StandardAction::Copy => sel!(copy:),
            StandardAction::Paste => sel!(paste:),
            StandardAction::Delete => sel!(delete:),
            StandardAction::SelectAll => sel!(selectAll:),
            StandardAction::Minimize => sel!(performMiniaturize:),
            StandardAction::Zoom => sel!(performZoom:),
            StandardAction::ToggleFullScreen => sel!(toggleFullScreen:),
            StandardAction::BringAllToFront => sel!(arrangeInFront:),
        }
    }
}

/// Sends a standard action down the responder chain (`NSApp sendAction:to:nil from:nil`); `false` when nobody
/// handled it.
pub fn perform_standard_action(action: StandardAction) -> bool {
    autoreleasepool(|_| unsafe {
        let app = shared_application();
        let none: Option<&AnyObject> = None;
        msg_send![&*app, sendAction: action.selector(), to: none, from: none]
    })
}

unsafe fn submenu_titled(menu: &AnyObject, title: &str) -> Option<(Retained<AnyObject>, Option<Retained<AnyObject>>)> {
    unsafe {
        let items: Option<Retained<AnyObject>> = msg_send![menu, itemArray];
        let items = items?;
        for item in array_items(&items) {
            let t: Option<Retained<AnyObject>> = msg_send![&*item, title];
            if t.as_deref().map(rust_string).as_deref() == Some(title) {
                let sub: Option<Retained<AnyObject>> = msg_send![&*item, submenu];
                return Some((item, sub));
            }
        }
        None
    }
}

/// Gives the Slint-built menu bar its AppKit roles, matched by (current) title: the item `services` in the app menu
/// becomes `NSApp.servicesMenu` (a submenu is created when missing), the top-level `window` menu becomes
/// `NSApp.windowsMenu` (AppKit then keeps the window list) and `help` becomes `NSApp.helpMenu` (search field).
/// Call after the menu bar exists and again after every relabel. Returns how many roles were set.
pub fn install_special_menus(services: &str, window: &str, help: &str) -> usize {
    autoreleasepool(|_| unsafe {
        let app = shared_application();
        let main: Option<Retained<AnyObject>> = msg_send![&*app, mainMenu];
        let Some(main) = main else { return 0 };
        let mut n = 0;
        let items: Option<Retained<AnyObject>> = msg_send![&*main, itemArray];
        let first_sub = items.and_then(|i| array_items(&i).into_iter().next()).and_then(|i| -> Option<Retained<AnyObject>> {
            msg_send![&*i, submenu]
        });
        if let Some(app_menu) = first_sub
            && let Some((item, sub)) = submenu_titled(&app_menu, services)
        {
            let sub = match sub {
                Some(s) => s,
                None => {
                    let t = ns_string(services);
                    let m: Allocated<AnyObject> = msg_send![class!(NSMenu), alloc];
                    let m: Retained<AnyObject> = msg_send![m, initWithTitle: &*t];
                    let _: () = msg_send![&*item, setSubmenu: &*m];
                    m
                }
            };
            let _: () = msg_send![&*app, setServicesMenu: &*sub];
            n += 1;
        }
        if let Some((_, Some(sub))) = submenu_titled(&main, window) {
            let _: () = msg_send![&*app, setWindowsMenu: &*sub];
            n += 1;
        }
        if let Some((_, Some(sub))) = submenu_titled(&main, help) {
            let _: () = msg_send![&*app, setHelpMenu: &*sub];
            n += 1;
        }
        n
    })
}

/// Live system accent color (`NSColor.controlAccentColor` in sRGB) as 0..1 RGBA.
pub fn accent_color() -> Option<[f64; 4]> {
    named_color(sel!(controlAccentColor))
}

/// A semantic `NSColor` class method (`labelColor`, `separatorColor`, `windowBackgroundColor`, …) in sRGB, resolved
/// for the current app appearance.
pub fn named_color(selector: Sel) -> Option<[f64; 4]> {
    autoreleasepool(|_| unsafe {
        let cls = class!(NSColor);
        let color: Option<Retained<AnyObject>> = msg_send![cls, performSelector: selector];
        let color = color?;
        let space: Retained<AnyObject> = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let rgb: Option<Retained<AnyObject>> = msg_send![&*color, colorUsingColorSpace: &*space];
        let rgb = rgb?;
        let r: f64 = msg_send![&*rgb, redComponent];
        let g: f64 = msg_send![&*rgb, greenComponent];
        let b: f64 = msg_send![&*rgb, blueComponent];
        let a: f64 = msg_send![&*rgb, alphaComponent];
        Some([r, g, b, a])
    })
}
