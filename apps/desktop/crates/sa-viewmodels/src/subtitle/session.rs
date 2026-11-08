//! Per-video state (win-viewmodels §2.5, mac-viewmodels §4) and the pure translation/list helpers both specs share
//! (win-viewmodels §10.1, §11; mac-viewmodels §3.5, §5.6).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sa_core::appearance::{SubtitleDisplay, TranslationEntry, Translations};
use sa_core::cancel::CancellationToken;
use sa_core::engine_types::Sentence;
use sa_core::l10n::LocalizedMessage;
use uuid::Uuid;

use crate::types::{ExportLanguage, JobKind, JobSettings, JobStatus, TextDraft};

/// Identity of a session (macOS reference identity; Windows path key).
pub type SessionId = u64;

/// What "go online and retry" re-runs.
#[derive(Debug, Clone, PartialEq)]
pub enum OfflineRetry {
    Request { kind: JobKind, settings: JobSettings },
    StartPlaylist,
}

/// Background (external LLM) pre-translation bookkeeping.
#[derive(Debug, Clone, Default)]
pub struct AsyncState {
    pub queued: HashSet<Uuid>,
    pub run_id: Uuid,
    pub task_running: bool,
    pub draining: bool,
    pub transcription_complete: bool,
    pub failed: bool,
    pub shown_in_status: bool,
    /// Settings snapshot of the job that started the pump (UNIFIED.md §3.7 FIX-W).
    pub settings: Option<JobSettings>,
    /// Kills the batch in flight.
    pub cancel: Option<CancellationToken>,
}

/// The running job's context (settings snapshot and flags).
#[derive(Debug, Clone, Default)]
pub struct JobContext {
    pub settings: Option<JobSettings>,
    pub then_translate: bool,
    pub batch_item: Option<Uuid>,
    pub wants_async: bool,
    pub failed: bool,
    pub awaiting_async: bool,
}

/// One video's state.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: SessionId,
    pub video: Option<PathBuf>,
    pub sentences: Vec<Sentence>,
    pub translations: Translations,
    pub translation_language: Option<String>,
    pub detected_language: Option<String>,
    pub lines_source_language: Option<String>,
    pub loaded_subtitle: Option<PathBuf>,
    pub saved_subtitle_name: Option<String>,
    pub has_unsaved_edits: bool,
    pub status: JobStatus,
    pub is_translation_job_running: bool,
    pub is_async_translating: bool,
    pub async_fraction: Option<f64>,
    pub is_playlist_translating: bool,
    pub display_mode: SubtitleDisplay,
    pub export_language: ExportLanguage,
    pub selection: Option<Uuid>,
    pub selected_uids: HashSet<Uuid>,
    pub draft: Option<TextDraft>,
    pub translation_draft: Option<TextDraft>,
    // bookkeeping
    pub job: Option<CancellationToken>,
    pub job_id: Uuid,
    pub offline_retry: Option<OfflineRetry>,
    pub saved_by_run: bool,
    pub batch_item_id: Option<Uuid>,
    pub asyncs: AsyncState,
    pub ctx: JobContext,
}

impl Session {
    pub fn new(id: SessionId, video: Option<PathBuf>, display_mode: SubtitleDisplay) -> Self {
        Self {
            id,
            video,
            sentences: Vec::new(),
            translations: HashMap::new(),
            translation_language: None,
            detected_language: None,
            lines_source_language: None,
            loaded_subtitle: None,
            saved_subtitle_name: None,
            has_unsaved_edits: false,
            status: JobStatus::Idle,
            is_translation_job_running: false,
            is_async_translating: false,
            async_fraction: None,
            is_playlist_translating: false,
            display_mode,
            export_language: ExportLanguage::Original,
            selection: None,
            selected_uids: HashSet::new(),
            draft: None,
            translation_draft: None,
            job: None,
            job_id: Uuid::new_v4(),
            offline_retry: None,
            saved_by_run: false,
            batch_item_id: None,
            asyncs: AsyncState { run_id: Uuid::new_v4(), ..AsyncState::default() },
            ctx: JobContext::default(),
        }
    }

    // ----- setters with side effects

    /// Status setter: not Working → translation job flag off; not Failed → offline retry dropped.
    pub fn set_status(&mut self, status: JobStatus) -> bool {
        if !status.is_working() {
            self.is_translation_job_running = false;
        }
        if !matches!(status, JobStatus::Failed { .. }) {
            self.offline_retry = None;
        }
        if self.status == status {
            return false;
        }
        self.status = status;
        true
    }

