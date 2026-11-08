//! Engine events and line decoding (mac-enginekit §1.5–1.6, win-enginekit §4.6, UNIFIED.md §3.7 FIX-W).

use serde_json::{Map, Value};

use sa_core::engine_types::{EngineStage, Sentence};

/// Payload of the `result` event (all fields required; extra keys ignored).
#[derive(Debug, Clone, PartialEq)]
pub struct EngineResult {
    pub language: String,
    pub duration: f64,
    pub audio: String,
    pub outputs: Vec<String>,
    pub sentences: Vec<Sentence>,
}

/// A decoded stdout event. Equality compares sentences by content (uid excluded).
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Stage { stage: EngineStage, fraction: Option<f64> },
    /// Full snapshot while transcribing; `language` is `"?"` until known.
    Partial { language: String, sentences: Vec<Sentence> },
    Result(EngineResult),
    /// Incremental growing prefix; the last one is complete.
    Translation { language: String, sentences: Vec<Sentence> },
    Error { message: String },
}

/// A malformed line (fatal for the stream: the engine is terminated).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DecodeError(pub String);

fn err(msg: impl Into<String>) -> DecodeError {
    DecodeError(msg.into())
}

fn required<'a>(obj: &'a Map<String, Value>, key: &str, event: &str) -> Result<&'a Value, DecodeError> {
    match obj.get(key) {
        Some(Value::Null) | None => Err(err(format!("{event}: {key} missing"))),
        Some(v) => Ok(v),
    }
}

fn required_str(obj: &Map<String, Value>, key: &str, event: &str) -> Result<String, DecodeError> {
    required(obj, key, event)?.as_str().map(str::to_owned).ok_or_else(|| err(format!("{event}: {key} is not a string")))
}

fn required_f64(obj: &Map<String, Value>, key: &str, event: &str) -> Result<f64, DecodeError> {
    required(obj, key, event)?.as_f64().ok_or_else(|| err(format!("{event}: {key} is not a number")))
}

fn sentences(obj: &Map<String, Value>, event: &str) -> Result<Vec<Sentence>, DecodeError> {
    let v = required(obj, "sentences", event)?;
    if !v.is_array() {
        return Err(err(format!("{event}: sentences is not an array")));
    }
    serde_json::from_value(v.clone()).map_err(|e| err(format!("{event}: {e}")))
}

