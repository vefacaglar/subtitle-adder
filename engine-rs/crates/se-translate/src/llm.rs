//! `LlamaTranslator`: prompt-driven LLM translation through a local `llama-server` or an external
//! OpenAI-compatible server (spec translate-llm.md §2.4, §4, §7, §10; translate-local.md §8).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use se_core::{BatchFn, EngineError, Map, ProgressFn, Result, Sentence, Value, json, pyfmt, t};

use crate::checkpoint::{self, Cache};
use crate::diag::{self, DiagSink};
use crate::external::{
    api_key_transport_allowed, check_external_llm, external_llm_api_key, external_llm_model, external_llm_url,
    external_models_url, is_opencode_go_url, normalize_external_chat_url,
};
use crate::http::{HttpFailure, HttpRequest, HttpTransport, UreqTransport, http_error_detail};
use crate::lan::{LanResolver, NetResolver, SystemLanNet};
use crate::profiles::{TranslationProfile, build_hy_mt_prompt, profile_for_model};
use crate::prompts::{build_system_prompt, build_translategemma_prompt, build_user_prompt, parse_numbered_translations, retry_suffix};
use crate::server::{self, LlamaServer};
use crate::validate::{clean_translation_pair, has_source_translation_pair, has_unrequested_markup, looks_untranslated};
use crate::{Device, Translator};

/// One chat message (`{"role": ..., "content": ...}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self { role: role.to_string(), content: content.into() }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new("user", content)
    }

    fn to_value(&self) -> Value {
        Value::Object(Map::new().with("role", self.role.as_str()).with("content", self.content.as_str()))
    }
}

/// An injected completion function (tests; Python `completion_fn`): messages → raw model reply.
pub type CompletionFn = Box<dyn FnMut(&[ChatMessage]) -> Result<String>>;

/// A replacement for `_temp_checkpoint_path(model_id, target, source, sentences, identity)` (tests).
pub type CheckpointPathFn = Box<dyn FnMut(&str, &str, Option<&str>, &[Sentence], &str) -> Result<Option<PathBuf>>>;

/// Where the chunk checkpoint lives (Python tests patch `_temp_checkpoint_path`).
pub enum CheckpointMode {
    /// `<temp>/SubtitleAdder/translations/chunk-<hash>.json` ([`checkpoint::checkpoint_path`]).
    Default,
    /// No checkpoint (the patched function returned `None`).
    Disabled,
    /// A fixed file.
    Fixed(PathBuf),
    /// Any function (e.g. one recording the real path).
    Custom(CheckpointPathFn),
}

/// Construction options (Python keyword arguments plus the module constants and seams tests override).
pub struct LlmOptions {
    pub device: Device,
    /// When set, no server is started and no HTTP request is made.
    pub completion_fn: Option<CompletionFn>,
    /// `None` = read `SUBTITLE_ENGINE_EXTERNAL_LLM_URL` (only when `completion_fn` is `None`).
    pub external_url: Option<String>,
    /// `None` = read `SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL` (only when `completion_fn` is `None`).
    pub external_model: Option<String>,
    /// `None` = read `SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY` (only when `completion_fn` is `None`).
    pub external_api_key: Option<String>,
    /// [`crate::LLM_BATCH_SIZE`].
    pub batch_size: usize,
    /// [`crate::LLM_CONTEXT_LINES`].
    pub context_lines: usize,
    /// `_HTTP_TRACE_INTERVAL` (15 s; `request_waiting` heartbeat).
    pub http_trace_interval: Duration,
    /// Test seam: `completion_fn` stands for a patched `_complete` (Python tests construct with a
    /// `completion_fn`, then clear it and patch `_complete`), so checkpoints stay enabled.
    pub completion_keeps_checkpoint: bool,
    /// Checkpoint location (default: the real temp path).
    pub checkpoint: CheckpointMode,
    /// `urlopen` replacement (default: [`UreqTransport`]).
    pub transport: Option<Arc<dyn HttpTransport>>,
    /// `_resolve_lan_url` replacement (default: the system resolver and subnet scan).
    pub resolver: Option<Box<dyn LanResolver>>,
    /// `_translation_diagnostic` replacement (default: stderr).
    pub diagnostics: Option<DiagSink>,
    /// Test seam: a `llama-server` already listening on `127.0.0.1:<port>` (Python tests assign `_process` and
    /// `_port`); local requests go there and no server is started.
    pub local_port: Option<u16>,
}

