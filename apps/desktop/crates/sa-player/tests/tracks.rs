//! Audio and subtitle tracks: enumeration, audio track switching (played into a virtual output device, so the audio
//! clock is master and what is heard can be checked) and reading embedded text subtitle tracks.

#![cfg(feature = "ffmpeg")]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{Recorder, custom_clip, fixtures_dir, is_opened, wait_until};
use sa_core::cancel::CancellationToken;
use sa_player::ffmpeg_backend::{AudioTap, ExtractError, FfmpegPlayer, PlayerOptions, extract_text_subtitles};
use sa_player::{MediaPlayerBackend, MediaSource, PlayerEvent, SubtitleKind};

const T: Duration = Duration::from_secs(10);
const LOW: f64 = 440.0;
const HIGH: f64 = 1000.0;

const SRT: &str = "1\n00:00:00,500 --> 00:00:01,250\nHello <i>there</i>\n\n2\n00:00:01,500 --> 00:00:02,750\nSecond line,\nwith a comma\n";
const ASS: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 240\n\n[V4+ Styles]\n\
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n\n[Events]\n\
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\an8}Sign one\n\
Dialogue: 0,0:00:02.50,0:00:03.20,Default,,0,0,0,,Two\\Nlines\n";

fn subtitle_inputs() -> (PathBuf, PathBuf) {
    let dir = fixtures_dir();
    let (srt, ass) = (dir.join("tracks-v1.srt"), dir.join("tracks-v1.ass"));
    if !srt.exists() {
        std::fs::write(&srt, SRT).expect("srt");
    }
    if !ass.exists() {
        std::fs::write(&ass, ASS).expect("ass");
    }
    (srt, ass)
}

/// 4 s of video, audio 1 = 440 Hz stereo 48 kHz (`eng`, `Main`), audio 2 = 1 kHz mono 44.1 kHz (`tur`,
/// `Commentary`), subtitle 3 = subrip (`eng`), subtitle 4 = ass (`tur`, `Signs`).
fn multi_track_mkv() -> Option<PathBuf> {
    let (srt, ass) = subtitle_inputs();
    let (srt, ass) = (srt.to_string_lossy().into_owned(), ass.to_string_lossy().into_owned());
    custom_clip(
        "tracks-v1.mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
            "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=44100",
            "-i", &srt, "-i", &ass,
            "-t", "4", "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:s", "-map", "4:s",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p",
            "-c:a", "aac", "-ac:a:0", "2", "-ac:a:1", "1", "-c:s:0", "srt", "-c:s:1", "ass",
            "-metadata:s:a:0", "language=eng", "-metadata:s:a:0", "title=Main",
            "-metadata:s:a:1", "language=tur", "-metadata:s:a:1", "title=Commentary",
            "-metadata:s:s:0", "language=eng", "-metadata:s:s:1", "language=tur", "-metadata:s:s:1", "title=Signs",
        ],
    )
}

/// The same two tones without a picture (Matroska audio).
fn audio_only_mka() -> Option<PathBuf> {
    custom_clip(
        "tracks-audio-v1.mka",
        &[
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
            "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=44100",
            "-t", "4", "-map", "0:a", "-map", "1:a", "-c:a", "pcm_s16le",
        ],
    )
}

/// An MP4 with a mov_text track made from the SRT.
fn mov_text_mp4() -> Option<PathBuf> {
    let (srt, _) = subtitle_inputs();
    let srt = srt.to_string_lossy().into_owned();
    custom_clip(
        "tracks-movtext-v1.mp4",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=10", "-i", &srt, "-t", "3", "-map", "0:v", "-map", "1:s",
            "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:s", "mov_text",
            "-metadata:s:s:0", "language=eng",
        ],
    )
}

macro_rules! or_skip {
    ($e:expr) => {
        match $e {
            Some(p) => p,
            None => {
                eprintln!("skipped: no ffmpeg CLI or encoder to generate the test clip");
                return;
            }
        }
    };
}

struct Rig {
    p: FfmpegPlayer,
    rec: Recorder,
    tap: AudioTap,
    g: i32,
}

