//! The app icon (mac-ui §13.5; port of `scripts/macos/make-icon.swift`, same design as the committed `windows/AppIcon.ico`):
//! a white superellipse body with a light shadow, a purple speech bubble with a white sound wave, and two dark
//! subtitle lines below. Drawn on the 1024 grid (origin top-left, y down) with an exact-coverage scanline
//! rasterizer (4 sub-scanlines per pixel, analytic horizontal coverage), composited in premultiplied RGBA.
//!
//! The shadow follows `CGContextSetShadow(offset (0, 10), blur 24, black 25 %)`: the offset is in CG base space
//! (y up), so in the top-down image it sits 10 px **above** the body, and offset/blur are in output pixels.

use std::f64::consts::PI;

type Pt = (f64, f64);

pub const ACCENT: u32 = 0x5B4BDB;
pub const INK: u32 = 0x1F2333;
pub const WAVE_HEIGHTS: [f64; 7] = [64.0, 136.0, 208.0, 120.0, 232.0, 152.0, 80.0];

/// Straight-alpha RGBA8 pixels, `size × size`.
pub struct IconImage {
    pub size: usize,
    pub rgba: Vec<u8>,
}

/// Superellipse (n = 5) in `(x, y, w, h)`, 720 steps; `sgn(0) = +1`.
pub fn squircle(rect: (f64, f64, f64, f64), n: f64) -> Vec<Pt> {
    let (a, b) = (rect.2 / 2.0, rect.3 / 2.0);
    let (cx, cy) = (rect.0 + a, rect.1 + b);
    let sgn = |v: f64| if v < 0.0 { -1.0 } else { 1.0 };
    (0..720)
        .map(|i| {
            let t = i as f64 / 720.0 * 2.0 * PI;
            let (c, s) = (t.cos(), t.sin());
            (cx + a * sgn(c) * c.abs().powf(2.0 / n), cy + b * sgn(s) * s.abs().powf(2.0 / n))
        })
        .collect()
}

/// Rounded rectangle with circular corners (`CGPath(roundedRect:cornerWidth:cornerHeight:)`), flattened.
pub fn rounded_rect(rect: (f64, f64, f64, f64), r: f64) -> Vec<Pt> {
    let (x, y, w, h) = rect;
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pts = Vec::new();
    let corners = [(x + w - r, y + r, -PI / 2.0), (x + w - r, y + h - r, 0.0), (x + r, y + h - r, PI / 2.0), (x + r, y + r, PI)];
    for (cx, cy, start) in corners {
        for k in 0..=24 {
            let t = start + k as f64 / 24.0 * (PI / 2.0);
            pts.push((cx + r * t.cos(), cy + r * t.sin()));
        }
    }
    pts
}

/// The bubble's tail: move (318,530) → line (292,668) → quad to (448,576) with control (360,644), closed.
pub fn tail() -> Vec<Pt> {
    let mut pts = vec![(318.0, 530.0), (292.0, 668.0)];
    let (p0, c, p1) = ((292.0, 668.0), (360.0, 644.0), (448.0, 576.0));
    for k in 1..=48 {
        let t = k as f64 / 48.0;
        let u = 1.0 - t;
        pts.push((u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p1.0, u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p1.1));
    }
    pts
}

/// Coverage (0..1) of a closed polygon (non-zero winding) on a `size × size` grid; `pts` in output pixels.
pub fn coverage(pts: &[Pt], size: usize) -> Vec<f32> {
    const SUB: usize = 4;
    let mut cov = vec![0f32; size * size];
    let n = pts.len();
    let mut xs: Vec<(f64, i32)> = Vec::new();
    for row in 0..size * SUB {
        let sy = (row as f64 + 0.5) / SUB as f64;
        xs.clear();
        for i in 0..n {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            if (a.1 <= sy && b.1 > sy) || (b.1 <= sy && a.1 > sy) {
                let t = (sy - a.1) / (b.1 - a.1);
                xs.push((a.0 + t * (b.0 - a.0), if b.1 > a.1 { 1 } else { -1 }));
            }
        }
        xs.sort_by(|p, q| p.0.total_cmp(&q.0));
        let y = row / SUB;
        let line = &mut cov[y * size..(y + 1) * size];
        let mut winding = 0;
        for k in 0..xs.len() {
            let was_inside = winding != 0;
            winding += xs[k].1;
            if was_inside && k > 0 {
                add_span(line, xs[k - 1].0, xs[k].0, 1.0 / SUB as f32);
            }
        }
    }
    for c in &mut cov {
        *c = c.min(1.0);
    }
    cov
}

