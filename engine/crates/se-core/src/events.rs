//! Engine protocol (version 1) event lines on stdout.
//!
//! Every event is `json.dumps(payload, ensure_ascii=False) + "\n"`, flushed immediately, with the exact
//! key orders below (Python `_emit`). The builders return a [`Value`] so tests can compare structure;
//! [`emit`] writes one line.

use std::io::{self, Write};

use crate::json::{self, Map, Value};
use crate::types::{Sentence, sentences_to_value};

/// `PROTOCOL_VERSION` of `cli.py` (printed by `--version` as `subtitle-engine protocol 1`).
pub const PROTOCOL_VERSION: u32 = 1;

/// Stage ids of the `stage` event.
pub mod stages {
    pub const EXTRACTING_AUDIO: &str = "extracting_audio";
    pub const DOWNLOADING_MODEL: &str = "downloading_model";
    pub const LOADING_MODEL: &str = "loading_model";
    pub const TRANSCRIBING: &str = "transcribing";
    pub const DIARIZING: &str = "diarizing";
    pub const SEGMENTING: &str = "segmenting";
    pub const TRANSLATING: &str = "translating";
}

/// One event line: `json.dumps(payload, ensure_ascii=False)` + `\n`, then flush.
pub fn emit(out: &mut dyn Write, payload: &Value) -> io::Result<()> {
    let mut line = json::dumps(payload, false);
    line.push('\n');
    out.write_all(line.as_bytes())?;
    out.flush()
}

/// `{"event": "stage", "stage": ..., "fraction": f | null}`.
pub fn stage(stage: &str, fraction: Option<f64>) -> Value {
    Map::new().with("event", "stage").with("stage", stage).with("fraction", fraction).into()
}

/// `{"event": "error", "message": ...}`.
pub fn error(message: &str) -> Value {
    Map::new().with("event", "error").with("message", message).into()
}

/// `{"event": "partial", "language": ..., "sentences": [...]}`.
pub fn partial(language: &str, sentences: &[Sentence]) -> Value {
    Map::new().with("event", "partial").with("language", language).with("sentences", sentences_to_value(sentences)).into()
}

/// `{"event": "result", "language", "duration", "audio", "outputs", "sentences"}`.
///
/// `duration` is always a float (`0.0` for `extract-audio`); `audio`/`outputs` are Python `str(path)` texts.
pub fn result(language: &str, duration: f64, audio: &str, outputs: &[String], sentences: &[Sentence]) -> Value {
    Map::new()
        .with("event", "result")
        .with("language", language)
        .with("duration", duration)
        .with("audio", audio)
        .with("outputs", Value::Array(outputs.iter().map(Value::from).collect()))
        .with("sentences", sentences_to_value(sentences))
        .into()
}

/// `{"event": "translation", "language": ..., "sentences": [...]}`.
pub fn translation(language: &str, sentences: &[Sentence]) -> Value {
    Map::new()
        .with("event", "translation")
        .with("language", language)
        .with("sentences", sentences_to_value(sentences))
        .into()
}

/// `{"event": "models", "models": [rows]}`; each row is built by the models command with the keys
/// `id, kind, backend, languages, size_mb, disk_mb, speed, accuracy, license, downloaded`.
pub fn models(rows: Vec<Value>) -> Value {
    Map::new().with("event", "models").with("models", Value::Array(rows)).into()
}

/// `{"event": "deleted", "id": ..., "freed_mb": n}`.
pub fn deleted(id: &str, freed_mb: i64) -> Value {
    Map::new().with("event", "deleted").with("id", id).with("freed_mb", freed_mb).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delete_model_json_event_bytes() {
        let mut out = Vec::new();
        emit(&mut out, &deleted("base", 123)).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "{\"event\": \"deleted\", \"id\": \"base\", \"freed_mb\": 123}\n");
    }

    #[test]
    fn test_stage_and_result_bytes() {
        assert_eq!(json::dumps(&stage("transcribing", Some(0.0)), false), r#"{"event": "stage", "stage": "transcribing", "fraction": 0.0}"#);
        assert_eq!(
            json::dumps(&result("?", 0.0, "C:\\o\\a-12345678.wav", &[], &[]), false),
            r#"{"event": "result", "language": "?", "duration": 0.0, "audio": "C:\\o\\a-12345678.wav", "outputs": [], "sentences": []}"#
        );
        assert_eq!(json::dumps(&error("Video bulunamadı: x"), false), r#"{"event": "error", "message": "Video bulunamadı: x"}"#);
    }
}