fn open(path: PathBuf, setup: impl FnOnce(&mut FfmpegPlayer)) -> Rig {
    let rec = Recorder::default();
    let tap = AudioTap::new(48_000, 2);
    let mut p = FfmpegPlayer::with_options(PlayerOptions { audio_output: false, ..PlayerOptions::default() });
    p.use_audio_tap(Some(tap.clone()));
    setup(&mut p);
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    assert!(rec.wait_for(0, T, is_opened(g)).is_some(), "{:?}", rec.snapshot());
    assert!(rec.wait_for(0, T, |e| matches!(e, PlayerEvent::TracksChanged { .. })).is_some());
    Rig { p, rec, tap, g }
}

impl Rig {
    /// Asks for `stream` and waits for the `TracksChanged` that answers it.
    fn select(&mut self, stream: usize) -> Vec<PlayerEvent> {
        let from = self.rec.len();
        self.p.select_audio_track(stream);
        assert!(self.rec.wait_for(from, T, |e| matches!(e, PlayerEvent::TracksChanged { .. })).is_some(), "no answer to the switch");
        self.rec.snapshot()[from..].to_vec()
    }

    /// The tone heard between media times `from` and `to` (Hz, from zero crossings of channel 0); waits until the
    /// device played past `to`.
    fn tone(&self, from: f64, to: f64) -> f64 {
        assert!(wait_until(T, || self.tap.blocks().iter().any(|b| b.pts > to)), "nothing played past {to}: at {}", self.p.position());
        let mut samples: Vec<f32> = Vec::new();
        for b in self.tap.blocks() {
            if b.pts >= from && b.pts < to {
                samples.extend(b.samples.iter().step_by(2));
            }
        }
        assert!(samples.len() > 4800, "too little audio between {from} and {to}: {}", samples.len());
        let crossings = samples.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        crossings as f64 / 2.0 / (samples.len() as f64 / 48_000.0)
    }

