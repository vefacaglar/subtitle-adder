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
//! uploads at render time; the copy stands in for the driver's upload copy). The `*_rgba` variants play an
//! interlaced 1080i25 and an HDR10 1080p24 clip through the RGBA path, which deinterlaces / tone-maps on the CPU, and
//! `convert_rgba_ns_per_pixel_1080p` times that conversion.

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
    Some(measure_path(c.file_name(), path, fps, audio, planar, play_for))
}

fn measure_path(clip: &'static str, path: std::path::PathBuf, fps: f64, audio: bool, planar: bool, play_for: Duration) -> Report {
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
    eprintln!("{clip}: {}", p.diagnostics().unwrap_or_default());
    p.dispose();
    Report {
        clip,
        audio: audio && had_audio,
        planar,
        seconds: media_seconds,
        expected: media_seconds * fps,
        shown_by_ui: shown,
        presented: stats.presented,
        dropped: stats.dropped(),
        max_gap_ms: max_gap,
        jitter_ms: jitter(&samples),
    }
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

/// 1080i25: interlaced MPEG-2 (top field first, 50 fields/s) with audio.
fn clip_1080i25() -> Option<std::path::PathBuf> {
    common::custom_clip(
        "1080i25-mpeg2.ts",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=50", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "10",
            "-vf", "tinterlace=interleave_top,fieldorder=tff", "-c:v", "mpeg2video", "-flags", "+ildct+ilme", "-top", "1", "-b:v", "20M",
            "-c:a", "mp2", "-f", "mpegts",
        ],
    )
}

/// 1080p24 HDR10: HEVC 10-bit PQ / BT.2020 with mastering metadata and MaxCLL, with audio.
fn clip_1080p24_hdr10() -> Option<std::path::PathBuf> {
    common::custom_clip(
        "1080p24-hdr10.mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=24", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "10",
            "-c:v", "libx265", "-preset", "ultrafast", "-g", "48", "-x265-params",
            "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:hdr10=1:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400",
            "-pix_fmt", "yuv420p10le", "-color_primaries", "bt2020", "-color_trc", "smpte2084", "-colorspace", "bt2020nc", "-c:a", "aac",
        ],
    )
}

/// 2160p HDR10 at `rate` frames/s (HEVC 10-bit PQ / BT.2020, MaxCLL 1000), with audio, cached as `name`.
fn clip_2160p_hdr10(name: &str, rate: u32) -> Option<std::path::PathBuf> {
    let src = format!("testsrc2=size=3840x2160:rate={rate}");
    common::custom_clip(
        name,
        &[
            "-f", "lavfi", "-i", &src, "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "10",
            "-c:v", "libx265", "-preset", "ultrafast", "-g", "60", "-x265-params",
            "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:hdr10=1:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400",
            "-pix_fmt", "yuv420p10le", "-color_primaries", "bt2020", "-color_trc", "smpte2084", "-colorspace", "bt2020nc", "-c:a", "aac",
        ],
    )
}

/// The RGBA path (software renderer) on `path`: `rate` pictures per second (fields for interlaced clips); at most
/// `missing_share` of them may be missing on screen.
fn run_rgba(name: &'static str, path: Option<std::path::PathBuf>, rate: f64, missing_share: f64) {
    let Some(path) = path else {
        eprintln!("skipped: no ffmpeg CLI or encoder for {name}");
        return;
    };
    for audio in [true, false] {
        let r = measure_path(name, path.clone(), rate, audio, false, Duration::from_secs(8));
        print(&r);
        assert!(r.seconds > 7.0, "played {}", r.seconds);
        let missing = (r.expected - r.shown_by_ui as f64).max(0.0);
        assert!(missing <= r.expected * missing_share + 2.0, "too many dropped pictures");
    }
}

/// 1080i25 through the RGBA path: deinterlaced on the CPU, 50 fields/s.
#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_1080i25_rgba() {
    run_rgba("1080i25-mpeg2.ts", clip_1080i25(), 50.0, 0.02);
}

