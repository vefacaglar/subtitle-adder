//! Port of the `extract_video_audio` part of `tests/test_pipeline_audio_reuse.py` (the `pipeline.run` tests
//! and the two path-only tests live in se-cli / se-core).

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use se_audio::reuse::{self, AudioBackend, FfmpegBackend, extract_video_audio_with};
use se_core::json::{self, Value};
use se_core::paths::{py_path_str, resolve, wav_path_for};
use se_core::{EngineError, ErrorKind, ProgressFn, Result};

type Stages = Vec<(String, Option<f64>)>;

#[derive(Default)]
struct FakeExtractor {
    calls: RefCell<Vec<PathBuf>>,
    /// Make `extract_audio` write a partial file and fail.
    fail: bool,
    /// `replace` of a `*.tmp.wav` fails; when `race` is set, another writer first puts a matching WAV in place.
    lock: bool,
    race: Option<PathBuf>,
}

impl AudioBackend for FakeExtractor {
    fn extract_audio(&self, video: &Path, out_wav: &Path, on_progress: Option<ProgressFn<'_>>) -> Result<PathBuf> {
        if self.fail {
            std::fs::write(out_wav, [0u8; 100]).unwrap();
            return Err(EngineError::runtime("decode failed"));
        }
        self.calls.borrow_mut().push(video.to_path_buf());
        let mut on_progress = on_progress;
        if let Some(cb) = on_progress.as_deref_mut() {
            cb(0.5);
        }
        let mut body = vec![0u8; 64];
        body.extend_from_slice(format!("audio-of:{}", py_path_str(video)).as_bytes());
        std::fs::write(out_wav, body).unwrap();
        if let Some(cb) = on_progress.as_deref_mut() {
            cb(1.0);
        }
        Ok(out_wav.to_path_buf())
    }

    fn replace(&self, src: &Path, dst: &Path) -> io::Result<()> {
        if self.lock && py_path_str(src).ends_with(".tmp.wav") {
            if let Some(video) = &self.race {
                let mut body = vec![0u8; 64];
                body.extend_from_slice(b"written-by-the-other-process");
                std::fs::write(dst, &body).unwrap();
                reuse::write_source_marker(dst, reuse::source_identity(video).as_ref(), body.len() as i64);
            }
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "target is in use"));
        }
        se_core::fsutil::replace(src, dst)
    }
}

fn make_video(folder: &Path, name: &str, content: &[u8]) -> PathBuf {
    std::fs::create_dir_all(folder).unwrap();
    let path = folder.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

fn extract(backend: &dyn AudioBackend, video: &Path, out_dir: &Path) -> Result<(PathBuf, Stages)> {
    let mut stages: Stages = Vec::new();
    let mut on_stage = |s: &str, f: Option<f64>| stages.push((s.to_string(), f));
    let wav = extract_video_audio_with(backend, video, out_dir, &mut on_stage)?;
    Ok((wav, stages))
}

fn marker_of(wav: &Path) -> Value {
    json::parse(&std::fs::read_to_string(se_core::paths::marker_path(wav)).unwrap()).unwrap()
}

fn audio_of(video: &Path) -> Vec<u8> {
    format!("audio-of:{}", py_path_str(video)).into_bytes()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn tmp_leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect()
}

fn none() -> (String, Option<f64>) {
    ("extracting_audio".into(), None)
}
fn one() -> (String, Option<f64>) {
    ("extracting_audio".into(), Some(1.0))
}

fn mtime_ns(path: &Path) -> i128 {
    reuse::mtime_ns(&std::fs::metadata(path).unwrap())
}

fn set_mtime_ns(path: &Path, ns: i128) {
    let t = filetime::FileTime::from_unix_time((ns.div_euclid(1_000_000_000)) as i64, ns.rem_euclid(1_000_000_000) as u32);
    filetime::set_file_mtime(path, t).unwrap();
}

#[test]
fn test_real_extraction_writes_identity_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let tone = tmp.path().join("tone.mp4");
    se_audio::testgen::tone_video(&tone).unwrap();
    let out_dir = tmp.path().join("out");
    let (wav, stages) = extract(&FfmpegBackend, &tone, &out_dir).unwrap();
    let meta = std::fs::metadata(&tone).unwrap();
    let marker = marker_of(&wav);
    assert_eq!(wav, wav_path_for(&tone, &out_dir));
    let name = wav.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("tone-") && name.ends_with(".wav") && name.len() == "tone-".len() + 8 + 4, "{name}");
    assert!(name[5..13].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_eq!(marker.get("source").unwrap().as_str().unwrap(), py_path_str(&resolve(&tone)));
    assert_eq!(marker.get("size").unwrap().as_i64().unwrap() as u64, meta.len());
    assert_eq!(marker.get("mtime_ns").unwrap().as_i64().unwrap() as i128, mtime_ns(&tone));
    assert_eq!(marker.get("wav_size").unwrap().as_i64().unwrap() as u64, std::fs::metadata(&wav).unwrap().len());
    assert_eq!(stages[0], none());
    assert_eq!(*stages.last().unwrap(), one());
    let mut names: Vec<String> =
        std::fs::read_dir(&out_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, vec![name.clone(), format!("{name}.source.json")]);
    // The marker text is Python's json.dumps (ensure_ascii, ", " / ": ").
    let text = std::fs::read_to_string(se_core::paths::marker_path(&wav)).unwrap();
    assert!(text.starts_with("{\"version\": 1, \"source\": \""), "{text}");
}

