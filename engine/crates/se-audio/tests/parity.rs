//! Byte parity with the Python engine on the fixtures of its own test suite: the fixtures are built by PyAV
//! (`tests/fixtures/py_parity.py`), then both engines extract audio / make previews from the same files.
//!
//! Runs only when the Python reference engine (`engine/.venv`) exists (override with `SE_PYTHON`).

use std::path::{Path, PathBuf};
use std::process::Command;

use se_audio::testgen::probe_preview;
use se_audio::{PreviewSource, extract_audio, make_preview};
use se_core::json::{self, Value};

fn find_python() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SE_PYTHON") {
        return Some(PathBuf::from(p));
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    [manifest.join("../../../engine/.venv/Scripts/python.exe"), manifest.join("../../../engine/.venv/bin/python")]
        .into_iter()
        .find(|p| p.is_file())
}

/// [`find_python`] when it can import the reference package: a stale venv whose `subtitle_engine` source was
/// removed skips like a missing one instead of failing with `ModuleNotFoundError`. Probed once per test binary.
fn python() -> Option<PathBuf> {
    static PYTHON: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    PYTHON
        .get_or_init(|| {
            let py = find_python()?;
            let ok = Command::new(&py).args(["-c", "import subtitle_engine"]).output().is_ok_and(|o| o.status.success());
            if !ok {
                eprintln!("Python reference package `subtitle_engine` cannot be imported from {}; skipping", py.display());
            }
            ok.then_some(py)
        })
        .clone()
}

/// Runs a PyAV fixture script from `tests/fixtures` into `work` and returns its manifest.
fn run_python(py: &Path, script: &str, work: &Path) -> Value {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(script);
    let status = Command::new(py).args(["-X", "utf8"]).arg(&script).arg(work).env("SUBTITLE_ENGINE_LANG", "en").status().unwrap();
    assert!(status.success());
    // SAFETY: tests in this binary do not read the environment concurrently.
    unsafe { std::env::set_var("SUBTITLE_ENGINE_LANG", "en") };
    json::parse(&std::fs::read_to_string(work.join("manifest.json")).unwrap()).unwrap()
}

/// Runs the Rust extraction and preview on every manifest entry and lists the differences from Python.
fn compare_manifest(manifest: &Value) -> Vec<String> {
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
            _ => mismatches.push(format!(
                "{}: result {:?} vs Python error {:?}",
                video.display(),
                result.as_ref().map(|_| ()).map_err(|e| e.to_string()),
                py_error
            )),
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
        let rs_error = result.as_ref().err().map(|e| e.to_string());
        if rs_error != py_error {
            mismatches.push(format!("{}: preview error {rs_error:?} vs Python {py_error:?}", source.display()));
            continue;
        }
        if result.is_err() {
            if rs_out.exists() != py_out.exists() {
                mismatches.push(format!("{}: output left behind differs", source.display()));
            }
            continue;
        }
        let (py_probe, rs_probe) = (probe_preview(&py_out), probe_preview(&rs_out));
        if py_probe != rs_probe {
            mismatches.push(format!("{}: probe {py_probe:?} vs {rs_probe:?}", source.display()));
        }
        let (a, b) = (std::fs::read(&py_out).unwrap(), std::fs::read(&rs_out).unwrap());
        if mask_lav_versions(&a) != mask_lav_versions(&b) {
            mismatches.push(format!("{}: preview bytes differ ({} vs {})", source.display(), a.len(), b.len()));
        }
    }
    mismatches
}

