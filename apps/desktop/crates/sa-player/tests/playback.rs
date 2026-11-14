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
    FfmpegPlayer::with_options(PlayerOptions { audio_output: false, ..PlayerOptions::default() })
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
    assert!(s.decoded >= 85 && s.presented >= 40 && s.presented + s.dropped_late >= 85, "{s:?}");
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

/// A burst of seeks (a slider drag, a held arrow key): every request is reported at once, the seeks requested
/// while one is still decoding wait for it instead of cancelling it, and the newest one wins.
#[test]
fn a_burst_of_seeks_reports_each_target_and_lands_on_the_last() {
    let path = clip_or_skip!(Clip::SmallH264);
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    opened(&rec, g);
    frame_where(&mut p, T, |_| true).expect("first frame");
    let targets: Vec<f64> = (0..20).map(|i| 0.1 + 0.13 * i as f64).collect();
    for &t in &targets {
        p.seek(t);
        assert_eq!(p.position(), t, "the position shows the newest target at once");
    }
    let last = *targets.last().unwrap();
    let f = frame_where(&mut p, T, |f| (f.pts - last).abs() < 0.04).expect("frame at the last target");
    assert!(f.pts + 1.0 / 60.0 >= last, "{} for {last}", f.pts);
    // Nothing else is pending: the position stays on the last target and no later frame replaces it.
    assert_eq!(p.position(), last);
    std::thread::sleep(Duration::from_millis(300));
    assert!(p.latest_frame().is_none_or(|f| (f.pts - last).abs() < 0.04));
    assert_eq!(p.position(), last);
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
    // The clock holds the target until the target frame is shown, then runs on; a starved test thread can read it
    // a little after that, never before the target.
    let at = p.position();
    assert!((2.0..2.3).contains(&at), "{at}");
    // The first frame taken after the seek comes from the target. It is usually the target frame itself; when the
    // test thread is starved (parallel tests decoding 1080p) newer frames may replace it before it is taken, so a
    // few frames of slack are allowed. Frames of the old position (< 1 s) never show up.
    let first = frame_where(&mut p, T, |f| f.pts >= 1.0).expect("a frame after the seek");
    assert!(first.pts >= 1.99 && first.pts < 2.5, "{}", first.pts);
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
    let g = p.load(Some(MediaSource::File(path)), true);
    // Timed from the open: the first load of a process may wait for the hardware decoding device.
    opened(&rec, g);
    let started = Instant::now();
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
        | PlayerEvent::PlayingChanged { generation, .. }
        | PlayerEvent::TracksChanged { generation }
        | PlayerEvent::AudioTrackFailed { generation, .. }
        | PlayerEvent::SubtitleTrackFailed { generation, .. }
        | PlayerEvent::SubtitlePictureChanged { generation } => *generation,
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
    assert!(samples >= 15, "samples {samples}");
    assert!(worst < 2.0 / 30.0 + 0.05, "worst {worst}");
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

/// Windows: as with the C# app's `BufferedVideoStream` (share read/write/delete), the file that is playing can be
/// renamed and deleted, and playback goes on from the open handle.
#[cfg(windows)]
#[test]
fn a_playing_file_can_be_renamed_and_deleted_on_windows() {
    let Some(src) = common::clip(Clip::SmallH264) else { return };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("playing.mp4");
    std::fs::copy(&src, &path).unwrap();
    let rec = Recorder::default();
    let mut p = quiet();
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path.clone())), true);
    opened(&rec, g);
    assert!(rec.wait_for(0, T, is_frame(g)).is_some());
    let renamed = dir.path().join("renamed.mp4");
    std::fs::rename(&path, &renamed).expect("a playing file can be renamed");
    std::fs::remove_file(&renamed).expect("a playing file can be deleted");
    // Playback still runs to the end from the open handle.
    assert!(rec.wait_for(0, T, is_ended(g)).is_some(), "{:?}", rec.snapshot());
    assert!(!rec.snapshot().iter().any(|e| matches!(e, PlayerEvent::Failed { .. })));
}

