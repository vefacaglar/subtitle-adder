//! Sessions, opening, subtitle import, routing of dropped/external files, rendering and saving
//! (win-viewmodels §8, §16; mac-viewmodels §5.9, §5.12, §5.13).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use sa_core::cancel::CancellationToken;
use sa_core::engine_types::{EngineError, Sentence, SubtitleFormat};
use sa_core::l;
use sa_core::l10n::LocalizedMessage;
use sa_core::subtitle_import;
use uuid::Uuid;

use super::session::{Session, SessionId};
use super::{Core, Fx, Inner, PendingOpen, State};
use crate::policy::SingleJobsAsListRows;
use crate::types::{ExportLanguage, JobSettings, JobStatus, SubtitleChange, Workspace};

/// What a confirmed open performs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OpenAction {
    Route { video: Option<PathBuf>, subtitle: Option<PathBuf>, finding_video: bool, switch_editor: bool },
    Video(Option<PathBuf>),
    Subtitle(PathBuf),
}

pub(crate) fn is_subtitle_path(p: &Path) -> bool {
    subtitle_import::is_subtitle_file(p)
}

pub(crate) fn find_companion_video(subtitle: &Path) -> Option<PathBuf> {
    subtitle_import::find_companion_video(subtitle)
}

/// The companion video under this platform's rules (Windows probing / macOS folder listing).
pub(crate) fn find_companion_video_with(flavor: subtitle_import::ImportFlavor, subtitle: &Path) -> Option<PathBuf> {
    match flavor {
        subtitle_import::ImportFlavor::Windows => subtitle_import::find_companion_video_windows(subtitle),
        subtitle_import::ImportFlavor::Mac => subtitle_import::find_companion_video_listing(subtitle),
    }
}

/// `!lines.empty && !working && (video || loaded subtitle)`.
pub(crate) fn can_save(s: &Session) -> bool {
    !s.sentences.is_empty() && !s.is_working() && s.base_path().is_some()
}

/// `Failed(LocalizedMessage("save-failed:<reason>", L("Dosya kaydedilemedi: %@", reason)))` message.
pub(crate) fn save_failure(reason: &str) -> LocalizedMessage {
    let r = reason.to_owned();
    LocalizedMessage::new(format!("save-failed:{reason}"), move || l!("Dosya kaydedilemedi: %@", r.as_str()))
}

/// Atomic UTF-8 (no BOM) write: temp file in the same folder, then rename.
pub(crate) fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "subtitle".into());
    let tmp = dir.join(format!(".{name}.{}.tmp", Uuid::new_v4().simple()));
    std::fs::write(&tmp, text.as_bytes())?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Windows: rename over an existing file can fail when it is open elsewhere; fall back to a direct write.
            let _ = std::fs::remove_file(&tmp);
            std::fs::write(path, text.as_bytes()).map_err(|_| e)
        }
    }
}

fn batch_busy_message() -> LocalizedMessage {
    LocalizedMessage::key("Bu video toplu listede işleniyor. Liste bitince ya da durdurunca altyazı açabilirsin.")
}

impl Inner {
    // ----- sessions

    fn is_reconstructible(s: &Session) -> bool {
        if s.job.is_some() || s.is_working() || s.has_unsaved_edits || matches!(s.status, JobStatus::Failed { .. }) {
            return false;
        }
        if s.saved_by_run {
            return true;
        }
        match (&s.video, &s.loaded_subtitle) {
            (Some(v), Some(l)) if s.translations.is_empty() => subtitle_import::find_sidecar_subtitle(v).as_deref() == Some(l.as_path()),
            _ => false,
        }
    }

    pub(crate) fn is_kept(st: &State, sid: SessionId) -> bool {
        let Some(s) = st.session(sid) else { return false };
        st.recent_jobs.contains(&sid)
            || s.batch_item_id.is_some()
            || s.is_playlist_translating
            || st.queue.is_queued(sid)
            || st.queue.is_running(sid)
            || (s.has_content() && !Self::is_reconstructible(s))
    }

    fn unregister(st: &mut State, sid: SessionId) {
        if let Some(v) = st.session(sid).and_then(|s| s.video.clone()) {
            let k = st.key(&v);
            if st.by_key.get(&k) == Some(&sid) {
                st.by_key.remove(&k);
            }
        }
        if !st.is_foreground(sid) && !st.recent_jobs.contains(&sid) {
            st.sessions.remove(&sid);
        }
    }