/// 1080p24 HDR10 through the RGBA path: tone-mapped on the CPU.
#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_1080p24_hdr10_rgba() {
    run_rgba("1080p24-hdr10.mkv", clip_1080p24_hdr10(), 24.0, 0.02);
}

/// 2160p24 HDR10 through the RGBA path: tone-mapped at half size (1920 × 1080 buffers).
#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_2160p24_hdr10_rgba() {
    run_rgba("2160p24-hdr10.mkv", clip_2160p_hdr10("2160p24-hdr10.mkv", 24), 24.0, 0.02);
}

/// 2160p60 HDR10 through the RGBA path. Reported, not asserted beyond "playback runs on": where decoding plus the
/// conversion take longer than a frame lasts, frames are dropped (the clock never waits for video).
#[test]
#[ignore = "real-time measurement"]
fn dropped_frames_2160p60_hdr10_rgba() {
    run_rgba("2160p60-hdr10.mkv", clip_2160p_hdr10("2160p60-hdr10.mkv", 60), 60.0, 1.0);
}

/// A 1920 × 1080 10-bit frame (P010 when `msb`, else yuv420p10le) of smooth gradients, HDR10.
fn hdr_frame(msb: bool) -> sa_player::PlanarImage {
    use sa_player::{ColorInfo, PlanarFormat, PlaneInfo, PlaneLayout, Primaries, Transfer, YuvMatrix};
    let (w, h) = (1920usize, 1080usize);
    let shift = if msb { 6 } else { 0 };
    let word = |v: usize| ((((v % 877) + 64) as u16) << shift).to_le_bytes();
    let mut y = vec![0u8; w * 2 * h];
    for r in 0..h {
        for x in 0..w {
            y[(r * w + x) * 2..][..2].copy_from_slice(&word(x / 2 + r / 3));
        }
    }
    let (cw, ch) = (w / 2, h / 2);
    let color = ColorInfo { matrix: YuvMatrix::Bt2020, full_range: false, transfer: Transfer::Pq, primaries: Primaries::Bt2020, peak_nits: 1000.0 };
    let info = |width: usize, rows: usize, comps: usize| PlaneInfo { width: width as u32, rows: rows as u32, stride: width * comps * 2, row_bytes: width * comps * 2 };
    let mut planes: Vec<Vec<u8>> = vec![y];
    let (format, infos) = if msb {
        let mut c = vec![0u8; cw * 4 * ch];
        for r in 0..ch {
            for x in 0..cw {
                c[(r * cw + x) * 4..][..2].copy_from_slice(&word(300 + x / 3));
                c[(r * cw + x) * 4 + 2..][..2].copy_from_slice(&word(500 + r / 2));
            }
        }
        planes.push(c);
        (PlanarFormat { layout: PlaneLayout::SemiPlanar, bits: 10, msb_aligned: true, chroma_shift: (1, 1), swap_uv: false }, vec![info(w, h, 1), info(cw, ch, 2)])
    } else {
        for k in 0..2 {
            let mut c = vec![0u8; cw * 2 * ch];
            for r in 0..ch {
                for x in 0..cw {
                    c[(r * cw + x) * 2..][..2].copy_from_slice(&word(300 + k * 200 + x / 3 + r / 2));
                }
            }
            planes.push(c);
        }
        (PlanarFormat { layout: PlaneLayout::Planar, bits: 10, msb_aligned: false, chroma_shift: (1, 1), swap_uv: false }, vec![info(w, h, 1), info(cw, ch, 1), info(cw, ch, 1)])
    };
    let mut ptrs: [Option<(*const u8, PlaneInfo)>; 3] = [None; 3];
    for (i, p) in planes.iter().enumerate() {
        ptrs[i] = Some((p.as_ptr(), infos[i]));
    }
    // SAFETY: the vectors move into the owner; their buffers stay in place.
    unsafe { sa_player::PlanarImage::new(format, (w as u32, h as u32), color, 0, Some(sa_player::Interlace { top_field_first: true }), ptrs, Box::new(planes)) }
}

