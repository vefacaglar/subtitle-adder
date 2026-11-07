//! `partial` snapshots (spec cli.md §5.6 step 15/16): throttled to one per 0.5 s (first call emits, the slot is
//! used even when the snapshot is empty), only for transcribers that support `on_words`, plus one unthrottled
//! flush of the complete ASR text before diarization.

mod common;

use std::cell::Cell;
use std::rc::Rc;
use std::path::Path;
use std::time::{Duration, Instant};

use serial_test::serial;

use common::*;
use se_asr::Transcriber;
use se_audio::AudioBackend;
use se_cli::Services;
use se_cli::pipeline::{self, RunOptions};
use se_core::{ProgressFn, Result, Sentence, Transcript, Word, WordsFn};
use se_diarize::{Diarizer, SpeakerSegment};

struct ClockServices {
    audio: FakeExtractor,
    start: Instant,
    offset_ms: Rc<Cell<u64>>,
}

impl Services for ClockServices {
    fn needs_download(&self, _: &str) -> bool {
        false
    }
    fn needs_cuda_download(&self) -> bool {
        false
    }
    fn audio(&self) -> &dyn AudioBackend {
        &self.audio
    }
    fn now(&self) -> Instant {
        self.start + Duration::from_millis(self.offset_ms.get())
    }
}

/// Calls `on_words` at the given clock offsets with growing word lists (an empty list at offset `None`).
struct Streaming {
    clock: Rc<Cell<u64>>,
    calls: Vec<(u64, usize)>,
    supports: bool,
}

impl Transcriber for Streaming {
    fn transcribe(&mut self, _: &Path, _: Option<&str>, _: Option<ProgressFn<'_>>, on_words: Option<WordsFn<'_>>) -> Result<Transcript> {
        let words = make_words("Bir. İki. Üç. Dört.");
        let mut on_words = on_words;
        for (at, n) in &self.calls {
            self.clock.set(*at);
            if let Some(f) = on_words.as_mut() {
                f(&words[..*n], if *n == 0 { "?" } else { "tr" });
            }
        }
        Ok(Transcript { language: "tr".into(), duration: 2.0, words })
    }
    fn supports_on_words(&self) -> bool {
        self.supports
    }
}

struct OneSpeaker;

impl Diarizer for OneSpeaker {
    fn diarize(&mut self, _: &Path, _: Option<u32>, on_progress: Option<ProgressFn<'_>>, _: Option<&[Word]>) -> Result<Vec<SpeakerSegment>> {
        if let Some(p) = on_progress {
            p(0.0);
            p(1.0);
        }
        Ok(vec![SpeakerSegment { start: 0.0, end: 9.0, speaker: 0 }])
    }
}

fn run_streaming(supports: bool, diarize: bool) -> (Vec<(Vec<String>, String)>, Vec<(String, Option<f64>)>) {
    let g = Guard::new();
    let tmp = g.path();
    let video = tmp.join("in").join("Clip.mp4");
    std::fs::create_dir_all(video.parent().unwrap()).unwrap();
    std::fs::write(&video, b"video").unwrap();
    let services =
        ClockServices { audio: FakeExtractor::new(ExtractMode::Fake), start: Instant::now(), offset_ms: Rc::new(Cell::new(0)) };
    let mut partials: Vec<(Vec<String>, String)> = Vec::new();
    let mut stages = Vec::new();
    {
        let mut on_partial =
            |s: &[Sentence], lang: &str| partials.push((s.iter().map(|x| x.text.clone()).collect(), lang.to_string()));
        let mut on_stage = |s: &str, f: Option<f64>| stages.push((s.to_string(), f));
        let clock = services.offset_ms.clone();
        // t=0 empty (slot used, nothing emitted), t=0.2 throttled, t=0.5 emits, t=0.9 throttled, t=1.0 emits.
        let calls = vec![(1000, 0), (1200, 1), (1500, 2), (1900, 3), (2000, 4)];
        let factory: pipeline::TranscriberFactory<'_> =
            Box::new(move || Ok(Box::new(Streaming { clock, calls, supports }) as Box<dyn Transcriber>));
        let options = RunOptions { diarize, ..RunOptions::default() };
        let diarizer: Option<pipeline::DiarizerFactory<'_>> =
            diarize.then(|| Box::new(|| Ok(Box::new(OneSpeaker) as Box<dyn Diarizer>)) as pipeline::DiarizerFactory<'_>);
        pipeline::run(&services, &video, factory, &tmp.join("out"), &options, &mut on_stage, diarizer, Some(&mut on_partial))
            .unwrap();
    }
    (partials, stages)
}

fn texts(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
#[serial]
fn test_partials_are_throttled_and_skip_empty_snapshots() {
    let (partials, _) = run_streaming(true, false);
    assert_eq!(
        partials,
        [(texts(&["Bir.", "İki."]), "tr".to_string()), (texts(&["Bir.", "İki.", "Üç.", "Dört."]), "tr".to_string())]
    );
}

#[test]
#[serial]
fn test_no_partials_without_on_words_support() {
    let (partials, _) = run_streaming(false, false);
    assert!(partials.is_empty());
}

#[test]
#[serial]
fn test_diarize_flushes_the_complete_text_before_the_diarizing_stage() {
    let (partials, stages) = run_streaming(false, true);
    assert_eq!(partials, [(texts(&["Bir.", "İki.", "Üç.", "Dört."]), "tr".to_string())]);
    let diarizing: Vec<Option<f64>> = stages.iter().filter(|(s, _)| s == "diarizing").map(|(_, f)| *f).collect();
    // The opening null, then the diarizer's 0.0 mapped to null, then 1.0.
    assert_eq!(diarizing, [None, None, Some(1.0)]);
    let order: Vec<&str> = stages.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(order.last(), Some(&"segmenting"));
}