#[test]
fn test_matching_marker_reuses_wav_with_single_stage() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
    let out_dir = tmp.path().join("out");
    let (first, first_stages) = extract(&fake, &video, &out_dir).unwrap();
    let (second, second_stages) = extract(&fake, &video, &out_dir).unwrap();
    assert_eq!(first, second);
    assert_eq!(first, wav_path_for(&video, &out_dir));
    assert_eq!(first_stages, vec![none(), ("extracting_audio".into(), Some(0.5)), one()]);
    assert_eq!(second_stages, vec![one()]);
    assert_eq!(*fake.calls.borrow(), vec![video]);
}

#[test]
fn test_same_stem_in_another_folder_gets_its_own_wav_and_both_stay_reusable() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let a = make_video(&tmp.path().join("S01"), "Episode 1.mkv", b"video-A");
    let b = make_video(&tmp.path().join("S02"), "Episode 1.mkv", b"video-B-is-longer");
    let out_dir = tmp.path().join("out");
    let (wav_a, _) = extract(&fake, &a, &out_dir).unwrap();
    let (wav_b, stages_b) = extract(&fake, &b, &out_dir).unwrap();
    assert_ne!(wav_a, wav_b);
    for w in [&wav_a, &wav_b] {
        assert!(w.file_name().unwrap().to_string_lossy().starts_with("Episode 1-"));
    }
    assert!(contains(&std::fs::read(&wav_a).unwrap(), &audio_of(&a)));
    assert!(contains(&std::fs::read(&wav_b).unwrap(), &audio_of(&b)));
    assert_eq!(stages_b[0], none());
    assert_eq!(marker_of(&wav_a).get("source").unwrap().as_str().unwrap(), py_path_str(&resolve(&a)));
    assert_eq!(marker_of(&wav_b).get("source").unwrap().as_str().unwrap(), py_path_str(&resolve(&b)));
    let (again, stages) = extract(&fake, &a, &out_dir).unwrap();
    assert_eq!(again, wav_a);
    assert_eq!(stages, vec![one()]);
    assert_eq!(*fake.calls.borrow(), vec![a, b]);
}

#[test]
fn test_same_stem_different_extension_in_one_folder_gets_its_own_wav() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let mkv = make_video(&tmp.path().join("in"), "Clip.mkv", b"video-mkv");
    let mp4 = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-mp4-longer");
    let out_dir = tmp.path().join("out");
    let (wav_mkv, _) = extract(&fake, &mkv, &out_dir).unwrap();
    let (wav_mp4, _) = extract(&fake, &mp4, &out_dir).unwrap();
    assert_ne!(wav_mkv, wav_mp4);
    assert!(contains(&std::fs::read(&wav_mkv).unwrap(), &audio_of(&mkv)));
    assert!(contains(&std::fs::read(&wav_mp4).unwrap(), &audio_of(&mp4)));
}

#[test]
fn test_same_size_and_mtime_in_another_folder_is_still_a_different_source() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let a = make_video(&tmp.path().join("A"), "Clip.mp4", b"same-size-A");
    let b = make_video(&tmp.path().join("B"), "Clip.mp4", b"same-size-B");
    set_mtime_ns(&b, mtime_ns(&a));
    let out_dir = tmp.path().join("out");
    let (wav_a, _) = extract(&fake, &a, &out_dir).unwrap();
    let (wav_b, _) = extract(&fake, &b, &out_dir).unwrap();
    assert_ne!(wav_a, wav_b);
    assert_eq!(*fake.calls.borrow(), vec![a.clone(), b.clone()]);
    assert!(contains(&std::fs::read(&wav_b).unwrap(), &audio_of(&b)));
    assert!(contains(&std::fs::read(&wav_a).unwrap(), &audio_of(&a)));
}