    fn frame_near(&mut self, t: f64) -> bool {
        let deadline = Instant::now() + T;
        while Instant::now() < deadline {
            if let Some(f) = self.p.latest_frame()
                && (f.pts - t).abs() < 0.05
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }
}

fn near(hz: f64, want: f64) -> bool {
    (hz - want).abs() < want * 0.05
}

#[test]
fn tracks_list_every_audio_and_subtitle_stream_with_its_tags() {
    let path = or_skip!(multi_track_mkv());
    let r = open(path, |_| {});
    let t = r.p.tracks();
    assert_eq!(t.audio.len(), 2, "{t:?}");
    let (a, b) = (&t.audio[0], &t.audio[1]);
    assert_eq!((a.stream_index, a.language.as_deref(), a.title.as_deref(), a.codec.as_str()), (1, Some("eng"), Some("Main"), "aac"));
    assert_eq!((a.channels, a.sample_rate, a.channel_layout.as_deref()), (2, 48_000, Some("stereo")));
    assert_eq!((b.stream_index, b.language.as_deref(), b.title.as_deref()), (2, Some("tur"), Some("Commentary")));
    assert_eq!((b.channels, b.sample_rate, b.channel_layout.as_deref()), (1, 44_100, Some("mono")));
    assert_eq!(t.active_audio, Some(1), "the default (best) stream plays");
    assert_eq!(t.subtitles.len(), 2);
    let (s, u) = (&t.subtitles[0], &t.subtitles[1]);
    assert_eq!((s.stream_index, s.codec.as_str(), s.kind, s.language.as_deref()), (3, "subrip", SubtitleKind::Text, Some("eng")));
    assert_eq!((u.stream_index, u.codec.as_str(), u.kind, u.language.as_deref(), u.title.as_deref()), (4, "ass", SubtitleKind::Text, Some("tur"), Some("Signs")));
    assert!(!s.forced && !u.forced);
}

#[test]
fn switching_audio_while_playing_keeps_the_position_and_sync_and_changes_what_is_heard() {
    let path = or_skip!(multi_track_mkv());
    let mut r = open(path, |_| {});
    assert!(r.p.has_audio_output(), "the virtual device is the master clock");
    r.p.play();
    assert!(wait_until(T, || r.p.position() > 1.0));
    assert!(near(r.tone(0.3, 0.9), LOW), "first track: {}", r.tone(0.3, 0.9));
    let before = r.p.position();
    let events = r.select(2);
    assert!(!events.iter().any(|e| matches!(e, PlayerEvent::AudioTrackFailed { .. })), "{events:?}");
    assert_eq!(r.p.tracks().active_audio, Some(2));
    let after = r.p.position();
    assert!((after - before).abs() < 0.25, "the position is kept: {before} → {after}");
    assert!(r.p.is_playing());
    // The new track is heard from shortly after the switch on (44.1 kHz mono resampled to the device's 48 kHz stereo).
    let at = after + 0.35;
    assert!(near(r.tone(at, at + 0.6), HIGH), "second track: {} Hz", r.tone(at, at + 0.6));
    // A/V sync: the picture follows the audio clock.
    let mut worst = 0f64;
    let mut n = 0;
    let deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < deadline {
        if let Some(f) = r.p.latest_frame() {
            worst = worst.max((r.p.position() - f.pts).abs());
            n += 1;
        }
        std::thread::sleep(Duration::from_millis(7));
    }
    assert!(n > 10 && worst < 2.0 / 30.0 + 0.03, "a/v after the switch: {n} frames, worst {worst}");
    // Back to the first track.
    r.select(1);
    let at = r.p.position() + 0.35;
    if at + 0.6 < 3.9 {
        assert!(near(r.tone(at, at + 0.5), LOW));
    }
    // It still plays to the end.
    assert!(r.rec.wait_for(0, T, common::is_ended(r.g)).is_some());
}

#[test]
fn switching_while_paused_keeps_the_frame_and_plays_the_new_track_on_resume() {
    let path = or_skip!(multi_track_mkv());
    let mut r = open(path, |_| {});
    r.p.seek(1.5);
    assert!(r.frame_near(1.5));
    r.select(2);
    assert_eq!(r.p.tracks().active_audio, Some(2));
    assert!(!r.p.is_playing());
    assert!((r.p.position() - 1.5).abs() < 1e-6, "{}", r.p.position());
    assert!(r.frame_near(1.5), "the frame at the position is shown again");
    std::thread::sleep(Duration::from_millis(150));
    assert!(r.tap.blocks().is_empty(), "nothing is played while paused");
    r.p.play();
    assert!(near(r.tone(1.55, 2.3), HIGH), "{} Hz", r.tone(1.55, 2.3));
}

#[test]
fn a_seek_after_a_switch_continues_on_the_new_track() {
    let path = or_skip!(multi_track_mkv());
    let mut r = open(path, |_| {});
    r.p.play();
    assert!(wait_until(T, || r.p.position() > 0.5));
    r.select(2);
    r.p.seek(2.5);
    assert!(r.frame_near(2.5) || r.p.position() > 2.5);
    assert!(wait_until(T, || r.p.position() > 2.6));
    r.tap.clear();
    assert!(near(r.tone(2.7, 3.4), HIGH));
    // Seeking back before the switch point still plays the chosen track.
    r.tap.clear();
    r.p.seek(0.2);
    assert!(wait_until(T, || r.p.position() > 0.3 && r.p.position() < 1.5));
    assert!(near(r.tone(0.4, 1.0), HIGH));
}

#[test]
fn a_track_whose_decoder_fails_leaves_the_previous_one_playing() {
    let path = or_skip!(multi_track_mkv());
    let mut r = open(path, |p| p.simulate_audio_decoder_failure(Some(2)));
    r.p.play();
    assert!(wait_until(T, || r.p.position() > 0.5));
    let events = r.select(2);
    assert!(
        events.iter().any(|e| matches!(e, PlayerEvent::AudioTrackFailed { stream_index: 2, message, .. } if !message.is_empty())),
        "{events:?}"
    );
    assert_eq!(r.p.tracks().active_audio, Some(1));
    let at = r.p.position() + 0.1;
    assert!(near(r.tone(at, at + 0.6), LOW));
    // A stream that is no audio track is refused the same way.
    let events = r.select(0);
    assert!(events.iter().any(|e| matches!(e, PlayerEvent::AudioTrackFailed { stream_index: 0, .. })), "{events:?}");
    assert_eq!(r.p.tracks().active_audio, Some(1));
}

#[test]
fn audio_only_files_switch_tracks_too() {
    let path = or_skip!(audio_only_mka());
    let mut r = open(path, |_| {});
    assert_eq!(r.p.tracks().audio.len(), 2);
    assert_eq!(r.p.tracks().active_audio, Some(0));
    r.p.play();
    assert!(wait_until(T, || r.p.position() > 0.8));
    assert!(near(r.tone(0.2, 0.7), LOW));
    r.select(1);
    let at = r.p.position() + 0.3;
    assert!(near(r.tone(at, at + 0.6), HIGH));
    assert!(r.rec.wait_for(0, T, common::is_ended(r.g)).is_some());
}

#[test]
fn a_video_without_audio_lists_no_audio_track() {
    let path = clip_or_skip!(common::Clip::SmallVideoOnly);
    let mut r = open(path, |_| {});
    let t = r.p.tracks();
    assert!(t.audio.is_empty() && t.subtitles.is_empty() && t.active_audio.is_none());
    let events = r.select(0);
    assert!(events.iter().any(|e| matches!(e, PlayerEvent::AudioTrackFailed { .. })));
    r.p.play();
    assert!(wait_until(T, || r.p.position() > 0.5), "plays on the wall clock");
}

fn lines(path: PathBuf, stream: usize) -> Vec<(f64, f64, String)> {
    let mut seen = Vec::new();
    let out = extract_text_subtitles(path, stream, &CancellationToken::new(), &mut |f| seen.push(f)).expect("extracted");
    assert_eq!(out.extension, "json");
    assert_eq!(seen.last(), Some(&1.0), "progress ends at 1");
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progress only grows: {seen:?}");
    sa_core::subtitle_import::parse(&out.text, Some(out.extension)).into_iter().map(|s| (s.start, s.end, s.text)).collect()
}

fn srt_lines() -> Vec<(f64, f64, String)> {
    vec![(0.5, 1.25, "Hello there".into()), (1.5, 2.75, "Second line, with a comma".into())]
}

#[test]
fn an_embedded_subrip_track_reads_back_with_its_timings() {
    let path = or_skip!(multi_track_mkv());
    assert_eq!(lines(path, 3), srt_lines());
}

#[test]
fn an_embedded_ass_track_reads_back_without_its_styling() {
    let path = or_skip!(multi_track_mkv());
    assert_eq!(lines(path, 4), vec![(1.0, 2.0, "Sign one".into()), (2.5, 3.2, "Two lines".into())]);
}

#[test]
fn an_mp4_mov_text_track_reads_back_with_its_timings() {
    let path = or_skip!(mov_text_mp4());
    let probe = {
        let mut p = FfmpegPlayer::with_options(PlayerOptions { audio_output: false, ..PlayerOptions::default() });
        let rec = Recorder::default();
        rec.attach(&mut p);
        let g = p.load(Some(MediaSource::File(path.clone())), false);
        assert!(rec.wait_for(0, T, is_opened(g)).is_some());
        p.tracks()
    };
    let track = probe.subtitles.first().expect("a subtitle track");
    assert_eq!((track.codec.as_str(), track.kind), ("mov_text", SubtitleKind::Text));
    assert_eq!(lines(path, track.stream_index), srt_lines());
}

#[test]
fn reading_a_track_can_be_cancelled_and_refuses_non_text_streams() {
    let path = or_skip!(multi_track_mkv());
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(extract_text_subtitles(path.clone(), 3, &token, &mut |_| {}), Err(ExtractError::Cancelled));
    assert_eq!(extract_text_subtitles(path.clone(), 1, &CancellationToken::new(), &mut |_| {}), Err(ExtractError::NotText));
    assert!(matches!(extract_text_subtitles(path, 42, &CancellationToken::new(), &mut |_| {}), Err(ExtractError::Failed(_))));
}

#[test]
fn a_read_cancelled_in_the_middle_stops_there() {
    let path = or_skip!(multi_track_mkv());
    let token = CancellationToken::new();
    let mut seen = Vec::new();
    let t = token.clone();
    let result = extract_text_subtitles(path, 3, &token, &mut |f| {
        seen.push(f);
        if f > 0.0 {
            t.cancel();
        }
    });
    assert_eq!(result, Err(ExtractError::Cancelled));
    assert!(seen.iter().any(|f| *f > 0.0 && *f < 1.0), "cancelled after it started: {seen:?}");
    assert!(!seen.contains(&1.0), "it did not run to the end: {seen:?}");
}

#[test]
fn a_read_past_its_limits_is_refused() {
    let path = or_skip!(multi_track_mkv());
    let limits = sa_player::ExtractLimits { max_events: 1, max_text_bytes: 1 << 20 };
    let r = sa_player::ffmpeg_backend::extract_text_subtitles_with(path.clone(), 3, limits, &CancellationToken::new(), &mut |_| {});
    assert_eq!(r, Err(ExtractError::TooLarge));
    let limits = sa_player::ExtractLimits { max_events: 100, max_text_bytes: 8 };
    let r = sa_player::ffmpeg_backend::extract_text_subtitles_with(path, 3, limits, &CancellationToken::new(), &mut |_| {});
    assert_eq!(r, Err(ExtractError::TooLarge));
}

const SRT_BRACES: &str = "1\n00:00:00,500 --> 00:00:01,250\na {b} c\n\n2\n00:00:01,500 --> 00:00:02,000\nback\\slash\n";
const VTT: &str = "WEBVTT\n\n00:00:00.400 --> 00:00:01.100\nFirst <b>cue</b>\n\n00:00:01.600 --> 00:00:02.400\nsecond {x} cue\n";
const ASS_DRAWING: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 240\n\n[V4+ Styles]\n\
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n\n[Events]\n\
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
Dialogue: 0,0:00:00.50,0:00:02.00,Default,,0,0,0,,{\\p1}m 0 0 l 100 0 100 100 0 100{\\p0}\n\
Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\an8}Words{}\n";