    pub fn set_async_translating(&mut self, on: bool) {
        self.is_async_translating = on;
        if !on {
            self.async_fraction = None;
        }
    }

    pub fn set_sentences(&mut self, sentences: Vec<Sentence>) {
        self.sentences = sentences;
        if !self.selected_uids.is_empty() || self.selection.is_some() {
            let uids = self.selected_uids.clone();
            let active = self.selection;
            self.set_selected_lines(uids, active);
        }
    }

    pub fn set_translations(&mut self, translations: Translations) {
        self.translations = translations;
        if self.translations.is_empty() {
            self.export_language = ExportLanguage::Original;
        }
    }

    pub fn set_translation_language(&mut self, language: Option<String>) {
        if language.is_none() {
            self.export_language = ExportLanguage::Original;
        }
        self.translation_language = language;
    }

    /// Display ↔ export coupling (one level of recursion; win-viewmodels §3.3, mac-viewmodels §4).
    pub fn set_display_mode(&mut self, mode: SubtitleDisplay) -> bool {
        if self.display_mode == mode {
            return false;
        }
        self.display_mode = mode;
        if self.has_translations() {
            match mode {
                SubtitleDisplay::Translation => self.export_language = ExportLanguage::Translated,
                SubtitleDisplay::Original => self.export_language = ExportLanguage::Original,
                SubtitleDisplay::Both => {}
            }
        }
        true
    }

    pub fn set_export_language(&mut self, language: ExportLanguage) -> bool {
        if self.export_language == language {
            return false;
        }
        self.export_language = language;
        if self.has_translations() {
            match (language, self.display_mode) {
                (ExportLanguage::Translated, SubtitleDisplay::Original) => self.display_mode = SubtitleDisplay::Translation,
                (ExportLanguage::Original, SubtitleDisplay::Translation) => self.display_mode = SubtitleDisplay::Original,
                _ => {}
            }
        }
        true
    }

    pub fn set_saved_subtitle_name(&mut self, name: Option<String>) {
        if name.is_some() {
            self.has_unsaved_edits = false;
        }
        self.saved_subtitle_name = name;
    }

    // ----- derived

    pub fn has_translations(&self) -> bool {
        !self.translations.is_empty()
    }
    pub fn effective_display_mode(&self) -> SubtitleDisplay {
        if self.has_translations() { self.display_mode } else { SubtitleDisplay::Original }
    }
    pub fn is_working(&self) -> bool {
        self.is_async_translating || self.status.is_working()
    }
    pub fn has_single_selection(&self) -> bool {
        self.selected_uids.len() == 1 && self.selection.is_some()
    }
    /// `lines_source_language ?? (language non-empty ? language : detected_language)`.
    pub fn translation_source(&self, language: &str) -> Option<String> {
        if let Some(l) = &self.lines_source_language {
            return Some(l.clone());
        }
        if !language.is_empty() { Some(language.to_owned()) } else { self.detected_language.clone() }
    }
    /// Windows `OpenVideo` restore test, negated: nothing worth restoring (no lines, translations, status, background
    /// translation, loaded file or job).
    pub fn is_blank(&self) -> bool {
        self.sentences.is_empty()
            && self.translations.is_empty()
            && self.status == JobStatus::Idle
            && !self.is_async_translating
            && self.loaded_subtitle.is_none()
            && self.job.is_none()
    }
    pub fn has_content(&self) -> bool {
        !self.sentences.is_empty() || self.job.is_some() || self.is_working()
    }
    /// The path a subtitle of this session is saved next to.
    pub fn base_path(&self) -> Option<&Path> {
        self.video.as_deref().or(self.loaded_subtitle.as_deref())
    }

    /// `%lld satır · dil: %@` (+ ` · çeviri: %@`, ` · kaydedildi: %@`).
    pub fn finished_summary(&self, detected_placeholder: bool) -> String {
        summary(
            self.sentences.len(),
            self.detected_language.as_deref(),
            self.translation_language.as_deref(),
            self.saved_subtitle_name.as_deref(),
            detected_placeholder,
        )
    }

    // ----- selection

