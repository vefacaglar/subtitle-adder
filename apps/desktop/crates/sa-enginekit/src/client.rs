//! Engine process management (mac-enginekit §3, win-enginekit §5).
//!
//! Every engine run is a child process with stdout piped (one dedicated reader thread per engine), stderr to a log
//! file `<temp>/subtitle-engine-<UUID>.log`, stdin from a background writer (render/translate), an inherited file
//! (preview) or the null device. Termination: Windows Job Object kill (`KILL_ON_JOB_CLOSE`), macOS `SIGTERM` only
//! (never SIGKILL; the engine stops `llama-server` itself) — see [`crate::process`].
//!
//! Shared rules (UNIFIED.md §3.7): unknown event types are skipped; an undecodable stdout line ends the run with an
//! error and terminates the engine; the external LLM variables reach `translate` only; `error` events become the
//! stream's terminal `Failed` error (the last one wins, whatever the exit code). All spawning happens on a worker
//! thread for the streaming commands; the one-shot commands block their (worker) caller.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use sa_core::cancel::CancellationToken;
use sa_core::diagnostics::DiagnosticSink;
use sa_core::engine_types::{EngineError, EngineModel, Sentence, SubtitleFormat};
use sa_core::l10n::{Arg, LanguageState, UiLanguage, engine_string};
use sa_core::models::ModelCatalogBackend;

use crate::command::EngineCommand;
use crate::events::{EngineEvent, decode};
use crate::launch::{
    EXTERNAL_LLM_API_KEY_VAR, EXTERNAL_LLM_URL_VAR, EngineLaunchSettings, EngineLaunchState, LANG_VAR, get_env, set_env,
};
use crate::line_splitter::LineSplitter;
use crate::process::{ChildHandle, ProcessControl};

/// `transcribe` options (defaults: no language, model `small`, `[Srt]`, no diarization, no speaker count).
#[derive(Debug, Clone, PartialEq)]
pub struct TranscribeOptions {
    pub language: Option<String>,
    pub model: String,
    pub formats: Vec<SubtitleFormat>,
    pub diarize: bool,
    /// Passed as `--speakers N` only when `diarize` is on.
    pub speakers: Option<u32>,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        Self { language: None, model: "small".into(), formats: vec![SubtitleFormat::Srt], diarize: false, speakers: None }
    }
}

/// A stream of events of one engine run. The terminal error (engine `error` event, non-zero exit, cancellation,
/// malformed line) is the last item. **Dropping the stream before it ends terminates the engine.**
pub struct EventStream {
    receiver: crossbeam_channel::Receiver<Result<EngineEvent, EngineError>>,
    cancel: CancellationToken,
}

impl EventStream {
    /// Builds a stream from a channel and the token that kills its process (fakes in tests use this too).
    pub fn new(receiver: crossbeam_channel::Receiver<Result<EngineEvent, EngineError>>, cancel: CancellationToken) -> Self {
        Self { receiver, cancel }
    }

    /// A stream that ends immediately (default `extract_audio` of fakes).
    pub fn empty() -> Self {
        let (_tx, rx) = crossbeam_channel::bounded(0);
        Self::new(rx, CancellationToken::new())
    }

    /// A stream that yields `items` and ends (fakes in view-model tests).
    pub fn from_items(items: Vec<Result<EngineEvent, EngineError>>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        for item in items {
            let _ = tx.send(item);
        }
        Self::new(rx, CancellationToken::new())
    }

    /// Blocks for the next item; `None` at the end.
    pub fn next_blocking(&self) -> Option<Result<EngineEvent, EngineError>> {
        self.receiver.recv().ok()
    }

