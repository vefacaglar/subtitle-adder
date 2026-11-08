//! `PlayerSessionGate` (win-media §4.2): events from a previous source never affect a newly loaded one. Used by the
//! FFmpeg backend on both platforms (the macOS "load generation" is the same idea).

use parking_lot::Mutex;

/// Unique id per created media source.
pub type SourceId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PlayerEventToken {
    pub generation: i32,
    pub source: Option<SourceId>,
}

#[derive(Debug, Default)]
struct GateState {
    load_generation: i32,
    active_generation: i32,
    active_source: Option<SourceId>,
    opened: bool,
    failed: bool,
    error: Option<String>,
}

impl GateState {
    fn clear_flags(&mut self) {
        self.opened = false;
        self.failed = false;
        self.error = None;
    }

    /// Shared acceptance test of the source-bound notifications.
    fn accepts_bound(&self, token: PlayerEventToken) -> bool {
        if token.generation == 0 || token.source.is_none() {
            return false;
        }
        if token.generation != self.load_generation {
            return false;
        }
        // Accepted while no source is active yet.
        !matches!(self.active_source, Some(active) if Some(active) != token.source)
    }

    fn accepts_event(&self, token: PlayerEventToken, current_player_source: Option<SourceId>) -> bool {
        if token.generation == 0 || token.source.is_none() {
            return false;
        }
        token.generation == self.load_generation
            && token.generation == self.active_generation
            && self.active_source.is_some()
            && token.source == self.active_source
            && current_player_source == self.active_source
    }
}

/// The gate (one mutex).
#[derive(Debug, Default)]
pub struct PlayerSessionGate {
    state: Mutex<GateState>,
}