impl Default for LlmOptions {
    fn default() -> Self {
        Self {
            device: Device::Auto,
            completion_fn: None,
            external_url: None,
            external_model: None,
            external_api_key: None,
            batch_size: crate::LLM_BATCH_SIZE,
            context_lines: crate::LLM_CONTEXT_LINES,
            http_trace_interval: Duration::from_secs(15),
            completion_keeps_checkpoint: false,
            checkpoint: CheckpointMode::Default,
            transport: None,
            resolver: None,
            diagnostics: None,
            local_port: None,
        }
    }
}

/// `_translation_batches(pending, limit)`: `(start, end)` ranges into `pending`, preferring a batch end after
/// a sentence end, a speaker change or a pause of at least 1.2 s (never shorter than `max(1, limit / 2)`).
pub fn translation_batches(pending: &[&Sentence], limit: usize) -> Vec<(usize, usize)> {
    let limit = limit.max(1);
    let mut out = Vec::new();
    let mut start = 0;
    while start < pending.len() {
        let mut end = (start + limit).min(pending.len());
        if end < pending.len() {
            let lowest = start + (limit / 2).max(1);
            let mut boundary = end;
            while boundary >= lowest {
                let previous = pending[boundary - 1];
                let following = pending[boundary];
                let tail = crate::pyunicode::rstrip_chars(pyfmt::rstrip(&previous.text), "\"'\u{201D}\u{2019}\u{bb})]");
                if tail.ends_with(['.', '!', '?', '。', '！', '？'])
                    || previous.speaker != following.speaker
                    || following.start.f() - previous.end.f() >= 1.2
                {
                    end = boundary;
                    break;
                }
                boundary -= 1;
            }
        }
        out.push((start, end));
        start = end;
    }
    out
}

/// The LLM translator. Starts `llama-server` (killed on [`Translator::close`] and on drop) for local models.
pub struct LlamaTranslator {
    info: &'static se_models::ModelInfo,
    device: Device,
    completion_fn: Option<CompletionFn>,
    completion_keeps_checkpoint: bool,
    external_chat_url: Option<String>,
    external_model: Option<String>,
    external_api_key: Option<String>,
    resolved_external_model: Option<String>,
    session_id: String,
    request_count: u64,
    server: Option<LlamaServer>,
    model_path: Option<PathBuf>,
    server_path: Option<PathBuf>,
    batch_size: usize,
    context_lines: usize,
    http_trace_interval: Duration,
    checkpoint: CheckpointMode,
    transport: Arc<dyn HttpTransport>,
    resolver: Box<dyn LanResolver>,
    diag: DiagSink,
    local_port: Option<u16>,
}


fn strip_or_none(v: Option<String>) -> Option<String> {
    v.map(|s| pyfmt::strip(&s).to_string()).filter(|s| !s.is_empty())
}

const DEEPSEEK_NO_THINKING: &[&str] =
    &["deepseek-v4-flash", "deepseek-v4-pro", "deepseek-v4.1-flash", "deepseek-v4.1-pro", "deepseek-flash"];