fn add_span(line: &mut [f32], x0: f64, x1: f64, weight: f32) {
    let w = line.len() as f64;
    let (x0, x1) = (x0.clamp(0.0, w), x1.clamp(0.0, w));
    if x1 <= x0 {
        return;
    }
    let (i0, i1) = (x0.floor() as usize, x1.floor() as usize);
    if i0 == i1 {
        if i0 < line.len() {
            line[i0] += ((x1 - x0) as f32) * weight;
        }
        return;
    }
    line[i0] += ((i0 as f64 + 1.0 - x0) as f32) * weight;
    let end = i1.min(line.len());
    for px in &mut line[i0 + 1..end] {
        *px += weight;
    }
    if i1 < line.len() {
        line[i1] += ((x1 - i1 as f64) as f32) * weight;
    }
}

/// Separable Gaussian blur (σ in pixels) of a coverage mask.
pub fn gaussian_blur(mask: &[f32], size: usize, sigma: f64) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil() as isize;
    let kernel: Vec<f32> = (-radius..=radius).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp() as f32).collect();
    let sum: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / sum).collect();
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut dst = vec![0f32; size * size];
        for y in 0..size {
            for x in 0..size {
                let mut acc = 0f32;
                for (k, w) in kernel.iter().enumerate() {
                    let d = k as isize - radius;
                    let (sx, sy) = if horizontal { (x as isize + d, y as isize) } else { (x as isize, y as isize + d) };
                    if sx >= 0 && sy >= 0 && (sx as usize) < size && (sy as usize) < size {
                        acc += src[sy as usize * size + sx as usize] * w;
                    }
                }
                dst[y * size + x] = acc;
            }
        }
        dst
    };
    pass(&pass(mask, true), false)
}

/// Premultiplied RGBA canvas.
struct Canvas {
    size: usize,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn fill(&mut self, cov: &[f32], rgb: u32, alpha: f32) {
        let (r, g, b) = (((rgb >> 16) & 0xFF) as f32 / 255.0, ((rgb >> 8) & 0xFF) as f32 / 255.0, (rgb & 0xFF) as f32 / 255.0);
        for (p, &c) in self.px.iter_mut().zip(cov) {
            let a = c * alpha;
            if a <= 0.0 {
                continue;
            }
            let k = 1.0 - a;
            *p = [r * a + p[0] * k, g * a + p[1] * k, b * a + p[2] * k, a + p[3] * k];
        }
    }

    fn into_rgba8(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.size * self.size * 4);
        for p in self.px {
            let a = p[3].clamp(0.0, 1.0);
            let un = |c: f32| if a <= 0.0 { 0 } else { ((c / a).clamp(0.0, 1.0) * 255.0).round() as u8 };
            out.extend_from_slice(&[un(p[0]), un(p[1]), un(p[2]), (a * 255.0).round() as u8]);
        }
        out
    }
}