    /// `valid = uids ∩ existing`; active = `active` if valid, else current selection if valid, else first in document
    /// order, else None.
    pub fn set_selected_lines(&mut self, uids: HashSet<Uuid>, active: Option<Uuid>) {
        let valid: HashSet<Uuid> = self.sentences.iter().map(|s| s.uid).filter(|u| uids.contains(u)).collect();
        let next = active
            .filter(|a| valid.contains(a))
            .or(self.selection.filter(|s| valid.contains(s)))
            .or_else(|| self.sentences.iter().map(|s| s.uid).find(|u| valid.contains(u)));
        self.selected_uids = valid;
        self.selection = next;
    }

    /// Single-select setter.
    pub fn set_selection(&mut self, uid: Option<Uuid>) {
        let set: HashSet<Uuid> = uid.into_iter().collect();
        self.set_selected_lines(set, uid);
    }

    /// macOS list selection change: the newly added line farthest from the anchor becomes active.
    pub fn update_selection(&mut self, uids: HashSet<Uuid>) {
        let added: Vec<(usize, Uuid)> = self
            .sentences
            .iter()
            .enumerate()
            .filter(|(_, s)| uids.contains(&s.uid) && !self.selected_uids.contains(&s.uid))
            .map(|(i, s)| (i, s.uid))
            .collect();
        let anchor = self.selection.and_then(|u| self.sentences.iter().position(|s| s.uid == u));
        let active = match anchor {
            Some(a) => {
                let mut best: Option<(usize, Uuid)> = None;
                for c in &added {
                    let d = c.0.abs_diff(a);
                    if best.is_none_or(|b| d > b.0.abs_diff(a)) {
                        best = Some(*c);
                    }
                }
                best.map(|b| b.1)
            }
            None => added.first().map(|c| c.1),
        };
        self.set_selected_lines(uids, active);
    }

    /// macOS `select_row(uid)`.
    pub fn select_row(&mut self, uid: Uuid) {
        if self.selected_uids.contains(&uid) {
            let uids = self.selected_uids.clone();
            self.set_selected_lines(uids, Some(uid));
        } else {
            self.set_selected_lines([uid].into_iter().collect(), Some(uid));
        }
    }

    // ----- translations

    /// `translations[uid].needs_review ?? sentence.needs_review`.
    pub fn needs_review(&self, s: &Sentence) -> bool {
        self.translations.get(&s.uid).map(|e| e.needs_review).unwrap_or(s.needs_review)
    }

    pub fn is_translation_stale(&self, s: &Sentence) -> bool {
        self.translations.get(&s.uid).is_some_and(|e| e.is_stale(s))
    }

    /// Lines as exported: translated text where available, words always cleared on the translated list.
    pub fn export_sentences(&self, translated: bool) -> Vec<Sentence> {
        if !translated {
            return self.sentences.clone();
        }
        self.sentences
            .iter()
            .map(|s| match self.translations.get(&s.uid) {
                Some(e) => export_line(s, &e.text, e.needs_review),
                None => export_line(s, "", false),
            })
            .collect()
    }

    /// `store_translations(pairs, language, merge, preserve_other_languages, review_fallback)` (mac-viewmodels §4).
    pub fn store_translations(
        &mut self,
        pairs: &[(Sentence, Sentence)],
        language: &str,
        merge: bool,
        preserve_other_languages: bool,
        review_fallback: Option<&Translations>,
    ) {
        let had = self.has_translations();
        let previous = review_fallback.cloned().unwrap_or_else(|| self.translations.clone());
        let mut entries = if merge
            && (preserve_other_languages
                || self.translation_language.as_deref() == Some(language)
                || self.translations.is_empty())
        {
            self.translations.clone()
        } else {
            HashMap::new()
        };
        for (result, original) in pairs {
            if result.needs_review
                && let Some(e) = previous.get(&original.uid)
                && (preserve_other_languages || e.language == language)
            {
                let mut e = e.clone();
                e.needs_review = true;
                entries.insert(original.uid, e);
                continue;
            }
            entries.insert(
                original.uid,
                TranslationEntry {
                    text: result.text.clone(),
                    source_text: original.text.clone(),
                    language: language.to_owned(),
                    needs_review: result.needs_review,
                },
            );
        }
        self.set_translations(entries);
        if !pairs.is_empty() {
            self.has_unsaved_edits = true;
        }
        self.translation_language = Some(language.to_owned());
        if !had && self.has_translations() {
            if self.display_mode == SubtitleDisplay::Original {
                self.display_mode = SubtitleDisplay::Translation;
            }
            self.export_language = ExportLanguage::Translated;
        }
    }

