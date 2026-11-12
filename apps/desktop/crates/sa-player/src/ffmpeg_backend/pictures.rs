//! The bitmap subtitle track shown over the video (PGS, VobSub, DVB, XSUB): its decoder, fed on the demux thread with
//! the packets of the selected stream (the player's own demuxer, so the pictures follow playback, seeks and track
//! switches exactly), and the conversion of each decoded event into a [`SubtitlePicture`] for
//! [`crate::picture::PictureTimeline`].
//!
//! FFmpeg's decoders hand over `SUBTITLE_BITMAP` rectangles: PAL8 indices (`data[0]`, `linesize[0]` per row) and a
//! palette of `nb_colors` native-endian `uint32` values `0xAARRGGBB` (`data[1]`, `AV_PIX_FMT_PAL8`'s layout) with
//! straight alpha (pgssubdec / dvdsubdec / dvbsubdec / xsubdec write `RGBA(r, g, b, a)` = `a << 24 | r << 16 | g << 8
//! | b`, colours not multiplied by alpha). They become straight-alpha RGBA8 bytes here; an index past `nb_colors` is
//! transparent.
//!
//! Times: every packet is decoded, also one without a timestamp (VobSub in MPEG-PS splits a subpicture over several
//! PES packets and only the first carries a PTS; dvdsubdec puts them back together). An event's time is
//! `AVSubtitle.pts` (`AV_TIME_BASE`; FFmpeg fills it from the packet when `pkt_timebase` is set, and pgssubdec sets it
//! to its presentation composition's time, so a display set is shown from its PCS, not from the packet that completed
//! it), else the newest packet timestamp seen, plus `start_display_time`. XSUB events carry their own times as text
//! (`[HH:MM:SS.mmm-HH:MM:SS.mmm]`, on the stream's timeline) and are timed by that alone: xsubdec reads the packet pts
//! as if it were in `AV_TIME_BASE`, and AVI (XSUB's container) numbers its subtitle packets 0, 1, 2… The end is
//! `end_display_time` when the decoder gives one, else (not for PGS, whose events end with the next display set) the
//! packet duration, else none (the timeline then ends it at the next event).
//!
//! The canvas is the decoder's (`width` × `height`: the PGS composition, the VobSub idx `size:`, the DVB display
//! definition) as given, even when a damaged rectangle lies outside it (the UI keeps rectangles on screen); only when
//! the decoder names none is one made up: 720 × 576 for DVB (its default display), else the video's coded size, grown
//! to hold the rectangles.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ffmpeg_next::ffi;
use ffmpeg_next::{Packet, Rational, codec};

use super::tracks;
use crate::SubtitleKind;
use crate::picture::{MAX_EVENT_PIXELS, MAX_SIDE, PictureTrackFailure, SubtitleBitmap, SubtitlePicture};

static NEXT_PICTURE_ID: AtomicU64 = AtomicU64::new(1);

/// One decoded event: a picture, or a clearing event (`picture` `None`).
pub(crate) struct DecodedEvent {
    pub start: f64,
    pub end: Option<f64>,
    pub picture: Option<SubtitlePicture>,
    /// Decoded with all the earlier state it builds on (always, except a PGS display set decoded before any epoch
    /// start or acquisition point since the decoder was opened or flushed: its objects or palette may be missing).
    /// `PictureTimeline::push` never lets an incomplete event replace a complete one at the same time.
    pub complete: bool,
}

/// Frees the `AVSubtitle` a decode filled (ffmpeg-next's wrapper does not).
struct SubtitleGuard(codec::subtitle::Subtitle);

impl Drop for SubtitleGuard {
    fn drop(&mut self) {
        unsafe { ffi::avsubtitle_free(self.0.as_mut_ptr()) };
    }
}

/// The formats whose timing or canvas needs a rule of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Pgs,
    Dvb,
    Xsub,
    Other,
}

/// Rejected packets after which a track that decoded nothing since it was chosen or last seeked is given up
/// (`SubtitleTrackFailed`). Only rejections a healthy stream cannot cause count: for PGS, those of display sets that
/// carry everything they need (an epoch start or an acquisition point), never those of normal sets, which build on
/// an earlier set of their epoch that a seek or a mid-epoch selection skipped.
pub(crate) const GIVE_UP_AFTER: u64 = 4;

