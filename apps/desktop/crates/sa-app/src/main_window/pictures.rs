//! The bitmap subtitle picture (PGS, VobSub, DVB, XSUB) as the player overlay draws it: each rectangle of the
//! player's [`SubtitlePicture`] as a straight-alpha Slint image with its place on the subtitle canvas, and the rule
//! that puts the canvas over the video ([`place`]), which the overlay (`PictureSubtitleOverlay`,
//! `ui/shared/overlay_label.slint`) asks through `MainBridge.place-picture` for the size it has: it draws over
//! whatever shows the video (the GPU path, the RGBA path, the software renderer) and follows every resize with no
//! frame of lag. The rows are built once per subtitle event, on the UI thread.
//!
//! How the canvas lies over the video ([`fit`], decided by the canvas's and the video's shapes):
//! * It stretches with the video, like the picture itself, when it is the picture's own raster: the video's coded
//!   size (including a canvas the player made up from it), or an SD raster (at most 720 wide and 480 or 576 high:
//!   DVD 720 / 704 / 352 × 480 / 576, DVB 720 × 576) over any video it is not known to be a crop of (HD video, a
//!   resized 640 × 480 or 544 × 576 rip, a 352-wide half-D1 encode, 704-wide broadcast video under a 720-wide
//!   canvas): nothing tells how the SD frame was scaled into the video's, so it follows the picture, as VLC and mpv
//!   do.
//! * The one case where the scale is certain: an SD raster over video of the canvas's own width (or 704 under
//!   720) and fewer lines, a vertical crop of the same raster (a 720 × 432 DVD rip of a 720 × 576 frame). The canvas
//!   lies at the video's pixel scale, each axis apart (the pixels are anamorphic), centred on the video, so its
//!   captions stay unsquashed where the disc put them, in the letterbox.
//! * A landscape canvas over a video turned a quarter round (its display matrix says so; the player hands its size
//!   over turned, `SubtitlePicture::video_turned`) never stretches; it covers, as below. Video merely coded
//!   narrower than tall (544 × 576) is not turned.
//! * Every other canvas keeps its own shape (PGS 1920 × 1080 or 1280 × 720, a 1024 × 576 or 960 × 540 DVB
//!   display, a UHD canvas over HD video, any landscape canvas over a quarter-turned video): scaled uniformly to cover
//!   the fitted video rectangle and centred on it, as VLC and mpv do. When the video is cropped (a 1920 × 800 encode
//!   of a 1920 × 1080 Blu-ray), the canvas reaches past the video into the letterbox, where the disc's captions were
//!   meant to be; when the aspects agree this is the same as stretching.
//! * An event that would then fall outside the player surface (the canvas reaches past a window as wide as the
//!   video; a damaged stream places it off the canvas) is brought inside as a whole: all its rectangles move by one
//!   offset, and when the event's bounding box is larger than the surface they shrink by one factor first, so lines
//!   that were apart stay apart and in their order (a sign at the top and a caption at the bottom of a canvas taller
//!   than the surface both stay visible, the whole event scaled down).

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
    /// The scale of the video's own pixels (`width` × `height`, the coded size), each axis apart, centred on it.
    VideoPixels { width: u32, height: u32 },
}

/// The rule for `canvas` over a video of coded size `video_size`, given in the orientation it is shown (the player
/// swaps width and height for a video turned a quarter round and says so, `turned`, so a canvas of the coded size
/// then covers; a video coded narrower than tall, a 544 × 576 broadcast encode, is not turned): module docs.
pub fn fit(canvas: (u32, u32), video_size: Option<(u32, u32)>, turned: bool) -> Fit {
    let sd_raster = canvas.0 <= 720 && matches!(canvas.1, 480 | 576);
    let turned = turned && canvas.0 > canvas.1 && video_size.is_some_and(|(w, h)| h > w);
    match video_size {
        _ if turned => Fit::Cover,
        Some(v) if v == canvas => Fit::Stretch,
        // A vertical crop of the canvas's own raster: the only SD case whose pixel scale is known.
        Some((width, height)) if sd_raster && (width == canvas.0 || (width == 704 && canvas.0 == 720)) && height > 0 && height < canvas.1 => {
            Fit::VideoPixels { width, height }
        }
        _ if sd_raster => Fit::Stretch,
        _ => Fit::Cover,
    }
}

