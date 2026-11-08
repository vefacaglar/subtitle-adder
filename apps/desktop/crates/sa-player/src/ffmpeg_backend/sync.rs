//! The master clock and a small wake-up primitive.

use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

/// A broadcast wake-up: waiters sleep until `notify` or a timeout (spurious wake-ups are harmless everywhere it is
/// used, since every waiter re-checks its condition).
#[derive(Default)]
pub(crate) struct Notify {
    counter: Mutex<u64>,
    cv: Condvar,
}

impl Notify {
    pub fn notify(&self) {
        let mut c = self.counter.lock();
        *c = c.wrapping_add(1);
        self.cv.notify_all();
    }

    pub fn wait(&self, timeout: Duration) {
        let mut c = self.counter.lock();
        let seen = *c;
        let deadline = Instant::now() + timeout;
        while *c == seen {
            if self.cv.wait_until(&mut c, deadline).timed_out() {
                break;
            }
        }
    }
}

#[derive(Debug)]
struct ClockState {
    /// Playback requested (and not ended).
    playing: bool,
    /// The decoders are ready (source opened); the clock never runs before.
    ready: bool,
    /// Held after open/seek until the first frame is on screen (and the audio may start).
    held: bool,
    /// The audio output drives the clock.
    audio_master: bool,
    speed: f64,
    anchor_pts: f64,
    anchor_at: Instant,
    /// Audio master: the clock never runs past the end of the samples handed to the device.
    cap: Option<f64>,
    /// Monotonic floor while running.
    last: f64,
    /// Audio master: the clock has been anchored to the device since the last start/jump (later anchors only slew).
    audio_synced: bool,
}

impl ClockState {
    fn running(&self) -> bool {
        self.playing && self.ready && !self.held
    }

    fn value_at(&mut self, t: Instant) -> f64 {
        if !self.running() {
            return self.anchor_pts;
        }
        let dt = if t >= self.anchor_at {
            t.duration_since(self.anchor_at).as_secs_f64()
        } else {
            -(self.anchor_at.duration_since(t).as_secs_f64())
        };
        let mut v = self.anchor_pts + dt * self.speed;
        if let Some(cap) = self.cap {
            v = v.min(cap);
        }
        if v < self.last {
            v = self.last;
        }
        self.last = v;
        v
    }

    fn reanchor(&mut self, now: Instant) {
        let v = self.value_at(now);
        self.anchor_pts = v;
        self.anchor_at = now;
        self.last = v;
    }
}

/// The playback clock (seconds of media time). The audio clock is master when an audio output runs; otherwise a
/// monotonic wall clock.
pub(crate) struct Clock {
    s: Mutex<ClockState>,
}

impl Clock {
    pub fn new(start: f64) -> Self {
        Self {
            s: Mutex::new(ClockState {
                playing: false,
                ready: false,
                held: true,
                audio_master: false,
                speed: 1.0,
                anchor_pts: start,
                anchor_at: Instant::now(),
                cap: None,
                last: start,
                audio_synced: false,
            }),
        }
    }

    pub fn now(&self) -> f64 {
        self.s.lock().value_at(Instant::now())
    }

    pub fn is_running(&self) -> bool {
        self.s.lock().running()
    }

    pub fn is_audio_master(&self) -> bool {
        self.s.lock().audio_master
    }

    /// The source opened: the clock may run from now on.
    pub fn set_ready(&self, audio_master: bool) {
        let mut s = self.s.lock();
        let now = Instant::now();
        s.anchor_at = now;
        s.ready = true;
        s.audio_master = audio_master;
        s.audio_synced = false;
        s.cap = if audio_master { Some(s.anchor_pts) } else { None };
    }

    pub fn set_playing(&self, playing: bool) {
        let mut s = self.s.lock();
        if s.playing == playing {
            return;
        }
        let now = Instant::now();
        s.reanchor(now);
        s.playing = playing;
        s.audio_synced = false;
        if playing && s.audio_master {
            // Wait for the audio callback to anchor the clock.
            s.cap = Some(s.anchor_pts);
        }
    }

    /// Jump (seek, end of stream): the clock stands at `pts` and is held until the next frame is shown.
    pub fn jump(&self, pts: f64, hold: bool) {
        let mut s = self.s.lock();
        s.anchor_pts = pts;
        s.anchor_at = Instant::now();
        s.last = pts;
        s.held = hold;
        s.audio_synced = false;
        s.cap = if s.audio_master { Some(pts) } else { None };
    }

    /// The first frame after open/seek is on screen.
    pub fn release(&self) {
        let mut s = self.s.lock();
        if !s.held {
            return;
        }
        s.held = false;
        s.anchor_at = Instant::now();
        if s.audio_master {
            s.cap = Some(s.anchor_pts);
        }
    }

    /// Audio master: the sample at `pts` is heard at `heard_at`; the samples written end at `end_pts`.
    ///
    /// The first anchor after a start or jump snaps the clock to the device. Later ones only slew it: device
    /// timestamps wobble by a few milliseconds per callback, and following them exactly would show frames a frame
    /// early or late. An error beyond [`Self::AUDIO_SNAP`] (a glitch, a device change) snaps again.
    pub fn audio_anchor(&self, pts: f64, heard_at: Instant, end_pts: f64) {
        self.audio_anchor_at(pts, heard_at, end_pts, Instant::now());
    }

