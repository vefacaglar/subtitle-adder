//! Port of `tests/test_pipeline_audio_reuse.py`: `extract_video_audio` reuses a WAV only when it was extracted
//! from the very same video, and `pipeline.run` keeps using its own audio.

mod common;

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use filetime::FileTime;
use serial_test::serial;

use common::*;
use se_asr::Transcriber;
use se_cli::pipeline::{self, RunOptions};
use se_core::json::Value;
use se_core::paths::{py_path_str, resolve, wav_path_for};
use se_core::{EngineError, ProgressFn, Result, Transcript, Word, WordsFn};
use se_diarize::{Diarizer, SpeakerSegment};

type Stages = Vec<(String, Option<f64>)>;

fn fake_services() -> Fake {
    Fake { audio: FakeExtractor::new(ExtractMode::Fake), text: "Merhaba dünya.", ..Fake::default() }
}

fn make_video(folder: &Path, name: &str, content: &[u8]) -> PathBuf {
    std::fs::create_dir_all(folder).unwrap();
    let path = folder.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

fn clip(folder: &Path) -> PathBuf {
    make_video(folder, "Clip.mp4", b"video-bytes")
}

fn extract(fake: &Fake, video: &Path, out_dir: &Path) -> Result<(PathBuf, Stages)> {
    let mut stages: Stages = Vec::new();
    let mut on_stage = |s: &str, f: Option<f64>| stages.push((s.to_string(), f));
    let wav = pipeline::extract_video_audio(fake, video, out_dir, &mut on_stage)?;
    Ok((wav, stages))
}

fn ok_extract(fake: &Fake, video: &Path, out_dir: &Path) -> (PathBuf, Stages) {
    extract(fake, video, out_dir).unwrap()
}

fn marker_file(wav: &Path) -> PathBuf {
    let mut name = wav.file_name().unwrap().to_os_string();
    name.push(".source.json");
    wav.with_file_name(name)
}

fn marker_of(wav: &Path) -> Value {
    se_core::json::parse(&std::fs::read_to_string(marker_file(wav)).unwrap()).unwrap()
}

fn mtime_ns(path: &Path) -> i128 {
    let t = FileTime::from_last_modification_time(&std::fs::metadata(path).unwrap());
    i128::from(t.unix_seconds()) * 1_000_000_000 + i128::from(t.nanoseconds())
}

fn set_mtime_ns(path: &Path, ns: i128) {
    let secs = ns.div_euclid(1_000_000_000) as i64;
    let nanos = ns.rem_euclid(1_000_000_000) as u32;
    filetime::set_file_mtime(path, FileTime::from_unix_time(secs, nanos)).unwrap();
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    v.sort();
    v
}

fn no_tmp(dir: &Path) -> bool {
    names(dir).iter().all(|n| !n.contains(".tmp"))
}

fn first(stages: &Stages) -> (String, Option<f64>) {
    stages[0].clone()
}

fn null_stage() -> (String, Option<f64>) {
    ("extracting_audio".into(), None)
}

fn reuse_stage() -> Stages {
    vec![("extracting_audio".into(), Some(1.0))]
}

#[test]
#[serial]
fn test_real_extraction_writes_identity_marker() {
    let g = Guard::new();
    let tmp = g.path();
    let Some(tone) = tone_video(&tmp) else { return };
    let out_dir = tmp.join("out");
    let fake = Fake::default(); // real FFmpeg extraction
    let (wav, stages) = ok_extract(&fake, &tone, &out_dir);
    let marker = marker_of(&wav);
    assert_eq!(wav, wav_path_for(&tone, &out_dir));
    let name = wav.file_name().unwrap().to_str().unwrap();
    assert!(name.len() == "tone-".len() + 8 + 4 && name.starts_with("tone-") && name.ends_with(".wav"));
    assert!(name[5..13].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_eq!(marker.get("source").unwrap().as_str(), Some(py_path_str(&resolve(&tone)).as_str()));
    assert_eq!(marker.get("size").unwrap().as_i64(), Some(std::fs::metadata(&tone).unwrap().len() as i64));
    assert_eq!(marker.get("mtime_ns").unwrap().py_str(), mtime_ns(&tone).to_string());
    assert_eq!(marker.get("wav_size").unwrap().as_i64(), Some(std::fs::metadata(&wav).unwrap().len() as i64));
    assert_eq!(first(&stages), null_stage());
    assert_eq!(stages.last().unwrap(), &("extracting_audio".to_string(), Some(1.0)));
    let mut expected = vec![name.to_string(), format!("{name}.source.json")];
    expected.sort();
    assert_eq!(names(&out_dir), expected);
}

#[test]
#[serial]
fn test_matching_marker_reuses_wav_with_single_stage() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    let (a, a_stages) = ok_extract(&fake, &video, &out_dir);
    let (b, b_stages) = ok_extract(&fake, &video, &out_dir);
    assert_eq!(a, b);
    assert_eq!(a, wav_path_for(&video, &out_dir));
    assert_eq!(first(&a_stages), null_stage());
    assert_eq!(b_stages, reuse_stage());
    assert_eq!(*fake.audio.calls.borrow(), [video]);
}

#[test]
#[serial]
fn test_same_stem_in_another_folder_gets_its_own_wav_and_both_stay_reusable() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let va = make_video(&tmp.join("S01"), "Episode 1.mkv", b"video-A");
    let vb = make_video(&tmp.join("S02"), "Episode 1.mkv", b"video-B-is-longer");
    let out_dir = tmp.join("out");
    let (wa, _) = ok_extract(&fake, &va, &out_dir);
    let (wb, sb) = ok_extract(&fake, &vb, &out_dir);
    assert_ne!(wa, wb);
    for w in [&wa, &wb] {
        assert!(w.file_name().unwrap().to_str().unwrap().starts_with("Episode 1-"));
    }
    assert!(contains(&std::fs::read(&wa).unwrap(), &audio_of(&va)));
    assert!(contains(&std::fs::read(&wb).unwrap(), &audio_of(&vb)));
    assert_eq!(first(&sb), null_stage());
    assert_eq!(marker_of(&wa).get("source").unwrap().as_str(), Some(py_path_str(&resolve(&va)).as_str()));
    assert_eq!(marker_of(&wb).get("source").unwrap().as_str(), Some(py_path_str(&resolve(&vb)).as_str()));
    let (again, s_again) = ok_extract(&fake, &va, &out_dir);
    assert_eq!(again, wa);
    assert_eq!(s_again, reuse_stage());
    assert_eq!(*fake.audio.calls.borrow(), [va, vb]);
}

