//! FfmpegPlayer against generated clips (feature `ffmpeg`). Audio output is off unless a test says otherwise, so the
//! wall clock is master and timing does not depend on the machine's audio device.

#![cfg(feature = "ffmpeg")]

mod common;

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::{Clip, Recorder, is_ended, is_frame, is_opened, wait_until};
use sa_player::ffmpeg_backend::{FfmpegPlayer, PlayerOptions};
use sa_player::{FailureKind, MediaPlayerBackend, MediaSource, PlayerEvent};

const T: Duration = Duration::from_secs(10);

fn quiet() -> FfmpegPlayer {
    FfmpegPlayer::with_options(PlayerOptions { audio_output: false })
}

fn opened(rec: &Recorder, g: i32) -> (f64, Option<(u32, u32)>) {
    match rec.wait_for(0, T, is_opened(g)) {
        Some(PlayerEvent::Opened { duration, natural_size, .. }) => (duration, natural_size),
        other => panic!("not opened: {other:?} / {:?}", rec.snapshot()),
    }
}

/// Takes frames until one satisfies `f`.
fn frame_where(p: &mut FfmpegPlayer, timeout: Duration, f: impl Fn(&sa_player::VideoFrame) -> bool) -> Option<sa_player::VideoFrame> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(fr) = p.latest_frame() {
            if f(&fr) {
                return Some(fr);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

#[test]
fn opening_reports_duration_size_and_shows_the_first_frame_while_paused() {
    let path = clip_or_skip!(Clip::SmallH264);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    assert_eq!(g, 1);
    let (duration, size) = opened(&rec, g);
    assert!((duration - 3.0).abs() < 0.1, "{duration}");
    assert_eq!(size, Some((320, 240)));
    assert!(rec.wait_for(0, T, is_frame(g)).is_some());
    let f = p.latest_frame().expect("first frame");
    assert_eq!((f.width, f.height, f.stride), (320, 240, 1280));
    assert_eq!(f.rgba.len(), 320 * 240 * 4);
    assert!(f.pts.abs() < 0.05, "{}", f.pts);
    assert!(f.rgba.chunks(4).all(|px| px[3] == 255), "opaque RGBA");
    // Paused: the clock does not move.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(p.position(), 0.0);
    assert!(!p.is_playing());
    assert_eq!(p.duration(), duration);
    assert_eq!(p.natural_size(), Some((320, 240)));
}

#[test]
fn auto_play_runs_to_the_end_and_stops_at_the_duration() {
    let path = clip_or_skip!(Clip::SmallH264);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let started = Instant::now();
    let g = p.load(Some(MediaSource::File(path)), true);
    let (duration, _) = opened(&rec, g);
    assert!(p.is_playing());
    assert!(rec.wait_for(0, Duration::from_secs(8), is_ended(g)).is_some(), "{:?}", rec.snapshot());
    let took = started.elapsed().as_secs_f64();
    assert!(took > duration - 0.2 && took < duration + 1.5, "took {took}");
    assert!(!p.is_playing());
    assert!(p.is_ended());
    assert!((p.position() - duration).abs() < 1e-6);
    let ev = rec.snapshot();
    let stop = ev.iter().position(|e| *e == PlayerEvent::PlayingChanged { generation: g, playing: false });
    let end = ev.iter().position(|e| *e == PlayerEvent::Ended { generation: g });
    assert!(stop.is_some() && stop < end, "{ev:?}");
    // 3 s at 30 fps without an audio clock: nearly every frame is shown.
    let s = p.stats();
    assert!(s.presented >= 85, "{s:?}");
    // Play at the end starts over.
    p.play();
    assert!(p.is_playing());
    assert!(wait_until(T, || p.position() > 0.2 && p.position() < 2.0));
}

#[test]
fn seeking_while_paused_shows_the_exact_target_frame() {
    let path = clip_or_skip!(Clip::SmallH264);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    opened(&rec, g);
    frame_where(&mut p, T, |_| true).expect("first frame");
    for target in [2.0, 0.5, 2.9, 1.234] {
        p.seek(target);
        assert_eq!(p.position(), target, "the position shows the target at once");
        let f = frame_where(&mut p, T, |f| (f.pts - target).abs() < 0.04).unwrap_or_else(|| panic!("frame at {target}"));
        assert!(f.pts + 1.0 / 60.0 >= target, "{} for {target}", f.pts);
        assert_eq!(p.position(), target);
    }
    // Clamped to the duration; beyond the last frame the last one is shown.
    p.seek(99.0);
    assert!((p.position() - p.duration()).abs() < 1e-9);
    assert!(frame_where(&mut p, T, |f| f.pts > 2.8).is_some());
    p.seek(-4.0);
    assert_eq!(p.position(), 0.0);
    assert!(frame_where(&mut p, T, |f| f.pts < 0.05).is_some());
}

#[test]
fn pause_freezes_the_clock_and_play_resumes_from_there() {
    let path = clip_or_skip!(Clip::SmallVideoOnly);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    opened(&rec, g);
    p.play();
    assert!(wait_until(T, || p.position() >= 0.5));
    p.pause();
    let at = p.position();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(p.position(), at);
    let ev = rec.snapshot();
    assert!(ev.contains(&PlayerEvent::PlayingChanged { generation: g, playing: true }));
    assert!(ev.contains(&PlayerEvent::PlayingChanged { generation: g, playing: false }));
    // Repeated calls post nothing new.
    let n = rec.len();
    p.pause();
    assert_eq!(rec.len(), n);
    p.play();
    std::thread::sleep(Duration::from_millis(300));
    let now = p.position();
    assert!(now > at + 0.15 && now < at + 0.6, "{at} → {now}");
}

#[test]
fn seeking_while_playing_continues_from_the_target() {
    let path = clip_or_skip!(Clip::SmallH264);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), true);
    opened(&rec, g);
    assert!(wait_until(T, || p.position() > 0.3));
    p.seek(2.0);
    assert_eq!(p.position(), 2.0);
    assert!(frame_where(&mut p, T, |f| f.pts >= 1.99 && f.pts < 2.1).is_some());
    assert!(wait_until(T, || p.position() > 2.2));
    assert!(p.is_playing());
    assert!(rec.wait_for(0, T, is_ended(g)).is_some());
}