impl LlamaTranslator {
    /// `LlamaTranslator(model_id, ...)`: validates the id (`unknown_translation_model_short`), normalizes and
    /// checks the external URL ([`crate::check_external_llm`]), or locates the GGUF file and `llama-server`
    /// (`offline_missing_llama`) and starts it (`llama_unavailable` on failure).
    pub fn new(model_id: &str, options: LlmOptions) -> Result<Self> {
        let has_fn = options.completion_fn.is_some();
        let resolved_url = options.external_url.or_else(|| if has_fn { None } else { external_llm_url() });
        let resolved_model = options.external_model.or_else(|| if has_fn { None } else { external_llm_model() });
        let resolved_key = options.external_api_key.or_else(|| if has_fn { None } else { external_llm_api_key() });
        let url_given = resolved_url.as_deref().is_some_and(|u| !u.is_empty());
        let info = match se_models::find(model_id) {
            Some(info)
                if info.kind == se_models::ModelKind::Translation
                    && (info.backend == se_models::Backend::Llama || url_given) =>
            {
                info
            }
            _ => return Err(EngineError::model(t!("unknown_translation_model_short", model = model_id))),
        };
        let external_chat_url = match resolved_url.as_deref().filter(|u| !u.is_empty()) {
            Some(u) => Some(normalize_external_chat_url(u)?),
            None => None,
        };
        let external_model = strip_or_none(resolved_model);
        let external_api_key = strip_or_none(resolved_key);
        if let Some(u) = resolved_url.as_deref().filter(|u| !u.is_empty()) {
            check_external_llm(u, external_api_key.as_deref())?;
        }
        let mut translator = Self {
            info,
            device: options.device,
            completion_fn: options.completion_fn,
            completion_keeps_checkpoint: options.completion_keeps_checkpoint,
            external_chat_url,
            resolved_external_model: external_model.clone(),
            external_model,
            external_api_key,
            session_id: uuid::Uuid::new_v4().simple().to_string(),
            request_count: 0,
            server: None,
            model_path: None,
            server_path: None,
            batch_size: options.batch_size,
            context_lines: options.context_lines,
            http_trace_interval: options.http_trace_interval,
            checkpoint: options.checkpoint,
            transport: options.transport.unwrap_or_else(|| Arc::new(UreqTransport::new())),
            resolver: options.resolver.unwrap_or_else(|| Box::new(NetResolver(SystemLanNet))),
            diag: options.diagnostics.unwrap_or_else(diag::stderr_sink),
            local_port: options.local_port,
        };
        if !has_fn && translator.external_chat_url.is_none() && translator.local_port.is_none() {
            let (model_path, server_path) = server::locate(model_id)?;
            translator.model_path = Some(model_path);
            translator.server_path = Some(server_path);
            translator.start_server()?;
        }
        Ok(translator)
    }

