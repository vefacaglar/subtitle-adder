//! `PlaybackTimeline` (win-media §4.1 = mac-viewmodels §7.2): keeps the displayed position stable while an
//! asynchronous seek completes.
//!
//! The two reference implementations agree except for a non-finite reported position: the C# one clamps it to 0,
//! the Swift one keeps the shown position (an item that is not ready yet). The Swift behavior is kept for both
//! platforms: the FFmpeg backend never reports a non-finite position, so on Windows the difference is unobservable,
//! and "not ready" must not jump the playhead to 0 on macOS.

use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct PlaybackTimeline {
    pub position: f64,
    pub duration: f64,
    pending: Option<f64>,
    requested_at: Instant,
}

impl Default for PlaybackTimeline {
    fn default() -> Self {
        Self { position: 0.0, duration: 0.0, pending: None, requested_at: Instant::now() }
    }
}

impl PlaybackTimeline {
    /// A pending seek is kept while the player reports > 0.25 s away and less than 2 s passed.
    pub const SEEK_TOLERANCE: f64 = 0.25;
    pub const SEEK_HOLD: Duration = Duration::from_secs(2);

    pub fn new() -> Self {
        Self::default()
    }

    /// `position = duration = 0`, `pending = None`.
    pub fn reset(&mut self) {
        self.position = 0.0;
        self.duration = 0.0;
        self.pending = None;
    }

    /// A seek was requested and the player has not reported a position near it yet.
    pub fn is_settling(&self) -> bool {
        self.pending.is_some()
    }

    /// Clamps, records a pending seek and returns the target.
    pub fn seek(&mut self, seconds: f64, now: Instant) -> f64 {
        self.position = Self::clamp(seconds, self.duration);
        self.pending = Some(self.position);
        self.requested_at = now;
        self.position
    }

    /// Poll update from the player.
    pub fn observe(&mut self, position: f64, duration: f64, now: Instant) {
        self.duration = if duration.is_finite() { duration.max(0.0) } else { 0.0 };
        if !position.is_finite() {
            return;
        }
        let reported = Self::clamp(position, self.duration);
        if let Some(pending) = self.pending {
            let target = Self::clamp(pending, self.duration);
            let elapsed = now.saturating_duration_since(self.requested_at);
            if (reported - target).abs() > Self::SEEK_TOLERANCE && elapsed < Self::SEEK_HOLD {
                self.position = target;
                return;
            }
            self.pending = None;
        }
        self.position = reported;
    }

    /// Non-finite → 0; else `clamp(s, 0, d > 0 ? d : MAX)`.
    pub fn clamp(seconds: f64, duration: f64) -> f64 {
        if !seconds.is_finite() {
            return 0.0;
        }
        let upper = if duration > 0.0 { duration } else { f64::MAX };
        seconds.max(0.0).min(upper)
    }

