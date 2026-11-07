//! Port of `engine/tests/test_onnx_slow.py` (`pytest -m slow`): end-to-end runs with the real onnx-asr models.
//! Run with `cargo test -p se-asr --test onnx_slow -- --ignored`; needs the models in the Hugging Face cache and
//! the sample videos in `<repo>/samples` (a missing sample skips its case, as in Python).
//!
//! `pipeline.run` lives in se-cli (downstream of this crate), so the test performs the same steps the pipeline
//! does for these cases: extract the 16 kHz WAV, call the transcriber factory, segment the words and write
//! `<stem>.srt`.

use std::path::{Path, PathBuf};

use se_asr::create_transcriber;
use se_text::sentences::{SegmenterConfig, segment};

const CASES: [(&str, &str, &str); 5] = [
    ("demo-pt.mp4", "parakeet-v3", "pt"),
    ("demo-es.mp4", "parakeet-v3", "es"),
    ("demo-ru.mp4", "parakeet-v3", "ru"),
    ("demo-ru.mp4", "gigaam-v3", "ru"),
    ("demo-pt.mp4", "canary-1b-v2", "pt"),
];

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../samples")
}

fn run_case(sample: &str, model: &str, language: &str) {
    let video = samples().join(sample);
    if !video.exists() {
        eprintln!("skipped: samples/{sample} missing (sample videos not generated)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let stem = video.file_stem().unwrap().to_string_lossy().to_string();
    let wav = tmp.path().join(format!("{stem}.wav"));
    se_audio::audio::extract_audio(&video, &wav, None).unwrap();

    let mut transcriber = create_transcriber(model).unwrap();
    let transcript = transcriber.transcribe(&wav, Some(language), None, None).unwrap();
    let sentences = segment(&transcript.words, &SegmenterConfig::default(), None).unwrap();
    se_text::subtitles::write(&sentences, &tmp.path().join(format!("{stem}.srt")), "srt").unwrap();

    assert_eq!(transcript.language, language);
    assert!(sentences.len() > 1, "{model}/{sample}: {} sentences", sentences.len());
    let words: Vec<&se_core::Word> = sentences.iter().flat_map(|s| s.words.iter()).collect();
    assert!(words.len() > 5);
    assert!(
        words.iter().all(|w| 0.0 <= w.start_f() && w.start_f() <= w.end_f() && w.end_f() <= transcript.duration + 0.01)
    );
    assert!(words.windows(2).all(|p| p[0].start_f() <= p[1].start_f()));
    assert!(tmp.path().join(format!("{stem}.srt")).is_file());
}

#[test]
#[ignore = "slow: real onnx-asr models and sample videos"]
fn test_onnx_model_transcribes_sample() {
    for (sample, model, language) in CASES {
        run_case(sample, model, language);
    }
}
