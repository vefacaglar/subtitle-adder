//! Embedded text subtitle tracks read out of a media file, for the subtitle importer.
//!
//! A track is read with its own demuxer (not the player's: playback is never touched), every other stream discarded
//! so the demuxer hands over only that track's packets (MP4 reads just those samples; Matroska and the rest still
//! read through the file, FFmpeg has no cue-based subtitle-only read). Each packet goes through FFmpeg's own decoder
//! for the codec (subrip, ass/ssa, mov_text, webvtt, text), which turns every format into ASS dialogue events. Their
//! text is turned into plain text here ([`plain_text`]: override blocks dropped, FFmpeg's escapes undone, drawing
//! events skipped) and written as the app's JSON sentence list, which `sa_core::subtitle_import` reads verbatim (an
//! `.ass` or `.srt` file would lose literal `{…}` there, which both importers strip as markup). Timings are on the
//! player's clock (the container start time subtracted).

use std::ffi::{CStr, c_char};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ffmpeg_next::ffi;
use ffmpeg_next::{Packet, codec};
use sa_core::cancel::CancellationToken;
use sa_core::engine_types::Sentence;

use super::io;
use super::tracks;
use crate::{ExtractLimits, MediaSource, SubtitleKind};

pub use crate::ExtractError;

/// The events of a text track as the app's JSON sentence list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSubtitle {
    /// `[{"id":1,"start":…,"end":…,"text":…,"words":[]}, …]` in start order, millisecond timings.
    pub text: String,
    /// The extension the importer reads `text` as.
    pub extension: &'static str,
    pub events: usize,
}

#[derive(Debug, Clone)]
struct Event {
    start: f64,
    end: Option<f64>,
    order: usize,
    text: String,
}

/// An event without a duration lasts until the next one starts, at most this long.
const OPEN_EVENT_MAX: f64 = 10.0;
/// ... and the last one this long.
const LAST_EVENT_DEFAULT: f64 = 3.0;

/// Frees the `AVSubtitle` a decode filled (ffmpeg-next's wrapper does not).
struct SubtitleGuard(codec::subtitle::Subtitle);

impl Drop for SubtitleGuard {
    fn drop(&mut self) {
        unsafe { ffi::avsubtitle_free(self.0.as_mut_ptr()) };
    }
}

/// Reads text subtitle stream `stream_index` of `path` with the default [`ExtractLimits`]. Blocking (run it off the
/// UI and player threads); `progress` gets 0…1 as the read advances; cancelling `cancel` interrupts it (also a
/// blocking read) with [`ExtractError::Cancelled`].
pub fn extract_text_subtitles(
    path: impl Into<PathBuf>,
    stream_index: usize,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(f64),
) -> Result<ExtractedSubtitle, ExtractError> {
    extract_text_subtitles_with(path, stream_index, ExtractLimits::default(), cancel, progress)
}

