//! The bitmap subtitle picture (PGS, VobSub, DVB, XSUB) as the player overlay draws it: each rectangle of the
//! player's [`SubtitlePicture`] as a straight-alpha Slint image with its place on the subtitle canvas, and the rule
//! that puts the canvas over the video ([`place`]), which the overlay (`PictureSubtitleOverlay`,
//! `ui/shared/overlay_label.slint`) asks through `MainBridge.place-picture` for the size it has: it draws over
//! whatever shows the video (the GPU path, the RGBA path, the software renderer) and follows every resize with no
//! frame of lag. The rows are built once per subtitle event, on the UI thread.
//!
//! How the canvas lies over the video ([`Fit`]):
//! * An anamorphic canvas stretches with the video, like the picture itself: SD canvases (DVD 720 × 480 / 576, DVB
//!   720 × 576, anything under 1280 wide), and a canvas that is the video's coded size (including one the player
//!   made up from it).
//! * A square-pixel HD canvas (at least 1280 wide; PGS is 1920 × 1080 or 1280 × 720) keeps its own shape: scaled
//!   uniformly to cover the fitted video rectangle and centred on it, as VLC and mpv do. When the video is cropped
//!   (a 1920 × 800 encode of a 1920 × 1080 Blu-ray), the canvas reaches past the video into the letterbox, where the
//!   disc's captions were meant to be; when the aspects agree this is the same as stretching.
//! * A rectangle that would then fall outside the player surface (the canvas reaches past a window as wide as the
//!   video; a damaged stream places it off the canvas) is moved inside it, and shrunk to fit if it is larger.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use sa_player::SubtitlePicture;

use crate::ui::{MainBridge, PictureBox, PictureSubtitleRect};

/// How a picture's canvas lies over the fitted video rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Each axis scaled to the video rectangle (the canvas is the video picture's own, anamorphic or not).
    Stretch,
    /// One scale for both axes, large enough to cover the video rectangle, centred on it.
    Cover,
}

/// The rule for `canvas` over a video of coded size `video_size` (module docs).
pub fn fit(canvas: (u32, u32), video_size: Option<(u32, u32)>) -> Fit {
    if canvas.0 >= 1280 && Some(canvas) != video_size { Fit::Cover } else { Fit::Stretch }
}

/// Where a rectangle `(x, y, width, height)` of a `canvas` goes on a player surface of `surface` logical pixels
/// showing the video with display aspect `aspect` (fitted and centred, as the video `Image` is), `(x, y, width,
/// height)` in the surface's logical pixels.
pub fn place(rect: (f32, f32, f32, f32), canvas: (f32, f32), fit: Fit, surface: (f32, f32), aspect: f32) -> (f32, f32, f32, f32) {
    let (sw, sh) = (surface.0.max(0.0), surface.1.max(0.0));
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 16.0 / 9.0 };
    let (cw, ch) = (canvas.0.max(1.0), canvas.1.max(1.0));
    let fit_w = sw.min(sh * aspect);
    let fit_h = fit_w / aspect;
    let (fit_x, fit_y) = ((sw - fit_w) / 2.0, (sh - fit_h) / 2.0);
    let (sx, sy, ox, oy) = match fit {
        Fit::Stretch => (fit_w / cw, fit_h / ch, fit_x, fit_y),
        Fit::Cover => {
            let s = (fit_w / cw).max(fit_h / ch);
            (s, s, fit_x + (fit_w - cw * s) / 2.0, fit_y + (fit_h - ch * s) / 2.0)
        }
    };
    let (mut x, mut y, mut w, mut h) = (ox + rect.0 * sx, oy + rect.1 * sy, rect.2 * sx, rect.3 * sy);
    // On the surface: shrunk (both axes alike) when larger, then moved inside.
    if w > sw || h > sh {
        let k = (sw / w.max(f32::MIN_POSITIVE)).min(sh / h.max(f32::MIN_POSITIVE));
        (w, h) = (w * k, h * k);
    }
    x = x.clamp(0.0, (sw - w).max(0.0));
    y = y.clamp(0.0, (sh - h).max(0.0));
    if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
        return (0.0, 0.0, 0.0, 0.0);
    }
    (x, y, w, h)
}

/// `MainBridge.place-picture`: [`place`] for an overlay row on a `width` × `height` surface.
pub fn place_row(r: &PictureSubtitleRect, width: f32, height: f32, aspect: f32) -> PictureBox {
    let fit = if r.cover { Fit::Cover } else { Fit::Stretch };
    let (x, y, width, height) = place((r.x, r.y, r.width, r.height), (r.canvas_width, r.canvas_height), fit, (width, height), aspect);
    PictureBox { x, y, width, height }
}

/// Answers the overlay's `place-picture` calls (every window and preview that shows pictures).
pub fn install(bridge: &MainBridge) {
    bridge.on_place_picture(|r, w, h, a| place_row(&r, w, h, a));
}