#[test]
fn double_speed_plays_twice_as_fast() {
    let path = clip_or_skip!(Clip::SmallVideoOnly);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    p.set_rate(2.0);
    let started = Instant::now();
    let g = p.load(Some(MediaSource::File(path)), true);
    assert!(rec.wait_for(0, T, is_ended(g)).is_some());
    let took = started.elapsed().as_secs_f64();
    assert!(took > 1.3 && took < 2.4, "took {took}");
    assert_eq!(p.rate(), 2.0);
    p.set_rate(f64::NAN);
    assert_eq!(p.rate(), 1.0);
    p.set_rate(10.0);
    assert_eq!(p.rate(), 4.0);
}

#[test]
fn a_missing_file_fails_with_a_message() {
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(std::env::temp_dir().join("sa-player-no-such-file.mp4"))), true);
    let e = rec.wait_for(0, T, |e| matches!(e, PlayerEvent::Failed { .. })).expect("failed");
    let PlayerEvent::Failed { generation, message } = e else { unreachable!() };
    assert_eq!(generation, g);
    assert!(!message.trim().is_empty());
    assert!(!rec.snapshot().iter().any(|e| matches!(e, PlayerEvent::Opened { .. })));
    assert_eq!(p.failure_kind(), Some(FailureKind::Io), "a missing file is not worth a preview conversion");
    // The auto-play never started (it would only be reported once the source opened) and cannot be started now.
    assert!(!p.is_playing());
    assert!(!rec.snapshot().iter().any(|e| matches!(e, PlayerEvent::PlayingChanged { .. })), "{:?}", rec.snapshot());
    p.play();
    assert!(!p.is_playing(), "a failed source does not play");
}

#[test]
fn a_file_that_is_not_media_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-a-video.mp4");
    std::fs::write(&path, b"this is not a video file at all, just some text").unwrap();
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    assert!(rec.wait_for(0, T, |e| matches!(e, PlayerEvent::Failed { generation, .. } if *generation == g)).is_some());
    assert_eq!(p.failure_kind(), Some(FailureKind::UnsupportedFormat), "the macOS preview fallback may convert it");
    // A new load clears the failure.
    p.load(None, false);
    assert_eq!(p.failure_kind(), None);
}