#[test]
#[serial]
fn test_same_stem_different_extension_in_one_folder_gets_its_own_wav() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let mkv = make_video(&tmp.join("in"), "Clip.mkv", b"video-mkv");
    let mp4 = make_video(&tmp.join("in"), "Clip.mp4", b"video-mp4-longer");
    let out_dir = tmp.join("out");
    let (a, _) = ok_extract(&fake, &mkv, &out_dir);
    let (b, _) = ok_extract(&fake, &mp4, &out_dir);
    assert_ne!(a, b);
    assert!(contains(&std::fs::read(&a).unwrap(), &audio_of(&mkv)));
    assert!(contains(&std::fs::read(&b).unwrap(), &audio_of(&mp4)));
}

#[test]
#[serial]
fn test_wav_name_hash_follows_the_resolved_path() {
    let g = Guard::new();
    let tmp = g.path();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    let alias = tmp.join("in").join("..").join("in").join("Clip.mp4");
    let other = clip(&tmp.join("other"));
    assert_eq!(wav_path_for(&alias, &out_dir), wav_path_for(&video, &out_dir));
    assert_ne!(wav_path_for(&other, &out_dir), wav_path_for(&video, &out_dir));
}

#[test]
#[serial]
fn test_long_stem_keeps_the_wav_and_its_companions_inside_the_path_budget() {
    let g = Guard::new();
    let tmp = g.path();
    let out_dir = tmp.join("out");
    let video = tmp.join("in").join(format!("{}.mp4", "x".repeat(215)));
    let wav = wav_path_for(&video, &out_dir);
    let len = |p: &Path| py_path_str(p).chars().count();
    assert!(len(&wav) <= se_core::paths::MAX_SAFE_PATH_CHARS);
    let tmp_wav = se_core::paths::tmp_wav_path(&wav, 4_194_304);
    let marker_tmp = se_core::paths::marker_tmp_path(&wav, 4_194_304);
    assert!(len(&tmp_wav).max(len(&marker_tmp)) < 260);
    let name = wav.file_name().unwrap().to_str().unwrap();
    let hash = &name[name.len() - 12..name.len() - 4];
    assert!(name.ends_with(".wav") && name.as_bytes()[name.len() - 13] == b'-');
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
#[serial]
fn test_same_size_and_mtime_in_another_folder_is_still_a_different_source() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let va = make_video(&tmp.join("A"), "Clip.mp4", b"same-size-A");
    let vb = make_video(&tmp.join("B"), "Clip.mp4", b"same-size-B");
    set_mtime_ns(&vb, mtime_ns(&va));
    let out_dir = tmp.join("out");
    let (wa, _) = ok_extract(&fake, &va, &out_dir);
    let (wb, _) = ok_extract(&fake, &vb, &out_dir);
    assert_ne!(wa, wb);
    assert_eq!(*fake.audio.calls.borrow(), [va.clone(), vb.clone()]);
    assert!(contains(&std::fs::read(&wb).unwrap(), &audio_of(&vb)));
    assert!(contains(&std::fs::read(&wa).unwrap(), &audio_of(&va)));
}