    /// The catalog entry.
    pub fn info(&self) -> &'static se_models::ModelInfo {
        self.info
    }

    /// The normalized external chat URL (mutated by `.local` resolution).
    pub fn external_chat_url(&self) -> Option<&str> {
        self.external_chat_url.as_deref()
    }

    /// Test seam: Python tests assign `_external_chat_url`.
    pub fn set_external_chat_url(&mut self, url: Option<String>) {
        self.external_chat_url = url;
    }

    /// Test seam: Python tests assign `_external_api_key`.
    pub fn set_external_api_key(&mut self, key: Option<String>) {
        self.external_api_key = key;
    }

    /// The per-translator OpenCode session id (32 lowercase hex characters).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn gpu_layers(&self) -> u32 {
        if self.device == Device::Cpu { 0 } else { 99 }
    }

    fn start_server(&mut self) -> Result<()> {
        self.close_server();
        let (Some(server_path), Some(model_path)) = (self.server_path.clone(), self.model_path.clone()) else {
            return Err(EngineError::model(t!("offline_missing_llama")));
        };
        let server = server::start_server(&server_path, &model_path, self.gpu_layers(), self.device == Device::Auto)?;
        self.server = Some(server);
        Ok(())
    }

    fn close_server(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop();
        }
    }

    fn emit(&self, event: &str, fields: Map) {
        (self.diag)(event, &fields);
    }

    /// `_request_headers(url)`: the API key only goes to the external server, and never over plain http to a
    /// host outside the local network (`external_llm_key_insecure`); `x-opencode-session` for OpenCode Go.
    pub fn request_headers(&self, url: Option<&str>) -> Result<Vec<(String, String)>> {
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
            ("User-Agent".to_string(), "SubtitleAdder/1.0".to_string()),
        ];
        if let (Some(key), Some(chat_url)) = (&self.external_api_key, &self.external_chat_url) {
            if !api_key_transport_allowed(url.unwrap_or(chat_url)) {
                return Err(EngineError::model(t!("external_llm_key_insecure")));
            }
            headers.push(("Authorization".to_string(), format!("Bearer {key}")));
        }
        if let Some(chat_url) = self.external_chat_url.as_deref().filter(|u| !u.is_empty()) {
            if is_opencode_go_url(chat_url) {
                headers.push(("x-opencode-session".to_string(), self.session_id.clone()));
            }
        }
        Ok(headers)
    }

    /// `_discover_external_model()`: the configured model, else the first model the server lists
    /// (`GET .../models`, 2.5 s, every failure swallowed), else the catalog id; cached.
    pub fn discover_external_model(&mut self) -> Result<String> {
        if let Some(m) = self.resolved_external_model.as_ref().filter(|m| !m.is_empty()) {
            return Ok(m.clone());
        }
        if let Some(chat_url) = self.external_chat_url.clone().filter(|u| !u.is_empty()) {
            if let Some(models_url) = external_models_url(&chat_url) {
                let headers = self.request_headers(Some(&models_url))?; // a refused key must not be swallowed
                let req = HttpRequest::new("GET", &models_url, headers, None);
                if let Ok(resp) = self.transport.open(&req, Duration::from_millis(2500)) {
                    if let Some(candidate) = first_listed_model(&resp.body) {
                        self.resolved_external_model = Some(candidate.clone());
                        return Ok(candidate);
                    }
                }
            }
        }
        self.resolved_external_model = Some(self.info.id.to_string());
        Ok(self.info.id.to_string())
    }

    /// `_post_completion(url, body, headers)`: one POST with `request_start` / `request_waiting` (heartbeat) /
    /// `request_complete` / `request_failed` diagnostics.
    fn post_completion(&mut self, url: &str, body: &Map, headers: &[(String, String)]) -> std::result::Result<Map, HttpFailure> {
        self.request_count += 1;
        let request_id = self.request_count;
        let started = Instant::now();
        let model = match body.get("model") {
            Some(v) if v.is_truthy() => v.as_str().map(str::to_string).unwrap_or_else(|| v.py_str()),
            _ => self.info.id.to_string(),
        };
        let model = match self.external_api_key.as_deref() {
            Some(key) if !key.is_empty() => model.replace(key, "[redacted]"),
            _ => model.replace('\0', "[redacted]"),
        };
        let host = crate::pyurl::urlsplit(url).ok().and_then(|p| p.hostname());
        self.emit(
            "request_start",
            Map::new().with("request", request_id as i64).with("host", host.map_or(Value::Null, Value::from)).with("model", model),
        );
        let (tx, rx) = mpsc::channel::<()>();
        let sink = self.diag.clone();
        let interval = self.http_trace_interval;
        let heartbeat = std::thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(interval) {
                let seconds = pyfmt::round_ndigits(started.elapsed().as_secs_f64(), 1);
                sink("request_waiting", &Map::new().with("request", request_id as i64).with("seconds", seconds));
            }
        });
        let request = HttpRequest::new(
            "POST",
            url,
            headers.to_vec(),
            Some(json::dumps(&Value::Object(body.clone()), true).into_bytes()),
        );
        let outcome = self.transport.open(&request, Duration::from_secs(300)).and_then(|resp| {
            let data = parse_json_body(&resp.body)?;
            let summary = response_summary(&data)?;
            let Value::Object(map) = data else { return Err(attr_error(&data)) };
            Ok((map, summary))
        });
        let _ = tx.send(());
        drop(tx);
        let _ = heartbeat.join();
        let seconds = pyfmt::round_ndigits(started.elapsed().as_secs_f64(), 2);
        match outcome {
            Ok((data, (truncated, content_chars, reasoning_chars))) => {
                self.emit(
                    "request_complete",
                    Map::new()
                        .with("request", request_id as i64)
                        .with("seconds", seconds)
                        .with("truncated", truncated)
                        .with("content_chars", content_chars as i64)
                        .with("reasoning_chars", reasoning_chars as i64),
                );
                Ok(data)
            }
            Err(failure) => {
                let status = failure.code().map_or(Value::Null, |c| Value::Int(c as i64));
                self.emit(
                    "request_failed",
                    Map::new().with("request", request_id as i64).with("seconds", seconds).with("http_status", status),
                );
                Err(failure)
            }
        }
    }

    /// `_complete(messages)`: one model reply (`""` for a truncated reply). Transport failures become
    /// `external_llm_unreachable` / `llama_unavailable`; the only HTTP retry is the 400/422 one without
    /// `chat_template_kwargs` on an external server.
    pub fn complete(&mut self, messages: &[ChatMessage]) -> Result<String> {
        if let Some(f) = self.completion_fn.as_mut() {
            return f(messages);
        }
        let messages_value = Value::Array(messages.iter().map(ChatMessage::to_value).collect());
        let (url, body) = if let Some(chat_url) = self.external_chat_url.clone() {
            let resolved = self.resolver.resolve(&chat_url, self.external_api_key.is_none());
            self.external_chat_url = Some(resolved.clone());
            let mut url = resolved;
            if url.to_lowercase().contains(":11434/v1/chat/completions") {
                let keep = url.chars().count().saturating_sub("/v1/chat/completions".len());
                url = url.chars().take(keep).collect::<String>() + "/api/chat";
            }
            let model = self.discover_external_model()?;
            let profile = profile_for_model(&model);
            let mut body = Map::new()
                .with("model", model.as_str())
                .with("messages", messages_value)
                .with("stream", false);
            merge(&mut body, profile.generation_options(url.to_lowercase().ends_with("/api/chat")));
            let deepseek_gateway = is_opencode_go_url(&url)
                || crate::pyurl::urlsplit(&url).map(|p| p.netloc.to_lowercase() == "api.deepseek.com").unwrap_or(false);
            if deepseek_gateway && DEEPSEEK_NO_THINKING.contains(&crate::pyunicode::casefold(&model).as_str()) {
                body.insert("thinking", Map::new().with("type", "disabled"));
            }
            (url, body)
        } else {
            let port = match (self.server.is_none(), self.local_port) {
                (true, Some(port)) => port,
                _ => {
                    let alive = self.server.as_mut().is_some_and(|s| !s.exited());
                    if !alive {
                        self.start_server()?;
                    }
                    self.server.as_ref().map(|s| s.port).unwrap_or(0)
                }
            };
            let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
            let mut body = Map::new().with("messages", messages_value).with("cache_prompt", false);
            merge(&mut body, profile_for_model(self.info.id).generation_options(false));
            (url, body)
        };
        let headers = self.request_headers(Some(&url))?;
        let external = self.external_chat_url.is_some();
        let result = match self.post_completion(&url, &body, &headers) {
            Err(HttpFailure::Http { code: 400 | 422, .. }) if external && body.contains_key("chat_template_kwargs") => {
                let mut retry = body.clone();
                retry.remove("chat_template_kwargs");
                self.post_completion(&url, &retry, &headers)
            }
            other => other,
        };
        let data = match result {
            Ok(data) => data,
            Err(failure) => {
                let detail = http_error_detail(&failure, self.external_api_key.as_deref());
                return Err(EngineError::model(if external {
                    t!("external_llm_unreachable", url = url, error = detail)
                } else {
                    t!("llama_unavailable", error = detail)
                }));
            }
        };
        Ok(reply_text(&data))
    }

    fn checkpoint_path_for(
        &mut self,
        target: &str,
        source: Option<&str>,
        sentences: &[Sentence],
        identity: &str,
    ) -> Result<Option<PathBuf>> {
        if self.completion_fn.is_some() && !self.completion_keeps_checkpoint {
            return Ok(None);
        }
        let model_id = self.info.id;
        match &mut self.checkpoint {
            CheckpointMode::Default => checkpoint::checkpoint_path(model_id, target, source, sentences, identity).map(Some),
            CheckpointMode::Disabled => Ok(None),
            CheckpointMode::Fixed(p) => Ok(Some(p.clone())),
            CheckpointMode::Custom(f) => f(model_id, target, source, sentences, identity),
        }
    }
}

