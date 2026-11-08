//! `Symbols.image(name, size)` / `Symbols.tinted-image(name, size, color)` for the mac tree (mac-ui §1, §14.5;
//! ARCHITECTURE.md §6.6).
//!
//! * macOS: the SF Symbol itself, rasterized through AppKit (`NSImage(systemSymbolName:)` with a point-size
//!   configuration, `sa_platform::mac::symbols`) at `size × scale` pixels, where the scale is the window's backing
//!   scale (at least 2, so an image built before the window reaches a Retina screen is never blurry).
//! * Every other target, and macOS when AppKit has no symbol of that name: the bundled vector stand-in ([`svg`]),
//!   drawn to resemble the SF Symbol and rendered by Slint at whatever size it is shown.
//! * A name neither knows: a neutral ring, so the layout stays visible.
//!
//! `image` returns templates (only the alpha counts): `Sym` tints them with `colorize` from the theme, so light and
//! dark mode (and accent / secondary tints) follow the palette exactly like template SF Symbols in SwiftUI.
//! `tinted-image` bakes a color into a bitmap for consumers that cannot colorize (the NSMenu item icons the `MenuBar`
//! hands to muda; the mac tree passes `MacTheme.menu-symbol`, which follows light/dark; on macOS
//! `sa_platform::mac::appkit::keep_menu_icons_template` then makes them templates). Both are cached per
//! (name, point size, scale[, color]) on the UI thread.

pub mod svg;

use std::cell::RefCell;
use std::collections::HashMap;

use slint::{Color, ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};

use crate::ui::Symbols;

/// Where an image came from (tests, diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolSource {
    /// AppKit's SF Symbol (macOS only).
    SfSymbol,
    /// The bundled SVG stand-in.
    Vector,
    /// Neither knows the name.
    Placeholder,
}

/// (name, point size × 100, scale × 100).
type Key = (String, u32, u32);

thread_local! {
    static CACHE: RefCell<HashMap<Key, (Image, SymbolSource)>> = RefCell::new(HashMap::new());
    static TINTED: RefCell<HashMap<(Key, u32), Image>> = RefCell::new(HashMap::new());
}

/// Installs the lookups on one window's `Symbols` global.
pub fn install<C: ComponentHandle + 'static>(component: &C)
where
    for<'a> Symbols<'a>: slint::Global<'a, C>,
{
    let symbols = component.global::<Symbols>();
    let weak = component.as_weak();
    let scale_of = move || weak.upgrade().map(|c| c.window().scale_factor()).unwrap_or(2.0);
    let scale = scale_of.clone();
    symbols.on_image(move |name, size| lookup(name.as_str(), size, scale()).0);
    let scale = scale_of.clone();
    symbols.on_image_width(move |name, size| extent(name.as_str(), size, scale()).0);
    let scale = scale_of.clone();
    symbols.on_image_height(move |name, size| extent(name.as_str(), size, scale()).1);
    symbols.on_tinted_image(move |name, size, color| tinted(name.as_str(), size, scale_of(), color));
}

fn key(name: &str, point_size: f32, scale: f32) -> (Key, f32, f32) {
    let point_size = if point_size.is_finite() && point_size > 0.0 { point_size } else { 16.0 };
    let scale = if scale.is_finite() { scale.max(2.0) } else { 2.0 };
    ((name.to_owned(), (point_size * 100.0).round() as u32, (scale * 100.0).round() as u32), point_size, scale)
}

/// The template image for `name` at `point_size` pt on a screen with `scale` pixels per point, and its source.
pub fn lookup(name: &str, point_size: f32, scale: f32) -> (Image, SymbolSource) {
    let (key, point_size, scale) = key(name, point_size, scale);
    CACHE.with(|c| c.borrow_mut().entry(key).or_insert_with(|| make(name, point_size, scale)).clone())
}

/// The logical size (width, height) [`lookup`]'s image is shown at: an SF Symbol's own bounds at `point_size`
/// (its bitmap is drawn at that size × the scale), a `point_size` square for the stand-ins and the ring.
pub fn extent(name: &str, point_size: f32, scale: f32) -> (f32, f32) {
    let (_, size, px_per_pt) = key(name, point_size, scale);
    match lookup(name, point_size, scale) {
        (image, SymbolSource::SfSymbol) => {
            let px = image.size();
            (px.width as f32 / px_per_pt, px.height as f32 / px_per_pt)
        }
        _ => (size, size),
    }
}