/// Every extension the former apps accepted (`MediaFileTypes` / `VideoTypes`: 14 video, 8 audio) opens, with a
/// typical codec pairing for that container (plus AV1, which Media Foundation and AVFoundation play): duration,
/// natural size and the first frame for video, no size for audio. This pins the decoders and demuxers the FFmpeg
/// build must carry to match the platform players. A few cheap containers of the wider `sa_core::media_types` list
/// follow (VOB, Ogg Theora, ASF, MXF, DV, Matroska audio, raw AC-3, AIFF); every case's extension must be accepted.
#[test]
fn every_media_extension_the_apps_accept_opens() {
    let Some(ff) = common::ffmpeg_cli() else {
        eprintln!("skipped: no ffmpeg CLI to generate test clips");
        return;
    };
    let v = |size: &str| vec!["-f".to_owned(), "lavfi".into(), "-i".into(), format!("testsrc2=size={size}:rate=25")];
    let a = |rate: u32| vec!["-f".to_owned(), "lavfi".into(), "-i".into(), format!("sine=frequency=440:sample_rate={rate}")];
    let s = |x: &[&str]| x.iter().map(|y| y.to_string()).collect::<Vec<_>>();
    let av = |size: &str, rate: u32, codecs: &[&str]| [v(size), a(rate), s(&["-t", "1"]), s(codecs)].concat();
    let ao = |rate: u32, codecs: &[&str]| [a(rate), s(&["-t", "1"]), s(codecs)].concat();
    let encoder_list: std::collections::HashSet<String> = std::process::Command::new(&ff)
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.split_whitespace().nth(1).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let av1_codecs: &[&str] = if encoder_list.contains("libsvtav1") {
        &["-c:v", "libsvtav1", "-preset", "12", "-svtav1-params", "log=0", "-c:a", "aac"]
    } else {
        &["-c:v", "libaom-av1", "-cpu-used", "8", "-b:v", "200k", "-c:a", "aac"]
    };
    let h264 = ["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"];
    let cases: Vec<(&str, Vec<String>, bool)> = vec![
        ("av1.mp4", av("160x120", 48000, av1_codecs), true),
        ("h264.mov", av("160x120", 48000, &[&h264[..], &["-c:a", "aac"]].concat()), true),
        ("prores.mov", av("160x120", 48000, &["-c:v", "prores_ks", "-profile:v", "0", "-c:a", "pcm_s16le"]), true),
        ("h264.m4v", av("160x120", 48000, &[&h264[..], &["-c:a", "aac"]].concat()), true),
        ("h264-ac3.mkv", av("160x120", 48000, &[&h264[..], &["-c:a", "ac3"]].concat()), true),
        ("mpeg4-mp3.avi", av("160x120", 44100, &["-c:v", "mpeg4", "-c:a", "libmp3lame"]), true),
        ("vp8-vorbis.webm", av("160x120", 48000, &["-c:v", "libvpx", "-deadline", "realtime", "-c:a", "libvorbis"]), true),
        ("wmv2-wmav2.wmv", av("160x120", 44100, &["-c:v", "wmv2", "-c:a", "wmav2"]), true),
        ("mpeg2.mpg", av("160x120", 48000, &["-c:v", "mpeg2video", "-c:a", "mp2", "-f", "vob"]), true),
        ("mpeg1.mpeg", av("160x120", 44100, &["-c:v", "mpeg1video", "-c:a", "mp2", "-f", "mpeg"]), true),
        ("h264.ts", av("160x120", 48000, &[&h264[..], &["-c:a", "aac", "-f", "mpegts"]].concat()), true),
        ("h264.mts", av("160x120", 48000, &[&h264[..], &["-c:a", "ac3", "-f", "mpegts", "-mpegts_m2ts_mode", "1"]].concat()), true),
        ("h264.m2ts", av("160x120", 48000, &[&h264[..], &["-c:a", "ac3", "-f", "mpegts", "-mpegts_m2ts_mode", "1"]].concat()), true),
        ("h263.3gp", av("176x144", 48000, &["-c:v", "h263", "-c:a", "aac", "-ac", "1"]), true),
        ("sorenson.flv", av("160x120", 44100, &["-c:v", "flv", "-c:a", "libmp3lame"]), true),
        ("tone.mp3", ao(44100, &["-c:a", "libmp3lame"]), false),
        ("tone.wav", ao(48000, &["-c:a", "pcm_s16le"]), false),
        ("tone.m4a", ao(48000, &["-c:a", "aac", "-f", "ipod"]), false),
        ("tone.aac", ao(48000, &["-c:a", "aac", "-f", "adts"]), false),
        ("tone.flac", ao(48000, &["-c:a", "flac"]), false),
        ("tone.ogg", ao(48000, &["-c:a", "libvorbis"]), false),
        ("tone.opus", ao(48000, &["-c:a", "libopus"]), false),
        ("tone.wma", ao(44100, &["-c:a", "wmav2", "-f", "asf"]), false),
        // Additions of the wider accepted list.
        ("mpeg2-ac3.vob", av("160x120", 48000, &["-c:v", "mpeg2video", "-c:a", "ac3", "-f", "vob"]), true),
        ("theora-vorbis.ogv", av("160x120", 48000, &["-c:v", "libtheora", "-c:a", "libvorbis"]), true),
        ("wmv2-wmav2.asf", av("160x120", 44100, &["-c:v", "wmv2", "-c:a", "wmav2"]), true),
        ("mpeg2-pcm.mxf", av("160x120", 48000, &["-c:v", "mpeg2video", "-c:a", "pcm_s16le"]), true),
        ("dv-pcm.dv", av("720x576", 48000, &["-c:v", "dvvideo", "-pix_fmt", "yuv420p", "-c:a", "pcm_s16le", "-ac", "2", "-f", "dv"]), true),
        ("tone.mka", ao(48000, &["-c:a", "libvorbis"]), false),
        ("tone.ac3", ao(48000, &["-c:a", "ac3"]), false),
        ("tone.aiff", ao(48000, &["-c:a", "pcm_s16be"]), false),
    ];
    for (name, _, video) in &cases {
        let ext = name.rsplit('.').next().unwrap();
        let accepted =
            if *video { sa_core::media_types::is_video_extension(ext) } else { sa_core::media_types::is_audio_extension(ext) };
        assert!(accepted, "{name}: not an accepted {} extension", if *video { "video" } else { "audio" });
    }
    let has_encoders = |args: &[String]| {
        args.windows(2).all(|w| !matches!(w[0].as_str(), "-c:v" | "-c:a" | "-c:s") || encoder_list.contains(&w[1]))
    };
    let cases: Vec<_> = cases.into_iter().filter(|(_, args, _)| encoder_list.is_empty() || has_encoders(args)).collect();
    assert!(cases.len() >= 20, "expected most media format fixtures to have encoders, got {}", cases.len());
    let dir = common::fixtures_dir().join("formats");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, args, video) in cases {
        let path = dir.join(name);
        if !path.exists() {
            let tmp = dir.join(format!("tmp-{}-{name}", std::process::id()));
            let ok = std::process::Command::new(&ff)
                .args(["-y", "-hide_banner", "-loglevel", "error"])
                .args(&args)
                .arg(&tmp)
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "ffmpeg failed to generate {name}");
            if std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        let rec = Recorder::default();
        let mut p = quiet();
        rec.attach(&mut p);
        let g = p.load(Some(MediaSource::File(path)), false);
        let Some(PlayerEvent::Opened { duration, natural_size, .. }) =
            rec.wait_for(0, T, |e| matches!(e, PlayerEvent::Opened { .. } | PlayerEvent::Failed { .. }))
        else {
            panic!("{name}: {:?}", rec.snapshot());
        };
        assert_eq!(p.failure_kind(), None, "{name}");
        assert!(duration > 0.8 && duration < 1.6, "{name}: duration {duration}");
        if video {
            let (w, h) = natural_size.unwrap_or_else(|| panic!("{name}: no size"));
            let f = frame_where(&mut p, T, |_| true).unwrap_or_else(|| panic!("{name}: no frame"));
            assert_eq!((f.width, f.height), (w, h), "{name}");
            assert!(w >= 160 && h >= 120, "{name}: {w}x{h}");
        } else {
            assert_eq!(natural_size, None, "{name}");
        }
        let _ = g;
        p.dispose();
    }
}

