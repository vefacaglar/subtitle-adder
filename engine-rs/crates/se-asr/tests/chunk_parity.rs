//! Behavioral parity of the pure parts of `transcribe.py` against the Python reference engine (`engine/.venv`):
//! `_find_silence_pause_cut`, `_iter_wav_chunks`, `_transcribe_checkpoint_path` (file name), `words_from_tokens`
//! and the checkpoint JSON. Skips silently when the venv is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use se_asr::chunks::{self, ChunkConfig};
use se_asr::wave::write_pcm16;

fn python() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SE_PYTHON") {
        return Some(PathBuf::from(p));
    }
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../engine/.venv/Scripts/python.exe");
    p.is_file().then_some(p)
}

fn run_py(py: &Path, script: &str, tmp: &Path) -> String {
    let file = tmp.join("script.py");
    std::fs::write(&file, script).unwrap();
    let out = Command::new(py)
        .arg(&file)
        .env("TMP", tmp)
        .env("TEMP", tmp)
        .env("TMPDIR", tmp)
        .env("PYTHONIOENCODING", "utf-8")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n")
}

/// Deterministic LCG.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn range(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Speech-like bursts separated by pauses of varying length and noise floor.
fn synth(rng: &mut Rng, seconds: f64, rate: u32) -> Vec<i16> {
    let total = (seconds * rate as f64) as usize;
    let mut out = Vec::with_capacity(total);
    while out.len() < total {
        let burst = (rate as u64 / 4 + rng.range(rate as u64 * 6)) as usize;
        let amp = 2000 + rng.range(12000) as i64;
        for _ in 0..burst {
            out.push((rng.range(2 * amp as u64) as i64 - amp) as i16);
        }
        let pause = (rng.range(rate as u64 * 7 / 10)) as usize;
        let floor = [0i64, 30, 200, 600][rng.range(4) as usize];
        for _ in 0..pause {
            out.push((rng.range(2 * floor as u64 + 1) as i64 - floor) as i16);
        }
    }
    out.truncate(total);
    out
}

#[test]
fn test_find_silence_pause_cut_matches_python() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut rng = Rng(7);
    let mut cases = Vec::new();
    let mut script = String::from(
        "import numpy as np\nfrom subtitle_engine.transcribe import _find_silence_pause_cut as f\n",
    );
    for i in 0..60 {
        let rate = [16000u32, 8000, 11025, 3][rng.range(4) as usize];
        let secs = [0.2, 0.71, 1.0, 3.3, 9.0][rng.range(5) as usize];
        let buf = synth(&mut rng, secs, rate.max(100));
        let tail = rng.range(buf.len() as u64 + 2000) as usize;
        let path = dir.path().join(format!("b{i}.i16"));
        std::fs::write(&path, buf.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
        script.push_str(&format!(
            "c, k = f(np.fromfile(r'{}', dtype='<i2'), {rate}, {tail}); print(c, k)\n",
            path.display()
        ));
        cases.push((buf, rate, tail));
    }
    let out = run_py(&py, &script, dir.path());
    for ((buf, rate, tail), line) in cases.iter().zip(out.lines()) {
        let (cut, clean) = chunks::find_silence_pause_cut(buf, *rate, *tail);
        let ours = format!("{cut} {}", if clean { "True" } else { "False" });
        assert_eq!(ours, line, "rate {rate} tail {tail} len {}", buf.len());
    }
    assert_eq!(out.lines().count(), cases.len());
}