#[test]
fn test_changed_size_re_extracts() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
    let out_dir = tmp.path().join("out");
    extract(&fake, &video, &out_dir).unwrap();
    let mtime = mtime_ns(&video);
    std::fs::write(&video, b"re-encoded video with a different size").unwrap();
    set_mtime_ns(&video, mtime);
    let (wav, stages) = extract(&fake, &video, &out_dir).unwrap();
    assert_eq!(stages[0], none());
    assert_eq!(fake.calls.borrow().len(), 2);
    assert_eq!(marker_of(&wav).get("size").unwrap().as_i64().unwrap() as u64, std::fs::metadata(&video).unwrap().len());
}

#[test]
fn test_changed_mtime_re_extracts() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
    let out_dir = tmp.path().join("out");
    extract(&fake, &video, &out_dir).unwrap();
    set_mtime_ns(&video, mtime_ns(&video) + 5_000_000_000);
    let (wav, stages) = extract(&fake, &video, &out_dir).unwrap();
    assert_eq!(stages[0], none());
    assert_eq!(fake.calls.borrow().len(), 2);
    assert_eq!(marker_of(&wav).get("mtime_ns").unwrap().as_i64().unwrap() as i128, mtime_ns(&video));
}

#[test]
fn test_wav_without_marker_is_re_extracted() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let stale = wav_path_for(&video, &out_dir);
    std::fs::write(&stale, [0u8; 200]).unwrap();
    let (wav, stages) = extract(&fake, &video, &out_dir).unwrap();
    assert_eq!(wav, stale);
    assert_eq!(stages[0], none());
    assert_eq!(fake.calls.borrow().len(), 1);
    assert!(contains(&std::fs::read(&wav).unwrap(), &audio_of(&video)));
    assert_eq!(marker_of(&wav).get("source").unwrap().as_str().unwrap(), py_path_str(&resolve(&video)));
}

#[test]
fn test_wav_left_by_the_older_stem_only_naming_is_ignored_and_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let legacy = out_dir.join("Clip.wav");
    std::fs::write(&legacy, [0u8; 200]).unwrap();
    let (wav, stages) = extract(&fake, &video, &out_dir).unwrap();
    assert_ne!(wav, legacy);
    assert_eq!(stages[0], none());
    assert_eq!(std::fs::read(&legacy).unwrap(), vec![0u8; 200]);
}

#[test]
fn test_unreadable_or_incomplete_marker_is_re_extracted() {
    for garbage in ["not json", "[]", "{\"source\": \"x\"}", ""] {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeExtractor::default();
        let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
        let out_dir = tmp.path().join("out");
        let (wav, _) = extract(&fake, &video, &out_dir).unwrap();
        std::fs::write(se_core::paths::marker_path(&wav), garbage).unwrap();
        let (_, stages) = extract(&fake, &video, &out_dir).unwrap();
        assert_eq!(stages[0], none(), "{garbage}");
        assert_eq!(fake.calls.borrow().len(), 2, "{garbage}");
    }
}

#[test]
fn test_tiny_wav_is_not_reused_even_with_matching_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
    let out_dir = tmp.path().join("out");
    let (wav, _) = extract(&fake, &video, &out_dir).unwrap();
    std::fs::write(&wav, [0u8; 44]).unwrap();
    let (_, stages) = extract(&fake, &video, &out_dir).unwrap();
    assert_eq!(stages[0], none());
    assert_eq!(fake.calls.borrow().len(), 2);
}

#[test]
fn test_wav_replaced_behind_the_markers_back_is_not_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
    let out_dir = tmp.path().join("out");
    let (wav, _) = extract(&fake, &video, &out_dir).unwrap();
    std::fs::write(&wav, [0u8; 500]).unwrap();
    let (_, stages) = extract(&fake, &video, &out_dir).unwrap();
    assert_eq!(stages[0], none());
    assert_eq!(fake.calls.borrow().len(), 2);
}

#[test]
fn test_unwritable_marker_does_not_crash_and_wav_is_not_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let expected = wav_path_for(&video, &out_dir);
    std::fs::create_dir(se_core::paths::marker_path(&expected)).unwrap();
    let (wav, stages) = extract(&fake, &video, &out_dir).unwrap();
    assert!(wav == expected && wav.is_file());
    assert_eq!(stages[0], none());
    assert!(tmp_leftovers(&out_dir).is_empty(), "{:?}", tmp_leftovers(&out_dir));
    let (_, again) = extract(&fake, &video, &out_dir).unwrap();
    assert_eq!(again[0], none());
    assert_eq!(fake.calls.borrow().len(), 2);
}

