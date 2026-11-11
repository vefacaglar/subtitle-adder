//! CPU attribution of the video path (diagnostics, `SUBTITLE_ADDER_PLAYER_STATS=1` only; free otherwise).
//!
//! Counted process-wide, cumulative since the process started (diff two `[player-stats]` lines for a window):
//! * `cpu_proc` — the process's CPU time (user + kernel);
//! * `cpu_decode` — the video decode threads' own CPU time (each thread adds what it used since its last report,
//!   after every frame it handles);
//! * `cpu_ui` — the UI thread's, read by the caller of [`line`] on that thread;
//! * `cpu_other` — the rest (FFmpeg's frame threads, audio, presenter, stripe workers, driver threads);
//! * the GPU frame transfer (`xfer`: decode thread, around the download of a hardware frame) and the plane upload
//!   (`upload`: the UI's upload loop, sa-app `video_gl.rs`), each as count / wall ms / CPU ms, split into the first
//!   [`EARLY_FRAMES`] frames after a start (load, seek, decoder restart) and the rest.
//!
//! Thread CPU times come from `GetThreadTimes` / `GetProcessTimes` (100 ns units, charged at the scheduler's
//! accounting, so short sections read in coarse steps; long sums are what they are good for). Not measured on other
//! platforms (the fields read 0).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Frames after a start that count as "early" (about a quarter second at 60 fps).
pub const EARLY_FRAMES: u32 = 16;

/// Whether the counters run (`SUBTITLE_ADDER_PLAYER_STATS=1`).
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SUBTITLE_ADDER_PLAYER_STATS").is_some_and(|v| v == "1"))
}

/// CPU time of the calling thread (ns), `None` where it is not measured.
pub fn thread_cpu_ns() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: the pseudo handle of the calling thread; out-pointers to locals.
        unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) }.ok()?;
        Some((filetime(k) + filetime(u)) * 100)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// CPU time of the process (ns), `None` where it is not measured.
pub fn process_cpu_ns() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: the pseudo handle of this process; out-pointers to locals.
        unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }.ok()?;
        Some((filetime(k) + filetime(u)) * 100)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn filetime(t: windows::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

#[derive(Default)]
struct Acc {
    count: AtomicU64,
    wall_ns: AtomicU64,
    cpu_ns: AtomicU64,
}

impl Acc {
    fn add(&self, wall: u64, cpu: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.wall_ns.fetch_add(wall, Ordering::Relaxed);
        self.cpu_ns.fetch_add(cpu, Ordering::Relaxed);
    }

    fn text(&self) -> String {
        let ms = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1e6;
        format!("{}/{:.1}/{:.1}", self.count.load(Ordering::Relaxed), ms(&self.wall_ns), ms(&self.cpu_ns))
    }
}

/// One measured section: frames early after a start and the rest.
#[derive(Default)]
pub struct Section {
    early: Acc,
    rest: Acc,
}

impl Section {
    const fn new() -> Self {
        const fn acc() -> Acc {
            Acc { count: AtomicU64::new(0), wall_ns: AtomicU64::new(0), cpu_ns: AtomicU64::new(0) }
        }
        Self { early: acc(), rest: acc() }
    }

    fn text(&self) -> String {
        format!("{}+{}", self.early.text(), self.rest.text())
    }
}

/// The GPU → system memory transfer of hardware frames (decode thread).
pub static TRANSFER: Section = Section::new();
/// The UI's plane upload (sa-app `video_gl.rs`).
pub static UPLOAD: Section = Section::new();
static DECODE_CPU_NS: AtomicU64 = AtomicU64::new(0);

/// A running measurement of one section ([`Timer::start`] → [`Timer::finish`]); `None` while the counters are off.
pub struct Timer {
    wall: Instant,
    cpu: u64,
}

impl Timer {
    pub fn start() -> Option<Self> {
        enabled().then(|| Self { wall: Instant::now(), cpu: thread_cpu_ns().unwrap_or(0) })
    }

    /// Adds the time since [`Timer::start`] to `section` (`early`: within the first [`EARLY_FRAMES`] after a start).
    pub fn finish(self, section: &Section, early: bool) {
        let wall = self.wall.elapsed().as_nanos() as u64;
        let cpu = thread_cpu_ns().unwrap_or(0).saturating_sub(self.cpu);
        if early { &section.early } else { &section.rest }.add(wall, cpu);
    }
}

/// The CPU time a video decode thread used since its last report; call on that thread.
#[derive(Default)]
pub(crate) struct ThreadMeter {
    last: Option<u64>,
}

impl ThreadMeter {
    pub fn report(&mut self) {
        if !enabled() {
            return;
        }
        let Some(now) = thread_cpu_ns() else { return };
        if let Some(last) = self.last {
            DECODE_CPU_NS.fetch_add(now.saturating_sub(last), Ordering::Relaxed);
        }
        self.last = Some(now);
    }
}

/// The counters as one line for `[player-stats]`, called on the UI thread (its CPU time is `cpu_ui`). Seconds for
/// the CPU totals; `count/wall ms/cpu ms` early `+` rest for the sections.
pub fn line() -> String {
    let s = |ns: u64| ns as f64 / 1e9;
    let proc_ = process_cpu_ns().unwrap_or(0);
    let ui = thread_cpu_ns().unwrap_or(0);
    let decode = DECODE_CPU_NS.load(Ordering::Relaxed);
    format!(
        "cpu_proc={:.3} cpu_decode={:.3} cpu_ui={:.3} cpu_other={:.3} xfer={} upload={}",
        s(proc_),
        s(decode),
        s(ui),
        s(proc_.saturating_sub(decode + ui)),
        TRANSFER.text(),
        UPLOAD.text()
    )
}
