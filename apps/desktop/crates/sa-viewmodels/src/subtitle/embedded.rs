//! Lines read from an embedded subtitle track of the foreground video (`crate::media_tracks`): whether one may be
//! chosen for the user, the snapshots a progressive read delivers, the complete track of a read that replaces the
//! user's lines, an abandoned read and the "Off" entry.
//!
//! Embedded lines are not the user's work until they change them ([`super::session::EmbeddedLines::pristine`]):
//! loading them leaves the session clean (no unsaved-changes prompt), no automatic save writes them, and macOS
//! evicts such a session like one that can be rebuilt (the track is read again when the video opens again).

use std::path::Path;

use sa_core::engine_types::Sentence;

use super::session::{EmbeddedLines, Session, SessionId, merge_sentences_preserving_uid};
use super::{Core, Fx, Inner, PendingOpen, State, SubtitleViewModel};
use crate::media_tracks::{AutoChoice, SessionMark, SnapshotOutcome};
use crate::types::{JobStatus, SubtitleChange};

/// The foreground's lines are empty (and not because the user deleted them) or an embedded track as it was read,
/// and nothing runs on it: a read may fill it as it goes.
fn takes_snapshots(st: &State, s: &Session) -> bool {
    let empty = s.sentences.is_empty() && !s.has_translations() && !s.has_unsaved_edits;
    (empty || s.embedded_pristine()) && s.job.is_none() && !s.is_working() && s.batch_item_id.is_none() && !st.playlist_running
}

fn same_video(st: &State, s: &Session, video: &Path) -> bool {
    s.video.as_deref().is_some_and(|v| st.key(v) == st.key(video))
}

impl Inner {
    /// Puts embedded lines into `sid`: the first snapshot (or the complete track) replaces what was there like an
    /// import does; a later snapshot of the same read is merged in keeping the lines' identities (selection, rows).
    fn put_embedded(&self, c: &mut Core, fx: &mut Fx, sid: SessionId, stream: usize, lines: Vec<Sentence>, complete: bool) {
        let Some(s) = c.st.session(sid) else { return };
        let continuing = !s.sentences.is_empty()
            && s.embedded_pristine()
            && s.embedded.is_some_and(|e| e.stream == stream && !e.complete);
        if continuing {
            let s = c.st.session_mut(sid).expect("session");
            let merged = merge_sentences_preserving_uid(&s.sentences, lines);
            s.set_sentences(merged);
            s.embedded = Some(EmbeddedLines { stream, pristine: true, complete });
            if c.st.is_foreground(sid) {
                // The status summary counts the lines.
                fx.all(&[SubtitleChange::Sentences, SubtitleChange::Selection, SubtitleChange::Status]);
            }
            return;
        }
        if s.job.is_some() || s.status.is_working() {
            self.stop_job(c, fx, sid, true);
        }
        if c.st.is_foreground(sid) {
            c.undo.clear();
        }
        if let Some(s) = c.st.session_mut(sid) {
            s.load_embedded(stream, lines, complete);
        }
        self.set_status(&mut c.st, fx, sid, JobStatus::Finished);
        fx.all(&[SubtitleChange::Sentences, SubtitleChange::Translations, SubtitleChange::Selection, SubtitleChange::Undo]);
    }

    /// The complete lines of a read over the user's lines, once confirmed (macOS) or right away (Windows): replaces
    /// the foreground's lines like importing a subtitle file.
    pub(crate) fn replace_with_embedded(&self, c: &mut Core, fx: &mut Fx, video: &Path, stream: usize, lines: Vec<Sentence>) {
        let sid = c.st.foreground;
        if lines.is_empty() || !same_video(&c.st, c.st.fg(), video) {
            return;
        }
        self.put_embedded(c, fx, sid, stream, lines, true);
    }

    /// Empties session `sid` of embedded lines that are still exactly the track's.
    fn clear_embedded(&self, c: &mut Core, fx: &mut Fx, sid: SessionId) {
        let Some(s) = c.st.session_mut(sid) else { return };
        s.set_sentences(Vec::new());
        s.selection = None;
        s.selected_uids.clear();
        s.draft = None;
        s.embedded = None;
        if c.st.is_foreground(sid) {
            c.undo.clear();
            fx.all(&[SubtitleChange::Sentences, SubtitleChange::Selection, SubtitleChange::Drafts, SubtitleChange::Undo]);
        }
        self.set_status(&mut c.st, fx, sid, JobStatus::Idle);
    }
}

impl SubtitleViewModel {
    /// The embedded track whose lines the foreground holds; `pristine` says whether they are still exactly as read.
    pub fn embedded_lines(&self) -> Option<EmbeddedLines> {
        self.read(|s| {
            let fg = s.fg();
            fg.embedded.map(|e| EmbeddedLines { pristine: fg.embedded_pristine(), ..e })
        })
    }

