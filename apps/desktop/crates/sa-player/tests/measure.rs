//! Dropped-frame measurement on 1080p clips (H.264 60 fps, HEVC 30 fps, HEVC 10-bit 60 fps, VP9 30 fps). Ignored
//! by default (each run plays real time):
//!
//! ```text
//! cargo test -p sa-player --release --test measure -- --ignored --nocapture --test-threads=1
//! ```
//!
//! A simulated UI thread takes every `FrameReady`, fetches the frame and copies it into a reused buffer, so the handoff
//! cost is part of the measurement: the RGBA frame (what the app does with `SharedPixelBuffer` without the GPU path),
//! or, for the `*_planar` variants, every plane of the YUV frame (`set_planar_output(true)`, what the app's GPU path
//! uploads at render time; the copy stands in for the driver's upload copy).

#![cfg(feature = "ffmpeg")]

mod common;

use std::time::{Duration, Instant};

use common::Clip;
use sa_player::ffmpeg_backend::{FfmpegPlayer, PlayerOptions};
use sa_player::{MediaPlayerBackend, MediaSource, PlayerEvent};

struct Report {
    clip: &'static str,
    audio: bool,
    planar: bool,
    seconds: f64,
    expected: f64,
    shown_by_ui: u64,
    presented: u64,
    dropped: u64,
    max_gap_ms: f64,
    /// Presentation jitter: how much later than its schedule (relative to the median) a frame reached the UI
    /// (p50 / p99 / max, ms).
    jitter_ms: (f64, f64, f64),
}

/// `samples` = (arrival, pts). Offsets `arrival - pts` are constant for perfectly paced frames.
fn jitter(samples: &[(Instant, f64)]) -> (f64, f64, f64) {
    let Some(&(t0, _)) = samples.first() else { return (0.0, 0.0, 0.0) };
    let mut off: Vec<f64> = samples.iter().map(|(t, pts)| t.duration_since(t0).as_secs_f64() - pts).collect();
    off.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = off[off.len() / 2];
    let late: Vec<f64> = off.iter().map(|o| ((o - median) * 1000.0).max(0.0)).collect();
    let pick = |q: f64| late[((late.len() - 1) as f64 * q) as usize];
    (pick(0.5), pick(0.99), *late.last().unwrap())
}

fn measure(c: Clip, fps: f64, audio: bool, planar: bool, play_for: Duration) -> Option<Report> {
    let path = common::clip(c)?;
    let (tx, rx) = crossbeam_channel::unbounded::<PlayerEvent>();
    let mut p = FfmpegPlayer::with_options(PlayerOptions { audio_output: audio, ..PlayerOptions::default() });
    p.set_volume(0.0);
    p.set_planar_output(planar);
    p.set_event_sink(Box::new(move |e| {
        let _ = tx.send(e);
    }));
    let g = p.load(Some(MediaSource::File(path)), false);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(PlayerEvent::Opened { generation, .. }) if generation == g => break,
            Ok(PlayerEvent::Failed { message, .. }) => panic!("{message}"),
            _ if Instant::now() > deadline => panic!("not opened"),
            _ => {}
        }
    }
    // Let the first frame arrive, then play.
    std::thread::sleep(Duration::from_millis(300));
    while p.latest_frame().is_some() {}
    let mut ui_buffer: Vec<u8> = Vec::new();
    let start_pos = p.position();
    p.play();
    let started = Instant::now();
    let mut shown = 0u64;
    let mut last_shown: Option<Instant> = None;
    let mut max_gap = 0f64;
    let mut samples: Vec<(Instant, f64)> = Vec::new();
    while started.elapsed() < play_for {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(PlayerEvent::FrameReady { .. }) => {
                if let Some(f) = p.latest_frame() {
                    match &f.planar {
                        Some(img) => {
                            assert!(planar, "planar frame without planar output");
                            ui_buffer.clear();
                            for i in 0..img.plane_count() {
                                let (bytes, _) = img.plane(i).expect("plane");
                                ui_buffer.extend_from_slice(bytes);
                            }
                        }
                        None => {
                            ui_buffer.resize(f.rgba.len(), 0);
                            ui_buffer.copy_from_slice(&f.rgba);
                        }
                    }
                    shown += 1;
                    let now = Instant::now();
                    samples.push((now, f.pts));
                    if let Some(prev) = last_shown {
                        max_gap = max_gap.max(now.duration_since(prev).as_secs_f64() * 1000.0);
                    }
                    last_shown = Some(now);
                }
            }
            Ok(PlayerEvent::Ended { .. }) => break,
            _ => {}
        }
    }
    let media_seconds = p.position() - start_pos;
    let stats = p.stats();
    let had_audio = p.has_audio_output();
    p.dispose();
    Some(Report {
        clip: c.file_name(),
        audio: audio && had_audio,
        planar,
        seconds: media_seconds,
        expected: media_seconds * fps,
        shown_by_ui: shown,
        presented: stats.presented,
        dropped: stats.dropped(),
        max_gap_ms: max_gap,
        jitter_ms: jitter(&samples),
    })
}

