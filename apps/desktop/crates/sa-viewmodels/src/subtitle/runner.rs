//! Jobs: request / begin / stop, transcription, translation, background pre-translation and the offline gate
//! (win-viewmodels §6, §9–§13; mac-viewmodels §5.4–§5.8).

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use sa_core::cancel::CancellationToken;
use sa_core::engine_types::{EngineError, EngineStage, Sentence};
use sa_core::l;
use sa_core::l10n::LocalizedMessage;
use sa_core::models::{DIARIZATION_MODEL_ID, ModelLabel};
use sa_core::settings::keys;
use sa_enginekit::client::TranscribeOptions;
use sa_enginekit::download_phase::{DownloadKind, DownloadPhaseTracker};
use sa_enginekit::events::EngineEvent;
use sa_providers::ProviderKeyRead;
use uuid::Uuid;

use super::session::{
    JobContext, OfflineRetry, SessionId, is_reusable_translation, merge_sentences_preserving_uid, pairs, saved_message,
    translation_progress, translation_progress_resumed, with_translation_context,
};
use super::{Core, Fx, Inner, JobRequest, State};
use crate::jobs::{Advanced, QueueEntry, SubmitOutcome};
use crate::policy::SingleJobsAsListRows;
use crate::types::{JobKind, JobPurpose, JobSettings, JobStatus, PlaylistItemState, SubtitleChange, stage_label};

/// A planned translation.
#[derive(Debug, Clone)]
pub(crate) struct TranslationPlan {
    pub request: Vec<Sentence>,
    pub contextual: Vec<Sentence>,
    pub already_done: usize,
    pub total: usize,
    pub baseline: sa_core::appearance::Translations,
    pub source: Option<String>,
    pub target: String,
    pub model: String,
    pub selected: bool,
    pub reuse: bool,
}

fn key_unreadable_message(name: &str) -> LocalizedMessage {
    let n = name.to_owned();
    LocalizedMessage::new(format!("key-unreadable:{name}"), move || {
        l!("%@ için API anahtarı okunamadı. Anahtarı yeniden girip tekrar dene.", n.as_str())
    })
}

fn offline_cloud_message(name: &str) -> LocalizedMessage {
    let n = name.to_owned();
    LocalizedMessage::new(format!("offline-cloud:{name}"), move || {
        l!(
            "Çevrimdışı mod açık: çeviri internetteki bir sağlayıcıya (%@) gönderilemez. Çevrimdışı modu kapat ya da yerel bir model veya sunucu seç.",
            n.as_str()
        )
    })
}

impl Inner {
    fn is_win(st: &State) -> bool {
        st.policy.single_jobs_as_list_rows == SingleJobsAsListRows::PlaylistRows
    }

    /// The single place that changes a session's status: mirrors into playlist rows (Windows single jobs, macOS
    /// batch items) and notifies.
    pub(crate) fn set_status(&self, st: &mut State, fx: &mut Fx, sid: SessionId, status: JobStatus) {
        let Some(s) = st.session_mut(sid) else { return };
        let changed = s.set_status(status);
        if st.is_foreground(sid) {
            fx.ch(SubtitleChange::Status);
        }
        if changed {
            self.session_status_changed(st, fx, sid);
        }
        fx.ch(SubtitleChange::Playlist);
    }

    pub(crate) fn session_status_changed(&self, st: &mut State, fx: &mut Fx, sid: SessionId) {
        let Some(s) = st.session(sid) else { return };
        if !st.mirroring_playlist && st.playlist_running {
            if let (Some(item), JobStatus::Working { label, fraction }) = (s.batch_item_id, s.status.clone()) {
                self.update_playlist_item(st, fx, item, PlaylistItemState::Working, label, fraction);
                return;
            }
        }
        // Windows: every single-video job appears as a row of the playlist panel.
        if Self::is_win(st) && !st.playlist_running && s.batch_item_id.is_none() {
            let Some(video) = s.video.clone() else { return };
            let src = s.detected_language.clone().unwrap_or_else(|| st.language.clone());
            match s.status.clone() {
                JobStatus::Working { label, fraction } => {
                    let state = if label.id() == "key:Sırada" { PlaylistItemState::Pending } else { PlaylistItemState::Working };
                    self.ensure_or_update_video_row(st, fx, &video, &src, state, label, fraction);
                }
                JobStatus::Finished if !s.is_async_translating => {
                    let msg = match &s.saved_subtitle_name {
                        Some(n) => saved_message(n),
                        None => {
                            let summary = s.finished_summary(true);
                            let id = format!(
                                "done:{}:{}:{}",
                                video.display(),
                                s.sentences.len(),
                                s.translation_language.clone().unwrap_or_default()
                            );
                            let (n, d, t, sv) = (
                                s.sentences.len(),
                                s.detected_language.clone(),
                                s.translation_language.clone(),
                                s.saved_subtitle_name.clone(),
                            );
                            let _ = summary;
                            LocalizedMessage::new(id, move || {
                                super::session::summary(n, d.as_deref(), t.as_deref(), sv.as_deref(), true)
                            })
                        }
                    };
                    self.ensure_or_update_video_row(st, fx, &video, &src, PlaylistItemState::Completed, msg, Some(1.0));
                }
                JobStatus::Failed { message } => {
                    self.ensure_or_update_video_row(st, fx, &video, &src, PlaylistItemState::Failed, message, None);
                }
                _ => {}
            }
        }
    }

    // ----- offline gate

    /// `offline_block(ids)` (mac-viewmodels §5.8 / win-viewmodels §6).
    pub(crate) fn offline_block(&self, st: &State, ids: &[String], translation_model: &str) -> Option<LocalizedMessage> {
        if !st.offline_mode {
            return None;
        }
        if ids.iter().any(|i| i == translation_model)
            && let Some(name) = st.active_cloud_provider_name()
        {
            return Some(offline_cloud_message(&name));
        }
        let external = st.is_external_llm_active();
        let mut missing: Vec<String> = Vec::new();
        for id in ids {
            if missing.contains(id) {
                continue;
            }
            let Some(m) = self.catalog.model(id) else { continue };
            if external && m.is_translation() {
                continue;
            }
            if !m.downloaded {
                missing.push(id.clone());
            }
        }
        if missing.is_empty() {
            return None;
        }
        let ids_joined = missing.join(",");
        Some(LocalizedMessage::new(format!("offline:{ids_joined}"), move || {
            let names: Vec<String> = missing.iter().map(|i| ModelLabel::name(i)).collect();
            l!("%@ indirilmemiş. Çevrimdışı mod açıkken model indirilemez.", names.join(", "))
        }))
    }

