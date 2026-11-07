//! `[translation] {"event": ...}` diagnostic lines on stderr (spec translate-llm.md §9). They never carry
//! prompts, responses or keys.

use std::io::Write;
use std::sync::Arc;

use se_core::{Map, Value, json};

/// A diagnostics sink (Python tests monkeypatch `_translation_diagnostic`): `(event, fields)`. It is called from
/// the request heartbeat thread too, so it must be `Send + Sync`.
pub type DiagSink = Arc<dyn Fn(&str, &Map) + Send + Sync>;

/// The diagnostic line without the trailing newline: `[translation] ` + `json.dumps({"event": event, **fields})`
/// (ensure_ascii=True, default separators).
pub fn format_line(event: &str, fields: &Map) -> String {
    let mut map = Map::new();
    map.insert("event", event);
    for (k, v) in fields.iter() {
        map.insert(k.clone(), v.clone());
    }
    format!("[translation] {}", json::dumps(&Value::Object(map), true))
}

/// `_translation_diagnostic(event, **fields)`: writes `[translation] ` + `json.dumps({"event": event, **fields})`
/// (ensure_ascii=True, default separators) + `\n` to stderr.
pub fn translation_diagnostic(event: &str, fields: Map) {
    write_stderr(event, &fields);
}

fn write_stderr(event: &str, fields: &Map) {
    let line = format_line(event, fields) + "\n";
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    // contextlib.suppress(OSError)
    let _ = lock.write_all(line.as_bytes());
    let _ = lock.flush();
}

/// The default sink (stderr).
pub fn stderr_sink() -> DiagSink {
    Arc::new(|event: &str, fields: &Map| write_stderr(event, fields))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_format_matches_python_json_dumps() {
        let fields = Map::new().with("request", 1).with("host", Value::Null).with("model", "çay").with("seconds", 15.0);
        assert_eq!(
            format_line("request_start", &fields),
            concat!(r#"[translation] {"event": "request_start", "request": 1, "host": null, "model": ""#, "\\u00e7ay", r#"", "seconds": 15.0}"#)
        );
    }
}