/// [`extract_text_subtitles`] with explicit bounds.
pub fn extract_text_subtitles_with(
    path: impl Into<PathBuf>,
    stream_index: usize,
    limits: ExtractLimits,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(f64),
) -> Result<ExtractedSubtitle, ExtractError> {
    let _ = ffmpeg_next::init();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let s = stop.clone();
        cancel.on_cancel(move || s.store(true, Ordering::SeqCst));
    }
    let cancelled = || stop.load(Ordering::SeqCst);
    let mut input = io::open_input(MediaSource::File(path.into()), stop.clone()).map_err(|f| {
        if cancelled() { ExtractError::Cancelled } else { ExtractError::Failed(f.message) }
    })?;
    let literal_blocks;
    let (tb, start_time, duration, mut dec) = {
        let ictx = input.input();
        let Some(stream) = ictx.stream(stream_index) else {
            return Err(ExtractError::Failed(ffmpeg_next::Error::StreamNotFound.to_string()));
        };
        let params = stream.parameters();
        let codec_id = unsafe { (*params.as_ptr()).codec_id };
        if params.medium() != ffmpeg_next::media::Type::Subtitle || tracks::subtitle_kind(codec_id) != SubtitleKind::Text {
            return Err(ExtractError::NotText);
        }
        literal_blocks = !matches!(codec_id, ffi::AVCodecID::AV_CODEC_ID_ASS | ffi::AVCodecID::AV_CODEC_ID_SSA);
        let tb = stream.time_base();
        let mut cctx = codec::context::Context::from_parameters(params).map_err(|e| ExtractError::Failed(e.to_string()))?;
        unsafe { (*cctx.as_mut_ptr()).pkt_timebase = tb.into() };
        let dec = cctx.decoder().subtitle().map_err(|e| ExtractError::Failed(e.to_string()))?;
        let (start_time, duration) = unsafe {
            let p = ictx.as_ptr();
            let st = if (*p).start_time == ffi::AV_NOPTS_VALUE { 0.0 } else { (*p).start_time as f64 / ffi::AV_TIME_BASE as f64 };
            let d = if (*p).duration > 0 && (*p).duration != ffi::AV_NOPTS_VALUE { (*p).duration as f64 / ffi::AV_TIME_BASE as f64 } else { 0.0 };
            (st, d)
        };
        // Only this track's packets come out of the demuxer.
        unsafe {
            let p = ictx.as_mut_ptr();
            for i in 0..(*p).nb_streams as usize {
                let st = *(*p).streams.add(i);
                (*st).discard = if i == stream_index { ffi::AVDiscard::AVDISCARD_DEFAULT } else { ffi::AVDiscard::AVDISCARD_ALL };
            }
        }
        (tb, start_time, duration, dec)
    };
    progress(0.0);
    let mut events: Vec<Event> = Vec::new();
    let mut text_bytes = 0usize;
    let mut reported = 0.0f64;
    loop {
        if cancelled() {
            return Err(ExtractError::Cancelled);
        }
        let mut packet = Packet::empty();
        match packet.read(input.input()) {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Eof) => break,
            Err(ffmpeg_next::Error::Other { errno }) if errno == ffi::EAGAIN => {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            Err(_) if cancelled() => return Err(ExtractError::Cancelled),
            // A damaged tail ends the track there, as playback does.
            Err(e) => {
                eprintln!("[subtitles] read error in stream {stream_index}: {e}");
                break;
            }
        }
        if packet.stream() != stream_index {
            continue;
        }
        let Some(ts) = packet.pts().or(packet.dts()) else { continue };
        let at = ts as f64 * f64::from(tb) - start_time;
        let packet_end = (packet.duration() > 0).then(|| at + packet.duration() as f64 * f64::from(tb));
        let mut sub = SubtitleGuard(codec::subtitle::Subtitle::new());
        match dec.decode(&packet, &mut sub.0) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                // A damaged event: skip it.
                eprintln!("[subtitles] event at {at:.3}s not decoded: {e}");
                continue;
            }
        }
        let (shown_from, shown_to) = unsafe {
            let s = sub.0.as_ptr();
            ((*s).start_display_time, (*s).end_display_time)
        };
        let start = at + f64::from(shown_from) / 1000.0;
        let end = packet_end.or_else(|| (shown_to > shown_from && shown_to != u32::MAX).then(|| at + f64::from(shown_to) / 1000.0));
        for raw in unsafe { rect_texts(sub.0.as_ptr()) } {
            let Some(text) = plain_text(&raw, literal_blocks) else { continue };
            text_bytes += text.len();
            if events.len() >= limits.max_events || text_bytes > limits.max_text_bytes {
                return Err(ExtractError::TooLarge);
            }
            let order = events.len();
            events.push(Event { start, end, order, text });
        }
        if duration > 0.0 {
            let f = (at / duration).clamp(0.0, 1.0);
            if f - reported >= 0.01 {
                reported = f;
                progress(f);
            }
        }
    }
    progress(1.0);
    let n = events.len();
    let text = json_document(events).map_err(ExtractError::Failed)?;
    Ok(ExtractedSubtitle { text, extension: "json", events: n })
}

