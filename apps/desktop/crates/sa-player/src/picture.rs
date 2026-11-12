//! Bitmap (picture) subtitles shown over the video: the decoded pictures ([`SubtitlePicture`]) and the timeline that
//! says which one is on screen at a time ([`PictureTimeline`]). Pure, so the rules are unit-tested without FFmpeg;
//! the FFmpeg side (`ffmpeg_backend::pictures`) decodes PGS / VobSub / DVB / XSUB packets into these.
//!
//! Every bitmap subtitle format describes the whole screen with each event: a new event replaces the one before
//! (a PGS display set without objects, an empty DVB page, a VobSub stop command clear it). So the picture on screen
//! at time `t` is the newest event that started at or before `t`, while `t` is before its end: its own end when it has
//! one, the next event's start, and never more than [`OPEN_EVENT_MAX`] after its start (a PGS event has no end; its
//! clearing event may be lost, a seek may have skipped it, or the track may simply never send one).

use std::collections::VecDeque;
use std::sync::Arc;

/// An event without an end (PGS: always) is shown at most this long when no later event comes.
pub const OPEN_EVENT_MAX: f64 = 30.0;
/// Any event is shown at most this long, whatever end it claims.
pub const EVENT_MAX: f64 = 300.0;
/// The real bound of the queued events is their RGBA bytes: past it, events already replaced go first (oldest
/// first), then the ones farthest ahead; the event on screen (or due next) is never dropped.
pub const MAX_QUEUED_BYTES: usize = 64 << 20;
/// A sanity limit on the number of queued events (clearing events weigh nothing), bounded the same way. High enough
/// for a whole film's worth of small pictures: AVI files can put every XSUB packet at the front of the file, where
/// the demuxer reads them all at once and never again after a seek.
pub const MAX_QUEUED_EVENTS: usize = 4096;
/// Pixels of one event (all its rectangles): a full UHD canvas. Larger events are dropped as damaged.
pub const MAX_EVENT_PIXELS: u64 = 3840 * 2160;
/// Width or height of one rectangle or of the canvas.
pub const MAX_SIDE: u32 = 8192;

/// Why a bitmap subtitle track cannot be shown (`PlayerEvent::SubtitleTrackFailed`); the UI words it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PictureTrackFailure {
    /// FFmpeg's own error text (the stream is no picture track, its decoder did not open or rejected the packets).
    Ffmpeg(String),
    /// XSUB packets without the time code they must start with.
    NoTimeCode,
    /// Pictures larger than the player keeps ([`MAX_EVENT_PIXELS`], [`MAX_SIDE`]).
    TooLarge,
}

/// One rectangle of a picture: straight (not premultiplied) RGBA8, `width * 4` bytes per row, at `(x, y)` on the
/// event's canvas.
#[derive(Clone, PartialEq, Eq)]
pub struct SubtitleBitmap {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

impl std::fmt::Debug for SubtitleBitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SubtitleBitmap({}x{} at {},{})", self.width, self.height, self.x, self.y)
    }
}

impl SubtitleBitmap {
    /// The RGBA bytes of pixel `(x, y)` of this rectangle.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }
}

/// A subtitle event's pictures, as shown: every rectangle positioned on the `canvas` (the subtitle's own coordinate
/// space: PGS / VobSub / DVB give it, else it is made up from the video's coded size; how it lies over the video is
/// the UI's rule, `sa-app`'s `main_window::pictures`), shown from `start` (s, on the player clock).
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitlePicture {
    /// Unique per player (a new id for every event, also for equal pictures).
    pub id: u64,
    /// The subtitle stream it came from.
    pub stream_index: usize,
    pub canvas: (u32, u32),
    /// The coded size of the video it is shown over, when there is one, in the orientation it is shown (width and
    /// height swapped for a video turned a quarter round).
    pub video_size: Option<(u32, u32)>,
    pub start: f64,
    /// The event's own end (s), when it has one.
    pub end: Option<f64>,
    pub rects: Vec<SubtitleBitmap>,
}