fn print(r: &Report) {
    let missing = (r.expected - r.shown_by_ui as f64).max(0.0);
    eprintln!(
        "{:<22} audio={:<5} planar={:<5} played={:>5.2}s expected={:>5.0} ui_shown={:>5} presented={:>5} dropped={:>4} missing={:>4.0} ({:.2}%) max_gap={:.1}ms late p50/p99/max={:.1}/{:.1}/{:.1}ms",
        r.clip,
        r.audio,
        r.planar,
        r.seconds,
        r.expected,
        r.shown_by_ui,
        r.presented,
        r.dropped,
        missing,
        100.0 * missing / r.expected.max(1.0),
        r.max_gap_ms,
        r.jitter_ms.0,
        r.jitter_ms.1,
        r.jitter_ms.2
    );
}

fn run(c: Clip, fps: f64, planar: bool) {
    for audio in [true, false] {
        let Some(r) = measure(c, fps, audio, planar, Duration::from_secs(8)) else {
            eprintln!("skipped: no ffmpeg CLI");
            return;
        };
        print(&r);
        assert!(r.seconds > 7.0, "played {}", r.seconds);
        // Smooth playback: at most 2 % of the frames are missing on screen.
        let missing = (r.expected - r.shown_by_ui as f64).max(0.0);
        assert!(missing <= r.expected * 0.02 + 2.0, "too many dropped frames");
    }
}

#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_1080p60_h264() {
    run(Clip::Hd60H264, 60.0, false);
}

#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_1080p60_h264_planar() {
    run(Clip::Hd60H264, 60.0, true);
}

#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_1080p30_hevc() {
    run(Clip::Hd30Hevc, 30.0, false);
}

#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_1080p30_hevc_planar() {
    run(Clip::Hd30Hevc, 30.0, true);
}

#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_1080p60_hevc_10bit() {
    run(Clip::Hd60Hevc10, 60.0, false);
}

#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_1080p60_hevc_10bit_planar() {
    run(Clip::Hd60Hevc10, 60.0, true);
}

#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_1080p30_vp9() {
    run(Clip::Hd30Vp9, 30.0, false);
}

#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_1080p30_vp9_planar() {
    run(Clip::Hd30Vp9, 30.0, true);
}

/// Seek latency (paused, exact seek: from `seek` to the target frame being available) on every 1080p clip.
#[test]
#[ignore = "real-time measurement"]
fn seek_latency_1080p() {
    for (c, fps) in [(Clip::Hd60H264, 60.0), (Clip::Hd30Hevc, 30.0), (Clip::Hd30Vp9, 30.0), (Clip::Hd60Hevc10, 60.0)] {
        let Some(path) = common::clip(c) else {
            eprintln!("skipped: no ffmpeg CLI");
            return;
        };
        let (tx, rx) = crossbeam_channel::unbounded::<PlayerEvent>();
        let mut p = FfmpegPlayer::with_options(PlayerOptions { audio_output: false, ..PlayerOptions::default() });
        p.set_event_sink(Box::new(move |e| {
            let _ = tx.send(e);
        }));
        let g = p.load(Some(MediaSource::File(path)), false);
        while !matches!(rx.recv_timeout(Duration::from_secs(10)), Ok(PlayerEvent::Opened { generation, .. }) if generation == g) {}
        let mut times = Vec::new();
        for i in 0..12 {
            let target = (i as f64 * 3.7) % 9.5 + 0.2;
            let t0 = Instant::now();
            p.seek(target);
            loop {
                if let Some(f) = p.latest_frame() {
                    if (f.pts - target).abs() <= 0.5 / fps + 1e-3 {
                        break;
                    }
                }
                assert!(t0.elapsed() < Duration::from_secs(5), "{c:?}: seek to {target} timed out");
                std::thread::sleep(Duration::from_millis(1));
            }
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        p.dispose();
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!("{:<22} seek latency median={:.0}ms max={:.0}ms", c.file_name(), times[times.len() / 2], times[times.len() - 1]);
        assert!(times[times.len() - 1] < 1500.0);
    }
}
