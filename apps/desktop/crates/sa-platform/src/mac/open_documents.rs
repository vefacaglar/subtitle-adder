//! Finder "Open With", double click, Dock drop (mac-ui §13.2): an Apple Event handler for `kAEOpenDocuments`
//! ('aevt','odoc') registered with `NSAppleEventManager`. AppKit installs its own handlers while it finishes
//! launching, so ours is installed right away **and** again on `NSApplicationWillFinishLaunchingNotification`
//! (after AppKit's, before the cold-launch `odoc` is dispatched). Every file URL goes through a
//! [`CoalescingDelivery`] (250 ms quiet / 1 s max); batches that arrive before the sink is set are replayed in order.
//! The reopen handler ('aevt','rapp': Dock click, [`OpenDocumentsHandler::set_reopen_handler`]) does AppKit's default
//! part (unhide, deminiaturize, `appkit::reopen_existing_windows`) and asks the app for a new main window when none is
//! left, like a SwiftUI `WindowGroup` (mac-ui §2.3).

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::OnceLock;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, Sel};
use objc2::{ClassType, class, msg_send, sel};
use parking_lot::Mutex;
use sa_core::platform::{ExternalOpenSource, InstanceRole};

use super::ffi::{ns_string, url_path};
use crate::coalescer::CoalescingDelivery;

const K_CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const K_AE_OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
const K_AE_REOPEN_APPLICATION: u32 = u32::from_be_bytes(*b"rapp");
const KEY_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

static DELIVERY: OnceLock<CoalescingDelivery> = OnceLock::new();
static REOPEN: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);
static HANDLER: OnceLock<usize> = OnceLock::new();

fn delivery() -> &'static CoalescingDelivery {
    DELIVERY.get_or_init(CoalescingDelivery::default)
}

/// File paths of an `odoc` event's direct object (a list of file URLs, or a single one).
unsafe fn event_paths(event: *mut AnyObject) -> Vec<PathBuf> {
    if event.is_null() {
        return Vec::new();
    }
    unsafe {
        let event = &*event;
        let direct: Option<Retained<AnyObject>> = msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
        let Some(direct) = direct else { return Vec::new() };
        let count: isize = msg_send![&*direct, numberOfItems];
        let mut out = Vec::new();
        if count <= 0 {
            if let Some(p) = descriptor_path(&direct) {
                out.push(p);
            }
            return out;
        }
        for i in 1..=count {
            let item: Option<Retained<AnyObject>> = msg_send![&*direct, descriptorAtIndex: i];
            if let Some(p) = item.as_deref().and_then(|d| descriptor_path(d)) {
                out.push(p);
            }
        }
        out
    }
}

unsafe fn descriptor_path(d: &AnyObject) -> Option<PathBuf> {
    unsafe {
        let url: Option<Retained<AnyObject>> = msg_send![d, fileURLValue];
        let url = url?;
        let is_file: bool = msg_send![&*url, isFileURL];
        if !is_file {
            return None;
        }
        url_path(&url)
    }
}

extern "C-unwind" fn handle_open_documents(_this: *mut AnyObject, _sel: Sel, event: *mut AnyObject, _reply: *mut AnyObject) {
    let paths = autoreleasepool(|_| unsafe { event_paths(event) });
    delivery().receive(paths);
}

extern "C-unwind" fn handle_reopen(_this: *mut AnyObject, _sel: Sel, _event: *mut AnyObject, _reply: *mut AnyObject) {
    // This handler replaces AppKit's, so its default part (unhide, deminiaturize) runs here first. `unhide:` does not
    // promise that the windows are back on screen when it returns, so after an unhide the window check waits one
    // main-queue turn; otherwise a hidden app with a window would look windowless and get a second main window.
    if super::appkit::unhide_if_hidden() {
        unsafe { dispatch_async_f(std::ptr::addr_of!(_dispatch_main_q).cast(), std::ptr::null_mut(), finish_reopen) };
    } else {
        finish_reopen(std::ptr::null_mut());
    }
}

/// Deminiaturizes a window, or asks the app for a new main window when none is left (main thread).
extern "C" fn finish_reopen(_context: *mut c_void) {
    if super::appkit::reopen_existing_windows()
        && let Some(f) = REOPEN.lock().as_ref()
    {
        f();
    }
}

