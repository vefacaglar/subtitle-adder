//! Real sherpa-onnx runs (equivalent of `pytest -m slow`): need the C API library (dev: the Python venv
//! copy, found automatically in debug builds) and the cached `speaker-diarization` model.
//!
//! `test_parity_with_python_engine` compares against a reference produced by the Python engine:
//! set `SE_DIARIZE_PARITY_DIR` to a folder holding `dialog16.wav` (16 kHz mono) and `ref.json` written by
//! `ref_diarize.py` (`SherpaDiarizer().diarize(...)` for several settings + one raw embedding).

mod common;

use std::path::PathBuf;

use se_core::json::Value;
use se_core::{Num, Word};
use se_diarize::ffi::SherpaApi;
use se_diarize::{Diarizer, SherpaDiarizer, SpeakerSegment, assign_speakers, model_files, read_wav};

#[test]
#[ignore = "needs the sherpa-onnx C API library"]
fn test_c_api_library_loads_with_every_symbol() {
    let api = SherpaApi::load().expect("library");
    assert!(api.path.is_file());
}

#[test]
#[ignore = "needs the sherpa-onnx C API library and the cached speaker-diarization model"]
fn test_real_diarizer_runs_on_synthetic_audio() {
    let mut d = SherpaDiarizer::new(None, se_diarize::CLUSTER_THRESHOLD).expect("model cached");
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("tone.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&wav, spec).unwrap();
    for i in 0..16000 * 4 {
        let t = i as f64 / 16000.0;
        w.write_sample(((t * 220.0 * std::f64::consts::TAU).sin() * 8000.0) as i16).unwrap();
    }
    w.finalize().unwrap();
    let mut progress = Vec::new();
    let mut on_progress = |f: f64| progress.push(f);
    let segs = d.diarize(&wav, None, Some(&mut on_progress), Some(&[])).unwrap();
    assert!(segs.iter().all(|s| s.end > s.start));
    assert_eq!(progress.last(), Some(&1.0));
    assert!(progress.windows(2).all(|p| p[1] > p[0]));
}

fn f(v: &Value) -> f64 {
    v.as_f64().expect("number")
}

#[test]
#[ignore = "needs SE_DIARIZE_PARITY_DIR, the sherpa-onnx library and the cached model"]
fn test_parity_with_python_engine() {
    let Some(dir) = std::env::var_os("SE_DIARIZE_PARITY_DIR").map(PathBuf::from) else {
        eprintln!("SE_DIARIZE_PARITY_DIR not set; skipping");
        return;
    };
    let reference = se_core::json::parse(&std::fs::read_to_string(dir.join("ref.json")).unwrap()).unwrap();
    let words: Vec<Word> = reference
        .get("words")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|w| Word {
            start: Num::from_value(w.get("start").unwrap()).unwrap(),
            end: Num::from_value(w.get("end").unwrap()).unwrap(),
            text: w.get("text").and_then(Value::as_str).unwrap().to_string(),
        })
        .collect();
    let wav = dir.join("dialog16.wav");
    assert!(model_files(None).is_ok());
    let mut d = SherpaDiarizer::new(None, se_diarize::CLUSTER_THRESHOLD).unwrap();
    for run in reference.get("runs").and_then(Value::as_array).unwrap() {
        let n = run.get("num_speakers").and_then(Value::as_i64).map(|n| n as u32);
        let use_words = run.get("words").and_then(Value::as_bool).unwrap();
        let mut progress = Vec::new();
        let mut on_progress = |x: f64| progress.push(x);
        let segs = d
            .diarize(&wav, n, Some(&mut on_progress), if use_words { Some(&words) } else { None })
            .unwrap();
        let expected: Vec<SpeakerSegment> = run
            .get("segments")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|s| {
                let s = s.as_array().unwrap();
                SpeakerSegment { start: f(&s[0]), end: f(&s[1]), speaker: s[2].as_i64().unwrap() }
            })
            .collect();
        assert_eq!(segs, expected, "segments for num_speakers={n:?} words={use_words}");
        let exp_progress: Vec<f64> = run.get("progress").and_then(Value::as_array).unwrap().iter().map(f).collect();
        assert_eq!(progress, exp_progress, "progress for num_speakers={n:?}");
        let assigned: Vec<Option<i64>> =
            run.get("assigned").and_then(Value::as_array).unwrap().iter().map(Value::as_i64).collect();
        assert_eq!(assign_speakers(&words, &segs), assigned);
    }

    // One raw embedding through the C API equals the Python binding's.
    let samples = read_wav(&wav).unwrap();
    let expected: Vec<f64> = reference.get("embedding").and_then(Value::as_array).unwrap().iter().map(f).collect();
    let got = d.embed(&samples[16000..48000]).unwrap();
    assert_eq!(got.len(), expected.len());
    let max_diff = got.iter().zip(&expected).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
    assert!(max_diff < 1e-4, "embedding differs by {max_diff}");
}
