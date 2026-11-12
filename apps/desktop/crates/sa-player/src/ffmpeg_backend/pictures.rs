//! The bitmap subtitle track shown over the video (PGS, VobSub, DVB, XSUB): its decoder, fed on the demux thread with
//! the packets of the selected stream (the player's own demuxer, so the pictures follow playback, seeks and track
//! switches exactly), and the conversion of each decoded event into a [`SubtitlePicture`] for
//! [`crate::picture::PictureTimeline`].
//!
//! FFmpeg's decoders hand over `SUBTITLE_BITMAP` rectangles: PAL8 indices (`data[0]`, `linesize[0]` per row) and a
//! palette of `nb_colors` native-endian `uint32` values `0xAARRGGBB` (`data[1]`, `AV_PIX_FMT_PAL8`'s layout) with
//! straight alpha (pgssubdec / dvdsubdec / dvbsubdec / xsubdec write `RGBA(r, g, b, a)` = `a << 24 | r << 16 | g << 8
//! | b`, colours not multiplied by alpha). They become straight-alpha RGBA8 bytes here; an index past `nb_colors` is
//! transparent. Times are taken from the packet (pts, else dts) plus `start_display_time`; the end is
//! `end_display_time` when the decoder gives one, else (not for PGS, whose events end with the next display set)
//! the packet duration, else none (the timeline then ends it at the next event).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ffmpeg_next::ffi;
use ffmpeg_next::{Packet, Rational, codec};

use super::tracks;
use crate::SubtitleKind;
use crate::picture::{MAX_EVENT_PIXELS, MAX_SIDE, SubtitleBitmap, SubtitlePicture};

static NEXT_PICTURE_ID: AtomicU64 = AtomicU64::new(1);

/// One decoded event: a picture, or a clearing event (`picture` `None`).
pub(crate) struct DecodedEvent {
    pub start: f64,
    pub end: Option<f64>,
    pub picture: Option<SubtitlePicture>,
}

/// Frees the `AVSubtitle` a decode filled (ffmpeg-next's wrapper does not).
struct SubtitleGuard(codec::subtitle::Subtitle);

impl Drop for SubtitleGuard {
    fn drop(&mut self) {
        unsafe { ffi::avsubtitle_free(self.0.as_mut_ptr()) };
    }
}

/// The decoder of the bitmap subtitle stream being shown.
pub(crate) struct PictureTrack {
    pub stream: usize,
    dec: codec::decoder::Subtitle,
    tb: Rational,
    /// PGS events end with the next display set, never with the packet's duration.
    pgs: bool,
    /// The canvas when the decoder names none: the video's coded size.
    fallback_canvas: (u32, u32),
    /// Events decoded / packets the decoder rejected / events dropped as too large (diagnostics).
    pub decoded: u64,
    pub failed: u64,
    pub oversized: u64,
}

impl PictureTrack {
    /// Opens the decoder of picture stream `stream` (codec parameters with extradata: the VobSub palette and size
    /// live there; the packet time base set). `video_size`: the video's coded size, the canvas fallback.
    pub fn open(ictx: &ffmpeg_next::format::context::Input, stream: usize, video_size: Option<(u32, u32)>) -> Result<Self, String> {
        let s = ictx.stream(stream).ok_or_else(|| ffmpeg_next::Error::StreamNotFound.to_string())?;
        let params = s.parameters();
        let codec_id = unsafe { (*params.as_ptr()).codec_id };
        if params.medium() != ffmpeg_next::media::Type::Subtitle || tracks::subtitle_kind(codec_id) != SubtitleKind::Image {
            return Err(ffmpeg_next::Error::DecoderNotFound.to_string());
        }
        let tb = s.time_base();
        let mut cctx = codec::context::Context::from_parameters(params).map_err(|e| e.to_string())?;
        unsafe { (*cctx.as_mut_ptr()).pkt_timebase = tb.into() };
        let dec = cctx.decoder().subtitle().map_err(|e| e.to_string())?;
        Ok(Self {
            stream,
            dec,
            tb,
            pgs: codec_id == ffi::AVCodecID::AV_CODEC_ID_HDMV_PGS_SUBTITLE,
            fallback_canvas: video_size.filter(|(w, h)| *w > 0 && *h > 0).unwrap_or((720, 576)),
            decoded: 0,
            failed: 0,
            oversized: 0,
        })
    }

    /// A seek: the decoder forgets the display set it was building.
    pub fn flush(&mut self) {
        self.dec.flush();
    }