#[test]
fn events_of_a_replaced_source_never_arrive_after_the_new_load() {
    let a = clip_or_skip!(Clip::SmallH264);
    let b = clip_or_skip!(Clip::SmallVideoOnly);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    for _ in 0..5 {
        let ga = p.load(Some(MediaSource::File(a.clone())), true);
        std::thread::sleep(Duration::from_millis(30));
        let gb = p.load(Some(MediaSource::File(b.clone())), false);
        let from = rec.len();
        assert!(gb == ga + 1);
        opened(&rec, gb);
        std::thread::sleep(Duration::from_millis(100));
        let late: Vec<_> = rec.snapshot().into_iter().skip(from).filter(|e| generation(e) != gb).collect();
        assert!(late.is_empty(), "late events of {ga}: {late:?}");
    }
    // Emptying the player posts nothing and leaves nothing to show.
    let n = rec.len();
    p.load(None, true);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(rec.len(), n);
    assert!(p.latest_frame().is_none());
    assert_eq!((p.position(), p.duration(), p.is_playing(), p.natural_size()), (0.0, 0.0, false, None));
}

fn generation(e: &PlayerEvent) -> i32 {
    match e {
        PlayerEvent::Opened { generation, .. }
        | PlayerEvent::Failed { generation, .. }
        | PlayerEvent::SizeChanged { generation, .. }
        | PlayerEvent::FrameReady { generation }
        | PlayerEvent::Ended { generation }
        | PlayerEvent::PlayingChanged { generation, .. } => *generation,
    }
}

#[test]
fn frame_ready_is_coalesced_until_the_frame_is_taken() {
    let path = clip_or_skip!(Clip::SmallVideoOnly);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), true);
    opened(&rec, g);
    // Nobody takes frames: at most one FrameReady is pending.
    std::thread::sleep(Duration::from_millis(500));
    let ready = rec.snapshot().iter().filter(|e| matches!(e, PlayerEvent::FrameReady { .. })).count();
    assert_eq!(ready, 1);
    assert!(p.stats().presented > 5);
    let f = p.latest_frame().expect("newest frame");
    assert!(f.pts > 0.2, "the newest frame wins: {}", f.pts);
    assert!(wait_until(T, || rec.snapshot().iter().filter(|e| matches!(e, PlayerEvent::FrameReady { .. })).count() == 2));
}

/// A `Read + Seek` that counts reads (the Windows `BufferedVideoStream` path).
struct CountingReader {
    inner: std::fs::File,
    reads: Arc<AtomicU64>,
}

impl Read for CountingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read(buf)
    }
}

impl Seek for CountingReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn a_reader_source_plays_and_seeks_through_custom_io() {
    let path = clip_or_skip!(Clip::SmallH264);
    let reads = Arc::new(AtomicU64::new(0));
    let r = reads.clone();
    let open_path = path.clone();
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let probe_calls = Arc::new(AtomicU64::new(0));
    let pc = probe_calls.clone();
    let g = p.load_with_probe(
        Some(MediaSource::Reader {
            path: path.clone(),
            open: Box::new(move || Ok(Box::new(CountingReader { inner: std::fs::File::open(open_path)?, reads: r }) as Box<dyn sa_player::ReadSeek>)),
        }),
        false,
        Some(Arc::new(move |ratio| {
            pc.fetch_add(1, Ordering::Relaxed);
            (ratio + 0.25).min(1.0)
        })),
    );
    let (duration, size) = opened(&rec, g);
    assert!((duration - 3.0).abs() < 0.1);
    assert_eq!(size, Some((320, 240)));
    assert!(reads.load(Ordering::Relaxed) > 0);
    p.seek(1.5);
    assert!(frame_where(&mut p, T, |f| (f.pts - 1.5).abs() < 0.04).is_some());
    assert!((p.buffered_ratio() - 0.75).abs() < 1e-9);
    assert!(probe_calls.load(Ordering::Relaxed) >= 1);
    // A reader that cannot be opened fails the load.
    let g2 = p.load(
        Some(MediaSource::Reader { path: path.clone(), open: Box::new(|| Err(std::io::Error::other("cache unavailable"))) }),
        false,
    );
    let e = rec.wait_for(0, T, |e| matches!(e, PlayerEvent::Failed { generation, .. } if *generation == g2)).expect("failed");
    assert_eq!(e, PlayerEvent::Failed { generation: g2, message: "cache unavailable".into() });
    assert_eq!(p.failure_kind(), Some(FailureKind::Io));
    assert_eq!(p.buffered_ratio(), 0.0);
}