/// The raw ASS texts of the rectangles of one decoded subtitle: an ASS rectangle's `Text` field (FFmpeg's event line
/// is `ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`), a plain text rectangle with its `{`, `}` and
/// `\` escaped so [`plain_text`] reads both alike. Picture rectangles carry no text.
unsafe fn rect_texts(sub: *const ffi::AVSubtitle) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        for i in 0..(*sub).num_rects as usize {
            let rect = *(*sub).rects.add(i);
            if rect.is_null() {
                continue;
            }
            let c = |p: *mut c_char| if p.is_null() { None } else { Some(CStr::from_ptr(p).to_string_lossy().into_owned()) };
            match (*rect).type_ {
                ffi::AVSubtitleType::SUBTITLE_ASS => {
                    if let Some(line) = c((*rect).ass) {
                        out.push(ass_event_text(&line).to_owned());
                    }
                }
                ffi::AVSubtitleType::SUBTITLE_TEXT => {
                    if let Some(t) = c((*rect).text) {
                        out.push(t.replace('\\', "\\\\").replace('{', "\\{").replace('}', "\\}"));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// The `Text` field of an FFmpeg ASS event line (the 9th comma-separated field; the text itself may hold commas).
fn ass_event_text(line: &str) -> &str {
    line.splitn(9, ',').nth(8).unwrap_or(line).trim_end_matches(['\r', '\n'])
}

/// The plain text of an ASS event's `Text` field: override blocks (`{\i1}`, also empty `{}`) dropped, FFmpeg's
/// escapes of literals undone (`\{`, `\}`, `\\`), `\N` / `\n` / `\h` and line breaks as spaces, white space
/// collapsed; text in drawing mode (`{\p1}` … `{\p0}`: vector paths, not words) dropped. `None` when nothing is left
/// (a drawing event, an empty event).
///
/// A block without any tag (`{b}`) is a comment in an ASS/SSA track and dropped; from the other codecs it is the
/// line's own text (FFmpeg's SubRip decoder passes literal braces through unescaped), so `literal_blocks` keeps it.
pub(crate) fn plain_text(ass: &str, literal_blocks: bool) -> Option<String> {
    let mut out = String::with_capacity(ass.len());
    let mut drawing = false;
    let mut chars = ass.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek().copied() {
                Some(e @ ('{' | '}' | '\\')) => {
                    chars.next();
                    if !drawing {
                        out.push(e);
                    }
                }
                Some('N' | 'n' | 'h') => {
                    chars.next();
                    out.push(' ');
                }
                _ => {
                    if !drawing {
                        out.push('\\');
                    }
                }
            },
            '{' => {
                let mut block = String::new();
                let mut closed = false;
                for b in chars.by_ref() {
                    if b == '}' {
                        closed = true;
                        break;
                    }
                    block.push(b);
                }
                if !closed {
                    // An unterminated brace is text.
                    if !drawing {
                        out.push('{');
                        out.push_str(&block);
                    }
                    break;
                }
                if literal_blocks && !block.is_empty() && !block.contains('\\') {
                    if !drawing {
                        out.push('{');
                        out.push_str(&block);
                        out.push('}');
                    }
                } else if let Some(level) = drawing_level(&block) {
                    drawing = level > 0;
                }
            }
            _ if drawing => {}
            c => out.push(c),
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

/// The last `\p<n>` of an override block (ASS drawing mode; `\pos`, `\pbo` are other tags).
fn drawing_level(block: &str) -> Option<u32> {
    let mut level = None;
    for tag in block.split('\\').skip(1) {
        if let Some(rest) = tag.strip_prefix('p') {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() && digits.len() == rest.trim_end().len() {
                level = digits.parse().ok();
            }
        }
    }
    level
}

/// Milliseconds, as the importer keeps them.
fn ms(t: f64) -> f64 {
    (t.max(0.0) * 1000.0).round() / 1000.0
}

/// Sorts the events by start (decode order for equal starts), gives open ones an end (the next later start, found
/// with one backward pass) and writes the JSON sentence list.
fn json_document(mut events: Vec<Event>) -> Result<String, String> {
    events.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.order.cmp(&b.order)));
    // next_later[i]: the first start after events[i].start (events with equal starts share it).
    let mut next_later: Vec<Option<f64>> = vec![None; events.len()];
    for i in (0..events.len().saturating_sub(1)).rev() {
        next_later[i] = if events[i + 1].start > events[i].start { Some(events[i + 1].start) } else { next_later[i + 1] };
    }
    let sentences: Vec<Sentence> = events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let end = e.end.unwrap_or_else(|| match next_later[i] {
                Some(next) => next.min(e.start + OPEN_EVENT_MAX),
                None => e.start + LAST_EVENT_DEFAULT,
            });
            Sentence::new(i as i64 + 1, ms(e.start), ms(end.max(e.start)), e.text.clone())
        })
        .collect();
    Sentence::encode_list(&sentences).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_text_is_the_ninth_field_and_keeps_its_commas() {
        assert_eq!(ass_event_text("0,0,Default,,0,0,0,,Hello, world"), "Hello, world");
        assert_eq!(ass_event_text("3,0,Top,Bob,10,10,10,fx,{\\i1}Hi{\\i0}\r\n"), "{\\i1}Hi{\\i0}");
    }

    #[test]
    fn plain_text_drops_markup_and_undoes_escapes() {
        assert_eq!(plain_text("{\\i1}Hi{\\i0} there", false).as_deref(), Some("Hi there"));
        assert_eq!(plain_text("a \\{b\\} c", false).as_deref(), Some("a {b} c"));
        assert_eq!(plain_text("back\\\\slash", false).as_deref(), Some("back\\slash"));
        assert_eq!(plain_text("one\\Ntwo\\hthree", false).as_deref(), Some("one two three"));
        assert_eq!(plain_text("empty{} block", false).as_deref(), Some("empty block"));
        assert_eq!(plain_text("{\\an8}  ", false).as_deref(), None);
        assert_eq!(plain_text("{\\pos(10,20)}placed", false).as_deref(), Some("placed"));
        assert_eq!(plain_text("open { brace", false).as_deref(), Some("open { brace"));
        // A tagless block: an ASS comment, or SubRip text that FFmpeg passed through.
        assert_eq!(plain_text("a {TL note} b", false).as_deref(), Some("a b"));
        assert_eq!(plain_text("a {b} c{\\i1}!", true).as_deref(), Some("a {b} c!"));
        assert_eq!(plain_text("empty{} too", true).as_deref(), Some("empty too"));
    }

    #[test]
    fn drawing_events_are_skipped_and_text_after_p0_is_kept() {
        assert_eq!(plain_text("{\\p1}m 0 0 l 100 0 100 100 0 100{\\p0}", false), None);
        assert_eq!(plain_text("{\\an7\\p2}m 0 0 l 10 10", false), None);
        assert_eq!(plain_text("{\\p1}m 0 0 l 1 1{\\p0}Caption", false).as_deref(), Some("Caption"));
        assert_eq!(drawing_level("\\pbo2"), None);
    }

    #[test]
    fn open_events_end_at_the_next_later_start_and_the_document_parses_back() {
        let events = vec![
            Event { start: 5.0, end: None, order: 1, text: "second".into() },
            Event { start: 1.0, end: Some(2.5), order: 0, text: "first, with {braces}".into() },
            Event { start: 5.0, end: None, order: 3, text: "same start".into() },
            Event { start: 30.0, end: None, order: 2, text: "last".into() },
        ];
        let doc = json_document(events).unwrap();
        let lines = sa_core::subtitle_import::parse(&doc, Some("json"));
        let got: Vec<(f64, f64, &str)> = lines.iter().map(|s| (s.start, s.end, s.text.as_str())).collect();
        assert_eq!(
            got,
            vec![(1.0, 2.5, "first, with {braces}"), (5.0, 15.0, "second"), (5.0, 15.0, "same start"), (30.0, 33.0, "last")]
        );
    }

    #[test]
    fn many_events_are_written_in_linear_time() {
        let events: Vec<Event> = (0..50_000).map(|i| Event { start: f64::from(i / 2), end: None, order: i as usize, text: "x".into() }).collect();
        let t = std::time::Instant::now();
        let doc = json_document(events).unwrap();
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        assert!(doc.starts_with("[{\"id\":1,"));
    }
}
