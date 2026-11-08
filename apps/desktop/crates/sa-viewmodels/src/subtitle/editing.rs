//! Line editing, drafts and undo (win-viewmodels §15, mac-viewmodels §5.11). One undo history for the foreground
//! session; cleared whenever another session comes to the front, a transcription restarts it or a file is loaded.

use std::collections::HashSet;

use sa_core::appearance::Translations;
use sa_core::engine_types::Sentence;
use sa_core::k;
use sa_core::undo::UndoStack;
use sa_enginekit::editing::SentenceListExt;
use uuid::Uuid;

use super::session::{SessionId, reverted_translations};
use super::{Core, Fx, Inner, State};
use crate::types::{SubtitleChange, TextDraft, TimingEdge};

const EDIT_CHANGES: [SubtitleChange; 5] =
    [SubtitleChange::Sentences, SubtitleChange::Translations, SubtitleChange::Selection, SubtitleChange::Drafts, SubtitleChange::Undo];

fn normalize_newlines(t: &str) -> String {
    t.replace("\r\n", " ").replace(['\n', '\r'], " ")
}

/// The single mutation primitive (win-viewmodels §15.4).
pub(crate) fn replace_sentences(
    st: &mut State,
    undo: &mut UndoStack<State>,
    sid: SessionId,
    updated: Vec<Sentence>,
    key: &str,
    revert: Option<(Translations, Translations)>,
    cleared: Option<HashSet<Uuid>>,
) {
    let Some(s) = st.session_mut(sid) else { return };
    let previous = s.sentences.clone();
    let previous_tr = s.translations.clone();
    if let Some(d) = &s.draft {
        let old = previous.iter().find(|x| x.uid == d.uid).map(|x| x.text.clone());
        let new = updated.iter().find(|x| x.uid == d.uid).map(|x| x.text.clone());
        if new.is_none() || new != old {
            s.draft = None;
        }
    }
    let valid: HashSet<Uuid> = updated.iter().map(|x| x.uid).collect();
    s.set_sentences(updated);
    let translations = match revert {
        Some((from, to)) => reverted_translations(&s.translations, s.translation_language.as_deref(), &from, &to, Some(&valid)),
        None => {
            let cleared = cleared.unwrap_or_default();
            s.translations.iter().filter(|(k, _)| valid.contains(k) && !cleared.contains(k)).map(|(k, v)| (*k, v.clone())).collect()
        }
    };
    s.set_translations(translations);
    if let Some(d) = &s.translation_draft
        && !s.translations.contains_key(&d.uid)
    {
        s.translation_draft = None;
    }
    if s.selection.is_some_and(|u| !valid.contains(&u)) {
        s.selection = None;
    }
    s.has_unsaved_edits = true;
    let applied = s.translations.clone();
    let key_owned = key.to_owned();
    undo.register(
        key,
        Box::new(move |st: &mut State, undo: &mut UndoStack<State>| {
            replace_sentences(st, undo, sid, previous, &key_owned, Some((applied, previous_tr)), None);
        }),
    );
}

/// Translation-only undo: reverts `from → to` without discarding newer translations.
pub(crate) fn revert_translations(
    st: &mut State,
    undo: &mut UndoStack<State>,
    sid: SessionId,
    from: Translations,
    to: Translations,
    key: &str,
) {
    let Some(s) = st.session_mut(sid) else { return };
    let current = s.translations.clone();
    let next = reverted_translations(&current, s.translation_language.as_deref(), &from, &to, None);
    s.set_translations(next);
    s.has_unsaved_edits = true;
    let applied = s.translations.clone();
    let key_owned = key.to_owned();
    undo.register(
        key,
        Box::new(move |st: &mut State, undo: &mut UndoStack<State>| {
            revert_translations(st, undo, sid, applied, current, &key_owned);
        }),
    );
}

/// Restores a whole translation map (macOS selected retry step), registering the inverse.
fn restore_translations(st: &mut State, undo: &mut UndoStack<State>, sid: SessionId, target: Translations, key: &str) {
    let Some(s) = st.session_mut(sid) else { return };
    let current = std::mem::take(&mut s.translations);
    s.set_translations(target);
    if let Some(d) = &s.translation_draft
        && !s.translations.contains_key(&d.uid)
    {
        s.translation_draft = None;
    }
    let key_owned = key.to_owned();
    undo.register(
        key,
        Box::new(move |st: &mut State, undo: &mut UndoStack<State>| {
            restore_translations(st, undo, sid, current, &key_owned);
        }),
    );
}

