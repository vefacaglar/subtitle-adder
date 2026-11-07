//! Port of `tests/test_preview.py` (+ error-path checks from spec `audio.md` §6).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use se_audio::testgen::{self, probe_preview};
use se_audio::{PreviewSource, make_preview};
use se_core::ErrorKind;

const CHILD_ENV: &str = "SE_AUDIO_PREVIEW_STDIN_CHILD_OUTPUT";

fn set_readonly(path: &Path, readonly: bool) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_readonly(readonly);
    std::fs::set_permissions(path, perms).unwrap();
}

fn approx(value: f64, expected: f64, tol: f64) -> bool {
    (value - expected).abs() <= tol
}

/// Child mode of the stdin case: `preview -` with this process's stdin (the parent passes the source file).
#[test]
fn preview_stdin_child() {
    let Ok(output) = std::env::var(CHILD_ENV) else { return };
    let code = match make_preview(PreviewSource::Stdin, Path::new(&output)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    };
    // stdin must still be usable (not closed) after the preview.
    use std::io::Read;
    let mut rest = Vec::new();
    let still_open = std::io::stdin().read_to_end(&mut rest).is_ok();
    std::process::exit(if still_open { code } else { 3 });
}

fn run_case(extension: &str, video_codec: &str, audio_codec: &str, video_options: &[(&str, &str)]) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Dış disk");
    std::fs::create_dir(&dir).unwrap();
    let source = dir.join(format!("Örnek video.{extension}"));
    let output = tmp.path().join("preview.mp4");
    testgen::preview_source(&source, video_codec, audio_codec, video_options).unwrap();
    let source_bytes = std::fs::read(&source).unwrap();
    set_readonly(&source, true);
    if extension == "wmv" {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "preview_stdin_child", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, &output)
            .stdin(Stdio::from(std::fs::File::open(&source).unwrap()))
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(0));
    } else {
        make_preview(PreviewSource::Path(source.clone()), &output).unwrap();
    }
    assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
    set_readonly(&source, false);
    let probe = probe_preview(&output).unwrap();
    assert_eq!(probe.video_codec, "h264");
    assert_eq!(probe.audio_codec, "aac");
    assert!(approx(probe.audio_start, 0.5, 0.06), "{probe:?}");
    assert!(approx(probe.duration, 2.5, 0.08), "{probe:?}");
    assert!(approx(probe.first_video_time, 0.0, 0.06), "{probe:?}");
    assert!(approx(probe.last_video_time_after_seek, 1.9, 0.06), "{probe:?}");
}

#[test]
fn test_previews_as_seekable_mp4_mkv_libx264_aac_video_options0() {
    run_case("mkv", "libx264", "aac", &[("bf", "0")]);
}

#[test]
fn test_previews_as_seekable_mp4_mkv_libx264_aac_video_options1() {
    run_case("mkv", "libx264", "aac", &[("bf", "2")]);
}

#[test]
fn test_previews_as_seekable_mp4_mkv_mpeg4_pcm_s16le_video_options2() {
    run_case("mkv", "mpeg4", "pcm_s16le", &[]);
}

#[test]
fn test_previews_as_seekable_mp4_wmv_wmv2_wmav2_video_options3() {
    run_case("wmv", "wmv2", "wmav2", &[]);
}

fn tone_video(dir: &Path) -> PathBuf {
    let path = dir.join("tone.mp4");
    testgen::tone_video(&path).unwrap();
    path
}

#[test]
fn test_preview_cannot_overwrite_source() {
    let tmp = tempfile::tempdir().unwrap();
    let tone = tone_video(tmp.path());
    let before = std::fs::read(&tone).unwrap();
    let err = make_preview(PreviewSource::Path(tone.clone()), &tone).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Os);
    assert_eq!(err.to_string(), format!("[Errno 17] File exists: {}", se_core::pyfmt::repr_str(&se_core::paths::py_path_str(&tone))));
    assert_eq!(std::fs::read(&tone).unwrap(), before);
}

#[test]
fn test_missing_source_fails_before_reserving_the_output() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("yok.mkv");
    let output = tmp.path().join("out.mp4");
    let err = make_preview(PreviewSource::Path(missing.clone()), &output).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("[Errno 2] No such file or directory: {}", se_core::pyfmt::repr_str(&se_core::paths::py_path_str(&missing)))
    );
    assert!(!output.exists());
}

#[test]
fn test_unprobeable_source_reports_ffmpeg_error_and_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join("bad.bin");
    std::fs::write(&bad, vec![0x5Au8; 4096]).unwrap();
    let output = tmp.path().join("out.mp4");
    let err = make_preview(PreviewSource::Path(bad.clone()), &output).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "[Errno 1094995529] Invalid data found when processing input: {}",
            se_core::pyfmt::repr_str(&se_core::paths::py_path_str(&bad))
        )
    );
    assert!(!output.exists());
}

#[test]
fn test_tone_video_preview_is_h264_aac() {
    let tmp = tempfile::tempdir().unwrap();
    let tone = tone_video(tmp.path());
    let output = tmp.path().join("p.mp4");
    make_preview(PreviewSource::Path(tone), &output).unwrap();
    assert_eq!(testgen::stream_codecs(&output).unwrap(), vec!["h264", "aac"]);
}
