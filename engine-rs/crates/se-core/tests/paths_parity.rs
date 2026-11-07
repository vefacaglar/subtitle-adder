//! WAV naming parity with the Python engine (`pipeline.wav_path_for`, `_safe_out_stem`, `_source_hash`) and
//! the path-only tests of `test_pipeline_audio_reuse.py`.
//!
//! The parity test runs the Python reference (`engine/.venv`) when it is present (override with
//! `SE_PYTHON=<python.exe>`); without it the test returns early.

use std::path::{Path, PathBuf};
use std::process::Command;

use se_core::paths::{self, py_path_str};

fn python() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SE_PYTHON") {
        return Some(PathBuf::from(p));
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("../../../engine/.venv/Scripts/python.exe"),
        manifest.join("../../../engine/.venv/bin/python"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

#[test]
fn test_wav_names_match_python_engine() {
    let Some(py) = python() else {
        eprintln!("Python reference engine not found; skipping");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let root = paths::resolve(root.path());
    let out = root.join("Out Dir");
    std::fs::create_dir_all(root.join("in")).unwrap();
    std::fs::write(root.join("in").join("Clip.mp4"), b"v").unwrap();
    let names = [
        "in/Clip.mp4".to_string(),
        "in/../in/Clip.mp4".to_string(),
        "IN/clip.MP4".to_string(),
        "missing/Ünlü Dizi İş.mkv".to_string(),
        "missing/foo. .mp4".to_string(),
        "missing/.bashrc".to_string(),
        "missing/a.tar.gz".to_string(),
        format!("missing/{}.mp4", "x".repeat(215)),
        format!("missing/{}ş .mp4", "ğ".repeat(230)),
    ];
    let mut script = String::from("import sys\nfrom pathlib import Path\nfrom subtitle_engine.pipeline import wav_path_for\n");
    script.push_str("root = Path(sys.argv[1]); out = Path(sys.argv[2])\n");
    script.push_str("for line in sys.stdin.read().split('\\n'):\n    if line:\n        sys.stdout.write(str(wav_path_for(root / line, out)) + '\\n')\n");
    let mut child = Command::new(&py)
        .args(["-X", "utf8", "-c", &script])
        .arg(&root)
        .arg(&out)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(names.join("\n").as_bytes()).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "python failed");
    let expected: Vec<String> = String::from_utf8(output.stdout).unwrap().lines().map(str::to_string).collect();
    assert_eq!(expected.len(), names.len());
    for (name, want) in names.iter().zip(&expected) {
        let video = root.join(name);
        assert_eq!(&py_path_str(&paths::wav_path_for(&video, &out)), want, "{name}");
    }
}

#[test]
fn test_wav_name_hash_follows_the_resolved_path() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::create_dir_all(base.join("in")).unwrap();
    std::fs::create_dir_all(base.join("other")).unwrap();
    std::fs::write(base.join("in/Clip.mp4"), b"video-bytes").unwrap();
    std::fs::write(base.join("other/Clip.mp4"), b"video-bytes").unwrap();
    let out = base.join("out");
    let a = paths::wav_path_for(&base.join("in/../in/Clip.mp4"), &out);
    let b = paths::wav_path_for(&base.join("in/Clip.mp4"), &out);
    let c = paths::wav_path_for(&base.join("other/Clip.mp4"), &out);
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn test_long_stem_keeps_the_wav_and_its_companions_inside_the_path_budget() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join(format!("{}.mp4", "x".repeat(215)));
    let out = dir.path().join("out");
    let wav = paths::wav_path_for(&video, &out);
    let wav_text = py_path_str(&wav);
    assert!(wav_text.chars().count() <= 210, "{}", wav_text.chars().count());
    assert!(py_path_str(&paths::tmp_wav_path(&wav, 4194304)).chars().count() < 260);
    assert!(py_path_str(&paths::marker_tmp_path(&wav, 4194304)).chars().count() < 260);
    let name = wav.file_name().unwrap().to_str().unwrap();
    let hash = &name[name.len() - 12..name.len() - 4];
    assert!(name.ends_with(".wav") && name.as_bytes()[name.len() - 13] == b'-');
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}