impl PlayerSessionGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears flags, `load_generation += 1` (wrapping); first call returns 1.
    pub fn begin_load(&self) -> i32 {
        let mut s = self.state.lock();
        s.clear_flags();
        s.load_generation = s.load_generation.wrapping_add(1);
        s.load_generation
    }

    pub fn is_current_load(&self, generation: i32) -> bool {
        generation != 0 && generation == self.state.lock().load_generation
    }

    pub fn create_bound_token(&self, generation: i32, source: Option<SourceId>) -> PlayerEventToken {
        PlayerEventToken { generation, source }
    }

    pub fn clear_active_source(&self) {
        let mut s = self.state.lock();
        s.active_generation = 0;
        s.active_source = None;
        s.clear_flags();
    }

    pub fn try_activate_source(&self, generation: i32, source: Option<SourceId>) -> bool {
        let mut s = self.state.lock();
        if generation == 0 || generation != s.load_generation {
            return false;
        }
        let same = s.active_source == source && s.active_generation == generation;
        s.active_generation = if source.is_none() { 0 } else { generation };
        s.active_source = source;
        if !same {
            s.clear_flags();
        }
        true
    }

    pub fn notify_source_open_completed(&self, token: PlayerEventToken, ok: bool, error: Option<String>) -> bool {
        let mut s = self.state.lock();
        if !s.accepts_bound(token) {
            return false;
        }
        if ok {
            s.opened = true;
            s.failed = false;
            s.error = None;
        } else {
            s.failed = true;
            s.error = error;
        }
        true
    }

    pub fn notify_source_state_changed(&self, token: PlayerEventToken, is_opened: bool, is_failed: bool) -> bool {
        let mut s = self.state.lock();
        if !s.accepts_bound(token) {
            return false;
        }
        if is_opened {
            s.opened = true;
            s.failed = false;
        } else if is_failed {
            s.failed = true;
        }
        true
    }

    pub fn capture_event_token(&self) -> PlayerEventToken {
        let s = self.state.lock();
        PlayerEventToken { generation: s.active_generation, source: s.active_source }
    }

    pub fn should_accept_event(&self, token: PlayerEventToken, current_player_source: Option<SourceId>) -> bool {
        self.state.lock().accepts_event(token, current_player_source)
    }

    pub fn should_accept_media_opened(&self, token: PlayerEventToken, current: Option<SourceId>, is_opened_state: bool) -> bool {
        let s = self.state.lock();
        s.accepts_event(token, current) && !s.failed && (is_opened_state || s.opened)
    }

    pub fn should_accept_media_failed(&self, token: PlayerEventToken, current: Option<SourceId>, is_failed_state: bool) -> bool {
        let s = self.state.lock();
        s.accepts_event(token, current) && (is_failed_state || s.failed)
    }

    pub fn load_generation(&self) -> i32 {
        self.state.lock().load_generation
    }

    pub fn active_generation(&self) -> i32 {
        self.state.lock().active_generation
    }

    pub fn active_source(&self) -> Option<SourceId> {
        self.state.lock().active_source
    }

    pub fn has_active_source_opened(&self) -> bool {
        self.state.lock().opened
    }

    pub fn has_active_source_failed(&self) -> bool {
        self.state.lock().failed
    }

    pub fn active_source_error(&self) -> Option<String> {
        self.state.lock().error.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Option<SourceId> = Some(101);
    const B: Option<SourceId> = Some(202);

    /// `LocalProfileTests.PlayerSessionGate_RejectsLateEventFromPreviousSourceAfterUIThreadSwap`.
    #[test]
    fn player_session_gate_rejects_late_event_from_previous_source_after_ui_thread_swap() {
        let gate = PlayerSessionGate::new();
        let gen_a = gate.begin_load();
        assert_eq!(gen_a, 1);
        assert!(gate.try_activate_source(gen_a, A));
        let bound_a = gate.create_bound_token(gen_a, A);

        let token_a = gate.capture_event_token();
        assert_eq!(token_a, PlayerEventToken { generation: 1, source: A });

        let gen_b = gate.begin_load();
        assert_eq!(gen_b, 2);
        gate.clear_active_source();
        assert!(gate.try_activate_source(gen_b, B));

        assert!(!gate.should_accept_event(token_a, B));
        assert!(!gate.should_accept_event(token_a, A));

        assert!(!gate.notify_source_open_completed(bound_a, false, None));
        assert!(!gate.notify_source_open_completed(bound_a, true, None));
        assert!(!gate.should_accept_media_failed(bound_a, B, false));
        assert!(!gate.should_accept_media_opened(bound_a, B, true));

        let late = gate.capture_event_token();
        assert_eq!(late, PlayerEventToken { generation: 2, source: B });
        assert!(!gate.should_accept_media_failed(late, B, false));
        assert!(!gate.should_accept_media_opened(late, B, false));

        let bound_b = gate.create_bound_token(gen_b, B);
        assert!(gate.notify_source_open_completed(bound_b, true, None));
        assert!(gate.should_accept_media_opened(bound_b, B, true));
        assert!(gate.should_accept_media_opened(late, B, true));
    }

    #[test]
    fn bound_notifications_are_accepted_before_activation_and_record_errors() {
        let gate = PlayerSessionGate::new();
        let g = gate.begin_load();
        let tok = gate.create_bound_token(g, A);
        assert!(gate.notify_source_open_completed(tok, false, Some("boom".into())));
        assert!(gate.has_active_source_failed());
        assert_eq!(gate.active_source_error().as_deref(), Some("boom"));
        // Activating a source that was not active clears the flags; re-activating the same one keeps them.
        assert!(gate.try_activate_source(g, A));
        assert!(!gate.has_active_source_failed());
        assert!(gate.notify_source_open_completed(tok, false, Some("boom".into())));
        assert!(gate.try_activate_source(g, A));
        assert!(gate.has_active_source_failed());
        assert!(gate.try_activate_source(g, B));
        assert!(!gate.has_active_source_failed());
        assert_eq!(gate.active_source_error(), None);
        // A token of another source is rejected once B is active.
        assert!(!gate.notify_source_state_changed(tok, true, false));
        assert!(gate.notify_source_state_changed(gate.create_bound_token(g, B), true, false));
        assert!(gate.has_active_source_opened());
    }

    #[test]
    fn zero_and_sourceless_tokens_are_never_accepted() {
        let gate = PlayerSessionGate::new();
        assert!(!gate.is_current_load(0));
        assert!(!gate.try_activate_source(0, A));
        let g = gate.begin_load();
        assert!(gate.is_current_load(g));
        assert!(gate.try_activate_source(g, None));
        assert_eq!(gate.active_generation(), 0);
        assert!(!gate.notify_source_open_completed(PlayerEventToken { generation: g, source: None }, true, None));
        assert!(!gate.should_accept_event(PlayerEventToken { generation: 0, source: A }, A));
        assert!(!gate.try_activate_source(g + 1, A));
    }

    #[test]
    fn begin_load_wraps_and_clears_flags() {
        let gate = PlayerSessionGate::new();
        gate.state.lock().load_generation = i32::MAX;
        assert_eq!(gate.begin_load(), i32::MIN);
        let g = gate.load_generation();
        assert!(gate.try_activate_source(g, A));
        assert!(gate.notify_source_open_completed(gate.create_bound_token(g, A), false, Some("x".into())));
        gate.begin_load();
        assert!(!gate.has_active_source_failed());
        assert_eq!(gate.active_source_error(), None);
    }
}