    /// Decodes one packet of the stream. `None`: no complete event yet (a PGS segment before its END), a damaged
    /// packet (skipped), or an event too large to keep.
    pub fn decode(&mut self, packet: &Packet, start_time: f64) -> Option<DecodedEvent> {
        let ts = packet.pts().or(packet.dts())?;
        let at = ts as f64 * f64::from(self.tb) - start_time;
        let packet_end = (!self.pgs && packet.duration() > 0).then(|| at + packet.duration() as f64 * f64::from(self.tb));
        let mut sub = SubtitleGuard(codec::subtitle::Subtitle::new());
        match self.dec.decode(packet, &mut sub.0) {
            Ok(true) => {}
            Ok(false) => return None,
            Err(e) => {
                self.failed += 1;
                if self.failed <= 3 {
                    eprintln!("[player] subtitle picture at {at:.3}s not decoded: {e}");
                }
                return None;
            }
        }
        let raw = unsafe { sub.0.as_ptr() };
        let (shown_from, shown_to) = unsafe { ((*raw).start_display_time, (*raw).end_display_time) };
        let start = at + f64::from(shown_from) / 1000.0;
        let end = if shown_to > shown_from && shown_to != u32::MAX { Some(at + f64::from(shown_to) / 1000.0) } else { packet_end };
        let canvas = unsafe {
            let c = self.dec.as_ptr();
            ((*c).width.max(0) as u32, (*c).height.max(0) as u32)
        };
        let canvas = if canvas.0 > 0 && canvas.1 > 0 { canvas } else { self.fallback_canvas };
        let rects = match unsafe { bitmaps(raw) } {
            Ok(r) => r,
            Err(()) => {
                self.oversized += 1;
                eprintln!("[player] subtitle picture at {at:.3}s skipped: larger than {MAX_EVENT_PIXELS} pixels");
                return None;
            }
        };
        self.decoded += 1;
        if rects.is_empty() {
            return Some(DecodedEvent { start, end, picture: None });
        }
        // A canvas too small for its rectangles (a VobSub track without a size) grows to hold them.
        let w = rects.iter().map(|r| r.x + r.width).max().unwrap_or(0).max(canvas.0).min(MAX_SIDE);
        let h = rects.iter().map(|r| r.y + r.height).max().unwrap_or(0).max(canvas.1).min(MAX_SIDE);
        let picture = SubtitlePicture {
            id: NEXT_PICTURE_ID.fetch_add(1, Ordering::Relaxed),
            stream_index: self.stream,
            canvas: (w, h),
            start,
            end,
            rects,
        };
        Some(DecodedEvent { start, end, picture: Some(picture) })
    }
}

/// The bitmap rectangles of a decoded subtitle as straight-alpha RGBA (empty and fully transparent rectangles left
/// out). `Err` when the event is larger than [`MAX_EVENT_PIXELS`] or a side larger than [`MAX_SIDE`].
///
/// # Safety
/// `sub` is a subtitle a decoder filled.
unsafe fn bitmaps(sub: *const ffi::AVSubtitle) -> Result<Vec<SubtitleBitmap>, ()> {
    let mut out = Vec::new();
    let mut pixels = 0u64;
    unsafe {
        for i in 0..(*sub).num_rects as usize {
            let rect = *(*sub).rects.add(i);
            if rect.is_null() || (*rect).type_ != ffi::AVSubtitleType::SUBTITLE_BITMAP {
                continue;
            }
            let r = &*rect;
            if r.w <= 0 || r.h <= 0 || r.data[0].is_null() || r.data[1].is_null() || r.linesize[0] < r.w {
                continue;
            }
            let (w, h) = (r.w as u32, r.h as u32);
            if w > MAX_SIDE || h > MAX_SIDE {
                return Err(());
            }
            pixels += u64::from(w) * u64::from(h);
            if pixels > MAX_EVENT_PIXELS {
                return Err(());
            }
            let colors = r.nb_colors.clamp(0, 256) as usize;
            let palette = std::slice::from_raw_parts(r.data[1] as *const u32, colors);
            let stride = r.linesize[0] as usize;
            let indices = std::slice::from_raw_parts(r.data[0] as *const u8, stride * (h as usize - 1) + w as usize);
            let rgba = pal8_to_rgba(indices, stride, w, h, palette);
            if rgba.chunks_exact(4).all(|p| p[3] == 0) {
                continue;
            }
            out.push(SubtitleBitmap { x: r.x.max(0) as u32, y: r.y.max(0) as u32, width: w, height: h, rgba: Arc::new(rgba) });
        }
    }
    Ok(out)
}

/// PAL8 rows (`stride` bytes apart) through a `0xAARRGGBB` palette into tightly packed straight-alpha RGBA8.
pub(crate) fn pal8_to_rgba(indices: &[u8], stride: usize, w: u32, h: u32, palette: &[u32]) -> Vec<u8> {
    let mut lut = [[0u8; 4]; 256];
    for (entry, argb) in lut.iter_mut().zip(palette) {
        let [b, g, r, a] = argb.to_le_bytes();
        *entry = [r, g, b, a];
    }
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = &indices[y * stride..y * stride + w];
        for (dst, &i) in out[y * w * 4..(y + 1) * w * 4].chunks_exact_mut(4).zip(row) {
            dst.copy_from_slice(&lut[i as usize]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_entries_are_argb_words_with_straight_alpha_and_unknown_indices_are_transparent() {
        // Two rows of three with a stride of four (one padding byte per row).
        let indices = [0, 1, 2, 9, 2, 1, 0, 9];
        let palette = [0x0000_0000, 0xFF10_2030, 0x80FF_0000];
        let rgba = pal8_to_rgba(&indices, 4, 3, 2, &palette);
        assert_eq!(&rgba[0..4], &[0, 0, 0, 0]);
        assert_eq!(&rgba[4..8], &[0x10, 0x20, 0x30, 0xFF]);
        assert_eq!(&rgba[8..12], &[0xFF, 0, 0, 0x80], "half-transparent red keeps its full red");
        assert_eq!(&rgba[12..16], &[0xFF, 0, 0, 0x80]);
        assert_eq!(rgba.len(), 3 * 2 * 4);
        // An index past the palette.
        let rgba = pal8_to_rgba(&[5], 1, 1, 1, &palette);
        assert_eq!(rgba, vec![0, 0, 0, 0]);
    }
}
