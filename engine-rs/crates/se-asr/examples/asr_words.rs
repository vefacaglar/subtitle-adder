//! Dev tool: transcribe a WAV and print `{"language", "duration", "progress", "words"}` as JSON (Python repr floats),
//! the same shape as the parity script that drives the Python engine.
//!
//! `cargo run -p se-asr --example asr_words -- <wav> <model> [language|-] [chunk_seconds]`

use std::path::Path;

use se_asr::Transcriber;
use se_asr::whisper::{Device, FasterWhisperTranscriber, WhisperHooks};
use se_core::json::{Value, dumps};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let wav = Path::new(&args[1]);
    let model = &args[2];
    let language = args.get(3).filter(|l| l.as_str() != "-").cloned();
    let chunk = args.get(4).and_then(|c| c.parse::<f64>().ok());
    let mut transcriber: Box<dyn Transcriber> = if se_asr::onnx_backend_for(model).is_some() {
        se_asr::create_transcriber(model).expect("load")
    } else {
        let hooks = WhisperHooks {
            cuda_usable: Box::new(|| false),
            register_libraries: Box::new(|| {}),
            is_downloaded: Box::new(|_| true),
            ..WhisperHooks::default()
        };
        let mut t = FasterWhisperTranscriber::with_hooks(model, Device::Auto, None, hooks).expect("load");
        if let Some(c) = chunk {
            t.chunks.chunk_seconds = c;
        }
        Box::new(t)
    };
    let mut progress: Vec<f64> = Vec::new();
    let mut on_progress = |f: f64| progress.push(f);
    let started = std::time::Instant::now();
    let result = transcriber.transcribe(wav, language.as_deref(), Some(&mut on_progress), None);
    let t = match result {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("transcribed in {:.1}s", started.elapsed().as_secs_f64());
    let words: Vec<Value> = t
        .words
        .iter()
        .map(|w| Value::Array(vec![w.start.to_value(), w.end.to_value(), Value::from(w.text.as_str())]))
        .collect();
    let out = se_core::Map::new()
        .with("language", t.language.as_str())
        .with("duration", t.duration)
        .with("progress", Value::Array(progress.into_iter().map(Value::from).collect()))
        .with("words", Value::Array(words));
    println!("{}", dumps(&Value::Object(out), false));
}
