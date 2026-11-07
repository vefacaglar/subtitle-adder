//! Dev tool: print faster-whisper-style segments (start, end, temperature, avg_logprob, no_speech_prob, tokens,
//! text) for parity checks against `faster_whisper.WhisperModel.transcribe` with the engine options.
//!
//! `cargo run --release -p se-asr --example fw_segments -- <wav> <model> [language|-]`

use std::path::Path;

use se_asr::fw::{AudioInput, WhisperModel};
use se_asr::whisper::{Device, FasterWhisperTranscriber, engine_options};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let wav = Path::new(&args[1]);
    let language = args.get(3).filter(|l| l.as_str() != "-").cloned();
    let dir = FasterWhisperTranscriber::resolve_model_dir(&args[2]).expect("model dir");
    let model = WhisperModel::load(&dir, Device::Cpu, "int8").expect("load");
    let (info, segments) = model.start(AudioInput::Path(wav), language.as_deref(), &engine_options()).expect("start");
    println!("language {} duration {} after_vad {}", info.language, info.duration, info.duration_after_vad);
    for s in segments {
        let s = s.expect("segment");
        println!(
            "{} {} {} T={} lp={:.6} ns={:.6} cr={:.6} {:?} {:?}",
            s.seek, s.start.v, s.end.v, s.temperature, s.avg_logprob, s.no_speech_prob, s.compression_ratio, s.tokens,
            s.text
        );
    }
}
