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
/// Events queued ahead of (or kept behind) the clock. Past this, events already replaced go first, then the ones
/// farthest ahead; the event on screen (or due next) is never dropped.
pub const MAX_QUEUED_EVENTS: usize = 64;
/// RGBA bytes of the queued pictures, bounded the same way.
pub const MAX_QUEUED_BYTES: usize = 64 << 20;
/// Pixels of one event (all its rectangles): a full UHD canvas. Larger events are dropped as damaged.
pub const MAX_EVENT_PIXELS: u64 = 3840 * 2160;
/// Width or height of one rectangle or of the canvas.
pub const MAX_SIDE: u32 = 8192;

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
    /// The coded size of the video it is shown over, when there is one.
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
    /// `None`: a clearing event (nothing on screen from its start).
    picture: Option<Arc<SubtitlePicture>>,
    bytes: usize,
}

/// The decoded events of the selected picture track, in start order, and which one is on screen.
///
/// Events carry the player's seek serial they were demuxed for; asking with a newer serial drops the older ones, so
/// nothing of the position before a seek is ever shown after it. Within one serial the asked times only move
/// forward (a frame's pts can fall a little behind the clock the previous check used), and events that ended before
/// the newest asked time are dropped.
#[derive(Default)]
pub struct PictureTimeline {
    events: VecDeque<Entry>,
    bytes: usize,
    serial: u64,
    /// The newest time asked for in `serial`.
    asked: f64,
    /// Events dropped because the queue was full (diagnostics).
    pub overflowed: u64,
}