    /// Errors above this snap the clock to the device; smaller ones are corrected by [`Self::AUDIO_SLEW`] per anchor.
    pub const AUDIO_SNAP: f64 = 0.06;
    pub const AUDIO_SLEW: f64 = 0.08;

    fn audio_anchor_at(&self, pts: f64, heard_at: Instant, end_pts: f64, now: Instant) {
        let mut s = self.s.lock();
        if !s.audio_master || !s.running() {
            return;
        }
        s.cap = Some(end_pts.max(pts));
        if !s.audio_synced {
            s.audio_synced = true;
            s.anchor_pts = pts;
            s.anchor_at = heard_at;
            return;
        }
        let since = |a: Instant, b: Instant| if a >= b { a.duration_since(b).as_secs_f64() } else { -b.duration_since(a).as_secs_f64() };
        // Media time being heard now according to the device, and according to the running clock.
        let device = pts + since(now, heard_at) * s.speed;
        let clock = s.anchor_pts + since(now, s.anchor_at) * s.speed;
        let err = device - clock;
        if err.abs() > Self::AUDIO_SNAP {
            s.anchor_pts = pts;
            s.anchor_at = heard_at;
        } else {
            s.anchor_pts = clock + err * Self::AUDIO_SLEW;
            s.anchor_at = now;
        }
    }

    /// Audio has nothing more to play (end of the audio stream or a failed device): run on the wall clock.
    pub fn audio_finished(&self) {
        let mut s = self.s.lock();
        if !s.audio_master {
            return;
        }
        let now = Instant::now();
        s.reanchor(now);
        s.audio_master = false;
        s.cap = None;
    }

    /// Re-enables (or disables) the audio clock, e.g. after a seek back from where the audio had run out.
    pub fn set_audio_master(&self, on: bool) {
        let mut s = self.s.lock();
        if s.audio_master == on {
            return;
        }
        let now = Instant::now();
        s.reanchor(now);
        s.audio_master = on;
        s.audio_synced = false;
        s.cap = if on { Some(s.anchor_pts) } else { None };
    }

    pub fn set_speed(&self, speed: f64) {
        let mut s = self.s.lock();
        let now = Instant::now();
        s.reanchor(now);
        s.speed = speed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_runs_only_when_ready_playing_and_released() {
        let c = Clock::new(5.0);
        c.set_playing(true);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(c.now(), 5.0, "not ready");
        c.set_ready(false);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(c.now(), 5.0, "held");
        c.release();
        std::thread::sleep(Duration::from_millis(50));
        let v = c.now();
        assert!(v > 5.03 && v < 5.5, "{v}");
        c.set_playing(false);
        let p = c.now();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(c.now(), p);
    }

    #[test]
    fn audio_master_is_capped_by_the_written_samples() {
        let c = Clock::new(0.0);
        c.set_ready(true);
        c.release();
        c.set_playing(true);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(c.now(), 0.0, "waits for audio");
        c.audio_anchor(1.0, Instant::now(), 1.01);
        std::thread::sleep(Duration::from_millis(40));
        assert!((c.now() - 1.01).abs() < 1e-9, "capped");
        c.audio_finished();
        std::thread::sleep(Duration::from_millis(30));
        assert!(c.now() > 1.02);
    }

    #[test]
    fn small_device_wobble_is_slewed_and_large_errors_snap() {
        let c = Clock::new(0.0);
        c.set_ready(true);
        c.release();
        c.set_playing(true);
        let t0 = Instant::now();
        // First anchor snaps.
        c.audio_anchor_at(10.0, t0, 20.0, t0);
        assert!((c.s.lock().anchor_pts - 10.0).abs() < 1e-12);
        // 10 ms later the device claims 4 ms more than the clock: only 8 % of it is applied.
        let t1 = t0 + Duration::from_millis(10);
        c.audio_anchor_at(10.014, t1, 20.0, t1);
        let a = c.s.lock().anchor_pts;
        assert!((a - (10.010 + 0.004 * Clock::AUDIO_SLEW)).abs() < 1e-9, "{a}");
        // A 200 ms jump (device glitch) snaps.
        let t2 = t1 + Duration::from_millis(10);
        c.audio_anchor_at(10.3, t2, 20.0, t2);
        assert!((c.s.lock().anchor_pts - 10.3).abs() < 1e-12);
        // A jump resets the sync: the next anchor snaps even for a small error.
        c.jump(5.0, false);
        let t3 = Instant::now();
        c.audio_anchor_at(5.01, t3, 6.0, t3);
        assert!((c.s.lock().anchor_pts - 5.01).abs() < 1e-12);
    }

    #[test]
    fn clock_is_monotonic_across_anchors() {
        let c = Clock::new(0.0);
        c.set_ready(true);
        c.release();
        c.set_playing(true);
        c.audio_anchor(2.0, Instant::now(), 3.0);
        std::thread::sleep(Duration::from_millis(10));
        let a = c.now();
        // A later anchor that would move the clock back a little is ignored until it catches up.
        c.audio_anchor(1.995, Instant::now(), 3.0);
        assert!(c.now() >= a);
        c.jump(0.5, true);
        assert_eq!(c.now(), 0.5);
    }
}