/// Decodes one stdout line.
///
/// * not JSON / no string `event` → `Err` (fatal);
/// * `stage` (required string `stage`, optional `fraction`), `partial`/`translation` (required `language` and
///   `sentences`), `result` (all fields), `error` (required `message`) → `Ok(Some(event))`, missing fields → `Err`;
/// * any other event name (`models`, `deleted`, `heartbeat`, …) → `Ok(None)`: skipped on **both** platforms
///   (CLAUDE.md "Clients must skip event types they do not know").
pub fn decode(line: &str) -> Result<Option<EngineEvent>, DecodeError> {
    let value: Value = serde_json::from_str(line).map_err(|e| err(e.to_string()))?;
    let Value::Object(obj) = value else { return Err(err("event missing")) };
    let Some(Value::String(event)) = obj.get("event") else { return Err(err("event missing")) };
    let event = event.as_str();
    Ok(Some(match event {
        "stage" => {
            let stage = required_str(&obj, "stage", event)?;
            let fraction = match obj.get("fraction") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_f64().ok_or_else(|| err("stage: fraction is not a number"))?),
            };
            EngineEvent::Stage { stage: EngineStage::parse(&stage), fraction }
        }
        "partial" => EngineEvent::Partial { language: required_str(&obj, "language", event)?, sentences: sentences(&obj, event)? },
        "translation" => {
            EngineEvent::Translation { language: required_str(&obj, "language", event)?, sentences: sentences(&obj, event)? }
        }
        "result" => {
            let outputs = required(&obj, "outputs", event)?;
            let outputs: Vec<String> = serde_json::from_value(outputs.clone()).map_err(|e| err(format!("result: {e}")))?;
            EngineEvent::Result(EngineResult {
                language: required_str(&obj, "language", event)?,
                duration: required_f64(&obj, "duration", event)?,
                audio: required_str(&obj, "audio", event)?,
                outputs,
                sentences: sentences(&obj, event)?,
            })
        }
        "error" => EngineEvent::Error { message: required_str(&obj, "message", event)? },
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sa_core::engine_types::Word;

    fn some(line: &str) -> EngineEvent {
        decode(line).expect("decodes").expect("known event")
    }

    #[test]
    fn decodes_stage_events() {
        assert_eq!(
            some(r#"{"event": "stage", "stage": "transcribing", "fraction": 0.5}"#),
            EngineEvent::Stage { stage: EngineStage::Transcribing, fraction: Some(0.5) }
        );
        assert_eq!(
            some(r#"{"event": "stage", "stage": "yeni_asama", "fraction": null}"#),
            EngineEvent::Stage { stage: EngineStage::Unknown, fraction: None }
        );
    }

    #[test]
    fn decodes_downloading_model_and_translating_stages() {
        assert_eq!(
            some(r#"{"event": "stage", "stage": "downloading_model", "fraction": 0.37}"#),
            EngineEvent::Stage { stage: EngineStage::DownloadingModel, fraction: Some(0.37) }
        );
        assert_eq!(
            some(r#"{"event": "stage", "stage": "translating", "fraction": 0.42}"#),
            EngineEvent::Stage { stage: EngineStage::Translating, fraction: Some(0.42) }
        );
        assert_eq!(EngineStage::parse("translating"), EngineStage::Translating);
    }

    #[test]
    fn stage_without_fraction_decodes_as_indeterminate() {
        assert_eq!(
            some(r#"{"event": "stage", "stage": "segmenting"}"#),
            EngineEvent::Stage { stage: EngineStage::Segmenting, fraction: None }
        );
        assert_eq!(
            some(r#"{"event": "stage", "stage": "diarizing", "fraction": null}"#),
            EngineEvent::Stage { stage: EngineStage::Diarizing, fraction: None }
        );
    }

    #[test]
    fn unknown_events_are_ignored_not_fatal() {
        assert_eq!(decode(r#"{"event": "heartbeat"}"#), Ok(None));
        assert_eq!(decode(r#"{"event": "mystery"}"#), Ok(None));
        assert_eq!(decode(r#"{"event": "models", "models": []}"#), Ok(None));
        assert!(decode("not json").is_err());
        assert!(decode(r#"{"event": "partial", "language": "tr"}"#).is_err());
    }

    #[test]
    fn a_line_without_a_string_event_is_an_error() {
        for line in [r#"{"stage":"transcribing"}"#, "[1]", "42", r#"{"event":7}"#] {
            assert!(decode(line).is_err(), "{line}");
        }
    }

    #[test]
    fn missing_required_fields_are_errors() {
        assert!(decode(r#"{"event": "stage"}"#).is_err());
        assert!(decode(r#"{"event": "stage", "stage": null}"#).is_err());
        assert!(decode(r#"{"event": "error"}"#).is_err());
        assert!(decode(r#"{"event": "translation", "sentences": []}"#).is_err());
        assert!(decode(r#"{"event": "result", "language": "tr", "duration": 1, "audio": "a", "outputs": []}"#).is_err());
    }

    #[test]
    fn decodes_result_and_error() {
        let line = r#"{"event": "result", "language": "tr", "duration": 3.5, "audio": "/tmp/a.wav", "outputs": ["/tmp/a.srt"], "sentences": [{"id": 1, "start": 0.0, "end": 1.2, "text": "Merhaba.", "words": [{"start": 0.0, "end": 1.2, "text": "Merhaba."}], "speaker": null}], "extra": 1}"#;
        let EngineEvent::Result(r) = some(line) else { panic!("result expected") };
        assert_eq!(r.language, "tr");
        assert_eq!(r.duration, 3.5);
        assert_eq!(r.audio, "/tmp/a.wav");
        assert_eq!(r.outputs, vec!["/tmp/a.srt"]);
        assert_eq!(r.sentences.len(), 1);
        assert_eq!(r.sentences[0].text, "Merhaba.");
        assert_eq!(r.sentences[0].words, vec![Word::new(0.0, 1.2, "Merhaba.")]);
        assert_eq!(r.sentences[0].speaker, None);
        assert_eq!(some(r#"{"event": "error", "message": "olmadı"}"#), EngineEvent::Error { message: "olmadı".into() });
    }

    #[test]
    fn decodes_the_extract_audio_result_without_sentences() {
        let EngineEvent::Result(r) =
            some(r#"{"event": "result", "language": "?", "duration": 0, "audio": "/tmp/out/a-1234abcd.wav", "outputs": [], "sentences": []}"#)
        else {
            panic!()
        };
        assert_eq!(r.audio, "/tmp/out/a-1234abcd.wav");
        assert_eq!(r.language, "?");
        assert_eq!(r.duration, 0.0);
        assert!(r.outputs.is_empty() && r.sentences.is_empty());
    }

    #[test]
    fn decodes_translation_event() {
        assert_eq!(
            some(r#"{"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Hola.", "words": []}]}"#),
            EngineEvent::Translation { language: "es".into(), sentences: vec![Sentence::new(1, 0.0, 1.0, "Hola.")] }
        );
    }

    #[test]
    fn decodes_partial_event() {
        let EngineEvent::Partial { language, sentences } = some(
            r#"{"event": "partial", "language": "tr", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Mer", "speaker": null, "words": [{"start": 0, "end": 1, "text": "Mer"}]}]}"#,
        ) else {
            panic!()
        };
        assert_eq!(language, "tr");
        assert_eq!(sentences[0].words.len(), 1);
        assert_eq!(
            some(r#"{"event": "partial", "language": "?", "sentences": []}"#),
            EngineEvent::Partial { language: "?".into(), sentences: vec![] }
        );
    }

    #[test]
    fn decodes_speaker_in_result_and_translation_events() {
        let EngineEvent::Result(r) = some(
            r#"{"event": "result", "language": "en", "duration": 1, "audio": "a", "outputs": [], "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Hi.", "words": [], "speaker": 0}]}"#,
        ) else {
            panic!()
        };
        assert_eq!(r.sentences[0].speaker, Some(0));
        let EngineEvent::Translation { sentences, .. } = some(
            r#"{"event": "translation", "language": "tr", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Selam.", "words": [], "speaker": 1}]}"#,
        ) else {
            panic!()
        };
        assert_eq!(sentences[0].speaker, Some(1));
    }

    #[test]
    fn the_review_flag_survives_events() {
        let EngineEvent::Translation { sentences, .. } = some(
            r#"{"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "x", "words": [], "needs_review": true}, {"id": 2, "start": 1, "end": 2, "text": "y", "words": [], "needs_review": "yes"}]}"#,
        ) else {
            panic!()
        };
        assert!(sentences[0].needs_review);
        assert!(!sentences[1].needs_review);
    }
}