/// Where a rectangle `(x, y, width, height)` of a `canvas` goes on a player surface of `surface` logical pixels
/// showing the video with display aspect `aspect` (fitted and centred, as the video `Image` is), `(x, y, width,
/// height)` in the surface's logical pixels. `group` is the bounding box of every rectangle of the event (canvas
/// pixels): the event is brought onto the surface as a whole (module docs).
pub fn place(rect: (f32, f32, f32, f32), group: (f32, f32, f32, f32), canvas: (f32, f32), fit: Fit, surface: (f32, f32), aspect: f32) -> (f32, f32, f32, f32) {
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
        Fit::VideoPixels { width, height } => {
            let (sx, sy) = (fit_w / (width.max(1) as f32), fit_h / (height.max(1) as f32));
            (sx, sy, fit_x + (fit_w - cw * sx) / 2.0, fit_y + (fit_h - ch * sy) / 2.0)
        }
    };
    let map = |r: (f32, f32, f32, f32)| (ox + r.0 * sx, oy + r.1 * sy, r.2 * sx, r.3 * sy);
    let (gx, gy, gw, gh) = map(group);
    let (x, y, w, h) = map(rect);
    // The event on the surface: shrunk (both axes alike, about its centre) when larger, then moved inside.
    let k = if gw > sw || gh > sh { (sw / gw.max(f32::MIN_POSITIVE)).min(sh / gh.max(f32::MIN_POSITIVE)) } else { 1.0 };
    let (nw, nh) = (gw * k, gh * k);
    let nx = (gx + (gw - nw) / 2.0).clamp(0.0, (sw - nw).max(0.0));
    let ny = (gy + (gh - nh) / 2.0).clamp(0.0, (sh - nh).max(0.0));
    let out = (nx + (x - gx) * k, ny + (y - gy) * k, w * k, h * k);
    if ![out.0, out.1, out.2, out.3].iter().all(|v| v.is_finite()) {
        return (0.0, 0.0, 0.0, 0.0);
    }
    out
}

/// `MainBridge.place-picture`: [`place`] for an overlay row on a `width` × `height` surface.
pub fn place_row(r: &PictureSubtitleRect, width: f32, height: f32, aspect: f32) -> PictureBox {
    let fit = if r.pixel_width > 0.0 && r.pixel_height > 0.0 {
        Fit::VideoPixels { width: r.pixel_width as u32, height: r.pixel_height as u32 }
    } else if r.cover {
        Fit::Cover
    } else {
        Fit::Stretch
    };
    let group = (r.group_x, r.group_y, r.group_width, r.group_height);
    let (x, y, width, height) = place((r.x, r.y, r.width, r.height), group, (r.canvas_width, r.canvas_height), fit, (width, height), aspect);
    PictureBox { x, y, width, height }
}

/// Answers the overlay's `place-picture` calls (every window and preview that shows pictures).
pub fn install(bridge: &MainBridge) {
    bridge.on_place_picture(|r, w, h, a| place_row(&r, w, h, a));
}