fn merge(body: &mut Map, options: Value) {
    if let Value::Object(map) = options {
        for (k, v) in map.iter() {
            body.insert(k.clone(), v.clone());
        }
    }
}

fn parse_json_body(body: &[u8]) -> std::result::Result<Value, HttpFailure> {
    let text = std::str::from_utf8(body).map_err(|e| {
        HttpFailure::Other(format!("'utf-8' codec can't decode bytes in position {}: invalid continuation byte", e.valid_up_to()))
    })?;
    json::parse(text).map_err(|e| HttpFailure::Other(e.to_string()))
}

fn attr_error(v: &Value) -> HttpFailure {
    HttpFailure::Other(format!("'{}' object has no attribute 'get'", v.py_type_name()))
}

fn py_len(v: &Value) -> std::result::Result<usize, HttpFailure> {
    match v {
        Value::Str(s) => Ok(s.chars().count()),
        Value::Array(a) => Ok(a.len()),
        Value::Object(m) => Ok(m.len()),
        other if !other.is_truthy() => Ok(0),
        other => Err(HttpFailure::Other(format!("object of type '{}' has no len()", other.py_type_name()))),
    }
}

/// `(data.get(k) or default)` for a dict `data`.
fn get_or<'a>(map: &'a Map, key: &str) -> Option<&'a Value> {
    map.get(key).filter(|v| v.is_truthy())
}