    /// Waits at most `timeout`: `Ok(Some(item))`, `Ok(None)` at the end, `Err(())` when nothing arrived in time.
    #[allow(clippy::result_unit_err)]
    pub fn next_timeout(&self, timeout: Duration) -> Result<Option<Result<EngineEvent, EngineError>>, ()> {
        match self.receiver.recv_timeout(timeout) {
            Ok(item) => Ok(Some(item)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Ok(None),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(()),
        }
    }

    /// The underlying receiver (for `select!`).
    pub fn receiver(&self) -> &crossbeam_channel::Receiver<Result<EngineEvent, EngineError>> {
        &self.receiver
    }

    /// Terminates the engine now.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl Iterator for EventStream {
    type Item = Result<EngineEvent, EngineError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_blocking()
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// The engine operations the view models use (C# `IEngineClient`; tests substitute an in-process fake).
/// One-shot calls block; call them from worker threads.
pub trait EngineClient: Send + Sync {
    /// `extract-audio <video> --out-dir <dir> --json`.
    fn extract_audio(&self, video: &Path, out_dir: &Path, cancel: &CancellationToken) -> EventStream {
        let _ = (video, out_dir, cancel);
        EventStream::empty()
    }
    /// `transcribe <video> --out-dir <dir> --model <m> --json --format <f…> [--language l] [--diarize [--speakers N]]`.
    fn transcribe(&self, video: &Path, out_dir: &Path, options: &TranscribeOptions, cancel: &CancellationToken) -> EventStream;
    /// `translate --to <to> --model <m> --json [--from <from>]`, stdin = sentences JSON (words included);
    /// launched with the external-LLM environment (the only command that receives it).
    fn translate(&self, sentences: &[Sentence], to: &str, from: Option<&str>, model: &str, cancel: &CancellationToken) -> EventStream;
    /// `render --format <fmt>`, stdin = sentences JSON; stdout decoded lossily.
    fn render(&self, sentences: &[Sentence], format: SubtitleFormat, cancel: &CancellationToken) -> Result<String, EngineError>;
    /// `models --json` (last non-empty stdout line).
    fn models(&self, cancel: &CancellationToken) -> Result<Vec<EngineModel>, EngineError>;
    /// `delete-model <id> --json` → MB freed.
    fn delete_model(&self, id: &str, cancel: &CancellationToken) -> Result<i64, EngineError>;
    /// macOS only: `preview - --output <mp4> --json` with the opened source as stdin.
    fn make_preview(&self, source: &Path, output: &Path, cancel: &CancellationToken) -> Result<(), EngineError> {
        let _ = (source, output, cancel);
        Err(EngineError::Failed("preview is not supported by this client".into()))
    }
    /// One diagnostic line, redacting the launch API key. Never subtitle text or prompts.
    fn log_diagnostic(&self, category: &str, message: &str) {
        let _ = (category, message);
    }
}

/// Builds a stderr log path for one run.
pub type LogPathFactory = Arc<dyn Fn() -> PathBuf + Send + Sync>;

/// The real process-backed client.
pub struct ProcessEngineClient {
    pub command: EngineCommand,
    /// `None` = `LanguageState::shared()` at each launch.
    pub language: Option<UiLanguage>,
    /// `None` = `EngineLaunchState::shared()` at each launch.
    pub launch_settings: Option<EngineLaunchSettings>,
    /// Windows rule (win-enginekit §9.2): before a `translate` launch a `.local` external-LLM host is resolved to an
    /// IPv4 address (without an API key also through the cached address or the /24 scan). Default: on Windows only.
    pub resolve_local_llm_hosts: bool,
    /// macOS rule (mac-enginekit §3.2): a `translate` run's `[translation] ` stderr lines are copied into the
    /// diagnostic log (key redacted) before the stderr log is deleted. Default: on macOS only.
    pub copy_translation_trace: bool,
    /// Stderr log location of each run; `None` = `<temp>/subtitle-engine-<UUID>.log`.
    pub log_path_factory: Option<LogPathFactory>,
    /// Base environment of every launch; `None` = this process's environment at launch time (tests set it).
    pub base_environment: Option<Vec<(String, String)>>,
    process_control: Arc<dyn ProcessControl>,
    diagnostics: Arc<dyn DiagnosticSink>,
}

impl ProcessEngineClient {
    pub fn new(command: EngineCommand, process_control: Arc<dyn ProcessControl>, diagnostics: Arc<dyn DiagnosticSink>) -> Self {
        Self {
            command,
            language: None,
            launch_settings: None,
            resolve_local_llm_hosts: cfg!(windows),
            copy_translation_trace: cfg!(target_os = "macos"),
            log_path_factory: None,
            base_environment: None,
            process_control,
            diagnostics,
        }
    }

    /// A client for the located engine (`EngineCommand::locate_default`) with the platform process control.
    pub fn locate(diagnostics: Arc<dyn DiagnosticSink>) -> Result<Self, EngineError> {
        let command = EngineCommand::locate_default().ok_or(EngineError::NotFound)?;
        Ok(Self::new(command, crate::process::platform_process_control(), diagnostics))
    }

    /// The child environment: parent env + `SUBTITLE_ENGINE_LANG` + `EngineLaunchSettings::apply`.
    pub fn process_environment(
        language: UiLanguage,
        settings: &EngineLaunchSettings,
        base: impl IntoIterator<Item = (String, String)>,
        for_translation: bool,
    ) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = base.into_iter().collect();
        set_env(&mut env, LANG_VAR, language.code());
        settings.apply(&mut env, for_translation);
        env
    }

    /// The current process environment (non-UTF-8 entries converted lossily).
    pub fn current_environment() -> Vec<(String, String)> {
        std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect()
    }

    /// `self.language`, else `current`.
    pub fn resolved_language(&self, current: UiLanguage) -> UiLanguage {
        self.language.unwrap_or(current)
    }

    /// `self.launch_settings`, else `current`.
    pub fn resolved_launch_settings(&self, current: EngineLaunchSettings) -> EngineLaunchSettings {
        self.launch_settings.clone().unwrap_or(current)
    }

    /// `<temp>/subtitle-engine-<UUID>.log`.
    pub fn new_log_path() -> PathBuf {
        crate::working_dir::new_log_path(&std::env::temp_dir())
    }

    #[doc(hidden)]
    pub fn parts(&self) -> (&Arc<dyn ProcessControl>, &Arc<dyn DiagnosticSink>) {
        (&self.process_control, &self.diagnostics)
    }

    /// The `transcribe` argument vector (after the command's prefix arguments).
    pub fn transcribe_arguments(video: &Path, out_dir: &Path, options: &TranscribeOptions) -> Vec<String> {
        let mut args = vec![
            "transcribe".to_owned(),
            path_arg(video),
            "--out-dir".to_owned(),
            path_arg(out_dir),
            "--model".to_owned(),
            options.model.clone(),
            "--json".to_owned(),
            "--format".to_owned(),
        ];
        args.extend(options.formats.iter().map(|f| f.raw().to_owned()));
        if let Some(lang) = &options.language {
            args.push("--language".to_owned());
            args.push(lang.clone());
        }
        if options.diarize {
            args.push("--diarize".to_owned());
            if let Some(n) = options.speakers {
                args.push("--speakers".to_owned());
                args.push(n.to_string());
            }
        }
        args
    }

    /// The `extract-audio` argument vector.
    pub fn extract_audio_arguments(video: &Path, out_dir: &Path) -> Vec<String> {
        vec!["extract-audio".to_owned(), path_arg(video), "--out-dir".to_owned(), path_arg(out_dir), "--json".to_owned()]
    }

    /// The `translate` argument vector.
    pub fn translate_arguments(to: &str, from: Option<&str>, model: &str) -> Vec<String> {
        let mut args =
            vec!["translate".to_owned(), "--to".to_owned(), to.to_owned(), "--model".to_owned(), model.to_owned(), "--json".to_owned()];
        if let Some(from) = from {
            args.push("--from".to_owned());
            args.push(from.to_owned());
        }
        args
    }

    fn launcher(&self) -> Launcher {
        Launcher {
            command: self.command.clone(),
            language: self.language,
            launch_settings: self.launch_settings.clone(),
            resolve_local_llm_hosts: self.resolve_local_llm_hosts,
            copy_translation_trace: self.copy_translation_trace,
            log_path_factory: self.log_path_factory.clone(),
            base_environment: self.base_environment.clone(),
            control: Arc::clone(&self.process_control),
            diagnostics: Arc::clone(&self.diagnostics),
        }
    }

    fn stream(&self, args: Vec<String>, stdin: StdinSource, for_translation: bool, cancel: &CancellationToken) -> EventStream {
        let token = cancel.child();
        let (tx, rx) = crossbeam_channel::bounded(32);
        let launcher = self.launcher();
        let pump_token = token.clone();
        let spawned = std::thread::Builder::new()
            .name("sa-engine-pump".into())
            .spawn(move || pump(launcher, args, stdin, for_translation, pump_token, tx.clone()));
        if let Err(e) = spawned {
            let (tx, rx) = crossbeam_channel::bounded(1);
            let _ = tx.send(Err(EngineError::Failed(e.to_string())));
            return EventStream::new(rx, token);
        }
        EventStream::new(rx, token)
    }

    /// Runs a one-shot command to the end: `(stdout bytes, exit status, run)`.
    fn run_to_end(&self, args: &[String], stdin: StdinSource, cancel: &CancellationToken) -> Result<(Vec<u8>, i32, Run), EngineError> {
        let mut run = self.launcher().start(args, stdin, false, cancel)?;
        let mut stdout = run.stdout.take().expect("stdout is piped");
        let mut bytes = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let status = run.child.wait_exit();
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        Ok((bytes, status, run))
    }
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The last non-empty stdout line, split on `\n`: Windows trims every trailing `\r` (C# `TrimEnd('\r')`), macOS one
/// (the Swift `LineSplitter`).
fn last_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .split('\n')
        .map(|l| if cfg!(target_os = "macos") { l.strip_suffix('\r').unwrap_or(l) } else { l.trim_end_matches('\r') })
        .filter(|l| !l.is_empty())
        .last()
        .map(str::to_owned)
}

/// The `error` event message of a one-shot payload, if any.
fn error_message(payload: &Option<serde_json::Map<String, Value>>) -> Option<String> {
    let p = payload.as_ref()?;
    if p.get("event").and_then(Value::as_str) == Some("error") {
        return p.get("message").and_then(Value::as_str).map(str::to_owned);
    }
    None
}

fn payload(line: Option<String>) -> Option<serde_json::Map<String, Value>> {
    match serde_json::from_str::<Value>(&line?) {
        Ok(Value::Object(o)) => Some(o),
        _ => None,
    }
}

/// macOS: Swift decodes the last line as one `Decodable` payload (`ModelsPayload` / `DeletePayload`), so a
/// non-string `event` or any present, non-null optional field of the wrong type makes the whole payload absent (no
/// error message is read from it either). Windows reads each property on its own (C# `JsonDocument`).
fn swift_payload(
    payload: Option<serde_json::Map<String, Value>>,
    field_ok: impl Fn(&str, &Value) -> bool,
) -> Option<serde_json::Map<String, Value>> {
    if !cfg!(target_os = "macos") {
        return payload;
    }
    payload.filter(|p| {
        p.get("event").is_some_and(Value::is_string) && p.iter().all(|(k, v)| k == "event" || v.is_null() || field_ok(k, v))
    })
}

/// A JSON integer as the platform's decoder reads one: Windows `System.Text.Json` `GetInt32`/`TryGetInt32` (an
/// integer literal in the `i32` range; `464.0` and `1e3` fail), macOS `JSONDecoder` `Int` (any number that is exactly
/// an integer in the `i64` range, so `464.0` and `1e3` are accepted).
pub fn json_int(value: &Value) -> Option<i64> {
    if cfg!(target_os = "macos") {
        if let Some(i) = value.as_i64() {
            return Some(i);
        }
        let f = value.as_f64()?;
        // 2^63 is not representable as i64; -2^63 is.
        (f.fract() == 0.0 && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&f)).then_some(f as i64)
    } else {
        value.as_i64().filter(|v| i32::try_from(*v).is_ok())
    }
}

/// Decodes `models` rows (absent or `null` `kind`/`backend` take their defaults; any bad row fails the list; the
/// integer fields follow [`json_int`]).
pub fn decode_models(rows: &[Value]) -> Option<Vec<EngineModel>> {
    rows.iter()
        .map(|row| {
            let mut row = row.as_object()?.clone();
            for key in ["kind", "backend", "languages", "disk_mb", "license"] {
                if row.get(key).is_some_and(Value::is_null) {
                    row.remove(key);
                }
            }
            for key in ["size_mb", "disk_mb", "speed", "accuracy"] {
                if let Some(v) = row.get_mut(key) {
                    *v = Value::from(json_int(v)?);
                }
            }
            serde_json::from_value::<EngineModel>(Value::Object(row)).ok()
        })
        .collect()
}

impl EngineClient for ProcessEngineClient {
    fn extract_audio(&self, video: &Path, out_dir: &Path, cancel: &CancellationToken) -> EventStream {
        self.stream(Self::extract_audio_arguments(video, out_dir), StdinSource::Null, false, cancel)
    }

