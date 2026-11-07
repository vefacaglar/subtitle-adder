//! Manual differential probe (ignored): translates `LLM_PROBE_INPUT` (sentence JSON) through
//! `LLM_PROBE_URL` with a real `LlamaTranslator` and writes the diagnostics plus the result or error to
//! `LLM_PROBE_OUT`, in the same shape as the Python side of the comparison script.

use std::sync::{Arc, Mutex};

use se_core::{Map, Sentence, Value, json};
use se_translate::{LlamaTranslator, LlmOptions, Translator};

#[test]
#[ignore]
fn llm_probe() {
    let url = std::env::var("LLM_PROBE_URL").unwrap();
    let input = std::fs::read_to_string(std::env::var("LLM_PROBE_INPUT").unwrap()).unwrap();
    let out = std::env::var("LLM_PROBE_OUT").unwrap();
    let key = std::env::var("LLM_PROBE_KEY").ok();
    let sentences: Vec<Sentence> =
        json::parse(&input).unwrap().as_array().unwrap().iter().map(|v| Sentence::from_value(v).unwrap()).collect();
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink_lines = lines.clone();
    let options = LlmOptions {
        external_url: Some(url),
        external_model: Some(String::new()),
        external_api_key: Some(key.unwrap_or_default()),
        diagnostics: Some(Arc::new(move |event: &str, fields: &Map| {
            let mut line = se_translate::diag::format_line(event, fields);
            if let Some(i) = line.find("\"seconds\": ") {
                let end = line[i + 11..].find([',', '}']).map_or(line.len(), |e| i + 11 + e);
                line.replace_range(i + 11..end, "X");
            }
            sink_lines.lock().unwrap().push(line);
        })),
        ..LlmOptions::default()
    };
    let result = LlamaTranslator::new("qwen3.5-4b", options)
        .and_then(|mut t| t.translate(&sentences, "tr", Some("en"), None, None));
    let tail = match result {
        Ok(sents) => json::dumps(&Value::Array(sents.iter().map(Sentence::to_value).collect()), false),
        Err(e) => format!("ERROR {e}"),
    };
    let mut text = lines.lock().unwrap().join("\n");
    text.push('\n');
    text.push_str(&tail);
    std::fs::write(out, text).unwrap();
}
