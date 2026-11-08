//! SF Symbols rasterizer (mac-ui §1, §14.5): `NSImage(systemSymbolName:)` with a point-size configuration, drawn
//! into an RGBA bitmap at `point size × scale`. The symbol is a template (black + alpha), so Slint tints it with
//! `colorize`. Results are cached per (name, size, weight, scale). Main thread (AppKit drawing).

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use parking_lot::Mutex;

use super::ffi::{CGFloat, CGPoint, CGRect, CGSize, ns_string};
use crate::mac_rules::pixel_extent;

/// `NSFontWeight` values.
pub const WEIGHT_REGULAR: f64 = 0.0;
pub const WEIGHT_MEDIUM: f64 = 0.23;
pub const WEIGHT_SEMIBOLD: f64 = 0.3;
/// `NSImageSymbolScaleMedium`.
const SCALE_MEDIUM: isize = 2;
const NS_COMPOSITING_OPERATION_SOURCE_OVER: usize = 2;

/// A rasterized symbol: straight (unpremultiplied) RGBA8, `width × height` pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolBitmap {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

type Key = (String, u32, u32, u32);

fn cache() -> &'static Mutex<HashMap<Key, Option<Arc<SymbolBitmap>>>> {
    static C: OnceLock<Mutex<HashMap<Key, Option<Arc<SymbolBitmap>>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The symbol `name` at `point_size` pt (regular weight), `scale` pixels per point; `None` for an unknown name.
pub fn rasterize(name: &str, point_size: f64, scale: f64) -> Option<Arc<SymbolBitmap>> {
    rasterize_weighted(name, point_size, WEIGHT_REGULAR, scale)
}

pub fn rasterize_weighted(name: &str, point_size: f64, weight: f64, scale: f64) -> Option<Arc<SymbolBitmap>> {
    let key = (name.to_owned(), (point_size * 100.0) as u32, ((weight + 1.0) * 100.0) as u32, (scale * 100.0) as u32);
    if let Some(hit) = cache().lock().get(&key) {
        return hit.clone();
    }
    let made = autoreleasepool(|_| unsafe { draw(name, point_size, weight, scale) }).map(Arc::new);
    cache().lock().insert(key, made.clone());
    made
}

unsafe fn draw(name: &str, point_size: f64, weight: f64, scale: f64) -> Option<SymbolBitmap> {
    unsafe {
        let n = ns_string(name);
        let none: Option<&AnyObject> = None;
        let image: Option<Retained<AnyObject>> =
            msg_send![class!(NSImage), imageWithSystemSymbolName: &*n, accessibilityDescription: none];
        let image = image?;
        let config: Retained<AnyObject> = msg_send![
            class!(NSImageSymbolConfiguration),
            configurationWithPointSize: point_size as CGFloat,
            weight: weight as CGFloat,
            scale: SCALE_MEDIUM
        ];
        let sized: Option<Retained<AnyObject>> = msg_send![&*image, imageWithSymbolConfiguration: &*config];
        let image = sized.unwrap_or(image);
        let size: CGSize = msg_send![&*image, size];
        if !(size.width > 0.0 && size.height > 0.0) {
            return None;
        }
        let (w, h) = (pixel_extent(size.width, scale), pixel_extent(size.height, scale));
        let device_rgb = ns_string("NSDeviceRGBColorSpace");
        let planes: *mut *mut u8 = std::ptr::null_mut();
        let rep: Allocated<AnyObject> = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: Option<Retained<AnyObject>> = msg_send![
            rep,
            initWithBitmapDataPlanes: planes,
            pixelsWide: w as isize,
            pixelsHigh: h as isize,
            bitsPerSample: 8isize,
            samplesPerPixel: 4isize,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: &*device_rgb,
            bytesPerRow: (w * 4) as isize,
            bitsPerPixel: 32isize
        ];
        let rep = rep?;
        // Drawing in points maps onto the pixel grid.
        let _: () = msg_send![&*rep, setSize: CGSize { width: w as f64 / scale, height: h as f64 / scale }];
        let ctx: Option<Retained<AnyObject>> = msg_send![class!(NSGraphicsContext), graphicsContextWithBitmapImageRep: &*rep];
        let ctx = ctx?;
        let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
        let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: &*ctx];
        let target = CGRect {
            origin: CGPoint { x: (w as f64 / scale - size.width) / 2.0, y: (h as f64 / scale - size.height) / 2.0 },
            size,
        };
        let _: () = msg_send![
            &*image,
            drawInRect: target,
            fromRect: CGRect::default(),
            operation: NS_COMPOSITING_OPERATION_SOURCE_OVER,
            fraction: 1.0 as CGFloat
        ];
        let _: () = msg_send![&*ctx, flushGraphics];
        let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];
        let data: *const u8 = msg_send![&*rep, bitmapData];
        let row: isize = msg_send![&*rep, bytesPerRow];
        if data.is_null() || row < (w * 4) as isize {
            return None;
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let line = std::slice::from_raw_parts(data.add(y * row as usize), w * 4);
            for p in line.chunks_exact(4) {
                let a = p[3] as u32;
                let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
                rgba.extend_from_slice(&[un(p[0]), un(p[1]), un(p[2]), p[3]]);
            }
        }
        Some(SymbolBitmap { width: w, height: h, rgba })
    }
}