    fn transcribe(&self, video: &Path, out_dir: &Path, options: &TranscribeOptions, cancel: &CancellationToken) -> EventStream {
        self.stream(Self::transcribe_arguments(video, out_dir, options), StdinSource::Null, false, cancel)
    }

    fn translate(&self, sentences: &[Sentence], to: &str, from: Option<&str>, model: &str, cancel: &CancellationToken) -> EventStream {
        let payload = match Sentence::encode_list(sentences) {
            Ok(json) => json.into_bytes(),
            Err(e) => return EventStream::from_items(vec![Err(EngineError::Failed(e.to_string()))]),
        };
        self.stream(Self::translate_arguments(to, from, model), StdinSource::Data(payload), true, cancel)
    }

    fn render(&self, sentences: &[Sentence], format: SubtitleFormat, cancel: &CancellationToken) -> Result<String, EngineError> {
        let payload = Sentence::encode_list(sentences).map_err(|e| EngineError::Failed(e.to_string()))?;
        let args = vec!["render".to_owned(), "--format".to_owned(), format.raw().to_owned()];
        let (bytes, status, run) = self.run_to_end(&args, StdinSource::Data(payload.into_bytes()), cancel)?;
        if status != 0 {
            return Err(run.failure(status));
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn models(&self, cancel: &CancellationToken) -> Result<Vec<EngineModel>, EngineError> {
        let args = vec!["models".to_owned(), "--json".to_owned()];
        let (bytes, status, run) = self.run_to_end(&args, StdinSource::Null, cancel)?;
        let payload = swift_payload(payload(last_line(&bytes)), |key, value| match key {
            "message" => value.is_string(),
            "models" => value.as_array().is_some_and(|rows| decode_models(rows).is_some()),
            _ => true,
        });
        if let Some(message) = error_message(&payload) {
            return Err(EngineError::Failed(message));
        }
        if status != 0 {
            return Err(run.failure(status));
        }
        let models = payload
            .as_ref()
            .filter(|p| p.get("event").and_then(Value::as_str) == Some("models"))
            .and_then(|p| p.get("models"))
            .and_then(Value::as_array)
            .and_then(|rows| decode_models(rows));
        models.ok_or_else(|| EngineError::Failed(engine_string("Motor model listesi yazmadı.", Some(run.language), &[])))
    }

    fn delete_model(&self, id: &str, cancel: &CancellationToken) -> Result<i64, EngineError> {
        let args = vec!["delete-model".to_owned(), id.to_owned(), "--json".to_owned()];
        let (bytes, status, run) = self.run_to_end(&args, StdinSource::Null, cancel)?;
        let payload = swift_payload(payload(last_line(&bytes)), |key, value| match key {
            "message" | "id" => value.is_string(),
            "freed_mb" => json_int(value).is_some(),
            _ => true,
        });
        if let Some(message) = error_message(&payload) {
            return Err(EngineError::Failed(message));
        }
        if status != 0 {
            return Err(run.failure(status));
        }
        let freed = payload
            .as_ref()
            .filter(|p| p.get("event").and_then(Value::as_str) == Some("deleted"))
            .filter(|p| p.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|p| p.get("freed_mb"))
            .and_then(json_int);
        freed.ok_or_else(|| EngineError::Failed(engine_string("Motor silme sonucunu yazmadı.", Some(run.language), &[])))
    }

    fn make_preview(&self, source: &Path, output: &Path, cancel: &CancellationToken) -> Result<(), EngineError> {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let file = File::open(source).map_err(|e| EngineError::Failed(e.to_string()))?;
        let args = vec!["preview".to_owned(), "-".to_owned(), "--output".to_owned(), path_arg(output), "--json".to_owned()];
        let (bytes, status, run) = self.run_to_end(&args, StdinSource::File(file), cancel)?;
        let mut splitter = LineSplitter::new();
        let mut lines = splitter.feed(&bytes);
        lines.extend(splitter.rest());
        let mut engine_error = None;
        for line in lines.iter().filter(|l| !l.is_empty()) {
            if let Ok(Some(EngineEvent::Error { message })) = decode(line) {
                engine_error = Some(message);
            }
        }
        if let Some(message) = engine_error {
            return Err(EngineError::Failed(message));
        }
        if status != 0 {
            return Err(run.failure(status));
        }
        Ok(())
    }

    fn log_diagnostic(&self, category: &str, message: &str) {
        let settings = self.resolved_launch_settings(EngineLaunchState::shared().get());
        let key = settings.external_llm.as_ref().map(|l| l.api_key.trim().to_owned()).unwrap_or_default();
        self.diagnostics.write(category, &redact(message, &key));
    }
}

fn redact(text: &str, key: &str) -> String {
    if key.is_empty() { text.to_owned() } else { text.replace(key, "[redacted]") }
}

/// Every engine client can back the model catalog.
pub struct CatalogBackend(pub Arc<dyn EngineClient>);

impl ModelCatalogBackend for CatalogBackend {
    fn models(&self, cancel: &CancellationToken) -> Result<Vec<EngineModel>, EngineError> {
        self.0.models(cancel)
    }
    fn delete_model(&self, id: &str, cancel: &CancellationToken) -> Result<i64, EngineError> {
        self.0.delete_model(id, cancel)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Launching
// ---------------------------------------------------------------------------------------------------------------

/// Where a run's stdin comes from.
enum StdinSource {
    /// The null device (immediate EOF).
    Null,
    /// Written by a background thread, then closed (write errors such as a broken pipe are ignored).
    Data(Vec<u8>),
    /// An opened file inherited by the engine (preview).
    File(File),
}

/// Everything a launch needs, detached from the client so it can move to the pump thread.
struct Launcher {
    command: EngineCommand,
    language: Option<UiLanguage>,
    launch_settings: Option<EngineLaunchSettings>,
    resolve_local_llm_hosts: bool,
    copy_translation_trace: bool,
    log_path_factory: Option<LogPathFactory>,
    base_environment: Option<Vec<(String, String)>>,
    control: Arc<dyn ProcessControl>,
    diagnostics: Arc<dyn DiagnosticSink>,
}

impl Launcher {
    fn start(&self, args: &[String], stdin: StdinSource, for_translation: bool, cancel: &CancellationToken) -> Result<Run, EngineError> {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let language = self.language.unwrap_or_else(|| LanguageState::shared().language());
        let settings = self.launch_settings.clone().unwrap_or_else(|| EngineLaunchState::shared().get());
        let base = self.base_environment.clone().unwrap_or_else(ProcessEngineClient::current_environment);
        let mut env = ProcessEngineClient::process_environment(language, &settings, base, for_translation);
        let run_key = if for_translation { get_env(&env, EXTERNAL_LLM_API_KEY_VAR).unwrap_or("").to_owned() } else { String::new() };
        if for_translation
            && self.resolve_local_llm_hosts
            && let Some(url) = get_env(&env, EXTERNAL_LLM_URL_VAR).map(str::to_owned)
        {
            let resolved = crate::external_llm::ExternalLlmServer::resolve_local_host(&url, !run_key.is_empty(), cancel);
            set_env(&mut env, EXTERNAL_LLM_URL_VAR, &resolved);
        }

        let log_path = self.log_path_factory.as_ref().map_or_else(ProcessEngineClient::new_log_path, |f| f());
        let log = create_log(&log_path).map_err(|e| start_failure(language, &e))?;

        let mut cmd = Command::new(&self.command.executable);
        cmd.args(&self.command.arguments).args(args);
        cmd.env_clear().envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd.stdout(Stdio::piped()).stderr(Stdio::from(log));
        let mut payload = None;
        match stdin {
            StdinSource::Null => {
                cmd.stdin(Stdio::null());
            }
            StdinSource::Data(bytes) => {
                cmd.stdin(Stdio::piped());
                payload = Some(bytes);
            }
            StdinSource::File(file) => {
                cmd.stdin(Stdio::from(file));
            }
        }
        let priority = crate::process::EnginePriority::for_arguments(args);
        self.control.configure(&mut cmd, priority);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                drop(cmd);
                let _ = std::fs::remove_file(&log_path);
                return Err(start_failure(language, &e));
            }
        };
        drop(cmd);
        let stdout = child.stdout.take();
        let child_stdin = child.stdin.take();
        let handle = ChildHandle::new(child);
        self.control.adopt(&handle, priority);

        if let (Some(mut pipe), Some(bytes)) = (child_stdin, payload) {
            let _ = std::thread::Builder::new().name("sa-engine-stdin".into()).spawn(move || {
                let _ = pipe.write_all(&bytes);
                let _ = pipe.flush();
                drop(pipe);
            });
        }

        let weak = Arc::downgrade(&handle);
        let control = Arc::clone(&self.control);
        cancel.on_cancel(move || {
            if let Some(child) = weak.upgrade() {
                control.terminate(&child);
            }
        });

        // Swift redacts the configured key of the run's launch settings, whether or not its address was usable.
        let trace = (for_translation && self.copy_translation_trace && args.first().is_some_and(|a| a == "translate")).then(|| {
            let key = settings.external_llm.as_ref().map(|l| l.api_key.trim().to_owned()).unwrap_or_default();
            (Arc::clone(&self.diagnostics), key)
        });
        Ok(Run { child: handle, stdout, log_path, language, control: Arc::clone(&self.control), trace })
    }
}

fn start_failure(language: UiLanguage, e: &std::io::Error) -> EngineError {
    EngineError::Failed(engine_string("Motor başlatılamadı: %@", Some(language), &[Arg::Str(e.to_string())]))
}

#[cfg(windows)]
fn create_log(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).share_mode(7).open(path)
}

#[cfg(not(windows))]
fn create_log(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(path)
}

/// One running engine. Dropping it runs the cleanup (terminate if still running, copy the translation trace, delete
/// the stderr log) on every exit path.
struct Run {
    child: Arc<ChildHandle>,
    stdout: Option<ChildStdout>,
    log_path: PathBuf,
    /// The language the run was launched with (its messages use it).
    language: UiLanguage,
    control: Arc<dyn ProcessControl>,
    /// `translate` runs on macOS: where to copy `[translation] ` lines and which key to redact.
    trace: Option<(Arc<dyn DiagnosticSink>, String)>,
}

impl Run {
    /// `Motor hata koduyla çıktı (%lld). %@` with the last 5 non-empty stderr lines.
    fn failure(&self, status: i32) -> EngineError {
        EngineError::Failed(engine_string(
            "Motor hata koduyla çıktı (%lld). %@",
            Some(self.language),
            &[Arg::Int(i64::from(status)), Arg::Str(log_tail(&self.log_path))],
        ))
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        drop(self.stdout.take());
        if !self.child.has_exited() {
            self.control.terminate(&self.child);
        }
        let exited = self.child.wait_exit_timeout(Duration::from_secs(2)).is_some();
        let log = std::mem::take(&mut self.log_path);
        let trace = self.trace.take();
        let finish = move || {
            if let Some((sink, key)) = trace {
                for line in translation_lines(&log) {
                    sink.write("engine", &redact(&line, &key));
                }
            }
            let _ = std::fs::remove_file(&log);
        };
        if exited {
            finish();
        } else {
            // The engine is still shutting down (SIGTERM): reap it and delete the log off this thread.
            let child = Arc::clone(&self.child);
            let _ = std::thread::Builder::new().name("sa-engine-reaper".into()).spawn(move || {
                child.wait_exit();
                finish();
            });
        }
    }
}

/// The last 5 non-empty lines of a stderr log, joined with `\n`, as each platform's app cut them:
/// * Windows (C# `FailureAsync`): decoded lossily, split on `\n`, every trailing `\r` removed;
/// * macOS (Swift `failure`): a log that is not valid UTF-8 gives `""` (`String(contentsOf:encoding: .utf8)` fails);
///   split on the `Character` `"\n"`, which a CR LF pair is not (one grapheme cluster), and nothing is trimmed.
pub fn log_tail(path: &Path) -> String {
    let Ok(bytes) = std::fs::read(path) else { return String::new() };
    let lines: Vec<String> = if cfg!(target_os = "macos") {
        match String::from_utf8(bytes) {
            Ok(text) => split_on_lone_newlines(&text).into_iter().filter(|l| !l.is_empty()).map(str::to_owned).collect(),
            Err(_) => return String::new(),
        }
    } else {
        let text = String::from_utf8_lossy(&bytes);
        text.split('\n').map(|l| l.trim_end_matches('\r')).filter(|l| !l.is_empty()).map(str::to_owned).collect()
    };
    lines[lines.len().saturating_sub(5)..].join("\n")
}

/// Swift `String.split(separator: "\n")`: splits at every `\n` that is not the second half of a `\r\n` cluster.
fn split_on_lone_newlines(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut pieces = Vec::new();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' && !(i > 0 && bytes[i - 1] == b'\r') {
            pieces.push(&text[start..i]);
            start = i + 1;
        }
    }
    pieces.push(&text[start..]);
    pieces
}