/// The `request_complete` fields `(truncated, content_chars, reasoning_chars)`; the same shape checks Python
/// performs inside the request `try` (a non-dict reply fails the request).
fn response_summary(data: &Value) -> std::result::Result<(bool, usize, usize), HttpFailure> {
    let Value::Object(map) = data else { return Err(attr_error(data)) };
    let empty = Value::Object(Map::new());
    let choice = match get_or(map, "choices") {
        Some(Value::Array(items)) => items.first().cloned().unwrap_or(Value::Null),
        Some(other) => return Err(HttpFailure::Other(format!("'{}' object is not subscriptable", other.py_type_name()))),
        None => empty.clone(),
    };
    let Value::Object(choice) = &choice else { return Err(attr_error(&choice)) };
    let msg = get_or(choice, "message").or_else(|| get_or(map, "message")).cloned().unwrap_or(empty);
    let Value::Object(msg) = &msg else { return Err(attr_error(&msg)) };
    let finish = get_or(choice, "finish_reason").or_else(|| get_or(map, "done_reason"));
    let truncated = matches!(finish, Some(Value::Str(s)) if s == "length");
    let content = get_or(msg, "content").map(py_len).transpose()?.unwrap_or(0);
    let reasoning = get_or(msg, "reasoning_content").map(py_len).transpose()?.unwrap_or(0);
    Ok((truncated, content, reasoning))
}

/// The reply text of a parsed response (OpenAI `choices[0].message.content` or Ollama `message.content`);
/// `""` when truncated (`finish_reason` / `done_reason` `length`).
fn reply_text(data: &Map) -> String {
    let content_of = |msg: Option<&Value>| match msg.and_then(Value::as_object).and_then(|m| get_or(m, "content")) {
        Some(Value::Str(s)) => s.clone(),
        Some(other) => other.py_str(),
        None => String::new(),
    };
    if let Some(Value::Array(choices)) = get_or(data, "choices") {
        let first = choices.first().and_then(Value::as_object);
        if let Some(first) = first {
            if matches!(first.get("finish_reason"), Some(Value::Str(s)) if s == "length") {
                return String::new();
            }
            return content_of(get_or(first, "message"));
        }
        return String::new();
    }
    if let Some(Value::Object(_)) = data.get("message") {
        if matches!(data.get("done_reason"), Some(Value::Str(s)) if s == "length") {
            return String::new();
        }
        return content_of(data.get("message"));
    }
    String::new()
}

/// The first model id the discovery reply lists (`data` or `models`, `id` or `name`).
fn first_listed_model(body: &[u8]) -> Option<String> {
    let payload = parse_json_body(body).ok()?;
    let map = payload.as_object()?;
    let items = get_or(map, "data").or_else(|| get_or(map, "models"))?;
    let first = items.as_array()?.first()?.as_object()?;
    let value = get_or(first, "id").or_else(|| get_or(first, "name"))?;
    let text = match value {
        Value::Str(s) => s.clone(),
        other => other.py_str(),
    };
    let candidate = pyfmt::strip(&text).to_string();
    if candidate.is_empty() { None } else { Some(candidate) }
}

fn fresh(s: &Sentence, text: String, needs_review: bool) -> Sentence {
    Sentence { id: s.id, start: s.start, end: s.end, text, words: Vec::new(), speaker: s.speaker, needs_review }
}

fn strip_owned(s: &str) -> String {
    pyfmt::strip(s).to_string()
}

