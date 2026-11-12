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
use super::session::{Delivered, EmbeddedLines, Session, SessionId, TrackRest, merge_by_start, merge_sentences_preserving_uid};
use super::{Core, Fx, Inner, PendingOpen, State, SubtitleViewModel};
use crate::media_tracks::{AutoChoice, SessionMark, SnapshotOutcome};
use crate::types::{JobStatus, PlaylistItemState, SubtitleChange};

/// The foreground's lines are empty (and not because the user deleted them) or an embedded track as it was read,
/// and nothing runs on it: a read may fill it as it goes.
fn takes_snapshots(st: &State, s: &Session) -> bool {
    let empty = s.sentences.is_empty() && !s.has_translations() && !s.has_unsaved_edits;
    (empty || s.embedded_pristine()) && s.job.is_none() && !s.is_working() && !in_batch(st, s)
}

/// `s` is a batch item now, or its video waits in the batch list of a run in progress. Only such a video's track
/// reads and choices give way to the run: the user may watch (and read the track of) another one meanwhile.
pub(crate) fn in_batch(st: &State, s: &Session) -> bool {
    s.batch_item_id.is_some() || (st.playlist_running && s.video.as_deref().is_some_and(|v| queued_in_playlist(st, v)))
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
    /// The progress of the progressive read filling the foreground's list, while the list is still nothing but what
    /// it delivered (empty, or the track's lines nobody changed): then translating, saving and exporting wait for
    /// the rest. Once the user changed the lines they are theirs, and can be saved at once.
    pub(crate) fn embedded_read_of_fg(&self) -> Option<f64> {
        let (video, fraction) = self.embedded_read.as_ref()?;
        let fg = self.fg();
        let filling = (fg.sentences.is_empty() && fg.embedded.is_none() && !fg.has_unsaved_edits) || fg.embedded_pristine();
        (filling && same_video(self, fg, video)).then_some(*fraction)
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
            let delivered = std::sync::Arc::new(Delivered::of(&lines));
            let merged = merge_sentences_preserving_uid(&s.sentences, lines);
            s.set_sentences(merged);
            s.embedded = Some(EmbeddedLines::read(stream, complete));
            s.embedded_delivered = Some(delivered);
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
    /// the run start and drops its read silently). Text being typed into the foreground's lines is committed first:
    /// it is the user's work, and then the lines are theirs (the run translates them as it would any edited lines).
    /// Without that the run would go by lines whose fate an open editor still decides: drop them and lose the
    /// typing, or keep them and write the track's own text when the typing is cancelled.
    pub(crate) fn drop_embedded_for_batch(&self, c: &mut Core, fx: &mut Fx, video: &Path) {
        let foreground = same_video(&c.st, c.st.fg(), video);
        let sid = if foreground { Some(c.st.foreground) } else { c.st.existing_session(video) };
        let Some(sid) = sid else { return };
        if foreground && c.st.fg().embedded.is_some() {
            self.finish_editing(c, fx);
        }
        if c.st.session(sid).is_some_and(|s| s.embedded.is_some() && s.embedded_unchanged() && !s.is_working() && s.job.is_none()) {
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
    /// an empty, idle session with no subtitle file loaded or being looked for next to the video, and no job, the
    /// video not a batch item nor waiting in the batch list, no open confirmation and no "Off" chosen for it in this
    /// run of the app (the rule late reads follow: never behind the user's back). A batch run of other videos does
    /// not stop it.
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
            let same_video = c.st.embedded_read.as_ref().is_some_and(|(v, _)| c.st.key(v) == c.st.key(&video));
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
                    && !in_batch(&c.st, fg);
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
    /// changed its lines): the lines that read had not delivered go in among the user's by start time (theirs stay
    /// as they are, in their order; the last lines it had get the end the track gives them, unless the user re-timed
    /// them), or the whole track when the user deleted every line. The lines are numbered again as every edit numbers
    /// them. One undo step (`Kalan Satırları Ekle`) takes exactly that back, the track's "stopped early" state
    /// included, so the rest can be added again. Returns how many were added (`None`: the lines are not that read's
    /// any more).
    pub fn append_embedded_rest(&self, video: &Path, stream: usize, all: Vec<Sentence>) -> Option<usize> {
        self.inner.edit(|i, c, fx| {
            let s = c.st.fg();
            let e = s.embedded.filter(|e| e.stream == stream && !e.complete)?;
            if !same_video(&c.st, s, video) || !takes_rest(&c.st, s) {
                return None;
            }
            i.finish_editing(c, fx);
            let s = c.st.fg();
            let mut lines = s.sentences.clone();
            let rest = if lines.is_empty() {
                all
            } else {
                let rest = TrackRest::of(s.embedded_delivered.as_deref().unwrap_or(&Delivered::default()), all);
                rest.fix_ends(&mut lines);
                rest.lines
            };
            let added = rest.len();
            let mut lines = merge_by_start(lines, rest);
            lines.renumber();
            let sid = c.st.foreground;
            let delivered = std::sync::Arc::new(Delivered::of(&lines));
            let track = (Some(EmbeddedLines { complete: true, ..e }), Some(delivered));
            replace_with_track_state(&mut c.st, &mut c.undo, sid, lines, track, k("Kalan Satırları Ekle").to_owned());
            fx.all(&[SubtitleChange::Sentences, SubtitleChange::Translations, SubtitleChange::Selection, SubtitleChange::Undo, SubtitleChange::Status]);
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
        self.clear_track_lines(true)
    }

    /// A bitmap (picture) subtitle track of the foreground video was chosen to be shown over the video: like
    /// choosing another text track, lines that are an embedded text track nobody changed go (they are not the user's
    /// work), after a draft being typed is committed; anything the user made stays exactly as it is. Unlike "Off" it
    /// is not remembered for the video. True when lines were cleared.
    pub fn clear_embedded_lines_for_picture_track(&self) -> bool {
        self.clear_track_lines(false)
    }

    fn clear_track_lines(&self, remember_off: bool) -> bool {
        self.inner.edit(|i, c, fx| {
            i.finish_editing(c, fx);
            if remember_off && let Some(v) = c.st.fg().video.clone() {
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

    /// Whether `video` is a batch item or waits in the list of the batch run in progress: a read of its track
    /// gives way to the run (`media_tracks`).
    pub fn embedded_video_in_batch(&self, video: &Path) -> bool {
        self.read(|st| {
            let s = if same_video(st, st.fg(), video) { Some(st.fg()) } else { st.existing_session(video).and_then(|sid| st.session(sid)) };
            s.is_some_and(|s| in_batch(st, s)) || (st.playlist_running && queued_in_playlist(st, video))
        })
    }

    /// No read runs for `video` and none will continue the track its list holds: lines of a track that stopped
    /// early and that nobody touched go (another track was chosen and failed before its first lines), so the list
    /// never shows half a track as if it were all of it.
    pub fn drop_unfinished_embedded(&self, video: &Path) {
        self.inner.edit(|i, c, fx| {
            let s = c.st.fg();
            if same_video(&c.st, s, video) && s.embedded_pristine() && s.embedded.is_some_and(|e| !e.complete) && !s.is_working() && s.job.is_none() {
                let sid = c.st.foreground;
                i.clear_embedded(c, fx, sid);
            }
        });
    }

    /// Puts `lines` into `video`'s session as an embedded track nobody changed, past every guard (tests: the
    /// automatic-write backstops must refuse such lines wherever they turn up).
    #[doc(hidden)]
    pub fn put_track_lines_for_tests(&self, video: &Path, stream: usize, lines: Vec<Sentence>) {
        self.inner.edit(|_, c, fx| {
            let sid = if same_video(&c.st, c.st.fg(), video) { Some(c.st.foreground) } else { c.st.existing_session(video) };
            let Some(s) = sid.and_then(|sid| c.st.session_mut(sid)) else { return };
            s.set_sentences(lines);
            s.has_unsaved_edits = false;
            s.set_translations(Default::default());
            s.saved_subtitle_name = None;
            s.embedded = Some(EmbeddedLines::read(stream, true));
            fx.all(&[SubtitleChange::Sentences, SubtitleChange::Translations]);
        });
    }

    /// The foreground's sidecar lookup still runs (tests wait for it instead of sleeping).
    #[doc(hidden)]
    pub fn sidecar_lookup_pending(&self) -> bool {
        self.read(|st| st.fg().sidecar_lookups > 0)
    }
}

/// Replaces `sid`'s lines (as [`replace_sentences`] does) together with the state of the track behind them, as one
/// undo step that restores both.
fn replace_with_track_state(
    st: &mut State,
    undo: &mut sa_core::undo::UndoStack<State>,
    sid: SessionId,
    lines: Vec<Sentence>,
    track: (Option<EmbeddedLines>, Option<std::sync::Arc<Delivered>>),
    key: String,
) {
    let Some(s) = st.session(sid) else { return };
    let previous = (s.sentences.clone(), (s.embedded, s.embedded_delivered.clone()));
    // The lines go through the one mutation primitive (drafts, translations, selection); its own undo entry is
    // replaced by the one below.
    let mut scratch = sa_core::undo::UndoStack::new();
    replace_sentences(st, &mut scratch, sid, lines, &key, None, None);
    if let Some(s) = st.session_mut(sid) {
        (s.embedded, s.embedded_delivered) = track;
    }
    let undo_key = key.clone();
    undo.register(
        &undo_key,
        Box::new(move |st: &mut State, undo: &mut sa_core::undo::UndoStack<State>| {
            replace_with_track_state(st, undo, sid, previous.0, previous.1, key);
        }),
    );
}

/// Lines may be added to `s` now: idle, not in a batch run.
fn takes_rest(st: &State, s: &Session) -> bool {
    s.job.is_none() && !s.is_working() && !in_batch(st, s)
}
