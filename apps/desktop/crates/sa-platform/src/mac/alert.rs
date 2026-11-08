//! Confirmations as `NSAlert` (mac-ui §1 "confirmationDialog / alert", §7.8, §9.5): a sheet on the window that owns
//! dialogs ([`crate::dialogs::set_dialog_parent`]), app-modal when there is none. The first button is the default
//! (Return), a `Cancel` button answers Esc, a `Destructive` button is drawn red (`hasDestructiveAction`). Main thread.
//!
//! The response is the index of the pressed button (`NSAlertFirstButtonReturn` = 1000 → 0), like
//! [`sa_core::platform::AlertResponse`]; Esc gives the cancel button's index.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use block2::RcBlock;
use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use sa_core::future::LocalBoxFuture;
use sa_core::platform::{Alert, AlertResponse};

use super::ffi::ns_string;
use crate::mac_rules::{alert_button_index, alert_key_equivalent};

#[derive(Default)]
struct Slot {
    value: Option<AlertResponse>,
    done: bool,
    waker: Option<Waker>,
}

struct Pending(Rc<RefCell<Slot>>);

impl Future for Pending {
    type Output = AlertResponse;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<AlertResponse> {
        let mut s = self.0.borrow_mut();
        if s.done {
            return Poll::Ready(s.value.take().flatten());
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

unsafe fn build(alert: &Alert) -> Retained<AnyObject> {
    unsafe {
        let a: Allocated<AnyObject> = msg_send![class!(NSAlert), alloc];
        let a: Retained<AnyObject> = msg_send![a, init];
        let title = ns_string(&alert.title);
        let message = ns_string(&alert.message);
        let _: () = msg_send![&*a, setMessageText: &*title];
        let _: () = msg_send![&*a, setInformativeText: &*message];
        for b in &alert.buttons {
            let t = ns_string(&b.title);
            let button: Retained<AnyObject> = msg_send![&*a, addButtonWithTitle: &*t];
            if let Some(key) = alert_key_equivalent(b.role) {
                let k = ns_string(key);
                let _: () = msg_send![&*button, setKeyEquivalent: &*k];
            }
            if b.role == sa_core::platform::AlertButtonRole::Destructive {
                let _: () = msg_send![&*button, setHasDestructiveAction: true];
            }
        }
        a
    }
}

/// Shows `alert` (a sheet on `ns_view`'s window when given) and resolves with the pressed button.
pub fn present(alert: Alert, ns_view: Option<*mut std::ffi::c_void>) -> LocalBoxFuture<'static, AlertResponse> {
    let count = alert.buttons.len();
    let window = ns_view.and_then(|v| unsafe { super::appkit::window_of_view(v) });
    autoreleasepool(|_| unsafe {
        let a = build(&alert);
        match window {
            Some(w) => {
                let slot = Rc::new(RefCell::new(Slot::default()));
                let s = slot.clone();
                let handler: RcBlock<dyn Fn(isize)> = RcBlock::new(move |response: isize| {
                    let mut st = s.borrow_mut();
                    st.value = Some(alert_button_index(response, count));
                    st.done = true;
                    if let Some(w) = st.waker.take() {
                        w.wake();
                    }
                });
                let _: () = msg_send![&*a, beginSheetModalForWindow: &*w, completionHandler: &*handler];
                Box::pin(Pending(slot)) as LocalBoxFuture<'static, AlertResponse>
            }
            None => {
                let response: isize = msg_send![&*a, runModal];
                let index = alert_button_index(response, count);
                Box::pin(async move { index }) as LocalBoxFuture<'static, AlertResponse>
            }
        }
    })
}
