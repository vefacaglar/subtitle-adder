//! Test clips generated with the FFmpeg CLI (same commands as `tools/fixtures/player/gen-clips.sh`), cached under
//! `%TEMP%/sa-player-fixtures-v1`, plus an event recorder for the player.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sa_player::{LoadGeneration, MediaPlayerBackend, PlayerEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clip {
    SmallH264,
    SmallVideoOnly,
    SmallRotated,
    Tone,
    Hd60H264,
    Hd30Hevc,
    Hd30Vp9,
    Hd60Hevc10,
}

impl Clip {
    pub fn file_name(self) -> &'static str {
        match self {
            Clip::SmallH264 => "small-h264.mp4",
            Clip::SmallVideoOnly => "small-video-only.mp4",
            Clip::SmallRotated => "small-rotated.mp4",
            Clip::Tone => "tone.wav",
            Clip::Hd60H264 => "1080p60-h264.mp4",
            Clip::Hd30Hevc => "1080p30-hevc.mkv",
            Clip::Hd30Vp9 => "1080p30-vp9.mkv",
            Clip::Hd60Hevc10 => "1080p60-hevc10.mp4",
        }
    }

    fn args(self, dir: &Path) -> Vec<String> {
        let src = |size: &str, rate: u32| -> Vec<String> {
            vec![
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                format!("testsrc2=size={size}:rate={rate}"),
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                "sine=frequency=440:sample_rate=48000".into(),
            ]
        };
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let mut a = match self {
            Clip::SmallH264 => [src("320x240", 30), s(&["-t", "3", "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "2"])].concat(),
            Clip::SmallVideoOnly => s(&["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30", "-t", "3", "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p"]),
            Clip::SmallRotated => {
                let input = dir.join(Clip::SmallH264.file_name());
                vec!["-display_rotation:v:0".into(), "90".into(), "-i".into(), input.to_string_lossy().into_owned(), "-c".into(), "copy".into()]
            }
            Clip::Tone => s(&["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "3", "-c:a", "pcm_s16le"]),
            Clip::Hd60H264 => [src("1920x1080", 60), s(&["-t", "10", "-c:v", "libx264", "-preset", "veryfast", "-g", "120", "-pix_fmt", "yuv420p", "-c:a", "aac"])].concat(),
            Clip::Hd30Hevc => [src("1920x1080", 30), s(&["-t", "10", "-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "log-level=error", "-g", "60", "-pix_fmt", "yuv420p", "-c:a", "aac"])].concat(),
            Clip::Hd30Vp9 => [src("1920x1080", 30), s(&["-t", "10", "-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-row-mt", "1", "-b:v", "4M", "-g", "60", "-pix_fmt", "yuv420p", "-c:a", "libopus"])].concat(),
            Clip::Hd60Hevc10 => [src("1920x1080", 60), s(&["-t", "10", "-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "log-level=error", "-g", "120", "-pix_fmt", "yuv420p10le", "-c:a", "aac"])].concat(),
        };
        let mut out = s(&["-y", "-hide_banner", "-loglevel", "error"]);
        out.append(&mut a);
        out
    }
}

/// The FFmpeg CLI: `SA_TEST_FFMPEG`, `$FFMPEG_DIR/bin/ffmpeg`, or `ffmpeg` on PATH.
pub fn ffmpeg_cli() -> Option<PathBuf> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
            let mut candidates = Vec::new();
            if let Ok(p) = std::env::var("SA_TEST_FFMPEG") {
                candidates.push(PathBuf::from(p));
            }
            if let Ok(d) = std::env::var("FFMPEG_DIR") {
                candidates.push(Path::new(&d).join("bin").join(exe));
            }
            candidates.push(PathBuf::from(exe));
            candidates.into_iter().find(|c| Command::new(c).arg("-version").output().is_ok_and(|o| o.status.success()))
        })
        .clone()
}

/// The encoders of the FFmpeg CLI (`ffmpeg -encoders`).
fn encoders(ff: &Path) -> &'static [String] {
    static LIST: OnceLock<Vec<String>> = OnceLock::new();
    LIST.get_or_init(|| {
        let Ok(out) = Command::new(ff).args(["-hide_banner", "-encoders"]).output() else { return Vec::new() };
        // Lines look like " V....D libx264              libx264 H.264 ...".
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_whitespace().nth(1).map(str::to_owned)).collect()
    })
}

/// Whether every encoder `args` names (`-c:v X`, `-c:a X`, `-c X`) is in the FFmpeg CLI; prints the skip reason when
/// not (an LGPL-only build has no libx265 or libsvtav1, for example).
fn encoders_present(ff: &Path, name: &str, args: &[String]) -> bool {
    let list = encoders(ff);
    for w in args.windows(2) {
        if matches!(w[0].as_str(), "-c:v" | "-c:a" | "-c" | "-vcodec" | "-acodec") && w[1] != "copy" && !list.iter().any(|e| *e == w[1]) {
            eprintln!("skipped: the ffmpeg CLI has no {} encoder to generate {name}", w[1]);
            return false;
        }
    }
    true
}

pub fn fixtures_dir() -> PathBuf {
    let d = std::env::temp_dir().join("sa-player-fixtures-v1");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// The clip's path, generated on first use; `None` when no FFmpeg CLI or no encoder for it is available (the test
/// then skips).
pub fn clip(c: Clip) -> Option<PathBuf> {
    static LOCK: Mutex<()> = Mutex::new(());
    let ff = ffmpeg_cli()?;
    if c == Clip::SmallRotated {
        clip(Clip::SmallH264)?;
    }
    let dir = fixtures_dir();
    let path = dir.join(c.file_name());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if path.exists() {
        return Some(path);
    }
    let tmp = dir.join(format!("tmp-{}-{}", std::process::id(), c.file_name()));
    let mut args = c.args(&dir);
    if !encoders_present(&ff, c.file_name(), &args) {
        return None;
    }
    args.push(tmp.to_string_lossy().into_owned());
    let status = Command::new(&ff).args(&args).status().ok()?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        panic!("ffmpeg failed to generate {}", c.file_name());
    }
    if std::fs::rename(&tmp, &path).is_err() {
        // Another test process won the race.
        let _ = std::fs::remove_file(&tmp);
    }
    Some(path)
}

/// A small clip generated by `args` (FFmpeg CLI arguments between the global options and the output), cached as
/// `name` in the fixtures folder; `None` without an FFmpeg CLI or without an encoder `args` name (the test then
/// skips).
pub fn custom_clip(name: &str, args: &[&str]) -> Option<PathBuf> {
    static LOCK: Mutex<()> = Mutex::new(());
    let ff = ffmpeg_cli()?;
    let path = fixtures_dir().join(name);
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if path.exists() {
        return Some(path);
    }
    if !encoders_present(&ff, name, &args.iter().map(|a| a.to_string()).collect::<Vec<_>>()) {
        return None;
    }
    let tmp = fixtures_dir().join(format!("tmp-{}-{name}", std::process::id()));
    let status = Command::new(&ff).args(["-y", "-hide_banner", "-loglevel", "error"]).args(args).arg(&tmp).status().ok()?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        panic!("ffmpeg failed to generate {name}");
    }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Some(path)
}

/// One second of the 320×240 test pattern as uncompressed `pix_fmt` frames (NUT keeps every pixel format), so the
/// decoder hands that format over unchanged.
pub fn raw_clip(pix_fmt: &str) -> Option<PathBuf> {
    custom_clip(
        &format!("raw-{pix_fmt}.nut"),
        &["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=10", "-t", "1", "-c:v", "rawvideo", "-pix_fmt", pix_fmt, "-f", "nut"],
    )
}

#[macro_export]
macro_rules! clip_or_skip {
    ($c:expr) => {
        match common::clip($c) {
            Some(p) => p,
            None => {
                eprintln!("skipped: no ffmpeg CLI or encoder to generate the test clip");
                return;
            }
        }
    };
}

/// Records every event a player posts.
#[derive(Clone, Default)]
pub struct Recorder {
    pub events: Arc<Mutex<Vec<(Instant, PlayerEvent)>>>,
}

impl Recorder {
    pub fn attach(&self, player: &mut dyn MediaPlayerBackend) {
        let events = self.events.clone();
        player.set_event_sink(Box::new(move |e| events.lock().unwrap().push((Instant::now(), e))));
    }

    pub fn snapshot(&self) -> Vec<PlayerEvent> {
        self.events.lock().unwrap().iter().map(|(_, e)| e.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    /// Waits until an event matching `f` was recorded (searching from index `from`).
    pub fn wait_for(&self, from: usize, timeout: Duration, f: impl Fn(&PlayerEvent) -> bool) -> Option<PlayerEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let ev = self.events.lock().unwrap();
                if let Some((_, e)) = ev.iter().skip(from).find(|(_, e)| f(e)) {
                    return Some(e.clone());
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

pub fn is_opened(g: LoadGeneration) -> impl Fn(&PlayerEvent) -> bool {
    move |e| matches!(e, PlayerEvent::Opened { generation, .. } if *generation == g)
}

pub fn is_ended(g: LoadGeneration) -> impl Fn(&PlayerEvent) -> bool {
    move |e| matches!(e, PlayerEvent::Ended { generation } if *generation == g)
}

pub fn is_frame(g: LoadGeneration) -> impl Fn(&PlayerEvent) -> bool {
    move |e| matches!(e, PlayerEvent::FrameReady { generation } if *generation == g)
}

/// Polls `cond` until it holds or `timeout` passes.
pub fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}