// libdispatch (libSystem): `dispatch_get_main_queue()` is the address of `_dispatch_main_q`.
unsafe extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
}

extern "C-unwind" fn will_finish_launching(this: *mut AnyObject, _sel: Sel, _note: *mut AnyObject) {
    if !this.is_null() {
        unsafe { register_handlers(&*this) };
    }
}

fn handler_class() -> &'static AnyClass {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        if let Some(c) = AnyClass::get(c"SAOpenDocumentsHandler") {
            return c as *const AnyClass as usize;
        }
        let mut b = ClassBuilder::new(c"SAOpenDocumentsHandler", NSObject::class()).expect("class name is free");
        unsafe {
            b.add_method(
                sel!(handleOpenDocuments:withReplyEvent:),
                handle_open_documents as extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
            );
            b.add_method(
                sel!(handleReopen:withReplyEvent:),
                handle_reopen as extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
            );
            b.add_method(sel!(willFinishLaunching:), will_finish_launching as extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject));
        }
        b.register() as *const AnyClass as usize
    });
    unsafe { &*(ptr as *const AnyClass) }
}

unsafe fn register_handlers(handler: &AnyObject) {
    unsafe {
        let manager: Retained<AnyObject> = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
        let _: () = msg_send![
            &*manager,
            setEventHandler: handler,
            andSelector: sel!(handleOpenDocuments:withReplyEvent:),
            forEventClass: K_CORE_EVENT_CLASS,
            andEventID: K_AE_OPEN_DOCUMENTS
        ];
        if REOPEN.lock().is_some() {
            let _: () = msg_send![
                &*manager,
                setEventHandler: handler,
                andSelector: sel!(handleReopen:withReplyEvent:),
                forEventClass: K_CORE_EVENT_CLASS,
                andEventID: K_AE_REOPEN_APPLICATION
            ];
        }
    }
}

fn handler_object() -> &'static AnyObject {
    let ptr = *HANDLER.get_or_init(|| {
        let obj: Retained<AnyObject> = unsafe { msg_send![handler_class(), new] };
        // Kept for the process lifetime (Apple Event handlers are not retained by the manager).
        Retained::into_raw(obj) as usize
    });
    unsafe { &*(ptr as *const AnyObject) }
}

pub struct OpenDocumentsHandler {
    _private: (),
}

impl OpenDocumentsHandler {
    /// Must run on the main thread before the Slint event loop starts (cold-launch files arrive during launch).
    pub fn install() -> Self {
        autoreleasepool(|_| unsafe {
            let handler = handler_object();
            register_handlers(handler);
            let center: Retained<AnyObject> = msg_send![class!(NSNotificationCenter), defaultCenter];
            let name = ns_string("NSApplicationWillFinishLaunchingNotification");
            let none: Option<&AnyObject> = None;
            let _: () = msg_send![&*center, addObserver: handler, selector: sel!(willFinishLaunching:), name: &*name, object: none];
        });
        Self { _private: () }
    }

    /// Dock-icon click (`rapp`): replaces AppKit's reopen handling. A hidden app is unhidden and a miniaturized
    /// window comes back when none is visible; `f` runs (main thread, inside the Apple Event dispatch) only when no
    /// window is left, and opens a new main window.
    pub fn set_reopen_handler(&self, f: Box<dyn Fn() + Send>) {
        *REOPEN.lock() = Some(f);
        autoreleasepool(|_| unsafe { register_handlers(handler_object()) });
    }

    /// Delivers what is pending now (application will terminate).
    pub fn flush(&self) {
        delivery().flush();
    }
}

impl ExternalOpenSource for OpenDocumentsHandler {
    fn acquire_or_forward(&mut self, _initial_paths: &[PathBuf]) -> InstanceRole {
        // Launch Services keeps one process per bundle; every launch is primary.
        InstanceRole::Primary
    }
    fn set_sink(&mut self, sink: Box<dyn FnMut(Vec<PathBuf>) + Send>) {
        delivery().set_sink(sink);
    }
}