/// The `[translation] ` lines of a stderr log (mac-enginekit 9.5: the last 4 MiB, split on LF or CR, at most the
/// last 500 lines), as `sa_core::diagnostics::DiagnosticLog::translation_lines` extracts them.
pub fn translation_lines(path: &Path) -> Vec<String> {
    sa_core::diagnostics::DiagnosticLog::translation_lines(path)
}

/// What one stdout line means for the run.
enum LineOutcome {
    Continue,
    /// Stop now; the error (if any) is the stream's terminal item, sent after the engine was terminated.
    Stop(Option<EngineError>),
}

/// The pump of one streaming run (runs on its own thread; also the run's stdout reader).
fn pump(
    launcher: Launcher,
    args: Vec<String>,
    stdin: StdinSource,
    for_translation: bool,
    token: CancellationToken,
    tx: crossbeam_channel::Sender<Result<EngineEvent, EngineError>>,
) {
    let mut run = match launcher.start(&args, stdin, for_translation, &token) {
        Ok(run) => run,
        Err(e) => {
            let _ = tx.send(Err(e));
            return;
        }
    };
    let mut stdout = run.stdout.take().expect("stdout is piped");
    let mut splitter = LineSplitter::new();
    let mut engine_error: Option<String> = None;
    let mut buf = vec![0u8; 64 * 1024];

    let handle = |line: String, engine_error: &mut Option<String>| -> LineOutcome {
        if line.is_empty() {
            return LineOutcome::Continue;
        }
        if token.is_cancelled() {
            return LineOutcome::Stop(Some(EngineError::Cancelled));
        }
        match decode(&line) {
            Err(e) => LineOutcome::Stop(Some(EngineError::Failed(e.0))),
            Ok(None) => LineOutcome::Continue,
            Ok(Some(EngineEvent::Error { message })) => {
                *engine_error = Some(message);
                LineOutcome::Continue
            }
            Ok(Some(event)) => match tx.send(Ok(event)) {
                Ok(()) => LineOutcome::Continue,
                // The consumer dropped the stream.
                Err(_) => LineOutcome::Stop(None),
            },
        }
    };

    let mut stop: Option<Option<EngineError>> = None;
    'read: loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for line in splitter.feed(&buf[..n]) {
            if let LineOutcome::Stop(e) = handle(line, &mut engine_error) {
                stop = Some(e);
                break 'read;
            }
        }
    }
    if stop.is_none() {
        for line in splitter.rest() {
            if let LineOutcome::Stop(e) = handle(line, &mut engine_error) {
                stop = Some(e);
                break;
            }
        }
    }
    drop(stdout);
    let terminal = match stop {
        Some(e) => e,
        None => {
            let status = run.child.wait_exit();
            if token.is_cancelled() {
                Some(EngineError::Cancelled)
            } else if let Some(message) = engine_error {
                Some(EngineError::Failed(message))
            } else if status != 0 {
                Some(run.failure(status))
            } else {
                None
            }
        }
    };
    // Terminate (if still running) and clean up before the stream ends, so the engine is gone and its stderr log
    // deleted once the consumer sees the end.
    drop(run);
    if let Some(e) = terminal {
        let _ = tx.send(Err(e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_are_read_as_the_platform_decoder_reads_them() {
        let v = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        assert_eq!(json_int(&v("464")), Some(464));
        assert_eq!(json_int(&v("-3")), Some(-3));
        assert_eq!(json_int(&v("464.5")), None);
        assert_eq!(json_int(&v("\"464\"")), None);
        if cfg!(target_os = "macos") {
            // Swift `JSONDecoder` reads any number that is exactly an integer.
            assert_eq!(json_int(&v("464.0")), Some(464));
            assert_eq!(json_int(&v("1e3")), Some(1000));
            assert_eq!(json_int(&v("2147483648")), Some(2_147_483_648));
            assert_eq!(json_int(&v("1e19")), None);
        } else {
            // .NET `TryGetInt32`: an integer literal in the `int` range only.
            assert_eq!(json_int(&v("464.0")), None);
            assert_eq!(json_int(&v("1e3")), None);
            assert_eq!(json_int(&v("2147483647")), Some(2_147_483_647));
            assert_eq!(json_int(&v("2147483648")), None);
        }
    }

    #[test]
    fn model_rows_follow_the_integer_rule() {
        let rows: Vec<Value> =
            serde_json::from_str(r#"[{"id": "small", "size_mb": 464.0, "speed": 4, "accuracy": 3, "downloaded": true, "disk_mb": null}]"#)
                .unwrap();
        let decoded = decode_models(&rows);
        if cfg!(target_os = "macos") {
            assert_eq!(decoded.unwrap()[0].size_mb, 464);
        } else {
            assert!(decoded.is_none());
        }
    }

    #[test]
    fn the_log_tail_keeps_the_last_five_lines_without_carriage_returns() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("e.log");
        std::fs::write(&log, "1\r\n2\n\n3\r\r\n4\n5\n6 ünlü\n").unwrap();
        if cfg!(target_os = "macos") {
            // Swift: a CR LF pair is one `Character`, never equal to "\n", so it does not split; nothing is trimmed.
            assert_eq!(log_tail(&log), "1\r\n2\n3\r\r\n4\n5\n6 ünlü");
        } else {
            assert_eq!(log_tail(&log), "2\n3\n4\n5\n6 ünlü");
        }
        assert_eq!(split_on_lone_newlines("a\nb\r\nc\n\nd\r"), vec!["a", "b\r\nc", "", "d\r"]);
        std::fs::write(&log, b"ok\n\xff\xfebad\n").unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(log_tail(&log), "");
        } else {
            assert_eq!(log_tail(&log), "ok\n\u{FFFD}\u{FFFD}bad");
        }
        assert_eq!(log_tail(&dir.path().join("missing.log")), "");
    }
}
