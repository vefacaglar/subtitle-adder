//! Golden checks against the Python reference (faster-whisper / numpy) when `engine/.venv` exists, and against
//! the cached Whisper `base` tokenizer when it is in the Hugging Face cache. Both skip silently otherwise.

use std::path::{Path, PathBuf};
use std::process::Command;

use se_asr::mel::FeatureExtractor;
use se_asr::tokenizer::{JsonTokenizer, Tokenizer};

fn python() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SE_PYTHON") {
        return Some(PathBuf::from(p));
    }
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../engine/.venv/Scripts/python.exe");
    p.is_file().then_some(p)
}

fn signal(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let t = i as f64;
            ((t * 0.013).sin() * 0.3 + (t * 0.0021).cos() * 0.2 + ((i * 7919) % 1000) as f64 / 20000.0 - 0.025) as f32
        })
        .collect()
}

#[test]
fn test_log_mel_matches_faster_whisper_feature_extractor() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("x.f32");
    let output = dir.path().join("mel.f32");
    let x = signal(16000 * 3 + 123);
    std::fs::write(&input, x.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
    for n_mels in [80usize, 128] {
        let script = format!(
            "import numpy as np\nfrom faster_whisper.feature_extractor import FeatureExtractor\n\
             x = np.fromfile(r'{}', dtype='<f4')\nm = FeatureExtractor(feature_size={n_mels})(x)\n\
             np.ascontiguousarray(m, dtype='<f4').tofile(r'{}')\nprint(m.shape[1])",
            input.display(),
            output.display()
        );
        let out = Command::new(&py).arg("-c").arg(&script).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let frames: usize = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        let reference: Vec<f32> = std::fs::read(&output)
            .unwrap()
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let ours = FeatureExtractor::new(n_mels).compute(&x);
        assert_eq!(ours.frames, frames);
        assert_eq!(ours.data.len(), reference.len());
        let max_diff = ours.data.iter().zip(&reference).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        let exact = ours.data.iter().zip(&reference).filter(|(a, b)| a == b).count();
        eprintln!("n_mels {n_mels}: max |diff| {max_diff:e}, {exact}/{} values bit-identical", reference.len());
        assert!(max_diff < 2e-5, "max diff {max_diff}");
    }
}

fn base_tokenizer() -> Option<JsonTokenizer> {
    let dir = se_asr::hf::cached_snapshot("Systran/faster-whisper-base")?;
    JsonTokenizer::from_file(&dir.join("tokenizer.json")).ok()
}

#[test]
fn test_base_tokenizer_special_ids_and_suppressed_tokens() {
    let Some(hf) = base_tokenizer() else { return };
    let tok = Tokenizer::new(&hf, true, "transcribe", Some("tr")).unwrap();
    assert_eq!((tok.sot, tok.eot, tok.timestamp_begin(), tok.no_speech), (50258, 50257, 50364, 50362));
    assert_eq!(tok.sot_sequence(), vec![50258, 50268, 50359]);
    let expected: Vec<u32> = vec![
        1, 2, 7, 8, 9, 10, 14, 25, 26, 27, 28, 29, 31, 58, 59, 60, 61, 62, 63, 90, 91, 92, 93, 359, 503, 522, 542, 873,
        893, 902, 918, 922, 931, 1350, 1853, 1982, 2460, 2627, 3246, 3253, 3268, 3536, 3846, 3961, 4183, 4667, 6585,
        6647, 7273, 9061, 9383, 10428, 10929, 11938, 12033, 12331, 12562, 13793, 14157, 14635, 15265, 15618, 16553,
        16604, 18362, 18956, 20075, 21675, 22520, 26130, 26161, 26435, 28279, 29464, 31650, 32302, 32470, 36865,
        42863, 47425, 49870, 50254, 50258, 50358, 50359, 50360, 50361, 50362,
    ];
    assert_eq!(tok.suppressed_tokens(), expected);
    let yue = Tokenizer::new(&hf, true, "transcribe", Some("yue")).unwrap();
    assert_eq!(yue.sot_sequence(), vec![50258, 50359]);
    let err = Tokenizer::new(&hf, true, "transcribe", Some("xx")).err().unwrap();
    assert!(err.message.starts_with("'xx' is not a valid language code (accepted language codes: af, am, ar,"));
    assert!(err.message.ends_with("zh, yue)"));
}