/// The decoder of the bitmap subtitle stream being shown.
pub(crate) struct PictureTrack {
    pub stream: usize,
    dec: codec::decoder::Subtitle,
    tb: Rational,
    format: Format,
    /// The canvas when the decoder names none.
    fallback_canvas: (u32, u32),
    /// The video's coded size in the orientation it is shown (handed on with every picture).
    video_size: Option<(u32, u32)>,
    /// The timestamp (stream time base) and duration of the newest packet that carried one.
    last_ts: Option<(i64, i64)>,
    /// PGS: the display set being decoded started with an epoch start or an acquisition point (needs nothing of
    /// before it).
    self_contained: bool,
    /// PGS: such a set was decoded since the decoder was opened or flushed (the sets after it have what they need).
    epoch_known: bool,
    /// Events decoded / packets the decoder rejected / events dropped as too large (diagnostics, the whole track).
    pub decoded: u64,
    pub failed: u64,
    pub oversized: u64,
    /// Since the track was chosen or last seeked: events decoded, and rejections that count towards giving up.
    decoded_lately: u64,
    failed_lately: u64,
    /// Why the decoder last failed.
    pub last_error: Option<PictureTrackFailure>,
}

impl PictureTrack {
    /// Opens the decoder of picture stream `stream` (codec parameters with extradata: the VobSub palette and size
    /// live there; the packet time base set). `video_size`: the video's coded size; `turned`: it is shown a quarter
    /// turn round (the pictures then carry the size with width and height swapped, so that the UI never takes a
    /// canvas of the coded size for the picture's own raster).
    pub fn open(ictx: &ffmpeg_next::format::context::Input, stream: usize, video_size: Option<(u32, u32)>, turned: bool) -> Result<Self, String> {
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
        let format = match codec_id {
            ffi::AVCodecID::AV_CODEC_ID_HDMV_PGS_SUBTITLE => Format::Pgs,
            ffi::AVCodecID::AV_CODEC_ID_DVB_SUBTITLE => Format::Dvb,
            ffi::AVCodecID::AV_CODEC_ID_XSUB => Format::Xsub,
            _ => Format::Other,
        };
        let video_size = video_size.filter(|(w, h)| *w > 0 && *h > 0);
        Ok(Self {
            stream,
            dec,
            tb,
            format,
            fallback_canvas: fallback_canvas(format, video_size),
            video_size: video_size.map(|(w, h)| if turned { (h, w) } else { (w, h) }),
            last_ts: None,
            self_contained: format != Format::Pgs,
            epoch_known: format != Format::Pgs,
            decoded: 0,
            failed: 0,
            oversized: 0,
            decoded_lately: 0,
            failed_lately: 0,
            last_error: None,
        })
    }

    /// A seek: the decoder forgets the display set it was building, and the give-up count starts again.
    pub fn flush(&mut self) {
        self.dec.flush();
        self.last_ts = None;
        self.self_contained = self.format != Format::Pgs;
        self.epoch_known = self.format != Format::Pgs;
        self.decoded_lately = 0;
        self.failed_lately = 0;
    }

    /// Whether the track decoded nothing since it was chosen or last seeked and the decoder rejected enough packets
    /// a healthy stream would not have to give it up ([`GIVE_UP_AFTER`]).
    pub fn hopeless(&self) -> bool {
        self.decoded_lately == 0 && self.failed_lately >= GIVE_UP_AFTER
    }

    /// A packet or event that failed: counted, and towards giving up only when the stream could not have caused it
    /// by being joined mid-epoch.
    fn reject(&mut self, why: PictureTrackFailure) {
        self.failed += 1;
        if self.self_contained {
            self.failed_lately += 1;
        }
        self.last_error = Some(why);
    }

