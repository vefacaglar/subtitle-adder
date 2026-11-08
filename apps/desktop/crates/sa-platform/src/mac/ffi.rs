//! Small Objective-C / Core Foundation helpers on top of the `objc2` runtime (no framework binding crates, so the
//! module type-checks for aarch64 on the Windows dev machine; see `ARCHITECTURE.md` §11).

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{Encode, Encoding, RefEncode, class, msg_send};

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// `CGFloat` on 64-bit Apple platforms.
pub type CGFloat = f64;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGPoint {
    pub x: CGFloat,
    pub y: CGFloat,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGSize {
    pub width: CGFloat,
    pub height: CGFloat,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[CGFloat::ENCODING, CGFloat::ENCODING]);
}
unsafe impl RefEncode for CGPoint {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}
unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[CGFloat::ENCODING, CGFloat::ENCODING]);
}
unsafe impl RefEncode for CGSize {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}
unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}
unsafe impl RefEncode for CGRect {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

/// An `NSString` (toll-free bridged from a `CFString`).
pub fn ns_string(s: &str) -> Retained<AnyObject> {
    let cf = CFString::new(s);
    let ptr = cf.as_concrete_TypeRef() as *mut AnyObject;
    // SAFETY: CFStringRef and NSString* are toll-free bridged; retain gives us our own +1 reference.
    unsafe { Retained::retain(ptr).expect("CFString is never null") }
}

/// Rust text of an `NSString` (`UTF8String`).
pub fn rust_string(obj: &AnyObject) -> String {
    unsafe {
        let p: *const c_char = msg_send![obj, UTF8String];
        if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

/// `+[NSURL fileURLWithPath:]`.
pub fn file_url(path: &Path) -> Retained<AnyObject> {
    let s = ns_string(&path.to_string_lossy());
    unsafe { msg_send![class!(NSURL), fileURLWithPath: &*s] }
}

/// `-[NSURL path]`.
pub fn url_path(url: &AnyObject) -> Option<PathBuf> {
    unsafe {
        let p: Option<Retained<AnyObject>> = msg_send![url, path];
        p.map(|p| PathBuf::from(rust_string(&p)))
    }
}

/// `+[NSArray arrayWithObjects:count:]`.
pub fn ns_array(items: &[&AnyObject]) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSArray), arrayWithObjects: items.as_ptr(), count: items.len()] }
}

/// Elements of an `NSArray`.
pub fn array_items(array: &AnyObject) -> Vec<Retained<AnyObject>> {
    unsafe {
        let n: usize = msg_send![array, count];
        (0..n).filter_map(|i| -> Option<Retained<AnyObject>> { msg_send![array, objectAtIndex: i] }).collect()
    }
}

/// `NSApplication.sharedApplication` (creates it when needed; main thread only).
pub fn shared_application() -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSApplication), sharedApplication] }
}

/// Whether the object responds to a selector.
pub fn responds(obj: &AnyObject, sel: objc2::runtime::Sel) -> bool {
    unsafe { msg_send![obj, respondsToSelector: sel] }
}

/// A raw CF reference as an object pointer (toll-free bridging).
pub fn cf_as_object(r: *const c_void) -> *mut AnyObject {
    r as *mut AnyObject
}