fn selected_index(st: &State, need_single: bool) -> Option<usize> {
    let fg = st.fg();
    let sel = fg.selection?;
    if need_single && st.policy.split_merge_timing_need_single_selection && !fg.has_single_selection() {
        return None;
    }
    fg.sentences.index_of_uid(sel)
}

pub(crate) fn can_split_selected(st: &State, at: f64) -> bool {
    selected_index(st, true).is_some_and(|i| st.fg().sentences.can_split(i, at))
}

pub(crate) fn can_merge_selected(st: &State) -> bool {
    selected_index(st, true).is_some_and(|i| i + 1 < st.fg().sentences.len())
}

impl Inner {
    /// Clones the foreground lines, applies `change`, and records one replacement (no record when unchanged).
    fn commit(c: &mut Core, key: &str, cleared: Option<HashSet<Uuid>>, change: impl FnOnce(&mut Vec<Sentence>) -> bool) -> bool {
        let sid = c.st.foreground;
        let mut lines = c.st.fg().sentences.clone();
        if !change(&mut lines) {
            return false;
        }
        replace_sentences(&mut c.st, &mut c.undo, sid, lines, key, None, cleared);
        true
    }

    fn grouped<R>(c: &mut Core, f: impl FnOnce(&mut Core) -> R) -> R {
        c.undo.begin_group();
        let r = f(c);
        c.undo.end_group();
        r
    }

    // ----- drafts

    pub(crate) fn begin_editing(&self, c: &mut Core, fx: &mut Fx, uid: Uuid) {
        if c.st.fg().draft.as_ref().is_some_and(|d| d.uid == uid) {
            return;
        }
        self.commit_draft(c, fx);
        let fg = c.st.fg_mut();
        if let Some(line) = fg.sentences.iter().find(|x| x.uid == uid) {
            fg.draft = Some(TextDraft { uid, text: line.text.clone() });
            fx.ch(SubtitleChange::Drafts);
        }
    }

    pub(crate) fn update_draft(&self, c: &mut Core, fx: &mut Fx, uid: Uuid, text: String) {
        if c.st.fg().draft.as_ref().is_some_and(|d| d.uid != uid) {
            self.commit_draft(c, fx);
        }
        let fg = c.st.fg_mut();
        match &mut fg.draft {
            Some(d) if d.uid == uid => {
                if d.text != text {
                    d.text = text;
                }
            }
            _ => {
                if fg.sentences.iter().any(|x| x.uid == uid) {
                    fg.draft = Some(TextDraft { uid, text });
                }
            }
        }
        fx.ch(SubtitleChange::Drafts);
    }

    pub(crate) fn commit_draft(&self, c: &mut Core, fx: &mut Fx) {
        let Some(d) = c.st.fg_mut().draft.take() else { return };
        fx.ch(SubtitleChange::Drafts);
        let Some(i) = c.st.fg().sentences.index_of_uid(d.uid) else { return };
        let text = normalize_newlines(&d.text);
        if c.undo.is_undoing() || c.undo.is_redoing() {
            let s = &mut c.st.fg_mut().sentences[i];
            s.text = text;
            return;
        }
        let changed = Self::grouped(c, |c| {
            Self::commit(c, k("Metni Değiştir"), None, |lines| {
                let changed = lines.set_text(&text, i);
                if changed {
                    lines[i].needs_review = false;
                }
                changed
            })
        });
        if changed {
            fx.all(&EDIT_CHANGES);
        }
    }

    pub(crate) fn begin_translation_editing(&self, c: &mut Core, fx: &mut Fx, uid: Uuid) {
        if c.st.fg().translation_draft.as_ref().is_some_and(|d| d.uid == uid) {
            return;
        }
        self.commit_translation_draft(c, fx);
        let fg = c.st.fg_mut();
        if let Some(e) = fg.translations.get(&uid) {
            fg.translation_draft = Some(TextDraft { uid, text: e.text.clone() });
            fx.ch(SubtitleChange::Drafts);
        }
    }

    pub(crate) fn update_translation_draft(&self, c: &mut Core, fx: &mut Fx, uid: Uuid, text: String) {
        if c.st.fg().translation_draft.as_ref().is_some_and(|d| d.uid != uid) {
            self.commit_translation_draft(c, fx);
        }
        let fg = c.st.fg_mut();
        match &mut fg.translation_draft {
            Some(d) if d.uid == uid => {
                if d.text != text {
                    d.text = text;
                }
            }
            _ => {
                if fg.translations.contains_key(&uid) {
                    fg.translation_draft = Some(TextDraft { uid, text });
                }
            }
        }
        fx.ch(SubtitleChange::Drafts);
    }

