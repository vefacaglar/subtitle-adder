//! Shared fixtures for the LLM translator tests (Python `conftest.py` + the fakes of the test modules).
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use se_core::{Map, Sentence, Value, i18n};
use se_translate::external::{EXTERNAL_LLM_API_KEY_ENV_VAR, EXTERNAL_LLM_MODEL_ENV_VAR, EXTERNAL_LLM_URL_ENV_VAR};
use se_translate::http::{HttpFailure, HttpRequest, HttpResponse, HttpTransport};
use se_translate::llm::{ChatMessage, CheckpointMode, LlmOptions};
use se_translate::{LlamaTranslator, Translator};

/// Per-test environment (conftest autouse fixtures): default language, strict offline off, external LLM env
/// vars unset, isolated temp dir. Tests using it must be `#[serial]`.
pub struct Env {
    pub temp: tempfile::TempDir,
}

impl Env {
    pub fn new() -> Self {
        Self::reset();
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().join("_tmp");
        std::fs::create_dir_all(&root).unwrap();
        se_core::fsutil::set_tempdir(Some(root));
        Env { temp }
    }

    fn reset() {
        // SAFETY: #[serial] tests.
        unsafe {
            std::env::remove_var(i18n::ENV_VAR);
            std::env::remove_var(EXTERNAL_LLM_URL_ENV_VAR);
            std::env::remove_var(EXTERNAL_LLM_MODEL_ENV_VAR);
            std::env::remove_var(EXTERNAL_LLM_API_KEY_ENV_VAR);
        }
        se_core::offline::reset_for_tests();
        i18n::set_language(None);
    }

    pub fn set(&self, name: &str, value: &str) {
        unsafe { std::env::set_var(name, value) };
    }

    pub fn english(&self) {
        self.set(i18n::ENV_VAR, "en");
    }

    pub fn offline(&self) {
        self.set(se_core::offline::OFFLINE_ENV_VAR, "1");
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        Self::reset();
        se_core::fsutil::set_tempdir(None);
    }
}

/// A fake `urlopen`: records every request and answers with `reply(request)`.
pub struct FakeTransport {
    pub requests: Mutex<Vec<HttpRequest>>,
    reply: Box<dyn Fn(&HttpRequest) -> Result<HttpResponse, HttpFailure>>,
}

impl FakeTransport {
    pub fn new(reply: impl Fn(&HttpRequest) -> Result<HttpResponse, HttpFailure> + 'static) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::new(Vec::new()), reply: Box::new(reply) })
    }

    /// Always answers with this JSON.
    pub fn json(payload: &str) -> Arc<Self> {
        let body = payload.as_bytes().to_vec();
        Self::new(move |_| Ok(HttpResponse { status: 200, body: body.clone() }))
    }