/// An empty file fails the same way through our own file handle (Windows) and through FFmpeg's file protocol (macOS;
/// forced here with a `file:` URL), and counts as a format problem, as AVFoundation's "file format not recognized".
#[test]
fn an_empty_file_fails_the_same_way_through_either_file_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.mp4");
    std::fs::write(&path, b"").unwrap();
    let url = std::path::PathBuf::from(format!("file:{}", path.display()));
    let mut kinds = Vec::new();
    for source in [path, url] {
        let rec = Recorder::default();
        let mut p = quiet();
        rec.attach(&mut p);
        let g = p.load(Some(MediaSource::File(source.clone())), false);
        let e = rec.wait_for(0, T, |e| matches!(e, PlayerEvent::Failed { generation, .. } if *generation == g));
        assert!(e.is_some(), "{source:?}: {:?}", rec.snapshot());
        kinds.push(p.failure_kind());
    }
    assert_eq!(kinds, [Some(FailureKind::UnsupportedFormat); 2]);
}

fn player(hardware: bool, planar: bool) -> FfmpegPlayer {
    let mut p = FfmpegPlayer::with_options(PlayerOptions { audio_output: false, hardware_decoding: hardware });
    p.set_planar_output(planar);
    p
}

/// The first frame of a paused load.
fn first_frame(p: &mut FfmpegPlayer, path: &std::path::Path) -> sa_player::VideoFrame {
    let rec = Recorder::default();
    rec.attach(p);
    let g = p.load(Some(MediaSource::File(path.to_path_buf())), false);
    opened(&rec, g);
    frame_where(p, T, |_| true).expect("first frame")
}

/// The R'G'B' (0..255) of pixel (`x`, `y`) of the coded picture, converted on the CPU the way the UI's shader does
/// (nearest chroma sample; `sa_player::color::yuv_to_rgb_reference`).
fn planar_rgb(img: &sa_player::PlanarImage, x: usize, y: usize) -> [f32; 3] {
    sa_player::color::yuv_to_rgb_reference(img, x, y).map(|c| c * 255.0)
}

/// Mean and worst channel difference between `img` converted like the shader and the RGBA path's `reference` frame
/// (same size, no rotation), over a grid of pixels.
fn difference_to_rgba(img: &sa_player::PlanarImage, reference: &sa_player::VideoFrame) -> (f32, f32) {
    let (w, h) = (img.coded_width as usize, img.coded_height as usize);
    assert_eq!((reference.width as usize, reference.height as usize), (w, h));
    let (mut sum, mut n, mut worst) = (0f32, 0f32, 0f32);
    for y in (0..h).step_by(7) {
        for x in (0..w).step_by(5) {
            let rgb = planar_rgb(img, x, y);
            let o = y * reference.stride as usize + x * 4;
            for k in 0..3 {
                let d = (rgb[k] - reference.rgba[o + k] as f32).abs();
                sum += d;
                n += 1.0;
                worst = worst.max(d);
            }
        }
    }
    (sum / n, worst)
}

/// Planar output hands over the decoder's planes (software: zero-copy; hardware: the one NV12 download); converted
/// with the frame's matrix they give the same picture as the RGBA path.
#[test]
fn planar_output_hands_over_planes_that_convert_to_the_rgba_picture() {
    let path = clip_or_skip!(Clip::SmallH264);
    let reference = first_frame(&mut player(false, false), &path);
    assert!(reference.planar.is_none());
    for hardware in [false, true] {
        let mut p = player(hardware, true);
        let f = first_frame(&mut p, &path);
        let img = f.planar.clone().unwrap_or_else(|| panic!("hardware={hardware}: planar frame"));
        assert!(f.rgba.is_empty());
        assert_eq!((f.width, f.height, f.pts), (320, 240, reference.pts));
        assert_eq!((img.coded_width, img.coded_height, img.bits, img.chroma_shift, img.quarter_turns), (320, 240, 8, (1, 1), 0));
        assert_eq!(img.matrix, sa_player::YuvMatrix::Bt601);
        assert_eq!((img.transfer, img.primaries, img.interlaced, f.second_field), (sa_player::Transfer::Sdr, sa_player::Primaries::Bt709, false, false));
        let stats = p.stats();
        assert!(stats.planar >= 1);
        if !hardware {
            assert_eq!((stats.hardware, img.layout), (0, sa_player::PlaneLayout::Planar));
        } else if stats.hardware > 0 {
            assert_eq!(img.layout, sa_player::PlaneLayout::SemiPlanar, "hardware frames are NV12");
        }
        let (_, yi) = img.plane(0).expect("luma");
        assert_eq!((yi.width, yi.rows), (320, 240));
        let (mean, worst) = difference_to_rgba(&img, &reference);
        if !hardware {
            // Software decode matches swscale to rounding precision.
            assert!(worst <= 24.0, "hardware={hardware}: mean {mean:.2}, worst channel difference {worst}");
        } else {
            // Hardware decoders (e.g. VideoToolbox NV12) may filter or site chroma slightly differently at sharp
            // testsrc2 edges; a wrong plane, stride, UV order, or range raises the mean error across the picture.
            assert!(mean <= 6.0 && worst <= 140.0, "hardware={hardware}: mean {mean:.2}, worst channel difference {worst}");
        }
        p.dispose();
    }
}