/// ns per output pixel of the RGBA path's conversion (`soft::convert_rgba`) and deinterlacer at 1080p (run in
/// release: `cargo test -p sa-player --release --test measure -- --ignored --nocapture convert_rgba`).
#[test]
#[ignore = "micro-benchmark"]
fn convert_rgba_ns_per_pixel_1080p() {
    use sa_player::color::{ColorParams, ToneLuts};
    use sa_player::soft;
    let px = 1920.0 * 1080.0;
    let time = |f: &mut dyn FnMut()| {
        f();
        let runs = 20;
        let t = Instant::now();
        for _ in 0..runs {
            f();
        }
        t.elapsed().as_secs_f64() * 1e9 / runs as f64 / px
    };
    let mut dst = vec![0u8; 1920 * 1080 * 4];
    let pool = soft::StripePool::new(soft::max_stripes() - 1, std::sync::Arc::new(|| Box::new(()) as Box<dyn std::any::Any>));
    eprintln!("{} hardware threads, up to {} stripes", std::thread::available_parallelism().map_or(1, |n| n.get()), soft::max_stripes());
    for msb in [false, true] {
        let img = hdr_frame(msb);
        let fmt = if msb { "P010" } else { "yuv420p10le" };
        for (mode, transfer) in [("PQ", sa_player::Transfer::Pq), ("HLG", sa_player::Transfer::Hlg), ("SDR", sa_player::Transfer::Sdr)] {
            let mut p = ColorParams::for_image(&img);
            if transfer != sa_player::Transfer::Pq {
                p = ColorParams::new(transfer, if transfer == sa_player::Transfer::Sdr { sa_player::Primaries::Bt709 } else { img.primaries }, img.peak_nits);
            }
            let luts = ToneLuts::new(&p);
            let opts = soft::ConvertOptions::default();
            let stripes = soft::stripes_for(1920 * 1080).min(pool.workers() + 1);
            let one = time(&mut || assert!(soft::convert_rgba(&img, 1920, &luts, opts, &mut dst, None)));
            let many = time(&mut || assert!(soft::convert_rgba(&img, 1920, &luts, opts, &mut dst, Some(&pool))));
            let mut d = vec![0u8; 2560 * 1080 * 4];
            let wide = time(&mut || assert!(soft::convert_rgba(&img, 2560, &luts, opts, &mut d, Some(&pool))));
            // Half size: ns per source pixel.
            let half = soft::ConvertOptions { half: true, ..opts };
            let halved = time(&mut || assert!(soft::convert_rgba(&img, 960, &luts, half, &mut dst, Some(&pool))));
            eprintln!(
                "convert_rgba {fmt} {mode}: {one:.2} ns/px on 1 thread, {many:.2} ns/px on {stripes} stripes, {wide:.2} ns/output px stretched to 2560, {halved:.2} ns/source px at half size"
            );
        }
        let prev = hdr_frame(msb);
        let mut planes: Vec<Vec<u8>> = (0..img.plane_count()).map(|i| vec![0u8; img.plane(i).unwrap().0.len()]).collect();
        let strides: Vec<usize> = (0..img.plane_count()).map(|i| img.plane(i).unwrap().1.stride).collect();
        let mut run = |prev: Option<&sa_player::PlanarImage>| {
            let mut out: Vec<soft::PlaneMut> = planes.iter_mut().zip(&strides).map(|(d, s)| soft::PlaneMut { data: d.as_mut_slice(), stride: *s }).collect();
            soft::deinterlace_planes(&img, prev, false, &mut out);
        };
        let bob = time(&mut || run(None));
        let adaptive = time(&mut || run(Some(&prev)));
        eprintln!("deinterlace_planes {fmt}: {bob:.2} ns/px without a previous frame, {adaptive:.2} ns/px motion-adaptive");
    }
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