    /// End of a playlist run: drop every background session that is not kept (macOS).
    pub(crate) fn prune_sessions(&self, st: &mut State) {
        if !st.policy.evict_sessions {
            return;
        }
        let ids: Vec<SessionId> = st.sessions.keys().copied().filter(|s| !st.is_foreground(*s)).collect();
        for sid in ids {
            if !Self::is_kept(st, sid) {
                Self::unregister(st, sid);
            }
        }
    }

    /// The session of a video: existing, or created (and its sidecar loaded in the background).
    pub(crate) fn session_for(&self, st: &mut State, fx: &mut Fx, video: &Path) -> SessionId {
        if let Some(sid) = st.existing_session(video) {
            return sid;
        }
        let sid = st.new_session(Some(video.to_path_buf()));
        let k = st.key(video);
        st.by_key.insert(k, sid);
        self.load_sidecar_in_background(st, fx, sid, video.to_path_buf());
        sid
    }

    fn load_sidecar_in_background(&self, st: &State, _fx: &mut Fx, sid: SessionId, video: PathBuf) {
        let key = self.key();
        let ui = self.svc.ui.clone();
        let fail_visible = st.policy.single_jobs_as_list_rows == SingleJobsAsListRows::RecentJobs;
        let flavor = st.policy.import_flavor;
        let _ = std::thread::Builder::new().name("sa-sidecar".into()).spawn(move || {
            let Some(sidecar) = subtitle_import::find_sidecar_subtitle(&video) else { return };
            let parsed = subtitle_import::parse_file_with(&sidecar, flavor);
            key.post(&ui, move |inner| {
                inner.edit(|i, c, fx| {
                    let Some(s) = c.st.session_mut(sid) else { return };
                    if !s.sentences.is_empty() || s.job.is_some() || s.status != JobStatus::Idle {
                        return;
                    }
                    match parsed {
                        Ok(lines) if !lines.is_empty() => {
                            s.load(sidecar, lines);
                            if c.st.is_foreground(sid) {
                                fx.all(&[SubtitleChange::Sentences, SubtitleChange::Translations, SubtitleChange::Status]);
                            }
                        }
                        Ok(_) => {}
                        Err(e) if fail_visible => {
                            let m = LocalizedMessage::error(&EngineError::Failed(e.to_string()));
                            i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: m });
                        }
                        Err(_) => {}
                    }
                });
            });
        });
    }

    /// `leave_foreground()`: commit drafts, clear undo, end or evict the old session.
    fn leave_foreground(&self, c: &mut Core, fx: &mut Fx) {
        self.finish_editing(c, fx);
        c.undo.clear();
        let old = c.st.foreground;
        let win = c.st.policy.single_jobs_as_list_rows == SingleJobsAsListRows::PlaylistRows;
        let has_video = c.st.fg().video.is_some();
        if !has_video && !win {
            self.stop_job(c, fx, old, true);
            c.st.sessions.remove(&old);
            c.st.foreground = 0;
        } else if has_video && c.st.policy.evict_sessions && !Self::is_kept(&c.st, old) {
            c.st.foreground = 0;
            Self::unregister(&mut c.st, old);
        }
    }

    /// Opens a video (or none).
    pub(crate) fn open_video(&self, c: &mut Core, fx: &mut Fx, path: Option<PathBuf>) {
        let current = c.st.fg().video.clone();
        let same = match (&current, &path) {
            (None, None) => true,
            (Some(a), Some(b)) if c.st.policy.case_insensitive_paths => a == b,
            (Some(a), Some(b)) => c.st.key(a) == c.st.key(b),
            _ => false,
        };
        if same {
            return;
        }
        let display = c.st.fg().display_mode;
        self.leave_foreground(c, fx);
        let next = match &path {
            Some(p) => {
                let existed = c.st.existing_session(p);
                let sid = self.session_for(&mut c.st, fx, p);
                // Windows `OpenVideo`: a session without lines, translations, status or loaded file is reset and its
                // sidecar looked up again (a subtitle may have appeared next to the video since). macOS evicts such
                // sessions, so its next open creates (and loads) a fresh one anyway.
                if existed.is_some() && !c.st.policy.evict_sessions && c.st.session(sid).is_some_and(Session::is_blank) {
                    if let Some(s) = c.st.session_mut(sid) {
                        s.detected_language = None;
                        s.saved_subtitle_name = None;
                        s.async_fraction = None;
                        s.export_language = ExportLanguage::Original;
                    }
                    self.load_sidecar_in_background(&c.st, fx, sid, p.clone());
                }
                sid
            }
            None => {
                let win = c.st.policy.single_jobs_as_list_rows == SingleJobsAsListRows::PlaylistRows;
                if win && let Some(sid) = c.st.by_key.get("").copied() {
                    sid
                } else {
                    let sid = c.st.new_session(None);
                    if win {
                        c.st.by_key.insert(String::new(), sid);
                    }
                    sid
                }
            }
        };
        if let Some(s) = c.st.session_mut(next)
            && s.sentences.is_empty()
            && !s.has_translations()
            && s.status == JobStatus::Idle
        {
            s.display_mode = display;
        }
        c.st.foreground = next;
        fx.all(&[
            SubtitleChange::Video,
            SubtitleChange::Sentences,
            SubtitleChange::Translations,
            SubtitleChange::Selection,
            SubtitleChange::Drafts,
            SubtitleChange::Status,
            SubtitleChange::Undo,
            SubtitleChange::Settings,
        ]);
    }

    /// Imports a subtitle into `sid` (parsing happens here: the picker or a drop asked for it).
    pub(crate) fn load_subtitle_file(&self, c: &mut Core, fx: &mut Fx, path: &Path, sid: SessionId) -> bool {
        if c.st.session(sid).is_some_and(|s| s.batch_item_id.is_some()) {
            c.st.open_refusal = Some(batch_busy_message());
            fx.ch(SubtitleChange::Notice);
            return false;
        }
        match subtitle_import::parse_file_with(path, c.st.policy.import_flavor) {
            Err(e) => {
                let m = LocalizedMessage::error(&EngineError::Failed(e.to_string()));
                self.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: m });
                false
            }
            Ok(lines) if lines.is_empty() => {
                self.set_status(
                    &mut c.st,
                    fx,
                    sid,
                    JobStatus::Failed { message: LocalizedMessage::key("Altyazı dosyasında geçerli satır bulunamadı.") },
                );
                false
            }
            Ok(lines) => {
                self.apply_lines(c, fx, sid, path.to_path_buf(), lines);
                true
            }
        }
    }

    pub(crate) fn apply_lines(&self, c: &mut Core, fx: &mut Fx, sid: SessionId, path: PathBuf, lines: Vec<Sentence>) {
        if c.st.session(sid).is_some_and(|s| s.job.is_some() || s.status.is_working()) {
            self.stop_job(c, fx, sid, true);
        }
        if c.st.is_foreground(sid) {
            c.undo.clear();
        }
        if let Some(s) = c.st.session_mut(sid) {
            s.load(path, lines);
        }
        self.set_status(&mut c.st, fx, sid, JobStatus::Finished);
        fx.all(&[SubtitleChange::Sentences, SubtitleChange::Translations, SubtitleChange::Selection, SubtitleChange::Undo]);
    }

    // ----- routing

    fn receiver_of(st: &State, video: Option<&Path>) -> Option<SessionId> {
        let v = video?;
        if st.fg().video.as_ref().is_some_and(|f| st.key(f) == st.key(v)) {
            Some(st.foreground)
        } else {
            st.existing_session(v)
        }
    }

    fn subtitle_receiver(st: &State, video: Option<&Path>, subtitle: Option<&Path>, finding_video: bool) -> Option<SessionId> {
        let subtitle = subtitle?;
        if video.is_some() {
            return Self::receiver_of(st, video);
        }
        if !finding_video {
            return Some(st.foreground);
        }
        if let Some(c) = find_companion_video_with(st.policy.import_flavor, subtitle) {
            return Self::receiver_of(st, Some(&c));
        }
        if st.fg().video.is_none() { Some(st.foreground) } else { None }
    }

    fn sessions_lost(st: &State, video: Option<&Path>, subtitle: Option<&Path>, finding_video: Option<bool>, restarting: bool) -> Vec<SessionId> {
        let mut out: Vec<SessionId> = Vec::new();
        let mut add = |s: SessionId| {
            if !out.contains(&s) {
                out.push(s);
            }
        };
        let fg_has_video = st.fg().video.is_some();
        let finding = finding_video.unwrap_or(!fg_has_video);
        if let Some(v) = video
            && Self::receiver_of(st, Some(v)) != Some(st.foreground)
            && !fg_has_video
        {
            add(st.foreground);
        }
        if video.is_none()
            && let Some(sub) = subtitle
            && finding
            && let Some(comp) = find_companion_video_with(st.policy.import_flavor, sub)
            && Self::receiver_of(st, Some(&comp)) != Some(st.foreground)
            && !fg_has_video
        {
            add(st.foreground);
        }
        if let Some(r) = Self::subtitle_receiver(st, video, subtitle, finding) {
            add(r);
        }
        if restarting && let Some(r) = Self::receiver_of(st, video) {
            add(r);
        }
        out
    }

    fn at_stake(st: &State, sessions: &[SessionId]) -> Option<(bool, bool)> {
        let stops = sessions.iter().filter_map(|s| st.session(*s)).any(|s| s.is_working());
        let loses = sessions.iter().filter_map(|s| st.session(*s)).any(|s| s.has_unsaved_edits);
        (stops || loses).then_some((stops, loses))
    }

    fn refuses_subtitle(&self, st: &mut State, fx: &mut Fx, video: Option<&Path>, subtitle: Option<&Path>, finding: bool) -> bool {
        if let Some(r) = Self::subtitle_receiver(st, video, subtitle, finding)
            && st.session(r).is_some_and(|s| s.batch_item_id.is_some())
        {
            st.open_refusal = Some(batch_busy_message());
            fx.ch(SubtitleChange::Notice);
            return true;
        }
        false
    }

    fn request_open(&self, c: &mut Core, fx: &mut Fx, losing: Vec<SessionId>, action: OpenAction) {
        if c.st.policy.confirm_open_when_losing_work
            && let Some((stops_job, loses_edits)) = Self::at_stake(&c.st, &losing)
        {
            c.st.pending_open = Some(PendingOpen { stops_job, loses_edits, action });
            fx.ch(SubtitleChange::Notice);
            return;
        }
        c.st.pending_open = None;
        self.perform_open(c, fx, action);
    }

    pub(crate) fn confirm_pending_open(&self, c: &mut Core, fx: &mut Fx) {
        if let Some(p) = c.st.pending_open.take() {
            fx.ch(SubtitleChange::Notice);
            self.perform_open(c, fx, p.action);
        }
    }

    fn perform_open(&self, c: &mut Core, fx: &mut Fx, action: OpenAction) {
        match action {
            OpenAction::Route { video, subtitle, finding_video, switch_editor } => {
                if switch_editor {
                    c.st.workspace = Workspace::Editor;
                    fx.ch(SubtitleChange::Workspace);
                }
                if video.is_some() {
                    self.open_video(c, fx, video);
                }
                if let Some(s) = subtitle {
                    self.open_subtitle(c, fx, &s, finding_video);
                }
            }
            OpenAction::Video(v) => self.open_video(c, fx, v),
            OpenAction::Subtitle(s) => {
                let finding = c.st.fg().video.is_none();
                self.open_subtitle(c, fx, &s, finding);
            }
        }
    }

    fn open_subtitle(&self, c: &mut Core, fx: &mut Fx, path: &Path, finding_video: bool) -> bool {
        if finding_video {
            match find_companion_video_with(c.st.policy.import_flavor, path) {
                Some(comp) => self.open_video(c, fx, Some(comp)),
                None if c.st.fg().video.is_some() => self.open_video(c, fx, None),
                None => {}
            }
        }
        let fg = c.st.foreground;
        self.load_subtitle_file(c, fx, path, fg)
    }

    pub(crate) fn request_open_video(&self, c: &mut Core, fx: &mut Fx, path: PathBuf) {
        let losing = Self::sessions_lost(&c.st, Some(&path), None, None, false);
        self.request_open(c, fx, losing, OpenAction::Video(Some(path)));
    }

    pub(crate) fn request_open_subtitle(&self, c: &mut Core, fx: &mut Fx, path: PathBuf) {
        let finding = c.st.fg().video.is_none();
        if self.refuses_subtitle(&mut c.st, fx, None, Some(&path), finding) {
            return;
        }
        let losing = Self::sessions_lost(&c.st, None, Some(&path), Some(finding), false);
        self.request_open(c, fx, losing, OpenAction::Subtitle(path));
    }

    /// `route_files(urls, external)`.
    pub(crate) fn route_files(&self, c: &mut Core, fx: &mut Fx, paths: Vec<PathBuf>, external: bool) {
        let files: Vec<PathBuf> = paths.into_iter().filter(|p| !p.is_dir()).collect();
        let subtitle = files.iter().find(|p| is_subtitle_path(p)).cloned();
        let videos: Vec<PathBuf> = files.iter().filter(|p| !is_subtitle_path(p)).cloned().collect();
        if videos.len() > 1 || (!videos.is_empty() && c.st.workspace == Workspace::Playlist && !external) {
            self.add_to_playlist(c, fx, videos, None);
            c.st.workspace = Workspace::Playlist;
            fx.ch(SubtitleChange::Workspace);
            return;
        }
        let first = videos.into_iter().next();
        let finding_video = first.is_none() && (external || c.st.fg().video.is_none());
        if self.refuses_subtitle(&mut c.st, fx, first.as_deref(), subtitle.as_deref(), finding_video) {
            return;
        }
        let losing = Self::sessions_lost(&c.st, first.as_deref(), subtitle.as_deref(), Some(finding_video), false);
        let switch_editor = external || c.st.workspace == Workspace::Platform || first.is_some();
        self.request_open(c, fx, losing, OpenAction::Route { video: first, subtitle, finding_video, switch_editor });
    }

    // ----- rendering and saving

    fn spawn_render(
        &self,
        sentences: Vec<Sentence>,
        format: SubtitleFormat,
        token: CancellationToken,
        then: impl FnOnce(&Rc<Inner>, Result<String, EngineError>) + Send + 'static,
    ) {
        let Some(engine) = self.svc.engine.clone() else {
            let key = self.key();
            key.post(&self.svc.ui, move |inner| then(inner, Err(EngineError::NotFound)));
            return;
        };
        let key = self.key();
        let ui = self.svc.ui.clone();
        let _ = std::thread::Builder::new().name("sa-render".into()).spawn(move || {
            let r = engine.render(&sentences, format, &token);
            let r = if token.is_cancelled() { Err(EngineError::Cancelled) } else { r };
            key.post(&ui, move |inner| then(inner, r));
        });
    }

    /// `save_subtitle_of(session, translated, format)`: renders, writes `<stem>.<fmt>` next to the video atomically
    /// off the UI thread.
    pub(crate) fn save_subtitle_of(&self, c: &mut Core, fx: &mut Fx, sid: SessionId, translated: Option<bool>, format: SubtitleFormat) {
        let Some(s) = c.st.session(sid) else { return };
        let use_translation = translated.unwrap_or(s.export_language == ExportLanguage::Translated) && s.has_translations();
        let Some(base) = s.base_path().map(Path::to_path_buf) else { return };
        let target = base.with_extension(format.raw());
        let loaded_before = s.loaded_subtitle.clone();
        let video = s.video.clone();
        let list = s.export_sentences(use_translation);
        if self.svc.engine.is_none() {
            self.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::error(&EngineError::NotFound) });
            return;
        }
        let token = s.job.clone().unwrap_or_default();
        let job_id = s.job_id;
        let target_w = target.clone();
        self.spawn_render(list, format, token.clone(), move |inner, r| {
            match r {
                Err(EngineError::Cancelled) => {}
                Err(e) => inner.edit(|i, c, fx| {
                    if c.st.session(sid).is_some_and(|s| s.job_id == job_id) && !token.is_cancelled() {
                        i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::error(&e) });
                    }
                }),
                Ok(text) => {
                    // Writing happens off the UI thread.
                    let key = inner.key();
                    let ui = inner.svc.ui.clone();
                    let _ = std::thread::Builder::new().name("sa-write".into()).spawn(move || {
                        let w = write_atomic(&target_w, &text);
                        key.post(&ui, move |inner| inner.subtitle_written(sid, target_w, loaded_before, video, w.err().map(|e| e.to_string())));
                    });
                }
            }
        });
    }

    fn subtitle_written(self: &Rc<Self>, sid: SessionId, target: PathBuf, loaded_before: Option<PathBuf>, video: Option<PathBuf>, error: Option<String>) {
        if let Some(e) = error {
            self.edit(|i, c, fx| i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: save_failure(&e) }));
            return;
        }
        if let Some(v) = &video {
            self.svc.media_index.invalidate_sidecar(v);
        }
        let hook = self.hooks.borrow().subtitle_saved.clone();
        if let Some(h) = hook {
            h(&target);
        }
        self.edit(|i, c, fx| {
            let Some(s) = c.st.session_mut(sid) else { return };
            if s.loaded_subtitle != loaded_before {
                return;
            }
            s.set_saved_subtitle_name(target.file_name().map(|n| n.to_string_lossy().into_owned()));
            let working = s.is_working();
            if !working && s.status != JobStatus::Finished {
                i.set_status(&mut c.st, fx, sid, JobStatus::Finished);
            } else {
                // Re-mirror the summary (saved name) into the list rows.
                i.session_status_changed(&mut c.st, fx, sid);
            }
            if c.st.is_foreground(sid) {
                fx.all(&[SubtitleChange::Status, SubtitleChange::Settings]);
            }
        });
    }

    /// Only translation jobs auto-save (mac-viewmodels §5.12).
    pub(crate) fn auto_save_translation_if_enabled(&self, c: &mut Core, fx: &mut Fx, sid: SessionId, settings: Option<&JobSettings>) {
        let auto_save = settings.map(|s| s.auto_save).unwrap_or(c.st.auto_save);
        let format = settings.map(|s| s.export_format).unwrap_or(c.st.export_format);
        let Some(s) = c.st.session(sid) else { return };
        if !auto_save || !s.has_translations() || !can_save(s) {
            return;
        }
        let Some(dir) = s.base_path().and_then(|p| p.parent()) else { return };
        if !dir.is_dir() {
            return;
        }
        self.save_subtitle_of(c, fx, sid, Some(true), format);
    }

    pub(crate) fn export_to(&self, c: &mut Core, fx: &mut Fx, target: PathBuf) {
        self.finish_editing(c, fx);
        let sid = c.st.foreground;
        let s = c.st.fg();
        let translated = s.export_language == ExportLanguage::Translated && s.has_translations();
        let list = s.export_sentences(translated);
        let format = c.st.export_format;
        self.spawn_render(list, format, CancellationToken::new(), move |inner, r| match r {
            Err(EngineError::Cancelled) => {}
            Err(e) => inner.edit(|i, c, fx| i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::error(&e) })),
            Ok(text) => {
                let key = inner.key();
                let ui = inner.svc.ui.clone();
                let _ = std::thread::Builder::new().name("sa-export".into()).spawn(move || {
                    let w = write_atomic(&target, &text);
                    key.post(&ui, move |inner| {
                        inner.edit(|i, c, fx| match w {
                            Ok(()) => {
                                if let Some(s) = c.st.session_mut(sid) {
                                    s.has_unsaved_edits = false;
                                }
                                fx.ch(SubtitleChange::Status);
                            }
                            Err(e) => i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: save_failure(&e.to_string()) }),
                        })
                    });
                });
            }
        });
    }

    pub(crate) fn render_foreground(&self, c: &mut Core, fx: &mut Fx, done: Box<dyn FnOnce(Option<String>)>) {
        self.finish_editing(c, fx);
        let sid = c.st.foreground;
        let s = c.st.fg();
        let translated = s.export_language == ExportLanguage::Translated && s.has_translations();
        let list = s.export_sentences(translated);
        let format = c.st.export_format;
        let id = {
            let mut h = self.hooks.borrow_mut();
            h.next_render += 1;
            let id = h.next_render;
            h.renders.insert(id, done);
            id
        };
        self.spawn_render(list, format, CancellationToken::new(), move |inner, r| {
            let cb = inner.hooks.borrow_mut().renders.remove(&id);
            match r {
                Ok(text) => {
                    if let Some(cb) = cb {
                        cb(Some(text));
                    }
                }
                Err(e) => {
                    if !matches!(e, EngineError::Cancelled) {
                        inner.edit(|i, c, fx| i.set_status(&mut c.st, fx, sid, JobStatus::Failed { message: LocalizedMessage::error(&e) }));
                    }
                    if let Some(cb) = cb {
                        cb(None);
                    }
                }
            }
        });
    }
}