#[test]
fn test_extraction_and_preview_match_python_engine() {
    let Some(py) = python() else {
        eprintln!("Python reference engine not found; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    let manifest = run_python(&py, "py_parity.py", &work);
    let mismatches = compare_manifest(&manifest);
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

#[test]
fn test_edge_cases_match_python_engine() {
    let Some(py) = python() else {
        eprintln!("Python reference engine not found; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    let manifest = run_python(&py, "py_parity_edge.py", &work);
    let mismatches = compare_manifest(&manifest);
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

/// WAV names, hashes, markers and source identities equal the Python engine's for tricky file names (Python
/// `Path.stem`, `rstrip(" .")`, the path budget, `normcase` lowercasing, non-strict `resolve`).
#[test]
fn test_wav_naming_matches_python_engine() {
    let Some(py) = python() else {
        eprintln!("Python reference engine not found; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let root = se_core::paths::resolve(tmp.path());
    let media = root.join("Medya İçerik");
    std::fs::create_dir_all(&media).unwrap();
    // Known se-core `resolve` divergences on Windows, left out here (reported): a MISSING name with a trailing
    // dot or space (Python keeps it, `std::path::absolute` strips it) and an EXISTING path over MAX_PATH (Python strips
    // the `\?\` prefix, se-core keeps it), both of which change the WAV hash and the marker's `source`.
    let long = "x".repeat(215);
    let names = [
        // Created (even index), so Windows stores it as `a` and both engines resolve to that.
        "a.".to_string(),
        "Clip.mp4".to_string(),
        ".bashrc".to_string(),
        "x.tar.gz".to_string(),
        "trailing. . .mkv".to_string(),
        "...mp4".to_string(),
        "İSTANBUL ΣΟΦΟΣ.MKV".to_string(),
        format!("{long}.mp4"),
        format!("{}  . .mp4", "y".repeat(150)),
        format!("{}  . .mp4", "w".repeat(20)),
        "emoji 😀 ü.webm".to_string(),
    ];
    let mut cases: Vec<(String, String)> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let video = media.join(name);
        if i % 2 == 0 {
            // Some exist (real resolve and stat), some do not (non-strict resolve, no identity).
            let _ = std::fs::write(&video, b"data");
        }
        let out_dir = root.join("out");
        cases.push((se_core::paths::py_path_str(&video), se_core::paths::py_path_str(&out_dir)));
        // Relative-looking spellings: `..` segments, forward slashes, a deep out dir eating the path budget.
        cases.push((format!("{}/../{}/{name}", media.display(), media.file_name().unwrap().to_string_lossy()), se_core::paths::py_path_str(&root.join("o").join("z".repeat(120)))));
    }
    let input = json::dumps(&Value::Array(cases.iter().map(|(v, o)| Value::Array(vec![v.as_str().into(), o.as_str().into()])).collect()), true);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/py_naming.py");
    let mut child = Command::new(&py)
        .args(["-X", "utf8"])
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let expected = json::parse(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
    let expected = expected.as_array().unwrap();
    assert_eq!(expected.len(), cases.len());
    for ((video, out_dir), want) in cases.iter().zip(expected) {
        let (v, o) = (Path::new(video), Path::new(out_dir));
        let wav = se_core::paths::wav_path_for(v, o);
        assert_eq!(se_core::paths::safe_out_stem(v, o), want.get("stem").unwrap().as_str().unwrap(), "{video}");
        let resolved = se_core::paths::py_path_str(&se_core::paths::resolve(v));
        assert_eq!(se_core::paths::source_hash(v), want.get("hash").unwrap().as_str().unwrap(), "{video} -> {resolved}");
        assert_eq!(se_core::paths::py_path_str(&wav), want.get("wav").unwrap().as_str().unwrap(), "{video}");
        assert_eq!(
            se_core::paths::py_path_str(&se_core::paths::marker_path(&wav)),
            want.get("marker").unwrap().as_str().unwrap(),
            "{video}"
        );
        let identity = se_audio::reuse::source_identity(v);
        match want.get("identity").unwrap() {
            Value::Null => assert_eq!(identity, None, "{video}"),
            id => {
                let identity = identity.unwrap_or_else(|| panic!("{video}: no identity"));
                assert_eq!(identity.source, id.get("source").unwrap().as_str().unwrap(), "{video}");
                assert_eq!(Some(identity.size as i64), id.get("size").unwrap().as_i64(), "{video}");
                assert_eq!(
                    identity.mtime_ns.to_string(),
                    json::dumps(id.get("mtime_ns").unwrap(), true),
                    "{video}"
                );
            }
        }
    }
}