/// The overlay's rows for `picture` (one per rectangle; the RGBA bytes copied once into Slint's buffers).
pub fn overlay_rects(picture: &SubtitlePicture) -> Vec<PictureSubtitleRect> {
    let cover = fit(picture.canvas, picture.video_size) == Fit::Cover;
    picture
        .rects
        .iter()
        .filter(|r| r.width > 0 && r.height > 0 && r.rgba.len() >= (r.width * r.height * 4) as usize)
        .map(|r| {
            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&r.rgba[..(r.width * r.height * 4) as usize], r.width, r.height);
            // Straight alpha, as the player hands it over (`from_rgba8`, not `from_rgba8_premultiplied`).
            PictureSubtitleRect {
                image: Image::from_rgba8(buf),
                x: r.x as f32,
                y: r.y as f32,
                width: r.width as f32,
                height: r.height as f32,
                canvas_width: picture.canvas.0 as f32,
                canvas_height: picture.canvas.1 as f32,
                cover,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sa_player::SubtitleBitmap;

    use super::*;

    fn near(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
        [(a.0, b.0), (a.1, b.1), (a.2, b.2), (a.3, b.3)].iter().all(|(x, y)| (x - y).abs() < 0.01)
    }

    macro_rules! assert_near {
        ($a:expr, $b:expr) => {
            let (a, b) = ($a, $b);
            assert!(near(a, b), "{a:?} != {b:?}");
        };
    }

    #[test]
    fn the_canvas_rule_stretches_anamorphic_and_video_sized_canvases_and_covers_with_square_hd_ones() {
        assert_eq!(fit((720, 480), Some((720, 480))), Fit::Stretch, "NTSC DVD");
        assert_eq!(fit((720, 576), Some((720, 576))), Fit::Stretch, "PAL DVD / DVB");
        assert_eq!(fit((720, 576), Some((1920, 1080))), Fit::Stretch, "an SD canvas over HD video");
        assert_eq!(fit((1920, 1080), Some((1920, 1080))), Fit::Stretch, "the video's own size");
        assert_eq!(fit((1920, 1080), Some((1920, 800))), Fit::Cover, "a cropped Blu-ray encode");
        assert_eq!(fit((1920, 1080), Some((1440, 1080))), Fit::Cover);
        assert_eq!(fit((1280, 720), None), Fit::Cover);
        assert_eq!(fit((320, 240), Some((640, 480))), Fit::Stretch);
    }

    #[test]
    fn a_cropped_encode_keeps_its_captions_unsquashed_in_the_letterbox() {
        // 1920 × 800 video in a 1600 × 900 surface: fitted 1600 × 666.7 at y 116.7. The 1920 × 1080 canvas covers
        // it at 1600 × 900, the whole surface: a caption at y 950 of the canvas lands in the bottom bar, with the
        // canvas's own proportions.
        let caption = (480.0, 950.0, 960.0, 80.0);
        let aspect = 1920.0 / 800.0;
        let at = place(caption, (1920.0, 1080.0), Fit::Cover, (1600.0, 900.0), aspect);
        assert_near!(at, (400.0, 791.67, 800.0, 66.67));
        assert!((at.2 / at.3 - 12.0).abs() < 1e-3, "not squashed");
        assert!(at.1 > 116.67 + 666.67, "in the letterbox under the picture");
        // Stretched it would have been squashed by 26 % and pulled up into the picture.
        let squashed = place(caption, (1920.0, 1080.0), Fit::Stretch, (1600.0, 900.0), aspect);
        assert_near!(squashed, (400.0, 116.67 + 950.0 * 666.67 / 1080.0, 800.0, 80.0 * 666.67 / 1080.0));
        assert!(squashed.1 + squashed.3 < 116.67 + 666.67 + 0.01, "inside the picture");
    }

    #[test]
    fn a_square_hd_canvas_over_anamorphic_video_of_the_same_display_aspect_maps_like_the_picture() {
        // 1440 × 1080 coded, shown 16:9: the 1920 × 1080 canvas covers the fitted rectangle exactly.
        let rect = (0.0, 0.0, 1920.0, 1080.0);
        let cover = place(rect, (1920.0, 1080.0), fit((1920, 1080), Some((1440, 1080))), (1280.0, 720.0), 16.0 / 9.0);
        assert_near!(cover, (0.0, 0.0, 1280.0, 720.0));
        assert_near!(place((960.0, 540.0, 96.0, 54.0), (1920.0, 1080.0), Fit::Cover, (1280.0, 800.0), 16.0 / 9.0), (640.0, 400.0, 64.0, 36.0));
        // Shown 4:3 (square pixels) in a 1280 × 720 surface: the 960 × 720 picture at x 160; the canvas keeps 16:9
        // (1280 × 720) and reaches past it into the pillarbox.
        let wide = place(rect, (1920.0, 1080.0), Fit::Cover, (1280.0, 720.0), 4.0 / 3.0);
        assert_near!(wide, (0.0, 0.0, 1280.0, 720.0));
        let sign = place((0.0, 0.0, 192.0, 108.0), (1920.0, 1080.0), Fit::Cover, (1280.0, 720.0), 4.0 / 3.0);
        assert_near!(sign, (0.0, 0.0, 128.0, 72.0));
    }

    #[test]
    fn an_anamorphic_pal_dvd_canvas_stretches_with_the_picture() {
        // 720 × 576 shown 16:9 in a 1920 × 1080 surface: x scaled 1920 / 720, y 1080 / 576.
        let at = place((72.0, 500.0, 576.0, 60.0), (720.0, 576.0), fit((720, 576), Some((720, 576))), (1920.0, 1080.0), 16.0 / 9.0);
        assert_near!(at, (192.0, 937.5, 1536.0, 112.5));
        // Pillarboxed 4:3 in the same surface: the canvas follows the picture rectangle (1440 wide at x 240).
        let at = place((0.0, 0.0, 720.0, 576.0), (720.0, 576.0), Fit::Stretch, (1920.0, 1080.0), 4.0 / 3.0);
        assert_near!(at, (240.0, 0.0, 1440.0, 1080.0));
    }

    #[test]
    fn in_a_tall_narrow_window_the_canvas_covers_the_video_and_rectangles_stay_on_the_surface() {
        // 1920 × 800 video in a 400 × 1000 surface: fitted 400 × 166.7 at y 416.7; the canvas 400 × 225 at y 387.5.
        let at = place((480.0, 950.0, 960.0, 80.0), (1920.0, 1080.0), Fit::Cover, (400.0, 1000.0), 2.4);
        assert_near!(at, (100.0, 387.5 + 950.0 * 400.0 / 1920.0, 200.0, 80.0 * 400.0 / 1920.0));
        // A window exactly as wide as the video: the canvas reaches past the surface's top and bottom; the caption is
        // moved up inside it, at its size.
        let at = place((480.0, 950.0, 960.0, 80.0), (1920.0, 1080.0), Fit::Cover, (2400.0, 1000.0), 2.4);
        assert_near!(at, (600.0, 900.0, 1200.0, 100.0));
    }

    #[test]
    fn rectangles_outside_the_canvas_or_larger_than_the_surface_are_brought_on_screen() {
        // A damaged stream: a rectangle beyond the canvas's right and bottom edges (fractions over 1).
        let at = place((1900.0, 1070.0, 200.0, 50.0), (1920.0, 1080.0), Fit::Stretch, (960.0, 540.0), 16.0 / 9.0);
        assert_near!(at, (860.0, 515.0, 100.0, 25.0));
        // Larger than the surface: shrunk to fit, keeping its shape.
        let at = place((0.0, 0.0, 7680.0, 1080.0), (1920.0, 1080.0), Fit::Stretch, (960.0, 540.0), 16.0 / 9.0);
        assert_near!(at, (0.0, 0.0, 960.0, 135.0));
        // Degenerate input never yields NaN.
        assert_near!(place((0.0, 0.0, 10.0, 10.0), (0.0, 0.0), Fit::Cover, (0.0, 0.0), 0.0), (0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn every_rectangle_becomes_an_image_of_its_size_and_broken_ones_are_left_out() {
        let rect = |x, y, w, h, len| SubtitleBitmap { x, y, width: w, height: h, rgba: Arc::new(vec![200; len]) };
        let picture = SubtitlePicture {
            id: 1,
            stream_index: 3,
            canvas: (720, 480),
            video_size: None,
            start: 1.0,
            end: None,
            rects: vec![rect(72, 400, 360, 48, 360 * 48 * 4), rect(0, 0, 10, 10, 12), rect(36, 24, 72, 24, 72 * 24 * 4)],
        };
        let rows = overlay_rects(&picture);
        assert_eq!(rows.len(), 2, "a rectangle with too few bytes is skipped");
        assert_eq!((rows[0].image.size().width, rows[0].image.size().height), (360, 48));
        assert_eq!((rows[0].x, rows[0].y, rows[0].width, rows[0].height, rows[0].canvas_width, rows[0].canvas_height), (72.0, 400.0, 360.0, 48.0, 720.0, 480.0));
        assert!(!rows[0].cover);
        assert_eq!((rows[1].x, rows[1].y), (36.0, 24.0));
        let b = place_row(&rows[0], 1440.0, 960.0, 1.5);
        assert_eq!((b.x, b.y, b.width, b.height), (144.0, 800.0, 720.0, 96.0));
        let hd = SubtitlePicture { canvas: (1920, 1080), video_size: Some((1920, 800)), ..picture };
        assert!(overlay_rects(&hd)[0].cover);
    }
}
