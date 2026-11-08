//! Batch playlist: list operations, list-tab rows and the 3-stage pipeline (≤ 1 audio extraction + 1 lookahead,
//! ≤ 1 recognition on the shared lane, ≤ 1 translation through the gate) (win-viewmodels §14, mac-viewmodels §5.14–
//! §5.15). The orchestrator runs on its own worker thread and drives the UI state through `UiKey::invoke`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, bounded};
use sa_core::appearance::Translations;
use sa_core::cancel::CancellationToken;
use sa_core::engine_types::{EngineError, EngineStage, Sentence, SubtitleFormat};
use sa_core::l10n::LocalizedMessage;
use sa_core::models::DIARIZATION_MODEL_ID;
use sa_core::subtitle_import;
use sa_enginekit::client::EngineClient;
use sa_enginekit::events::EngineEvent;
use sa_providers::ProviderKeyRead;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::session::{
    SessionId, export_line, is_reusable_translation, pairs, saved_message, translation_progress, translation_progress_resumed,
    with_translation_context,
};
use super::{Core, Fx, Inner, MAX_AUDIO_LOOKAHEAD, PlaylistNotice, State, most_accurate};
use crate::jobs::TranslationGate;
use crate::policy::SingleJobsAsListRows;
use crate::types::{
    JobPurpose, JobSettings, JobStatus, ListRow, ListRowOrigin, PlaylistItem, PlaylistItemState, SubtitleChange, stage_label,
};

/// A running playlist.
pub(crate) struct RunHandle {
    pub cancel: CancellationToken,
}

/// `<work>/playlist/<hex(first 6 bytes of SHA-256(standardized path))>`.
pub fn playlist_out_directory(work: &Path, video: &Path) -> PathBuf {
    let std = crate::policy::standardize(&absolute(video));
    let digest = Sha256::digest(std.as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    work.join("playlist").join(hex)
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf()) }
}

fn sirada() -> LocalizedMessage {
    LocalizedMessage::key("Sırada")
}

fn clean_fraction(f: Option<f64>) -> Option<f64> {
    f.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0))
}

fn valid_code(code: &str) -> String {
    if sa_core::models::SubtitleLanguage::from_code(code).is_some() { code.to_owned() } else { String::new() }
}

/// Rows of the translation-list tab (mac-viewmodels §5.15; Windows: playlist items only).
pub(crate) fn list_rows(st: &State) -> Vec<ListRow> {
    let mut rows: Vec<ListRow> = st
        .playlist
        .iter()
        .map(|p| ListRow {
            id: p.id,
            origin: ListRowOrigin::Playlist(p.id),
            video_path: p.video_path.clone(),
            title: p.file_name(),
            status_message: p.status_message.clone(),
            status_text: p.status_text(),
            state: p.state,
            fraction: p.fraction,
            source_language: p.source_language.clone(),
            is_audio_ready: p.is_audio_ready(),
            can_stop: false,
        })
        .collect();
    if st.policy.single_jobs_as_list_rows != SingleJobsAsListRows::RecentJobs {
        return rows;
    }
    for sid in &st.recent_jobs {
        let Some(s) = st.session(*sid) else { continue };
        let Some(video) = s.video.clone() else { continue };
        let k = st.key(&video);
        if st.playlist.iter().any(|p| st.key(&p.video_path) == k) {
            continue;
        }
        let (state, message, fraction, can_stop) = match &s.status {
            JobStatus::Idle => continue,
            JobStatus::Working { label, fraction } => {
                if st.queue.is_queued(*sid) {
                    (PlaylistItemState::Pending, sirada(), None, true)
                } else {
                    (PlaylistItemState::Working, label.clone(), *fraction, true)
                }
            }
            JobStatus::Finished => {
                let (n, d, t, sv) = (s.sentences.len(), s.detected_language.clone(), s.translation_language.clone(), s.saved_subtitle_name.clone());
                let id = format!(
                    "done:{n}:{}:{}:{}",
                    d.clone().unwrap_or_else(|| "-".into()),
                    t.clone().unwrap_or_else(|| "-".into()),
                    sv.clone().unwrap_or_else(|| "-".into())
                );
                let msg = LocalizedMessage::new(id, move || super::session::summary(n, d.as_deref(), t.as_deref(), sv.as_deref(), false));
                (PlaylistItemState::Completed, msg, Some(1.0), false)
            }
            JobStatus::Failed { message } => (PlaylistItemState::Failed, message.clone(), None, false),
        };
        rows.push(ListRow {
            id: Uuid::from_u64_pair(0, *sid),
            origin: ListRowOrigin::Job,
            title: video.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            video_path: video,
            status_text: message.text(),
            status_message: message,
            state,
            fraction,
            source_language: s.lines_source_language.clone().unwrap_or_default(),
            is_audio_ready: false,
            can_stop,
        });
    }
    rows
}

/// What stage 2 found for an item.
enum Stage2Start {
    Ready(SessionId),
    Failed(LocalizedMessage),
    Wait { sid: SessionId, job_id: Uuid, done: Receiver<()> },
}

enum Stage2Outcome {
    Ready(SessionId),
    Failed(LocalizedMessage),
    Cancelled,
    Interrupted,
}