/// `name` as an RGBA bitmap of `point_size × scale` pixels (height; SF Symbols keep their aspect) drawn in `color`.
pub fn tinted(name: &str, point_size: f32, scale: f32, color: Color) -> Image {
    let (key, point_size, scale) = key(name, point_size, scale);
    let cache_key = (key, color.as_argb_encoded());
    if let Some(hit) = TINTED.with(|c| c.borrow().get(&cache_key).cloned()) {
        return hit;
    }
    let px = (point_size * scale).round().max(2.0) as u32;
    let mask = match lookup(name, point_size, scale) {
        // An SVG renders at its document size: draw the stand-in once more at the pixel size asked for.
        (_, SymbolSource::Vector) => svg::document_sized(name, px)
            .and_then(|doc| Image::load_from_svg_data(doc.as_bytes()).ok())
            .and_then(|image| image.to_rgba8()),
        (image, _) => image.to_rgba8(),
    };
    let image = mask.map(|m| Image::from_rgba8(tint(&m, color))).unwrap_or_default();
    TINTED.with(|c| c.borrow_mut().insert(cache_key, image.clone()));
    image
}

/// The alpha of `mask` (straight RGBA) in `color`.
fn tint(mask: &SharedPixelBuffer<Rgba8Pixel>, color: Color) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut out = SharedPixelBuffer::<Rgba8Pixel>::new(mask.width(), mask.height());
    let (r, g, b, alpha) = (color.red(), color.green(), color.blue(), color.alpha() as u32);
    for (dst, src) in out.make_mut_slice().iter_mut().zip(mask.as_slice()) {
        let a = ((src.a as u32 * alpha + 127) / 255) as u8;
        *dst = if a == 0 { Rgba8Pixel { r: 0, g: 0, b: 0, a: 0 } } else { Rgba8Pixel { r, g, b, a } };
    }
    out
}

fn make(name: &str, point_size: f32, scale: f32) -> (Image, SymbolSource) {
    #[cfg(target_os = "macos")]
    if let Some(bitmap) = sa_platform::mac::symbols::rasterize(name, point_size as f64, scale as f64) {
        let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&bitmap.rgba, bitmap.width as u32, bitmap.height as u32);
        return (Image::from_rgba8(buffer), SymbolSource::SfSymbol);
    }
    if let Some(image) = svg::document(name).and_then(|doc| Image::load_from_svg_data(doc.as_bytes()).ok()) {
        return (image, SymbolSource::Vector);
    }
    (placeholder((point_size * scale).round().max(2.0) as u32), SymbolSource::Placeholder)
}

