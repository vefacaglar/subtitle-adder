//! The CLI/pipeline tests of `tests/test_diarize.py` (the pure diarization tests live in se-diarize).

mod common;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serial_test::serial;

use common::*;
use se_asr::Transcriber;
use se_cli::pipeline::{self, RunOptions};
use se_core::paths::py_path_str;
use se_core::{ProgressFn, Result, Sentence, Transcript, Word, WordsFn};
use se_diarize::{Diarizer, SpeakerSegment};

const SPEECH: &str = "Selam nasılsın iyiyim sen";

/// test_diarize `FakeTranscriber` (no `on_words`).
struct DiarizeTranscriber;

impl Transcriber for DiarizeTranscriber {
    fn transcribe(&mut self, _: &Path, _: Option<&str>, _: Option<ProgressFn<'_>>, _: Option<WordsFn<'_>>) -> Result<Transcript> {
        Ok(Transcript { language: "tr".into(), duration: 2.0, words: make_words(SPEECH) })
    }
    fn supports_on_words(&self) -> bool {
        false
    }
}

/// test_diarize `FakeDiarizer` (optionally reporting 0.0 first, like `ReportingDiarizer`).
struct FakeDiarizer {
    calls: Rc<RefCell<Vec<Option<u32>>>>,
    report_zero: bool,
}

impl Diarizer for FakeDiarizer {
    fn diarize(
        &mut self,
        _: &Path,
        num_speakers: Option<u32>,
        on_progress: Option<ProgressFn<'_>>,
        _: Option<&[Word]>,
    ) -> Result<Vec<SpeakerSegment>> {
        self.calls.borrow_mut().push(num_speakers);
        if let Some(p) = on_progress {
            if self.report_zero {
                p(0.0);
            }
            p(0.5);
        }
        // words last 0.4 s each: the first three belong to speaker 0
        Ok(vec![
            SpeakerSegment { start: 0.0, end: 1.2, speaker: 4 },
            SpeakerSegment { start: 1.2, end: 2.0, speaker: 2 },
        ])
    }
}

/// A video file plus the fake extractor (the Python tests decode `tone_video`; the audio is irrelevant here).
fn fake_video(tmp: &Path) -> PathBuf {
    let video = tmp.join("in").join("tone.mp4");
    std::fs::create_dir_all(video.parent().unwrap()).unwrap();
    std::fs::write(&video, b"video").unwrap();
    video
}

fn transcriber_factory<'a>() -> pipeline::TranscriberFactory<'a> {
    Box::new(|| Ok(Box::new(DiarizeTranscriber) as Box<dyn Transcriber>))
}

#[test]
#[serial]
fn test_render_command_reads_speaker_from_stdin() {
    let _g = Guard::new();
    let payload = r#"[{"id": 1, "start": 0.0, "end": 1.0, "text": "Selam.", "words": [], "speaker": 0}, {"id": 2, "start": 1.0, "end": 2.0, "text": "Merhaba.", "words": [], "speaker": 1}]"#;
    let (code, out, _) = run_with_stdin(&Fake::default(), &["render", "-f", "srt"], payload.as_bytes());
    assert_eq!(code, 0);
    assert!(out.contains("- Selam.") && out.contains("- Merhaba."), "{out}");
}

#[test]
#[serial]
fn test_pipeline_diarize_emits_stages_and_speakers() {
    let g = Guard::new();
    let tmp = g.path();
    let video = fake_video(&tmp);
    let fake = Fake {
        needs: Some(Box::new(|id| id == se_diarize::MODEL_ID)),
        ensure_fractions: Some(vec![0.5, 1.0]),
        audio: FakeExtractor::new(ExtractMode::Fake),
        ..Fake::default()
    };
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut stages: Vec<(String, Option<f64>)> = Vec::new();
    let result = {
        let calls = calls.clone();
        let diarizer: pipeline::DiarizerFactory<'_> =
            Box::new(move || Ok(Box::new(FakeDiarizer { calls, report_zero: false }) as Box<dyn Diarizer>));
        let options = RunOptions { diarize: true, num_speakers: Some(2), ..RunOptions::default() };
        let mut on_stage = |s: &str, f: Option<f64>| stages.push((s.to_string(), f));
        pipeline::run(&fake, &video, transcriber_factory(), &tmp, &options, &mut on_stage, Some(diarizer), None).unwrap()
    };
    let names: Vec<&str> = stages.iter().map(|(s, _)| s.as_str()).collect();
    let index = |name: &str| names.iter().position(|n| *n == name).unwrap();
    assert!(index("downloading_model") < index("loading_model") && index("loading_model") < index("transcribing"));
    assert!(index("transcribing") < index("diarizing") && index("diarizing") < index("segmenting"));
    assert!(stages.contains(&("diarizing".to_string(), Some(0.5))));
    assert_eq!(*calls.borrow(), [Some(2)]);
    assert_eq!(fake.ensured.borrow().as_slice(), [se_diarize::MODEL_ID.to_string()]);
    let got: Vec<(&str, Option<i64>)> = result.sentences.iter().map(|x| (x.text.as_str(), x.speaker)).collect();
    assert_eq!(got, [("Selam nasılsın iyiyim", Some(0)), ("sen", Some(1))]);
}