/// Everything stage 3 needs, captured at hand-off.
struct Stage3Job {
    item: Uuid,
    sid: SessionId,
    video: PathBuf,
    lines: Vec<Sentence>,
    pre_translated: Translations,
    source: Option<String>,
    target: String,
    model: String,
    format: SubtitleFormat,
    external: bool,
    refresh_catalog: bool,
    claim: Uuid,
    provider: Option<(Uuid, String, bool)>,
    offline: bool,
}

fn wait(rx: &Receiver<()>, token: &CancellationToken) -> bool {
    loop {
        match rx.recv_timeout(Duration::from_millis(15)) {
            Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return true,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if token.is_cancelled() {
                    return false;
                }
            }
        }
    }
}

impl Inner {
    // ----- row helpers

    pub(crate) fn update_playlist_item(
        &self,
        st: &mut State,
        fx: &mut Fx,
        id: Uuid,
        state: PlaylistItemState,
        message: LocalizedMessage,
        fraction: Option<f64>,
    ) {
        let Some(item) = st.playlist.iter_mut().find(|p| p.id == id) else { return };
        item.state = state;
        item.status_message = message.clone();
        item.fraction = clean_fraction(fraction);
        fx.ch(SubtitleChange::Playlist);
        self.mirror_to_session(st, fx, id, state, message, fraction);
    }

    fn mirror_to_session(
        &self,
        st: &mut State,
        fx: &mut Fx,
        id: Uuid,
        state: PlaylistItemState,
        message: LocalizedMessage,
        fraction: Option<f64>,
    ) {
        let Some(video) = st.playlist.iter().find(|p| p.id == id).map(|p| p.video_path.clone()) else { return };
        let Some(sid) = st.existing_session(&video).or_else(|| {
            st.fg().video.as_ref().filter(|v| st.key(v) == st.key(&video)).map(|_| st.foreground)
        }) else {
            return;
        };
        if st.session(sid).is_none_or(|s| s.batch_item_id != Some(id)) {
            return;
        }
        if st.session(sid).is_some_and(|s| s.job.is_none()) {
            st.mirroring_playlist = true;
            let status = match state {
                PlaylistItemState::Working => JobStatus::Working { label: message.clone(), fraction },
                PlaylistItemState::Pending => JobStatus::Idle,
                PlaylistItemState::Completed => JobStatus::Finished,
                PlaylistItemState::Failed => JobStatus::Failed { message: message.clone() },
            };
            self.set_status(st, fx, sid, status);
            st.mirroring_playlist = false;
        }
        let s = st.session_mut(sid).expect("session");
        if state != PlaylistItemState::Working {
            s.batch_item_id = None;
        }
        if state == PlaylistItemState::Completed {
            s.saved_by_run = true;
            let drop_it = !st.is_foreground(sid) && !st.session(sid).is_some_and(|s| s.has_unsaved_edits) && st.policy.evict_sessions;
            if drop_it && !Self::is_kept(st, sid) {
                let k = st.key(&video);
                st.by_key.remove(&k);
                st.sessions.remove(&sid);
            }
        }
    }

