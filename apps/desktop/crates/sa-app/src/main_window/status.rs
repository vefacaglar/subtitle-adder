//! Status surfaces: the Windows error InfoBar state machine (win-ui-main §9.4) and the macOS playback notice rule
//! (mac-ui §7.7, `playbackNoticeSitsNextToACalmEditorStatus`). Pure.

use std::rc::Rc;

use sa_core::l10n::LocalizedMessage;
use sa_viewmodels::types::{JobStatusKind, Workspace};

/// A message the bar can rebuild after a language switch (`show_save_error(make_message)`).
pub type MakeMessage = Rc<dyn Fn() -> String>;

/// Windows error bar.
#[derive(Default)]
pub struct ErrorBar {
    pub open: bool,
    pub message: String,
    pub go_online_visible: bool,
    shown_error: Option<String>,
    save_error: Option<MakeMessage>,
}

impl ErrorBar {
    /// `update_status()` part of §9.4: a failed status opens the bar unless the user closed this very failure.
    pub fn update(&mut self, failed: Option<&LocalizedMessage>, can_go_online: bool) {
        if let Some(m) = failed {
            let id = m.id().to_owned();
            if self.shown_error.as_deref() != Some(id.as_str()) || self.open {
                self.message = m.text();
                self.open = true;
            }
            self.shown_error = Some(id);
            self.save_error = None;
        } else if self.save_error.is_none() {
            self.open = false;
            self.shown_error = None;
        }
        self.go_online_visible = can_go_online && self.save_error.is_none();
    }

    /// Save / export / playback errors stay until closed or replaced.
    pub fn show_save_error(&mut self, make: MakeMessage) {
        self.message = make();
        self.save_error = Some(make);
        self.go_online_visible = false;
        self.open = true;
    }

    /// The close button.
    pub fn close(&mut self) {
        self.open = false;
        self.save_error = None;
    }

    /// A video change drops a pending save error (the bar itself stays until the next status update).
    pub fn video_changed(&mut self) {
        self.save_error = None;
    }

    /// Language switch: rebuild the shown save error.
    pub fn relocalize(&mut self, failed: Option<&LocalizedMessage>) {
        if let Some(make) = &self.save_error {
            if self.open {
                self.message = make();
            }
        } else if let (Some(m), true) = (failed, self.open) {
            self.message = m.text();
        }
    }

    pub fn has_save_error(&self) -> bool {
        self.save_error.is_some()
    }
}

/// `MainBridge.status-kind`: 0 idle, 1 working, 2 finished, 3 failed.
pub fn status_kind_index(kind: JobStatusKind) -> i32 {
    match kind {
        JobStatusKind::Idle => 0,
        JobStatusKind::Working => 1,
        JobStatusKind::Finished => 2,
        JobStatusKind::Failed => 3,
    }
}

/// macOS `vm.playbackNotice(player.failureMessage)`: shown only in the editor next to a calm status (idle or
/// finished) and not while a background translation reports progress.
pub fn mac_playback_notice(failure: Option<LocalizedMessage>, workspace: Workspace, status: JobStatusKind, is_async_translating: bool) -> Option<LocalizedMessage> {
    let calm = matches!(status, JobStatusKind::Idle | JobStatusKind::Finished) && !is_async_translating;
    failure.filter(|_| workspace == Workspace::Editor && calm)
}

/// Progress value of a status fraction for the bar (`clamp(fraction ?? 0, 0, 1)`, non-finite → 0; `-1` = none /
/// indeterminate in the bridge).
pub fn bridge_fraction(fraction: Option<f64>) -> f32 {
    match fraction {
        Some(f) if f.is_finite() => f.clamp(0.0, 1.0) as f32,
        Some(_) => 0.0,
        None => -1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, text: &'static str) -> LocalizedMessage {
        LocalizedMessage::new(id.to_owned(), move || text.to_owned())
    }

    #[test]
    fn a_closed_failure_stays_closed_until_another_one_arrives() {
        let mut bar = ErrorBar::default();
        let a = msg("a", "A");
        bar.update(Some(&a), false);
        assert!(bar.open);
        assert_eq!(bar.message, "A");
        bar.close();
        bar.update(Some(&a), false);
        assert!(!bar.open, "the same failure does not reopen");
        let b = msg("b", "B");
        bar.update(Some(&b), true);
        assert!(bar.open);
        assert_eq!(bar.message, "B");
        assert!(bar.go_online_visible);
        bar.update(None, true);
        assert!(!bar.open);
        // The retry button follows `CanGoOnlineAndRetry` even with the bar closed (the bar hides it).
        assert!(bar.go_online_visible);
    }

    #[test]
    fn a_save_error_survives_idle_updates_until_closed_or_replaced() {
        let mut bar = ErrorBar::default();
        bar.show_save_error(Rc::new(|| "disk full".to_owned()));
        bar.update(None, true);
        assert!(bar.open);
        assert!(!bar.go_online_visible);
        bar.video_changed();
        assert!(!bar.has_save_error());
        bar.update(None, false);
        assert!(!bar.open);
        bar.show_save_error(Rc::new(|| "x".to_owned()));
        bar.update(Some(&msg("f", "F")), false);
        assert_eq!(bar.message, "F");
        assert!(!bar.has_save_error());
    }

    #[test]
    fn playback_notice_sits_next_to_a_calm_editor_status() {
        let m = || Some(msg("playback-failed:unknown", "x"));
        assert!(mac_playback_notice(m(), Workspace::Editor, JobStatusKind::Idle, false).is_some());
        assert!(mac_playback_notice(m(), Workspace::Editor, JobStatusKind::Finished, false).is_some());
        assert!(mac_playback_notice(m(), Workspace::Editor, JobStatusKind::Working, false).is_none());
        assert!(mac_playback_notice(m(), Workspace::Editor, JobStatusKind::Failed, false).is_none());
        assert!(mac_playback_notice(m(), Workspace::Editor, JobStatusKind::Idle, true).is_none());
        assert!(mac_playback_notice(m(), Workspace::Playlist, JobStatusKind::Idle, false).is_none());
    }

    #[test]
    fn fractions_and_kinds() {
        assert_eq!(bridge_fraction(None), -1.0);
        assert_eq!(bridge_fraction(Some(f64::NAN)), 0.0);
        assert_eq!(bridge_fraction(Some(1.7)), 1.0);
        assert_eq!(status_kind_index(JobStatusKind::Failed), 3);
    }
}
