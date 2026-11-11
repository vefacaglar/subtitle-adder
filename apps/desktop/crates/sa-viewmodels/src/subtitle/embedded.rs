//! Lines read from an embedded subtitle track of the foreground video (`crate::media_tracks`): whether one may be
//! chosen for the user, the snapshots a progressive read delivers, the complete track of a read that replaces the
//! user's lines, the rest of a track whose read stopped early, an abandoned read and the "Off" entry.
//!
//! Embedded lines are not the user's work until they change them ([`super::session::EmbeddedLines::pristine`]):
//! loading them leaves the session clean (no unsaved-changes prompt), no automatic write puts them on disk
//! ([`Session::may_write_automatically`]), a batch run treats them as no lines at all, and macOS evicts such a session
//! like one that can be rebuilt (the track is read again when the video opens again). While a progressive read of
//! the foreground runs, its list is incomplete: translating, saving and exporting wait for it
//! ([`SubtitleViewModel::embedded_read_progress`]).

use std::path::Path;

use sa_core::engine_types::Sentence;
use sa_core::k;
use sa_enginekit::editing::SentenceListExt;

use super::editing::replace_sentences;
use super::session::{EmbeddedLines, Session, SessionId, merge_sentences_preserving_uid, resume_after};
use super::{Core, Fx, Inner, PendingOpen, State, SubtitleViewModel};
use crate::media_tracks::{AutoChoice, SessionMark, SnapshotOutcome};
use crate::types::{JobStatus, PlaylistItemState, SubtitleChange};

/// The foreground's lines are empty (and not because the user deleted them) or an embedded track as it was read,
/// and nothing runs on it: a read may fill it as it goes.
fn takes_snapshots(st: &State, s: &Session) -> bool {
    let empty = s.sentences.is_empty() && !s.has_translations() && !s.has_unsaved_edits;
    (empty || s.embedded_pristine()) && s.job.is_none() && !s.is_working() && s.batch_item_id.is_none() && !st.playlist_running
}

fn same_video(st: &State, s: &Session, video: &Path) -> bool {
    s.video.as_deref().is_some_and(|v| st.key(v) == st.key(video))
}

/// `video` is in the batch list and waits or runs there.
fn queued_in_playlist(st: &State, video: &Path) -> bool {
    let key = st.key(video);
    st.playlist
        .iter()
        .any(|p| matches!(p.state, PlaylistItemState::Pending | PlaylistItemState::Working) && st.key(&p.video_path) == key)
}

impl State {
    /// The progress of the progressive read filling the foreground's list, when one runs.
    pub(crate) fn embedded_read_of_fg(&self) -> Option<f64> {
        let (video, fraction) = self.embedded_read.as_ref()?;
        same_video(self, self.fg(), video).then_some(*fraction)
    }
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
            let embedded = EmbeddedLines::read(stream, &lines, complete);
            let merged = merge_sentences_preserving_uid(&s.sentences, lines);
            s.set_sentences(merged);
            s.embedded = Some(embedded);
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
    pub(crate) fn clear_embedded(&self, c: &mut Core, fx: &mut Fx, sid: SessionId) {
        let Some(s) = c.st.session_mut(sid) else { return };
        s.drop_embedded();
        if c.st.is_foreground(sid) {
            c.undo.clear();
            fx.all(&[SubtitleChange::Sentences, SubtitleChange::Selection, SubtitleChange::Drafts, SubtitleChange::Undo]);
        }
        self.set_status(&mut c.st, fx, sid, JobStatus::Idle);
    }