/// 3 s of video with a subrip track holding literal braces (stream 1), a WebVTT track (2) and an ASS track with a
/// drawing event (3).
fn text_tracks_mkv() -> Option<PathBuf> {
    let dir = fixtures_dir();
    let (srt, vtt, ass) = (dir.join("text-v2.srt"), dir.join("text-v2.vtt"), dir.join("text-v2.ass"));
    for (p, text) in [(&srt, SRT_BRACES), (&vtt, VTT), (&ass, ASS_DRAWING)] {
        if !p.exists() {
            std::fs::write(p, text).expect("subtitle input");
        }
    }
    let s = |p: &PathBuf| p.to_string_lossy().into_owned();
    let (srt, vtt, ass) = (s(&srt), s(&vtt), s(&ass));
    // The subtitle inputs are stream-copied: re-encoding would pass the cues through FFmpeg's encoders (the WebVTT
    // one rewrites escaped braces), and the test is about what the decoders hand over.
    custom_clip(
        "tracks-text-v3.mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=10", "-i", &srt, "-i", &vtt, "-i", &ass,
            "-t", "3", "-map", "0:v", "-map", "1:s", "-map", "2:s", "-map", "3:s",
            "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:s", "copy",
        ],
    )
}

#[test]
fn literal_braces_and_backslashes_survive_and_webvtt_tracks_read_back() {
    let path = or_skip!(text_tracks_mkv());
    assert_eq!(lines(path.clone(), 1), vec![(0.5, 1.25, "a {b} c".into()), (1.5, 2.0, "back\\slash".into())]);
    assert_eq!(lines(path, 2), vec![(0.4, 1.1, "First cue".into()), (1.6, 2.4, "second {x} cue".into())]);
}