impl PictureTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.bytes = 0;
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

    /// Adds an event demuxed for `serial` (`picture` `None`: it clears the screen from `start`). Events of an older
    /// serial than the newest seen are ignored; a newer serial drops what is queued.
    pub fn push(&mut self, serial: u64, start: f64, end: Option<f64>, picture: Option<Arc<SubtitlePicture>>) {
        if !start.is_finite() || serial < self.serial {
            return;
        }
        self.adopt(serial);
        let end = end.filter(|e| e.is_finite() && *e > start);
        let bytes = picture.as_ref().map_or(0, |p| p.rects.iter().map(|r| r.rgba.len()).sum());
        // After every queued event with a start at or before this one (equal starts: the later decoded replaces).
        let at = self.events.iter().rposition(|e| e.start <= start).map_or(0, |i| i + 1);
        self.events.insert(at, Entry { start, end, picture, bytes });
        self.bytes += bytes;
        while self.events.len() > MAX_QUEUED_EVENTS || (self.bytes > MAX_QUEUED_BYTES && self.events.len() > 1) {
            // Never the event `active_at` would pick now (the one on screen, or the next one due): events it already
            // replaced go first, then the one farthest ahead (which the demuxer reads again after a seek).
            let current = self.events.iter().rposition(|e| e.start <= self.asked).unwrap_or(0);
            let drop = if current > 0 { 0 } else { self.events.len() - 1 };
            if drop == current {
                break;
            }
            if let Some(e) = self.events.remove(drop) {
                self.bytes -= e.bytes;
                self.overflowed += 1;
            }
        }
    }

    /// A restart of decoding that keeps the position (another audio track, another speed, a new video decoder): the
    /// event on screen at the newest asked time carries over to `serial` with its own start and end (so it still
    /// ends when it would have), and everything else queued goes (the restarted demuxer reads it again).
    pub fn carry_over(&mut self, serial: u64) {
        if serial <= self.serial {
            return;
        }
        let asked = self.asked;
        let kept = self.events.iter().rposition(|e| e.start <= asked).and_then(|i| self.events.remove(i));
        self.adopt(serial);
        if let Some(e) = kept {
            self.asked = asked;
            self.bytes = e.bytes;
            self.events.push_back(e);
        }
    }

    fn adopt(&mut self, serial: u64) {
        if serial > self.serial {
            self.serial = serial;
            self.asked = f64::NEG_INFINITY;
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
        // The newest event started by `t`; everything before it has been replaced.
        let i = self.events.iter().rposition(|e| e.start <= t)?;
        for _ in 0..i {
            if let Some(e) = self.events.pop_front() {
                self.bytes -= e.bytes;
            }
        }
        let e = &self.events[0];
        let mut end = e.start + e.end.map_or(OPEN_EVENT_MAX, |end| (end - e.start).min(EVENT_MAX));
        if let Some(next) = self.events.get(1) {
            end = end.min(next.start);
        }
        if t < end { e.picture.clone() } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pic(id: u64) -> Option<Arc<SubtitlePicture>> {
        Some(Arc::new(SubtitlePicture {
            id,
            stream_index: 3,
            canvas: (1920, 1080),
            video_size: None,
            start: 0.0,
            end: None,
            rects: vec![SubtitleBitmap { x: 10, y: 20, width: 2, height: 1, rgba: Arc::new(vec![0; 8]) }],
        }))
    }

    fn shown(t: &mut PictureTimeline, serial: u64, at: f64) -> Option<u64> {
        t.active_at(serial, at).map(|p| p.id)
    }

    #[test]
    fn an_open_event_lasts_until_the_next_one_and_a_clearing_event_ends_it() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, None, pic(1));
        t.push(1, 3.0, None, None);
        t.push(1, 5.0, None, pic(2));
        assert_eq!(shown(&mut t, 1, 0.5), None, "ahead of the clock");
        assert_eq!(shown(&mut t, 1, 1.0), Some(1));
        assert_eq!(shown(&mut t, 1, 2.999), Some(1));
        assert_eq!(shown(&mut t, 1, 3.0), None, "cleared");
        assert_eq!(shown(&mut t, 1, 5.0), Some(2));
        assert_eq!(shown(&mut t, 1, 5.0 + OPEN_EVENT_MAX - 0.01), Some(2));
        assert_eq!(shown(&mut t, 1, 5.0 + OPEN_EVENT_MAX), None, "nothing stays forever");
        assert_eq!(t.len(), 1, "the past is dropped");
    }

    #[test]
    fn an_explicit_end_holds_unless_a_newer_event_replaces_it_earlier() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, Some(2.0), pic(1));
        t.push(1, 4.0, Some(9.0), pic(2));
        t.push(1, 6.0, Some(7.0), pic(3));
        assert_eq!(shown(&mut t, 1, 1.5), Some(1));
        assert_eq!(shown(&mut t, 1, 2.0), None);
        assert_eq!(shown(&mut t, 1, 5.9), Some(2));
        assert_eq!(shown(&mut t, 1, 6.5), Some(3), "a new event replaces the screen");
        assert_eq!(shown(&mut t, 1, 7.5), None, "and does not bring the older one back");
        // Absurd ends are capped.
        t.push(1, 10.0, Some(1e9), pic(4));
        assert_eq!(shown(&mut t, 1, 10.0 + EVENT_MAX + 1.0), None);
    }

    #[test]
    fn events_out_of_order_are_sorted_and_equal_starts_take_the_later_one() {
        let mut t = PictureTimeline::new();
        t.push(1, 4.0, None, pic(2));
        t.push(1, 2.0, None, pic(1));
        t.push(1, 4.0, None, pic(3));
        assert_eq!(shown(&mut t, 1, 3.0), Some(1));
        assert_eq!(shown(&mut t, 1, 4.0), Some(3));
    }

    #[test]
    fn a_newer_serial_drops_everything_older_and_old_pushes_are_ignored() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, None, pic(1));
        assert_eq!(shown(&mut t, 1, 1.5), Some(1));
        assert_eq!(shown(&mut t, 2, 1.5), None, "a seek: nothing of before it");
        t.push(1, 1.0, None, pic(9));
        assert_eq!(shown(&mut t, 2, 1.5), None, "a packet demuxed before the seek is not shown after it");
        t.push(2, 0.5, None, pic(2));
        assert_eq!(shown(&mut t, 2, 0.6), Some(2), "a backward seek starts its own time again");
        assert_eq!(shown(&mut t, 1, 0.6), None, "an older serial asks for nothing");
    }

    #[test]
    fn time_only_moves_forward_within_a_serial() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, None, pic(1));
        t.push(1, 2.0, None, pic(2));
        assert_eq!(shown(&mut t, 1, 2.01), Some(2));
        // A late frame's pts behind the clock: no flicker back to the replaced picture (or to none).
        assert_eq!(shown(&mut t, 1, 1.99), Some(2));
    }

    fn big(id: u64) -> Option<Arc<SubtitlePicture>> {
        Some(Arc::new(SubtitlePicture {
            id,
            stream_index: 3,
            canvas: (3840, 2160),
            video_size: None,
            start: 0.0,
            end: None,
            rects: vec![SubtitleBitmap { x: 0, y: 0, width: 3840, height: 2160, rgba: Arc::new(vec![0; 3840 * 2160 * 4]) }],
        }))
    }

    #[test]
    fn the_queue_is_bounded_by_count_and_bytes_and_drops_the_farthest_ahead() {
        let mut t = PictureTimeline::new();
        for i in 0..(MAX_QUEUED_EVENTS as u64 + 10) {
            t.push(1, 100.0 + i as f64, None, pic(i));
        }
        assert_eq!(t.len(), MAX_QUEUED_EVENTS);
        assert_eq!(t.overflowed, 10);
        assert_eq!(shown(&mut t, 1, 100.5), Some(0), "the next one due was kept");
        assert_eq!(shown(&mut t, 1, 163.5), Some(63));
        assert_eq!(shown(&mut t, 1, 164.5), Some(63), "the last ten were the ones dropped");
        // Pictures of 33 MB: two fit; a third never at the cost of the one on screen.
        let mut t = PictureTimeline::new();
        t.push(1, 0.0, None, big(0));
        assert_eq!(shown(&mut t, 1, 0.5), Some(0));
        for i in 1u32..4 {
            t.push(1, f64::from(i), None, big(u64::from(i)));
            assert!(t.bytes() <= MAX_QUEUED_BYTES, "{}", t.bytes());
            assert_eq!(shown(&mut t, 1, 0.6), Some(0), "the picture on screen stays");
        }
        assert_eq!(t.overflowed, 2, "the ones farthest ahead");
        assert_eq!(shown(&mut t, 1, 1.0), Some(1));
    }

    #[test]
    fn the_picture_on_screen_is_never_the_one_dropped_even_when_replaced_ones_are_queued_before_it() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, None, big(1));
        assert_eq!(shown(&mut t, 1, 2.5), Some(1));
        t.push(1, 3.0, None, big(2));
        // A late event that sorts before the one on screen (already replaced) is the one that goes.
        t.push(1, 0.5, None, big(0));
        assert_eq!(t.overflowed, 1);
        assert_eq!(shown(&mut t, 1, 2.6), Some(1));
        assert_eq!(shown(&mut t, 1, 3.0), Some(2), "and the one ahead stayed");
    }

    #[test]
    fn a_restart_at_the_same_position_carries_the_picture_on_screen_over_with_its_own_end() {
        let mut t = PictureTimeline::new();
        t.push(1, 1.0, Some(4.0), pic(1));
        t.push(1, 5.0, None, pic(2));
        assert_eq!(shown(&mut t, 1, 2.0), Some(1));
        t.carry_over(2);
        assert_eq!(shown(&mut t, 2, 2.0), Some(1), "still on screen after the restart");
        assert_eq!(t.len(), 1, "what lies ahead is demuxed again");
        // The restarted demuxer decodes the same event again (a new picture with the same start) and the next.
        t.push(2, 1.0, Some(4.0), pic(3));
        t.push(2, 5.0, None, pic(2));
        assert_eq!(shown(&mut t, 2, 2.1), Some(3));
        assert_eq!(shown(&mut t, 2, 4.0), None, "its own end still holds");
        assert_eq!(shown(&mut t, 2, 5.0), Some(2));
        // Nothing on screen: nothing carried. An old serial is ignored.
        t.carry_over(1);
        assert_eq!(shown(&mut t, 2, 5.1), Some(2));
        let mut t = PictureTimeline::new();
        t.push(1, 3.0, None, pic(1));
        assert_eq!(shown(&mut t, 1, 2.0), None);
        t.carry_over(2);
        assert!(t.is_empty());
    }

    #[test]
    fn bounds_cover_every_rect() {
        let mut p = (*pic(1).unwrap()).clone();
        p.rects.push(SubtitleBitmap { x: 100, y: 900, width: 50, height: 40, rgba: Arc::new(vec![0; 50 * 40 * 4]) });
        assert_eq!(p.bounds(), (10, 20, 140, 920));
        assert_eq!(p.pixels(), 2 + 2000);
        p.rects.clear();
        assert_eq!(p.bounds(), (0, 0, 0, 0));
    }
}