    /// Decodes one packet of the stream. `None`: no complete event yet (a PGS segment before its END, a VobSub
    /// fragment), a damaged packet (skipped), or an event too large to keep.
    pub fn decode(&mut self, packet: &Packet, start_time: f64) -> Option<DecodedEvent> {
        if let Some(ts) = packet.pts().or(packet.dts()) {
            self.last_ts = Some((ts, packet.duration()));
        }
        if self.format == Format::Pgs
            && let Some(state) = packet.data().and_then(pgs_composition_state)
        {
            // 0x80 epoch start, 0x40 acquisition point, 0x00 normal (builds on the epoch's earlier sets).
            self.self_contained = state & 0xC0 != 0;
        }
        let mut sub = SubtitleGuard(codec::subtitle::Subtitle::new());
        match self.dec.decode(packet, &mut sub.0) {
            Ok(true) => {}
            Ok(false) => return None,
            Err(e) => {
                if self.failed < 3 {
                    eprintln!("[player] subtitle packet of stream {} not decoded: {e}", self.stream);
                }
                self.reject(PictureTrackFailure::Ffmpeg(e.to_string()));
                return None;
            }
        }
        if self.self_contained {
            self.epoch_known = true;
        }
        let complete = self.epoch_known;
        let raw = unsafe { sub.0.as_ptr() };
        let (pts, shown_from, shown_to) = unsafe { ((*raw).pts, (*raw).start_display_time, (*raw).end_display_time) };
        let tb = f64::from(self.tb);
        let (start, end) = match self.format {
            Format::Xsub => {
                let Some((from, to)) = packet.data().and_then(xsub_times) else {
                    self.reject(PictureTrackFailure::NoTimeCode);
                    return None;
                };
                (from - start_time, Some(to - start_time))
            }
            _ => {
                let base = if pts != ffi::AV_NOPTS_VALUE {
                    pts as f64 / f64::from(ffi::AV_TIME_BASE) - start_time
                } else if let Some((ts, _)) = self.last_ts {
                    ts as f64 * tb - start_time
                } else {
                    // Nothing says when (no timestamp seen yet): not shown at a made-up time.
                    return None;
                };
                let start = base + f64::from(shown_from) / 1000.0;
                let end = if shown_to > shown_from && shown_to != u32::MAX {
                    Some(base + f64::from(shown_to) / 1000.0)
                } else if self.format != Format::Pgs {
                    self.last_ts.filter(|(_, d)| *d > 0).map(|(_, d)| base + d as f64 * tb)
                } else {
                    None
                };
                (start, end)
            }
        };
        let given = unsafe {
            let c = self.dec.as_ptr();
            ((*c).width.max(0) as u32, (*c).height.max(0) as u32)
        };
        let rects = match unsafe { bitmaps(raw) } {
            Ok(r) => r,
            Err(()) => {
                // Counted as oversized rather than rejected, and towards giving up like a rejection.
                self.oversized += 1;
                self.failed_lately += 1;
                self.last_error = Some(PictureTrackFailure::TooLarge);
                eprintln!("[player] subtitle picture at {start:.3}s skipped: larger than {MAX_EVENT_PIXELS} pixels or {MAX_SIDE} on a side");
                return None;
            }
        };
        self.decoded += 1;
        self.decoded_lately += 1;
        if rects.is_empty() {
            return Some(DecodedEvent { start, end, picture: None, complete });
        }
        let picture = SubtitlePicture {
            id: NEXT_PICTURE_ID.fetch_add(1, Ordering::Relaxed),
            stream_index: self.stream,
            canvas: canvas(given, self.fallback_canvas, &rects),
            video_size: self.video_size,
            start,
            end,
            rects,
        };
        Some(DecodedEvent { start, end, picture: Some(picture), complete })
    }
}

/// The composition state byte of the PGS presentation composition segment `data` starts with, if it does (a
/// display set's first packet: segment type 0x16, its 2-byte length, then width, height, frame rate, composition
/// number and the state).
fn pgs_composition_state(data: &[u8]) -> Option<u8> {
    (data.len() > 10 && data[0] == 0x16).then(|| data[10])
}

/// One event of [`decode_picture_stream`]: start, end and picture (`None`: it clears the screen).
pub type DecodedPictureEvent = (f64, Option<f64>, Option<SubtitlePicture>);

/// Tests: decodes bitmap subtitle stream `stream` of `path` the way the player does, with every packet cut into
/// `fragments` pieces of which only the first keeps the timestamps and the duration (what an MPEG-PS demuxer without
/// FFmpeg's dvdsub parser would hand over for a VobSub subpicture spread over several PES packets).
#[doc(hidden)]
pub fn decode_picture_stream(path: &std::path::Path, stream: usize, fragments: usize) -> Result<Vec<DecodedPictureEvent>, String> {
    ffmpeg_next::init().map_err(|e| e.to_string())?;
    let mut ictx = ffmpeg_next::format::input(path).map_err(|e| e.to_string())?;
    let start_time = unsafe {
        let p = ictx.as_ptr();
        if (*p).start_time == ffi::AV_NOPTS_VALUE { 0.0 } else { (*p).start_time as f64 / f64::from(ffi::AV_TIME_BASE) }
    };
    let video = ictx.streams().best(ffmpeg_next::media::Type::Video).map(|s| unsafe {
        let p = s.parameters();
        ((*p.as_ptr()).width.max(0) as u32, (*p.as_ptr()).height.max(0) as u32)
    });
    let mut track = PictureTrack::open(&ictx, stream, video, false)?;
    let mut out = Vec::new();
    for (s, packet) in ictx.packets() {
        if s.index() != stream {
            continue;
        }
        let data = packet.data().unwrap_or_default();
        let n = fragments.clamp(1, data.len().max(1));
        let size = data.len().div_ceil(n).max(1);
        for (i, chunk) in data.chunks(size).enumerate() {
            let mut piece = Packet::copy(chunk);
            piece.set_stream(stream);
            if i == 0 {
                piece.set_pts(packet.pts());
                piece.set_dts(packet.dts());
                piece.set_duration(packet.duration());
            }
            if let Some(e) = track.decode(&piece, start_time) {
                out.push((e.start, e.end, e.picture));
            }
        }
    }
    Ok(out)
}