    /// Windows: a single job's row in the playlist panel.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ensure_or_update_video_row(
        &self,
        st: &mut State,
        fx: &mut Fx,
        video: &Path,
        source: &str,
        state: PlaylistItemState,
        message: LocalizedMessage,
        fraction: Option<f64>,
    ) {
        let k = st.key(video);
        let lang = valid_code(source);
        let fraction = clean_fraction(fraction);
        if let Some(item) = st.playlist.iter_mut().find(|p| st.policy.path_key(&p.video_path) == k) {
            item.state = state;
            item.status_message = message;
            item.fraction = fraction;
            if item.source_language.is_empty() {
                item.source_language = lang;
            }
        } else {
            st.playlist.push(PlaylistItem {
                id: Uuid::new_v4(),
                video_path: video.to_path_buf(),
                source_language: lang,
                state,
                status_message: message,
                fraction,
            });
        }
        fx.ch(SubtitleChange::Playlist);
    }

    pub(crate) fn remove_pending_row_for_session(&self, st: &mut State, fx: &mut Fx, sid: SessionId) {
        let Some(video) = st.session(sid).and_then(|s| s.video.clone()) else { return };
        let k = st.key(&video);
        if let Some(pos) = st.playlist.iter().position(|p| {
            st.policy.path_key(&p.video_path) == k && matches!(p.state, PlaylistItemState::Pending | PlaylistItemState::Working)
        }) {
            st.playlist.remove(pos);
            fx.ch(SubtitleChange::Playlist);
        }
    }

    // ----- list operations

    pub(crate) fn add_to_playlist(&self, c: &mut Core, fx: &mut Fx, paths: Vec<PathBuf>, default_source: Option<String>) {
        let st = &mut c.st;
        let lang = valid_code(&default_source.unwrap_or_else(|| st.language.clone()));
        let mut added = false;
        for p in paths {
            if p.as_os_str().is_empty() {
                continue;
            }
            let k = st.key(&p);
            let exists = st.playlist.iter().any(|i| {
                st.policy.path_key(&i.video_path) == k && matches!(i.state, PlaylistItemState::Pending | PlaylistItemState::Working)
            });
            if exists {
                continue;
            }
            st.playlist.push(PlaylistItem {
                id: Uuid::new_v4(),
                video_path: p,
                source_language: lang.clone(),
                state: PlaylistItemState::Pending,
                status_message: sirada(),
                fraction: None,
            });
            added = true;
        }
        if added {
            fx.ch(SubtitleChange::Playlist);
        }
    }

    pub(crate) fn remove_from_playlist(&self, c: &mut Core, fx: &mut Fx, id: Uuid) {
        let Some(item) = c.st.playlist.iter().find(|p| p.id == id).cloned() else { return };
        if item.state == PlaylistItemState::Working {
            return;
        }
        if let Some(sid) = c.st.existing_session(&item.video_path)
            && c.st.queue.is_queued(sid)
        {
            self.dequeue(c, fx, sid);
        }
        c.st.playlist.retain(|p| p.id != id);
        fx.ch(SubtitleChange::Playlist);
    }

    pub(crate) fn clear_playlist(&self, c: &mut Core, fx: &mut Fx) {
        if c.st.playlist_running {
            c.st.playlist.retain(|p| p.state == PlaylistItemState::Working);
        } else {
            if c.st.policy.single_jobs_as_list_rows == SingleJobsAsListRows::PlaylistRows {
                let waiting: Vec<SessionId> = c.st.queue.waiting().iter().map(|e| e.session).collect();
                for sid in waiting {
                    c.st.queue.remove(sid);
                    self.set_status(&mut c.st, fx, sid, JobStatus::Idle);
                }
                c.st.playlist.retain(|p| p.state == PlaylistItemState::Working);
            } else {
                c.st.playlist.clear();
            }
        }
        let forget: Vec<SessionId> = c
            .st
            .recent_jobs
            .iter()
            .copied()
            .filter(|s| c.st.session(*s).is_none_or(|x| !x.status.is_working()))
            .collect();
        for s in forget {
            self.forget_recent_job(&mut c.st, s);
        }
        fx.ch(SubtitleChange::Playlist);
    }

    pub(crate) fn remove_list_row(&self, c: &mut Core, fx: &mut Fx, row: &ListRow) {
        match row.origin {
            ListRowOrigin::Playlist(id) => self.remove_from_playlist(c, fx, id),
            ListRowOrigin::Job => {
                if row.can_stop {
                    self.cancel_job_of(c, fx, &row.video_path);
                } else if let Some(sid) = c.st.existing_session(&row.video_path) {
                    self.forget_recent_job(&mut c.st, sid);
                }
            }
        }
        fx.ch(SubtitleChange::Playlist);
    }

    // ----- run

    /// `start_playlist()`.
    pub(crate) fn start_playlist(self: &Rc<Self>) {
        let run = self.edit(|i, c, fx| -> Option<(CancellationToken, JobSettings)> {
            let st = &mut c.st;
            let can = !st.playlist_running
                && st.playlist.iter().any(|p| matches!(p.state, PlaylistItemState::Pending | PlaylistItemState::Failed));
            if !can {
                return None;
            }
            let win = st.policy.single_jobs_as_list_rows == SingleJobsAsListRows::PlaylistRows;
            st.playlist_notice = None;
            fx.ch(SubtitleChange::Notice);
            if i.svc.engine.is_none() {
                let m = LocalizedMessage::error(&EngineError::NotFound);
                if win {
                    let fg = st.foreground;
                    i.set_status(st, fx, fg, JobStatus::Failed { message: m });
                } else {
                    st.playlist_notice = Some(PlaylistNotice { message: m, has_retry: false });
                }
                return None;
            }
            let mut needed = vec![st.model.clone(), st.translation_model.clone()];
            if st.diarize {
                needed.push(DIARIZATION_MODEL_ID.to_owned());
            }
            let tm = st.translation_model.clone();
            if win {
                let fg = st.foreground;
                if i.blocked_offline(st, fx, fg, &needed, &tm, super::session::OfflineRetry::StartPlaylist) {
                    return None;
                }
            } else if let Some(block) = i.offline_block(st, &needed, &tm) {
                st.playlist_notice = Some(PlaylistNotice { message: block, has_retry: true });
                return None;
            }
            let token = CancellationToken::new();
            st.playlist_running = true;
            st.playlist_run = Some(RunHandle { cancel: token.clone() });
            st.stage2_started.clear();
            st.audio_ready.clear();
            fx.all(&[SubtitleChange::Playlist, SubtitleChange::Status]);
            Some((token, st.job_settings()))
        });
        let Some((token, settings)) = run else { return };
        let Some(engine) = self.svc.engine.clone() else { return };
        let key = self.key();
        let ui = self.svc.ui.clone();
        let gate = self.gate.clone();
        let work = self.svc.shell.engine_work_dir();
        let (per_item, flavor) = self.read(|c| (c.st.policy.playlist_item_work_dir, c.st.policy.import_flavor));
        let _ = std::thread::Builder::new().name("sa-playlist".into()).spawn(move || {
            let orch =
                Orchestrator { key, ui, engine, gate, token, settings, work, per_item, flavor, extractions: HashMap::new(), last: None };
            orch.run();
        });
    }

    /// `cancel_playlist()`: the run, every extraction and stage-3 task, and every batch item's job.
    pub(crate) fn cancel_playlist(&self, c: &mut Core, fx: &mut Fx) {
        if let Some(run) = &c.st.playlist_run {
            run.cancel.cancel();
        }
        c.st.queue.cancel_batch_waiter();
        let batch: Vec<SessionId> = c.st.sessions.values().filter(|s| s.batch_item_id.is_some()).map(|s| s.id).collect();
        for sid in batch {
            self.stop_job(c, fx, sid, true);
        }
        fx.ch(SubtitleChange::Playlist);
    }

    fn playlist_finished(self: &Rc<Self>, cancelled: bool) {
        self.edit(|i, c, fx| {
            let st = &mut c.st;
            if cancelled {
                let ids: Vec<Uuid> = st
                    .playlist
                    .iter()
                    .filter(|p| p.state == PlaylistItemState::Working || p.is_audio_ready())
                    .map(|p| p.id)
                    .collect();
                for id in ids {
                    i.update_playlist_item(st, fx, id, PlaylistItemState::Pending, sirada(), None);
                }
            }
            st.playlist_background_pending = 0;
            st.playlist_run = None;
            st.playlist_running = false;
            st.stage2_started.clear();
            i.prune_sessions(st);
            fx.all(&[SubtitleChange::Playlist, SubtitleChange::Status]);
        });
    }

    fn model_fitting(&self, language: &str, preferred: &str) -> String {
        let models = self.catalog.models_for(language);
        if models.is_empty() || models.iter().any(|m| m.id == preferred) {
            return preferred.to_owned();
        }
        most_accurate(&models).map(|m| m.id.clone()).unwrap_or_else(|| preferred.to_owned())
    }

    fn stage2_begin(self: &Rc<Self>, item: Uuid, settings: JobSettings, sidecar: Option<(PathBuf, Vec<Sentence>)>) -> Stage2Start {
        self.edit(|i, c, fx| {
            let Some(it) = c.st.playlist.iter().find(|p| p.id == item).cloned() else {
                return Stage2Start::Failed(sirada());
            };
            let sid = i.session_for(&mut c.st, fx, &it.video_path);
            i.dequeue(c, fx, sid);
            let mut item_settings = settings.clone();
            item_settings.language = it.source_language.clone();
            item_settings.model = i.model_fitting(&it.source_language, &settings.model);
            if c.st.session(sid).is_some_and(|s| s.sentences.is_empty())
                && let Some((path, lines)) = sidecar
                && !lines.is_empty()
            {
                c.st.session_mut(sid).expect("session").load(path, lines);
            }
            c.st.session_mut(sid).expect("session").batch_item_id = Some(item);
            let has_lines = c.st.session(sid).is_some_and(|s| !s.sentences.is_empty());
            if !has_lines {
                let mut ids = vec![item_settings.model.clone()];
                if item_settings.diarize {
                    ids.push(DIARIZATION_MODEL_ID.to_owned());
                }
                let tm = item_settings.translation_model.clone();
                if i.blocked_offline(&mut c.st, fx, sid, &ids, &tm, super::session::OfflineRetry::StartPlaylist)
                    && let Some(JobStatus::Failed { message }) = c.st.session(sid).map(|s| s.status.clone())
                {
                    return Stage2Start::Failed(message);
                }
                i.start_generate(c, fx, sid, false, item_settings, Some(item));
                let Some(s) = c.st.session(sid) else { return Stage2Start::Failed(sirada()) };
                if s.job.is_none() {
                    return match &s.status {
                        JobStatus::Failed { message } => Stage2Start::Failed(message.clone()),
                        _ => Stage2Start::Failed(LocalizedMessage::key("Videoda konuşma bulunamadı.")),
                    };
                }
                let job_id = s.job_id;
                let (tx, rx) = bounded(1);
                c.st.job_waiters.insert((sid, job_id), tx);
                return Stage2Start::Wait { sid, job_id, done: rx };
            }
            if !it.source_language.is_empty() {
                c.st.session_mut(sid).expect("session").lines_source_language = Some(it.source_language.clone());
            }
            Stage2Start::Ready(sid)
        })
    }

    fn stage2_outcome(self: &Rc<Self>, sid: SessionId, job_id: Uuid, token: &CancellationToken) -> Stage2Outcome {
        self.read(|c| {
            if token.is_cancelled() {
                return Stage2Outcome::Cancelled;
            }
            let Some(s) = c.st.session(sid) else { return Stage2Outcome::Interrupted };
            if s.job_id != job_id || s.loaded_subtitle.is_some() {
                return Stage2Outcome::Interrupted;
            }
            if let JobStatus::Failed { message } = &s.status {
                return Stage2Outcome::Failed(message.clone());
            }
            if s.sentences.is_empty() {
                return Stage2Outcome::Failed(LocalizedMessage::key("Videoda konuşma bulunamadı."));
            }
            Stage2Outcome::Ready(sid)
        })
    }

    fn hand_off(self: &Rc<Self>, item: Uuid, sid: SessionId, settings: &JobSettings) -> Option<Stage3Job> {
        self.edit(|i, c, fx| {
            let it = c.st.playlist.iter().find(|p| p.id == item).cloned()?;
            let s = c.st.session(sid)?;
            let lines = s.sentences.clone();
            let pre_translated = s.translations.clone();
            let source = s.translation_source(&it.source_language);
            let external = c.st.is_external_llm_active();
            let refresh_catalog = !external && i.catalog.model(&settings.translation_model).is_some_and(|m| !m.downloaded);
            let initial = translation_progress(pre_translated.len(), lines.len());
            let (label, f) = match initial {
                Some(f) => (stage_label(EngineStage::Translating, Some(f), JobPurpose::Translation), Some(f)),
                None => (LocalizedMessage::key("Çeviri hazırlanıyor…"), None),
            };
            i.update_playlist_item(&mut c.st, fx, item, PlaylistItemState::Working, label, f);
            let claim = Uuid::new_v4();
            if !external {
                i.claim_models(&mut c.st, claim, [settings.translation_model.clone()].into_iter().collect());
            }
            c.st.session_mut(sid)?.is_playlist_translating = true;
            c.st.playlist_background_pending += 1;
            let provider = c
                .st
                .provider
                .active
                .as_ref()
                .filter(|_| external)
                .map(|p| (p.id, p.name.clone(), !c.st.provider.on_local_network));
            fx.ch(SubtitleChange::Status);
            Some(Stage3Job {
                item,
                sid,
                video: it.video_path.clone(),
                lines,
                pre_translated,
                source,
                target: settings.translation_target.clone(),
                model: settings.translation_model.clone(),
                format: settings.export_format,
                external,
                refresh_catalog,
                claim,
                provider,
                offline: c.st.offline_mode,
            })
        })
    }

    fn stage3_row(self: &Rc<Self>, item: Uuid, state: PlaylistItemState, message: LocalizedMessage, fraction: Option<f64>) {
        self.edit(|i, c, fx| i.update_playlist_item(&mut c.st, fx, item, state, message, fraction));
    }

    fn stage3_ended(self: &Rc<Self>, sid: SessionId, claim: Uuid) {
        self.edit(|i, c, fx| {
            c.st.playlist_background_pending = c.st.playlist_background_pending.saturating_sub(1);
            if let Some(s) = c.st.session_mut(sid) {
                s.is_playlist_translating = false;
            }
            i.release_models(&mut c.st, claim);
            fx.all(&[SubtitleChange::Status, SubtitleChange::Playlist]);
        });
    }
}

