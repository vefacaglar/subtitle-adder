//! Hub card pictures on macOS (Swift `ThumbnailGenerator.jpegData`, UNIFIED.md §5.8): `AVAssetImageGenerator` at a
//! tenth of the duration (at most 30 s), nearest key frame, preferred track transform applied, at most 384×216 px,
//! encoded as JPEG quality 0.75 through ImageIO. Audio files have no picture. Two at a time. Never called logged out.

use std::ffi::c_void;
use std::path::Path;

use core_foundation::base::{CFTypeRef, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{Encode, Encoding, RefEncode, class, msg_send};
use sa_core::cancel::CancellationToken;
use sa_core::platform::ThumbnailGenerator;

use super::ffi::{CGSize, file_url};
use crate::mac_rules::{is_audio, thumbnail_seconds};

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}
#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGImageRelease(image: *mut CGImage);
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    static kCGImageDestinationLossyCompressionQuality: CFStringRef;
    fn CGImageDestinationCreateWithData(data: *mut c_void, kind: CFStringRef, count: usize, options: CFTypeRef) -> *mut c_void;
    fn CGImageDestinationAddImage(dest: *mut c_void, image: *mut CGImage, properties: CFTypeRef);
    fn CGImageDestinationFinalize(dest: *mut c_void) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreateMutable(allocator: *const c_void, capacity: isize) -> *mut c_void;
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFRelease(cf: *const c_void);
}

pub const MAX_SIZE: CGSize = CGSize { width: 384.0, height: 216.0 };
pub const JPEG_QUALITY: f64 = 0.75;

/// Opaque `CGImage` (encodes as `^{CGImage=}`).
#[repr(C)]
pub struct CGImage {
    _p: [u8; 0],
}
unsafe impl RefEncode for CGImage {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
}

/// CoreMedia `CMTime` (`{?=qiIq}`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CMTime {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}
unsafe impl Encode for CMTime {
    const ENCODING: Encoding = Encoding::Struct("?", &[i64::ENCODING, i32::ENCODING, u32::ENCODING, i64::ENCODING]);
}
unsafe impl RefEncode for CMTime {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

const K_CMTIME_FLAGS_VALID: u32 = 1;
const K_CMTIME_FLAGS_POSITIVE_INFINITY: u32 = 4;

impl CMTime {
    pub const POSITIVE_INFINITY: CMTime = CMTime { value: 0, timescale: 0, flags: K_CMTIME_FLAGS_VALID | K_CMTIME_FLAGS_POSITIVE_INFINITY, epoch: 0 };

    /// `CMTime(seconds:preferredTimescale: 600)`.
    pub fn from_seconds(s: f64) -> CMTime {
        CMTime { value: (s * 600.0).round() as i64, timescale: 600, flags: K_CMTIME_FLAGS_VALID, epoch: 0 }
    }

    pub fn seconds(&self) -> f64 {
        if self.flags & K_CMTIME_FLAGS_VALID == 0 || self.timescale <= 0 { f64::NAN } else { self.value as f64 / self.timescale as f64 }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AssetThumbnailGenerator;

impl AssetThumbnailGenerator {
    pub fn new() -> Self {
        Self
    }
}

impl ThumbnailGenerator for AssetThumbnailGenerator {
    fn generate(&self, path: &Path, cancel: &CancellationToken) -> Option<Vec<u8>> {
        if cancel.is_cancelled() || is_audio(path) || !path.is_file() {
            return None;
        }
        let bytes = autoreleasepool(|_| unsafe { jpeg_data(path) });
        if cancel.is_cancelled() { None } else { bytes }
    }

    fn max_concurrency(&self) -> usize {
        2
    }
}

unsafe fn jpeg_data(path: &Path) -> Option<Vec<u8>> {
    unsafe {
        let url = file_url(path);
        let none: Option<&AnyObject> = None;
        let asset: Option<Retained<AnyObject>> = msg_send![class!(AVURLAsset), URLAssetWithURL: &*url, options: none];
        let asset = asset?;
        let duration: CMTime = msg_send![&*asset, duration];
        let at = CMTime::from_seconds(thumbnail_seconds(duration.seconds()));
        let generator: Option<Retained<AnyObject>> = msg_send![class!(AVAssetImageGenerator), assetImageGeneratorWithAsset: &*asset];
        let generator = generator?;
        let _: () = msg_send![&*generator, setAppliesPreferredTrackTransform: true];
        let _: () = msg_send![&*generator, setMaximumSize: MAX_SIZE];
        // The nearest key frame is good enough for a card, and much faster than an exact frame.
        let _: () = msg_send![&*generator, setRequestedTimeToleranceBefore: CMTime::POSITIVE_INFINITY];
        let _: () = msg_send![&*generator, setRequestedTimeToleranceAfter: CMTime::POSITIVE_INFINITY];
        let image: *mut CGImage = msg_send![
            &*generator,
            copyCGImageAtTime: at,
            actualTime: std::ptr::null_mut::<CMTime>(),
            error: std::ptr::null_mut::<*mut AnyObject>()
        ];
        if image.is_null() {
            return None;
        }
        let out = encode_jpeg(image, JPEG_QUALITY);
        CGImageRelease(image);
        out
    }
}

unsafe fn encode_jpeg(image: *mut CGImage, quality: f64) -> Option<Vec<u8>> {
    unsafe {
        let data = CFDataCreateMutable(std::ptr::null(), 0);
        if data.is_null() {
            return None;
        }
        let kind = CFString::new("public.jpeg");
        let dest = CGImageDestinationCreateWithData(data, kind.as_concrete_TypeRef(), 1, std::ptr::null());
        let mut out = None;
        if !dest.is_null() {
            let key = CFString::wrap_under_get_rule(kCGImageDestinationLossyCompressionQuality);
            let props = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFNumber::from(quality).as_CFType())]);
            CGImageDestinationAddImage(dest, image, props.as_CFTypeRef());
            if CGImageDestinationFinalize(dest) {
                let len = CFDataGetLength(data);
                let ptr = CFDataGetBytePtr(data);
                if len > 0 && !ptr.is_null() {
                    out = Some(std::slice::from_raw_parts(ptr, len as usize).to_vec());
                }
            }
            CFRelease(dest);
        }
        CFRelease(data);
        out
    }
}