impl Translator for LlamaTranslator {
    fn translate(
        &mut self,
        sentences: &[Sentence],
        target: &str,
        source: Option<&str>,
        mut on_progress: Option<ProgressFn<'_>>,
        mut on_batch: Option<BatchFn<'_>>,
    ) -> Result<Vec<Sentence>> {
        crate::check_request(self.info.id, target, source)?;
        let mut result: Vec<Sentence> = sentences.iter().map(|s| fresh(s, s.text.clone(), false)).collect();
        let pending: Vec<(usize, &Sentence)> =
            sentences.iter().enumerate().filter(|(_, s)| !pyfmt::strip(&s.text).is_empty()).collect();
        if pending.is_empty() {
            if let Some(p) = on_progress.as_mut() {
                p(1.0);
            }
            return Ok(result);
        }

        let remote_model = if self.external_chat_url.is_some() { self.discover_external_model()? } else { String::new() };
        let profile: TranslationProfile =
            profile_for_model(if remote_model.is_empty() { self.info.id } else { remote_model.as_str() });
        let window_translation = matches!(profile.name.as_str(), "translategemma" | "hunyuan-mt");
        let specialized = window_translation || profile.name == "hy-mt1.5";
        let system_prompt = match profile.name.as_str() {
            "translategemma" => build_translategemma_prompt(&[], target, source)?,
            "hunyuan-mt" | "hy-mt1.5" => build_hy_mt_prompt(&[], target, source, None, profile.name == "hunyuan-mt"),
            _ => build_system_prompt(target, source),
        };
        let identity = format!(
            "{}\n{}\n{}\n{}",
            profile.py_repr(),
            system_prompt,
            self.external_chat_url.as_deref().unwrap_or(""),
            remote_model
        );
        let checkpoint_path = self.checkpoint_path_for(target, source, sentences, &identity)?;
        let mut cached = match &checkpoint_path {
            Some(p) => checkpoint::load(p),
            None => Cache::new(),
        };

        let ctx_n = self.context_lines;
        let pending_refs: Vec<&Sentence> = pending.iter().map(|(_, s)| *s).collect();
        let total = pending.len();
        let mut processed = 0usize;
        for (start, end) in translation_batches(&pending_refs, self.batch_size) {
            let batch = &pending[start..end];
            self.emit(
                "batch_start",
                Map::new().with("offset", start as i64).with("count", batch.len() as i64).with("total", total as i64),
            );
            let all_cached = !cached.is_empty()
                && batch.iter().all(|(_, s)| match cached.get(s.id) {
                    Some(c) => !looks_untranslated(&s.text, c, source, Some(target)) && !has_unrequested_markup(&s.text, c),
                    None => false,
                });
            if all_cached {
                for (index, s) in batch {
                    result[*index] = fresh(s, cached.get(s.id).unwrap_or("").to_string(), false);
                }
            } else {
                let ctx_start = start.saturating_sub(ctx_n);
                let context_lines: Vec<String> = pending[ctx_start..start].iter().map(|(_, s)| strip_owned(&s.text)).collect();
                let previous_translations: Vec<String> = pending[ctx_start..start]
                    .iter()
                    .map(|(i, _)| if result[*i].needs_review { String::new() } else { result[*i].text.clone() })
                    .collect();
                let nxt_end = total.min(start + batch.len() + ctx_n);
                let following_lines: Vec<String> =
                    pending[start + batch.len()..nxt_end].iter().map(|(_, s)| strip_owned(&s.text)).collect();
                let batch_texts: Vec<String> = batch.iter().map(|(_, s)| strip_owned(&s.text)).collect();
                let user_prompt = build_user_prompt(
                    &batch_texts,
                    Some(&context_lines),
                    Some(target),
                    Some(&following_lines),
                    Some(&previous_translations),
                );
                let mut messages = profile.chat_messages(&system_prompt, &user_prompt);
                let mut window: Vec<String> = Vec::new();
                if window_translation {
                    window = context_lines.iter().chain(&batch_texts).chain(&following_lines).cloned().collect();
                    let window_prompt = if profile.name == "translategemma" {
                        build_translategemma_prompt(&window, target, source)?
                    } else {
                        build_hy_mt_prompt(&window, target, source, None, true)
                    };
                    messages = vec![ChatMessage::user(window_prompt)];
                } else if profile.name == "hy-mt1.5" {
                    let mut reference = context_lines.clone();
                    if !previous_translations.is_empty() {
                        reference.push("Previous translations (terminology reference only):".to_string());
                        reference.extend(previous_translations.iter().cloned());
                    }
                    reference.extend(following_lines.iter().cloned());
                    messages = vec![ChatMessage::user(build_hy_mt_prompt(&batch_texts, target, source, Some(&reference), false))];
                }
                let references: Vec<String> = context_lines.iter().chain(&following_lines).cloned().collect();
                let rejected = |src: &str, cand: &str| {
                    looks_untranslated(src, cand, source, Some(target))
                        || has_unrequested_markup(src, cand)
                        || references.iter().any(|r| has_source_translation_pair(r, cand))
                };

                let mut accepted: BTreeMap<usize, String> = BTreeMap::new();
                let mut review_offsets: BTreeSet<usize> = BTreeSet::new();
                for attempt in 0..2 {
                    let raw_reply = self.complete(&messages)?;
                    let parsed_raw: BTreeMap<usize, String> = if window_translation {
                        let wp = parse_numbered_translations(&raw_reply, window.len());
                        (1..=batch.len())
                            .map(|i| (i, wp.get(&(i + context_lines.len())).cloned().unwrap_or_default()))
                            .collect()
                    } else {
                        parse_numbered_translations(&raw_reply, batch.len())
                    };
                    let parsed: Vec<String> = batch
                        .iter()
                        .enumerate()
                        .map(|(i, (_, s))| clean_translation_pair(&s.text, parsed_raw.get(&(i + 1)).map_or("", |v| v)))
                        .collect();
                    let invalid: Vec<usize> = batch
                        .iter()
                        .enumerate()
                        .filter(|(i, (_, s))| {
                            let cand = &parsed[*i];
                            pyfmt::strip(cand).is_empty() || rejected(&s.text, cand)
                        })
                        .map(|(i, _)| i + 1)
                        .collect();
                    for (i, text) in parsed.into_iter().enumerate() {
                        if !invalid.contains(&(i + 1)) {
                            accepted.insert(i + 1, text);
                        }
                    }
                    if invalid.is_empty() || attempt == 1 {
                        break;
                    }
                    self.emit(
                        "batch_retry",
                        Map::new().with("offset", start as i64).with("invalid_lines", invalid.len() as i64),
                    );
                    if specialized {
                        continue;
                    }
                    messages = profile.chat_messages(&system_prompt, &(user_prompt.clone() + &retry_suffix(batch.len(), target)));
                }

                for (i, (_, s)) in batch.iter().enumerate() {
                    let offset = i + 1;
                    if accepted.contains_key(&offset) {
                        continue;
                    }
                    self.emit("line_recovery", Map::new().with("offset", (start + offset - 1) as i64));
                    let before_all: Vec<String> = context_lines.iter().chain(&batch_texts[..offset - 1]).cloned().collect();
                    let before: Vec<String> = before_all[before_all.len().saturating_sub(ctx_n)..].to_vec();
                    let after: Vec<String> =
                        batch_texts[offset..].iter().chain(&following_lines).take(ctx_n).cloned().collect();
                    let single_messages = match profile.name.as_str() {
                        "translategemma" => {
                            vec![ChatMessage::user(build_translategemma_prompt(&[s.text.clone()], target, source)?)]
                        }
                        "hunyuan-mt" | "hy-mt1.5" => {
                            let context: Vec<String> = before.iter().chain(&after).cloned().collect();
                            let ctx = if profile.name == "hy-mt1.5" { Some(context.as_slice()) } else { None };
                            vec![ChatMessage::user(build_hy_mt_prompt(
                                &[s.text.clone()],
                                target,
                                source,
                                ctx,
                                profile.name == "hunyuan-mt",
                            ))]
                        }
                        _ => profile.chat_messages(
                            &system_prompt,
                            &build_user_prompt(&[s.text.clone()], Some(&before), Some(target), Some(&after), None),
                        ),
                    };
                    let reply = self.complete(&single_messages)?;
                    let single = parse_numbered_translations(&reply, 1).get(&1).cloned().unwrap_or_default();
                    let single = clean_translation_pair(&s.text, &single);
                    if !single.is_empty() && !rejected(&s.text, &single) {
                        accepted.insert(offset, single);
                    } else {
                        accepted.insert(offset, s.text.clone());
                        review_offsets.insert(offset);
                    }
                }

                for (i, (index, s)) in batch.iter().enumerate() {
                    let offset = i + 1;
                    let final_text = strip_owned(accepted.get(&offset).map_or("", |v| v));
                    let review = review_offsets.contains(&offset);
                    result[*index] = fresh(s, final_text.clone(), review);
                    if !review {
                        cached.insert(s.id, final_text);
                    }
                }
                if let Some(p) = &checkpoint_path {
                    checkpoint::save(p, &cached);
                }
            }
            processed += batch.len();
            self.emit("batch_complete", Map::new().with("processed", processed as i64).with("total", total as i64));
            if let Some(cb) = on_batch.as_mut() {
                let last_index = batch[batch.len() - 1].0;
                cb(&result[..=last_index]);
            }
            if let Some(p) = on_progress.as_mut() {
                p((processed as f64 / total as f64).min(1.0));
            }
        }
        checkpoint::delete(checkpoint_path.as_deref());
        Ok(result)
    }

    fn close(&mut self) {
        self.close_server();
    }
}

impl Drop for LlamaTranslator {
    fn drop(&mut self) {
        self.close_server();
    }
}