    pub(crate) fn commit_translation_draft(&self, c: &mut Core, fx: &mut Fx) {
        let Some(d) = c.st.fg_mut().translation_draft.take() else { return };
        fx.ch(SubtitleChange::Drafts);
        let text = normalize_newlines(&d.text);
        self.set_translation_text(c, fx, d.uid, &text);
    }

    pub(crate) fn finish_editing(&self, c: &mut Core, fx: &mut Fx) {
        self.commit_draft(c, fx);
        self.commit_translation_draft(c, fx);
    }

    // ----- operations (each one undo group)

    pub(crate) fn set_text(&self, c: &mut Core, fx: &mut Fx, uid: Uuid, text: &str) -> bool {
        let Some(i) = c.st.fg().sentences.index_of_uid(uid) else { return false };
        let changed = Self::grouped(c, |c| {
            Self::commit(c, k("Metni Değiştir"), None, |lines| {
                let changed = lines.set_text(text, i);
                if changed {
                    lines[i].needs_review = false;
                }
                changed
            })
        });
        if changed {
            fx.all(&EDIT_CHANGES);
        }
        changed
    }

    /// Hand edit of a translation: clears the review marker; one group `Çeviriyi Değiştir`.
    pub(crate) fn set_translation_text(&self, c: &mut Core, fx: &mut Fx, uid: Uuid, text: &str) -> bool {
        let sid = c.st.foreground;
        let fg = c.st.fg();
        let Some(entry) = fg.translations.get(&uid) else { return false };
        if entry.text == text && !entry.needs_review {
            return false;
        }
        if entry.text == text {
            return false;
        }
        let previous = fg.translations.clone();
        let mut updated = previous.clone();
        let mut e = entry.clone();
        e.text = text.to_owned();
        e.needs_review = false;
        updated.insert(uid, e);
        Self::grouped(c, |c| {
            let s = c.st.fg_mut();
            s.set_translations(updated.clone());
            s.has_unsaved_edits = true;
            let key = k("Çeviriyi Değiştir").to_owned();
            c.undo.register(
                k("Çeviriyi Değiştir"),
                Box::new(move |st: &mut State, undo: &mut UndoStack<State>| {
                    revert_translations(st, undo, sid, updated, previous, &key);
                }),
            );
        });
        fx.all(&EDIT_CHANGES);
        true
    }

    /// macOS selected retry: one undo step restoring `baseline` (registered on the retry's first batch).
    pub(crate) fn register_translation_undo(&self, c: &mut Core, baseline: Translations, key: &str) {
        let sid = c.st.foreground;
        let key_owned = key.to_owned();
        Self::grouped(c, |c| {
            c.undo.register(
                key,
                Box::new(move |st: &mut State, undo: &mut UndoStack<State>| {
                    restore_translations(st, undo, sid, baseline, &key_owned);
                }),
            );
        });
    }

    fn set_speaker_body(&self, c: &mut Core, fx: &mut Fx, uid: Uuid, speaker: Option<i64>) {
        self.commit_draft(c, fx);
        let Some(i) = c.st.fg().sentences.index_of_uid(uid) else { return };
        if Self::grouped(c, |c| Self::commit(c, k("Konuşmacıyı Değiştir"), None, |l| l.set_speaker(speaker, i))) {
            fx.all(&EDIT_CHANGES);
        }
    }

    pub(crate) fn assign_new_speaker(&self, c: &mut Core, fx: &mut Fx, uid: Uuid) {
        let next = c.st.fg().sentences.next_new_speaker();
        self.set_speaker(c, fx, uid, Some(next));
    }

    fn nudge_body(&self, c: &mut Core, fx: &mut Fx, edge: TimingEdge, delta: f64) {
        self.commit_draft(c, fx);
        let Some(i) = selected_index(&c.st, true) else { return };
        let changed = Self::grouped(c, |c| {
            Self::commit(c, k("Zamanlamayı Değiştir"), None, |l| match edge {
                TimingEdge::Start => l.nudge_start(i, delta),
                TimingEdge::End => l.nudge_end(i, delta),
            })
        });
        if changed {
            fx.all(&EDIT_CHANGES);
        }
    }

    fn set_edge_body(&self, c: &mut Core, fx: &mut Fx, edge: TimingEdge, time: f64) {
        self.commit_draft(c, fx);
        let Some(i) = selected_index(&c.st, true) else { return };
        let changed = Self::grouped(c, |c| {
            Self::commit(c, k("Zamanlamayı Değiştir"), None, |l| match edge {
                TimingEdge::Start => l.set_start(time, i),
                TimingEdge::End => l.set_end(time, i),
            })
        });
        if changed {
            fx.all(&EDIT_CHANGES);
        }
    }