impl SubtitlePicture {
    /// Pixels of every rectangle.
    pub fn pixels(&self) -> u64 {
        self.rects.iter().map(|r| u64::from(r.width) * u64::from(r.height)).sum()
    }

    /// The bounding box of the rectangles on the canvas, `(x, y, w, h)` (zero for none): what `[player-stats]`
    /// reports as the shown size.
    pub fn bounds(&self) -> (u32, u32, u32, u32) {
        let Some(first) = self.rects.first() else { return (0, 0, 0, 0) };
        let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x + first.width, first.y + first.height);
        for r in &self.rects[1..] {
            x0 = x0.min(r.x);
            y0 = y0.min(r.y);
            x1 = x1.max(r.x + r.width);
            y1 = y1.max(r.y + r.height);
        }
        (x0, y0, x1 - x0, y1 - y0)
    }
}

struct Entry {
    start: f64,
    end: Option<f64>,
    /// The end as decoded (`end` can be capped by a carry-over, `seek`): what a copy decoded again is compared by.
    decoded_end: Option<f64>,
    /// `None`: a clearing event (nothing on screen from its start).
    picture: Option<Arc<SubtitlePicture>>,
    /// Decoded with all the earlier state it builds on (see `push`).
    complete: bool,
    bytes: usize,
}

impl Entry {
    /// The same event decoded again: same times and the same pictures.
    fn same_as(&self, end: Option<f64>, picture: &Option<Arc<SubtitlePicture>>) -> bool {
        self.decoded_end == end
            && match (&self.picture, picture) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b) || (a.canvas == b.canvas && a.rects == b.rects),
                _ => false,
            }
    }
}

/// The decoded events of the selected picture track, in start order, and which one is on screen.
///
/// The queue holds what the demuxer delivered: events ahead of the clock and, as long as they fit, the ones already
/// replaced. It knows the screen exactly from its first event's start up to `known_until`, the newest start of a
/// contiguous run of deliveries: the demuxer reads a stream in order, so no event between two delivered ones is
/// missing.
///
/// Events carry the player's seek serial they were demuxed for; events of an older serial than the newest seen are
/// ignored. A seek or a restart of decoding moves the timeline to a new serial when it is issued ([`Self::seek`]):
/// the events stay only while the queue knows the screen at the new position (their own times then say what is due
/// there), so nothing of the old position is ever shown at the new one, and events the demuxer will not deliver
/// again (packets before the point it resumes reading from) are not lost. Within one serial the asked times only
/// move forward (a frame's pts can fall a little behind the clock the previous check used).
pub struct PictureTimeline {
    events: VecDeque<Entry>,
    bytes: usize,
    serial: u64,
    /// The newest time asked for in `serial`.
    asked: f64,
    /// The newest start of the contiguous deliveries the queue holds (`NEG_INFINITY`: none).
    known_until: f64,
    /// An event ahead was dropped (queue full): `known_until` stays below it until the queue is cleared.
    capped: bool,
    /// The target of the seek that started `serial` (`NEG_INFINITY`: none): until the first ask of the serial,
    /// eviction protects what is due there.
    floor: f64,
    /// Events ahead of the position lost because the queue was full (removed, or turned into a clearing event);
    /// replaced events it lets go of to stay in its bound are not counted (diagnostics).
    pub overflowed: u64,
    /// Pictures carried over a restart in place (diagnostics, `seek`).
    pub carried: u64,
}

impl Default for PictureTimeline {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            bytes: 0,
            serial: 0,
            asked: f64::NEG_INFINITY,
            known_until: f64::NEG_INFINITY,
            capped: false,
            floor: f64::NEG_INFINITY,
            overflowed: 0,
            carried: 0,
        }
    }
}

