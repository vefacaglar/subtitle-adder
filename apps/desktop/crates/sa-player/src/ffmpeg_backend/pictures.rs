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
//! the decoder names none is one made up: 720 × 576 for DVB (its default display), else the video's coded size, never
//! grown to hold the rectangles (one pixel more would change the canvas's fit in the UI); a side is clamped to 8192.
//!
//! XSUB and VobSub packets are kept encoded in a [`PacketStore`] and decoded near the position (see there).

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
    Dvd,
    Other,
}

/// Formats whose packets decode on their own (no state carried from one event to the next), kept encoded in a
/// [`PacketStore`]: XSUB and VobSub. FFmpeg's AVI muxer puts every XSUB chunk of a film with its first seconds of
/// video, and the demuxer never reads them again after a seek; PGS and DVB build on earlier packets and arrive
/// interleaved by time, so they are decoded as they are read.
fn stores_packets(format: Format) -> bool {
    matches!(format, Format::Xsub | Format::Dvd)
}

/// How far ahead of the playback position stored packets are decoded (s): only this window's pictures are held as
/// RGBA, so a film's worth of captions never meets the timeline's byte bound.
pub(crate) const STORE_LOOKAHEAD: f64 = 8.0;
/// Encoded bytes a [`PacketStore`] holds (an XSUB or VobSub caption is a few KB: tens of thousands fit).
pub(crate) const MAX_STORED_BYTES: usize = 64 << 20;

/// One stored packet: its bytes (a VobSub subpicture's fragments joined) and timestamps.
struct Stored {
    /// On the player clock: the XSUB time code's start, or the packet time.
    start: f64,
    data: Vec<u8>,
    pts: Option<i64>,
    dts: Option<i64>,
    duration: i64,
    /// The serial it was last decoded for (each seek or restart decodes what it needs again).
    fed: u64,
}

/// Who handed a packet to the store: the playing demuxer or the scan of the whole stream. Each reads the stream in
/// order, so the fragments without a timestamp that follow a packet belong to the packet the same reader gave last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Demuxer = 0,
    Scan = 1,
}

/// The encoded packets of a stream that decodes packet by packet, in start order, from both the playing demuxer and
/// a scan of the whole stream (`session::scan_pictures`): nothing is lost to a seek or to the timeline's RGBA bound,
/// because a packet is decoded only when it comes within [`STORE_LOOKAHEAD`] of the position, and again after a seek.
#[derive(Default)]
struct PacketStore {
    entries: Vec<Stored>,
    bytes: usize,
    /// The first entry not decoded yet for the current serial (an entry inserted before it moves it back).
    next: usize,
    /// Per [`Source`]: the entry the VobSub fragments without a timestamp that follow belong to, and whether that
    /// reader's packet was a copy of one already stored (its fragments are then skipped).
    building: [Option<(usize, bool)>; 2],
    overflowed: u64,
}

impl PacketStore {
    /// Adds a packet from `source` that starts at `start` (`None`: a fragment of that reader's subpicture before it).
    fn add(&mut self, source: Source, start: Option<f64>, data: &[u8], pts: Option<i64>, dts: Option<i64>, duration: i64) {
        let slot = source as usize;
        let Some(start) = start.filter(|s| s.is_finite()) else {
            if let Some((i, false)) = self.building[slot]
                && self.bytes + data.len() <= MAX_STORED_BYTES
            {
                self.entries[i].data.extend_from_slice(data);
                self.bytes += data.len();
            }
            return;
        };
        let at = self.entries.partition_point(|e| e.start < start);
        // The same packet again (the demuxer after a seek, or the other reader): a copy, or the first fragment of one.
        if let Some(i) = (at..self.entries.len()).take_while(|&i| self.entries[i].start == start).find(|&i| self.entries[i].data.starts_with(data)) {
            self.building[slot] = Some((i, true));
            return;
        }
        if self.bytes + data.len() > MAX_STORED_BYTES {
            self.overflowed += 1;
            self.building[slot] = None;
            return;
        }
        let at = self.entries.partition_point(|e| e.start <= start);
        self.entries.insert(at, Stored { start, data: data.to_vec(), pts, dts, duration, fed: 0 });
        self.bytes += data.len();
        if at < self.next {
            self.next = at;
        }
        // The other reader's entry moved one place on when it lies at or after the insertion.
        for b in self.building.iter_mut().flatten() {
            if b.0 >= at {
                b.0 += 1;
            }
        }
        self.building[slot] = Some((at, false));
    }

