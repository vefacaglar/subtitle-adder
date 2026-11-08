//! `Symbols.image(name, size)` for the mac tree (mac-ui §14.5). On macOS the platform agent replaces the
//! placeholder with SF Symbols rasterized through AppKit (`sa-platform::mac`), cached per (name, size, scale).
//! Elsewhere (cross-platform previews) a neutral placeholder glyph keeps the layout visible.

use std::cell::RefCell;
use std::collections::HashMap;

use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};

use crate::ui::Symbols;

thread_local! {
    static CACHE: RefCell<HashMap<(String, u32), Image>> = RefCell::new(HashMap::new());
}

/// Installs the lookup on one window's `Symbols` global.
pub fn install<C: ComponentHandle>(component: &C)
where
    for<'a> Symbols<'a>: slint::Global<'a, C>,
{
    component.global::<Symbols>().on_image(|name, size| image(name.as_str(), size));
}

fn image(name: &str, size: f32) -> Image {
    let px = (size * 2.0).round().max(2.0) as u32;
    CACHE.with(|c| {
        c.borrow_mut().entry((name.to_owned(), px)).or_insert_with(|| placeholder(px)).clone()
    })
}

/// A ring (white; tinted by `colorize` in Slint).
fn placeholder(px: u32) -> Image {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(px, px);
    let c = (px as f32 - 1.0) / 2.0;
    let (outer, inner) = (c * 0.82, c * 0.82 - (px as f32 * 0.12).max(1.5));
    for (i, p) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((i as u32 % px) as f32 - c, (i as u32 / px) as f32 - c);
        let d = (x * x + y * y).sqrt();
        let a = if d <= outer && d >= inner { 255 } else { 0 };
        *p = Rgba8Pixel { r: 255, g: 255, b: 255, a };
    }
    Image::from_rgba8(buf)
}