#[test]
fn ass_drawing_events_are_skipped() {
    let path = or_skip!(text_tracks_mkv());
    assert_eq!(lines(path, 3), vec![(1.0, 2.0, "Words".into())]);
}

#[test]
fn switching_after_the_end_takes_effect_on_the_next_seek() {
    let path = or_skip!(multi_track_mkv());
    let mut r = open(path, |_| {});
    r.p.play();
    assert!(r.rec.wait_for(0, T, common::is_ended(r.g)).is_some());
    let events = r.select(2);
    assert!(!events.iter().any(|e| matches!(e, PlayerEvent::AudioTrackFailed { .. })), "{events:?}");
    assert_eq!(r.p.tracks().active_audio, Some(2));
    r.tap.clear();
    r.p.seek(0.5);
    r.p.play();
    assert!(near(r.tone(0.7, 1.4), HIGH), "{} Hz", r.tone(0.7, 1.4));
}

#[test]
fn a_switch_racing_a_seek_plays_the_new_track_after_the_seek() {
    let path = or_skip!(multi_track_mkv());
    let mut r = open(path, |_| {});
    r.p.play();
    assert!(wait_until(T, || r.p.position() > 0.4));
    let from = r.rec.len();
    // No wait between them: the seek can land before or after the demux thread handles the switch.
    r.p.select_audio_track(2);
    r.p.seek(2.0);
    assert!(r.rec.wait_for(from, T, |e| matches!(e, PlayerEvent::TracksChanged { .. })).is_some());
    assert!(wait_until(T, || r.p.position() > 2.1));
    r.tap.clear();
    assert!(near(r.tone(2.3, 3.0), HIGH), "{} Hz", r.tone(2.3, 3.0));
}