/// A ring (white; only the alpha counts).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vector_set_is_sorted_unique_and_every_entry_loads_as_a_24pt_template() {
        for pair in svg::SYMBOLS.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} must sort before {}", pair[0].0, pair[1].0);
        }
        for (name, _) in svg::SYMBOLS {
            let doc = svg::document(name).unwrap();
            let image = Image::load_from_svg_data(doc.as_bytes()).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let size = image.size();
            assert_eq!((size.width, size.height), (24, 24), "{name}");
        }
    }

    #[test]
    fn known_names_come_from_the_vector_set_off_macos_and_unknown_ones_fall_back_to_the_ring() {
        let (_, source) = lookup("film", 16.0, 2.0);
        if cfg!(target_os = "macos") {
            assert_eq!(source, SymbolSource::SfSymbol);
        } else {
            assert_eq!(source, SymbolSource::Vector);
        }
        let (ring, source) = lookup("no.such.symbol", 16.0, 2.0);
        assert_eq!(source, SymbolSource::Placeholder);
        let size = ring.size();
        assert_eq!((size.width, size.height), (32, 32));
        // Cached per (name, size, scale): the same request returns the same image.
        assert!(lookup("film", 16.0, 2.0).0 == lookup("film", 16.0, 2.0).0);
    }

    #[test]
    fn tinted_images_are_bitmaps_of_the_requested_pixel_size_in_the_requested_color() {
        let red = Color::from_argb_u8(0xd9, 0xff, 0x00, 0x00);
        for name in ["film", "captions.bubble", "stop.fill", "sidebar.left", "no.such.symbol"] {
            let image = tinted(name, 16.0, 2.0, red);
            let pixels = image.to_rgba8().unwrap_or_else(|| panic!("{name}: a bitmap"));
            if !cfg!(target_os = "macos") {
                // SF Symbols keep their own aspect ratio; the stand-ins and the ring are square.
                assert_eq!((pixels.width(), pixels.height()), (32, 32), "{name}");
            }
            let ink: Vec<_> = pixels.as_slice().iter().filter(|p| p.a > 0).collect();
            assert!(ink.len() > 20, "{name}: something is drawn");
            assert!(ink.iter().all(|p| (p.r, p.g, p.b) == (0xff, 0, 0) && p.a <= 0xd9), "{name}: only the tint");
            assert!(ink.iter().any(|p| p.a == 0xd9), "{name}: opaque strokes reach the tint's alpha");
        }
        // Light and dark tints are separate cache entries; the same request is the same image.
        let white = Color::from_argb_u8(0xd9, 0xff, 0xff, 0xff);
        assert!(tinted("film", 16.0, 2.0, red) == tinted("film", 16.0, 2.0, red));
        assert!(tinted("film", 16.0, 2.0, red) != tinted("film", 16.0, 2.0, white));
    }

    #[test]
    fn symbols_are_shown_at_their_own_bounds() {
        for name in ["film", "captions.bubble", "no.such.symbol"] {
            let (w, h) = extent(name, 16.0, 2.0);
            match lookup(name, 16.0, 2.0) {
                (image, SymbolSource::SfSymbol) => {
                    let px = image.size();
                    assert_eq!((w, h), (px.width as f32 / 2.0, px.height as f32 / 2.0), "{name}");
                }
                _ => assert_eq!((w, h), (16.0, 16.0), "{name}"),
            }
        }
        // Below 2× the bitmaps are still drawn at 2×: the shown size stays in points.
        let (w, h) = extent("film", 16.0, 1.0);
        assert_eq!((w, h), extent("film", 16.0, 2.0));
        // A broken size falls back to 16 pt like the image itself.
        assert_eq!(extent("no.such.symbol", f32::NAN, 2.0), (16.0, 16.0));
    }

    /// Every SF Symbol name the mac tree, the Rust glue and the SwiftUI views use has a vector stand-in.
    #[test]
    fn every_symbol_the_app_uses_has_a_vector_stand_in() {
        let mut used = std::collections::BTreeSet::new();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        collect(&root.join("ui/mac"), "slint", &mut used);
        collect(&root.join("src"), "rs", &mut used);
        let swift = root.join("../../../macos/Sources");
        if swift.is_dir() {
            collect(&swift, "swift", &mut used);
        }
        let missing: Vec<_> = used.iter().filter(|n| svg::body(n).is_none()).collect();
        assert!(missing.is_empty(), "no vector stand-in for {missing:?}");
        assert!(used.len() >= 70, "the scan found the symbols ({})", used.len());
    }

    /// Names in symbol positions: `Symbols.image("…"`, `symbol: "…"` / `name: "…"` in Slint, `.symbol("…")` and
    /// `symbol: "…".into()` in Rust, `systemImage: "…"` / `systemName: "…"` / `"…"` returned by `symbolName` /
    /// `buttonSymbol` / empty-state tuples in Swift (string literals that are SF Symbol names).
    fn collect(dir: &std::path::Path, ext: &str, out: &mut std::collections::BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, ext, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some(ext) && !path.ends_with("symbols/svg.rs") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for literal in string_literals(&text) {
                    if looks_like_symbol(&literal, ext, &text) {
                        out.insert(literal);
                    }
                }
            }
        }
    }

    fn string_literals(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find('"') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('"') else { break };
            out.push(after[..end].to_owned());
            rest = &after[end + 1..];
        }
        out
    }

    /// The SF Symbol names in this repository's sources: lowercase dotted words from the SF vocabulary, or one of
    /// the single-word names used. File names, domains and catalog keys never match the vocabulary.
    fn looks_like_symbol(s: &str, ext: &str, text: &str) -> bool {
        const SINGLE: &[&str] = &[
            "checkmark", "circle", "clock", "cloud", "eye", "film", "folder", "gearshape", "globe", "heart", "house",
            "lock", "magnifyingglass", "minus", "network", "paintbrush", "pencil", "plus", "scissors", "scope", "star",
            "translate", "trash", "waveform", "xmark", "externaldrive",
        ];
        const HEADS: &[&str] = &[
            "arrow", "captions", "character", "checkmark", "chevron", "circle", "clock", "doc", "exclamationmark",
            "externaldrive", "eye", "film", "folder", "heart", "info", "list", "menubar", "pause", "person", "play",
            "questionmark", "rectangle", "sidebar", "speaker", "square", "star", "stop", "text", "wand", "xmark",
        ];
        /// File names (`sidebar.slint` imports, fixtures) share the vocabulary's heads.
        const FILE_EXTENSIONS: &[&str] = &["json", "log", "mp4", "rs", "slint", "srt", "swift", "txt", "vtt", "wav"];
        if SINGLE.contains(&s) {
            // Single words are common identifiers too: only where the file names symbols at all.
            return match ext {
                "swift" => text.contains(&format!("systemImage: \"{s}\""))
                    || text.contains(&format!("systemName: \"{s}\""))
                    || text.contains(&format!("\"{s}\")"))
                    || text.contains(&format!(": \"{s}\"\n"))
                    || text.contains(&format!("\"{s}\" :")),
                _ => text.contains(&format!("name: \"{s}\""))
                    || text.contains(&format!("symbol: \"{s}\""))
                    || text.contains(&format!("Symbols.image(\"{s}\""))
                    || text.contains(&format!("symbol(\"{s}\")"))
                    || text.contains(&format!("symbol: \"{s}\".into()"))
                    // Match arms of the Rust symbol tables (`quick_symbol`, `empty_symbol`, …).
                    || (ext == "rs" && text.contains(&format!("=> \"{s}\",")) && text.contains("_symbol(")),
            };
        }
        let mut parts = s.split('.');
        let Some(head) = parts.next() else { return false };
        HEADS.contains(&head)
            && s.contains('.')
            && s.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
            && !s.rsplit('.').next().is_some_and(|ext| FILE_EXTENSIONS.contains(&ext))
    }
}