    /// `next_async_batch()` (mac-viewmodels §4).
    pub fn next_async_batch(&self) -> Vec<Sentence> {
        let a = &self.asyncs;
        if a.draining || a.failed {
            return Vec::new();
        }
        let complete = a.transcription_complete;
        let n = self.sentences.len();
        if (complete && n == 0) || (!complete && n <= 2) {
            return Vec::new();
        }
        let stable = if complete { n } else { n - 2 };
        let candidates: Vec<Sentence> = self.sentences[..stable]
            .iter()
            .filter(|s| !a.queued.contains(&s.uid) && !s.text.trim().is_empty())
            .cloned()
            .collect();
        let need = if complete { 1 } else { crate::subtitle::MIN_ASYNC_BATCH_SENTENCES };
        if candidates.len() >= need { candidates } else { Vec::new() }
    }

    /// Clears everything a new transcription replaces.
    pub fn reset_for_new_run(&mut self) {
        self.set_sentences(Vec::new());
        self.set_translations(HashMap::new());
        self.translation_draft = None;
        self.set_translation_language(None);
        self.selection = None;
        self.selected_uids.clear();
        self.draft = None;
        self.saved_subtitle_name = None;
        self.has_unsaved_edits = false;
        self.detected_language = None;
        self.lines_source_language = None;
        self.loaded_subtitle = None;
        self.saved_by_run = false;
    }

    /// Loads an imported subtitle file into this session.
    pub fn load(&mut self, subtitle: PathBuf, lines: Vec<Sentence>) {
        self.loaded_subtitle = Some(subtitle);
        self.saved_by_run = false;
        self.saved_subtitle_name = None;
        self.has_unsaved_edits = false;
        self.set_translations(HashMap::new());
        self.translation_draft = None;
        self.draft = None;
        self.selection = None;
        self.selected_uids.clear();
        self.detected_language = None;
        self.lines_source_language = None;
        self.set_translation_language(None);
        self.display_mode = SubtitleDisplay::Original;
        self.export_language = ExportLanguage::Original;
        self.set_sentences(lines);
        self.set_status(JobStatus::Finished);
    }
}

/// The finished summary text (`detected_placeholder`: Windows shows `?` without a detected language).
pub fn summary(
    count: usize,
    detected: Option<&str>,
    translation: Option<&str>,
    saved: Option<&str>,
    detected_placeholder: bool,
) -> String {
    use sa_core::l;
    let mut s = match detected {
        Some(d) => l!("%lld satır · dil: %@", count as i64, d),
        None if detected_placeholder => l!("%lld satır · dil: %@", count as i64, "?"),
        None => l!("%lld satır", count as i64),
    };
    if let Some(t) = translation {
        s.push_str(&l!(" · çeviri: %@", t));
    }
    if let Some(n) = saved {
        s.push_str(&l!(" · kaydedildi: %@", n));
    }
    s
}

/// `export_line(line, text, needs_review)`: copy; non-empty `text` replaces the text and sets the flag; words cleared.
pub fn export_line(line: &Sentence, text: &str, needs_review: bool) -> Sentence {
    let mut s = line.clone();
    if !text.is_empty() {
        s.text = text.to_owned();
        s.needs_review = needs_review;
    }
    s.words = Vec::new();
    s
}

/// `None` if nothing is done; else `clamp(done/total, 0, 0.99)`.
pub fn translation_progress(done: usize, total: usize) -> Option<f64> {
    if done == 0 || total == 0 {
        return None;
    }
    Some((done as f64 / total as f64).clamp(0.0, 0.99))
}

/// Progress of a request on top of already finished lines.
pub fn translation_progress_resumed(already_done: usize, requested: usize, part: f64, total: usize) -> f64 {
    let part = if part.is_finite() { part } else { 0.0 };
    if total == 0 {
        return part.clamp(0.0, 0.99);
    }
    ((already_done as f64 + requested as f64 * part.clamp(0.0, 1.0)) / total as f64).clamp(0.0, 0.99)
}

/// Swift `caseInsensitiveCompare == .orderedSame` approximation.
fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// A translation that a "translate what is missing" run may keep.
pub fn is_reusable_translation(sentence: &Sentence, entry: &TranslationEntry, target: &str) -> bool {
    if entry.needs_review || entry.is_stale(sentence) || entry.language != target {
        return false;
    }
    let t = entry.text.trim();
    if t.is_empty() {
        return false;
    }
    let s = sentence.text.trim();
    !(s.chars().count() >= 4 && eq_ignore_case(t, s))
}