/// The canvas made up when the decoder names none: DVB's default display, else the video's coded size.
fn fallback_canvas(format: Format, video_size: Option<(u32, u32)>) -> (u32, u32) {
    match (format, video_size) {
        (Format::Dvb, _) | (_, None) => (720, 576),
        (_, Some(size)) => size,
    }
}

/// The event's canvas: the decoder's when it gives one (rectangles outside it stay outside), else `fallback` grown to
/// hold every rectangle.
fn canvas(given: (u32, u32), fallback: (u32, u32), rects: &[SubtitleBitmap]) -> (u32, u32) {
    if given.0 > 0 && given.1 > 0 {
        return (given.0.min(MAX_SIDE), given.1.min(MAX_SIDE));
    }
    let w = rects.iter().map(|r| r.x.saturating_add(r.width)).max().unwrap_or(0).max(fallback.0).min(MAX_SIDE);
    let h = rects.iter().map(|r| r.y.saturating_add(r.height)).max().unwrap_or(0).max(fallback.1).min(MAX_SIDE);
    (w, h)
}

/// The start and end (s) of an XSUB packet: `[HH:MM:SS.mmm-HH:MM:SS.mmm]` at its start.
fn xsub_times(data: &[u8]) -> Option<(f64, f64)> {
    fn time(s: &[u8]) -> Option<f64> {
        let s = std::str::from_utf8(s).ok()?;
        let (h, rest) = s.split_once(':')?;
        let (m, rest) = rest.split_once(':')?;
        let (sec, ms) = rest.split_once('.')?;
        let n = |v: &str| v.parse::<u32>().ok().map(f64::from);
        Some(n(h)? * 3600.0 + n(m)? * 60.0 + n(sec)? + n(ms)? / 1000.0)
    }
    if data.len() < 27 || data[0] != b'[' || data[13] != b'-' || data[26] != b']' {
        return None;
    }
    Some((time(&data[1..13])?, time(&data[14..26])?))
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
        if (*sub).rects.is_null() {
            return Ok(out);
        }
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

    /// A rectangle as a decoder fills one: `indices` (rows `linesize` apart) through `palette`.
    struct Rect {
        rect: Box<ffi::AVSubtitleRect>,
        _indices: Vec<u8>,
        _palette: Vec<u32>,
    }

    fn rect(x: i32, y: i32, w: i32, h: i32, linesize: i32, index: u8, palette: &[u32]) -> Rect {
        let mut indices = vec![index; (linesize.max(1) * h.max(1)) as usize];
        let mut palette = palette.to_vec();
        let mut r: Box<ffi::AVSubtitleRect> = Box::new(unsafe { std::mem::zeroed() });
        r.type_ = ffi::AVSubtitleType::SUBTITLE_BITMAP;
        (r.x, r.y, r.w, r.h, r.nb_colors) = (x, y, w, h, palette.len() as i32);
        r.linesize[0] = linesize;
        r.data[0] = indices.as_mut_ptr();
        r.data[1] = palette.as_mut_ptr().cast();
        Rect { rect: r, _indices: indices, _palette: palette }
    }

    /// `bitmaps` over a subtitle holding `rects` (a null entry where `None`).
    fn decode_rects(rects: &mut [Option<&mut Rect>]) -> Result<Vec<SubtitleBitmap>, ()> {
        let mut ptrs: Vec<*mut ffi::AVSubtitleRect> =
            rects.iter_mut().map(|r| r.as_mut().map_or(std::ptr::null_mut(), |r| &mut *r.rect as *mut _)).collect();
        let mut sub: ffi::AVSubtitle = unsafe { std::mem::zeroed() };
        sub.num_rects = ptrs.len() as u32;
        sub.rects = ptrs.as_mut_ptr();
        unsafe { bitmaps(&sub) }
    }

    const OPAQUE: [u32; 2] = [0, 0xFFFF_FFFF];

    #[test]
    fn rectangles_a_decoder_could_not_have_meant_are_left_out_and_oversized_events_refused() {
        let mut good = rect(4, 6, 3, 2, 8, 1, &OPAQUE);
        let out = decode_rects(&mut [Some(&mut good)]).unwrap();
        assert_eq!((out.len(), out[0].x, out[0].y, out[0].width, out[0].height), (1, 4, 6, 3, 2));
        assert_eq!(out[0].pixel(2, 1), [255, 255, 255, 255], "the row stride is honoured");
        // Not a bitmap, null data, a stride shorter than a row, an empty size, a null entry, fully transparent.
        let mut text = rect(0, 0, 3, 2, 3, 1, &OPAQUE);
        text.rect.type_ = ffi::AVSubtitleType::SUBTITLE_ASS;
        let mut no_pixels = rect(0, 0, 3, 2, 3, 1, &OPAQUE);
        no_pixels.rect.data[0] = std::ptr::null_mut();
        let mut no_palette = rect(0, 0, 3, 2, 3, 1, &OPAQUE);
        no_palette.rect.data[1] = std::ptr::null_mut();
        let mut short_rows = rect(0, 0, 3, 2, 2, 1, &OPAQUE);
        let mut empty = rect(0, 0, 0, 2, 3, 1, &OPAQUE);
        let mut clear = rect(0, 0, 3, 2, 3, 0, &OPAQUE);
        let mut good = rect(1, 1, 1, 1, 1, 1, &OPAQUE);
        let out = decode_rects(&mut [
            Some(&mut text),
            Some(&mut no_pixels),
            Some(&mut no_palette),
            Some(&mut short_rows),
            Some(&mut empty),
            None,
            Some(&mut clear),
            Some(&mut good),
        ])
        .unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!((out[0].x, out[0].y), (1, 1));
        // A negative position is clamped to the canvas edge.
        let mut left = rect(-5, -2, 1, 1, 1, 1, &OPAQUE);
        let out = decode_rects(&mut [Some(&mut left)]).unwrap();
        assert_eq!((out[0].x, out[0].y), (0, 0));
        // A side over MAX_SIDE, or more pixels in one event than MAX_EVENT_PIXELS (over two rectangles): refused.
        let mut wide = rect(0, 0, MAX_SIDE as i32 + 1, 1, MAX_SIDE as i32 + 1, 1, &OPAQUE);
        assert!(decode_rects(&mut [Some(&mut wide)]).is_err());
        let half = (MAX_EVENT_PIXELS / 2 / 2000) as i32 + 1;
        let mut a = rect(0, 0, 2000, half, 2000, 1, &OPAQUE);
        let mut b = rect(0, 0, 2000, half, 2000, 1, &OPAQUE);
        assert!(decode_rects(&mut [Some(&mut a)]).is_ok(), "one half fits");
        assert!(decode_rects(&mut [Some(&mut a), Some(&mut b)]).is_err(), "both do not");
        // No rectangle array at all.
        let mut sub: ffi::AVSubtitle = unsafe { std::mem::zeroed() };
        sub.num_rects = 3;
        assert!(unsafe { bitmaps(&sub) }.unwrap().is_empty());
    }

    #[test]
    fn the_decoders_canvas_is_kept_and_only_a_missing_one_is_made_up() {
        let r = |x, y, width, height| SubtitleBitmap { x, y, width, height, rgba: Arc::new(Vec::new()) };
        // Given: kept, even with a damaged rectangle outside it.
        assert_eq!(canvas((1920, 1080), (720, 576), &[r(1800, 1000, 400, 200)]), (1920, 1080));
        // Made up: the fallback, grown to hold the rectangles.
        assert_eq!(canvas((0, 0), (720, 576), &[r(10, 10, 20, 20)]), (720, 576));
        assert_eq!(canvas((0, 480), (720, 576), &[r(700, 500, 100, 100)]), (800, 600));
        assert_eq!(fallback_canvas(Format::Dvb, Some((1920, 1080))), (720, 576), "DVB's default display");
        assert_eq!(fallback_canvas(Format::Other, Some((1920, 1080))), (1920, 1080));
        assert_eq!(fallback_canvas(Format::Other, None), (720, 576));
    }

    #[test]
    fn xsub_time_codes_are_read_from_the_packet() {
        let t = xsub_times(b"[00:01:02.500-01:00:00.001]rest").unwrap();
        assert!((t.0 - 62.5).abs() < 1e-9 && (t.1 - 3600.001).abs() < 1e-9, "{t:?}");
        assert_eq!(xsub_times(b"[00:01:02.500-01:00:00.001"), None);
        assert_eq!(xsub_times(b"(00:01:02.500-01:00:00.001]"), None);
        assert_eq!(xsub_times(b"[00:01:0x.500-01:00:00.001]"), None);
    }
}