#[test]
#[serial]
fn test_changed_size_re_extracts() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    ok_extract(&fake, &video, &out_dir);
    let mtime = mtime_ns(&video);
    std::fs::write(&video, b"re-encoded video with a different size").unwrap();
    set_mtime_ns(&video, mtime);
    let (wav, stages) = ok_extract(&fake, &video, &out_dir);
    assert_eq!(first(&stages), null_stage());
    assert_eq!(*fake.audio.calls.borrow(), [video.clone(), video.clone()]);
    assert_eq!(marker_of(&wav).get("size").unwrap().as_i64(), Some(std::fs::metadata(&video).unwrap().len() as i64));
}

#[test]
#[serial]
fn test_changed_mtime_re_extracts() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    ok_extract(&fake, &video, &out_dir);
    set_mtime_ns(&video, mtime_ns(&video) + 5_000_000_000);
    let (wav, stages) = ok_extract(&fake, &video, &out_dir);
    assert_eq!(first(&stages), null_stage());
    assert_eq!(fake.audio.calls.borrow().len(), 2);
    assert_eq!(marker_of(&wav).get("mtime_ns").unwrap().py_str(), mtime_ns(&video).to_string());
}

#[test]
#[serial]
fn test_wav_without_marker_is_re_extracted() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let stale = wav_path_for(&video, &out_dir);
    std::fs::write(&stale, [0u8; 200]).unwrap();
    let (wav, stages) = ok_extract(&fake, &video, &out_dir);
    assert_eq!(wav, stale);
    assert_eq!(first(&stages), null_stage());
    assert_eq!(*fake.audio.calls.borrow(), [video.clone()]);
    assert!(contains(&std::fs::read(&wav).unwrap(), &audio_of(&video)));
    assert_eq!(marker_of(&wav).get("source").unwrap().as_str(), Some(py_path_str(&resolve(&video)).as_str()));
}

#[test]
#[serial]
fn test_wav_left_by_the_older_stem_only_naming_is_ignored_and_untouched() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let legacy = out_dir.join("Clip.wav");
    std::fs::write(&legacy, [0u8; 200]).unwrap();
    let (wav, stages) = ok_extract(&fake, &video, &out_dir);
    assert_ne!(wav, legacy);
    assert_eq!(first(&stages), null_stage());
    assert_eq!(std::fs::read(&legacy).unwrap(), vec![0u8; 200]);
}

#[test]
#[serial]
fn test_unreadable_or_incomplete_marker_is_re_extracted() {
    for garbage in ["not json", "[]", r#"{"source": "x"}"#, ""] {
        let g = Guard::new();
        let tmp = g.path();
        let fake = fake_services();
        let video = clip(&tmp.join("in"));
        let out_dir = tmp.join("out");
        let (wav, _) = ok_extract(&fake, &video, &out_dir);
        std::fs::write(marker_file(&wav), garbage).unwrap();
        let (_, stages) = ok_extract(&fake, &video, &out_dir);
        assert_eq!(first(&stages), null_stage(), "{garbage:?}");
        assert_eq!(fake.audio.calls.borrow().len(), 2, "{garbage:?}");
    }
}

