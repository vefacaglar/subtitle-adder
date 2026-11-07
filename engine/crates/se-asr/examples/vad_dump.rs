//! Dev tool: print faster-whisper VAD speech timestamps (engine options) for a WAV and write the log-mel features
//! of the VAD-concatenated audio to `<wav>.vadfeat.f32` ([n_mels, T] row-major) for parity checks.
//!
//! `cargo run --release -p se-asr --example vad_dump -- <wav> [n_mels]`

use std::path::Path;

use se_asr::fw_vad::{collect_chunks, get_speech_timestamps};
use se_asr::mel::FeatureExtractor;
use se_asr::whisper::WHISPER_VAD_OPTIONS;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let wav = Path::new(&args[1]);
    let n_mels: usize = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(80);
    let audio = se_asr::fw::decode_audio(wav).unwrap();
    let speeches = get_speech_timestamps(&audio, &WHISPER_VAD_OPTIONS).unwrap();
    let pairs: Vec<String> = speeches.iter().map(|s| format!("({}, {})", s.start, s.end)).collect();
    println!("[{}]", pairs.join(", "));
    let chunk = collect_chunks(&audio, &speeches);
    let feats = FeatureExtractor::new(n_mels).compute(&chunk);
    println!("frames {}", feats.frames);
    std::fs::write(
        format!("{}.vadfeat.f32", wav.display()),
        feats.data.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(),
    )
    .unwrap();
}
