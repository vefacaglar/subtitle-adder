//! Hub card pictures on Windows (UNIFIED.md §5.8; C# `StorageFile.GetThumbnailAsync(VideosView, 240,
//! UseCurrentScale)`): the shell thumbnail through `IShellItemImageFactory::GetImage` at 240 px × the system scale,
//! one at a time. The bytes are encoded as JPEG (opaque) or PNG (with transparency), the formats the C# app decodes.
//! Never called logged out (the caller's rule).

use std::path::Path;

use image::{ImageEncoder, RgbImage, RgbaImage};
use sa_core::cancel::CancellationToken;
use sa_core::platform::ThumbnailGenerator;
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, SIZE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_RESIZETOFIT};

use super::wide::Wide;

/// Requested edge in DIP.
pub const THUMBNAIL_DIP: i32 = 240;
/// JPEG quality of the encoded card picture.
pub const JPEG_QUALITY: u8 = 85;

#[derive(Debug, Default, Clone, Copy)]
pub struct ShellThumbnailGenerator;

impl ShellThumbnailGenerator {
    pub fn new() -> Self {
        Self
    }
}

impl ThumbnailGenerator for ShellThumbnailGenerator {
    fn generate(&self, path: &Path, cancel: &CancellationToken) -> Option<Vec<u8>> {
        if cancel.is_cancelled() || !path.is_file() {
            return None;
        }
        let size = (THUMBNAIL_DIP as f64 * unsafe { GetDpiForSystem() }.max(96) as f64 / 96.0).round() as i32;
        let (w, h, bgra) = with_com(|| unsafe { shell_bitmap(path, size) })?;
        if cancel.is_cancelled() {
            return None;
        }
        encode_bgra(w, h, &bgra)
    }

    fn max_concurrency(&self) -> usize {
        1
    }
}

/// Runs `f` inside an STA on this thread (initialized and released around it unless the thread already chose
/// another apartment).
fn with_com<T>(f: impl FnOnce() -> T) -> T {
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let r = f();
    if hr != RPC_E_CHANGED_MODE {
        unsafe { CoUninitialize() };
    }
    r
}

/// Top-down 32-bit BGRA pixels of the shell image.
unsafe fn shell_bitmap(path: &Path, size: i32) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let wpath = Wide::path(path);
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(wpath.pcwstr(), None).ok()?;
        let hbm: HBITMAP = factory.GetImage(SIZE { cx: size, cy: size }, SIIGBF_RESIZETOFIT).ok()?;
        let result = read_bitmap(hbm);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        result
    }
}

unsafe fn read_bitmap(hbm: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(HGDIOBJ(hbm.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        if w <= 0 || h <= 0 {
            return None;
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbm, 0, h as u32, Some(px.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        (lines == h).then_some((w as u32, h as u32, px))
    }
}

/// JPEG for opaque pictures (an all-zero alpha channel means "no alpha"), PNG (unpremultiplied) otherwise.
pub fn encode_bgra(w: u32, h: u32, bgra: &[u8]) -> Option<Vec<u8>> {
    if bgra.len() != (w * h * 4) as usize || w == 0 || h == 0 {
        return None;
    }
    let alphas = bgra.chunks_exact(4).map(|p| p[3]);
    let opaque = alphas.clone().all(|a| a == 255) || alphas.clone().all(|a| a == 0);
    let mut out = Vec::new();
    if opaque {
        let rgb: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
        let img = RgbImage::from_raw(w, h, rgb)?;
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).encode_image(&img).ok()?;
    } else {
        let rgba: Vec<u8> = bgra
            .chunks_exact(4)
            .flat_map(|p| {
                let a = p[3] as u32;
                let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
                [un(p[2]), un(p[1]), un(p[0]), p[3]]
            })
            .collect();
        let img = RgbaImage::from_raw(w, h, rgba)?;
        image::codecs::png::PngEncoder::new(&mut out).write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_pixels_become_jpeg_and_transparent_ones_png() {
        let opaque: Vec<u8> = (0..16 * 9).flat_map(|i| [i as u8, 10, 200, 255]).collect();
        let j = encode_bgra(16, 9, &opaque).unwrap();
        assert_eq!(&j[..2], &[0xFF, 0xD8]);
        let no_alpha: Vec<u8> = (0..16 * 9).flat_map(|_| [1, 2, 3, 0]).collect();
        assert_eq!(&encode_bgra(16, 9, &no_alpha).unwrap()[..2], &[0xFF, 0xD8]);
        let mut mixed = opaque.clone();
        mixed[3] = 128;
        let p = encode_bgra(16, 9, &mixed).unwrap();
        assert_eq!(&p[..4], b"\x89PNG");
        let decoded = image::load_from_memory(&p).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(1, 0).0, [200, 10, 1, 255]);
        assert!(encode_bgra(2, 2, &[0; 3]).is_none());
    }

    #[test]
    fn shell_thumbnails_of_a_real_image_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("pic.png");
        RgbImage::from_pixel(64, 36, image::Rgb([20, 120, 220])).save(&img).unwrap();
        let g = ShellThumbnailGenerator::new();
        let token = CancellationToken::new();
        let bytes = g.generate(&img, &token).expect("shell image");
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert!(decoded.width() > 0 && decoded.height() > 0);
        assert_eq!(g.max_concurrency(), 1);
        assert!(g.generate(&dir.path().join("missing.mp4"), &token).is_none());
        token.cancel();
        assert!(g.generate(&img, &token).is_none());
    }
}