#[test]
#[serial]
fn test_tiny_wav_is_not_reused_even_with_matching_marker() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    let (wav, _) = ok_extract(&fake, &video, &out_dir);
    std::fs::write(&wav, [0u8; 44]).unwrap();
    let (_, stages) = ok_extract(&fake, &video, &out_dir);
    assert_eq!(first(&stages), null_stage());
    assert_eq!(fake.audio.calls.borrow().len(), 2);
}

#[test]
#[serial]
fn test_wav_replaced_behind_the_markers_back_is_not_reused() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    let (wav, _) = ok_extract(&fake, &video, &out_dir);
    std::fs::write(&wav, [0u8; 500]).unwrap();
    let (_, stages) = ok_extract(&fake, &video, &out_dir);
    assert_eq!(first(&stages), null_stage());
    assert_eq!(fake.audio.calls.borrow().len(), 2);
}

#[test]
#[serial]
fn test_unwritable_marker_does_not_crash_and_wav_is_not_reused() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let expected = wav_path_for(&video, &out_dir);
    std::fs::create_dir(marker_file(&expected)).unwrap();
    let (wav, stages) = ok_extract(&fake, &video, &out_dir);
    assert!(wav == expected && wav.is_file());
    assert_eq!(first(&stages), null_stage());
    assert!(no_tmp(&out_dir));
    let (_, again) = ok_extract(&fake, &video, &out_dir);
    assert_eq!(first(&again), null_stage());
    assert_eq!(fake.audio.calls.borrow().len(), 2);
}

#[test]
#[serial]
fn test_failed_extraction_cleans_temp_file_and_keeps_previous_wav_and_marker() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = make_video(&tmp.join("in"), "Clip.mp4", b"video-v1");
    let out_dir = tmp.join("out");
    let (wav, _) = ok_extract(&fake, &video, &out_dir);
    let before = std::fs::read(&wav).unwrap();
    let old_marker = marker_of(&wav);
    std::fs::write(&video, b"video-v2-is-longer").unwrap();
    fake.audio.mode.set(ExtractMode::Failing);
    let err = extract(&fake, &video, &out_dir).unwrap_err();
    assert!(err.to_string().contains("decode failed"));
    assert!(no_tmp(&out_dir));
    assert_eq!(std::fs::read(&wav).unwrap(), before);
    assert_eq!(marker_of(&wav), old_marker);
}

fn is_tmp_wav(p: &Path) -> bool {
    p.file_name().unwrap().to_string_lossy().ends_with(".tmp.wav")
}

#[test]
#[serial]
fn test_locked_target_keeps_old_wav_and_marker_and_returns_own_audio() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = make_video(&tmp.join("in"), "Clip.mp4", b"video-v1");
    let out_dir = tmp.join("out");
    let (wav, _) = ok_extract(&fake, &video, &out_dir);
    let (old_bytes, old_marker) = (std::fs::read(&wav).unwrap(), marker_of(&wav));
    std::fs::write(&video, b"video-v2-is-longer").unwrap();
    *fake.audio.replace_hook.borrow_mut() = Some(Box::new(|src: &Path, _dst: &Path| {
        is_tmp_wav(src).then(|| Err(io::Error::new(io::ErrorKind::PermissionDenied, "target is in use")))
    }));
    let (result, _) = ok_extract(&fake, &video, &out_dir);
    assert!(result != wav && is_tmp_wav(&result));
    assert!(contains(&std::fs::read(&result).unwrap(), b"audio-of:"));
    assert_eq!(std::fs::read(&wav).unwrap(), old_bytes);
    assert_eq!(marker_of(&wav), old_marker);
}

