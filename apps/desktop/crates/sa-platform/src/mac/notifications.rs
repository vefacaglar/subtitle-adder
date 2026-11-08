//! `NSNotificationCenter` observers delivered on the main queue (main thread), removed on drop. Used for the
//! notifications the Swift app followed and winit/Slint do not surface:
//!
//! * volumes mounted/unmounted (`NSWorkspace` center; explorer quick locations and hub storage, mac-ui §10.2);
//! * system colors / accent changed (`NSSystemColorsDidChangeNotification`, mac-ui §14.2);
//! * application will terminate (`platform.prepareForQuit()`, temp cleanup, mac-ui §2.1 step 10);
//! * a window became key / will close (`onBecomeKey` → `claimPlayback`, `onClose` → detach, mac-ui §3.2).

use std::ffi::c_void;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

use super::ffi::ns_string;

pub const WORKSPACE_DID_MOUNT: &str = "NSWorkspaceDidMountNotification";
pub const WORKSPACE_DID_UNMOUNT: &str = "NSWorkspaceDidUnmountNotification";
pub const SYSTEM_COLORS_DID_CHANGE: &str = "NSSystemColorsDidChangeNotification";
pub const APPLICATION_WILL_TERMINATE: &str = "NSApplicationWillTerminateNotification";
pub const WINDOW_DID_BECOME_KEY: &str = "NSWindowDidBecomeKeyNotification";
pub const WINDOW_WILL_CLOSE: &str = "NSWindowWillCloseNotification";
pub const WINDOW_DID_ENTER_FULL_SCREEN: &str = "NSWindowDidEnterFullScreenNotification";
pub const WINDOW_DID_EXIT_FULL_SCREEN: &str = "NSWindowDidExitFullScreenNotification";
pub const COLOR_PANEL_COLOR_DID_CHANGE: &str = "NSColorPanelColorDidChangeNotification";

/// One registration; dropping it removes the observer.
pub struct NotificationObserver {
    center: Retained<AnyObject>,
    token: Option<Retained<AnyObject>>,
}

impl NotificationObserver {
    /// `[center addObserverForName:object:queue:NSOperationQueue.mainQueue usingBlock:]`.
    unsafe fn add(center: Retained<AnyObject>, name: &str, object: Option<&AnyObject>, f: Box<dyn Fn()>) -> Self {
        unsafe {
            let n = ns_string(name);
            let queue: Retained<AnyObject> = msg_send![class!(NSOperationQueue), mainQueue];
            let block: RcBlock<dyn Fn(NonNull<AnyObject>)> = RcBlock::new(move |_note: NonNull<AnyObject>| f());
            let token: Option<Retained<AnyObject>> =
                msg_send![&*center, addObserverForName: &*n, object: object, queue: &*queue, usingBlock: &*block];
            Self { center, token }
        }
    }

    /// An app notification (`NSNotificationCenter.defaultCenter`), from any sender.
    pub fn app(name: &str, f: Box<dyn Fn()>) -> Self {
        autoreleasepool(|_| unsafe {
            let center: Retained<AnyObject> = msg_send![class!(NSNotificationCenter), defaultCenter];
            Self::add(center, name, None, f)
        })
    }

    /// A notification of one object (e.g. the shared color panel).
    pub fn of_object(name: &str, object: &AnyObject, f: Box<dyn Fn()>) -> Self {
        autoreleasepool(|_| unsafe {
            let center: Retained<AnyObject> = msg_send![class!(NSNotificationCenter), defaultCenter];
            Self::add(center, name, Some(object), f)
        })
    }

    /// A window notification of the `NSWindow` that holds `ns_view` (raw-window-handle `AppKitWindowHandle`);
    /// `None` when the view has no window yet.
    pub unsafe fn of_window(name: &str, ns_view: *mut c_void, f: Box<dyn Fn()>) -> Option<Self> {
        let window = unsafe { super::appkit::window_of_view(ns_view) }?;
        Some(Self::of_object(name, &window, f))
    }

    /// An `NSWorkspace` notification (its own notification center).
    pub fn workspace(name: &str, f: Box<dyn Fn()>) -> Self {
        autoreleasepool(|_| unsafe {
            let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
            let center: Retained<AnyObject> = msg_send![&*ws, notificationCenter];
            Self::add(center, name, None, f)
        })
    }
}

impl Drop for NotificationObserver {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            autoreleasepool(|_| unsafe {
                let _: () = msg_send![&*self.center, removeObserver: &*token];
            });
        }
    }
}

/// Calls `f` (main thread) whenever a volume is mounted or unmounted.
pub fn observe_volume_changes(f: impl Fn() + Clone + 'static) -> [NotificationObserver; 2] {
    [
        NotificationObserver::workspace(WORKSPACE_DID_MOUNT, Box::new(f.clone())),
        NotificationObserver::workspace(WORKSPACE_DID_UNMOUNT, Box::new(f)),
    ]
}