    /// Whether an embedded track of `video` (the foreground's, just opened) may be chosen for the user: only into
    /// an empty, idle session with no subtitle file loaded or being looked for next to the video, and no job, batch
    /// run or open confirmation going on (the rule late reads follow: never behind the user's back).
    pub fn embedded_auto_choice(&self, video: &Path) -> AutoChoice {
        self.read(|st| {
            let s = st.fg();
            let untouched = same_video(st, s, video)
                && s.sentences.is_empty()
                && !s.has_translations()
                && !s.has_unsaved_edits
                && s.loaded_subtitle.is_none()
                && s.embedded.is_none()
                && s.status == JobStatus::Idle
                && s.job.is_none()
                && !s.is_working()
                && s.batch_item_id.is_none()
                && !st.playlist_running
                && st.pending_open.is_none();
            match (untouched, s.sidecar_lookups) {
                (false, _) => AutoChoice::Refused,
                (true, 0) => AutoChoice::Allowed,
                (true, _) => AutoChoice::Wait,
            }
        })
    }

    /// Whether a read started now fills the foreground's list as it goes (it is empty or an unedited embedded
    /// track, and idle); otherwise its complete lines go through [`Self::request_embedded_lines`] at the end.
    pub fn embedded_reads_progressively(&self) -> bool {
        self.read(|st| takes_snapshots(st, st.fg()))
    }

    /// A snapshot of a progressive read of track `stream` of `video` (`complete`: the read's whole result), applied
    /// when the session is still `mark` (as the read's start or its previous snapshot left it); then the new mark.
    pub fn apply_embedded_snapshot(&self, video: &Path, stream: usize, mark: SessionMark, lines: Vec<Sentence>, complete: bool) -> SnapshotOutcome {
        let applied = self.inner.edit(|i, c, fx| {
            let fg = c.st.fg();
            // `takes_snapshots`: never over lines the user made, whatever mark the caller holds.
            if c.st.session_mark() != mark || !same_video(&c.st, fg, video) || !takes_snapshots(&c.st, fg) {
                let edited = same_video(&c.st, fg, video) && !fg.sentences.is_empty() && fg.embedded.is_some_and(|e| e.stream == stream);
                return Err(if edited { SnapshotOutcome::Edited } else { SnapshotOutcome::Superseded });
            }
            if !lines.is_empty() {
                let sid = c.st.foreground;
                i.put_embedded(c, fx, sid, stream, lines, complete);
            }
            Ok(())
        });
        match applied {
            Ok(()) => SnapshotOutcome::Applied(self.session_mark()),
            Err(outcome) => outcome,
        }
    }

    /// The complete lines of track `stream` of `video` read while the list held the user's lines: replaces them the
    /// way opening a subtitle file does (macOS asks first when edits would be lost or a job stopped; a video in a
    /// batch run refuses).
    pub fn request_embedded_lines(&self, video: &Path, stream: usize, lines: Vec<Sentence>) {
        let video = video.to_path_buf();
        self.inner.edit(move |i, c, fx| {
            if c.st.fg().batch_item_id.is_some() {
                c.st.open_refusal = Some(super::files::batch_busy_message());
                fx.ch(SubtitleChange::Notice);
                return;
            }
            let action = super::files::OpenAction::Embedded { video, stream, lines };
            let s = c.st.fg();
            let (stops_job, loses_edits) = (s.is_working(), s.has_unsaved_edits);
            if c.st.policy.confirm_open_when_losing_work && (stops_job || loses_edits) {
                c.st.pending_open = Some(PendingOpen { stops_job, loses_edits, action });
                fx.ch(SubtitleChange::Notice);
                return;
            }
            c.st.pending_open = None;
            if let super::files::OpenAction::Embedded { video, stream, lines } = action {
                i.replace_with_embedded(c, fx, &video, stream, lines);
            }
        });
    }

    /// A progressive read of track `stream` of `video` stopped before its end (another video was opened): lines it
    /// left that nobody touched are removed, so the video starts empty (and is read again) when it is opened again.
    pub fn abandon_embedded_read(&self, video: &Path, stream: usize) {
        self.inner.edit(|i, c, fx| {
            let sid = if same_video(&c.st, c.st.fg(), video) { Some(c.st.foreground) } else { c.st.existing_session(video) };
            let Some(sid) = sid else { return };
            let Some(s) = c.st.session(sid) else { return };
            if s.embedded_pristine() && s.embedded.is_some_and(|e| e.stream == stream && !e.complete) && !s.is_working() {
                i.clear_embedded(c, fx, sid);
            }
        });
    }

    /// The track menu's "Off": clears the foreground's lines when they are an embedded track nobody changed (true
    /// then).
    pub fn clear_embedded_lines(&self) -> bool {
        self.inner.edit(|i, c, fx| {
            let s = c.st.fg();
            if !s.embedded_pristine() || s.is_working() || s.job.is_some() {
                return false;
            }
            let sid = c.st.foreground;
            i.clear_embedded(c, fx, sid);
            true
        })
    }
}