/// Every pixel format reaches the GPU path: planar YUV/gray formats as their own planes, everything else as swscale's
/// RGBA in one packed plane; each converts to the picture the RGBA path shows.
#[test]
fn every_pixel_format_is_handed_over_as_planes() {
    use sa_player::PlaneLayout::{Gray, Packed, Planar, SemiPlanar};
    let cases = [
        ("yuv410p", Planar, 8, (2, 2)),
        ("yuv411p", Planar, 8, (2, 0)),
        ("yuv440p", Planar, 8, (0, 1)),
        ("yuvj422p", Planar, 8, (1, 0)),
        ("yuv420p9le", Planar, 9, (1, 1)),
        ("yuv422p12le", Planar, 12, (1, 0)),
        ("yuv444p14le", Planar, 14, (0, 0)),
        ("yuv444p16le", Planar, 16, (0, 0)),
        ("yuva420p", Planar, 8, (1, 1)),
        ("yuva444p10le", Planar, 10, (0, 0)),
        ("nv12", SemiPlanar, 8, (1, 1)),
        ("nv21", SemiPlanar, 8, (1, 1)),
        ("gray", Gray, 8, (0, 0)),
        ("gray10le", Gray, 10, (0, 0)),
        ("gray16le", Gray, 16, (0, 0)),
        ("gbrp", Packed, 8, (0, 0)),
        ("gbrp10le", Packed, 8, (0, 0)),
        ("rgb24", Packed, 8, (0, 0)),
        ("bgra", Packed, 8, (0, 0)),
        ("pal8", Packed, 8, (0, 0)),
        ("yuyv422", Packed, 8, (0, 0)),
        ("yuv420p10be", Packed, 8, (0, 0)),
        ("rgb48le", Packed, 8, (0, 0)),
    ];
    let mut failures = Vec::new();
    for (fmt, layout, bits, shift) in cases {
        let Some(path) = common::raw_clip(fmt) else {
            eprintln!("skipped: no ffmpeg CLI");
            return;
        };
        let reference = first_frame(&mut player(false, false), &path);
        let mut p = player(true, true);
        let f = first_frame(&mut p, &path);
        let Some(img) = f.planar.clone() else {
            failures.push(format!("{fmt}: no planar frame"));
            continue;
        };
        assert!(f.rgba.is_empty(), "{fmt}");
        assert_eq!((f.width, f.height), (320, 240), "{fmt}");
        assert_eq!((img.layout, img.bits, img.chroma_shift), (layout, bits, shift), "{fmt}");
        assert_eq!(img.swap_uv, fmt == "nv21", "{fmt}");
        let (mean, worst) = difference_to_rgba(&img, &reference);
        // Nearest vs. interpolated chroma differs at the pattern's edges (most at 4:1:0); a wrong plane, stride, bit
        // depth or range is off everywhere.
        if mean > 6.0 || worst > 140.0 {
            failures.push(format!("{fmt}: mean difference {mean:.1}, worst {worst:.0}"));
        }
        p.dispose();
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Interlaced frames carry their field order and are handed over twice, the second field half a frame later.
#[test]
fn interlaced_frames_are_handed_over_field_by_field() {
    let Some(path) = interlaced_clip() else { return };
    let rec = Recorder::default();
    let mut p = player(false, true);
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    opened(&rec, g);
    let first = frame_where(&mut p, T, |_| true).expect("first frame");
    let img = first.planar.clone().expect("planar");
    assert!(img.interlaced && img.top_field_first);
    assert!(!first.second_field);
    assert!(!img.shown_field_is_bottom(false) && img.shown_field_is_bottom(true));
    p.play();
    let mut seen = Vec::new();
    let ok = common::wait_until(T, || {
        if let Some(f) = p.latest_frame() {
            seen.push((f.pts, f.second_field));
        }
        seen.len() >= 12
    });
    assert!(ok, "frames: {seen:?}");
    let seconds: Vec<_> = seen.iter().filter(|(_, s)| *s).collect();
    assert!(!seconds.is_empty(), "no second fields: {seen:?}");
    // 25 frames/s → fields 20 ms apart.
    for w in seen.windows(2) {
        let d = w[1].0 - w[0].0;
        if w[0].1 != w[1].1 && d > 0.0 && d < 0.03 {
            assert!((d - 0.02).abs() < 0.004, "field spacing {d}: {seen:?}");
        }
    }
    p.dispose();

    // A progressive clip is never split.
    let path = clip_or_skip!(Clip::SmallH264);
    let f = first_frame(&mut player(false, true), &path);
    assert!(!f.planar.expect("planar").interlaced && !f.second_field);
}

/// HDR10: PQ, BT.2020, mastering metadata (4000 nits) + MaxCLL 1200, from the lavfi `source`, cached as `name`.
fn hdr10_clip_of(name: &str, source: &str) -> Option<std::path::PathBuf> {
    common::custom_clip(
        name,
        &[
            "-f", "lavfi", "-i", source, "-t", "1", "-c:v", "libx265", "-preset", "ultrafast",
            "-x265-params",
            "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:hdr10=1:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(40000000,1):max-cll=1200,400",
            "-pix_fmt", "yuv420p10le", "-color_primaries", "bt2020", "-color_trc", "smpte2084", "-colorspace", "bt2020nc",
        ],
    )
}

fn hdr10_clip() -> Option<std::path::PathBuf> {
    hdr10_clip_of("hdr10-pq.mkv", "testsrc2=size=320x240:rate=10")
}

/// HLG in BT.2020 from the lavfi `source`, cached as `name`.
fn hlg_clip_of(name: &str, source: &str) -> Option<std::path::PathBuf> {
    common::custom_clip(
        name,
        &[
            "-f", "lavfi", "-i", source, "-t", "1", "-c:v", "libx265", "-preset", "ultrafast",
            "-x265-params", "log-level=error:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc",
            "-pix_fmt", "yuv420p10le", "-color_primaries", "bt2020", "-color_trc", "arib-std-b67", "-colorspace", "bt2020nc",
        ],
    )
}

fn hlg_clip() -> Option<std::path::PathBuf> {
    hlg_clip_of("hlg-hevc.mkv", "testsrc2=size=320x240:rate=10")
}

/// HDR10 (PQ, BT.2020, mastering metadata + MaxCLL) and HLG frames carry what the shader needs to tone-map them.
#[test]
fn hdr_frames_carry_their_transfer_primaries_and_peak() {
    let Some(pq) = hdr10_clip() else {
        eprintln!("skipped: no ffmpeg CLI");
        return;
    };
    for hardware in [false, true] {
        let f = first_frame(&mut player(hardware, true), &pq);
        let img = f.planar.expect("planar");
        assert_eq!((img.transfer, img.primaries, img.matrix), (sa_player::Transfer::Pq, sa_player::Primaries::Bt2020, sa_player::YuvMatrix::Bt2020), "hardware={hardware}");
        assert_eq!(img.peak_nits, 1200.0, "hardware={hardware}: MaxCLL wins over the 4000-nit mastering peak");
    }
    let Some(hlg) = hlg_clip() else {
        return;
    };
    let img = first_frame(&mut player(false, true), &hlg).planar.expect("planar");
    assert_eq!((img.transfer, img.primaries), (sa_player::Transfer::Hlg, sa_player::Primaries::Bt2020));
    let params = sa_player::color::ColorParams::for_image(&img);
    assert_eq!(params.mode, sa_player::color::ColorMode::Hlg);
}

/// The RGBA path (software renderer) tone-maps HDR10 and HLG as the shader does: compared with the planar frame
/// converted by the shader's math (`yuv_to_rgb_reference` + `ColorParams::apply`), and far from the washed-out
/// picture of the plain matrix conversion. Software and hardware decoding.
#[test]
fn rgba_path_tone_maps_hdr10_and_hlg_like_the_shader() {
    // SMPTE HD bars: large flat areas at many levels (on testsrc2 only the black background is flat).
    let bars = "smptehdbars=size=320x240:rate=10";
    let (Some(pq), Some(hlg)) = (hdr10_clip_of("hdr10-bars.mkv", bars), hlg_clip_of("hlg-bars.mkv", bars)) else {
        eprintln!("skipped: no ffmpeg CLI or libx265");
        return;
    };
    for (name, path) in [("HDR10", pq), ("HLG", hlg)] {
        for hardware in [false, true] {
            let img = first_frame(&mut player(hardware, true), &path).planar.expect("planar");
            let mut p = player(hardware, false);
            let f = first_frame(&mut p, &path);
            assert!(f.planar.is_none() && f.pts == 0.0);
            assert_eq!((f.width, f.height, f.stride), (320, 240, 320 * 4));
            let params = sa_player::color::ColorParams::for_image(&img);
            let reference = |x: usize, y: usize| sa_player::color::yuv_to_rgb_reference(&img, x, y);
            let (mut sum, mut sum_plain, mut n, mut worst_flat, mut flat) = (0f64, 0f64, 0f64, 0f32, 0);
            let (mut flat_plain, mut flat_n) = (0f64, 0f64);
            for y in (2..238).step_by(3) {
                for x in (2..318).step_by(3) {
                    let rgb = reference(x, y);
                    let want = params.apply(rgb).map(|c| c * 255.0);
                    // Flat: the same colour two pixels around (bilinear and nearest chroma agree there; at the
                    // pattern's saturated edges the tone curve magnifies their difference up to a full code range).
                    let is_flat = [(x - 2, y), (x + 2, y), (x, y - 2), (x, y + 2)]
                        .iter()
                        .all(|&(a, b)| reference(a, b).iter().zip(rgb).all(|(c, r)| (c - r).abs() < 0.5 / 255.0));
                    flat += is_flat as u32;
                    let o = y * f.stride as usize + x * 4;
                    for k in 0..3 {
                        let got = f.rgba[o + k] as f32;
                        let d = (got - want[k]).abs();
                        sum += d as f64;
                        sum_plain += (got - rgb[k] * 255.0).abs() as f64;
                        n += 1.0;
                        if is_flat {
                            worst_flat = worst_flat.max(d);
                            flat_plain += (got - rgb[k] * 255.0).abs() as f64;
                            flat_n += 1.0;
                        }
                    }
                }
            }
            let (mean, mean_plain, flat_plain) = (sum / n, sum_plain / n, flat_plain / flat_n);
            let s = p.stats();
            eprintln!(
                "{name} hardware={hardware}: mean {mean:.2}, worst in flat areas {worst_flat} ({flat} pixels), mean to the untone-mapped picture {mean_plain:.1} (flat areas {flat_plain:.1}); {s:?}"
            );
            assert!(s.soft_tonemapped >= 1 && s.planar == 0, "{name} hardware={hardware}: {s:?}");
            assert!(flat > 1000, "{name}: {flat} flat pixels");
            // Within one code of the rounded shader value (`want` is unrounded).
            assert!(mean <= 3.0 && worst_flat <= 1.5, "{name} hardware={hardware}: mean {mean}, worst in flat areas {worst_flat}");
            assert!(mean_plain >= 3.0 * mean && flat_plain >= 10.0, "{name} hardware={hardware}: tone mapping changed little ({mean_plain}, flat {flat_plain})");
            p.dispose();
        }
    }
}

/// Above 1440p the RGBA path tone-maps at half width and height: the buffer is half size, the video keeps its size
/// (`video_size`, `natural_size`), and flat areas keep the shader's colour.
#[test]
fn large_hdr_frames_are_tone_mapped_at_half_size() {
    let Some(path) = hdr10_clip_of("hdr10-bars-1600p.mkv", "smptehdbars=size=2560x1600:rate=10") else {
        eprintln!("skipped: no ffmpeg CLI or libx265");
        return;
    };
    let img = first_frame(&mut player(false, true), &path).planar.expect("planar");
    let mut p = player(false, false);
    let f = first_frame(&mut p, &path);
    assert!(f.planar.is_none());
    assert_eq!((f.width, f.height, f.stride, f.video_size), (1280, 800, 1280 * 4, (2560, 1600)));
    assert_eq!(p.natural_size(), Some((2560, 1600)), "the frame size is not the video size");
    assert!(p.stats().soft_tonemapped >= 1);
    let params = sa_player::color::ColorParams::for_image(&img);
    let reference = |x: usize, y: usize| sa_player::color::yuv_to_rgb_reference(&img, x, y);
    let (mut flat, mut worst) = (0, 0f32);
    for y in (4..796).step_by(7) {
        for x in (4..1276).step_by(7) {
            let (sx, sy) = (2 * x, 2 * y);
            let rgb = reference(sx, sy);
            let is_flat = [(sx - 6, sy), (sx + 6, sy), (sx, sy - 6), (sx, sy + 6)]
                .iter()
                .all(|&(a, b)| reference(a, b).iter().zip(rgb).all(|(c, r)| (c - r).abs() < 0.5 / 255.0));
            if !is_flat {
                continue;
            }
            flat += 1;
            let want = params.apply(rgb).map(|c| c * 255.0);
            let o = y * f.stride as usize + x * 4;
            for k in 0..3 {
                worst = worst.max((f.rgba[o + k] as f32 - want[k]).abs());
            }
        }
    }
    eprintln!("half size: worst in flat areas {worst} ({flat} pixels)");
    assert!(flat > 1000 && worst <= 1.5, "worst {worst} over {flat} flat pixels");
    p.dispose();
}

/// Vertical comb strength of a picture given as rows of luma: how much rows stick out from both neighbours in the
/// same direction (the teeth of two fields captured at different times), averaged.
fn comb(rows: &[Vec<f32>]) -> f64 {
    let (mut sum, mut n) = (0f64, 0f64);
    for y in 1..rows.len() - 1 {
        for x in 0..rows[y].len() {
            let (a, c, b) = (rows[y - 1][x], rows[y][x], rows[y + 1][x]);
            sum += ((c - a) * (c - b)).max(0.0) as f64;
            n += 1.0;
        }
    }
    sum / n
}

/// The RGBA path deinterlaces: interlaced frames come field by field (`second_field`, 20 ms apart at 25 frames/s)
/// and show far less combing than the woven frame.
#[test]
fn rgba_path_deinterlaces_field_by_field() {
    let Some(path) = interlaced_clip() else { return };
    // The woven picture: luma of the planar frame as decoded.
    let img = first_frame(&mut player(false, true), &path).planar.expect("planar");
    let (y, yi) = img.plane(0).expect("luma");
    let woven: Vec<Vec<f32>> = (0..yi.rows as usize).map(|r| y[r * yi.stride..][..yi.width as usize].iter().map(|v| *v as f32 / 255.0).collect()).collect();
    let woven_comb = comb(&woven);
    let rec = Recorder::default();
    let mut p = player(false, false);
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    opened(&rec, g);
    p.play();
    let mut seen = Vec::new();
    let mut combs = Vec::new();
    let ok = common::wait_until(T, || {
        if let Some(f) = p.latest_frame() {
            assert!(f.planar.is_none());
            seen.push((f.pts, f.second_field));
            let rows: Vec<Vec<f32>> = (0..f.height as usize)
                .map(|r| {
                    (0..f.width as usize)
                        .map(|x| {
                            let o = r * f.stride as usize + x * 4;
                            (0.2126 * f.rgba[o] as f32 + 0.7152 * f.rgba[o + 1] as f32 + 0.0722 * f.rgba[o + 2] as f32) / 255.0
                        })
                        .collect()
                })
                .collect();
            combs.push(comb(&rows));
        }
        seen.len() >= 16
    });
    assert!(ok, "frames: {seen:?}");
    let s = p.stats();
    p.dispose();
    let worst = combs.iter().cloned().fold(0.0, f64::max);
    eprintln!("woven comb {woven_comb:.6}, deinterlaced comb worst {worst:.6}; {s:?}");
    // Every frame handed over makes two fields: the second one converted, or dropped as late on a slow machine (so
    // this does not depend on the machine's speed). Two frames of slack for the one in flight at the snapshot.
    let frames = s.decoded - s.dropped_decoder;
    assert!(s.soft_deinterlaced + s.dropped_fields + 4 >= 2 * frames, "{s:?}");
    assert!(seen.iter().any(|(_, s)| *s) || s.dropped_fields > 0, "no second fields: {seen:?} {s:?}");
    for w in seen.windows(2) {
        let d = w[1].0 - w[0].0;
        if w[0].1 != w[1].1 && d > 0.0 && d < 0.03 {
            assert!((d - 0.02).abs() < 0.004, "field spacing {d}: {seen:?}");
        }
    }
    assert!(s.soft_deinterlaced >= 16 && s.planar == 0, "{s:?}");
    assert!(worst < woven_comb * 0.25, "comb {worst} vs woven {woven_comb}: {combs:?}");
}

#[test]
fn planar_frames_carry_the_rotation_and_the_shown_size() {
    let path = clip_or_skip!(Clip::SmallRotated);
    let f = first_frame(&mut player(true, true), &path);
    let img = f.planar.expect("planar");
    assert_eq!((f.width, f.height), (240, 320));
    // `-display_rotation 90` is counter-clockwise: three clockwise quarter turns, as the RGBA path rotates.
    assert_eq!((img.coded_width, img.coded_height, img.quarter_turns), (320, 240, 3));
}

/// 10-bit HEVC: P010 from the hardware decoder (high bits) or yuv420p10le from software (low bits).
#[test]
fn ten_bit_frames_arrive_as_sixteen_bit_planes() {
    let path = clip_or_skip!(Clip::Hd60Hevc10);
    for hardware in [false, true] {
        let mut p = player(hardware, true);
        let f = first_frame(&mut p, &path);
        let img = f.planar.expect("planar");
        assert_eq!((img.bits, img.sample_bytes()), (10, 2), "hardware={hardware}");
        assert_eq!(img.msb_aligned, img.layout == sa_player::PlaneLayout::SemiPlanar);
        let (y, yi) = img.plane(0).expect("luma");
        assert_eq!(yi.row_bytes, 1920 * 2);
        // Limited-range luma of the test pattern stays inside 64..=940 (10-bit), wherever the bits sit.
        let shift = if img.msb_aligned { 6 } else { 0 };
        let sample = |i: usize| (u16::from_le_bytes([y[i], y[i + 1]]) >> shift) as u32;
        assert!((0..1920).step_by(37).all(|x| (64..=940).contains(&sample(540 * yi.stride + x * 2))), "hardware={hardware}");
        p.dispose();
    }
}

/// AV1: FFmpeg's default decoder (libdav1d) has no hardware config, so hardware decoding goes through the native
/// `av1` decoder. Where the GPU cannot decode the stream (too small here for some GPUs, or no AV1 decoder) the
/// software decoder takes over; either way playback runs. `SA_TEST_EXPECT_AV1_HW=1` requires the GPU path.
#[test]
fn av1_decodes_on_the_gpu_through_the_native_decoder_or_falls_back_to_software() {
    let Some(path) = common::custom_clip(
        "av1-10bit-720p.mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30", "-t", "2", "-c:v", "libsvtav1", "-preset", "12",
            "-svtav1-params", "log=0", "-g", "30", "-pix_fmt", "yuv420p10le",
        ],
    ) else {
        eprintln!("skipped: no ffmpeg CLI");
        return;
    };
    for hardware in [true, false] {
        let rec = Recorder::default();
        let mut p = player(hardware, true);
        rec.attach(&mut p);
        let g = p.load(Some(MediaSource::File(path.clone())), true);
        opened(&rec, g);
        assert!(rec.wait_for(0, T, is_ended(g)).is_some(), "hardware={hardware}: {:?}", rec.snapshot());
        let s = p.stats();
        eprintln!("hardware={hardware}: {s:?}");
        assert!(s.decoded >= 55 && s.presented >= 25 && s.presented + s.dropped_late >= 50, "hardware={hardware}: {s:?}");
        if !hardware {
            assert_eq!(s.hardware, 0);
        } else if std::env::var("SA_TEST_EXPECT_AV1_HW").is_ok_and(|v| v == "1") {
            assert!(s.hardware >= 50, "{s:?}");
        }
        p.dispose();
    }
}

/// The first frame a player hands over after `start` (a load or a seek), then the frames up to `until` (pts).
fn frames_after(p: &mut FfmpegPlayer, until: f64) -> Vec<f64> {
    let mut seen = Vec::new();
    common::wait_until(T, || {
        if let Some(f) = p.latest_frame() {
            seen.push(f.pts);
        }
        seen.last().is_some_and(|t| *t >= until)
    });
    seen
}

/// A hardware decoder that fails on its first packets (simulated: each packet is consumed and fails, as on a GPU that
/// cannot decode the codec) is replaced by the software decoder, which starts again from the keyframe: the first
/// frame shown is the one at the position, not one after the packets the failed decoder consumed, and playback runs
/// on from there. The same when the failure happens while a seek waits for its frame, and with a user seek deferred
/// meanwhile. (On a machine without a hardware decoding device this runs in software and passes trivially.)
#[test]
fn a_hardware_fall_back_restarts_from_the_keyframe_at_the_position() {
    // Keyframes at 0, 1 and 2 s (30 fps, GOP 30).
    let path = clip_or_skip!(Clip::SmallVideoOnly);
    let new = || {
        let mut p = player(true, true);
        p.simulate_hardware_failure(true);
        let rec = Recorder::default();
        rec.attach(&mut p);
        (p, rec)
    };
    // A load: the frame at 0 first (the failed decoder consumed the keyframe and the frames after it).
    let (mut p, rec) = new();
    let g = p.load(Some(MediaSource::File(path.clone())), false);
    opened(&rec, g);
    let first = frame_where(&mut p, T, |_| true).expect("first frame");
    assert!(first.pts.abs() < 0.01, "first frame at {}", first.pts);
    std::thread::sleep(Duration::from_millis(300));
    assert!(p.latest_frame().is_none_or(|f| f.pts.abs() < 0.01), "paused: nothing else is shown");
    assert_eq!(p.stats().hardware, 0);
    p.play();
    let seen = frames_after(&mut p, 1.2);
    assert!(seen.first().is_some_and(|t| *t < 0.3) && seen.windows(2).all(|w| w[1] >= w[0]), "{seen:?}");
    assert!(seen.last().is_some_and(|t| *t >= 1.2), "{seen:?}");
    p.dispose();

    // A seek requested before the source opened: its frame (1.033 s, decoded from the keyframe at 1 s) first.
    let (mut p, rec) = new();
    let g = p.load(Some(MediaSource::File(path.clone())), false);
    p.seek(1.04);
    opened(&rec, g);
    let first = frame_where(&mut p, T, |_| true).expect("first frame");
    assert!((first.pts - 1.0 / 30.0 - 1.0).abs() < 0.01, "first frame at {}", first.pts);
    assert_eq!(p.position(), 1.04);
    p.play();
    let seen = frames_after(&mut p, 1.6);
    assert!(seen.first().is_some_and(|t| *t < 1.3) && seen.windows(2).all(|w| w[1] >= w[0]), "{seen:?}");
    p.dispose();

    // A seek while the load's frame is not on screen yet (deferred, or run at once if the frame came first): only the
    // frame at 0 and the target's frame are ever shown.
    let (mut p, rec) = new();
    let g = p.load(Some(MediaSource::File(path)), false);
    opened(&rec, g);
    p.seek(2.04);
    let target = 2.0 + 1.0 / 30.0;
    let mut seen = Vec::new();
    common::wait_until(T, || {
        if let Some(f) = p.latest_frame() {
            seen.push(f.pts);
        }
        seen.last().is_some_and(|t| (t - target).abs() < 0.01)
    });
    assert!(seen.iter().all(|t| t.abs() < 0.01 || (t - target).abs() < 0.01), "{seen:?}");
    assert!(seen.last().is_some_and(|t| (t - target).abs() < 0.01), "{seen:?}");
    assert_eq!(p.stats().hardware, 0);
    p.dispose();
}

/// The second field of an interlaced frame lands half a frame after its first field, also right after seeks (the
/// field period is not measured against a frame of the previous position).
#[test]
fn second_fields_stay_half_a_frame_after_the_first_across_seeks() {
    let Some(path) = interlaced_clip() else { return };
    let rec = Recorder::default();
    let mut p = player(false, true);
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    opened(&rec, g);
    let first = frame_where(&mut p, T, |_| true).expect("first frame");
    // 25 frames/s: first fields on a 40 ms grid, second fields 20 ms after them.
    let offset = first.pts.rem_euclid(0.04);
    p.play();
    let mut seen = Vec::new();
    for target in [0.4, 0.52, 0.6, 0.72, 0.84, 1.0, 1.08, 1.2] {
        p.seek(target);
        let started = Instant::now();
        let min_until = started + Duration::from_millis(200);
        let max_until = started + Duration::from_millis(700);
        let mut saw_second_after_seek = false;
        while Instant::now() < max_until && (!saw_second_after_seek || Instant::now() < min_until) {
            if let Some(f) = p.latest_frame() {
                if f.second_field && started.elapsed() >= Duration::from_millis(40) {
                    saw_second_after_seek = true;
                }
                seen.push((f.pts, f.second_field));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let seconds: Vec<f64> = seen.iter().filter(|(_, s)| *s).map(|(t, _)| *t).collect();
    assert!(!seconds.is_empty(), "{seen:?}");
    for t in &seconds {
        let phase = (t - offset).rem_euclid(0.04);
        assert!((phase - 0.02).abs() < 0.004, "second field at {t} (phase {phase}): {seen:?}");
    }
    p.dispose();
}

fn interlaced_clip() -> Option<std::path::PathBuf> {
    let c = common::custom_clip(
        "interlaced-tff.ts",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=50", "-t", "2", "-vf", "tinterlace=interleave_top,fieldorder=tff",
            "-c:v", "mpeg2video", "-flags", "+ildct+ilme", "-top", "1", "-b:v", "4M", "-f", "mpegts",
        ],
    );
    if c.is_none() {
        eprintln!("skipped: no ffmpeg CLI or encoder");
    }
    c
}

#[test]
fn without_hardware_decoding_nothing_is_decoded_on_the_gpu() {
    let path = clip_or_skip!(Clip::SmallH264);
    let mut p = player(false, false);
    first_frame(&mut p, &path);
    assert_eq!(p.stats().hardware, 0);
}