    /// Sets the failure (Windows: on the foreground, KEEP-W) and the retry; true when blocked.
    pub(crate) fn blocked_offline(
        &self,
        st: &mut State,
        fx: &mut Fx,
        sid: SessionId,
        ids: &[String],
        translation_model: &str,
        retry: OfflineRetry,
    ) -> bool {
        let target = if Self::is_win(st) { st.foreground } else { sid };
        if let Some(s) = st.session_mut(target) {
            s.offline_retry = None;
        }
        let Some(block) = self.offline_block(st, ids, translation_model) else { return false };
        self.set_status(st, fx, target, JobStatus::Failed { message: block });
        if let Some(s) = st.session_mut(target) {
            s.offline_retry = Some(retry);
        }
        true
    }

    pub(crate) fn go_online_and_retry(&self, c: &mut Core, fx: &mut Fx) {
        let st = &mut c.st;
        let fg = st.foreground;
        if !matches!(st.fg().status, JobStatus::Failed { .. }) {
            return;
        }
        let Some(retry) = st.fg_mut().offline_retry.take() else { return };
        self.set_status(st, fx, fg, JobStatus::Idle);
        st.offline_mode = false;
        self.svc.settings.set_bool(keys::OFFLINE_MODE, false);
        self.publish_launch_settings(st);
        fx.ch(SubtitleChange::Settings);
        match retry {
            OfflineRetry::Request { kind, settings } => self.request(c, fx, kind, fg, settings),
            OfflineRetry::StartPlaylist => fx.then(|inner| inner.start_playlist()),
        }
    }

    // ----- recent jobs (macOS list rows)

    pub(crate) fn note_recent_job(&self, st: &mut State, sid: SessionId) {
        if st.policy.single_jobs_as_list_rows != SingleJobsAsListRows::RecentJobs {
            return;
        }
        let Some(video) = st.session(sid).and_then(|s| s.video.clone()) else { return };
        if !st.recent_jobs.contains(&sid) {
            st.recent_jobs.push(sid);
        }
        let k = st.key(&video);
        st.by_key.insert(k, sid);
    }

    pub(crate) fn forget_recent_job(&self, st: &mut State, sid: SessionId) {
        st.recent_jobs.retain(|s| *s != sid);
    }

    // ----- request / begin / end / stop