    fn merge_selected_body(&self, c: &mut Core, fx: &mut Fx) {
        self.commit_draft(c, fx);
        let Some(i) = selected_index(&c.st, true) else { return };
        let lines = &c.st.fg().sentences;
        if i + 1 >= lines.len() {
            return;
        }
        let cleared: HashSet<Uuid> = [lines[i].uid, lines[i + 1].uid].into_iter().collect();
        if Self::grouped(c, |c| Self::commit(c, k("Satırları Birleştir"), Some(cleared), |l| l.merge_with_next(i))) {
            fx.all(&EDIT_CHANGES);
        }
    }

    fn split_selected_body(&self, c: &mut Core, fx: &mut Fx, at: f64) {
        self.commit_draft(c, fx);
        let Some(i) = selected_index(&c.st, true) else { return };
        let cleared: HashSet<Uuid> = [c.st.fg().sentences[i].uid].into_iter().collect();
        if Self::grouped(c, |c| Self::commit(c, k("Satırı Böl"), Some(cleared), |l| l.split_sentence(i, at))) {
            fx.all(&EDIT_CHANGES);
        }
    }

    /// Deletes every selected line (UNIFIED.md §3.5 FIX-W); `Satırı Sil` / `Satırları Sil`.
    fn delete_selected_body(&self, c: &mut Core, fx: &mut Fx) {
        self.commit_draft(c, fx);
        let fg = c.st.fg();
        let mut targets: HashSet<Uuid> = fg.selected_uids.clone();
        if let Some(s) = fg.selection {
            targets.insert(s);
        }
        let indices: Vec<usize> = fg.sentences.iter().enumerate().filter(|(_, s)| targets.contains(&s.uid)).map(|(i, _)| i).collect();
        let Some(&first) = indices.first() else { return };
        let key = if indices.len() > 1 { k("Satırları Sil") } else { k("Satırı Sil") };
        let changed = Self::grouped(c, |c| {
            Self::commit(c, key, None, |l| {
                let mut any = false;
                for &i in indices.iter().rev() {
                    any |= l.delete_sentence(i);
                }
                any
            })
        });
        if !changed {
            return;
        }
        let fg = c.st.fg_mut();
        let next = if fg.sentences.is_empty() { None } else { Some(fg.sentences[first.min(fg.sentences.len() - 1)].uid) };
        fg.set_selection(next);
        fx.all(&EDIT_CHANGES);
    }

    pub(crate) fn set_speaker(&self, c: &mut Core, fx: &mut Fx, uid: Uuid, speaker: Option<i64>) {
        c.undo.begin_group();
        self.set_speaker_body(c, fx, uid, speaker);
        c.undo.end_group();
    }

    pub(crate) fn nudge(&self, c: &mut Core, fx: &mut Fx, edge: TimingEdge, delta: f64) {
        c.undo.begin_group();
        self.nudge_body(c, fx, edge, delta);
        c.undo.end_group();
    }

    pub(crate) fn set_edge(&self, c: &mut Core, fx: &mut Fx, edge: TimingEdge, time: f64) {
        c.undo.begin_group();
        self.set_edge_body(c, fx, edge, time);
        c.undo.end_group();
    }

    pub(crate) fn merge_selected(&self, c: &mut Core, fx: &mut Fx) {
        c.undo.begin_group();
        self.merge_selected_body(c, fx);
        c.undo.end_group();
    }

    pub(crate) fn split_selected(&self, c: &mut Core, fx: &mut Fx, at: f64) {
        c.undo.begin_group();
        self.split_selected_body(c, fx, at);
        c.undo.end_group();
    }

    pub(crate) fn delete_selected(&self, c: &mut Core, fx: &mut Fx) {
        c.undo.begin_group();
        self.delete_selected_body(c, fx);
        c.undo.end_group();
    }

    pub(crate) fn undo(&self, c: &mut Core, fx: &mut Fx) {
        self.finish_editing(c, fx);
        let Core { st, undo } = c;
        undo.undo(st);
        fx.all(&EDIT_CHANGES);
    }

    pub(crate) fn redo(&self, c: &mut Core, fx: &mut Fx) {
        self.finish_editing(c, fx);
        let Core { st, undo } = c;
        undo.redo(st);
        fx.all(&EDIT_CHANGES);
    }
}