impl PictureTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets every event (another track, or none).
    pub fn clear(&mut self) {
        self.events.clear();
        self.bytes = 0;
        self.known_until = f64::NEG_INFINITY;
        self.capped = false;
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Queued RGBA bytes.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The serial the timeline is at.
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Whether the queue knows what is on screen at `t`.
    fn knows(&self, t: f64) -> bool {
        self.events.front().is_some_and(|e| e.start <= t) && t <= self.known_until
    }

    /// A seek to `target` (or a restart of decoding at the same position: another audio track or speed, a new video
    /// decoder) issued as `serial`. The queue stays when it knows the screen at `target`; otherwise it goes, except
    /// that a restart in place (`in_place`, straight after the serial before it) keeps the picture on screen at the
    /// newest asked time when it is also due at `target`: the restarted demuxer reads only from the keyframe before
    /// `target` and might not deliver it again. Either way, what is due at `target` is the event whose window holds
    /// it, exactly as before the seek.
    pub fn seek(&mut self, serial: u64, target: f64, in_place: bool) {
        if serial <= self.serial {
            return;
        }
        let follows = serial == self.serial + 1;
        let asked = self.asked;        self.serial = serial;
        self.asked = f64::NEG_INFINITY;
        self.floor = if target.is_finite() { target } else { f64::NEG_INFINITY };
        if target.is_finite() && self.knows(target) {
            return;
        }
        let kept = (in_place && follows && asked.is_finite() && target.is_finite())
            .then(|| self.window_at(asked))
            .flatten()
            .filter(|&(i, end)| self.events[i].start <= target && target < end);
        let kept = kept.and_then(|(i, end)| self.events.remove(i).map(|e| (e, end)));
        self.clear();
        if let Some((mut e, end)) = kept {
            // Its window ended where the next event started: that event is gone, so the end is its own now.
            if e.end.is_none_or(|own| own > end) {
                e.end = Some(end);
            }
            self.bytes = e.bytes;
            self.known_until = e.start;
            self.events.push_back(e);
            self.carried += 1;
        }
    }

    /// The index of the event due at `t` and the end of its window (its own end, capped, or the next start).
    fn window_at(&self, t: f64) -> Option<(usize, f64)> {
        let i = self.events.partition_point(|e| e.start <= t).checked_sub(1)?;
        let e = &self.events[i];
        let mut end = e.start + e.end.map_or(OPEN_EVENT_MAX, |end| (end - e.start).min(EVENT_MAX));
        if let Some(next) = self.events.get(i + 1) {
            end = end.min(next.start);
        }
        Some((i, end))
    }

    /// Adds an event demuxed for `serial` (`picture` `None`: it clears the screen from `start`). Events of an older
    /// serial than the newest seen are ignored; a newer serial drops what is queued. At a start already queued the
    /// same event decoded again is ignored (the queued picture keeps its identity), and a different one replaces
    /// it, except that an incomplete one (`complete` false: a PGS set decoded without the epoch it builds on) never
    /// replaces a complete one.
    pub fn push(&mut self, serial: u64, start: f64, end: Option<f64>, picture: Option<Arc<SubtitlePicture>>, complete: bool) {
        if !start.is_finite() || serial < self.serial {
            return;
        }
        self.adopt(serial);
        let end = end.filter(|e| e.is_finite() && *e > start);
        let bytes = picture.as_ref().map_or(0, |p| p.rects.iter().map(|r| r.rgba.len()).sum());
        let at = self.events.partition_point(|e| e.start < start);
        if let Some(old) = self.events.get_mut(at).filter(|e| e.start == start) {
            if old.same_as(end, &picture) || (old.complete && !complete) {
                old.complete |= complete;
                return;
            }
            self.bytes = self.bytes - old.bytes + bytes;
            *old = Entry { start, end, decoded_end: end, picture, complete, bytes };
        } else {
            self.events.insert(at, Entry { start, end, decoded_end: end, picture, complete, bytes });
            self.bytes += bytes;
        }
        if start > self.known_until && !self.capped {
            self.known_until = start;
        }
        self.evict();
    }

    /// Bounds the queue. Judged from the newest asked time, or, before the first ask after a seek, from the seek's
    /// target: never the event due there (on screen, or due next). Replaced events go first (oldest first), then the
    /// one farthest ahead. Over the byte bound a picture ahead is not removed but turned into a clearing event at
    /// its start (zero bytes), so the caption before it ends there instead of standing in for it; after that the
    /// queue claims to know the screen only up to the last event it kept whole.
    fn evict(&mut self) {
        let at = if self.asked.is_finite() { self.asked } else { self.floor };
        loop {
            let over_count = self.events.len() > MAX_QUEUED_EVENTS;
            if !over_count && self.bytes <= MAX_QUEUED_BYTES {
                break;
            }
            let current = self.events.partition_point(|e| e.start <= at).saturating_sub(1);
            let replaced = (0..current).find(|&i| over_count || self.events[i].bytes > 0);
            if let Some(i) = replaced {
                let e = self.events.remove(i).expect("index in range");
                self.bytes -= e.bytes;
                continue;
            }
            let ahead = if over_count {
                (self.events.len() > current + 1).then(|| self.events.len() - 1)
            } else {
                (current + 1..self.events.len()).rev().find(|&i| self.events[i].bytes > 0)
            };
            let Some(i) = ahead else { break };
            self.overflowed += 1;
            self.capped = true;
            if over_count {
                let e = self.events.remove(i).expect("index in range");
                self.bytes -= e.bytes;
            } else {
                let e = &mut self.events[i];
                self.bytes -= e.bytes;
                e.bytes = 0;
                e.picture = None;
                e.end = None;
            }
            // Up to the event before it: from its start on, what is due is lost.
            let before = if i > 0 { self.events[i - 1].start } else { f64::NEG_INFINITY };
            self.known_until = self.known_until.min(before);
        }
    }

    fn adopt(&mut self, serial: u64) {
        if serial > self.serial {
            self.serial = serial;
            self.asked = f64::NEG_INFINITY;
            self.floor = f64::NEG_INFINITY;
            self.clear();
        }
    }

    /// The picture on screen at `t` for the player's current seek `serial`.
    pub fn active_at(&mut self, serial: u64, t: f64) -> Option<Arc<SubtitlePicture>> {
        if serial < self.serial {
            return None;
        }
        self.adopt(serial);
        if t.is_finite() && t > self.asked {
            self.asked = t;
        }
        let t = self.asked;
        let (i, end) = self.window_at(t)?;
        if t < end { self.events[i].picture.clone() } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small picture; its rectangle sits at `x = id`, so pictures of different ids differ.
    fn pic(id: u64) -> Option<Arc<SubtitlePicture>> {
        Some(Arc::new(SubtitlePicture {
            id,
            stream_index: 3,
            canvas: (1920, 1080),
            video_size: None,
            start: 0.0,
            end: None,
            rects: vec![SubtitleBitmap { x: id as u32, y: 20, width: 2, height: 1, rgba: Arc::new(vec![0; 8]) }],
        }))
    }

    /// The same event as `pic(like)`, decoded again: a new id, equal pictures.
    fn again(id: u64, like: u64) -> Option<Arc<SubtitlePicture>> {
        let mut p = (*pic(like).unwrap()).clone();
        p.id = id;
        Some(Arc::new(p))
    }

    fn shown(t: &mut PictureTimeline, serial: u64, at: f64) -> Option<u64> {
        t.active_at(serial, at).map(|p| p.id)
    }

    #[test]
    fn an_open_event_lasts_until_the_next_one_and_a_clearing_event_ends_it() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, None, pic(1), true);
        t.push(1, 3.0, None, None, true);
        t.push(1, 5.0, None, pic(2), true);
        assert_eq!(shown(&mut t, 1, 0.5), None, "ahead of the clock");
        assert_eq!(shown(&mut t, 1, 1.0), Some(1));
        assert_eq!(shown(&mut t, 1, 2.999), Some(1));
        assert_eq!(shown(&mut t, 1, 3.0), None, "cleared");
        assert_eq!(shown(&mut t, 1, 5.0), Some(2));
        assert_eq!(shown(&mut t, 1, 5.0 + OPEN_EVENT_MAX - 0.01), Some(2));
        assert_eq!(shown(&mut t, 1, 5.0 + OPEN_EVENT_MAX), None, "nothing stays forever");
        assert_eq!(t.len(), 3, "replaced events stay while they fit (a seek back may need them)");
    }

    #[test]
    fn an_explicit_end_holds_unless_a_newer_event_replaces_it_earlier() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, Some(2.0), pic(1), true);
        t.push(1, 4.0, Some(9.0), pic(2), true);
        t.push(1, 6.0, Some(7.0), pic(3), true);
        assert_eq!(shown(&mut t, 1, 1.5), Some(1));
        assert_eq!(shown(&mut t, 1, 2.0), None);
        assert_eq!(shown(&mut t, 1, 5.9), Some(2));
        assert_eq!(shown(&mut t, 1, 6.5), Some(3), "a new event replaces the screen");
        assert_eq!(shown(&mut t, 1, 7.5), None, "and does not bring the older one back");
        // Absurd ends are capped.
        t.push(1, 10.0, Some(1e9), pic(4), true);
        assert_eq!(shown(&mut t, 1, 10.0 + EVENT_MAX + 1.0), None);
    }

    #[test]
    fn events_out_of_order_are_sorted_and_a_different_event_at_the_same_start_replaces() {
        let mut t = PictureTimeline::new();
        t.push(1, 4.0, None, pic(2), true);
        t.push(1, 2.0, None, pic(1), true);
        t.push(1, 4.0, None, pic(3), true);
        assert_eq!(shown(&mut t, 1, 3.0), Some(1));
        assert_eq!(shown(&mut t, 1, 4.0), Some(3));
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn the_same_event_decoded_again_keeps_its_identity_and_an_incomplete_one_never_replaces_a_complete_one() {
        let mut t = PictureTimeline::new();
        t.push(1, 2.0, None, pic(1), true);
        assert_eq!(shown(&mut t, 1, 2.5), Some(1));
        t.push(1, 2.0, None, again(7, 1), true);
        assert_eq!(shown(&mut t, 1, 2.6), Some(1), "re-delivered after a restart: the same picture stays");
        // A PGS normal set decoded without its epoch: missing objects (here: none at all).
        t.push(1, 2.0, None, None, false);
        t.push(1, 2.0, None, pic(8), false);
        assert_eq!(shown(&mut t, 1, 2.7), Some(1));
        assert_eq!((t.len(), t.bytes()), (1, 8));
        // An incomplete event is replaced by the complete decode of the same set.
        t.push(1, 4.0, None, None, false);
        t.push(1, 4.0, None, pic(9), true);
        assert_eq!(shown(&mut t, 1, 4.0), Some(9));
    }

    #[test]
    fn a_newer_serial_drops_everything_older_and_old_pushes_are_ignored() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, None, pic(1), true);
        assert_eq!(shown(&mut t, 1, 1.5), Some(1));
        assert_eq!(shown(&mut t, 2, 1.5), None, "a serial nobody announced: nothing of before it");
        t.push(1, 1.0, None, pic(9), true);
        assert_eq!(shown(&mut t, 2, 1.5), None, "a packet demuxed before the seek is not shown after it");
        t.push(2, 0.5, None, pic(2), true);
        assert_eq!(shown(&mut t, 2, 0.6), Some(2), "a backward seek starts its own time again");
        assert_eq!(shown(&mut t, 1, 0.6), None, "an older serial asks for nothing");
    }

    #[test]
    fn time_only_moves_forward_within_a_serial() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, None, pic(1), true);
        t.push(1, 2.0, None, pic(2), true);
        assert_eq!(shown(&mut t, 1, 2.01), Some(2));
        // A late frame's pts behind the clock: no flicker back to the replaced picture (or to none).
        assert_eq!(shown(&mut t, 1, 1.99), Some(2));
    }

    #[test]
    fn a_seek_keeps_the_events_only_where_the_queue_knows_the_screen() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, Some(3.0), pic(1), true);
        t.push(1, 5.0, None, pic(2), true);
        t.push(1, 8.0, None, None, true);
        assert_eq!(shown(&mut t, 1, 6.0), Some(2));
        // Back to 2.0 (known: from 1.0 up to 8.0): the events stay and their own times say what is due.
        t.seek(2, 2.0, false);
        assert_eq!(t.serial(), 2);
        assert_eq!(shown(&mut t, 2, 2.0), Some(1), "the earlier event, even if the demuxer never delivers it again");
        assert_eq!(shown(&mut t, 2, 3.5), None);
        assert_eq!(shown(&mut t, 2, 5.5), Some(2));
        // To 20.0, past the last delivery: something unknown may start in between, so nothing stays.
        t.seek(3, 20.0, false);
        assert!(t.is_empty());
        assert_eq!(shown(&mut t, 3, 20.0), None);
        // Before the first event: what was on screen there is unknown.
        let mut t = PictureTimeline::new();
        t.push(1, 4.0, None, pic(1), true);
        t.push(1, 6.0, None, pic(2), true);
        t.seek(2, 3.0, false);
        assert!(t.is_empty());
        // An old serial does nothing.
        t.push(2, 4.0, None, pic(3), true);
        t.seek(2, 4.0, false);
        assert_eq!(shown(&mut t, 2, 4.5), Some(3));
    }

    #[test]
    fn a_seek_never_lets_a_restart_carry_a_picture_that_misses_the_new_position() {
        // On screen at 10.0, nothing known after it: a seek to 40.0, then a restart (another speed, an audio
        // track) before the seek's first frame was asked for.
        let mut t = PictureTimeline::new();
        t.push(1, 10.0, None, pic(1), true);
        assert_eq!(shown(&mut t, 1, 12.0), Some(1));
        t.seek(2, 40.0, false);
        t.seek(3, 40.0, true);
        assert!(t.is_empty(), "nothing asked in serial 2: no picture of serial 1 can come back");
        assert_eq!(shown(&mut t, 3, 40.0), None);
        // The same while paused: several restarts in a row after the seek.
        let mut t = PictureTimeline::new();
        t.push(1, 10.0, None, pic(1), true);
        assert_eq!(shown(&mut t, 1, 12.0), Some(1));
        t.seek(2, 14.0, false);
        t.seek(3, 14.0, true);
        t.seek(4, 14.0, true);
        assert!(t.is_empty());
        // A restart that skips a serial (a seek in between it never saw) carries nothing either.
        let mut t = PictureTimeline::new();
        t.push(1, 10.0, None, pic(1), true);
        assert_eq!(shown(&mut t, 1, 12.0), Some(1));
        t.seek(3, 12.0, true);
        assert!(t.is_empty());
    }

    #[test]
    fn a_restart_in_place_carries_the_picture_on_screen_over_with_its_window() {
        // Past the last delivery (12.0 > 10.0): the picture on screen carries over, and only it.
        let mut t = PictureTimeline::new();
        t.push(1, 10.0, None, pic(1), true);
        assert_eq!(shown(&mut t, 1, 12.0), Some(1));
        t.seek(2, 12.0, true);
        assert_eq!(t.len(), 1);
        assert_eq!(shown(&mut t, 2, 12.0), Some(1), "still on screen after the restart");
        // The restarted demuxer delivers it again when the keyframe lies before it: the carried one, capped to its
        // window, stays (no new identity, no change announced).
        t.push(2, 10.0, None, again(6, 1), true);
        assert_eq!(shown(&mut t, 2, 12.5), Some(1), "the carried picture keeps its identity");
        assert_eq!(t.len(), 1);
        assert_eq!(shown(&mut t, 2, 10.0 + OPEN_EVENT_MAX), None, "its own end still holds");
        // Not due at the restart position: not carried.
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, Some(4.0), pic(1), true);
        assert_eq!(shown(&mut t, 1, 3.9), Some(1));
        t.seek(2, 4.5, true);
        assert!(t.is_empty());
        // Nothing asked yet in the serial before (nothing on screen yet): nothing carried.
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, None, pic(1), true);
        t.seek(2, 2.0, true);
        assert!(t.is_empty());
        // Where the queue knows the screen, a restart keeps it all, and the picture keeps its identity.
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, None, pic(1), true);
        t.push(1, 3.0, None, pic(2), true);
        assert_eq!(shown(&mut t, 1, 2.0), Some(1));
        t.seek(2, 2.0, true);
        assert_eq!(t.len(), 2);
        assert_eq!(shown(&mut t, 2, 2.0), Some(1));
        t.push(2, 3.0, None, again(5, 2), true);
        assert_eq!(shown(&mut t, 2, 3.0), Some(2), "the re-delivered event is the queued one");
    }

    fn big(id: u64) -> Option<Arc<SubtitlePicture>> {
        Some(Arc::new(SubtitlePicture {
            id,
            stream_index: 3,
            canvas: (3840, 2160),
            video_size: None,
            start: 0.0,
            end: None,
            rects: vec![SubtitleBitmap { x: id as u32, y: 0, width: 3840, height: 2160, rgba: Arc::new(vec![0; 3840 * 2160 * 4]) }],
        }))
    }

    #[test]
    fn the_queue_is_bounded_by_bytes_with_a_count_limit_and_drops_replaced_events_first() {
        // Small pictures: thousands fit (a whole film of XSUB events read at once from the front of an AVI).
        let mut t = PictureTimeline::new();
        for i in 0..2000u64 {
            t.push(1, i as f64, None, pic(i), true);
        }
        assert_eq!((t.len(), t.overflowed), (2000, 0));
        assert_eq!(shown(&mut t, 1, 1500.5), Some(1500));
        // Past the count limit, replaced events go first.
        for i in 2000..(MAX_QUEUED_EVENTS as u64 + 10) {
            t.push(1, i as f64, None, pic(i), true);
        }
        assert_eq!((t.len(), t.overflowed), (MAX_QUEUED_EVENTS, 0), "replaced ones let go of: nothing ahead lost");
        assert_eq!(shown(&mut t, 1, (MAX_QUEUED_EVENTS + 9) as f64), Some(MAX_QUEUED_EVENTS as u64 + 9), "the newest stayed");
        // Pictures of 33 MB: two fit; a third never at the cost of the one on screen.
        let mut t = PictureTimeline::new();
        t.push(1, 0.0, None, big(0), true);
        assert_eq!(shown(&mut t, 1, 0.5), Some(0));
        for i in 1u32..4 {
            t.push(1, f64::from(i), None, big(u64::from(i)), true);
            assert!(t.bytes() <= MAX_QUEUED_BYTES, "{}", t.bytes());
            assert_eq!(shown(&mut t, 1, 0.6), Some(0), "the picture on screen stays");
        }
        assert_eq!(t.overflowed, 2, "the ones farthest ahead");
        assert_eq!(shown(&mut t, 1, 1.0), Some(1));
        // An event ahead was dropped (3.0): the queue no longer claims to know the screen past what it holds.
        t.seek(2, 2.5, false);
        assert!(t.is_empty(), "2.5 lies past the last event kept (1.0), where the dropped one may matter");
        // Later on the replaced one goes first.
        let mut t = PictureTimeline::new();
        t.push(1, 0.0, None, big(0), true);
        t.push(1, 1.0, None, big(1), true);
        assert_eq!(shown(&mut t, 1, 1.5), Some(1));
        t.push(1, 2.0, None, big(2), true);
        assert_eq!((t.len(), t.overflowed), (2, 0), "the replaced one went: nothing ahead lost");
        assert_eq!(shown(&mut t, 1, 1.6), Some(1));
        assert_eq!(shown(&mut t, 1, 2.0), Some(2));
    }

    #[test]
    fn the_picture_on_screen_is_never_the_one_dropped_even_when_replaced_ones_are_queued_before_it() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, None, big(1), true);
        assert_eq!(shown(&mut t, 1, 2.5), Some(1));
        t.push(1, 3.0, None, big(2), true);
        // A late event that sorts before the one on screen (already replaced) is the one that goes.
        t.push(1, 0.5, None, big(0), true);
        assert_eq!((t.len(), t.overflowed), (2, 0));
        assert_eq!(shown(&mut t, 1, 2.6), Some(1));
        assert_eq!(shown(&mut t, 1, 3.0), Some(2), "and the one ahead stayed");
    }

    #[test]
    fn a_time_read_before_a_seek_carries_the_old_serial_and_cannot_pin_the_new_one() {
        // Events at 22:00, 25:00 and 30:00; on screen at 30:00. A seek to 25:00 keeps the queue.
        let mut t = PictureTimeline::new();
        t.push(1, 1320.0, None, pic(1), true);
        t.push(1, 1500.0, None, pic(2), true);
        t.push(1, 1800.0, None, pic(3), true);
        assert_eq!(shown(&mut t, 1, 1800.0), Some(3));
        t.seek(2, 1500.0, false);
        assert_eq!(t.len(), 3);
        // A reader that took the clock before the seek moved it asks with the serial it read then: ignored.
        assert_eq!(shown(&mut t, 1, 1800.0), None);
        assert_eq!(shown(&mut t, 2, 1500.0), Some(2), "the new position's picture, not the old one");
        assert_eq!(shown(&mut t, 2, 1500.5), Some(2));
    }

    #[test]
    fn after_a_seek_eviction_protects_what_is_due_at_the_target_until_the_first_ask() {
        // 33 MB pictures: two fit. Played to 1.5 (0 and 1 queued, 1 on screen), then a seek to 1.0 keeps the queue.
        let mut t = PictureTimeline::new();
        t.push(1, 0.0, None, big(0), true);
        t.push(1, 1.0, None, big(1), true);
        assert_eq!(shown(&mut t, 1, 1.5), Some(1));
        t.seek(2, 1.0, false);
        assert_eq!(t.len(), 2);
        // Before the first frame of the seek, the demuxer delivers the next event: the replaced one before the
        // target goes, not the new one.
        t.push(2, 2.0, None, big(2), true);
        assert_eq!((t.len(), t.overflowed), (2, 0));
        assert_eq!(shown(&mut t, 2, 1.0), Some(1));
        assert_eq!(shown(&mut t, 2, 2.0), Some(2), "delivered after the seek, kept");
    }

    #[test]
    fn a_picture_turned_away_by_the_byte_bound_ends_the_one_before_it_instead_of_standing_in() {
        let mut t = PictureTimeline::new();
        t.push(1, 0.0, None, big(0), true);
        assert_eq!(shown(&mut t, 1, 0.5), Some(0));
        t.push(1, 1.0, None, big(1), true);
        // No room for a third, ahead of the clock: it becomes a clearing event at its start.
        t.push(1, 2.0, None, big(2), true);
        assert_eq!(t.overflowed, 1);
        assert!(t.bytes() <= MAX_QUEUED_BYTES);
        assert_eq!(shown(&mut t, 1, 1.5), Some(1));
        assert_eq!(shown(&mut t, 1, 2.5), None, "not the caption from 1.0 in its place");
        // The queue no longer claims to know the screen from there on.
        t.seek(2, 2.5, false);
        assert!(t.is_empty());
    }

    #[test]
    fn bounds_cover_every_rect() {
        let mut p = (*pic(1).unwrap()).clone();
        p.rects.push(SubtitleBitmap { x: 100, y: 900, width: 50, height: 40, rgba: Arc::new(vec![0; 50 * 40 * 4]) });
        assert_eq!(p.bounds(), (1, 20, 149, 920));
        assert_eq!(p.pixels(), 2 + 2000);
        p.rects.clear();
        assert_eq!(p.bounds(), (0, 0, 0, 0));
    }
}