#[test]
#[serial]
fn test_locked_target_already_produced_by_another_process_for_this_video_is_reused() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let wav = wav_path_for(&video, &out_dir);
    let (hook_wav, hook_video) = (wav.clone(), video.clone());
    *fake.audio.replace_hook.borrow_mut() = Some(Box::new(move |src: &Path, _dst: &Path| {
        if !is_tmp_wav(src) {
            return None;
        }
        let mut body = vec![0u8; 64];
        body.extend_from_slice(b"written-by-the-other-process");
        std::fs::write(&hook_wav, &body).unwrap();
        let identity = se_audio::reuse::source_identity(&hook_video);
        se_audio::reuse::write_source_marker(&hook_wav, identity.as_ref(), body.len() as i64);
        Some(Err(io::Error::new(io::ErrorKind::PermissionDenied, "target is in use")))
    }));
    let (result, _) = ok_extract(&fake, &video, &out_dir);
    assert_eq!(result, wav);
    assert!(contains(&std::fs::read(&wav).unwrap(), b"written-by-the-other-process"));
    assert!(names(&out_dir).iter().all(|n| !n.ends_with(".tmp.wav")));
}

#[test]
#[serial]
fn test_missing_video_still_reports_extraction_error_and_never_reuses_a_wav() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = Fake::default(); // real extractor
    let out_dir = tmp.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let missing = tmp.join("yok.mp4");
    std::fs::write(wav_path_for(&missing, &out_dir), [0u8; 200]).unwrap();
    let err = extract(&fake, &missing, &out_dir).unwrap_err();
    assert!(err.is(se_core::ErrorKind::AudioExtraction), "{err:?}");
    assert!(err.to_string().contains("yok.mp4"));
    assert!(no_tmp(&out_dir));
}

// ---------------------------------------------------------------------------------------------------------
// pipeline.run

/// `RecordingTranscriber`: records the audio bytes it was given.
struct Recording {
    seen: Rc<RefCell<Vec<Vec<u8>>>>,
}

fn transcript() -> Transcript {
    Transcript { language: "tr".into(), duration: 2.0, words: make_words("Merhaba dünya.") }
}

impl Transcriber for Recording {
    fn transcribe(&mut self, audio: &Path, _: Option<&str>, _: Option<ProgressFn<'_>>, _: Option<WordsFn<'_>>) -> Result<Transcript> {
        self.seen.borrow_mut().push(std::fs::read(audio).unwrap());
        Ok(transcript())
    }
    fn supports_on_words(&self) -> bool {
        false
    }
}

/// Runs `action(audio)` during transcription, then returns the usual transcript.
struct Hooked<F: FnMut(&Path)> {
    action: F,
}

impl<F: FnMut(&Path)> Transcriber for Hooked<F> {
    fn transcribe(&mut self, audio: &Path, _: Option<&str>, _: Option<ProgressFn<'_>>, _: Option<WordsFn<'_>>) -> Result<Transcript> {
        (self.action)(audio);
        Ok(transcript())
    }
    fn supports_on_words(&self) -> bool {
        false
    }
}

/// `Boom`: transcription is cancelled.
struct Boom;

impl Transcriber for Boom {
    fn transcribe(&mut self, _: &Path, _: Option<&str>, _: Option<ProgressFn<'_>>, _: Option<WordsFn<'_>>) -> Result<Transcript> {
        Err(EngineError::runtime("cancelled"))
    }
    fn supports_on_words(&self) -> bool {
        false
    }
}

struct RecordingDiarizer {
    seen: Rc<RefCell<Vec<(PathBuf, Vec<u8>)>>>,
}

impl Diarizer for RecordingDiarizer {
    fn diarize(&mut self, audio: &Path, _: Option<u32>, _: Option<ProgressFn<'_>>, _: Option<&[Word]>) -> Result<Vec<SpeakerSegment>> {
        assert!(audio.is_file());
        self.seen.borrow_mut().push((audio.to_path_buf(), std::fs::read(audio).unwrap()));
        Ok(vec![SpeakerSegment { start: 0.0, end: 2.0, speaker: 0 }])
    }
}