    /// A batch run is about to process `video`: embedded lines nobody changed are no lines at all to it (it
    /// transcribes the video), so they go, and the read filling them stops (`media_tracks`: the controller sees
    /// the run start and drops its read silently).
    pub(crate) fn drop_embedded_for_batch(&self, c: &mut Core, fx: &mut Fx, video: &Path) {
        let sid = if same_video(&c.st, c.st.fg(), video) { Some(c.st.foreground) } else { c.st.existing_session(video) };
        let Some(sid) = sid else { return };
        if c.st.session(sid).is_some_and(|s| s.embedded_pristine() && !s.is_working() && s.job.is_none()) {
            self.clear_embedded(c, fx, sid);
        }
        if c.st.embedded_read.as_ref().is_some_and(|(v, _)| c.st.key(v) == c.st.key(video)) {
            c.st.embedded_read = None;
            fx.ch(SubtitleChange::Status);
        }
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
    /// run (or the video waiting in the batch list), open confirmation or "Off" chosen for it in this run of the app
    /// (the rule late reads follow: never behind the user's back).
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
                && !queued_in_playlist(st, video)
                && !st.embedded_off.contains(&st.key(video))
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

    /// A progressive read of `video` runs (`Some(progress)`) or ended (`None`). While it fills the foreground's list,
    /// translating, saving and exporting are not offered (the list is not complete yet) and the status says
    /// `Okunuyor… %N`; generating subtitles is (it replaces the track's lines).
    pub fn set_embedded_read(&self, video: &Path, progress: Option<f64>) {
        let video = video.to_path_buf();
        self.inner.edit(move |_, c, fx| {
            let next = progress.map(|f| (video.clone(), f.clamp(0.0, 1.0)));
            let same_video = c.st.embedded_read.as_ref().is_some_and(|(v, _)| *v == video);
            if progress.is_none() && !same_video {
                return;
            }
            if c.st.embedded_read != next {
                c.st.embedded_read = next;
                fx.ch(SubtitleChange::Status);
            }
        });
    }

    /// The progress of the progressive read filling the foreground's list, while one runs.
    pub fn embedded_read_progress(&self) -> Option<f64> {
        self.read(State::embedded_read_of_fg)
    }

    /// A snapshot of a progressive read of track `stream` of `video` (`complete`: the read's whole result), applied
    /// when the session is still `mark` (as the read's start or its previous snapshot left it); then the new mark.
    pub fn apply_embedded_snapshot(&self, video: &Path, stream: usize, mark: SessionMark, lines: Vec<Sentence>, complete: bool) -> SnapshotOutcome {
        let applied = self.inner.edit(|i, c, fx| {
            let fg = c.st.fg();
            // `takes_snapshots`: never over lines the user made, whatever mark the caller holds.
            if c.st.session_mark() != mark || !same_video(&c.st, fg, video) || !takes_snapshots(&c.st, fg) {
                let edited = same_video(&c.st, fg, video)
                    && !fg.sentences.is_empty()
                    && fg.embedded.is_some_and(|e| e.stream == stream)
                    && fg.job.is_none()
                    && !fg.is_working()
                    && !c.st.playlist_running;
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

    /// Whether choosing track `stream` again loads only the rest of it ([`Self::append_embedded_rest`]): the
    /// foreground's lines are an earlier read of it that stopped before the end and that the user changed since.
    pub fn embedded_resumes(&self, stream: usize) -> bool {
        self.read(|st| {
            let s = st.fg();
            s.embedded.is_some_and(|e| e.stream == stream && !e.complete) && !s.embedded_pristine() && takes_rest(st, s)
        })
    }

    /// The complete track `stream` of `video`, read again after an earlier read of it stopped early (the user
    /// changed its lines): the lines past the point that read reached are added after the user's, which stay as
    /// they are. One undo step (`Kalan Satırları Ekle`) removes exactly the added lines; the lines are numbered again
    /// as every edit numbers them. Returns how many were added (`None`: the lines are not that read's any more).
    pub fn append_embedded_rest(&self, video: &Path, stream: usize, all: Vec<Sentence>) -> Option<usize> {
        self.inner.edit(|i, c, fx| {
            let s = c.st.fg();
            let e = s.embedded.filter(|e| e.stream == stream && !e.complete)?;
            if !same_video(&c.st, s, video) || !takes_rest(&c.st, s) {
                return None;
            }
            i.finish_editing(c, fx);
            let rest = resume_after(&e, all);
            let sid = c.st.foreground;
            let added = rest.len();
            if added > 0 {
                let mut lines = c.st.fg().sentences.clone();
                lines.extend(rest);
                lines.renumber();
                replace_sentences(&mut c.st, &mut c.undo, sid, lines, k("Kalan Satırları Ekle"), None, None);
                fx.all(&[SubtitleChange::Sentences, SubtitleChange::Translations, SubtitleChange::Selection, SubtitleChange::Undo, SubtitleChange::Status]);
            }
            if let Some(e) = c.st.fg_mut().embedded.as_mut() {
                e.complete = true;
            }
            Some(added)
        })
    }

    /// A progressive read of track `stream` of `video` stopped before its end (another video was opened, it
    /// failed): lines it left that nobody touched are removed, so the video starts empty (and is read again) when it
    /// is opened again.
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

    /// The track menu's "Off": no embedded track for the foreground video for the rest of this run of the app (it is
    /// not chosen for the user when it opens again), and its lines are cleared when they are an embedded track nobody
    /// changed. A draft being typed is committed first (then the lines are the user's and stay). True when lines
    /// were cleared.
    pub fn clear_embedded_lines(&self) -> bool {
        self.inner.edit(|i, c, fx| {
            i.finish_editing(c, fx);
            if let Some(v) = c.st.fg().video.clone() {
                let key = c.st.key(&v);
                c.st.embedded_off.insert(key);
            }
            let s = c.st.fg();
            if s.embedded.is_none() || !s.embedded_pristine() || s.is_working() || s.job.is_some() {
                return false;
            }
            let sid = c.st.foreground;
            i.clear_embedded(c, fx, sid);
            true
        })
    }

    /// The user chose an embedded track of the foreground video: "Off" no longer holds for it.
    pub fn embedded_track_chosen(&self) {
        self.inner.edit(|_, c, _| {
            if let Some(v) = c.st.fg().video.clone() {
                let key = c.st.key(&v);
                c.st.embedded_off.remove(&key);
            }
        });
    }

    /// "Off" holds for the foreground video (chosen in this run of the app, or nothing from the video is loaded).
    pub fn embedded_off(&self) -> bool {
        self.read(|st| st.fg().embedded.is_none())
    }

}

/// Lines may be added to `s` now: idle, no batch run.
fn takes_rest(st: &State, s: &Session) -> bool {
    s.job.is_none() && !s.is_working() && s.batch_item_id.is_none() && !st.playlist_running
}