    /// `MM:SS`, or `HH:MM:SS` when `hours` or ≥ 3600 s.
    pub fn format_time(seconds: f64, hours: bool) -> String {
        // .NET TimeSpan.MaxValue.TotalSeconds
        const MAX_SECONDS: f64 = 922_337_203_685.477_580_7;
        let s = if seconds.is_finite() { seconds } else { 0.0 };
        let total = s.clamp(0.0, MAX_SECONDS).floor() as i64;
        if hours || total >= 3600 {
            format!("{:02}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
        } else {
            format!("{:02}:{:02}", total / 60, total % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    // --- PlaybackTimelineTests.cs (win-media §5.2) ---

    #[test]
    fn subtitle_seek_updates_display_immediately_and_ignores_the_old_paused_frame() {
        let t0 = Instant::now();
        let mut t = PlaybackTimeline::new();
        t.observe(10.0, 300.0, t0);
        assert_eq!(t.seek(138.0, t0), 138.0);
        t.observe(10.0, 300.0, ms(t0, 100));
        assert_eq!(t.position, 138.0);
        t.observe(138.0, 300.0, ms(t0, 200));
        t.observe(139.0, 300.0, ms(t0, 300));
        assert_eq!(t.position, 139.0);
    }

    #[test]
    fn rapid_second_seek_wins_over_completion_of_the_first_seek() {
        let t0 = Instant::now();
        let mut t = PlaybackTimeline::new();
        t.observe(0.0, 300.0, t0);
        t.seek(30.0, t0);
        t.seek(180.0, ms(t0, 50));
        t.observe(30.0, 300.0, ms(t0, 100));
        assert_eq!(t.position, 180.0);
        t.observe(180.0, 300.0, ms(t0, 200));
        assert_eq!(t.position, 180.0);
    }

    #[test]
    fn failed_or_imprecise_seek_eventually_uses_the_actual_player_position() {
        let t0 = Instant::now();
        let mut t = PlaybackTimeline::new();
        t.observe(0.0, 300.0, t0);
        t.seek(138.0, t0);
        t.observe(136.0, 300.0, t0 + Duration::from_secs(3));
        assert_eq!(t.position, 136.0);
    }

    #[test]
    fn new_video_clears_duration_and_any_pending_seek() {
        let t0 = Instant::now();
        let mut t = PlaybackTimeline::new();
        t.observe(20.0, 300.0, t0);
        t.seek(140.0, t0);
        t.reset();
        assert_eq!(t.duration, 0.0);
        t.observe(0.0, 30.0, ms(t0, 10));
        assert_eq!(t.position, 0.0);
        assert_eq!(t.seek(100.0, ms(t0, 10)), 30.0);
        assert_eq!(t.seek(-1.0, ms(t0, 10)), 0.0);
        assert_eq!(t.seek(f64::NAN, ms(t0, 10)), 0.0);
    }

    #[test]
    fn time_readout_handles_hours_without_wrapping() {
        for (s, h, e) in [
            (138.9, false, "02:18"),
            (138.0, true, "00:02:18"),
            (3661.0, false, "01:01:01"),
            (90000.0, true, "25:00:00"),
            (-5.0, false, "00:00"),
        ] {
            assert_eq!(PlaybackTimeline::format_time(s, h), e, "{s} {h}");
        }
        assert_eq!(PlaybackTimeline::format_time(f64::NAN, false), "00:00");
        assert_eq!(PlaybackTimeline::format_time(f64::INFINITY, false), "00:00");
    }

    // --- PlayerControlsTests.swift (mac-viewmodels §7.2) ---

    #[test]
    fn seek_holds_the_requested_position_while_player_still_reports_the_old_time() {
        let t0 = Instant::now();
        let mut t = PlaybackTimeline::new();
        t.observe(10.0, 60.0, t0);
        assert_eq!(t.position, 10.0);
        assert_eq!(t.seek(30.0, t0), 30.0);
        assert_eq!(t.position, 30.0);
        t.observe(10.1, 60.0, ms(t0, 100));
        t.observe(10.2, 60.0, ms(t0, 200));
        assert_eq!(t.position, 30.0);
        assert!(t.is_settling());
        t.observe(30.2, 60.0, ms(t0, 300));
        assert_eq!(t.position, 30.2);
        assert!(!t.is_settling());
        t.observe(30.6, 60.0, ms(t0, 400));
        assert_eq!(t.position, 30.6);
    }

    #[test]
    fn tolerance_is_inclusive_at_a_quarter_second() {
        let t0 = Instant::now();
        let mut t = PlaybackTimeline::new();
        t.observe(0.0, 60.0, t0);
        t.seek(20.0, t0);
        t.observe(19.75, 60.0, ms(t0, 50));
        assert_eq!(t.position, 19.75);
        t.seek(20.0, ms(t0, 100));
        t.observe(19.7, 60.0, ms(t0, 150));
        assert_eq!(t.position, 20.0);
    }

    #[test]
    fn requested_position_is_given_up_after_two_seconds() {
        let t0 = Instant::now();
        let mut t = PlaybackTimeline::new();
        t.seek(40.0, t0);
        t.observe(5.0, 60.0, ms(t0, 1900));
        assert_eq!(t.position, 40.0);
        t.observe(5.0, 60.0, t0 + Duration::from_secs(2));
        assert_eq!(t.position, 5.0);
        assert!(!t.is_settling());
    }

    #[test]
    fn a_newer_seek_replaces_the_pending_one() {
        let t0 = Instant::now();
        let mut t = PlaybackTimeline::new();
        t.seek(10.0, t0);
        t.seek(50.0, ms(t0, 500));
        t.observe(10.1, 60.0, ms(t0, 600));
        assert_eq!(t.position, 50.0);
        t.observe(10.1, 60.0, ms(t0, 2400));
        assert_eq!(t.position, 50.0);
        t.observe(10.1, 60.0, ms(t0, 2500));
        assert_eq!(t.position, 10.1);
    }

    #[test]
    fn reset_clears_the_pending_seek_and_the_duration() {
        let t0 = Instant::now();
        let mut t = PlaybackTimeline::new();
        t.observe(0.0, 60.0, t0);
        t.seek(30.0, t0);
        t.reset();
        assert!(t.position == 0.0 && t.duration == 0.0 && !t.is_settling());
        t.observe(2.0, 10.0, ms(t0, 10));
        assert_eq!(t.position, 2.0);
    }

    #[test]
    fn position_stays_inside_the_known_duration() {
        let t0 = Instant::now();
        let mut t = PlaybackTimeline::new();
        assert_eq!(t.seek(500.0, t0), 500.0);
        assert_eq!(t.seek(-5.0, t0), 0.0);
        let t3 = t0 + Duration::from_secs(3);
        t.observe(0.0, 60.0, t3);
        assert_eq!(t.seek(500.0, t3), 60.0);
        assert_eq!(t.seek(f64::NAN, t3), 0.0);
        let t6 = t0 + Duration::from_secs(6);
        t.observe(12.0, 60.0, t6);
        t.observe(f64::NAN, f64::NAN, t6);
        assert!(t.position == 12.0 && t.duration == 0.0);
    }
}