fn run_pipeline(
    fake: &Fake,
    video: &Path,
    factory: pipeline::TranscriberFactory<'_>,
    out_dir: &Path,
    options: &RunOptions,
    diarizer: Option<pipeline::DiarizerFactory<'_>>,
) -> Result<(pipeline::PipelineResult, Stages)> {
    let mut stages: Stages = Vec::new();
    let mut on_stage = |s: &str, f: Option<f64>| stages.push((s.to_string(), f));
    let result = pipeline::run(fake, video, factory, out_dir, options, &mut on_stage, diarizer, None)?;
    Ok((result, stages))
}

#[test]
#[serial]
fn test_run_after_a_failed_run_of_another_video_with_the_same_stem_uses_its_own_audio() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let va = make_video(&tmp.join("A"), "Clip.mp4", b"video-A");
    let vb = make_video(&tmp.join("B"), "Clip.mkv", b"video-B-is-longer");
    let out_dir = tmp.join("out");
    let boom: pipeline::TranscriberFactory<'_> = Box::new(|| Ok(Box::new(Boom) as Box<dyn Transcriber>));
    assert!(run_pipeline(&fake, &va, boom, &out_dir, &RunOptions::default(), None).is_err());
    assert!(wav_path_for(&va, &out_dir).is_file());

    let seen = Rc::new(RefCell::new(Vec::new()));
    let s2 = seen.clone();
    let (_, stages) = run_pipeline(
        &fake,
        &vb,
        Box::new(move || Ok(Box::new(Recording { seen: s2 }) as Box<dyn Transcriber>)),
        &out_dir,
        &RunOptions::default(),
        None,
    )
    .unwrap();
    assert_eq!(*fake.audio.calls.borrow(), [va.clone(), vb.clone()]);
    assert!(contains(&seen.borrow()[0], &audio_of(&vb)));
    assert_eq!(first(&stages), null_stage());
    assert!(wav_path_for(&va, &out_dir).is_file());
}

#[test]
#[serial]
fn test_transcription_failure_propagates_and_keeps_the_wav() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    let err = run_pipeline(&fake, &video, Box::new(|| Ok(Box::new(Boom) as Box<dyn Transcriber>)), &out_dir, &RunOptions::default(), None)
        .unwrap_err();
    assert_eq!(err.to_string(), "cancelled");
    assert!(wav_path_for(&video, &out_dir).is_file());
}

#[test]
#[serial]
fn test_same_stem_extraction_between_audio_step_and_transcription_does_not_change_the_audio() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let va = make_video(&tmp.join("S01"), "Episode 1.mkv", b"video-A");
    let vb = make_video(&tmp.join("S02"), "Episode 1.mkv", b"video-B-is-longer");
    let out_dir = tmp.join("out");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let s2 = seen.clone();
    let factory: pipeline::TranscriberFactory<'_> = Box::new(|| {
        ok_extract(&fake, &vb, &out_dir);
        Ok(Box::new(Recording { seen: s2 }) as Box<dyn Transcriber>)
    });
    let (result, _) = run_pipeline(&fake, &va, factory, &out_dir, &RunOptions::default(), None).unwrap();
    assert_eq!(*fake.audio.calls.borrow(), [va.clone(), vb.clone()]);
    assert!(contains(&seen.borrow()[0], &audio_of(&va)));
    assert!(!contains(&seen.borrow()[0], &audio_of(&vb)));
    assert_eq!(result.audio, wav_path_for(&va, &out_dir));
}

#[test]
#[serial]
fn test_same_stem_extraction_during_transcription_does_not_change_the_diarized_audio() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let va = make_video(&tmp.join("S01"), "Episode 1.mkv", b"video-A");
    let vb = make_video(&tmp.join("S02"), "Episode 1.mkv", b"video-B-is-longer");
    let out_dir = tmp.join("out");
    let (vb2, out2) = (vb.clone(), out_dir.clone());
    let racing: pipeline::TranscriberFactory<'_> = Box::new(move || {
        Ok(Box::new(Hooked {
            action: move |_: &Path| {
                let other = fake_services();
                ok_extract(&other, &vb2, &out2);
            },
        }) as Box<dyn Transcriber>)
    });
    let seen = Rc::new(RefCell::new(Vec::new()));
    let s2 = seen.clone();
    let options = RunOptions { diarize: true, ..RunOptions::default() };
    run_pipeline(
        &fake,
        &va,
        racing,
        &out_dir,
        &options,
        Some(Box::new(move || Ok(Box::new(RecordingDiarizer { seen: s2 }) as Box<dyn Diarizer>))),
    )
    .unwrap();
    assert!(contains(&seen.borrow()[0].1, &audio_of(&va)));
}