/// The overlay's rows for `picture` (one per rectangle; the RGBA bytes copied once into Slint's buffers).
pub fn overlay_rects(picture: &SubtitlePicture) -> Vec<PictureSubtitleRect> {
    let rule = fit(picture.canvas, picture.video_size, picture.video_turned);
    let cover = rule == Fit::Cover;
    let (pixel_width, pixel_height) = match rule {
        Fit::VideoPixels { width, height } => (width as f32, height as f32),
        _ => (0.0, 0.0),
    };
    let usable: Vec<_> = picture.rects.iter().filter(|r| r.width > 0 && r.height > 0 && r.rgba.len() >= (r.width * r.height * 4) as usize).collect();
    // The bounding box of what is drawn (canvas pixels).
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for r in &usable {
        x0 = x0.min(r.x as f32);
        y0 = y0.min(r.y as f32);
        x1 = x1.max(r.x as f32 + r.width as f32);
        y1 = y1.max(r.y as f32 + r.height as f32);
    }
    usable
        .into_iter()
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
                pixel_width,
                pixel_height,
                group_x: x0,
                group_y: y0,
                group_width: x1 - x0,
                group_height: y1 - y0,
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
    fn the_canvas_rule_stretches_only_the_pictures_own_raster_and_covers_with_every_other_shape() {
        assert_eq!(fit((720, 480), Some((720, 480)), false), Fit::Stretch, "NTSC DVD");
        assert_eq!(fit((720, 576), Some((720, 576)), false), Fit::Stretch, "PAL DVD / DVB");
        assert_eq!(fit((720, 576), Some((720, 432)), false), Fit::VideoPixels { width: 720, height: 432 }, "a cropped DVD rip");
        assert_eq!(fit((720, 576), Some((704, 432)), false), Fit::VideoPixels { width: 704, height: 432 }, "a crop of 704-wide video");
        assert_eq!(fit((720, 480), Some((720, 360)), false), Fit::VideoPixels { width: 720, height: 360 }, "an NTSC crop");
        // Every SD case that could be a resize stretches over the picture.
        assert_eq!(fit((720, 576), Some((704, 576)), false), Fit::Stretch, "DVB 720 × 576 over 704 × 576 broadcast video");
        assert_eq!(fit((720, 480), Some((640, 480)), false), Fit::Stretch, "a resized 640 × 480 rip");
        assert_eq!(fit((720, 576), Some((640, 360)), false), Fit::Stretch, "a resized 640 × 360 rip");
        assert_eq!(fit((720, 576), Some((544, 576)), false), Fit::Stretch, "544 × 576 broadcast video");
        assert_eq!(fit((720, 576), Some((352, 288)), false), Fit::Stretch, "a CIF encode");
        assert_eq!(fit((720, 480), Some((352, 240)), false), Fit::Stretch, "half-D1 video: a scaled-down frame");
        assert_eq!(fit((720, 576), Some((688, 432)), false), Fit::Stretch, "cropped on the sides too: the scale is unknown");
        assert_eq!(fit((720, 576), Some((1920, 1080)), false), Fit::Stretch, "an SD raster over HD video");
        assert_eq!(fit((704, 576), Some((1920, 1080)), false), Fit::Stretch, "704 wide");
        assert_eq!(fit((352, 480), None, false), Fit::Stretch, "half-D1");
        assert_eq!(fit((1920, 1080), Some((1920, 1080)), false), Fit::Stretch, "the video's own size");
        assert_eq!(fit((640, 360), Some((640, 360)), false), Fit::Stretch, "a canvas made up from the video");
        assert_eq!(fit((1920, 1080), Some((1920, 800)), false), Fit::Cover, "a cropped Blu-ray encode");
        assert_eq!(fit((1920, 1080), Some((1440, 1080)), false), Fit::Cover);
        assert_eq!(fit((1280, 720), None, false), Fit::Cover);
        assert_eq!(fit((1024, 576), Some((1024, 576 * 2)), false), Fit::Cover, "a 1024 × 576 DVB display: square pixels");
        assert_eq!(fit((960, 540), Some((1920, 1080)), false), Fit::Cover, "960 × 540");
        assert_eq!(fit((3840, 2160), Some((1920, 1080)), false), Fit::Cover, "a UHD canvas over 1080p");
        assert_eq!(fit((320, 240), Some((640, 480)), false), Fit::Cover, "not an SD raster: same shape, uniformly scaled");
        // A video turned a quarter round: the player hands its size over turned (1080 × 1920), so a canvas of the
        // coded size keeps its shape instead of being stretched onto the portrait picture.
        assert_eq!(fit((1920, 1080), Some((1080, 1920)), true), Fit::Cover);
        assert_eq!(fit((720, 576), Some((576, 720)), true), Fit::Cover, "nor does an SD raster");
        assert_eq!(fit((720, 576), Some((432, 720)), true), Fit::Cover);
        assert_eq!(fit((720, 576), Some((1080, 1920)), true), Fit::Cover);
        // Coded narrower than tall but not turned (544 × 576 broadcast video, a portrait phone recording with no
        // display matrix): the picture's own frame, stretched like any other SD case.
        assert_eq!(fit((720, 576), Some((544, 576)), false), Fit::Stretch);
        assert_eq!(fit((720, 576), Some((1080, 1920)), false), Fit::Stretch);
        assert_eq!(fit((1920, 1080), Some((1080, 1920)), false), Fit::Cover, "not an SD raster: keeps its shape");
        // The player never grows a canvas to hold a rectangle (`sa-player` `canvas`), so a damaged rectangle cannot
        // turn the video's own raster into another shape; one pixel off is another raster and keeps its shape.
        assert_eq!(fit((640, 360), Some((640, 360)), false), Fit::Stretch);
        assert_eq!(fit((641, 360), Some((640, 360)), false), Fit::Cover);
    }

    #[test]
    fn an_sd_canvas_over_sd_video_lies_at_the_videos_pixel_scale() {
        let surface = (1920.0, 1080.0);
        let pal = (720.0, 576.0);
        let whole = (0.0, 0.0, 720.0, 576.0);
        let fit_over = |video| fit((720, 576), Some(video), false);
        // PAL 16:9 and 4:3, uncropped: the same as stretching.
        assert_near!(place(whole, whole, pal, fit_over((720, 576)), surface, 16.0 / 9.0), (0.0, 0.0, 1920.0, 1080.0));
        assert_near!(place(whole, whole, pal, fit_over((720, 576)), surface, 4.0 / 3.0), (240.0, 0.0, 1440.0, 1080.0));
        // 720 × 432 cropped from a 16:9 PAL frame (shown 2.370:1, 1920 × 810 at y 135): the pixels keep the frame's
        // scale (2.667 × 1.875), so the canvas fills the surface and a caption at canvas y 520 lands in the
        // letterbox at 975, its glyphs as tall as on the disc.
        let aspect = 720.0 * (16.0 / 9.0) / 720.0 * 576.0 / 432.0;
        let rule = fit_over((720, 432));
        assert_near!(place(whole, whole, pal, rule, surface, aspect), (0.0, 0.0, 1920.0, 1080.0));
        let caption = (72.0, 520.0, 576.0, 40.0);
        let at = place(caption, caption, pal, rule, surface, aspect);
        assert_near!(at, (192.0, 975.0, 1536.0, 75.0));
        assert!(at.1 > 135.0 + 810.0, "under the picture");
        // Stretched it would have been squashed into the picture.
        let squashed = place(caption, caption, pal, Fit::Stretch, surface, aspect);
        assert!(squashed.3 < at.3 * 0.8 && squashed.1 + squashed.3 <= 135.0 + 810.0 + 0.01, "{squashed:?}");
        // 704 × 432 cropped from 704 × 576 video under a 720 × 576 canvas: the canvas reaches 8 video pixels past
        // each side, so the 704 pixels in the middle of the canvas are the picture's.
        let aspect704 = 704.0 * (16.0 / 9.0) / 704.0 * 576.0 / 432.0;
        let middle = (8.0, 72.0, 704.0, 432.0);
        assert_near!(place(middle, middle, pal, fit_over((704, 432)), surface, aspect704), (0.0, 135.0, 1920.0, 810.0));
        // A resized 640 × 480 rip, or 704 × 576 broadcast video: stretched onto the picture, never past its sides.
        for video in [(640, 480), (704, 576), (544, 576)] {
            assert_near!(place(whole, whole, pal, fit_over(video), surface, 4.0 / 3.0), (240.0, 0.0, 1440.0, 1080.0));
        }
        // NTSC 4:3: 720 × 480 over 720 × 480 shown in 1440 × 1080 at x 240.
        let ntsc = (0.0, 0.0, 720.0, 480.0);
        assert_near!(place(ntsc, ntsc, (720.0, 480.0), fit((720, 480), Some((720, 480)), false), surface, 4.0 / 3.0), (240.0, 0.0, 1440.0, 1080.0));
        // A half-D1 NTSC encode (352 × 240) under a full 720 × 480 canvas: stretched onto the picture.
        let at = place(ntsc, ntsc, (720.0, 480.0), fit((720, 480), Some((352, 240)), false), surface, 4.0 / 3.0);
        assert_near!(at, (240.0, 0.0, 1440.0, 1080.0));
        // Over HD video the SD canvas stretches onto the picture (how the frame was scaled is unknown).
        assert_eq!(fit((720, 576), Some((1920, 1080)), false), Fit::Stretch);
        assert_near!(place(whole, whole, pal, Fit::Stretch, surface, 16.0 / 9.0), (0.0, 0.0, 1920.0, 1080.0));
    }

    #[test]
    fn an_sd_canvas_over_a_quarter_turned_video_keeps_its_shape() {
        // 720 × 576 video turned (shown 576 × 720, portrait) in 1920 × 1080: fitted 864 × 1080 at x 528. The canvas
        // keeps a 1.25 shape, covering: 1350 × 1080 at x 285, never 864 × 1080 (glyphs 2.2 times taller than wide).
        let whole = (0.0, 0.0, 720.0, 576.0);
        let at = place(whole, whole, (720.0, 576.0), fit((720, 576), Some((576, 720)), true), (1920.0, 1080.0), 576.0 / 720.0);
        assert_near!(at, (285.0, 0.0, 1350.0, 1080.0));
        assert!((at.2 / at.3 - 1.25).abs() < 1e-3);
    }

    #[test]
    fn uniform_canvases_of_the_videos_shape_map_like_stretching() {
        // 3840 × 2160 over 1080p and 960 × 540 over 1080p in a 1920 × 1080 surface: scale 0.5 / 2, no offset.
        let r = (100.0, 2000.0, 1000.0, 100.0);
        assert_near!(place(r, r, (3840.0, 2160.0), Fit::Cover, (1920.0, 1080.0), 16.0 / 9.0), (50.0, 1000.0, 500.0, 50.0));
        let r = (50.0, 500.0, 500.0, 30.0);
        assert_near!(place(r, r, (960.0, 540.0), Fit::Cover, (1920.0, 1080.0), 16.0 / 9.0), (100.0, 1000.0, 1000.0, 60.0));
    }

    fn overlaps(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
        a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
    }

    fn inside(a: (f32, f32, f32, f32), surface: (f32, f32)) -> bool {
        a.0 >= -0.01 && a.1 >= -0.01 && a.0 + a.2 <= surface.0 + 0.01 && a.1 + a.3 <= surface.1 + 0.01
    }

    #[test]
    fn two_lines_pushed_off_a_full_screen_scope_film_move_up_together_and_stay_apart() {
        // 2.40:1 video full screen on 2560 × 1080: fitted 2560 × 1066.7 at y 6.7; the 1920 × 1080 canvas covers it
        // at scale 4 / 3, 1440 high at y -180. Lines at canvas y 870 and 950 (70 high) map to y 980 and 1086.7:
        // the second would hang off the bottom. Moved one by one it would land on the first.
        let surface = (2560.0, 1080.0);
        let (a, b) = ((480.0, 870.0, 960.0, 70.0), (480.0, 950.0, 960.0, 70.0));
        let group = (480.0, 870.0, 960.0, 150.0);
        let pa = place(a, group, (1920.0, 1080.0), Fit::Cover, surface, 2.4);
        let pb = place(b, group, (1920.0, 1080.0), Fit::Cover, surface, 2.4);
        assert_near!(pa, (640.0, 880.0, 1280.0, 93.33));
        assert_near!(pb, (640.0, 986.67, 1280.0, 93.33));
        assert!(inside(pa, surface) && inside(pb, surface));
        assert!(!overlaps(pa, pb), "{pa:?} {pb:?}");
        assert!((pb.1 - pa.1 - 80.0 * 4.0 / 3.0).abs() < 0.01, "the gap between them is the canvas's");
    }

    #[test]
    fn a_sign_at_the_top_and_a_caption_at_the_bottom_both_stay_visible_the_event_scaled_down_as_a_whole() {
        // The same screen: the canvas reaches 180 px past the top and bottom. A sign at canvas y 40 and a caption at
        // y 950 span 1306.7 px on the surface, more than its 1080: the event shrinks by one factor (0.8265) and
        // fits exactly, the sign above the caption, both their own shape.
        let surface = (2560.0, 1080.0);
        let (sign, caption) = ((200.0, 40.0, 600.0, 60.0), (480.0, 950.0, 960.0, 70.0));
        let group = (200.0, 40.0, 1240.0, 980.0);
        let ps = place(sign, group, (1920.0, 1080.0), Fit::Cover, surface, 2.4);
        let pc = place(caption, group, (1920.0, 1080.0), Fit::Cover, surface, 2.4);
        let k = 1080.0 / (980.0 * 4.0 / 3.0);
        assert!(inside(ps, surface) && inside(pc, surface), "{ps:?} {pc:?}");
        assert!(!overlaps(ps, pc) && ps.1 + ps.3 < pc.1);
        assert!((ps.1).abs() < 0.01 && (pc.1 + pc.3 - 1080.0).abs() < 0.01, "top to bottom: {ps:?} {pc:?}");
        assert!((ps.2 - 600.0 * 4.0 / 3.0 * k).abs() < 0.01 && (pc.3 - 70.0 * 4.0 / 3.0 * k).abs() < 0.01);
        assert!((ps.2 / ps.3 - 10.0).abs() < 1e-3 && (pc.2 / pc.3 - 960.0 / 70.0).abs() < 1e-3, "not squashed");
        // In a surface that holds it, nothing is scaled: the same event in a 2560 × 1440 window is exactly placed.
        let tall = (2560.0, 1440.0);
        let ps = place(sign, group, (1920.0, 1080.0), Fit::Cover, tall, 16.0 / 9.0);
        assert_near!(ps, (266.67, 53.33, 800.0, 80.0));
    }

    #[test]
    fn a_cropped_encode_keeps_its_captions_unsquashed_in_the_letterbox() {
        // 1920 × 800 video in a 1600 × 900 surface: fitted 1600 × 666.7 at y 116.7. The 1920 × 1080 canvas covers
        // it at 1600 × 900, the whole surface: a caption at y 950 of the canvas lands in the bottom bar, with the
        // canvas's own proportions.
        let caption = (480.0, 950.0, 960.0, 80.0);
        let aspect = 1920.0 / 800.0;
        let at = place(caption, caption, (1920.0, 1080.0), Fit::Cover, (1600.0, 900.0), aspect);
        assert_near!(at, (400.0, 791.67, 800.0, 66.67));
        assert!((at.2 / at.3 - 12.0).abs() < 1e-3, "not squashed");
        assert!(at.1 > 116.67 + 666.67, "in the letterbox under the picture");
        // Stretched it would have been squashed by 26 % and pulled up into the picture.
        let squashed = place(caption, caption, (1920.0, 1080.0), Fit::Stretch, (1600.0, 900.0), aspect);
        assert_near!(squashed, (400.0, 116.67 + 950.0 * 666.67 / 1080.0, 800.0, 80.0 * 666.67 / 1080.0));
        assert!(squashed.1 + squashed.3 < 116.67 + 666.67 + 0.01, "inside the picture");
    }

    #[test]
    fn a_square_hd_canvas_over_anamorphic_video_of_the_same_display_aspect_maps_like_the_picture() {
        // 1440 × 1080 coded, shown 16:9: the 1920 × 1080 canvas covers the fitted rectangle exactly.
        let rect = (0.0, 0.0, 1920.0, 1080.0);
        let cover = place(rect, rect, (1920.0, 1080.0), fit((1920, 1080), Some((1440, 1080)), false), (1280.0, 720.0), 16.0 / 9.0);
        assert_near!(cover, (0.0, 0.0, 1280.0, 720.0));
        assert_near!(place((960.0, 540.0, 96.0, 54.0), (960.0, 540.0, 96.0, 54.0), (1920.0, 1080.0), Fit::Cover, (1280.0, 800.0), 16.0 / 9.0), (640.0, 400.0, 64.0, 36.0));
        // Shown 4:3 (square pixels) in a 1280 × 720 surface: the 960 × 720 picture at x 160; the canvas keeps 16:9
        // (1280 × 720) and reaches past it into the pillarbox.
        let wide = place(rect, rect, (1920.0, 1080.0), Fit::Cover, (1280.0, 720.0), 4.0 / 3.0);
        assert_near!(wide, (0.0, 0.0, 1280.0, 720.0));
        let sign = place((0.0, 0.0, 192.0, 108.0), (0.0, 0.0, 192.0, 108.0), (1920.0, 1080.0), Fit::Cover, (1280.0, 720.0), 4.0 / 3.0);
        assert_near!(sign, (0.0, 0.0, 128.0, 72.0));
    }

    #[test]
    fn an_anamorphic_pal_dvd_canvas_stretches_with_the_picture() {
        // 720 × 576 shown 16:9 in a 1920 × 1080 surface: x scaled 1920 / 720, y 1080 / 576.
        let at = place((72.0, 500.0, 576.0, 60.0), (72.0, 500.0, 576.0, 60.0), (720.0, 576.0), fit((720, 576), Some((720, 576)), false), (1920.0, 1080.0), 16.0 / 9.0);
        assert_near!(at, (192.0, 937.5, 1536.0, 112.5));
        // Pillarboxed 4:3 in the same surface: the canvas follows the picture rectangle (1440 wide at x 240).
        let at = place((0.0, 0.0, 720.0, 576.0), (0.0, 0.0, 720.0, 576.0), (720.0, 576.0), Fit::Stretch, (1920.0, 1080.0), 4.0 / 3.0);
        assert_near!(at, (240.0, 0.0, 1440.0, 1080.0));
    }

    #[test]
    fn in_a_tall_narrow_window_the_canvas_covers_the_video_and_rectangles_stay_on_the_surface() {
        // 1920 × 800 video in a 400 × 1000 surface: fitted 400 × 166.7 at y 416.7; the canvas 400 × 225 at y 387.5.
        let at = place((480.0, 950.0, 960.0, 80.0), (480.0, 950.0, 960.0, 80.0), (1920.0, 1080.0), Fit::Cover, (400.0, 1000.0), 2.4);
        assert_near!(at, (100.0, 387.5 + 950.0 * 400.0 / 1920.0, 200.0, 80.0 * 400.0 / 1920.0));
        // A window exactly as wide as the video: the canvas reaches past the surface's top and bottom; the caption is
        // moved up inside it, at its size.
        let at = place((480.0, 950.0, 960.0, 80.0), (480.0, 950.0, 960.0, 80.0), (1920.0, 1080.0), Fit::Cover, (2400.0, 1000.0), 2.4);
        assert_near!(at, (600.0, 900.0, 1200.0, 100.0));
    }

    #[test]
    fn rectangles_outside_the_canvas_or_larger_than_the_surface_are_brought_on_screen() {
        // A damaged stream: a rectangle beyond the canvas's right and bottom edges (fractions over 1).
        let at = place((1900.0, 1070.0, 200.0, 50.0), (1900.0, 1070.0, 200.0, 50.0), (1920.0, 1080.0), Fit::Stretch, (960.0, 540.0), 16.0 / 9.0);
        assert_near!(at, (860.0, 515.0, 100.0, 25.0));
        // Larger than the surface: shrunk to fit about its centre, keeping its shape.
        let at = place((0.0, 0.0, 7680.0, 1080.0), (0.0, 0.0, 7680.0, 1080.0), (1920.0, 1080.0), Fit::Stretch, (960.0, 540.0), 16.0 / 9.0);
        assert_near!(at, (0.0, 202.5, 960.0, 135.0));
        // Degenerate input never yields NaN.
        assert_near!(place((0.0, 0.0, 10.0, 10.0), (0.0, 0.0, 10.0, 10.0), (0.0, 0.0), Fit::Cover, (0.0, 0.0), 0.0), (0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn every_rectangle_becomes_an_image_of_its_size_and_broken_ones_are_left_out() {
        let rect = |x, y, w, h, len| SubtitleBitmap { x, y, width: w, height: h, rgba: Arc::new(vec![200; len]) };
        let picture = SubtitlePicture {
            id: 1,
            stream_index: 3,
            canvas: (720, 480),
            video_size: None,
            video_turned: false,
            start: 1.0,
            end: None,
            rects: vec![rect(72, 400, 360, 48, 360 * 48 * 4), rect(0, 0, 10, 10, 12), rect(36, 24, 72, 24, 72 * 24 * 4)],
        };
        let rows = overlay_rects(&picture);
        assert_eq!(rows.len(), 2, "a rectangle with too few bytes is skipped");
        assert_eq!((rows[0].image.size().width, rows[0].image.size().height), (360, 48));
        assert_eq!((rows[0].x, rows[0].y, rows[0].width, rows[0].height, rows[0].canvas_width, rows[0].canvas_height), (72.0, 400.0, 360.0, 48.0, 720.0, 480.0));
        assert!(!rows[0].cover);
        assert_eq!((rows[0].group_x, rows[0].group_y, rows[0].group_width, rows[0].group_height), (36.0, 24.0, 396.0, 424.0), "the drawn rectangles' box");
        assert_eq!((rows[1].group_x, rows[1].group_height), (36.0, 424.0));
        assert_eq!((rows[1].x, rows[1].y), (36.0, 24.0));
        let b = place_row(&rows[0], 1440.0, 960.0, 1.5);
        assert_eq!((b.x, b.y, b.width, b.height), (144.0, 800.0, 720.0, 96.0));
        let cropped_dvd = SubtitlePicture { canvas: (720, 576), video_size: Some((720, 432)), ..picture.clone() };
        let rows_dvd = overlay_rects(&cropped_dvd);
        assert!(!rows_dvd[0].cover && (rows_dvd[0].pixel_width, rows_dvd[0].pixel_height) == (720.0, 432.0));
        assert_eq!(rows[0].pixel_width, 0.0, "no video size: stretched");
        let hd = SubtitlePicture { canvas: (1920, 1080), video_size: Some((1920, 800)), ..picture };
        assert!(overlay_rects(&hd)[0].cover);
    }

    #[test]
    fn place_row_brings_the_event_on_screen_by_its_group_box_not_the_rectangle_alone() {
        // The two lines of a full-screen scope film: the event (canvas y 870..1020, surface 980..1180) hangs 100 px off
        // the bottom, so both lines move up by 100 px.
        let row = |rect: (f32, f32, f32, f32)| PictureSubtitleRect {
            image: Image::default(),
            x: rect.0,
            y: rect.1,
            width: rect.2,
            height: rect.3,
            canvas_width: 1920.0,
            canvas_height: 1080.0,
            cover: true,
            pixel_width: 0.0,
            pixel_height: 0.0,
            group_x: 480.0,
            group_y: 870.0,
            group_width: 960.0,
            group_height: 150.0,
        };
        let upper = place_row(&row((480.0, 870.0, 960.0, 70.0)), 2560.0, 1080.0, 2.4);
        let lower = place_row(&row((480.0, 950.0, 960.0, 70.0)), 2560.0, 1080.0, 2.4);
        assert_near!((upper.x, upper.y, upper.width, upper.height), (640.0, 880.0, 1280.0, 93.33));
        assert_near!((lower.x, lower.y, lower.width, lower.height), (640.0, 986.67, 1280.0, 93.33));
        // Placed by its own box the upper line would not have moved at all (and would sit where the lower one is).
        let alone = place((480.0, 870.0, 960.0, 70.0), (480.0, 870.0, 960.0, 70.0), (1920.0, 1080.0), Fit::Cover, (2560.0, 1080.0), 2.4);
        assert_near!(alone, (640.0, 980.0, 1280.0, 93.33));
        // The pixel-scale rule reaches place_row through the row's video size.
        let dvd = PictureSubtitleRect { canvas_width: 720.0, canvas_height: 576.0, cover: false, pixel_width: 720.0, pixel_height: 432.0, group_x: 72.0, group_y: 500.0, group_width: 576.0, group_height: 60.0, ..row((72.0, 500.0, 576.0, 60.0)) };
        let b = place_row(&dvd, 1920.0, 1080.0, 720.0 * (16.0 / 9.0) / 720.0 * 576.0 / 432.0);
        assert_near!((b.x, b.y, b.width, b.height), (192.0, 937.5, 1536.0, 112.5));
    }
}