#[test]
fn test_word_splitting_matches_faster_whisper() {
    let Some(hf) = base_tokenizer() else { return };
    let Some(py) = python() else { return };
    let tok = Tokenizer::new(&hf, true, "transcribe", Some("en")).unwrap();
    let text = " Hello, world! It's 3.5 \"quoted\" — naïve café 日本語 (test).";
    let mut ids = tok.encode(text);
    ids.push(tok.eot);
    let (words, word_tokens) = tok.split_tokens_on_spaces(&ids);
    let ja = Tokenizer::new(&hf, true, "transcribe", Some("ja")).unwrap();
    let (uwords, _) = ja.split_to_word_tokens(&ids);
    let dir = se_asr::hf::cached_snapshot("Systran/faster-whisper-base").unwrap();
    let script = format!(
        "import json, tokenizers\nfrom faster_whisper.tokenizer import Tokenizer\n\
         hf = tokenizers.Tokenizer.from_file(r'{}')\nids = {:?}\n\
         t = Tokenizer(hf, True, task='transcribe', language='en')\nw, wt = t.split_tokens_on_spaces(ids)\n\
         j = Tokenizer(hf, True, task='transcribe', language='ja')\nu, _ = j.split_to_word_tokens(ids)\n\
         print(json.dumps([w, wt, u]))",
        dir.join("tokenizer.json").display(),
        ids
    );
    let out = Command::new(&py).arg("-c").arg(&script).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let py_words: Vec<String> = serde_json::from_value(v[0].clone()).unwrap();
    let py_tokens: Vec<Vec<u32>> = serde_json::from_value(v[1].clone()).unwrap();
    let py_u: Vec<String> = serde_json::from_value(v[2].clone()).unwrap();
    assert_eq!(words, py_words);
    assert_eq!(word_tokens, py_tokens);
    assert_eq!(uwords, py_u);
}

#[test]
fn test_compression_ratio_matches_python_zlib() {
    let Some(py) = python() else { return };
    let mut texts: Vec<String> = vec![
        String::new(),
        "Hello, how are you doing today?".into(),
        "Merhaba, bu bir altyazı testidir. Sen ne yapacaksın?".into(),
        "Thank you. Thank you. Thank you. Thank you. Thank you. Thank you. Thank you.".into(),
        "la la la la la la la la la la la la la la la la la la la la la la la la".into(),
        "日本語のテキストです。これはテストです。".into(),
    ];
    let words = ["the", "project", "is", "going", "fine", "we", "finished", "first", "part", "yesterday", "ok", "!"];
    for i in 0..400usize {
        let n = 1 + (i * 37) % 90;
        let s: Vec<&str> = (0..n).map(|k| words[(k * k + i * 7 + k / 3) % words.len()]).collect();
        texts.push(s.join(if i % 3 == 0 { " " } else { ", " }));
        if i % 5 == 0 {
            texts.push(s[..n.min(4)].join(" ").repeat(1 + i % 13));
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("texts.json");
    std::fs::write(&input, serde_json::to_string(&texts).unwrap()).unwrap();
    let script = format!(
        "import json, zlib\nt = json.load(open(r'{}', encoding='utf-8'))\n\
         print(json.dumps([len(s.encode()) / len(zlib.compress(s.encode())) for s in t]))",
        input.display()
    );
    let out = Command::new(&py).arg("-c").arg(&script).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // se-core's parser rounds floats exactly like Python (serde_json's default parser can be one ulp off).
    let parsed = se_core::json::parse(std::str::from_utf8(&out.stdout).unwrap().trim()).unwrap();
    let reference: Vec<f64> = parsed.as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    let mismatches: Vec<(&String, f64, f64)> = texts
        .iter()
        .zip(&reference)
        .map(|(t, &r)| (t, se_asr::fw::compression_ratio(t), r))
        .filter(|(_, a, b)| a != b)
        .collect();
    assert!(mismatches.is_empty(), "{} of {} differ, e.g. {:?}", mismatches.len(), texts.len(), mismatches.first());
}