#[test]
fn a_rotated_phone_video_is_shown_upright() {
    let path = clip_or_skip!(Clip::SmallRotated);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    let (_, size) = opened(&rec, g);
    assert_eq!(size, Some((240, 320)));
    let f = frame_where(&mut p, T, |_| true).expect("frame");
    assert_eq!((f.width, f.height), (240, 320));
}

#[test]
fn audio_only_files_play_to_the_end_without_frames() {
    let path = clip_or_skip!(Clip::Tone);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    let (duration, size) = opened(&rec, g);
    assert!((duration - 3.0).abs() < 0.05);
    assert_eq!(size, None);
    p.seek(2.0);
    p.play();
    assert!(rec.wait_for(0, Duration::from_secs(4), is_ended(g)).is_some());
    assert!(p.latest_frame().is_none());
    assert!((p.position() - duration).abs() < 1e-6);
}

/// With the real audio device (when the machine has one) the audio clock drives playback.
#[test]
fn the_audio_clock_drives_playback_when_a_device_exists() {
    let path = clip_or_skip!(Clip::SmallH264);
    let rec = Recorder::default();
    let mut p = FfmpegPlayer::new();
    p.set_volume(0.0); // silent test
    rec.attach(&mut p);
    let started = Instant::now();
    let g = p.load(Some(MediaSource::File(path)), true);
    let (duration, _) = opened(&rec, g);
    eprintln!("audio output: {}", p.has_audio_output());
    assert!(rec.wait_for(0, Duration::from_secs(10), is_ended(g)).is_some(), "{:?}", rec.snapshot());
    let took = started.elapsed().as_secs_f64();
    assert!(took > duration - 0.3 && took < duration + 2.0, "took {took}");
    // Seek back after the audio ran out: the audio clock takes over again.
    p.seek(1.0);
    p.play();
    assert!(wait_until(T, || p.position() > 1.3));
    assert!(rec.wait_for(0, Duration::from_secs(5), |e| matches!(e, PlayerEvent::Ended { .. })).is_some());
    let s = p.stats();
    eprintln!("audio-clock stats: {s:?}");
}

#[test]
fn volume_and_mute_stay_on_the_player_across_loads() {
    let mut p = quiet();
    assert_eq!((p.volume(), p.is_muted()), (1.0, false));
    p.set_volume(0.75);
    p.set_muted(true);
    p.load(None, false);
    assert_eq!((p.volume(), p.is_muted()), (0.75, true));
    p.set_volume(7.0);
    assert_eq!(p.volume(), 1.0);
    p.set_volume(-1.0);
    assert_eq!(p.volume(), 0.0);
    // Exact read-back (a double, as MediaPlayer.Volume) and a non-finite request keeps the volume.
    p.set_volume(0.33);
    assert_eq!(p.volume(), 0.33);
    p.set_volume(f64::NAN);
    assert_eq!(p.volume(), 0.33);
}

#[test]
fn dispose_stops_everything_and_posts_nothing_more() {
    let path = clip_or_skip!(Clip::SmallH264);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), true);
    opened(&rec, g);
    std::thread::sleep(Duration::from_millis(100));
    p.dispose();
    let n = rec.len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rec.len(), n);
    assert!(!p.is_playing());
}

