//! Real-model check of the Whisper port against the Python engine (`-m slow` equivalent):
//! `cargo test -p se-asr --test whisper_slow -- --ignored --nocapture`.
//!
//! Needs Windows (speech is synthesized with System.Speech), the cached `Systran/faster-whisper-base` model,
//! `engine/.venv` (the Python reference, `FasterWhisperTranscriber` with CUDA disabled) and onnxruntime for the
//! VAD (`ORT_DYLIB_PATH`, next to the test executable, or `SUBTITLE_ENGINE_SHERPA_DIR`; defaults to the sherpa-onnx
//! copy in `engine/.venv`). Missing prerequisites skip the test with a message.
//!
//! Bit-exact output is not expected: the int8 CTranslate2/oneDNN build and the f32 log-mel rounding differ slightly
//! from the Python wheel, and Whisper's timestamp decoding amplifies such differences. The test checks the same
//! language and duration, near-identical text and word times within 0.1 s for matching words.

use std::path::{Path, PathBuf};
use std::process::Command;

use se_asr::Transcriber;
use se_asr::whisper::{Device, FasterWhisperTranscriber, WhisperHooks};

const TEXT_EN: &str = "Hello, how are you doing today? I wanted to ask you about the project. I am doing well, thank \
                       you. The project is going fine, we finished the first part yesterday.";
const TEXT_TR: &str = "Merhaba, bu bir altyazı testidir. Sen ne yapacaksın? Bugün hava çok güzel, parka gitmeyi \
                       düşünüyorum.";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn synthesize(out: &Path, voice_culture: &str, text: &str) -> bool {
    let script = format!(
        "Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
         $v = $s.GetInstalledVoices() | ? {{ $_.VoiceInfo.Culture.Name -eq '{voice_culture}' }} | select -First 1; \
         if (-not $v) {{ exit 3 }}; $s.SelectVoice($v.VoiceInfo.Name); \
         $f = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, \
         [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono); \
         $s.SetOutputToWaveFile('{}', $f); $s.Speak('{}'); $s.Dispose()",
        out.display(),
        text.replace('\'', "''")
    );
    // PowerShell 7 (.NET) also sees the OneCore voices (e.g. Microsoft Tolga, tr-TR); Windows PowerShell 5.1 only
    // the Desktop ones.
    ["pwsh.exe", "powershell.exe"].iter().any(|shell| {
        Command::new(shell)
            .args(["-NoProfile", "-Command", &script])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

type Words = Vec<(f64, f64, String)>;

fn python_reference(python: &Path, wav: &Path, language: &str, tmp: &Path) -> (String, f64, Words) {
    let script = "import json, sys\nfrom pathlib import Path\nfrom subtitle_engine import cuda, transcribe as t\n\
                  cuda.cuda_usable = lambda: False\n\
                  r = t.FasterWhisperTranscriber('base').transcribe(Path(sys.argv[1]), language=sys.argv[2])\n\
                  print(json.dumps({'language': r.language, 'duration': r.duration, \
                  'words': [[w.start, w.end, w.text] for w in r.words]}, ensure_ascii=True))";
    let out = Command::new(python)
        .args(["-c", script])
        .arg(wav)
        .arg(language)
        .env("TMPDIR", tmp)
        .output()
        .expect("run python");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v = se_core::json::parse(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let words = v
        .get("words")
        .and_then(|w| w.as_array())
        .unwrap()
        .iter()
        .map(|w| {
            let w = w.as_array().unwrap();
            (w[0].as_f64().unwrap(), w[1].as_f64().unwrap(), w[2].as_str().unwrap().to_string())
        })
        .collect();
    (v.get("language").unwrap().as_str().unwrap().to_string(), v.get("duration").unwrap().as_f64().unwrap(), words)
}

/// Longest-common-subsequence matching of the word texts: (matched pairs, similarity 2*M/(a+b)).
fn match_words<'a>(a: &'a Words, b: &'a Words) -> (Vec<(&'a (f64, f64, String), &'a (f64, f64, String))>, f64) {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i].2 == b[j].2 { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut pairs) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i].2 == b[j].2 {
            pairs.push((&a[i], &b[j]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let sim = if n + m == 0 { 1.0 } else { 2.0 * pairs.len() as f64 / (n + m) as f64 };
    (pairs, sim)
}

#[test]
#[ignore = "slow: real Whisper base model and the Python reference engine"]
fn test_whisper_base_matches_python_engine_on_synthesized_speech() {
    let python = repo().join("engine/.venv/Scripts/python.exe");
    if !cfg!(windows) || !python.is_file() || se_asr::hf::cached_snapshot("Systran/faster-whisper-base").is_none() {
        eprintln!("skipped: needs Windows, engine/.venv and the cached faster-whisper-base model");
        return;
    }
    if std::env::var_os("ORT_DYLIB_PATH").is_none() && std::env::var_os("SUBTITLE_ENGINE_SHERPA_DIR").is_none() {
        // SAFETY: set before any thread of this test binary loads onnxruntime (single test in this file).
        unsafe {
            std::env::set_var(
                "SUBTITLE_ENGINE_SHERPA_DIR",
                repo().join("engine/.venv/Lib/site-packages/sherpa_onnx/lib"),
            )
        };
    }
    let dir = tempfile::tempdir().unwrap();
    for (name, culture, text, language) in [("en.wav", "en-US", TEXT_EN, "en"), ("tr.wav", "tr-TR", TEXT_TR, "tr")] {
        let wav = dir.path().join(name);
        if !synthesize(&wav, culture, text) {
            eprintln!("skipped {name}: no {culture} System.Speech voice");
            continue;
        }
        let (py_lang, py_duration, py_words) = python_reference(&python, &wav, language, dir.path());

        let hooks = WhisperHooks { cuda_usable: Box::new(|| false), ..WhisperHooks::default() };
        let mut t = FasterWhisperTranscriber::with_hooks("base", Device::Auto, None, hooks).unwrap();
        t.checkpoint_root = Some(dir.path().join("ckpt"));
        let mut progress = Vec::new();
        let mut on_progress = |f: f64| progress.push(f);
        let rs = t.transcribe(&wav, None, Some(&mut on_progress), None).unwrap();
        let rs_words: Words = rs.words.iter().map(|w| (w.start_f(), w.end_f(), w.text.clone())).collect();

        let (pairs, sim) = match_words(&py_words, &rs_words);
        let max_dt = pairs.iter().map(|(a, b)| (a.0 - b.0).abs().max((a.1 - b.1).abs())).fold(0.0, f64::max);
        eprintln!(
            "{name}: python {} words, rust {} words, similarity {sim:.3}, max |dt| {max_dt:.3}\n  py: {}\n  rs: {}",
            py_words.len(),
            rs_words.len(),
            py_words.iter().map(|w| w.2.as_str()).collect::<Vec<_>>().join(" "),
            rs_words.iter().map(|w| w.2.as_str()).collect::<Vec<_>>().join(" ")
        );
        // Language detection (no language given to the Rust run) must agree with the forced Python language.
        assert_eq!(rs.language, py_lang);
        assert_eq!(rs.duration, py_duration);
        assert!(sim >= 0.85, "{name}: word similarity {sim}");
        assert!(max_dt <= 0.1, "{name}: matched word times differ by {max_dt}");
        assert!(progress.windows(2).all(|p| p[0] <= p[1]) && progress.last().is_some_and(|&p| p <= 1.0));
        assert!(rs_words.windows(2).all(|p| p[0].0 <= p[1].0));
    }
}
