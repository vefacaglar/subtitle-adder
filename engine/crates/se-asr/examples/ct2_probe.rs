//! Dev tool: one greedy/beam `generate` on the first 30 s window of a WAV (no VAD), to compare CTranslate2 output
//! with the Python wheel on identical features. Writes the window to `<wav>.feat.f32`.
//!
//! `cargo run --release -p se-asr --example ct2_probe -- <wav> <model> <language> [features.f32]`

use std::path::Path;

use ct2rs::sys::{Config, Device, StorageView, Whisper, WhisperOptions};
use se_asr::mel::FeatureExtractor;
use se_asr::tokenizer::{HfTokenizer, JsonTokenizer, Tokenizer};
use se_asr::whisper::FasterWhisperTranscriber;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let wav = Path::new(&args[1]);
    let dir = FasterWhisperTranscriber::resolve_model_dir(&args[2]).unwrap();
    let audio = se_asr::fw::decode_audio(wav).unwrap();
    let fe = FeatureExtractor::new(80);
    let feats = fe.compute(&audio);
    let mut window = match args.get(4) {
        // A [80, T] feature file (e.g. from vad_dump) instead of the WAV features.
        Some(file) => {
            let bytes = std::fs::read(file).unwrap();
            let data: Vec<f32> = bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
            let frames = data.len() / 80;
            let f = se_asr::mel::Features { n_mels: 80, frames, data };
            f.window(0, 3000.min(f.content_frames()))
        }
        None => feats.window(0, 3000.min(feats.content_frames())),
    };
    std::fs::write(
        format!("{}.feat.f32", wav.display()),
        window.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(),
    )
    .unwrap();
    let config = Config { compute_type: ct2rs::sys::ComputeType::INT8, ..Config::default() };
    println!("config {config:?}");
    let model = Whisper::new(&dir, config).unwrap();
    let hf = JsonTokenizer::from_file(&dir.join("tokenizer.json")).unwrap();
    let tok = Tokenizer::new(&hf, true, "transcribe", Some(&args[3])).unwrap();
    let prompt: Vec<String> = tok.sot_sequence().iter().map(|&t| hf.id_to_token(t).unwrap()).collect();
    let sv = StorageView::new(&[1, 80, 3000], &mut window, Device::CPU).unwrap();
    let t0 = std::time::Instant::now();
    let enc = model.encode(&sv, false).unwrap();
    println!("encode {:.2}s", t0.elapsed().as_secs_f64());
    let opts = WhisperOptions {
        return_scores: true,
        return_no_speech_prob: true,
        suppress_tokens: tok.suppressed_tokens().into_iter().map(|t| t as i32).collect(),
        ..WhisperOptions::default()
    };
    let t0 = std::time::Instant::now();
    let r = model.generate(&enc, &[prompt], &opts).unwrap();
    println!("generate {:.2}s", t0.elapsed().as_secs_f64());
    println!("ns={} score={:?} {:?}", r[0].no_speech_prob, r[0].scores, r[0].sequences_ids[0]);
}