/// Exact seeks in the 1080p clips of every supported codec/container (H.264 mp4, HEVC mkv, VP9 mkv, HEVC 10-bit
/// mp4): the shown frame is the one at the target, decoded forward from the previous keyframe.
#[test]
fn seeking_in_1080p_clips_of_every_codec_lands_on_the_target_frame() {
    for (c, fps) in [(Clip::Hd60H264, 60.0), (Clip::Hd30Hevc, 30.0), (Clip::Hd30Vp9, 30.0), (Clip::Hd60Hevc10, 60.0)] {
        let path = clip_or_skip!(c);
        let rec = Recorder::default();
        let mut p = quiet();
        rec.attach(&mut p);
        let g = p.load(Some(MediaSource::File(path)), false);
        let (duration, size) = opened(&rec, g);
        assert!((duration - 10.0).abs() < 0.1, "{c:?} {duration}");
        assert_eq!(size, Some((1920, 1080)), "{c:?}");
        let first = frame_where(&mut p, T, |_| true).expect("first frame");
        assert_eq!((first.width, first.height, first.rgba.len()), (1920, 1080, 1920 * 1080 * 4));
        let half = 0.5 / fps + 1e-3;
        // Backwards too: a later keyframe must not be used for an earlier target.
        for target in [4.5, 7.25, 1.1, 9.9] {
            p.seek(target);
            let f = frame_where(&mut p, T, |f| (f.pts - target).abs() <= half).unwrap_or_else(|| panic!("{c:?}: frame at {target}"));
            assert!(f.rgba.chunks(4).step_by(997).all(|px| px[3] == 255), "{c:?}: opaque");
        }
        p.dispose();
    }
}

/// With an audio device the shown frame follows the audio clock: while playing, the newest frame's pts stays within
/// two frame intervals of the reported position.
#[test]
fn video_stays_in_sync_with_the_audio_clock() {
    let path = clip_or_skip!(Clip::Hd30Hevc);
    let rec = Recorder::default();
    let mut p = FfmpegPlayer::new();
    p.set_volume(0.0);
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    opened(&rec, g);
    if !p.has_audio_output() {
        eprintln!("skipped: no audio output device");
        return;
    }
    p.seek(3.0);
    frame_where(&mut p, T, |f| (f.pts - 3.0).abs() < 0.02).expect("seek frame");
    p.play();
    let mut worst = 0f64;
    let mut samples = 0;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(f) = p.latest_frame() {
            let pos = p.position();
            if pos > 3.2 {
                worst = worst.max((pos - f.pts).abs());
                samples += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(7));
    }
    eprintln!("a/v: {samples} samples, worst |position - frame pts| = {:.1} ms", worst * 1000.0);
    assert!(samples > 30);
    assert!(worst < 2.0 / 30.0 + 0.02, "worst {worst}");
    assert!(p.position() > 5.5, "the audio clock ran: {}", p.position());
}

/// A seek issued before the source opened is applied once it opens (clamped to the duration).
#[test]
fn a_seek_before_the_source_opened_is_applied_when_it_opens() {
    let path = clip_or_skip!(Clip::SmallH264);
    for (target, expect) in [(1.5f64, 1.5f64), (99.0, 3.0)] {
        let rec = Recorder::default();
        let mut p = quiet();
        rec.attach(&mut p);
        let g = p.load(Some(MediaSource::File(path.clone())), false);
        p.seek(target);
        let (duration, _) = opened(&rec, g);
        let f = frame_where(&mut p, T, |f| f.pts > 1.0).unwrap_or_else(|| panic!("frame for {target}"));
        assert!((f.pts - expect.min(duration)).abs() < 0.1, "{target}: {}", f.pts);
        assert!((p.position() - expect.min(duration)).abs() < 1e-6, "{target}: {}", p.position());
    }
}

#[test]
fn an_auto_play_load_reports_playing_once_the_source_opened() {
    let path = clip_or_skip!(Clip::SmallH264);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), true);
    assert!(p.is_playing(), "playing (waiting for the source) from the load on");
    opened(&rec, g);
    let playing = PlayerEvent::PlayingChanged { generation: g, playing: true };
    assert!(rec.wait_for(0, T, |e| *e == playing).is_some());
    let ev = rec.snapshot();
    let open_at = ev.iter().position(|e| matches!(e, PlayerEvent::Opened { .. })).unwrap();
    let play_at = ev.iter().position(|e| *e == playing).unwrap();
    assert!(play_at > open_at, "{ev:?}");
    // Reports alternate: a second play() reports nothing, pause reports once.
    p.play();
    p.pause();
    p.pause();
    let changes: Vec<bool> = rec
        .snapshot()
        .iter()
        .filter_map(|e| match e {
            PlayerEvent::PlayingChanged { playing, .. } => Some(*playing),
            _ => None,
        })
        .collect();
    assert_eq!(changes, [true, false]);
}