#[test]
#[serial]
fn test_wav_removed_while_loading_the_model_is_re_extracted_atomically_with_a_marker() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let s2 = seen.clone();
    let factory: pipeline::TranscriberFactory<'_> = Box::new(|| {
        std::fs::remove_file(wav_path_for(&video, &out_dir)).unwrap();
        Ok(Box::new(Recording { seen: s2 }) as Box<dyn Transcriber>)
    });
    let (result, stages) = run_pipeline(&fake, &video, factory, &out_dir, &RunOptions::default(), None).unwrap();
    assert_eq!(*fake.audio.calls.borrow(), [video.clone(), video.clone()]);
    assert!(fake.audio.targets.borrow().iter().all(|t| is_tmp_wav(t)));
    assert_eq!(result.audio, wav_path_for(&video, &out_dir));
    assert!(contains(&seen.borrow()[0], &audio_of(&video)));
    assert_eq!(marker_of(&result.audio).get("source").unwrap().as_str(), Some(py_path_str(&resolve(&video)).as_str()));
    assert!(no_tmp(&out_dir));
    let loading = stages.iter().position(|s| *s == ("loading_model".to_string(), None)).unwrap();
    let extracting = loading + stages[loading..].iter().position(|s| *s == null_stage()).unwrap();
    let transcribing = stages.iter().position(|s| *s == ("transcribing".to_string(), Some(0.0))).unwrap();
    assert!(loading < extracting && extracting < transcribing);
}

#[test]
#[serial]
fn test_wav_removed_during_transcription_is_re_extracted_before_diarization() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    let cleaned: pipeline::TranscriberFactory<'_> = Box::new(|| {
        Ok(Box::new(Hooked { action: |audio: &Path| std::fs::remove_file(audio).unwrap() }) as Box<dyn Transcriber>)
    });
    let seen = Rc::new(RefCell::new(Vec::new()));
    let s2 = seen.clone();
    let options = RunOptions { diarize: true, ..RunOptions::default() };
    let (result, _) = run_pipeline(
        &fake,
        &video,
        cleaned,
        &out_dir,
        &options,
        Some(Box::new(move || Ok(Box::new(RecordingDiarizer { seen: s2 }) as Box<dyn Diarizer>))),
    )
    .unwrap();
    assert_eq!(*fake.audio.calls.borrow(), [video.clone(), video.clone()]);
    assert!(fake.audio.targets.borrow().iter().all(|t| is_tmp_wav(t)));
    let diarized: Vec<PathBuf> = seen.borrow().iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(diarized, [wav_path_for(&video, &out_dir)]);
    assert_eq!(diarized, [result.audio.clone()]);
    assert_eq!(marker_of(&result.audio).get("source").unwrap().as_str(), Some(py_path_str(&resolve(&video)).as_str()));
    assert!(no_tmp(&out_dir));
    assert!(result.sentences.iter().all(|s| s.speaker == Some(0)));
}

#[test]
#[serial]
fn test_subtitle_write_failure_falls_back_to_a_pid_named_file() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = fake_services();
    let video = clip(&tmp.join("in"));
    let out_dir = tmp.join("out");
    std::fs::create_dir_all(out_dir.join("Clip.srt")).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let options = RunOptions { formats: vec!["srt".into(), "vtt".into()], ..RunOptions::default() };
    let (result, _) = run_pipeline(
        &fake,
        &video,
        Box::new(move || Ok(Box::new(Recording { seen }) as Box<dyn Transcriber>)),
        &out_dir,
        &options,
        None,
    )
    .unwrap();
    let fallback = out_dir.join(format!("Clip.{}.srt", std::process::id()));
    assert_eq!(result.outputs, [fallback.clone(), out_dir.join("Clip.vtt")]);
    assert!(std::fs::read_to_string(&fallback).unwrap().contains("Merhaba"));
    assert!(out_dir.join("Clip.vtt").is_file() && out_dir.join("Clip.srt").is_dir());
}