    /// `request(kind, session, settings)`.
    pub(crate) fn request(&self, c: &mut Core, fx: &mut Fx, kind: JobKind, sid: SessionId, settings: JobSettings) {
        let Some(s) = c.st.session(sid) else { return };
        if s.batch_item_id.is_some() {
            return;
        }
        // Preflight.
        match &kind {
            JobKind::Generate { then_translate } => {
                if s.video.is_none() {
                    return;
                }
                if self.svc.engine.is_none() {
                    self.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::error(&EngineError::NotFound) });
                    return;
                }
                let mut ids = vec![settings.model.clone()];
                if settings.diarize {
                    ids.push(DIARIZATION_MODEL_ID.to_owned());
                }
                if *then_translate {
                    ids.push(settings.translation_model.clone());
                }
                let retry = OfflineRetry::Request { kind: kind.clone(), settings: settings.clone() };
                if self.blocked_offline(&mut c.st, fx, sid, &ids, &settings.translation_model, retry) {
                    return;
                }
            }
            JobKind::Translate { reuse_existing, only } => {
                if self.prepare_translation(c, fx, sid, *reuse_existing, only.clone(), &settings).is_none() {
                    return;
                }
            }
        }
        let st = &mut c.st;
        self.note_recent_job(st, sid);
        let s = st.session(sid).expect("session");
        let previous_status = st
            .queue
            .queued_entry(sid)
            .map(|e| e.payload.previous_status.clone())
            .unwrap_or_else(|| if s.job.is_none() { s.status.clone() } else { JobStatus::Idle });
        let entry = QueueEntry { id: Uuid::new_v4(), session: sid, payload: JobRequest { kind, settings, previous_status } };
        if st.queue.is_running(sid) {
            self.stop_job(c, fx, sid, false);
            c.st.queue.take_over(entry.clone());
            self.begin(c, fx, entry);
            return;
        }
        if c.st.session(sid).is_some_and(|s| s.job.is_some()) {
            self.stop_job(c, fx, sid, true);
        }
        let (outcome, started) = c.st.queue.submit(entry);
        match outcome {
            SubmitOutcome::Started => {
                if let Some(e) = started {
                    self.begin(c, fx, e);
                }
            }
            SubmitOutcome::Queued => {
                self.set_status(&mut c.st, fx, sid, JobStatus::Working { label: LocalizedMessage::key("Sırada"), fraction: None });
            }
        }
        fx.ch(SubtitleChange::Playlist);
    }

    /// The lane started `entry`.
    pub(crate) fn begin(&self, c: &mut Core, fx: &mut Fx, entry: QueueEntry<JobRequest>) {
        let sid = entry.session;
        match entry.payload.kind.clone() {
            JobKind::Generate { then_translate } => self.start_generate(c, fx, sid, then_translate, entry.payload.settings.clone(), None),
            JobKind::Translate { reuse_existing, only } => {
                self.start_translation(c, fx, sid, reuse_existing, only, entry.payload.settings.clone())
            }
        }
        let launched = c.st.session(sid).is_some_and(|s| s.job.is_some());
        if !launched {
            if c.st.session(sid).is_some_and(|s| s.status.is_working()) {
                self.set_status(&mut c.st, fx, sid, entry.payload.previous_status.clone());
            }
            let adv = c.st.queue.job_ended(sid);
            self.handle_advanced(c, fx, adv);
        }
    }

    pub(crate) fn handle_advanced(&self, c: &mut Core, fx: &mut Fx, adv: Advanced<JobRequest>) {
        if let Advanced::Start(e) = adv {
            self.begin(c, fx, e);
        }
        fx.ch(SubtitleChange::Playlist);
    }

    /// `job_did_end(session, id)`.
    pub(crate) fn job_did_end(&self, c: &mut Core, fx: &mut Fx, sid: SessionId, job_id: Uuid) {
        self.release_models(&mut c.st, job_id);
        if let Some(tx) = c.st.job_waiters.remove(&(sid, job_id)) {
            let _ = tx.send(());
        }
        let current = c.st.session(sid).is_some_and(|s| s.job_id == job_id);
        if current {
            if let Some(s) = c.st.session_mut(sid) {
                s.job = None;
            }
            let adv = c.st.queue.job_ended(sid);
            self.handle_advanced(c, fx, adv);
        }
        fx.all(&[SubtitleChange::Status, SubtitleChange::Playlist]);
    }

    /// `stop_job(session, advancing)`: does not touch the status.
    pub(crate) fn stop_job(&self, c: &mut Core, fx: &mut Fx, sid: SessionId, advancing: bool) {
        c.st.queue.remove(sid);
        let Some(s) = c.st.session_mut(sid) else { return };
        if let Some(t) = s.job.take() {
            t.cancel();
        }
        let old = s.job_id;
        s.job_id = Uuid::new_v4();
        s.ctx = JobContext::default();
        self.release_models(&mut c.st, old);
        self.cancel_async_translation(&mut c.st, sid);
        if advancing {
            let adv = c.st.queue.job_ended(sid);
            self.handle_advanced(c, fx, adv);
        }
        fx.ch(SubtitleChange::Status);
    }

    /// Removes a waiting entry and restores the status it had.
    pub(crate) fn dequeue(&self, c: &mut Core, fx: &mut Fx, sid: SessionId) -> bool {
        let Some(e) = c.st.queue.remove(sid) else { return false };
        let prev = e.payload.previous_status.clone();
        self.set_status(&mut c.st, fx, sid, prev.clone());
        if prev == JobStatus::Idle {
            self.forget_recent_job(&mut c.st, sid);
            if Self::is_win(&c.st) {
                self.remove_pending_row_for_session(&mut c.st, fx, sid);
            }
        }
        true
    }

    pub(crate) fn clear_queue(&self, c: &mut Core, fx: &mut Fx) {
        let waiting: Vec<SessionId> = c.st.queue.waiting().iter().map(|e| e.session).collect();
        for sid in waiting {
            self.dequeue(c, fx, sid);
        }
        fx.ch(SubtitleChange::Playlist);
    }

    /// Stop / Esc.
    pub(crate) fn cancel(&self, c: &mut Core, fx: &mut Fx) {
        let fg = c.st.foreground;
        if Self::is_win(&c.st) {
            // Windows: dequeue the foreground's waiting job, else cancel everything.
            if !c.st.playlist_running && c.st.fg().video.is_some() && !c.st.queue.is_running(fg) && c.st.queue.is_queued(fg) {
                self.dequeue(c, fx, fg);
                return;
            }
            self.cancel_everything(c, fx);
            return;
        }
        if c.st.workspace == crate::types::Workspace::Playlist && c.st.playlist_running {
            self.cancel_playlist(c, fx);
            return;
        }
        let s = c.st.fg();
        if s.batch_item_id.is_some() || s.is_playlist_translating {
            self.cancel_playlist(c, fx);
        } else if self.dequeue(c, fx, fg) {
        } else if c.st.fg().job.is_some() || c.st.fg().is_working() {
            self.stop_job(c, fx, fg, true);
            self.forget_recent_job(&mut c.st, fg);
            self.set_status(&mut c.st, fx, fg, JobStatus::Idle);
        } else if c.st.playlist_running {
            self.cancel_playlist(c, fx);
        }
    }

    /// Windows `Cancel()` second branch / shutdown: every queued job, the playlist and the running job.
    pub(crate) fn cancel_everything(&self, c: &mut Core, fx: &mut Fx) {
        let waiting: Vec<SessionId> = c.st.queue.waiting().iter().map(|e| e.session).collect();
        for sid in waiting {
            c.st.queue.remove(sid);
            self.set_status(&mut c.st, fx, sid, JobStatus::Idle);
            self.remove_pending_row_for_session(&mut c.st, fx, sid);
        }
        self.cancel_playlist(c, fx);
        let running: Vec<SessionId> = c.st.sessions.values().filter(|s| s.job.is_some() || s.is_working()).map(|s| s.id).collect();
        for sid in running {
            self.stop_job(c, fx, sid, true);
            self.set_status(&mut c.st, fx, sid, JobStatus::Idle);
        }
        let fg = c.st.foreground;
        if c.st.fg().status.is_working() {
            self.set_status(&mut c.st, fx, fg, JobStatus::Idle);
        }
        c.st.claims.clear();
        self.publish_claims(&c.st);
    }

    pub(crate) fn cancel_job_of(&self, c: &mut Core, fx: &mut Fx, video: &std::path::Path) {
        let Some(sid) = c.st.existing_session(video).or_else(|| {
            c.st.fg().video.as_ref().filter(|v| c.st.key(v) == c.st.key(video)).map(|_| c.st.foreground)
        }) else {
            return;
        };
        let s = c.st.session(sid).expect("session");
        if s.batch_item_id.is_some() {
            self.cancel_playlist(c, fx);
        } else if !self.dequeue(c, fx, sid) {
            self.stop_job(c, fx, sid, true);
            self.forget_recent_job(&mut c.st, sid);
            self.set_status(&mut c.st, fx, sid, JobStatus::Idle);
        }
    }

    // ----- transcription

    /// `start_generate(session, then_translate, settings, batch_item)` (mac-viewmodels §5.5).
    pub(crate) fn start_generate(
        &self,
        c: &mut Core,
        fx: &mut Fx,
        sid: SessionId,
        then_translate: bool,
        settings: JobSettings,
        batch_item: Option<Uuid>,
    ) {
        let Some(video) = c.st.session(sid).and_then(|s| s.video.clone()) else { return };
        let Some(engine) = self.svc.engine.clone() else {
            self.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::error(&EngineError::NotFound) });
            return;
        };
        self.cancel_async_translation(&mut c.st, sid);
        let wants_async = (then_translate || batch_item.is_some()) && c.st.is_external_async_active();
        let is_fg = c.st.is_foreground(sid);
        {
            let s = c.st.session_mut(sid).expect("session");
            s.is_translation_job_running = false;
            s.reset_for_new_run();
        }
        if is_fg {
            c.undo.clear();
            c.st.model_notice = None;
            fx.all(&[SubtitleChange::Sentences, SubtitleChange::Translations, SubtitleChange::Selection, SubtitleChange::Undo]);
        }
        self.set_status(&mut c.st, fx, sid, JobStatus::Working { label: LocalizedMessage::key("Başlıyor…"), fraction: None });
        let work = self.svc.shell.engine_work_dir();
        let out_dir = if batch_item.is_some() && c.st.policy.playlist_item_work_dir {
            super::playlist::playlist_out_directory(&work, &video)
        } else {
            work
        };
        let options = TranscribeOptions {
            language: (!settings.language.is_empty()).then(|| settings.language.clone()),
            model: settings.model.clone(),
            formats: vec![sa_core::engine_types::SubtitleFormat::Srt],
            diarize: settings.diarize,
            speakers: (settings.diarize && settings.speaker_count >= 2).then_some(settings.speaker_count as u32),
        };
        let downloaded = |id: &str| self.catalog.model(id).map(|m| m.downloaded).unwrap_or(true);
        let diar_downloaded = self.catalog.diarization_models().first().map(|m| m.downloaded).unwrap_or(true);
        let tracker = DownloadPhaseTracker::new(downloaded(&settings.model), settings.diarize, diar_downloaded);

        let token = CancellationToken::new();
        let job_id = Uuid::new_v4();
        {
            let s = c.st.session_mut(sid).expect("session");
            if let Some(t) = s.job.take() {
                t.cancel();
            }
            s.job_id = job_id;
            s.job = Some(token.clone());
            s.ctx = JobContext { settings: Some(settings.clone()), then_translate, batch_item, wants_async, failed: false, awaiting_async: false };
            s.asyncs.settings = Some(settings.clone());
        }
        let mut models: HashSet<String> = [settings.model.clone()].into_iter().collect();
        if settings.diarize {
            models.insert(DIARIZATION_MODEL_ID.to_owned());
        }
        let mut sorted: Vec<&String> = models.iter().collect();
        sorted.sort();
        self.svc.diagnostics.write(
            "Job",
            &format!("StartJob jobId={job_id} video='{}' models=[{}]", video.display(), sorted.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",")),
        );
        self.claim_models(&mut c.st, job_id, models);

        let key = self.key();
        let ui = self.svc.ui.clone();
        let _ = std::thread::Builder::new().name("sa-transcribe".into()).spawn(move || {
            let mut tracker = tracker;
            let stream = engine.transcribe(&video, &out_dir, &options, &token);
            let mut outcome: Result<(), EngineError> = Ok(());
            for item in stream {
                if token.is_cancelled() {
                    outcome = Err(EngineError::Cancelled);
                    break;
                }
                match item {
                    Ok(ev) => {
                        let purpose = match &ev {
                            EngineEvent::Stage { stage, fraction } => match tracker.update(*stage, *fraction) {
                                Some(DownloadKind::Diarization) => JobPurpose::Diarization,
                                _ => JobPurpose::Transcription,
                            },
                            _ => JobPurpose::Transcription,
                        };
                        let keep = key.invoke(&ui, move |inner| inner.on_generate_event(sid, job_id, ev, purpose)).unwrap_or(false);
                        if !keep {
                            return;
                        }
                    }
                    Err(e) => {
                        outcome = Err(if token.is_cancelled() { EngineError::Cancelled } else { e });
                        break;
                    }
                }
            }
            if token.is_cancelled() && outcome.is_ok() {
                outcome = Err(EngineError::Cancelled);
            }
            key.post(&ui, move |inner| inner.on_generate_end(sid, job_id, outcome));
        });
    }

    fn is_current_job(st: &State, sid: SessionId, job_id: Uuid) -> bool {
        st.session(sid).is_some_and(|s| s.job_id == job_id && s.job.as_ref().is_some_and(|t| !t.is_cancelled()))
    }

    fn on_generate_event(self: &Rc<Self>, sid: SessionId, job_id: Uuid, ev: EngineEvent, purpose: JobPurpose) -> bool {
        self.edit(|i, c, fx| {
            if !Self::is_current_job(&c.st, sid, job_id) {
                return false;
            }
            let is_fg = c.st.is_foreground(sid);
            match ev {
                EngineEvent::Stage { stage, fraction } => {
                    i.set_status(&mut c.st, fx, sid, JobStatus::Working { label: stage_label(stage, fraction, purpose), fraction });
                    if matches!(stage, EngineStage::Diarizing | EngineStage::Segmenting) {
                        c.st.session_mut(sid).expect("session").asyncs.transcription_complete = true;
                        if c.st.session(sid).is_some_and(|s| s.ctx.wants_async) {
                            i.start_async_translation(&mut c.st, fx, sid);
                        }
                    }
                }
                EngineEvent::Partial { language, sentences } => {
                    let s = c.st.session_mut(sid).expect("session");
                    let merged = merge_sentences_preserving_uid(&s.sentences, sentences);
                    s.set_sentences(merged);
                    if !language.is_empty() && language != "?" {
                        s.detected_language = Some(language);
                        i.language_detected(&mut c.st, fx, sid);
                    }
                    if c.st.session(sid).is_some_and(|s| s.ctx.wants_async) {
                        i.start_async_translation(&mut c.st, fx, sid);
                    }
                    if is_fg {
                        fx.all(&[SubtitleChange::Sentences, SubtitleChange::Selection]);
                    }
                }
                EngineEvent::Result(r) => {
                    let delete_wav = c.st.policy.delete_wav_on_result;
                    let s = c.st.session_mut(sid).expect("session");
                    s.asyncs.draining = true;
                    let merged = merge_sentences_preserving_uid(&s.sentences, r.sentences);
                    s.set_sentences(merged);
                    s.detected_language = Some(r.language.clone());
                    if let Some(lang) = s.ctx.settings.as_ref().map(|x| x.language.clone()).filter(|l| !l.is_empty()) {
                        s.lines_source_language = Some(lang);
                    }
                    i.language_detected(&mut c.st, fx, sid);
                    i.update_async_fraction(&mut c.st, fx, sid);
                    if !r.audio.is_empty() {
                        if delete_wav {
                            let p = PathBuf::from(&r.audio);
                            let _ = std::thread::Builder::new().spawn(move || {
                                let _ = std::fs::remove_file(p);
                            });
                        } else {
                            c.st.temp_files.push(PathBuf::from(&r.audio));
                        }
                    }
                    if !delete_wav {
                        for o in r.outputs.iter().filter(|o| !o.is_empty()) {
                            c.st.temp_files.push(PathBuf::from(o));
                        }
                    }
                    if is_fg {
                        fx.all(&[SubtitleChange::Sentences, SubtitleChange::Selection, SubtitleChange::Status]);
                    }
                }
                EngineEvent::Translation { .. } => {}
                EngineEvent::Error { message } => {
                    c.st.session_mut(sid).expect("session").ctx.failed = true;
                    i.cancel_async_translation(&mut c.st, sid);
                    i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::verbatim(message) });
                }
            }
            true
        })
    }

    fn on_generate_end(self: &Rc<Self>, sid: SessionId, job_id: Uuid, outcome: Result<(), EngineError>) {
        self.edit(|i, c, fx| {
            let current = Self::is_current_job(&c.st, sid, job_id) || (c.st.session(sid).is_some_and(|s| s.job_id == job_id));
            match outcome {
                Err(EngineError::Cancelled) => {
                    if current {
                        i.set_status(&mut c.st, fx, sid, JobStatus::Idle);
                    }
                    i.job_did_end(c, fx, sid, job_id);
                }
                Err(e) => {
                    if current {
                        i.cancel_async_translation(&mut c.st, sid);
                        i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::error(&e) });
                    }
                    i.job_did_end(c, fx, sid, job_id);
                }
                Ok(()) => {
                    if !current {
                        i.job_did_end(c, fx, sid, job_id);
                        return;
                    }
                    let (failed, wants_async) = {
                        let s = c.st.session(sid).expect("session");
                        (s.ctx.failed, s.ctx.wants_async)
                    };
                    if failed {
                        i.cancel_async_translation(&mut c.st, sid);
                    }
                    if wants_async && !failed {
                        let s = c.st.session_mut(sid).expect("session");
                        s.asyncs.draining = true;
                        if s.asyncs.task_running {
                            s.asyncs.shown_in_status = true;
                            s.ctx.awaiting_async = true;
                            let f = s.async_fraction;
                            i.set_status(
                                &mut c.st,
                                fx,
                                sid,
                                JobStatus::Working { label: stage_label(EngineStage::Translating, f, JobPurpose::Translation), fraction: f },
                            );
                            return;
                        }
                    }
                    i.generate_continue(c, fx, sid, job_id);
                }
            }
        });
    }

    /// The part of the transcription end after the background pump drained.
    fn generate_continue(&self, c: &mut Core, fx: &mut Fx, sid: SessionId, job_id: Uuid) {
        let Some(s) = c.st.session_mut(sid) else { return };
        if s.job_id != job_id {
            return;
        }
        let ctx = s.ctx.clone();
        s.asyncs.shown_in_status = false;
        s.set_async_translating(false);
        let has_lines = !s.sentences.is_empty();
        if ctx.then_translate && !ctx.failed && has_lines {
            let settings = ctx.settings.clone().unwrap_or_else(|| c.st.job_settings());
            c.st.session_mut(sid).expect("session").job = None;
            self.release_models(&mut c.st, job_id);
            self.start_translation(c, fx, sid, ctx.wants_async, None, settings);
            if c.st.session(sid).is_some_and(|s| s.job.is_none()) {
                // Nothing launched: the lane moves on.
                let adv = c.st.queue.job_ended(sid);
                self.handle_advanced(c, fx, adv);
            }
            return;
        }
        if !ctx.failed {
            self.set_status(&mut c.st, fx, sid, JobStatus::Finished);
        }
        self.catalog.load();
        self.job_did_end(c, fx, sid, job_id);
    }

    /// `language_detected(session, settings)`.
    fn language_detected(&self, st: &mut State, fx: &mut Fx, sid: SessionId) {
        let Some(s) = st.session_mut(sid) else { return };
        if let Some(settings) = s.ctx.settings.as_mut() {
            let src = s.lines_source_language.clone().or_else(|| {
                if !settings.language.is_empty() { Some(settings.language.clone()) } else { s.detected_language.clone() }
            });
            settings.translation_target = super::reconciled_target(&settings.translation_target, src.as_deref());
            s.asyncs.settings = Some(settings.clone());
        }
        let updates_toolbar = st.is_foreground(sid) && st.session(sid).is_some_and(|s| s.batch_item_id.is_none());
        if updates_toolbar {
            self.reconcile_translation_target(st, fx);
        }
    }

    // ----- translation

    /// `prepare_translation(...)` (mac-viewmodels §5.6).
    pub(crate) fn prepare_translation(
        &self,
        c: &mut Core,
        fx: &mut Fx,
        sid: SessionId,
        reuse: bool,
        only: Option<HashSet<Uuid>>,
        settings: &JobSettings,
    ) -> Option<TranslationPlan> {
        let s = c.st.session(sid)?;
        if s.sentences.is_empty() {
            return None;
        }
        let selected_request: Option<Vec<Sentence>> =
            only.as_ref().map(|w| s.sentences.iter().filter(|x| w.contains(&x.uid)).cloned().collect());
        if selected_request.as_ref().is_some_and(|r| r.is_empty()) {
            return None;
        }
        if self.svc.engine.is_none() {
            self.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::error(&EngineError::NotFound) });
            return None;
        }
        if c.st.is_foreground(sid) {
            self.finish_editing(c, fx);
        }
        let s = c.st.session(sid)?;
        let source = s.translation_source(&settings.language);
        let target = settings.translation_target.clone();
        if source.as_deref() == Some(target.as_str()) {
            self.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::key("Kaynak ve hedef dil aynı olamaz.") });
            return None;
        }
        if settings.translation_model == "nllb-1.3b" && source.is_none() && !c.st.is_external_llm_active() {
            self.set_status(
                &mut c.st,
                fx,
                sid,
                JobStatus::Failed { message: LocalizedMessage::key("NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç.") },
            );
            return None;
        }
        let retry = OfflineRetry::Request { kind: JobKind::Translate { reuse_existing: reuse, only: only.clone() }, settings: settings.clone() };
        if self.blocked_offline(&mut c.st, fx, sid, &[settings.translation_model.clone()], &settings.translation_model, retry) {
            return None;
        }
        let s = c.st.session(sid)?;
        let selected = selected_request.is_some();
        let request: Vec<Sentence> = match selected_request {
            Some(r) => r,
            None if reuse => s
                .sentences
                .iter()
                .filter(|x| !s.translations.get(&x.uid).is_some_and(|e| is_reusable_translation(x, e, &target)))
                .cloned()
                .collect(),
            None => s.sentences.clone(),
        };
        let contextual = with_translation_context(&s.sentences, &request);
        let already_done = if reuse && !selected {
            s.sentences.iter().filter(|x| s.translations.get(&x.uid).is_some_and(|e| is_reusable_translation(x, e, &target))).count()
        } else {
            0
        };
        let total = if selected { request.len().max(1) } else { s.sentences.len().max(1) };
        Some(TranslationPlan {
            request,
            contextual,
            already_done,
            total,
            baseline: s.translations.clone(),
            source,
            target,
            model: settings.translation_model.clone(),
            selected,
            reuse,
        })
    }

    /// `start_translation(...)`.
    pub(crate) fn start_translation(
        &self,
        c: &mut Core,
        fx: &mut Fx,
        sid: SessionId,
        reuse: bool,
        only: Option<HashSet<Uuid>>,
        settings: JobSettings,
    ) {
        let Some(plan) = self.prepare_translation(c, fx, sid, reuse, only, &settings) else { return };
        let Some(engine) = self.svc.engine.clone() else { return };
        self.cancel_async_translation(&mut c.st, sid);
        let external = c.st.is_external_llm_active();
        let token = CancellationToken::new();
        let job_id = Uuid::new_v4();
        {
            let s = c.st.session_mut(sid).expect("session");
            s.set_saved_subtitle_name(None);
            if let Some(t) = s.job.take() {
                t.cancel();
            }
            s.job_id = job_id;
            s.job = Some(token.clone());
            s.ctx = JobContext { settings: Some(settings.clone()), ..JobContext::default() };
        }
        if c.st.is_foreground(sid) {
            fx.ch(SubtitleChange::Status);
        }
        let claims: HashSet<String> = if external { HashSet::new() } else { [plan.model.clone()].into_iter().collect() };
        self.claim_models(&mut c.st, job_id, claims);
        self.svc.diagnostics.write("Job", &format!("StartJob jobId={job_id} translate to={} model={}", plan.target, plan.model));

        if plan.request.is_empty() {
            self.set_status(&mut c.st, fx, sid, JobStatus::Working { label: LocalizedMessage::key("Çeviri hazırlanıyor…"), fraction: None });
            c.st.session_mut(sid).expect("session").is_translation_job_running = true;
            fx.then(move |inner| {
                inner.edit(|i, c, fx| {
                    if !Self::is_current_job(&c.st, sid, job_id) {
                        return;
                    }
                    i.set_status(&mut c.st, fx, sid, JobStatus::Finished);
                    i.auto_save_translation_if_enabled(c, fx, sid, Some(&settings));
                    i.job_did_end(c, fx, sid, job_id);
                });
            });
            return;
        }
        let initial = translation_progress(plan.already_done, plan.total);
        let status = match initial {
            Some(f) => JobStatus::Working { label: stage_label(EngineStage::Translating, Some(f), JobPurpose::Translation), fraction: Some(f) },
            None => JobStatus::Working { label: LocalizedMessage::key("Çeviri hazırlanıyor…"), fraction: None },
        };
        self.set_status(&mut c.st, fx, sid, status);
        c.st.session_mut(sid).expect("session").is_translation_job_running = true;

        let provider = c.st.provider.active.clone().filter(|_| external);
        let providers = self.svc.providers.clone();
        let launch = self.svc.launch_state.clone();
        let gate = self.gate.clone();
        let key = self.key();
        let ui = self.svc.ui.clone();
        let plan_w = plan.clone();
        let _ = std::thread::Builder::new().name("sa-translate".into()).spawn(move || {
            let plan = plan_w;
            let Some(permit) = gate.acquire(&token) else {
                key.post(&ui, move |inner| inner.on_translation_end(sid, job_id, Err(EngineError::Cancelled), None));
                return;
            };
            // Re-read the key off the UI thread (mac-viewmodels §6.3 prepare_external_launch).
            if let Some(p) = &provider {
                if matches!(providers.read_key(p.id), ProviderKeyRead::Unavailable) {
                    drop(permit);
                    let msg = key_unreadable_message(&p.name);
                    key.post(&ui, move |inner| inner.on_translation_end(sid, job_id, Ok(()), Some(msg)));
                    return;
                }
                // The launch carries the key as stored right now (an edit since the request is honored).
                if providers.active_provider_id() == Some(p.id) {
                    launch.set_external_llm(providers.active_external_llm().map(|a| a.llm));
                }
            }
            let stream = engine.translate(&plan.contextual, &plan.target, plan.source.as_deref(), &plan.model, &token);
            let mut outcome: Result<(), EngineError> = Ok(());
            let mut first = true;
            for item in stream {
                if token.is_cancelled() {
                    break;
                }
                match item {
                    Ok(ev) => {
                        let p = plan.clone();
                        let is_first = first;
                        if matches!(ev, EngineEvent::Translation { .. }) {
                            first = false;
                        }
                        let keep = key.invoke(&ui, move |inner| inner.on_translation_event(sid, job_id, ev, &p, is_first)).unwrap_or(false);
                        if !keep {
                            drop(permit);
                            return;
                        }
                    }
                    Err(e) => {
                        outcome = Err(e);
                        break;
                    }
                }
            }
            drop(permit);
            if token.is_cancelled() {
                outcome = Err(EngineError::Cancelled);
            }
            key.post(&ui, move |inner| inner.on_translation_end(sid, job_id, outcome, None));
        });
    }

    fn on_translation_event(self: &Rc<Self>, sid: SessionId, job_id: Uuid, ev: EngineEvent, plan: &TranslationPlan, first: bool) -> bool {
        self.edit(|i, c, fx| {
            if !Self::is_current_job(&c.st, sid, job_id) {
                return false;
            }
            match ev {
                EngineEvent::Stage { stage, fraction } => {
                    let f = match (stage, fraction) {
                        (EngineStage::Translating, Some(f)) => {
                            Some(translation_progress_resumed(plan.already_done, plan.request.len(), f, plan.total))
                        }
                        _ => fraction,
                    };
                    i.set_status(&mut c.st, fx, sid, JobStatus::Working { label: stage_label(stage, f, JobPurpose::Translation), fraction: f });
                }
                EngineEvent::Translation { language, sentences } => {
                    if plan.selected && first && c.st.is_foreground(sid) && c.st.policy.selected_retry_undo_group {
                        i.register_translation_undo(c, plan.baseline.clone(), sa_core::k("Seçilenleri Yeniden Çevir"));
                    }
                    let p = pairs(&sentences, &plan.request);
                    let s = c.st.session_mut(sid).expect("session");
                    s.store_translations(&p, &language, plan.reuse || plan.selected, plan.selected, Some(&plan.baseline));
                    let updates_toolbar = c.st.is_foreground(sid) && c.st.session(sid).is_some_and(|s| s.batch_item_id.is_none());
                    if updates_toolbar {
                        i.set_target(&mut c.st, &language);
                        fx.all(&[SubtitleChange::Translations, SubtitleChange::Settings]);
                    }
                    fx.ch(SubtitleChange::Playlist);
                }
                EngineEvent::Error { message } => {
                    c.st.session_mut(sid).expect("session").ctx.failed = true;
                    i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::verbatim(message) });
                }
                _ => {}
            }
            true
        })
    }

    fn on_translation_end(self: &Rc<Self>, sid: SessionId, job_id: Uuid, outcome: Result<(), EngineError>, problem: Option<LocalizedMessage>) {
        self.edit(|i, c, fx| {
            let current = c.st.session(sid).is_some_and(|s| s.job_id == job_id);
            if !current {
                i.job_did_end(c, fx, sid, job_id);
                return;
            }
            if let Some(p) = problem {
                i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: p });
                i.job_did_end(c, fx, sid, job_id);
                return;
            }
            match outcome {
                Err(EngineError::Cancelled) => i.set_status(&mut c.st, fx, sid, JobStatus::Idle),
                Err(e) => i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::error(&e) }),
                Ok(()) => {
                    let failed = c.st.session(sid).is_some_and(|s| s.ctx.failed);
                    if !failed {
                        i.set_status(&mut c.st, fx, sid, JobStatus::Finished);
                        let settings = c.st.session(sid).and_then(|s| s.ctx.settings.clone());
                        i.auto_save_translation_if_enabled(c, fx, sid, settings.as_ref());
                    }
                    i.catalog.load();
                }
            }
            i.job_did_end(c, fx, sid, job_id);
        });
    }

    // ----- background pre-translation (external LLM "Asenkron")

    pub(crate) fn cancel_async_translation(&self, st: &mut State, sid: SessionId) {
        let Some(s) = st.session_mut(sid) else { return };
        if let Some(t) = s.asyncs.cancel.take() {
            t.cancel();
        }
        s.asyncs.task_running = false;
        s.asyncs.run_id = Uuid::new_v4();
        s.asyncs.queued.clear();
        s.asyncs.draining = false;
        s.asyncs.transcription_complete = false;
        s.asyncs.failed = false;
        s.asyncs.shown_in_status = false;
        s.set_async_translating(false);
    }

    fn update_async_fraction(&self, st: &mut State, fx: &mut Fx, sid: SessionId) {
        let Some(s) = st.session(sid) else { return };
        if s.is_async_translating {
            let f = translation_progress(s.translations.len(), s.sentences.len());
            self.set_async_fraction(st, fx, sid, f);
        }
    }

    fn set_async_fraction(&self, st: &mut State, fx: &mut Fx, sid: SessionId, f: Option<f64>) {
        let Some(s) = st.session_mut(sid) else { return };
        s.async_fraction = f;
        let mirror = s.asyncs.shown_in_status && s.status.is_working();
        if mirror {
            self.set_status(st, fx, sid, JobStatus::Working { label: stage_label(EngineStage::Translating, f, JobPurpose::Translation), fraction: f });
        }
        if st.is_foreground(sid) {
            fx.ch(SubtitleChange::Status);
        }
    }

    /// Starts the pump when nothing else translates and a batch is ready.
    pub(crate) fn start_async_translation(&self, st: &mut State, fx: &mut Fx, sid: SessionId) {
        self.update_async_fraction(st, fx, sid);
        let Some(engine) = self.svc.engine.clone() else { return };
        let s = st.session_mut(sid).expect("session");
        if s.asyncs.task_running || s.asyncs.draining || s.asyncs.failed || self.gate.is_busy() || s.next_async_batch().is_empty() {
            return;
        }
        let run_id = Uuid::new_v4();
        let token = CancellationToken::new();
        s.asyncs.run_id = run_id;
        s.asyncs.task_running = true;
        s.asyncs.cancel = Some(token.clone());
        let key = self.key();
        let ui = self.svc.ui.clone();
        let _ = std::thread::Builder::new().name("sa-async-translate".into()).spawn(move || {
            loop {
                if token.is_cancelled() {
                    break;
                }
                let step = key.invoke(&ui, move |inner| inner.async_next_step(sid, run_id)).flatten();
                let Some(step) = step else { break };
                let AsyncStep { permit, request, batch, target, source, model, already, total } = step;
                let stream = engine.translate(&request, &target, source.as_deref(), &model, &token);
                let mut ok = true;
                let mut failure: Option<String> = None;
                for item in stream {
                    if token.is_cancelled() {
                        ok = false;
                        break;
                    }
                    match item {
                        Ok(EngineEvent::Error { message }) => {
                            failure = Some(message);
                            ok = false;
                        }
                        Ok(ev) => {
                            let b = batch.clone();
                            let alive = key.invoke(&ui, move |inner| inner.async_event(sid, run_id, ev, &b, already, total)).unwrap_or(false);
                            if !alive {
                                ok = false;
                                break;
                            }
                        }
                        Err(e) => {
                            if !token.is_cancelled() {
                                failure = Some(e.to_string());
                            }
                            ok = false;
                            break;
                        }
                    }
                }
                drop(permit);
                if !ok {
                    let n = batch.len();
                    key.post(&ui, move |inner| inner.async_failed(sid, run_id, n, failure));
                    break;
                }
            }
            key.post(&ui, move |inner| inner.async_pump_ended(sid, run_id));
        });
    }

    fn async_next_step(self: &Rc<Self>, sid: SessionId, run_id: Uuid) -> Option<AsyncStep> {
        self.edit(|i, c, fx| {
            let s = c.st.session(sid)?;
            if s.asyncs.run_id != run_id {
                return None;
            }
            let permit = i.gate.try_acquire()?;
            let batch = s.next_async_batch();
            let settings = s.asyncs.settings.clone().unwrap_or_else(|| c.st.job_settings());
            let target = settings.translation_target.clone();
            let source = s.translation_source(&settings.language);
            if batch.is_empty() || source.as_deref() == Some(target.as_str()) {
                return None;
            }
            let request = with_translation_context(&s.sentences, &batch);
            let already = s.translations.len();
            let total = s.sentences.len().max(already + batch.len());
            let s = c.st.session_mut(sid).expect("session");
            for b in &batch {
                s.asyncs.queued.insert(b.uid);
            }
            s.is_async_translating = true;
            i.update_async_fraction(&mut c.st, fx, sid);
            if c.st.is_foreground(sid) {
                fx.ch(SubtitleChange::Status);
            }
            Some(AsyncStep { permit, request, batch, target, source, model: settings.translation_model.clone(), already, total })
        })
    }

    fn async_event(self: &Rc<Self>, sid: SessionId, run_id: Uuid, ev: EngineEvent, batch: &[Sentence], already: usize, total: usize) -> bool {
        self.edit(|i, c, fx| {
            let Some(s) = c.st.session(sid) else { return false };
            if s.asyncs.run_id != run_id {
                return false;
            }
            match ev {
                EngineEvent::Translation { language, sentences } => {
                    let s = c.st.session_mut(sid).expect("session");
                    let mut accepted = Vec::new();
                    for (r, o) in pairs(&sentences, batch) {
                        let same = s.sentences.iter().any(|x| x.uid == o.uid && x.text == o.text);
                        if same {
                            accepted.push((r, o));
                        } else {
                            s.asyncs.queued.remove(&o.uid);
                        }
                    }
                    if !accepted.is_empty() {
                        s.store_translations(&accepted, &language, true, false, None);
                    }
                    let updates_toolbar = c.st.is_foreground(sid) && c.st.session(sid).is_some_and(|s| s.batch_item_id.is_none());
                    if updates_toolbar {
                        i.set_target(&mut c.st, &language);
                        fx.all(&[SubtitleChange::Translations, SubtitleChange::Settings]);
                    }
                    i.update_async_fraction(&mut c.st, fx, sid);
                }
                EngineEvent::Stage { stage: EngineStage::Translating, fraction: Some(part) } => {
                    if c.st.session(sid).is_some_and(|s| s.is_async_translating) {
                        let f = translation_progress_resumed(already, batch.len(), part, total);
                        i.set_async_fraction(&mut c.st, fx, sid, Some(f));
                    }
                }
                _ => {}
            }
            true
        })
    }

    fn async_failed(self: &Rc<Self>, sid: SessionId, run_id: Uuid, lines: usize, failure: Option<String>) {
        self.edit(|i, c, fx| {
            let Some(s) = c.st.session_mut(sid) else { return };
            if s.asyncs.run_id != run_id {
                return;
            }
            if let Some(m) = failure {
                i.svc.diagnostics.write("translation", &format!("background batch of {lines} lines failed: {m}"));
            }
            s.asyncs.failed = true;
            s.asyncs.shown_in_status = false;
            s.set_async_translating(false);
            fx.ch(SubtitleChange::Status);
        });
    }

    fn async_pump_ended(self: &Rc<Self>, sid: SessionId, run_id: Uuid) {
        self.edit(|i, c, fx| {
            let Some(s) = c.st.session_mut(sid) else { return };
            if s.asyncs.run_id != run_id {
                return;
            }
            s.asyncs.task_running = false;
            s.asyncs.cancel = None;
            if s.ctx.awaiting_async {
                s.ctx.awaiting_async = false;
                let job_id = s.job_id;
                i.generate_continue(c, fx, sid, job_id);
            } else if !s.asyncs.draining && !s.asyncs.failed && !s.next_async_batch().is_empty() {
                // More lines became stable while the batch ran.
                i.start_async_translation(&mut c.st, fx, sid);
            }
        });
    }
}

/// One background batch handed to the pump worker.
pub(crate) struct AsyncStep {
    permit: crate::jobs::TranslationPermit,
    request: Vec<Sentence>,
    batch: Vec<Sentence>,
    target: String,
    source: Option<String>,
    model: String,
    already: usize,
    total: usize,
}

// SAFETY-free: every field is Send.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<AsyncStep>();
};