    /// Fails the test on any request.
    pub fn forbid() -> Arc<Self> {
        Self::new(|r| panic!("no request may be made: {}", r.url))
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for FakeTransport {
    fn open(&self, request: &HttpRequest, _timeout: Duration) -> Result<HttpResponse, HttpFailure> {
        self.requests.lock().unwrap().push(request.clone());
        (self.reply)(request)
    }
}

pub const CHAT_REPLY: &str = r#"{"choices": [{"message": {"content": "[1] Merhaba!"}}]}"#;

pub fn messages() -> Vec<ChatMessage> {
    vec![ChatMessage::user("hi")]
}

/// `_resolve_lan_url` patched to the identity.
pub fn identity_resolver() -> Option<Box<dyn se_translate::lan::LanResolver>> {
    Some(Box::new(|u: &str, _allow: bool| u.to_string()))
}

/// Options for an external translator with a fake transport.
pub fn external(url: &str, model: Option<&str>, key: Option<&str>, transport: Arc<FakeTransport>) -> LlmOptions {
    LlmOptions {
        external_url: Some(url.to_string()),
        external_model: model.map(str::to_string),
        external_api_key: key.map(str::to_string),
        transport: Some(transport),
        ..LlmOptions::default()
    }
}

/// Recorded calls of a `completion_fn`.
pub type Calls = Arc<Mutex<Vec<Vec<ChatMessage>>>>;

/// A `completion_fn` answering from `replies` in order and recording every call.
pub fn scripted(replies: &[&str]) -> (se_translate::llm::CompletionFn, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let queue: Arc<Mutex<std::collections::VecDeque<String>>> =
        Arc::new(Mutex::new(replies.iter().map(|s| s.to_string()).collect()));
    let rec = calls.clone();
    let f = Box::new(move |m: &[ChatMessage]| {
        rec.lock().unwrap().push(m.to_vec());
        Ok(queue.lock().unwrap().pop_front().expect("unexpected extra completion call"))
    });
    (f, calls)
}

/// A translator driven by a `completion_fn`.
pub fn with_completion(model: &str, f: se_translate::llm::CompletionFn) -> LlamaTranslator {
    LlamaTranslator::new(model, LlmOptions { completion_fn: Some(f), ..LlmOptions::default() }).expect("translator")
}

/// `completion_fn` standing for a patched `_complete` (checkpoints enabled) with a fixed checkpoint path.
pub fn with_patched_complete(model: &str, f: se_translate::llm::CompletionFn, checkpoint: CheckpointMode) -> LlamaTranslator {
    LlamaTranslator::new(
        model,
        LlmOptions { completion_fn: Some(f), completion_keeps_checkpoint: true, checkpoint, ..LlmOptions::default() },
    )
    .expect("translator")
}

pub fn sentence(id: i64, start: f64, end: f64, text: &str) -> Sentence {
    Sentence::new(id, start, end, text)
}

pub fn int_sentence(id: i64, start: i64, end: i64, text: &str) -> Sentence {
    let mut s = Sentence::new(id, 0.0, 0.0, text);
    s.start = se_core::Num::Int(start);
    s.end = se_core::Num::Int(end);
    s
}

/// `translator.translate(...)` collecting batch events (texts) and progress values.
pub fn translate_collect(
    t: &mut LlamaTranslator,
    sentences: &[Sentence],
    target: &str,
    source: Option<&str>,
) -> (se_core::Result<Vec<Sentence>>, Vec<Vec<Sentence>>, Vec<f64>) {
    let mut batches: Vec<Vec<Sentence>> = Vec::new();
    let mut progress: Vec<f64> = Vec::new();
    let result = {
        let mut on_batch = |b: &[Sentence]| batches.push(b.to_vec());
        let mut on_progress = |f: f64| progress.push(f);
        t.translate(sentences, target, source, Some(&mut on_progress), Some(&mut on_batch))
    };
    (result, batches, progress)
}

/// Diagnostics captured as `(event, fields)`.
pub type Captured = Arc<Mutex<Vec<(String, Map)>>>;

pub fn capture_diagnostics() -> (se_translate::diag::DiagSink, Captured) {
    let events: Captured = Arc::new(Mutex::new(Vec::new()));
    let rec = events.clone();
    (Arc::new(move |e: &str, f: &Map| rec.lock().unwrap().push((e.to_string(), f.clone()))), events)
}

/// The stderr text the captured diagnostics would have produced.
pub fn diagnostics_text(events: &Captured) -> String {
    events.lock().unwrap().iter().map(|(e, f)| se_translate::diag::format_line(e, f) + "\n").collect()
}

// ---- a tiny HTTP/1.1 server on loopback ---------------------------------------------------------------

/// A request the mock server received.
#[derive(Clone, Debug)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Received {
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
    }
}

/// A mock response: status, extra headers, body.
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(body: &str) -> Self {
        Reply { status: 200, headers: vec![("Content-Type".into(), "application/json".into())], body: body.as_bytes().to_vec() }
    }

    pub fn redirect(status: u16, location: &str) -> Self {
        Reply { status, headers: vec![("Location".into(), location.to_string())], body: Vec::new() }
    }
}

/// Serves `handler` on `127.0.0.1:<port>` in a background thread (one connection at a time, `Connection:
/// close`). Returns the port; the thread lives until the test process exits.
pub fn serve(handler: impl Fn(&Received) -> Reply + Send + Sync + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut it = line.split_whitespace();
            let method = it.next().unwrap_or("").to_string();
            let path = it.next().unwrap_or("").to_string();
            let mut headers = Vec::new();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.trim_end().split_once(':') {
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            let len = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; len];
            let _ = reader.read_exact(&mut body);
            let received = Received { method, path, headers, body };
            let reply = handler(&received);
            let reason = match reply.status {
                200 => "OK",
                301 => "Moved Permanently",
                302 => "Found",
                303 => "See Other",
                400 => "Bad Request",
                _ => "Status",
            };
            let mut out = format!("HTTP/1.1 {} {}\r\n", reply.status, reason);
            for (k, v) in &reply.headers {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
            out.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", reply.body.len()));
            let _ = stream.write_all(out.as_bytes());
            let _ = stream.write_all(&reply.body);
            let _ = stream.flush();
        }
    });
    port
}

pub fn value_str(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}