/// Renders the icon at `size` px.
pub fn render(size: usize) -> IconImage {
    let s = size as f64 / 1024.0;
    let scale = |pts: Vec<Pt>| -> Vec<Pt> { pts.into_iter().map(|(x, y)| (x * s, y * s)).collect() };
    let mut canvas = Canvas { size, px: vec![[0.0; 4]; size * size] };

    // Body with its shadow (offset and blur in output pixels; the shadow sits above the body, see module docs).
    let body = scale(squircle((100.0, 100.0, 824.0, 824.0), 5.0));
    let shadow_pts: Vec<Pt> = body.iter().map(|&(x, y)| (x, y - 10.0)).collect();
    let shadow = gaussian_blur(&coverage(&shadow_pts, size), size, 12.0);
    canvas.fill(&shadow, 0x000000, 0.25);
    canvas.fill(&coverage(&body, size), 0xFFFFFF, 1.0);

    // Speech bubble and tail.
    let bubble = (232.0, 232.0, 560.0, 360.0);
    canvas.fill(&coverage(&scale(rounded_rect(bubble, 112.0)), size), ACCENT, 1.0);
    canvas.fill(&coverage(&scale(tail()), size), ACCENT, 1.0);

    // Sound wave: white bars centered on the bubble.
    let (bar_w, gap) = (36.0, 28.0);
    let total = WAVE_HEIGHTS.len() as f64 * bar_w + (WAVE_HEIGHTS.len() - 1) as f64 * gap;
    let (mid_x, mid_y) = (bubble.0 + bubble.2 / 2.0, bubble.1 + bubble.3 / 2.0);
    for (i, h) in WAVE_HEIGHTS.iter().enumerate() {
        let x = mid_x - total / 2.0 + i as f64 * (bar_w + gap);
        canvas.fill(&coverage(&scale(rounded_rect((x, mid_y - h / 2.0, bar_w, *h), bar_w / 2.0)), size), 0xFFFFFF, 1.0);
    }

    // Subtitle lines.
    for (y, w) in [(704.0, 496.0), (776.0, 320.0)] {
        canvas.fill(&coverage(&scale(rounded_rect((512.0 - w / 2.0, y, w, 40.0), 20.0)), size), INK, 1.0);
    }
    IconImage { size, rgba: canvas.into_rgba8() }
}

/// PNG bytes of the icon.
pub fn png(size: usize) -> Vec<u8> {
    use image::ImageEncoder;
    let img = render(size);
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&img.rgba, size as u32, size as u32, image::ExtendedColorType::Rgba8)
        .expect("PNG encoding into memory");
    out
}

/// A multi-size `.ico` with PNG entries (16 … 256), like the committed `windows/AppIcon.ico`.
pub fn ico(sizes: &[usize]) -> Vec<u8> {
    let images: Vec<(usize, Vec<u8>)> = sizes.iter().map(|&s| (s, png(s))).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (s, data) in &images {
        let d = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[d, d, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for (_, data) in images {
        out.extend_from_slice(&data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &IconImage, x: usize, y: usize) -> [u8; 4] {
        let i = (y * img.size + x) * 4;
        [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2], img.rgba[i + 3]]
    }

    #[test]
    fn coverage_of_a_square_is_exact() {
        let c = coverage(&[(2.0, 2.0), (6.5, 2.0), (6.5, 6.0), (2.0, 6.0)], 8);
        assert_eq!(c[3 * 8 + 3], 1.0);
        assert!((c[3 * 8 + 6] - 0.5).abs() < 1e-6);
        assert_eq!(c[0], 0.0);
        let total: f32 = c.iter().sum();
        assert!((total - 18.0).abs() < 1e-3, "{total}");
    }

    #[test]
    fn the_icon_has_the_design_colors_in_place() {
        let img = render(256);
        let k = 256.0 / 1024.0;
        let at = |x: f64, y: f64| px(&img, (x * k) as usize, (y * k) as usize);
        assert_eq!(at(10.0, 10.0)[3], 0, "transparent corner");
        assert_eq!(at(150.0, 512.0), [255, 255, 255, 255], "white body");
        assert_eq!(at(300.0, 300.0), [0x5B, 0x4B, 0xDB, 255], "bubble");
        assert_eq!(at(512.0, 412.0), [255, 255, 255, 255], "center wave bar");
        assert_eq!(at(512.0, 724.0), [0x1F, 0x23, 0x33, 255], "first subtitle line");
        assert_eq!(at(330.0, 600.0), [0x5B, 0x4B, 0xDB, 255], "tail");
        // The shadow shows above the body (CG base-space offset), not below it.
        let above = at(512.0, 96.0)[3];
        let below = at(512.0, 928.0)[3];
        assert!(above > below, "{above} vs {below}");
        assert!(above > 0 && above < 80);
    }

    #[test]
    fn png_and_ico_containers() {
        let p = png(32);
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        let i = ico(&[16, 256]);
        assert_eq!(&i[..6], &[0, 0, 1, 0, 2, 0]);
        assert_eq!(i[6], 16);
        assert_eq!(i[22], 0, "256 px is stored as 0");
    }
}