#[test]
#[serial]
fn test_pipeline_without_diarize_leaves_speaker_null() {
    let g = Guard::new();
    let tmp = g.path();
    let video = fake_video(&tmp);
    let fake = Fake { audio: FakeExtractor::new(ExtractMode::Fake), ..Fake::default() };
    let mut on_stage = |_: &str, _: Option<f64>| {};
    let result =
        pipeline::run(&fake, &video, transcriber_factory(), &tmp, &RunOptions::default(), &mut on_stage, None, None)
            .unwrap();
    assert!(!result.sentences.is_empty());
    assert!(result.sentences.iter().all(|x| x.speaker.is_none()));
}

#[derive(Clone, Debug, PartialEq)]
enum Ev {
    Stage(String, Option<f64>),
    Partial(usize),
}

#[test]
#[serial]
fn test_complete_asr_text_and_indeterminate_stage_arrive_before_diarizer_load() {
    let g = Guard::new();
    let tmp = g.path();
    let video = fake_video(&tmp);
    let fake = Fake { audio: FakeExtractor::new(ExtractMode::Fake), ..Fake::default() };
    let events: RefCell<Vec<Ev>> = RefCell::new(Vec::new());
    {
        let load_diarizer: pipeline::DiarizerFactory<'_> = Box::new(|| {
            let events = events.borrow();
            assert_eq!(events.last(), Some(&Ev::Stage("diarizing".into(), None)));
            assert!(events.iter().any(|e| matches!(e, Ev::Partial(n) if *n > 0)));
            Ok(Box::new(FakeDiarizer { calls: Rc::default(), report_zero: true }) as Box<dyn Diarizer>)
        });
        let options = RunOptions { diarize: true, ..RunOptions::default() };
        let mut on_stage = |s: &str, f: Option<f64>| events.borrow_mut().push(Ev::Stage(s.into(), f));
        let mut on_partial = |s: &[Sentence], _: &str| events.borrow_mut().push(Ev::Partial(s.len()));
        pipeline::run(
            &fake,
            &video,
            transcriber_factory(),
            &tmp,
            &options,
            &mut on_stage,
            Some(load_diarizer),
            Some(&mut on_partial),
        )
        .unwrap();
    }
    let events = events.into_inner();
    assert!(!events.contains(&Ev::Stage("diarizing".into(), Some(0.0))));
    assert!(events.contains(&Ev::Stage("diarizing".into(), Some(0.5))));
}

#[test]
#[serial]
fn test_cli_diarize_flag_and_speakers_validation() {
    let g = Guard::new();
    let tmp = g.path();
    let video = py_path_str(&fake_video(&tmp));
    let out_dir = py_path_str(&tmp);
    let calls = Rc::new(RefCell::new(Vec::new()));
    let maker_calls = calls.clone();
    let fake = Fake {
        text: SPEECH,
        audio: FakeExtractor::new(ExtractMode::Fake),
        diarizer: Some(Box::new(move || {
            Ok(Box::new(FakeDiarizer { calls: maker_calls.clone(), report_zero: false }) as Box<dyn Diarizer>)
        })),
        ..Fake::default()
    };
    let argv = ["transcribe", video.as_str(), "-o", out_dir.as_str(), "--json"];

    let (code, out, err) = run(&fake, &[&argv[..], &["--diarize", "--speakers", "2"]].concat());
    assert_eq!(code, 0, "{out}{err}");
    let evs = events(&out);
    assert!(stages(&evs).iter().any(|(s, _)| s == "diarizing"));
    let speakers: Vec<Option<i64>> = evs
        .last()
        .unwrap()
        .get("sentences")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.get("speaker").and_then(se_core::json::Value::as_i64))
        .collect();
    assert_eq!(speakers, [Some(0), Some(1)]);
    assert_eq!(*calls.borrow(), [Some(2)]);

    let (code, out, _) = run(&fake, &[&argv[..], &["--speakers", "2"]].concat()); // --speakers needs --diarize
    assert_eq!(code, 1);
    assert_eq!(ev_str(events(&out).last().unwrap(), "event"), Some("error"));
}

#[test]
#[serial]
fn test_transcribe_rejects_non_transcription_catalog_models() {
    let g = Guard::new();
    let tmp = g.path();
    let video = py_path_str(&fake_video(&tmp));
    let out_dir = py_path_str(&tmp);
    // ensure_fractions None: ensure_model panics ("must fail before any download"); extraction is forbidden.
    let fake = Fake { audio: FakeExtractor::new(ExtractMode::Forbidden), ..Fake::default() };
    for model in ["speaker-diarization", "madlad-3b"] {
        let (code, out, _) = run(&fake, &["transcribe", video.as_str(), "-o", out_dir.as_str(), "-m", model, "--json"]);
        assert_eq!(code, 1);
        let evs = events(&out);
        assert_eq!(evs.len(), 1, "{out}");
        let event = evs.last().unwrap();
        assert_eq!(ev_str(event, "event"), Some("error"));
        assert!(ev_str(event, "message").unwrap().contains("konuşma tanıma modeli değil"), "{out}");
    }
}

#[test]
#[serial]
fn test_translate_rejects_non_translation_models() {
    let _g = Guard::new();
    let fake = Fake { translator: Some(Box::new(|_| panic!("must not load anything"))), ..Fake::default() };
    let (code, out, _) = run_with_stdin(&fake, &["translate", "--to", "es", "-m", "speaker-diarization", "--json"], b"[]");
    assert_eq!(code, 1);
    assert_eq!(ev_str(events(&out).last().unwrap(), "event"), Some("error"));
}
