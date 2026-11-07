//! Dev tool: run the Rust segment generator on features and VAD speeches computed by the Python reference
//! (`faster_whisper`), so that segment splitting, word alignment and timestamp restoration can be compared with
//! the Python engine without the (non bit-exact) feature extractor. Prints the `asr_words` JSON shape.
//!
//! `cargo run --release -p se-asr --example fw_inject -- <features.f32> <n_mels> <speeches.json> <duration> <model> [language|-]`

use std::path::Path;

use se_asr::fw::WhisperModel;
use se_asr::fw_vad::Speech;
use se_asr::mel::Features;
use se_asr::whisper::{Device, FasterWhisperTranscriber, engine_options, words_of};
use se_core::json::{Value, dumps};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).unwrap();
    let n_mels: usize = args[2].parse().unwrap();
    let data: Vec<f32> = bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let features = Features { n_mels, frames: data.len() / n_mels, data };
    let sp = se_core::json::parse(&std::fs::read_to_string(&args[3]).unwrap()).unwrap();
    let speeches: Vec<Speech> = sp
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let p = p.as_array().unwrap();
            Speech { start: p[0].as_i64().unwrap(), end: p[1].as_i64().unwrap() }
        })
        .collect();
    let duration: f64 = args[4].parse().unwrap();
    let language = args.get(6).filter(|l| l.as_str() != "-").cloned();
    let dir = FasterWhisperTranscriber::resolve_model_dir(&args[5]).unwrap();
    let model = WhisperModel::load(Path::new(&dir), Device::Cpu, "int8").unwrap();
    let after_vad = (features.frames - 1) as f64 * 0.01;
    let (info, segments) =
        model.start_features(features, speeches, duration, after_vad, language.as_deref(), &engine_options()).unwrap();
    let mut words = Vec::new();
    let mut progress = Vec::new();
    let mut reported = 0.0;
    for s in segments {
        let s = s.unwrap();
        words.extend(words_of(&s, 0.0));
        let f = (s.end.v / info.duration).min(1.0);
        if f >= reported {
            reported = f;
            progress.push(Value::from(f));
        }
    }
    let words: Vec<Value> = words
        .iter()
        .map(|w| Value::Array(vec![w.start.to_value(), w.end.to_value(), Value::from(w.text.as_str())]))
        .collect();
    let out = se_core::Map::new()
        .with("language", info.language.as_str())
        .with("duration", info.duration)
        .with("progress", Value::Array(progress))
        .with("words", Value::Array(words));
    println!("{}", dumps(&Value::Object(out), false));
}
