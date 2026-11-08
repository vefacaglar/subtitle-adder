//! End to end with the real staged engine (`engine/dist/subtitle-engine[.exe]`; `SUBTITLE_ENGINE` is ignored). Runs
//! only with `ENGINE_E2E=1` (C# RealEngineTests). Needs no downloaded model: models / render / errors run without
//! speech recognition, and strict offline mode makes sure nothing is downloaded.

use std::collections::HashMap;
use std::sync::Arc;

use sa_core::cancel::CancellationToken;
use sa_core::diagnostics::NullDiagnostics;
use sa_core::engine_types::{EngineError, Sentence, SubtitleFormat};
use sa_core::l10n::UiLanguage;
use sa_enginekit::events::EngineEvent;
use sa_enginekit::launch::EngineLaunchSettings;
use sa_enginekit::{EngineClient, EngineCommand, ProcessEngineClient, TranscribeOptions};

fn engine(language: UiLanguage) -> Option<ProcessEngineClient> {
    if std::env::var("ENGINE_E2E").ok().as_deref() != Some("1") {
        eprintln!("skipped: set ENGINE_E2E=1");
        return None;
    }
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.retain(|k, _| !k.eq_ignore_ascii_case("SUBTITLE_ENGINE"));
    let command = EngineCommand::locate(&env, std::env::current_exe().ok().as_deref()).expect("staged engine");
    let mut client = ProcessEngineClient::new(command, sa_enginekit::process::platform_process_control(), Arc::new(NullDiagnostics));
    client.language = Some(language);
    client.launch_settings = Some(EngineLaunchSettings { offline: true, external_llm: None });
    Some(client)
}

fn spoken(id: i64, start: f64, end: f64, text: &str, speaker: i64) -> Sentence {
    let mut s = Sentence::new(id, start, end, text);
    s.speaker = Some(speaker);
    s
}

#[test]
fn lists_catalog() {
    let Some(engine) = engine(UiLanguage::Tr) else { return };
    let models = engine.models(&CancellationToken::new()).unwrap();
    let get = |id: &str| models.iter().find(|m| m.id == id).unwrap_or_else(|| panic!("{id}"));
    assert!(get("small").languages.is_none());
    let _ = get("parakeet-v3");
    assert!(get("speaker-diarization").is_diarization());
    assert_eq!(get("speaker-diarization").license.as_deref(), Some("MIT AND Apache-2.0"));
    assert!(get("nllb-1.3b").has_non_commercial_license());
    assert!(get("madlad-3b").is_translation());
    assert_eq!(get("gigaam-v3").languages, Some(vec!["ru".to_owned()]));
    assert!(get("qwen3.5-4b").is_llama());
}

#[test]
fn renders_srt_with_speaker_dashes() {
    let Some(engine) = engine(UiLanguage::Tr) else { return };
    let sentences = [
        spoken(1, 0.0, 1.5, "Merhaba, nasılsın?", 0),
        spoken(2, 1.5, 3.0, "İyiyim, sağ ol.", 1),
        spoken(3, 3.0, 4.0, "Sen nasılsın?", 1),
    ];
    let srt = engine.render(&sentences, SubtitleFormat::Srt, &CancellationToken::new()).unwrap().replace("\r\n", "\n");
    assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:01,500\n- Merhaba, nasılsın?\n"), "{srt}");
    assert!(srt.contains("- İyiyim, sağ ol."));
    assert!(srt.contains("\nSen nasılsın?"));
    assert!(!srt.contains("- Sen nasılsın?"));
}

#[test]
fn renders_vtt_without_dashes_for_one_speaker() {
    let Some(engine) = engine(UiLanguage::En) else { return };
    let vtt = engine.render(&[spoken(1, 0.0, 1.0, "A.", 0), spoken(2, 1.0, 2.0, "B.", 0)], SubtitleFormat::Vtt, &CancellationToken::new()).unwrap();
    assert!(vtt.starts_with("WEBVTT"), "{vtt}");
    assert!(!vtt.contains("- "));
}

#[test]
fn deleting_an_unknown_model_is_a_localized_error() {
    let (Some(tr), Some(en)) = (engine(UiLanguage::Tr), engine(UiLanguage::En)) else { return };
    let (Err(EngineError::Failed(t)), Err(EngineError::Failed(e))) =
        (tr.delete_model("not-a-model", &CancellationToken::new()), en.delete_model("not-a-model", &CancellationToken::new()))
    else {
        panic!()
    };
    assert!(!t.is_empty());
    assert_ne!(t, e);
}

#[test]
fn transcribe_rejects_a_non_transcription_model_before_any_download() {
    let Some(engine) = engine(UiLanguage::En) else { return };
    let video = std::env::temp_dir().join("missing-video.mp4");
    let options = TranscribeOptions { model: "speaker-diarization".into(), ..Default::default() };
    let mut terminal = None;
    for item in engine.transcribe(&video, &std::env::temp_dir(), &options, &CancellationToken::new()) {
        match item {
            Ok(EngineEvent::Stage { .. }) => {}
            Ok(other) => panic!("{other:?}"),
            Err(e) => terminal = Some(e),
        }
    }
    let Some(EngineError::Failed(message)) = terminal else { panic!("{terminal:?}") };
    assert!(!message.is_empty() && !message.contains("error code"), "{message}");
}