/// Indices `i-2..=i+2` around every requested uid, in document order, unique, ids untouched.
pub fn with_translation_context(all: &[Sentence], requested: &[Sentence]) -> Vec<Sentence> {
    if requested.is_empty() || all.is_empty() {
        return Vec::new();
    }
    let wanted: HashSet<Uuid> = requested.iter().map(|s| s.uid).collect();
    let n = all.len();
    let mut include = vec![false; n];
    for (i, s) in all.iter().enumerate() {
        if wanted.contains(&s.uid) {
            let lo = i.saturating_sub(2);
            let hi = (i + 2).min(n - 1);
            for flag in include.iter_mut().take(hi + 1).skip(lo) {
                *flag = true;
            }
        }
    }
    all.iter().zip(include).filter(|(_, f)| *f).map(|(s, _)| s.clone()).collect()
}

/// Pairs translated results with their source lines by engine `id` only (first occurrence wins); others dropped.
pub fn pairs(translated: &[Sentence], source: &[Sentence]) -> Vec<(Sentence, Sentence)> {
    let mut by_id: HashMap<i64, &Sentence> = HashMap::new();
    for s in source {
        by_id.entry(s.id).or_insert(s);
    }
    translated.iter().filter_map(|r| by_id.get(&r.id).map(|o| (r.clone(), (*o).clone()))).collect()
}

/// Keeps uids stable across partial snapshots (win-viewmodels §10.1 = mac-enginekit §13; banker's rounding kept).
pub fn merge_sentences_preserving_uid(existing: &[Sentence], incoming: Vec<Sentence>) -> Vec<Sentence> {
    if existing.is_empty() {
        return incoming;
    }
    type Key = (i64, i64, String);
    let key = |s: &Sentence| -> Key {
        ((s.start * 1000.0).round_ties_even() as i64, (s.end * 1000.0).round_ties_even() as i64, s.text.clone())
    };
    let mut queues: HashMap<Key, std::collections::VecDeque<Uuid>> = HashMap::new();
    for s in existing {
        queues.entry(key(s)).or_default().push_back(s.uid);
    }
    let mut used: HashSet<Uuid> = HashSet::new();
    let mut assigned: Vec<Option<Uuid>> = vec![None; incoming.len()];
    for (i, s) in incoming.iter().enumerate() {
        if let Some(q) = queues.get_mut(&key(s)) {
            while let Some(u) = q.pop_front() {
                if used.insert(u) {
                    assigned[i] = Some(u);
                    break;
                }
            }
        }
    }
    for (i, s) in incoming.iter().enumerate() {
        if assigned[i].is_some() {
            continue;
        }
        if let Some(old) = existing.iter().find(|o| !used.contains(&o.uid) && o.text == s.text && (o.start - s.start).abs() <= 1.5) {
            used.insert(old.uid);
            assigned[i] = Some(old.uid);
        }
    }
    incoming
        .into_iter()
        .zip(assigned)
        .map(|(mut s, a)| {
            if let Some(u) = a {
                s.uid = u;
            }
            s
        })
        .collect()
}

/// `RevertedTranslations(from, to, valid)` (win-viewmodels §15.4): undoing an edit made before a re-translation never
/// throws the newer translation away; entries of another language are not brought back.
pub fn reverted_translations(
    current: &Translations,
    current_language: Option<&str>,
    from: &Translations,
    to: &Translations,
    valid: Option<&HashSet<Uuid>>,
) -> Translations {
    let mut result = current.clone();
    let keys: HashSet<Uuid> = from.keys().chain(to.keys()).copied().collect();
    for uid in keys {
        let before = from.get(&uid);
        let after = to.get(&uid);
        if before == after || current.get(&uid) != before {
            continue;
        }
        match after {
            None => {
                result.remove(&uid);
            }
            Some(a) => {
                if current_language.is_none_or(|l| a.language == l) {
                    result.insert(uid, a.clone());
                }
            }
        }
    }
    if let Some(v) = valid {
        result.retain(|k, _| v.contains(k));
    }
    result
}

/// `LocalizedMessage("saved:<name>", L("Kaydedildi: %@", name))`.
pub fn saved_message(name: &str) -> LocalizedMessage {
    let n = name.to_owned();
    LocalizedMessage::new(format!("saved:{name}"), move || sa_core::l!("Kaydedildi: %@", n.as_str()))
}