#[test]
fn test_iter_wav_chunks_and_checkpoint_name_match_python() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut rng = Rng(11);
    let configs = [
        ChunkConfig { chunk_seconds: 4.0, max_extension_seconds: 3.0, search_tail_seconds: 1.5 },
        ChunkConfig { chunk_seconds: 2.5, max_extension_seconds: 0.5, search_tail_seconds: 4.0 },
        ChunkConfig { chunk_seconds: 6.0, max_extension_seconds: 10.0, search_tail_seconds: 2.0 },
        ChunkConfig { chunk_seconds: 1.0, max_extension_seconds: 0.0, search_tail_seconds: 0.3 },
    ];
    let mut script = String::from(
        "import json\nfrom pathlib import Path\nimport subtitle_engine.transcribe as t\n",
    );
    let mut cases = Vec::new();
    for i in 0..12 {
        let rate = [16000u32, 8000][rng.range(2) as usize];
        let secs = 3.0 + rng.range(25) as f64;
        let samples = synth(&mut rng, secs, rate);
        let name = format!("vid é{i}-abcd.wav");
        let path = dir.path().join(&name);
        write_pcm16(&path, rate, 1, &samples).unwrap();
        let cfg = configs[i % configs.len()];
        script.push_str(&format!(
            "t.TRANSCRIBE_CHUNK_SECONDS = {}\nt.MAX_CHUNK_EXTENSION_SECONDS = {}\nt.CHUNK_SEARCH_TAIL_SECONDS = {}\n\
             p = Path(r'{}')\n\
             print(t._can_stream_wav_chunks(p), json.dumps([[i, o, int(s.size), d] for i, o, s, d in t._iter_wav_chunks(p)]))\n\
             print(t._transcribe_checkpoint_path(p, 'base', {}).name)\n",
            cfg.chunk_seconds,
            cfg.max_extension_seconds,
            cfg.search_tail_seconds,
            path.display(),
            ["None", "'tr'", "''"][i % 3],
        ));
        cases.push((path, cfg, [None, Some("tr"), Some("")][i % 3]));
    }
    let out = run_py(&py, &script, dir.path());
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), cases.len() * 2);
    for (k, (path, cfg, lang)) in cases.iter().enumerate() {
        let reader = chunks::WavChunks::open(path, cfg).unwrap();
        let dur = reader.total_duration();
        let items: Vec<String> = reader
            .map(|c| {
                let c = c.unwrap();
                format!(
                    "[{}, {}, {}, {}]",
                    c.index,
                    se_core::pyfmt::float_repr(c.offset_seconds),
                    c.samples.len(),
                    se_core::pyfmt::float_repr(dur)
                )
            })
            .collect();
        let stream = if chunks::can_stream_wav_chunks(path, cfg, None) { "True" } else { "False" };
        assert_eq!(format!("{stream} [{}]", items.join(", ")), lines[2 * k], "{}", path.display());
        let root = dir.path().join("SubtitleAdder").join("transcriptions");
        let ckpt = chunks::checkpoint_path(&root, path, "base", *lang).unwrap();
        assert_eq!(ckpt.file_name().unwrap().to_str().unwrap(), lines[2 * k + 1]);
    }
}

