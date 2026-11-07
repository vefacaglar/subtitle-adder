//! Byte parity with the Python engine on the fixtures of its own test suite: the fixtures are built by PyAV
//! (`tests/fixtures/py_parity.py`), then both engines extract audio / make previews from the same files.
//!
//! Runs only when the Python reference engine (`engine/.venv`) exists (override with `SE_PYTHON`).

use std::path::{Path, PathBuf};
use std::process::Command;

use se_audio::testgen::probe_preview;
use se_audio::{PreviewSource, extract_audio, make_preview};
use se_core::json::{self, Value};

fn python() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SE_PYTHON") {
        return Some(PathBuf::from(p));
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    [manifest.join("../../../engine/.venv/Scripts/python.exe"), manifest.join("../../../engine/.venv/bin/python")]
        .into_iter()
        .find(|p| p.is_file())
}

#[test]
fn test_extraction_and_preview_match_python_engine() {
    let Some(py) = python() else {
        eprintln!("Python reference engine not found; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/py_parity.py");
    let status = Command::new(&py).args(["-X", "utf8"]).arg(&script).arg(&work).env("SUBTITLE_ENGINE_LANG", "en").status().unwrap();
    assert!(status.success());
    let manifest = json::parse(&std::fs::read_to_string(work.join("manifest.json")).unwrap()).unwrap();
    // SAFETY: tests in this binary do not read the environment concurrently.
    unsafe { std::env::set_var("SUBTITLE_ENGINE_LANG", "en") };

    let mut mismatches = Vec::new();
    for entry in manifest.get("extract").unwrap().as_array().unwrap() {
        let video = PathBuf::from(entry.get("video").unwrap().as_str().unwrap());
        let rust_wav = PathBuf::from(format!("{}.rs.wav", video.display()));
        let mut progress = Vec::new();
        let mut cb = |f: f64| progress.push(f);
        let result = extract_audio(&video, &rust_wav, Some(&mut cb));
        let py_error = entry.get("error").and_then(Value::as_str).map(str::to_string);
        match (&result, &py_error) {
            (Ok(_), None) => {
                let py_bytes = std::fs::read(entry.get("wav").unwrap().as_str().unwrap()).unwrap();
                let rs_bytes = std::fs::read(&rust_wav).unwrap();
                if py_bytes != rs_bytes {
                    mismatches.push(format!("{}: WAV differs ({} vs {} bytes)", video.display(), py_bytes.len(), rs_bytes.len()));
                }
            }
            (Err(e), Some(p)) if e.to_string() == *p => {}
            _ => mismatches.push(format!("{}: result {:?} vs Python error {:?}", video.display(), result.as_ref().map(|_| ()), py_error)),
        }
        let py_progress: Vec<f64> =
            entry.get("progress").unwrap().as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        if py_progress != progress {
            mismatches.push(format!("{}: progress differs ({} vs {} values)", video.display(), py_progress.len(), progress.len()));
        }
    }
    for entry in manifest.get("preview").unwrap().as_array().unwrap() {
        let source = PathBuf::from(entry.get("source").unwrap().as_str().unwrap());
        let py_out = PathBuf::from(entry.get("output").unwrap().as_str().unwrap());
        let rs_out = PathBuf::from(format!("{}.rs.mp4", source.display()));
        let result = make_preview(PreviewSource::Path(source.clone()), &rs_out);
        let py_error = entry.get("error").and_then(Value::as_str).map(str::to_string);
        assert_eq!(result.as_ref().err().map(|e| e.to_string()), py_error, "{}", source.display());
        if result.is_err() {
            continue;
        }
        let (py_probe, rs_probe) = (probe_preview(&py_out).unwrap(), probe_preview(&rs_out).unwrap());
        if py_probe != rs_probe {
            mismatches.push(format!("{}: probe {py_probe:?} vs {rs_probe:?}", source.display()));
        }
        let (a, b) = (std::fs::read(&py_out).unwrap(), std::fs::read(&rs_out).unwrap());
        if mask_lav_versions(&a) != mask_lav_versions(&b) {
            mismatches.push(format!("{}: preview bytes differ ({} vs {})", source.display(), a.len(), b.len()));
        }
    }
    // WAV + marker written by the Python engine are reused by the Rust engine, and the Rust engine writes the
    // same WAV name and the same marker text.
    let reuse = manifest.get("reuse").unwrap();
    let tone = work.join("tone.mp4");
    let mut stages = Vec::new();
    let mut on_stage = |s: &str, f: Option<f64>| stages.push((s.to_string(), f));
    let wav = se_audio::extract_video_audio(&tone, &work.join("pyout"), &mut on_stage).unwrap();
    assert_eq!(se_core::paths::py_path_str(&wav), reuse.get("wav").unwrap().as_str().unwrap());
    assert_eq!(stages, vec![("extracting_audio".to_string(), Some(1.0))]);
    let py_stages = reuse.get("stages").unwrap().as_array().unwrap();
    let mut rs_stages = Vec::new();
    let mut on_stage = |s: &str, f: Option<f64>| rs_stages.push((s.to_string(), f));
    let rs_wav = se_audio::extract_video_audio(&tone, &work.join("rsout"), &mut on_stage).unwrap();
    assert_eq!(rs_wav.file_name(), wav.file_name());
    let rs_marker = std::fs::read_to_string(se_core::paths::marker_path(&rs_wav)).unwrap();
    assert_eq!(rs_marker, reuse.get("marker").unwrap().as_str().unwrap());
    assert_eq!(rs_stages.len(), py_stages.len());
    for (r, p) in rs_stages.iter().zip(py_stages) {
        let p = p.as_array().unwrap();
        assert_eq!(r.0, p[0].as_str().unwrap());
        assert_eq!(r.1, p[1].as_f64());
    }
    assert_eq!(std::fs::read(&rs_wav).unwrap(), std::fs::read(&wav).unwrap());

    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

/// The muxer/encoder version tags (`Lavf62.12.102`, `Lavc62.28.102 libx264`) name the FFmpeg build; the
/// Python wheel and the Rust dev SDK differ in the micro version, so those digits are masked.
fn mask_lav_versions(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i + 4 < out.len() {
        if &out[i..i + 3] == b"Lav" && matches!(out[i + 3], b'f' | b'c') {
            let mut j = i + 4;
            while j < out.len() && (out[j].is_ascii_digit() || out[j] == b'.') {
                out[j] = b'#';
                j += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}
