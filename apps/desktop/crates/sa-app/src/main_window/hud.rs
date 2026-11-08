//! Visibility of the player's floating controls.
//!
//! * Windows (win-ui-main §12): transport pill + display picker / engagement bar, 2500 ms idle timer.
//! * macOS (mac-ui §7.2, §7.4): AVKit-like control bar (visible while paused, on pointer movement and 3 s after the
//!   last move while playing) and the overlay panels (`!is_playing || pointer inside`).
//!
//! Pure state machine with an injected clock: the controller feeds pointer events and polls it on every 100 ms tick.

use std::time::{Duration, Instant};

/// Windows idle delay.
pub const WINDOWS_IDLE: Duration = Duration::from_millis(2500);
/// macOS idle delay.
pub const MAC_IDLE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudFlavor {
    Windows,
    Mac,
}

/// What the view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HudVisibility {
    /// Windows transport pill / macOS control bar.
    pub transport: bool,
    /// Windows display picker + engagement bar / macOS overlay panels.
    pub pickers: bool,
}

#[derive(Debug, Clone)]
pub struct HudState {
    flavor: HudFlavor,
    over_player: bool,
    over_picker: bool,
    idle_hidden: bool,
    interacting: bool,
    playing: bool,
    deadline: Option<Instant>,
}

impl HudState {
    pub fn new(flavor: HudFlavor) -> Self {
        Self { flavor, over_player: false, over_picker: false, idle_hidden: false, interacting: false, playing: false, deadline: None }
    }

    fn idle(&self) -> Duration {
        match self.flavor {
            HudFlavor::Windows => WINDOWS_IDLE,
            HudFlavor::Mac => MAC_IDLE,
        }
    }

    pub fn visibility(&self) -> HudVisibility {
        match self.flavor {
            HudFlavor::Windows => {
                let visible = (self.over_player && !self.idle_hidden) || self.interacting || self.over_picker;
                HudVisibility { transport: visible, pickers: visible || !self.playing }
            }
            HudFlavor::Mac => HudVisibility {
                transport: !self.playing || (self.over_player && !self.idle_hidden) || self.interacting,
                pickers: !self.playing || self.over_player,
            },
        }
    }

    pub fn is_timer_running(&self) -> bool {
        self.deadline.is_some()
    }

    fn restart_timer(&mut self, now: Instant) {
        self.deadline = None;
        if self.over_player && !self.interacting && !self.over_picker {
            self.deadline = Some(now + self.idle());
        }
    }

    /// `reveal()`: shows the controls again (only while the pointer is over the player) and restarts the timer.
    pub fn reveal(&mut self, now: Instant) {
        self.idle_hidden = false;
        self.restart_timer(now);
    }

    pub fn pointer_entered(&mut self, now: Instant) {
        self.over_player = true;
        self.reveal(now);
    }

    pub fn pointer_moved(&mut self, now: Instant) {
        self.over_player = true;
        self.reveal(now);
    }

    pub fn pointer_exited(&mut self) {
        self.over_player = false;
        self.deadline = None;
        self.idle_hidden = false;
    }

    pub fn picker_entered(&mut self, now: Instant) {
        self.over_picker = true;
        self.reveal(now);
    }

    pub fn picker_exited(&mut self, now: Instant) {
        self.over_picker = false;
        self.restart_timer(now);
    }

    /// Scrubbing of the seek or volume slider started/ended.
    pub fn set_interacting(&mut self, interacting: bool, now: Instant) {
        if self.interacting != interacting {
            self.interacting = interacting;
            self.restart_timer(now);
        }
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    /// Fires the idle timer when due; returns `true` when the visibility may have changed.
    pub fn poll(&mut self, now: Instant) -> bool {
        match self.deadline {
            Some(d) if now >= d => {
                self.deadline = None;
                if self.playing && !self.interacting && !self.over_picker {
                    self.idle_hidden = true;
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_hud_hides_after_the_idle_delay_only_while_playing() {
        let t0 = Instant::now();
        let mut h = HudState::new(HudFlavor::Windows);
        assert_eq!(h.visibility(), HudVisibility { transport: false, pickers: true });
        h.set_playing(true);
        assert_eq!(h.visibility(), HudVisibility { transport: false, pickers: false });
        h.pointer_entered(t0);
        assert_eq!(h.visibility(), HudVisibility { transport: true, pickers: true });
        assert!(!h.poll(t0 + Duration::from_millis(2499)));
        assert!(h.poll(t0 + WINDOWS_IDLE));
        assert_eq!(h.visibility(), HudVisibility { transport: false, pickers: false });
        // A move reveals again.
        h.pointer_moved(t0 + Duration::from_secs(3));
        assert!(h.visibility().transport);
        // Paused: the timer fires but nothing hides; the picker stays.
        h.set_playing(false);
        h.poll(t0 + Duration::from_secs(10));
        assert_eq!(h.visibility(), HudVisibility { transport: true, pickers: true });
    }

    #[test]
    fn windows_pointer_exit_hides_the_pill_and_stops_the_timer() {
        let t0 = Instant::now();
        let mut h = HudState::new(HudFlavor::Windows);
        h.set_playing(true);
        h.pointer_entered(t0);
        h.pointer_exited();
        assert!(!h.is_timer_running());
        assert_eq!(h.visibility(), HudVisibility { transport: false, pickers: false });
        h.set_playing(false);
        assert_eq!(h.visibility(), HudVisibility { transport: false, pickers: true });
    }

    #[test]
    fn windows_picker_and_scrubbing_keep_everything_visible() {
        let t0 = Instant::now();
        let mut h = HudState::new(HudFlavor::Windows);
        h.set_playing(true);
        h.pointer_entered(t0);
        h.picker_entered(t0);
        assert!(!h.is_timer_running());
        h.poll(t0 + Duration::from_secs(5));
        assert!(h.visibility().transport && h.visibility().pickers);
        h.picker_exited(t0 + Duration::from_secs(5));
        assert!(h.is_timer_running());
        h.set_interacting(true, t0 + Duration::from_secs(6));
        assert!(!h.is_timer_running());
        h.poll(t0 + Duration::from_secs(20));
        assert!(h.visibility().transport);
        h.set_interacting(false, t0 + Duration::from_secs(20));
        h.poll(t0 + Duration::from_secs(23));
        assert!(!h.visibility().transport);
    }

    #[test]
    fn windows_reveal_without_pointer_shows_nothing() {
        let t0 = Instant::now();
        let mut h = HudState::new(HudFlavor::Windows);
        h.set_playing(true);
        h.reveal(t0);
        assert!(!h.visibility().transport);
        assert!(!h.is_timer_running());
    }

    #[test]
    fn mac_bar_is_visible_while_paused_and_three_seconds_after_a_move() {
        let t0 = Instant::now();
        let mut h = HudState::new(HudFlavor::Mac);
        assert_eq!(h.visibility(), HudVisibility { transport: true, pickers: true });
        h.set_playing(true);
        assert_eq!(h.visibility(), HudVisibility { transport: false, pickers: false });
        h.pointer_moved(t0);
        assert_eq!(h.visibility(), HudVisibility { transport: true, pickers: true });
        h.poll(t0 + Duration::from_millis(2900));
        assert!(h.visibility().transport);
        h.poll(t0 + MAC_IDLE);
        // The bar fades, the panels stay while the pointer is inside.
        assert_eq!(h.visibility(), HudVisibility { transport: false, pickers: true });
        h.pointer_exited();
        assert_eq!(h.visibility(), HudVisibility { transport: false, pickers: false });
    }
}