/// Reads the file until `broken` is set, then fails every read (a drive that was unplugged mid-playback).
struct BreakingReader {
    inner: std::fs::File,
    broken: Arc<std::sync::atomic::AtomicBool>,
}

impl Read for BreakingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.broken.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("device not connected"));
        }
        self.inner.read(buf)
    }
}

impl Seek for BreakingReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn a_source_that_stops_being_readable_fails_instead_of_ending() {
    let path = clip_or_skip!(Clip::Hd60H264);
    let broken = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let events = Arc::new(std::sync::Mutex::new(Vec::<PlayerEvent>::new()));
    let mut p = quiet();
    {
        let (events, broken) = (events.clone(), broken.clone());
        // Break the source as soon as it opened: the demuxer has read only what probing needed.
        p.set_event_sink(Box::new(move |e| {
            if matches!(e, PlayerEvent::Opened { .. }) {
                broken.store(true, Ordering::Relaxed);
            }
            events.lock().unwrap().push(e);
        }));
    }
    let b = broken.clone();
    let g = p.load(
        Some(MediaSource::Reader {
            path: path.clone(),
            open: Box::new(move || Ok(Box::new(BreakingReader { inner: std::fs::File::open(path)?, broken: b }) as Box<dyn sa_player::ReadSeek>)),
        }),
        true,
    );
    assert!(wait_until(T, || events.lock().unwrap().iter().any(|e| matches!(e, PlayerEvent::Failed { .. }))), "{:?}", events.lock().unwrap());
    let ev = events.lock().unwrap().clone();
    let failed = ev.iter().find(|e| matches!(e, PlayerEvent::Failed { .. })).unwrap();
    let PlayerEvent::Failed { generation, message } = failed else { unreachable!() };
    assert_eq!(*generation, g);
    assert!(!message.trim().is_empty());
    assert!(!ev.iter().any(|e| matches!(e, PlayerEvent::Ended { .. })), "{ev:?}");
    assert_eq!(p.failure_kind(), Some(FailureKind::Io));
    assert!(wait_until(T, || !p.is_playing()));
    assert_eq!(ev.iter().filter(|e| matches!(e, PlayerEvent::Failed { .. })).count(), 1);
    p.play();
    assert!(!p.is_playing());
}

/// Two H.264 segments of different sizes in one MPEG-TS (a broadcast recording switching resolution).
fn resolution_change_clip() -> Option<std::path::PathBuf> {
    let ff = common::ffmpeg_cli()?;
    let dir = common::fixtures_dir();
    let out = dir.join("resolution-change.ts");
    if out.exists() {
        return Some(out);
    }
    let seg = |size: &str, offset: &str, name: &str| {
        let p = dir.join(format!("tmp-{}-{name}", std::process::id()));
        let ok = std::process::Command::new(&ff)
            .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=size={size}:rate=30"))
            .args(["-t", "2", "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p", "-output_ts_offset", offset])
            .arg(&p)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "ffmpeg failed to generate {name}");
        p
    };
    let a = seg("320x240", "0", "a.ts");
    let b = seg("640x360", "2", "b.ts");
    let mut bytes = std::fs::read(&a).unwrap();
    bytes.extend(std::fs::read(&b).unwrap());
    let tmp = dir.join(format!("tmp-{}-resolution-change.ts", std::process::id()));
    std::fs::write(&tmp, bytes).unwrap();
    let _ = std::fs::remove_file(a);
    let _ = std::fs::remove_file(b);
    if std::fs::rename(&tmp, &out).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Some(out)
}

#[test]
fn a_resolution_change_in_the_stream_reports_the_new_size() {
    let Some(path) = resolution_change_clip() else {
        eprintln!("skipped: no ffmpeg CLI to generate test clips");
        return;
    };
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), true);
    let (_, size) = opened(&rec, g);
    assert_eq!(size, Some((320, 240)));
    let changed = PlayerEvent::SizeChanged { generation: g, width: 640, height: 360 };
    assert!(rec.wait_for(0, T, |e| *e == changed).is_some(), "{:?}", rec.snapshot());
    assert!(frame_where(&mut p, T, |f| (f.width, f.height) == (640, 360)).is_some());
    assert_eq!(p.natural_size(), Some((640, 360)));
}