    /// A seek to `target`: decoding starts again at the event on screen there (every event replaces the screen, so
    /// the newest one started by `target` is all that can be due).
    fn rewind(&mut self, target: f64) {
        self.next = self.entries.partition_point(|e| e.start <= target).saturating_sub(1);
        self.building = [None; 2];
    }
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
    /// The video is shown a quarter turned.
    video_turned: bool,
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
    /// XSUB / VobSub: the stream's packets, decoded near the position ([`PacketStore`]).
    store: Option<PacketStore>,
    /// Per [`Source`]: packets turned away when stored (an XSUB packet without its time code). They never count
    /// towards giving up while anything else is stored: the scan reads the whole film at once, far from the
    /// position, so a few damaged chunks anywhere would otherwise turn the track off at selection.
    store_rejected: [u64; 2],
    /// Not stored formats (PGS, DVB): the newest packet timestamp handed to the decoder, and after a restart that
    /// keeps the decoder, the timestamp up to which the re-read packets are not fed to it again (it already applied
    /// them, and later sets of the read-ahead too: decoded again against that newer state they would differ).
    fed_until: Option<i64>,
    skip_until: Option<i64>,
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
            ffi::AVCodecID::AV_CODEC_ID_DVD_SUBTITLE => Format::Dvd,
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
            video_turned: turned && video_size.is_some(),
            last_ts: None,
            self_contained: format != Format::Pgs,
            epoch_known: format != Format::Pgs,
            decoded: 0,
            failed: 0,
            oversized: 0,
            decoded_lately: 0,
            failed_lately: 0,
            last_error: None,
            store: stores_packets(format).then(PacketStore::default),
            store_rejected: [0; 2],
            fed_until: None,
            skip_until: None,
        })
    }

    /// Whether the stream's packets are kept and decoded near the position ([`Self::take`], [`Self::feed`]) rather
    /// than decoded as they are read ([`Self::decode`]).
    pub fn stores(&self) -> bool {
        self.store.is_some()
    }

    /// Keeps a packet of a storing stream from `source`. An XSUB packet without its time code is turned away and
    /// counted (`failed`, and per source for [`Self::store_hopeless`]), never towards [`Self::hopeless`].
    pub fn take(&mut self, source: Source, data: &[u8], pts: Option<i64>, dts: Option<i64>, duration: i64, start_time: f64) {
        let tb = f64::from(self.tb);
        let start = match self.format {
            Format::Xsub => match xsub_times(data) {
                Some((from, _)) => Some(from - start_time),
                None => {
                    self.failed += 1;
                    self.store_rejected[source as usize] += 1;
                    self.last_error = Some(PictureTrackFailure::NoTimeCode);
                    return;
                }
            },
            _ => pts.map(|t| t as f64 * tb - start_time),
        };
        if let Some(store) = self.store.as_mut() {
            store.add(source, start, data, pts, dts, duration);
        }
    }

    /// A storing stream with nothing stored although at least [`GIVE_UP_AFTER`] of its packets were turned away (by
    /// one reader: each reads every packet once), once the whole stream was read (`read_all`: the scan ended, or the
    /// demuxer reached the end without one): it can show nothing at all.
    pub fn store_hopeless(&self, read_all: bool) -> bool {
        read_all && self.store.as_ref().is_some_and(|s| s.entries.is_empty()) && self.store_rejected.iter().max().is_some_and(|&n| n >= GIVE_UP_AFTER)
    }

    /// A seek to `target` of a storing stream: what is due there is decoded again.
    pub fn rewind(&mut self, target: f64) {
        if let Some(store) = self.store.as_mut() {
            store.rewind(target);
        }
    }

    /// Decodes the stored packets that start up to `at + STORE_LOOKAHEAD` and were not decoded for `serial` yet
    /// (`at`: the position of the seek being executed, not of one still deferred).
    /// Nothing replaced by `at` is decoded (only the event on screen there): a track chosen, or packets stored,
    /// after a seek far into the file start at the position, not at the store's beginning.
    pub fn feed(&mut self, serial: u64, at: f64, start_time: f64) -> Vec<DecodedEvent> {
        let until = at + STORE_LOOKAHEAD;
        if let Some(store) = self.store.as_mut() {
            let on_screen = store.entries.partition_point(|e| e.start <= at).saturating_sub(1);
            store.next = store.next.max(on_screen);
        }
        let mut out = Vec::new();
        loop {
            let packet = {
                let Some(store) = self.store.as_mut() else { break };
                let Some(e) = store.entries.get_mut(store.next).filter(|e| e.start <= until) else { break };
                store.next += 1;
                if e.fed == serial {
                    continue;
                }
                e.fed = serial;
                let mut packet = Packet::copy(&e.data);
                packet.set_stream(self.stream);
                packet.set_pts(e.pts);
                packet.set_dts(e.dts);
                packet.set_duration(e.duration);
                packet
            };
            if let Some(event) = self.decode(&packet, start_time) {
                out.push(event);
            }
        }
        out
    }

    /// Stored packets turned away because the store was full (diagnostics).
    pub fn store_overflow(&self) -> u64 {
        self.store.as_ref().map_or(0, |s| s.overflowed)
    }

    /// A restart that keeps the position (another speed or audio track, a new video decoder): the decoder keeps what
    /// it built (a PGS epoch's objects and palette, which the re-read stream may not send again before the sets that
    /// use them); only the give-up count starts again. PGS / DVB packets read again from the keyframe up to the newest
    /// one already decoded are not fed again: the decoder applied them, and the read-ahead's later sets too (an
    /// object redefined, a palette changed), so a set decoded again would come out against that newer state; the
    /// events they gave are queued. Stored formats decode packet by packet and are fed again from the store.
    pub fn restart(&mut self) {
        self.decoded_lately = 0;
        self.failed_lately = 0;
        if self.store.is_none() {
            self.skip_until = self.fed_until;
        }
    }

    /// A seek: the decoder forgets the display set it was building, and the give-up count starts again.
    pub fn flush(&mut self) {
        self.dec.flush();
        self.last_ts = None;
        self.fed_until = None;
        self.skip_until = None;
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
        if self.store.is_none() {
            let ts = packet.pts().or(packet.dts());
            if let Some(limit) = self.skip_until {
                // Re-read after a restart in place: already applied (a packet without a timestamp continues one).
                if ts.is_none_or(|t| t <= limit) {
                    return None;
                }
                self.skip_until = None;
            }
            if let Some(t) = ts {
                self.fed_until = Some(self.fed_until.map_or(t, |f| f.max(t)));
            }
        }
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
                // Counted as oversized rather than rejected, and towards giving up like a rejection (for PGS only
                // in a self-contained set, as for rejections).
                self.oversized += 1;
                if self.self_contained {
                    self.failed_lately += 1;
                }
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
            canvas: canvas(given, self.fallback_canvas),
            video_size: self.video_size,
            video_turned: self.video_turned,
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

/// The event's canvas: the decoder's when it gives one, else `fallback`, each side clamped to [`MAX_SIDE`]. A
/// rectangle outside it stays outside (the UI brings the event on screen as a whole): growing the canvas to hold it
/// would rescale every rectangle of the event and could change how the canvas lies over the video.
fn canvas(given: (u32, u32), fallback: (u32, u32)) -> (u32, u32) {
    let (w, h) = if given.0 > 0 && given.1 > 0 { given } else { fallback };
    (w.clamp(1, MAX_SIDE), h.clamp(1, MAX_SIDE))
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
        // Given: kept (a damaged rectangle outside it stays outside).
        assert_eq!(canvas((1920, 1080), (720, 576)), (1920, 1080));
        // Made up: the fallback.
        assert_eq!(canvas((0, 0), (720, 576)), (720, 576));
        assert_eq!(canvas((0, 480), (720, 576)), (720, 576), "a made-up canvas never grows: the fit stays");
        assert_eq!(canvas((16000, 1080), (720, 576)), (MAX_SIDE, 1080), "clamped, not dropped");
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

    fn starts(s: &PacketStore) -> Vec<(f64, Vec<u8>)> {
        s.entries.iter().map(|e| (e.start, e.data.clone())).collect()
    }

    #[test]
    fn the_packet_store_keeps_one_copy_per_packet_whoever_delivers_it() {
        use Source::{Demuxer, Scan};
        let mut s = PacketStore::default();
        // The demuxer's subpicture at 1.0 in two pieces, the scan's copy of it in between: one entry, whole.
        s.add(Demuxer, Some(1.0), b"A1", Some(1), None, 0);
        s.add(Scan, Some(1.0), b"A1", Some(1), None, 0);
        s.add(Demuxer, None, b"A2", None, None, 0);
        s.add(Scan, None, b"A2", None, None, 0);
        assert_eq!(starts(&s), vec![(1.0, b"A1A2".to_vec())], "the scan's copy and its fragment skipped");
        // A whole copy later (a seek re-reads it): still one.
        s.add(Demuxer, Some(1.0), b"A1A2", Some(1), None, 0);
        assert_eq!(s.entries.len(), 1);
        // The scan's new packet does not take the demuxer's next fragment: each reader continues its own.
        s.add(Demuxer, Some(3.0), b"C1", Some(3), None, 0);
        s.add(Scan, Some(2.0), b"B1", Some(2), None, 0);
        s.add(Demuxer, None, b"C2", None, None, 0);
        s.add(Scan, None, b"B2", None, None, 0);
        assert_eq!(starts(&s), vec![(1.0, b"A1A2".to_vec()), (2.0, b"B1B2".to_vec()), (3.0, b"C1C2".to_vec())]);
        assert_eq!(s.bytes, 12);
        // A different packet at the same start is kept too (after the one stored first).
        s.add(Scan, Some(2.0), b"X", Some(2), None, 0);
        assert_eq!(s.entries[2].data, b"X");
    }

    #[test]
    fn the_packet_store_moves_its_reading_point_for_late_packets_and_seeks_and_stays_bounded() {
        use Source::{Demuxer, Scan};
        let mut s = PacketStore::default();
        for (t, d) in [(1.0, b"a"), (2.0, b"b"), (3.0, b"c")] {
            s.add(Demuxer, Some(t), d, None, None, 0);
        }
        s.next = 3;
        // A packet that sorts before what was decoded already: decoding goes back to it.
        s.add(Scan, Some(1.5), b"x", None, None, 0);
        assert_eq!(s.next, 1);
        // A seek: from the event on screen at the target (the newest one started by it).
        s.rewind(2.5);
        assert_eq!(s.next, 2, "the event from 2.0 is the one on screen at 2.5");
        s.rewind(0.5);
        assert_eq!(s.next, 0);
        s.rewind(10.0);
        assert_eq!(s.next, 3);
        // Past the byte bound a packet is turned away and counted; its fragments are not attached anywhere.
        s.bytes = MAX_STORED_BYTES;
        s.add(Demuxer, Some(4.0), b"full", None, None, 0);
        s.add(Demuxer, None, b"more", None, None, 0);
        assert_eq!((s.entries.len(), s.overflowed), (4, 1));
        assert_eq!(s.entries[3].data, b"c");
    }

    /// One PGS segment of a .sup file at `pts` (s).
    fn segment(out: &mut Vec<u8>, pts: f64, kind: u8, body: &[u8]) {
        out.extend_from_slice(b"PG");
        out.extend_from_slice(&((pts * 90_000.0).round() as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.push(kind);
        out.extend_from_slice(&(body.len() as u16).to_be_bytes());
        out.extend_from_slice(body);
    }

    #[test]
    fn a_display_set_split_over_packets_starts_at_its_composition() {
        // A raw .sup (FFmpeg's sup demuxer: one packet per segment) whose epoch start has its PCS at 0.5 and its
        // WDS, PDS, ODS and END at 0.6, as Blu-ray streams can; the set is complete only at the END packet.
        let mut sup = Vec::new();
        segment(&mut sup, 0.5, 0x16, &[1, 64, 0, 240, 0x10, 0, 0, 0x80, 0, 0, 1, 0, 0, 0, 0, 0, 10, 0, 20]);
        segment(&mut sup, 0.6, 0x17, &[1, 0, 0, 10, 0, 20, 0, 2, 0, 1]);
        segment(&mut sup, 0.6, 0x14, &[0, 0, 1, 235, 128, 128, 255]);
        segment(&mut sup, 0.6, 0x15, &[0, 0, 0, 0xC0, 0, 0, 8, 0, 2, 0, 1, 1, 1, 0, 0]);
        segment(&mut sup, 0.6, 0x80, &[]);
        let path = std::env::temp_dir().join(format!("sa-player-split-set-{}.sup", std::process::id()));
        std::fs::write(&path, &sup).expect("sup");
        ffmpeg_next::init().expect("ffmpeg");
        let mut ictx = ffmpeg_next::format::input(&path).expect("sup demuxer");
        let mut track = PictureTrack::open(&ictx, 0, Some((320, 240)), false).expect("pgs decoder");
        let tb = f64::from(ictx.stream(0).expect("stream").time_base());
        let mut times = Vec::new();
        let mut events = Vec::new();
        for (_, packet) in ictx.packets() {
            times.push(packet.pts().map(|t| (t as f64 * tb * 100.0).round() / 100.0));
            events.extend(track.decode(&packet, 0.0));
        }
        drop(ictx);
        let _ = std::fs::remove_file(&path);
        // The premise: the segments reached the decoder in packets of their own times.
        assert_eq!(times, vec![Some(0.5), Some(0.6), Some(0.6), Some(0.6), Some(0.6)]);
        assert_eq!(events.len(), 1, "one event, at the END packet");
        let e = &events[0];
        assert!((e.start - 0.5).abs() < 1e-3, "shown from the PCS, not from the END at 0.6: {}", e.start);
        let p = e.picture.as_ref().expect("a picture");
        assert_eq!((p.rects.len(), p.rects[0].x, p.rects[0].y, p.rects[0].width, p.rects[0].height), (1, 10, 20, 2, 1));
    }

    #[test]
    fn a_restart_in_place_keeps_the_pgs_epoch_and_a_seek_forgets_it() {
        // An epoch start at 0.5 (object 0, palette), then normal sets at 1.0 and 1.5 that show object 0 again.
        let mut sup = Vec::new();
        segment(&mut sup, 0.5, 0x16, &[1, 64, 0, 240, 0x10, 0, 0, 0x80, 0, 0, 1, 0, 0, 0, 0, 0, 10, 0, 20]);
        segment(&mut sup, 0.5, 0x17, &[1, 0, 0, 10, 0, 20, 0, 2, 0, 1]);
        segment(&mut sup, 0.5, 0x14, &[0, 0, 1, 235, 128, 128, 255]);
        segment(&mut sup, 0.5, 0x15, &[0, 0, 0, 0xC0, 0, 0, 8, 0, 2, 0, 1, 1, 1, 0, 0]);
        segment(&mut sup, 0.5, 0x80, &[]);
        for (pts, n) in [(1.0, 1u8), (1.5, 2)] {
            segment(&mut sup, pts, 0x16, &[1, 64, 0, 240, 0x10, 0, n, 0x00, 0, 0, 1, 0, 0, 0, 0, 0, 10, 0, 20]);
            segment(&mut sup, pts, 0x80, &[]);
        }
        let path = std::env::temp_dir().join(format!("sa-player-epoch-restart-{}.sup", std::process::id()));
        std::fs::write(&path, &sup).expect("sup");
        ffmpeg_next::init().expect("ffmpeg");
        let mut ictx = ffmpeg_next::format::input(&path).expect("sup demuxer");
        let mut track = PictureTrack::open(&ictx, 0, Some((320, 240)), false).expect("pgs decoder");
        let packets: Vec<Packet> = ictx.packets().map(|(_, p)| p).collect();
        drop(ictx);
        let _ = std::fs::remove_file(&path);
        assert_eq!(packets.len(), 9);
        let decode = |track: &mut PictureTrack, from: usize, to: usize| -> Vec<DecodedEvent> { packets[from..to].iter().filter_map(|p| track.decode(p, 0.0)).collect() };
        let first = decode(&mut track, 0, 5);
        assert!(first.len() == 1 && first[0].complete);
        // Another speed or audio track: the decoder and what it knows of the epoch stay.
        track.restart();
        let normal = decode(&mut track, 5, 7);
        assert_eq!(normal.len(), 1);
        assert!(normal[0].complete, "the normal set after a restart builds on the epoch it knows");
        assert_eq!(normal[0].picture.as_ref().map(|p| p.rects.len()), Some(1));
        // A seek: the normal set read next is decoded without the epoch as far as the player knows.
        track.flush();
        let after_seek = decode(&mut track, 7, 9);
        assert_eq!(after_seek.len(), 1);
        assert!(!after_seek[0].complete, "after a seek the epoch is unknown until the next epoch start");
    }
}
