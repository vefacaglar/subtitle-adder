//! The overlay text color picker (SwiftUI `ColorPicker`, mac-ui §1 and §11.3): `NSColorPanel.sharedColorPanel`
//! without opacity, opened from a color-well-looking Slint button. Every change (continuous while dragging) is
//! reported in sRGB; the Settings view stores it as `#RRGGBB` ([`crate::mac_rules::hex_from_components`]).
//! Main thread.

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

use super::notifications::{COLOR_PANEL_COLOR_DID_CHANGE, NotificationObserver};

/// An open picking session; dropping it stops reporting (the panel itself stays where the user left it).
pub struct ColorPanelSession {
    _observer: NotificationObserver,
}

fn shared_panel() -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSColorPanel), sharedColorPanel] }
}

/// sRGB components of an `NSColor` (`None` for pattern/catalog colors that cannot be converted).
unsafe fn srgb_components(color: &AnyObject) -> Option<[f64; 3]> {
    unsafe {
        let space: Retained<AnyObject> = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let rgb: Option<Retained<AnyObject>> = msg_send![color, colorUsingColorSpace: &*space];
        let rgb = rgb?;
        let r: f64 = msg_send![&*rgb, redComponent];
        let g: f64 = msg_send![&*rgb, greenComponent];
        let b: f64 = msg_send![&*rgb, blueComponent];
        Some([r, g, b])
    }
}

/// Shows the shared color panel with `initial` (sRGB 0..1) and calls `on_change` with every new color.
pub fn show(initial: [f64; 3], on_change: Box<dyn Fn([f64; 3])>) -> ColorPanelSession {
    autoreleasepool(|_| unsafe {
        let panel = shared_panel();
        let _: () = msg_send![&*panel, setShowsAlpha: false];
        let _: () = msg_send![&*panel, setContinuous: true];
        let color: Retained<AnyObject> =
            msg_send![class!(NSColor), colorWithSRGBRed: initial[0], green: initial[1], blue: initial[2], alpha: 1.0f64];
        // Observe after setting the start color, so opening the panel reports nothing.
        let _: () = msg_send![&*panel, setColor: &*color];
        let p = panel.clone();
        let observer = NotificationObserver::of_object(
            COLOR_PANEL_COLOR_DID_CHANGE,
            &panel,
            Box::new(move || {
                let c: Option<Retained<AnyObject>> = msg_send![&*p, color];
                if let Some(rgb) = c.as_deref().and_then(|c| srgb_components(c)) {
                    on_change(rgb);
                }
            }),
        );
        let none: Option<&AnyObject> = None;
        let _: () = msg_send![&*panel, makeKeyAndOrderFront: none];
        ColorPanelSession { _observer: observer }
    })
}

/// Hides the shared panel (Settings window closed).
pub fn close() {
    autoreleasepool(|_| unsafe {
        let panel = shared_panel();
        let _: () = msg_send![&*panel, orderOut: Option::<&AnyObject>::None];
    });
}