#[test]
fn switching_back_before_the_first_switch_was_heard_ends_on_the_first_track() {
    let path = or_skip!(multi_track_mkv());
    let mut r = open(path, |_| {});
    r.p.play();
    assert!(wait_until(T, || r.p.position() > 0.4));
    // A → B → A with no wait at all: the backend, not a possibly stale list, decides.
    r.p.select_audio_track(2);
    r.p.select_audio_track(1);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(r.p.tracks().active_audio, Some(1));
    let at = r.p.position() + 0.3;
    assert!(near(r.tone(at, at + 0.5), LOW), "{} Hz", r.tone(at, at + 0.5));
    // A → B (the demux thread took it) → A at once, before or after the audio thread swapped decoders.
    r.select(2);
    r.select(1);
    assert_eq!(r.p.tracks().active_audio, Some(1));
    let at = r.p.position() + 0.3;
    if at + 0.5 < 3.9 {
        assert!(near(r.tone(at, at + 0.5), LOW), "{} Hz", r.tone(at, at + 0.5));
    }
}

#[test]
fn without_an_output_device_only_the_active_track_changes() {
    let path = or_skip!(multi_track_mkv());
    let rec = Recorder::default();
    let mut p = FfmpegPlayer::with_options(PlayerOptions { audio_output: false, ..PlayerOptions::default() });
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    assert!(rec.wait_for(0, T, is_opened(g)).is_some());
    assert!(rec.wait_for(0, T, |e| matches!(e, PlayerEvent::TracksChanged { .. })).is_some());
    assert!(!p.has_audio_output());
    assert_eq!(p.tracks().active_audio, Some(1));
    let from = rec.len();
    p.select_audio_track(2);
    assert!(rec.wait_for(from, T, |e| matches!(e, PlayerEvent::TracksChanged { .. })).is_some());
    assert_eq!(p.tracks().active_audio, Some(2));
    p.play();
    assert!(wait_until(T, || p.position() > 0.5), "plays on the wall clock");
}

#[test]
fn a_best_audio_stream_that_cannot_be_decoded_falls_back_at_open() {
    let path = or_skip!(multi_track_mkv());
    let mut r = open(path, |p| p.simulate_audio_decoder_failure(Some(1)));
    assert_eq!(r.p.tracks().audio.len(), 2, "both are still listed");
    assert_eq!(r.p.tracks().active_audio, Some(2), "the next stream plays");
    r.p.play();
    assert!(wait_until(T, || r.p.position() > 0.8));
    assert!(near(r.tone(0.2, 0.7), HIGH), "{} Hz", r.tone(0.2, 0.7));
}