#[test]
fn test_failed_extraction_cleans_temp_file_and_keeps_previous_wav_and_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-v1");
    let out_dir = tmp.path().join("out");
    let (wav, _) = extract(&fake, &video, &out_dir).unwrap();
    let before = std::fs::read(&wav).unwrap();
    let old_marker = marker_of(&wav);
    std::fs::write(&video, b"video-v2-is-longer").unwrap();
    let failing = FakeExtractor { fail: true, ..Default::default() };
    let err = extract(&failing, &video, &out_dir).unwrap_err();
    assert_eq!(err.to_string(), "decode failed");
    assert!(tmp_leftovers(&out_dir).is_empty());
    assert_eq!(std::fs::read(&wav).unwrap(), before);
    assert_eq!(marker_of(&wav), old_marker);
}

#[test]
fn test_locked_target_keeps_old_wav_and_marker_and_returns_own_audio() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-v1");
    let out_dir = tmp.path().join("out");
    let (wav, _) = extract(&fake, &video, &out_dir).unwrap();
    let (old_bytes, old_marker) = (std::fs::read(&wav).unwrap(), marker_of(&wav));
    std::fs::write(&video, b"video-v2-is-longer").unwrap();
    let locked = FakeExtractor { lock: true, ..Default::default() };
    let (result, _) = extract(&locked, &video, &out_dir).unwrap();
    assert_ne!(result, wav);
    assert!(py_path_str(&result).ends_with(".tmp.wav"));
    assert!(contains(&std::fs::read(&result).unwrap(), b"audio-of:"));
    assert_eq!(std::fs::read(&wav).unwrap(), old_bytes);
    assert_eq!(marker_of(&wav), old_marker);
}

#[test]
fn test_locked_target_already_produced_by_another_process_for_this_video_is_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let wav = wav_path_for(&video, &out_dir);
    let raced = FakeExtractor { lock: true, race: Some(video.clone()), ..Default::default() };
    let (result, _) = extract(&raced, &video, &out_dir).unwrap();
    assert_eq!(result, wav);
    assert!(contains(&std::fs::read(&wav).unwrap(), b"written-by-the-other-process"));
    assert!(tmp_leftovers(&out_dir).iter().all(|n| !n.ends_with(".tmp.wav")));
}

#[test]
fn test_missing_video_still_reports_extraction_error_and_never_reuses_a_wav() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let missing = tmp.path().join("yok.mp4");
    std::fs::write(wav_path_for(&missing, &out_dir), [0u8; 200]).unwrap();
    let (result, stages) = {
        let mut stages: Stages = Vec::new();
        let mut on_stage = |s: &str, f: Option<f64>| stages.push((s.to_string(), f));
        let r = extract_video_audio_with(&FfmpegBackend, &missing, &out_dir, &mut on_stage);
        (r, stages)
    };
    let err = result.unwrap_err();
    assert_eq!(err.kind, ErrorKind::AudioExtraction);
    assert!(err.to_string().contains("yok.mp4"));
    assert!(err.to_string().starts_with("Video bulunamadı: "));
    assert_eq!(stages, vec![none()]);
    assert!(tmp_leftovers(&out_dir).is_empty());
}

#[test]
fn test_marker_written_by_python_formatting_variants_is_accepted() {
    // Python `==` semantics: 1.0 == 1 == True; any JSON formatting (indent, key order) is read.
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeExtractor::default();
    let video = make_video(&tmp.path().join("in"), "Clip.mp4", b"video-bytes");
    let out_dir = tmp.path().join("out");
    let (wav, _) = extract(&fake, &video, &out_dir).unwrap();
    let id = reuse::source_identity(&video).unwrap();
    let wav_size = std::fs::metadata(&wav).unwrap().len();
    let text = format!(
        "{{\n  \"wav_size\": {wav_size}.0,\n  \"mtime_ns\": {},\n  \"size\": {},\n  \"source\": {},\n  \"version\": true\n}}",
        id.mtime_ns,
        id.size,
        json::dumps(&Value::from(id.source.as_str()), true)
    );
    std::fs::write(se_core::paths::marker_path(&wav), text).unwrap();
    assert!(reuse::wav_matches_source(&wav, Some(&id)));
    assert!(!reuse::wav_matches_source(&wav, None));
}

#[test]
fn test_marker_text_is_ascii_json() {
    let id = reuse::SourceIdentity { source: "C:\\Ü\\😀".into(), size: 1, mtime_ns: -5 };
    assert_eq!(
        reuse::marker_text(&id, 46),
        "{\"version\": 1, \"source\": \"C:\\\\\\u00dc\\\\\\ud83d\\ude00\", \"size\": 1, \"mtime_ns\": -5, \"wav_size\": 46}"
    );
}