/// The playlist run (worker thread).
struct Orchestrator {
    key: crate::uiref::UiKey<Inner>,
    ui: Arc<dyn sa_core::dispatch::UiDispatcher>,
    engine: Arc<dyn EngineClient>,
    gate: Arc<TranslationGate>,
    token: CancellationToken,
    settings: JobSettings,
    work: PathBuf,
    per_item: bool,
    flavor: subtitle_import::ImportFlavor,
    extractions: HashMap<Uuid, Receiver<()>>,
    last: Option<Receiver<()>>,
}

impl Orchestrator {
    fn invoke<R: Send + 'static>(&self, f: impl FnOnce(&Rc<Inner>) -> R + Send + 'static) -> Option<R> {
        self.key.invoke(&self.ui, f)
    }

    fn out_dir(&self, video: &Path) -> PathBuf {
        if self.per_item { playlist_out_directory(&self.work, video) } else { self.work.clone() }
    }

    fn row(&self, item: Uuid, state: PlaylistItemState, message: LocalizedMessage, fraction: Option<f64>) {
        let _ = self.invoke(move |i| i.stage3_row(item, state, message, fraction));
    }

    fn next_item(&self) -> Option<(Uuid, PathBuf)> {
        let has: Vec<Uuid> = self.extractions.keys().copied().collect();
        self.invoke(move |i| {
            i.read(|c| {
                c.st.playlist
                    .iter()
                    .find(|p| {
                        !c.st.stage2_started.contains(&p.id)
                            && (matches!(p.state, PlaylistItemState::Pending | PlaylistItemState::Failed) || has.contains(&p.id))
                    })
                    .map(|p| (p.id, p.video_path.clone()))
            })
        })
        .flatten()
    }

    /// Items waiting for recognition (lookahead candidates).
    fn waiting_items(&self, n: usize) -> Vec<(Uuid, PathBuf)> {
        let has: Vec<Uuid> = self.extractions.keys().copied().collect();
        self.invoke(move |i| {
            i.read(|c| {
                c.st.playlist
                    .iter()
                    .filter(|p| {
                        !c.st.stage2_started.contains(&p.id)
                            && (matches!(p.state, PlaylistItemState::Pending | PlaylistItemState::Failed) || has.contains(&p.id))
                    })
                    .take(n)
                    .map(|p| (p.id, p.video_path.clone()))
                    .collect()
            })
        })
        .unwrap_or_default()
    }

    /// Stage 1: strictly serial audio extraction (an item needing none still takes its slot).
    fn ensure_extraction(&mut self, item: Uuid, video: &Path) -> Receiver<()> {
        if let Some(rx) = self.extractions.get(&item) {
            return rx.clone();
        }
        let (tx, rx) = bounded::<()>(1);
        let needs = !self.token.is_cancelled() && subtitle_import::find_sidecar_subtitle(video).is_none() && {
            let v = video.to_path_buf();
            self.invoke(move |i| i.read(|c| c.st.existing_session(&v).and_then(|s| c.st.session(s)).is_none_or(|s| s.sentences.is_empty())))
                .unwrap_or(false)
        };
        if needs {
            let prev = self.last.clone();
            let engine = self.engine.clone();
            let token = self.token.child();
            let key = self.key;
            let ui = self.ui.clone();
            let video = video.to_path_buf();
            let out = self.out_dir(&video);
            let _ = std::thread::Builder::new().name("sa-extract".into()).spawn(move || {
                if let Some(p) = prev {
                    let _ = wait(&p, &token);
                }
                let _ = std::fs::create_dir_all(&out);
                let mut finished = false;
                if !token.is_cancelled() {
                    let stream = engine.extract_audio(&video, &out, &token);
                    for ev in stream {
                        if token.is_cancelled() {
                            break;
                        }
                        match ev {
                            Ok(EngineEvent::Stage { stage, fraction }) => {
                                let _ = key.invoke(&ui, move |i| {
                                    i.edit(|i, c, fx| {
                                        if !c.st.stage2_started.contains(&item) {
                                            i.update_playlist_item(
                                                &mut c.st,
                                                fx,
                                                item,
                                                PlaylistItemState::Working,
                                                stage_label(stage, fraction, JobPurpose::Transcription),
                                                fraction,
                                            );
                                        }
                                    })
                                });
                            }
                            Ok(EngineEvent::Result(r)) => {
                                finished = true;
                                if !r.audio.is_empty() {
                                    let a = PathBuf::from(r.audio);
                                    let _ = key.invoke(&ui, move |i| i.edit(|_, c, _| {
                                        if !c.st.policy.delete_wav_on_result {
                                            c.st.temp_files.push(a);
                                        }
                                    }));
                                }
                            }
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                }
                let cancelled = token.is_cancelled();
                let _ = key.invoke(&ui, move |i| {
                    i.edit(|i, c, fx| {
                        if c.st.stage2_started.contains(&item) {
                            return;
                        }
                        if finished && !cancelled {
                            c.st.audio_ready.insert(item);
                            i.update_playlist_item(&mut c.st, fx, item, PlaylistItemState::Pending, LocalizedMessage::key("Ses hazır · Sırada"), Some(1.0));
                        } else {
                            i.update_playlist_item(&mut c.st, fx, item, PlaylistItemState::Pending, sirada(), None);
                        }
                    })
                });
                drop(tx);
            });
            self.last = Some(rx.clone());
        } else {
            drop(tx);
        }
        self.extractions.insert(item, rx.clone());
        rx
    }

    fn schedule_lookahead(&mut self) {
        for (id, video) in self.waiting_items(MAX_AUDIO_LOOKAHEAD) {
            self.ensure_extraction(id, &video);
        }
    }

    fn acquire_lane(&self) -> bool {
        let (tx, rx) = bounded::<bool>(1);
        let granted = self.invoke(move |i| i.edit(|_, c, _| c.st.queue.acquire_batch_slot(tx))).flatten();
        if granted == Some(true) {
            return true;
        }
        loop {
            match rx.recv_timeout(Duration::from_millis(15)) {
                Ok(g) => {
                    if g && self.token.is_cancelled() {
                        self.release_lane();
                        return false;
                    }
                    return g;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return false,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if self.token.is_cancelled() {
                        let _ = self.invoke(|i| i.edit(|_, c, _| c.st.queue.cancel_batch_waiter()));
                        // A grant may have raced the cancel.
                        if let Ok(true) = rx.try_recv() {
                            self.release_lane();
                        }
                        return false;
                    }
                }
            }
        }
    }

    fn release_lane(&self) {
        let _ = self.invoke(|i| {
            i.edit(|i, c, fx| {
                let adv = c.st.queue.release_batch_slot();
                i.handle_advanced(c, fx, adv);
            })
        });
    }

    fn transcribe(&self, item: Uuid, video: &Path) -> Stage2Outcome {
        let sidecar = subtitle_import::find_sidecar_subtitle(video)
            .and_then(|p| subtitle_import::parse_file_with(&p, self.flavor).ok().map(|lines| (p, lines)));
        let settings = self.settings.clone();
        let Some(start) = self.invoke(move |i| i.stage2_begin(item, settings, sidecar)) else { return Stage2Outcome::Cancelled };
        match start {
            Stage2Start::Ready(sid) => {
                if self.token.is_cancelled() { Stage2Outcome::Cancelled } else { Stage2Outcome::Ready(sid) }
            }
            Stage2Start::Failed(m) => Stage2Outcome::Failed(m),
            Stage2Start::Wait { sid, job_id, done } => {
                if !wait(&done, &self.token) {
                    return Stage2Outcome::Cancelled;
                }
                let token = self.token.clone();
                self.invoke(move |i| i.stage2_outcome(sid, job_id, &token)).unwrap_or(Stage2Outcome::Cancelled)
            }
        }
    }

    fn run(mut self) {
        let mut background: Vec<std::thread::JoinHandle<()>> = Vec::new();
        loop {
            if self.token.is_cancelled() {
                break;
            }
            let Some((item, video)) = self.next_item() else { break };
            let ex = self.ensure_extraction(item, &video);
            let _ = wait(&ex, &self.token);
            if self.token.is_cancelled() {
                self.row(item, PlaylistItemState::Pending, sirada(), None);
                break;
            }
            if !self.acquire_lane() {
                self.row(item, PlaylistItemState::Pending, sirada(), None);
                break;
            }
            let mut holds_lane = true;
            let ready = self
                .invoke(move |i| {
                    i.edit(|_, c, _| {
                        c.st.stage2_started.insert(item);
                        c.st.audio_ready.contains(&item)
                    })
                })
                .unwrap_or(false);
            self.schedule_lookahead();
            self.row(item, PlaylistItemState::Working, LocalizedMessage::key("Başlıyor…"), ready.then_some(1.0));
            let outcome = self.transcribe(item, &video);
            let mut stop = false;
            match outcome {
                Stage2Outcome::Ready(sid) => {
                    let (external, memory, threshold) = self
                        .invoke(|i| i.read(|c| (c.st.is_external_llm_active(), c.st.physical_memory, c.st.policy.playlist_overlap_requires_memory)))
                        .unwrap_or((false, 0, None));
                    let may_overlap = match threshold {
                        None => true,
                        Some(t) => external || memory >= t,
                    };
                    if may_overlap {
                        self.release_lane();
                        holds_lane = false;
                    }
                    // Backlog of one: wait for earlier translations.
                    let pending = !background.is_empty() && background.iter().any(|h| !h.is_finished());
                    if pending {
                        self.row(item, PlaylistItemState::Working, LocalizedMessage::key("Çeviri hazırlanıyor…"), None);
                        for h in background.drain(..) {
                            while !h.is_finished() && !self.token.is_cancelled() {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            if h.is_finished() {
                                let _ = h.join();
                            }
                        }
                    } else {
                        background.retain(|h| !h.is_finished());
                    }
                    if self.token.is_cancelled() {
                        self.row(item, PlaylistItemState::Pending, sirada(), None);
                        stop = true;
                    } else {
                        let settings = self.settings.clone();
                        let job = self.invoke(move |i| i.hand_off(item, sid, &settings)).flatten();
                        if let Some(job) = job {
                            let h = self.spawn_stage3(job);
                            if may_overlap {
                                background.push(h);
                            } else {
                                let _ = h.join();
                            }
                        }
                    }
                }
                Stage2Outcome::Failed(m) => self.row(item, PlaylistItemState::Failed, m, None),
                Stage2Outcome::Cancelled => {
                    self.row(item, PlaylistItemState::Pending, sirada(), None);
                    stop = true;
                }
                Stage2Outcome::Interrupted => self.row(item, PlaylistItemState::Pending, sirada(), None),
            }
            if holds_lane {
                self.release_lane();
            }
            if stop {
                break;
            }
        }
        for h in background {
            let _ = h.join();
        }
        let last = self.last.take();
        if let Some(l) = last {
            let t = CancellationToken::new();
            let _ = wait(&l, &t);
        }
        let cancelled = self.token.is_cancelled();
        let _ = self.invoke(move |i| i.playlist_finished(cancelled));
    }

    fn spawn_stage3(&self, job: Stage3Job) -> std::thread::JoinHandle<()> {
        let key = self.key;
        let ui = self.ui.clone();
        let engine = self.engine.clone();
        let gate = self.gate.clone();
        let token = self.token.child();
        std::thread::Builder::new()
            .name("sa-playlist-translate".into())
            .spawn(move || {
                let (sid, claim) = (job.sid, job.claim);
                translate_and_save(&key, &ui, engine.as_ref(), &gate, &token, job);
                let _ = key.invoke(&ui, move |i| i.stage3_ended(sid, claim));
            })
            .expect("spawn stage 3")
    }
}

#[allow(clippy::too_many_lines)]
fn translate_and_save(
    key: &crate::uiref::UiKey<Inner>,
    ui: &Arc<dyn sa_core::dispatch::UiDispatcher>,
    engine: &dyn EngineClient,
    gate: &Arc<TranslationGate>,
    token: &CancellationToken,
    job: Stage3Job,
) {
    let item = job.item;
    let row = |state: PlaylistItemState, m: LocalizedMessage, f: Option<f64>| {
        let _ = key.invoke(ui, move |i| i.stage3_row(item, state, m, f));
    };
    if token.is_cancelled() {
        row(PlaylistItemState::Pending, sirada(), None);
        return;
    }
    if job.source.as_deref() == Some(job.target.as_str()) {
        row(PlaylistItemState::Failed, LocalizedMessage::key("Kaynak ve hedef dil aynı olamaz."), None);
        return;
    }
    if job.model == "nllb-1.3b" && job.source.is_none() && !job.external {
        row(PlaylistItemState::Failed, LocalizedMessage::key("NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç."), None);
        return;
    }
    let mut translated: HashMap<Uuid, (String, bool)> = HashMap::new();
    for l in &job.lines {
        if let Some(e) = job.pre_translated.get(&l.uid)
            && is_reusable_translation(l, e, &job.target)
        {
            translated.insert(l.uid, (e.text.clone(), e.needs_review));
        }
    }
    let pre_done = translated.len();
    let total = job.lines.len().max(1);
    let remaining: Vec<Sentence> = job.lines.iter().filter(|l| !translated.contains_key(&l.uid)).cloned().collect();
    let mut engine_error: Option<String> = None;
    let mut problem: Option<LocalizedMessage> = None;
    if !remaining.is_empty() {
        let Some(permit) = gate.acquire(token) else {
            row(PlaylistItemState::Pending, sirada(), None);
            return;
        };
        // starting_translation: offline / key checks at hand-off time.
        if let Some((pid, name, cloud)) = &job.provider {
            let offline_now = key.invoke(ui, |i| i.read(|c| c.st.offline_mode)).unwrap_or(job.offline);
            if offline_now && *cloud {
                let n = name.clone();
                problem = Some(LocalizedMessage::new(format!("offline-cloud:{name}"), move || {
                    sa_core::l!(
                        "Çevrimdışı mod açık: çeviri internetteki bir sağlayıcıya (%@) gönderilemez. Çevrimdışı modu kapat ya da yerel bir model veya sunucu seç.",
                        n.as_str()
                    )
                }));
            } else {
                let services = key.invoke(ui, |i| (i.svc.providers.clone(), i.svc.launch_state.clone()));
                if let Some((p, launch)) = services {
                    if matches!(p.read_key(*pid), ProviderKeyRead::Unavailable) {
                        let n = name.clone();
                        problem = Some(LocalizedMessage::new(format!("key-unreadable:{name}"), move || {
                            sa_core::l!("%@ için API anahtarı okunamadı. Anahtarı yeniden girip tekrar dene.", n.as_str())
                        }));
                    } else if p.active_provider_id() == Some(*pid) {
                        // The key is read again at hand-off (mac-viewmodels §5.14 `starting_translation`).
                        launch.set_external_llm(p.active_external_llm().map(|a| a.llm));
                    }
                }
            }
        }
        if problem.is_none() {
            let request = with_translation_context(&job.lines, &remaining);
            let stream = engine.translate(&request, &job.target, job.source.as_deref(), &job.model, token);
            for ev in stream {
                if token.is_cancelled() {
                    break;
                }
                match ev {
                    Ok(EngineEvent::Stage { stage, fraction }) => {
                        let f = match (stage, fraction) {
                            (EngineStage::Translating, Some(part)) => Some(translation_progress_resumed(pre_done, remaining.len(), part, total)),
                            _ => fraction,
                        };
                        row(PlaylistItemState::Working, stage_label(stage, f, JobPurpose::Translation), f);
                    }
                    Ok(EngineEvent::Translation { language, sentences }) => {
                        let p: Vec<(Sentence, Sentence)> =
                            pairs(&sentences, &remaining).into_iter().filter(|(r, _)| !r.text.is_empty()).collect();
                        for (r, o) in &p {
                            translated.insert(o.uid, (r.text.clone(), r.needs_review));
                        }
                        let uids: Vec<Uuid> = job.lines.iter().map(|l| l.uid).collect();
                        let sid = job.sid;
                        let _ = key.invoke(ui, move |i| {
                            i.edit(|_, c, fx| {
                                let Some(s) = c.st.session_mut(sid) else { return };
                                let same = s.sentences.iter().map(|l| l.uid).eq(uids.iter().copied());
                                if same {
                                    s.store_translations(&p, &language, true, false, None);
                                    fx.ch(SubtitleChange::Translations);
                                }
                            })
                        });
                    }
                    Ok(EngineEvent::Error { message }) => engine_error = Some(message),
                    Ok(_) => {}
                    Err(e) => {
                        if !token.is_cancelled() && !matches!(e, EngineError::Cancelled) {
                            engine_error = Some(e.to_string());
                        }
                        break;
                    }
                }
            }
        }
        drop(permit);
    }
    if token.is_cancelled() {
        row(PlaylistItemState::Pending, sirada(), None);
        return;
    }
    if let Some(p) = problem {
        row(PlaylistItemState::Failed, p, None);
        return;
    }
    if let Some(e) = engine_error {
        row(PlaylistItemState::Failed, LocalizedMessage::verbatim(e), None);
        return;
    }
    if translated.is_empty() {
        row(PlaylistItemState::Failed, LocalizedMessage::key("Çeviri tamamlanamadı."), None);
        return;
    }
    let final_lines: Vec<Sentence> = job
        .lines
        .iter()
        .map(|l| match translated.get(&l.uid) {
            Some((t, r)) => export_line(l, t, *r),
            None => export_line(l, "", false),
        })
        .collect();
    let text = match engine.render(&final_lines, job.format, token) {
        Ok(t) => t,
        Err(_) if token.is_cancelled() => {
            row(PlaylistItemState::Pending, sirada(), None);
            return;
        }
        Err(EngineError::Cancelled) => {
            row(PlaylistItemState::Pending, sirada(), None);
            return;
        }
        Err(e) => {
            row(PlaylistItemState::Failed, LocalizedMessage::error(&e), None);
            return;
        }
    };
    if token.is_cancelled() {
        row(PlaylistItemState::Pending, sirada(), None);
        return;
    }
    let target = job.video.with_extension(job.format.raw());
    if let Err(e) = super::files::write_atomic(&target, &text) {
        row(PlaylistItemState::Failed, super::files::save_failure(&e.to_string()), None);
        return;
    }
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let uids: Vec<Uuid> = job.lines.iter().map(|l| l.uid).collect();
    let (sid, video, refresh) = (job.sid, job.video.clone(), job.refresh_catalog);
    let _ = key.invoke(ui, move |i| {
        i.svc.media_index.invalidate_sidecar(&video);
        let hook = i.hooks.borrow().subtitle_saved.clone();
        if let Some(h) = hook {
            h(&target);
        }
        i.edit(|i, c, fx| {
            if let Some(s) = c.st.session_mut(sid)
                && s.sentences.iter().map(|l| l.uid).eq(uids.iter().copied())
            {
                s.set_saved_subtitle_name(Some(name.clone()));
            }
            i.update_playlist_item(&mut c.st, fx, item, PlaylistItemState::Completed, saved_message(&name), Some(1.0));
            if refresh {
                i.catalog.load();
            }
        });
    });
}