#[test]
fn test_words_from_tokens_matches_python() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let pieces = [
        " ", "\u{2581}", "\u{2581}Mer", "haba", ",", " bu", "\u{2581}\u{2581}", "<blk>", " <|en|>", "<a<b>", "ü", "",
        "\u{2581} ", " .", "?", "\u{3000}x", "<>", " <unk> ", "Ä\u{301}", "\t",
    ];
    let mut rng = Rng(3);
    let mut script = String::from(
        "import json, sys\nfrom subtitle_engine.transcribe import words_from_tokens as f\n\
         for line in open(sys.argv[1], encoding='utf-8'):\n    c = json.loads(line)\n\
         \x20   ws = f(c['tokens'], c['ts'], c['s'], c['e'])\n\
         \x20   print(json.dumps([[w.start, w.end, w.text] for w in ws], ensure_ascii=False))\n",
    );
    let mut cases = Vec::new();
    let mut input = String::new();
    for _ in 0..300 {
        let n = rng.range(9) as usize;
        let tokens: Vec<String> = (0..n).map(|_| pieces[rng.range(pieces.len() as u64) as usize].to_string()).collect();
        let s = rng.range(100000) as f64 / 1000.0;
        let e = if rng.range(6) == 0 { s - 0.5 } else { s + rng.range(5000) as f64 / 1000.0 };
        let ts: Option<Vec<f64>> = if rng.range(3) == 0 {
            None
        } else {
            let m = if rng.range(5) == 0 { n.saturating_sub(1) } else { n };
            Some((0..m).map(|_| rng.range(7000) as f64 / 1000.0 - 0.5).collect())
        };
        let tj = format!(
            "[{}]",
            tokens.iter().map(|t| se_core::json::dumps(&se_core::json::Value::from(t.as_str()), false)).collect::<Vec<_>>().join(", ")
        );
        let tsj = match &ts {
            None => "null".to_string(),
            Some(v) => format!("[{}]", v.iter().map(|x| se_core::pyfmt::float_repr(*x)).collect::<Vec<_>>().join(", ")),
        };
        input.push_str(&format!(
            "{{\"tokens\": {tj}, \"ts\": {tsj}, \"s\": {}, \"e\": {}}}\n",
            se_core::pyfmt::float_repr(s),
            se_core::pyfmt::float_repr(e)
        ));
        cases.push((tokens, ts, s, e));
    }
    let input_path = dir.path().join("cases.jsonl");
    std::fs::write(&input_path, input).unwrap();
    let file = dir.path().join("w.py");
    std::fs::write(&file, &script).unwrap();
    let out = Command::new(&py).arg(&file).arg(&input_path).env("PYTHONIOENCODING", "utf-8").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n");
    script.clear();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), cases.len());
    for ((tokens, ts, s, e), line) in cases.iter().zip(lines) {
        let words = se_asr::words_from_tokens(Some(tokens), ts.as_deref(), *s, *e);
        let ours = format!(
            "[{}]",
            words
                .iter()
                .map(|w| format!(
                    "[{}, {}, {}]",
                    w.start.py_str(),
                    w.end.py_str(),
                    se_core::json::dumps(&se_core::json::Value::from(w.text.as_str()), false)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
        assert_eq!(ours, line, "tokens {tokens:?} ts {ts:?} s {s} e {e}");
    }
}

#[test]
fn test_checkpoint_round_trip_matches_python() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let inputs = [
        r#"{"language": "pt", "chunks": {"1": [{"start": 1, "end": "2.5", "text": 3}], "0": [{"start": 0.1, "end": 0.30000000000000004, "text": "olá\n"}]}}"#,
        r#"{"language": 5, "chunks": {}}"#,
        r#"{"chunks": {"x": []}}"#,
        r#"{"language": "tr", "chunks": {" 2 ": "", "3": {}}}"#,
        r#"[1, 2]"#,
        r#"{"language": "en", "chunks": [1]}"#,
        r#"{"language": "en", "chunks": {"0": [{"start": true, "end": 1e400, "text": null}]}}"#,
        r#"{"language": "en", "chunks": {"1_0": [{"start": "1_5", "end": " 2.5e1 ", "text": 1.5}]}}"#,
        r#"{"language": "en", "chunks": {"0": [{"start": "_1", "end": 2, "text": ""}]}}"#,
        r#"{"language": "en", "chunks": {"0": [{"start": "1__0", "end": 2, "text": ""}]}}"#,
        r#"{"language": "en", "chunks": {"_0": []}}"#,
        r#"{"language": "en", "chunks": {"+3": [], "3": [{"start": 0, "end": 0, "text": "x"}], "2": []}}"#,
        r#"{"language": "en", "chunks": {"0": [{"start": "inf", "end": "-Infinity", "text": "nan"}]}}"#,
    ];
    let mut script = String::from(
        "import sys\nfrom pathlib import Path\nimport subtitle_engine.transcribe as t\n\
         for i in range(int(sys.argv[1])):\n    p = Path(sys.argv[2]) / f'in{i}.json'\n\
         \x20   lang, ch = t._load_transcribe_checkpoint(p)\n    o = Path(sys.argv[2]) / f'py{i}.json'\n\
         \x20   if isinstance(lang, str) or lang is None:\n        t._save_transcribe_checkpoint(o, lang, ch)\n\
         \x20   else:\n        o.write_text('nonstr', encoding='utf-8')\n",
    );
    for (i, s) in inputs.iter().enumerate() {
        std::fs::write(dir.path().join(format!("in{i}.json")), s).unwrap();
    }
    let file = dir.path().join("c.py");
    std::fs::write(&file, &script).unwrap();
    let out = Command::new(&py).arg(&file).arg(inputs.len().to_string()).arg(dir.path()).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    script.clear();
    for i in 0..inputs.len() {
        let py_out = std::fs::read_to_string(dir.path().join(format!("py{i}.json"))).unwrap();
        if py_out == "nonstr" {
            continue; // a non-string language: documented divergence (Rust reads it as None)
        }
        let (lang, ch) = chunks::load_checkpoint(Some(&dir.path().join(format!("in{i}.json"))));
        let ours = dir.path().join(format!("rs{i}.json"));
        chunks::save_checkpoint(Some(&ours), lang.as_deref(), &ch);
        assert_eq!(std::fs::read_to_string(&ours).unwrap(), py_out, "input {i}: {}", inputs[i]);
    }
}
