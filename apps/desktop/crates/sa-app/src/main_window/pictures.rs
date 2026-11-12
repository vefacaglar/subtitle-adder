//! The bitmap subtitle picture (PGS, VobSub, DVB) as the player overlay draws it: each rectangle of the player's
//! [`SubtitlePicture`] as a straight-alpha Slint image with its place as fractions of the subtitle canvas, which
//! covers the whole video picture. The overlay (`PictureSubtitleOverlay`, `ui/shared/overlay_label.slint`) maps those
//! fractions onto the letterboxed video rectangle, so the picture scales with the video in a window, in full screen,
//! on the GPU path, the RGBA path and the software renderer alike (it is drawn by Slint over whatever shows the
//! video). Built once per subtitle event, on the UI thread.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use sa_player::SubtitlePicture;

use crate::ui::PictureSubtitleRect;

/// Where a rectangle goes, as fractions of the canvas: `(x, y, width, height)`.
pub fn placement(canvas: (u32, u32), x: u32, y: u32, width: u32, height: u32) -> (f32, f32, f32, f32) {
    let (cw, ch) = (canvas.0.max(1) as f32, canvas.1.max(1) as f32);
    (x as f32 / cw, y as f32 / ch, width as f32 / cw, height as f32 / ch)
}

/// The overlay's rows for `picture` (one per rectangle; the RGBA bytes copied once into Slint's buffers).
pub fn overlay_rects(picture: &SubtitlePicture) -> Vec<PictureSubtitleRect> {
    picture
        .rects
        .iter()
        .filter(|r| r.width > 0 && r.height > 0 && r.rgba.len() >= (r.width * r.height * 4) as usize)
        .map(|r| {
            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&r.rgba[..(r.width * r.height * 4) as usize], r.width, r.height);
            let (x, y, width, height) = placement(picture.canvas, r.x, r.y, r.width, r.height);
            // Straight alpha, as the player hands it over (`from_rgba8`, not `from_rgba8_premultiplied`).
            PictureSubtitleRect { image: Image::from_rgba8(buf), x, y, width, height }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sa_player::SubtitleBitmap;

    use super::*;

    #[test]
    fn rectangles_are_placed_as_fractions_of_the_canvas() {
        assert_eq!(placement((1920, 1080), 480, 810, 960, 135), (0.25, 0.75, 0.5, 0.125));
        // A degenerate canvas never divides by zero.
        assert_eq!(placement((0, 0), 1, 2, 3, 4), (1.0, 2.0, 3.0, 4.0));
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
        assert_eq!((rows[0].x, rows[0].y, rows[0].width, rows[0].height), (0.1, 400.0 / 480.0, 0.5, 0.1));
        assert_eq!((rows[1].x, rows[1].y), (0.05, 0.05));
    }
}
