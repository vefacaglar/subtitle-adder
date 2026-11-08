//! Engine process tests against the scripted fake engine (C# EngineKitTests / LanguageEnvTests / SpeakerTests,
//! Swift EngineKitTests / ConcurrentEnginesTests / ExtractAudioTests / LaunchSettingsTests / DiagnosticLogTests).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use sa_core::cancel::CancellationToken;
use sa_core::engine_types::{EngineError, EngineModel, EngineStage, Sentence, SubtitleFormat};
use sa_core::l10n::{LanguageState, UiLanguage};
use sa_enginekit::events::EngineEvent;
use sa_enginekit::launch::{EngineLaunchSettings, EngineLaunchState, ExternalLlm};
use sa_enginekit::{EngineClient, EngineCommand, ProcessEngineClient, TranscribeOptions};

fn no_cancel() -> CancellationToken {
    CancellationToken::new()
}

fn sentences() -> Vec<Sentence> {
    vec![Sentence::new(1, 0.0, 1.0, "Merhaba."), Sentence::new(2, 1.0, 2.0, "Selam.")]
}

// MARK: models / delete-model

#[test]
fn lists_models_from_engine() {
    let f = Fake::tr(
        r#"out {"event": "models", "models": [{"id": "base", "backend": "whisper", "languages": null, "size_mb": 145, "disk_mb": 141, "speed": 5, "accuracy": 2, "downloaded": true}, {"id": "gigaam-v3", "backend": "onnx", "languages": ["ru"], "size_mb": 216, "disk_mb": null, "speed": 5, "accuracy": 4, "downloaded": false}]}"#,
    );
    let models = f.client.models(&no_cancel()).unwrap();
    let mut base = EngineModel::new("base", 145, 5, 2, true);
    base.disk_mb = Some(141);
    let mut giga = EngineModel::new("gigaam-v3", 216, 5, 4, false);
    giga.backend = "onnx".into();
    giga.languages = Some(vec!["ru".into()]);
    assert_eq!(models, vec![base, giga]);
    assert_eq!(models[0].kind, "transcription");
    assert_eq!(models[0].license, None);
    assert!(!models[0].is_translation());
    assert!(!models[0].has_non_commercial_license());
    assert!(models[0].detects_language());
    assert!(models[0].supports("tr"));
    assert!(!models[1].detects_language());
    assert!(models[1].supports("ru"));
    assert!(!models[1].supports("tr"));
}

#[test]
fn decodes_models_without_new_fields() {
    let f = Fake::tr(
        r#"out {"event": "models", "models": [{"id": "small", "kind": null, "size_mb": 465, "speed": 4, "accuracy": 3, "downloaded": false}]}"#,
    );
    let models = f.client.models(&no_cancel()).unwrap();
    assert_eq!(models.len(), 1);
    let m = &models[0];
    assert_eq!(m.kind, "transcription");
    assert_eq!(m.backend, "whisper");
    assert_eq!(m.languages, None);
    assert_eq!(m.disk_mb, None);
    assert_eq!(m.license, None);
}

#[test]
fn decodes_translation_and_llama_model_fields() {
    let f = Fake::tr(
        r#"out {"event": "models", "models": [{"id": "nllb-1.3b", "kind": "translation", "backend": "ctranslate2", "languages": ["tr", "en", "pt", "es", "ru"], "size_mb": 1336, "disk_mb": 1336, "speed": 3, "accuracy": 4, "license": "CC-BY-NC-4.0", "downloaded": true}, {"id": "gemma-4-e4b", "kind": "translation", "backend": "llama", "languages": ["tr", "en", "ja"], "size_mb": 4747, "speed": 3, "accuracy": 4, "license": "Apache-2.0", "downloaded": false}, {"id": "speaker-diarization", "kind": "diarization", "backend": "sherpa", "languages": null, "size_mb": 29, "speed": 5, "accuracy": 3, "license": "MIT AND Apache-2.0", "downloaded": false}]}"#,
    );
    let models = f.client.models(&no_cancel()).unwrap();
    let nllb = &models[0];
    assert_eq!(nllb.kind, "translation");
    assert!(nllb.is_translation() && !nllb.is_llama());
    assert_eq!(nllb.license.as_deref(), Some("CC-BY-NC-4.0"));
    assert!(nllb.has_non_commercial_license());
    assert!(nllb.supports("es"));
    assert!(models[1].is_llama() && models[1].is_translation() && models[1].supports("ja"));
    assert!(models[2].is_diarization() && !models[2].is_transcription() && !models[2].is_translation());
    assert!(!models[2].has_non_commercial_license());
}

#[test]
fn a_non_integer_size_fails_the_whole_list() {
    let f = Fake::en(
        r#"out {"event": "models", "models": [{"id": "small", "size_mb": 145.5, "speed": 4, "accuracy": 3, "downloaded": false}]}"#,
    );
    assert_eq!(f.client.models(&no_cancel()), Err(EngineError::Failed("The engine did not output a model list.".into())));
}

#[test]
fn models_without_output_is_localized_error() {
    let f = Fake::en("out");
    assert_eq!(f.client.models(&no_cancel()), Err(EngineError::Failed("The engine did not output a model list.".into())));
    let f = Fake::tr("out");
    assert_eq!(f.client.models(&no_cancel()), Err(EngineError::Failed("Motor model listesi yazmadı.".into())));
}

#[test]
fn models_error_event_wins_even_with_exit_zero() {
    let f = Fake::tr(r#"out {"event": "error", "message": "olmadı"}"#);
    assert_eq!(f.client.models(&no_cancel()), Err(EngineError::Failed("olmadı".into())));
}

#[test]
fn delete_model_returns_freed_space() {
    let f = Fake::tr("");
    let args = f.path("args.txt");
    let f = Fake::tr(&format!("args {args}\nout {{\"event\": \"deleted\", \"id\": \"small\", \"freed_mb\": 464}}"));
    assert_eq!(f.client.delete_model("small", &no_cancel()), Ok(464));
    assert!(read(&args).contains("delete-model small --json"), "{}", read(&args));
}

#[test]
fn delete_model_surfaces_engine_error() {
    let f = Fake::tr("out {\"event\": \"error\", \"message\": \"Model indirilmemiş: small\"}\nexit 1");
    assert_eq!(f.client.delete_model("small", &no_cancel()), Err(EngineError::Failed("Model indirilmemiş: small".into())));
}

#[test]
fn delete_model_with_wrong_id_or_fractional_size_is_error() {
    let f = Fake::tr(r#"out {"event": "deleted", "id": "base", "freed_mb": 464}"#);
    assert_eq!(f.client.delete_model("small", &no_cancel()), Err(EngineError::Failed("Motor silme sonucunu yazmadı.".into())));
    // Windows: C# `TryGetInt32` refuses `464.0`; macOS: `JSONDecoder` reads it as the integer 464.
    let f = Fake::tr(r#"out {"event": "deleted", "id": "small", "freed_mb": 464.0}"#);
    if cfg!(target_os = "macos") {
        assert_eq!(f.client.delete_model("small", &no_cancel()), Ok(464));
    } else {
        assert_eq!(f.client.delete_model("small", &no_cancel()), Err(EngineError::Failed("Motor silme sonucunu yazmadı.".into())));
    }
    let f = Fake::tr(r#"out {"event": "deleted", "id": "small", "freed_mb": 464.5}"#);
    assert_eq!(f.client.delete_model("small", &no_cancel()), Err(EngineError::Failed("Motor silme sonucunu yazmadı.".into())));
    let f = Fake::en(r#"out {"event": "deleted", "id": "small"}"#);
    assert_eq!(
        f.client.delete_model("small", &no_cancel()),
        Err(EngineError::Failed("The engine did not output a deletion result.".into()))
    );
}

// MARK: streams

#[test]
fn streams_events_until_result() {
    let f = Fake::tr(
        "out {\"event\": \"stage\", \"stage\": \"extracting_audio\", \"fraction\": null}\nerr progress noise\nout {\"event\": \"result\", \"language\": \"tr\", \"duration\": 1.5, \"audio\": \"a.wav\", \"outputs\": [], \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Merhaba.\"}]}",
    );
    let (events, error) = collect(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel()));
    assert_eq!(error, None);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], EngineEvent::Stage { stage: EngineStage::ExtractingAudio, fraction: None });
    let EngineEvent::Result(r) = &events[1] else { panic!() };
    assert_eq!(r.sentences[0].text, "Merhaba.");
}

#[test]
fn transcribe_passes_arguments() {
    let f = Fake::tr("");
    let args = f.path("args.txt");
    let f = Fake::tr(&format!("args {args}"));
    let options = TranscribeOptions {
        language: Some("tr".into()),
        model: "base".into(),
        formats: vec![SubtitleFormat::Srt, SubtitleFormat::Vtt],
        ..Default::default()
    };
    let (_, error) = collect(f.client.transcribe(p(r"C:\videos\my video.mp4"), p(r"C:\out"), &options, &no_cancel()));
    assert_eq!(error, None);
    assert_eq!(read(&args), r"transcribe C:\videos\my video.mp4 --out-dir C:\out --model base --json --format srt vtt --language tr");
}

#[test]
fn transcribe_arguments_include_diarize_options() {
    let video = p("/v.mp4");
    let out = p("/out");
    let speakers_only = TranscribeOptions { speakers: Some(2), ..Default::default() };
    let args = ProcessEngineClient::transcribe_arguments(video, out, &speakers_only);
    assert!(!args.contains(&"--diarize".to_owned()) && !args.contains(&"--speakers".to_owned()));
    let diarize = TranscribeOptions { diarize: true, ..Default::default() };
    let args = ProcessEngineClient::transcribe_arguments(video, out, &diarize);
    assert_eq!(args.last().map(String::as_str), Some("--diarize"));
    assert!(!args.contains(&"--speakers".to_owned()));
    let both = TranscribeOptions { diarize: true, speakers: Some(2), ..Default::default() };
    let args = ProcessEngineClient::transcribe_arguments(video, out, &both);
    assert_eq!(&args[args.len() - 3..], ["--diarize", "--speakers", "2"]);
    assert_eq!(
        ProcessEngineClient::transcribe_arguments(video, out, &TranscribeOptions::default()),
        ["transcribe", "/v.mp4", "--out-dir", "/out", "--model", "small", "--json", "--format", "srt"]
    );
}

#[test]
fn engine_error_event_ends_the_stream_with_its_message() {
    let f = Fake::tr("out {\"event\": \"error\", \"message\": \"Video bulunamadı\"}\nexit 1");
    let error = drain_error(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel()));
    assert_eq!(error, EngineError::Failed("Video bulunamadı".into()));
}

#[test]
fn an_error_event_wins_even_with_exit_zero_and_the_last_one_counts() {
    let f = Fake::tr("out {\"event\": \"error\", \"message\": \"first\"}\nout {\"event\": \"error\", \"message\": \"second\"}");
    let (events, error) = collect(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel()));
    assert!(events.is_empty());
    assert_eq!(error, Some(EngineError::Failed("second".into())));
}

#[test]
fn nonzero_exit_reports_code_and_log_tail_then_deletes_log() {
    let mut f = Fake::en(
        "err line 1\nerr line 2\nerr line 3\nerr line 4\nerr line 5\nerr line 6\nerr line 7 ünlü\nexit 3",
    );
    let log = f.dir.path().join("subtitle-engine-test.log");
    let log_for_factory = log.clone();
    f.client.log_path_factory = Some(Arc::new(move || log_for_factory.clone()));
    let error = drain_error(f.client.transcribe(p(r"C:\v.mp4"), &std::env::temp_dir(), &TranscribeOptions::default(), &no_cancel()));
    assert_eq!(
        error,
        EngineError::Failed("The engine exited with an error code (3). line 3\nline 4\nline 5\nline 6\nline 7 ünlü".into())
    );
    assert!(!log.exists());
}

#[test]
fn every_run_deletes_its_log_also_on_success() {
    let mut f = Fake::tr("err something\nout {\"event\": \"stage\", \"stage\": \"transcribing\", \"fraction\": 1}");
    let log = f.dir.path().join("ok.log");
    let l = log.clone();
    f.client.log_path_factory = Some(Arc::new(move || l.clone()));
    let (events, error) = collect(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel()));
    assert_eq!((events.len(), error), (1, None));
    assert!(!log.exists());
    assert!(f.client.render(&sentences(), SubtitleFormat::Srt, &no_cancel()).is_ok());
    assert!(!log.exists());
}

#[test]
fn nonzero_exit_in_render_reports_log_tail() {
    let f = Fake::tr("err panicked: boom\nexit 2");
    assert_eq!(
        f.client.render(&[Sentence::new(1, 0.0, 1.0, "A")], SubtitleFormat::Srt, &no_cancel()),
        Err(EngineError::Failed("Motor hata koduyla çıktı (2). panicked: boom".into()))
    );
}

#[test]
fn early_exit_before_reading_stdin_is_reported_not_crashed() {
    let big: Vec<Sentence> = (1..=20_000).map(|i| Sentence::new(i, i as f64, i as f64 + 0.5, "x".repeat(200))).collect();
    let f = Fake::en("err refusing input\nexit 4");
    assert_eq!(
        f.client.render(&big, SubtitleFormat::Srt, &no_cancel()),
        Err(EngineError::Failed("The engine exited with an error code (4). refusing input".into()))
    );
    let EngineError::Failed(message) = drain_error(f.client.translate(&big, "es", None, "madlad-3b", &no_cancel())) else { panic!() };
    assert!(message.contains("(4)"), "{message}");
}

#[test]
fn a_huge_payload_is_streamed_without_deadlock() {
    let big: Vec<Sentence> = (1..=20_000).map(|i| Sentence::new(i, i as f64, i as f64 + 0.5, "y".repeat(200))).collect();
    let f = Fake::tr("cat");
    let out = f.client.render(&big, SubtitleFormat::Json, &no_cancel()).unwrap();
    assert_eq!(Sentence::decode_list(&out).unwrap().len(), 20_000);
}

#[test]
fn missing_executable_is_localized_start_failure() {
    let mut f = Fake::en("");
    f.client.command = EngineCommand::new(f.dir.path().join("no-such-engine.exe"));
    let Err(EngineError::Failed(message)) = f.client.render(&sentences(), SubtitleFormat::Srt, &no_cancel()) else { panic!() };
    assert!(message.starts_with("Could not start the engine: "), "{message}");
    let EngineError::Failed(message) = drain_error(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel()))
    else {
        panic!()
    };
    assert!(message.starts_with("Could not start the engine: "), "{message}");
    // The log of a failed start is removed too.
    let leftovers = std::fs::read_dir(f.dir.path()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".log")).count();
    assert_eq!(leftovers, 0);
}

#[test]
fn render_sends_sentences_on_stdin() {
    let f = Fake::tr("cat");
    let out = f.client.render(&sentences(), SubtitleFormat::Srt, &no_cancel()).unwrap();
    assert!(out.contains("\"Selam.\""), "{out}");
}

#[test]
fn render_passes_format_and_keeps_utf8() {
    let f = Fake::tr("");
    let args = f.path("args.txt");
    let f = Fake::tr(&format!("args {args}\nout Altyazı – şöyle ğ"));
    let out = f.client.render(&sentences(), SubtitleFormat::Vtt, &no_cancel()).unwrap();
    assert_eq!(out.replace("\r\n", "\n"), "Altyazı – şöyle ğ\n");
    assert_eq!(read(&args), "render --format vtt");
}

#[test]
fn render_and_translate_send_speakers_on_stdin() {
    let mut list = sentences();
    list[0].speaker = Some(0);
    list[1].speaker = Some(1);
    let f = Fake::tr("cat");
    let out = f.client.render(&list, SubtitleFormat::Srt, &no_cancel()).unwrap();
    assert!(out.contains("\"speaker\":1"), "{out}");
    let f = Fake::tr("");
    let stdin = f.path("stdin.txt");
    let f = Fake::tr(&format!("stdin {stdin}"));
    let (_, error) = collect(f.client.translate(&list, "en", Some("tr"), "madlad-3b", &no_cancel()));
    assert_eq!(error, None);
    let sent = read(&stdin);
    assert!(sent.contains("\"speaker\":0") && sent.contains("\"speaker\":1"), "{sent}");
}

#[test]
fn translate_streams_events_and_sends_request() {
    let f = Fake::tr("");
    let (args, stdin) = (f.path("args.txt"), f.path("stdin.txt"));
    let f = Fake::tr(&format!(
        "args {args}\nstdin {stdin}\nout {{\"event\": \"stage\", \"stage\": \"downloading_model\", \"fraction\": 0.12}}\nout {{\"event\": \"stage\", \"stage\": \"translating\", \"fraction\": 0.4}}\nout {{\"event\": \"translation\", \"language\": \"es\", \"sentences\": [{{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Hola.\", \"words\": []}}]}}"
    ));
    let (events, error) = collect(f.client.translate(&sentences(), "es", Some("tr"), "madlad-3b", &no_cancel()));
    assert_eq!(error, None);
    assert_eq!(
        events,
        vec![
            EngineEvent::Stage { stage: EngineStage::DownloadingModel, fraction: Some(0.12) },
            EngineEvent::Stage { stage: EngineStage::Translating, fraction: Some(0.4) },
            EngineEvent::Translation { language: "es".into(), sentences: vec![Sentence::new(1, 0.0, 1.0, "Hola.")] },
        ]
    );
    assert!(read(&args).contains("translate --to es --model madlad-3b --json --from tr"), "{}", read(&args));
    let sent = read(&stdin);
    assert!(sent.contains("\"Merhaba.\"") && sent.contains("\"words\""), "{sent}");
}

#[test]
fn translate_omits_source_when_none() {
    let f = Fake::tr("");
    let args = f.path("args.txt");
    let f = Fake::tr(&format!("args {args}\ndrain"));
    let (_, error) = collect(f.client.translate(&sentences(), "es", None, "madlad-3b", &no_cancel()));
    assert_eq!(error, None);
    assert_eq!(read(&args), "translate --to es --model madlad-3b --json");
}

#[test]
fn translate_surfaces_engine_error() {
    let f = Fake::tr("drain\nout {\"event\": \"error\", \"message\": \"NLLB için kaynak dil gerekli (--from).\"}\nexit 1");
    let error = drain_error(f.client.translate(&sentences(), "es", None, "nllb-1.3b", &no_cancel()));
    assert_eq!(error, EngineError::Failed("NLLB için kaynak dil gerekli (--from).".into()));
}

#[test]
fn translate_streams_every_incremental_translation() {
    let f = Fake::tr(
        "drain\nout {\"event\": \"translation\", \"language\": \"es\", \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Hola.\", \"words\": []}]}\nout {\"event\": \"translation\", \"language\": \"es\", \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Hola.\", \"words\": []}, {\"id\": 2, \"start\": 1, \"end\": 2, \"text\": \"Adios.\", \"words\": []}]}",
    );
    let (events, error) = collect(f.client.translate(&sentences(), "es", None, "madlad-3b", &no_cancel()));
    assert_eq!(error, None);
    let texts: Vec<Vec<String>> = events
        .iter()
        .map(|e| match e {
            EngineEvent::Translation { sentences, .. } => sentences.iter().map(|s| s.text.clone()).collect(),
            _ => panic!(),
        })
        .collect();
    assert_eq!(texts, vec![vec!["Hola."], vec!["Hola.", "Adios."]]);
}

#[test]
fn stream_skips_unknown_events_and_delivers_partials() {
    let f = Fake::tr(
        "out {\"event\": \"stage\", \"stage\": \"transcribing\", \"fraction\": 0.1}\nout {\"event\": \"partial\", \"language\": \"?\", \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Mer\", \"speaker\": null, \"words\": []}]}\nout {\"event\": \"heartbeat\"}\nout {\"event\": \"partial\", \"language\": \"tr\", \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Merhaba\", \"speaker\": null, \"words\": []}]}\nout {\"event\": \"result\", \"language\": \"tr\", \"duration\": 1, \"audio\": \"a.wav\", \"outputs\": [], \"sentences\": []}",
    );
    let (events, error) = collect(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel()));
    assert_eq!(error, None);
    assert_eq!(events.len(), 4);
    assert!(matches!(&events[1], EngineEvent::Partial { language, .. } if language == "?"));
    assert!(matches!(&events[2], EngineEvent::Partial { language, .. } if language == "tr"));
    assert!(matches!(events[3], EngineEvent::Result(_)));
}

#[test]
fn a_last_line_without_newline_and_windows_line_endings_are_read() {
    let f = Fake::tr(
        r#"outraw {"event": "stage", "stage": "transcribing", "fraction": 0.5}\r\n{"event": "stage", "stage": "segmenting"}"#,
    );
    let (events, error) = collect(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel()));
    assert_eq!(error, None);
    assert_eq!(
        events,
        vec![
            EngineEvent::Stage { stage: EngineStage::Transcribing, fraction: Some(0.5) },
            EngineEvent::Stage { stage: EngineStage::Segmenting, fraction: None },
        ]
    );
}

#[test]
fn a_multi_megabyte_line_is_read() {
    // A `result` snapshot can be several MB on one line; the garbage check must still see it whole.
    let f = Fake::tr("outbig 3000000");
    let error = drain_error(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel()));
    assert!(matches!(error, EngineError::Failed(_)));
}

// MARK: cancellation and termination

#[test]
fn cancel_kills_engine_process_tree() {
    let f = Fake::tr("");
    let (pid_file, child_file) = (f.path("pid.txt"), f.path("child.txt"));
    let f = Fake::tr(&format!(
        "pid {pid_file}\nchild {child_file}\nout {{\"event\": \"stage\", \"stage\": \"transcribing\", \"fraction\": 0.1}}\nsleep 30000"
    ));
    let cancel = CancellationToken::new();
    let stream = f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &cancel);
    assert!(matches!(stream.next_blocking(), Some(Ok(EngineEvent::Stage { .. }))));
    let (pid, child) = (read_pid(&pid_file), read_pid(&child_file));
    assert!(is_alive(pid));
    cancel.cancel();
    let started = Instant::now();
    let rest: Vec<_> = stream.collect();
    assert_eq!(rest, vec![Err(EngineError::Cancelled)]);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(wait_until_dead(pid), "engine still alive");
    if cfg!(windows) {
        // The Job Object takes the whole tree down (llama-server in real runs).
        assert!(wait_until_dead(child), "grandchild still alive");
    }
}

#[test]
fn abandoned_stream_kills_engine() {
    let f = Fake::tr("");
    let pid_file = f.path("pid.txt");
    let f = Fake::tr(&format!("pid {pid_file}\nout {{\"event\": \"stage\", \"stage\": \"transcribing\", \"fraction\": 0.1}}\nsleep 30000"));
    let stream = f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel());
    assert!(stream.next_blocking().is_some());
    let pid = read_pid(&pid_file);
    drop(stream);
    assert!(wait_until_dead(pid));
}

#[test]
fn cancel_render_kills_engine() {
    let f = Fake::tr("");
    let pid_file = f.path("pid.txt");
    let f = Fake::tr(&format!("pid {pid_file}\nsleep 30000"));
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    let pf = pid_file.clone();
    let canceller = std::thread::spawn(move || {
        let pid = read_pid(&pf);
        c.cancel();
        pid
    });
    let started = Instant::now();
    assert_eq!(f.client.render(&sentences(), SubtitleFormat::Srt, &cancel), Err(EngineError::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(wait_until_dead(canceller.join().unwrap()));
}

#[test]
fn an_already_cancelled_token_never_spawns() {
    let f = Fake::tr("");
    let pid_file = f.path("pid.txt");
    let f = Fake::tr(&format!("pid {pid_file}"));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(f.client.models(&cancel), Err(EngineError::Cancelled));
    assert_eq!(drain_error(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &cancel)), EngineError::Cancelled);
    std::thread::sleep(Duration::from_millis(200));
    assert!(!Path::new(&pid_file).exists());
}

#[test]
fn garbage_stdout_line_kills_engine_and_errors() {
    let f = Fake::tr("");
    let pid_file = f.path("pid.txt");
    let f = Fake::tr(&format!("pid {pid_file}\nout not JSON\nsleep 30000"));
    let started = Instant::now();
    let error = drain_error(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel()));
    assert!(matches!(error, EngineError::Failed(_)));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(wait_until_dead(read_pid(&pid_file)));
}

#[test]
fn transcribe_does_not_block_caller_thread() {
    let f = Fake::tr(
        "sleep 500\nout {\"event\": \"result\", \"language\": \"tr\", \"duration\": 1, \"audio\": \"a.wav\", \"outputs\": [], \"sentences\": []}",
    );
    let started = Instant::now();
    let stream = f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel());
    assert!(started.elapsed() < Duration::from_millis(400), "{:?}", started.elapsed());
    assert!(stream.next_timeout(Duration::from_millis(0)).is_err(), "the first item is not ready yet");
    assert!(matches!(stream.next_blocking(), Some(Ok(EngineEvent::Result(_)))));
    assert!(stream.next_blocking().is_none());
}

// MARK: concurrency

#[test]
fn a_quiet_engine_does_not_hold_up_another() {
    let quiet = Fake::tr("out {\"event\": \"stage\", \"stage\": \"transcribing\", \"fraction\": 0.1}\nsleep 20000");
    let quiet_stream = quiet.client.transcribe(p("q.mp4"), &quiet.out_dir(), &TranscribeOptions::default(), &no_cancel());
    assert!(quiet_stream.next_blocking().is_some());
    let busy = Fake::tr(
        "out {\"event\": \"stage\", \"stage\": \"extracting_audio\", \"fraction\": null}\nout {\"event\": \"result\", \"language\": \"?\", \"duration\": 0, \"audio\": \"a.wav\", \"outputs\": [], \"sentences\": []}",
    );
    let started = Instant::now();
    let (events, error) = collect(busy.client.extract_audio(p("b.mp4"), &busy.out_dir(), &no_cancel()));
    assert_eq!(error, None);
    assert_eq!(events.iter().filter(|e| matches!(e, EngineEvent::Result(_))).count(), 1);
    let renderer = Fake::tr("out result");
    assert!(renderer.client.render(&sentences(), SubtitleFormat::Srt, &no_cancel()).unwrap().contains("result"));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn lines_are_handed_over_as_soon_as_they_are_written() {
    let f = Fake::tr("out {\"event\": \"stage\", \"stage\": \"transcribing\", \"fraction\": 0.1}\nsleep 20000");
    let started = Instant::now();
    let stream = f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel());
    assert!(matches!(stream.next_blocking(), Some(Ok(_))));
    assert!(started.elapsed() < Duration::from_secs(10));
}

// MARK: extract-audio

#[test]
fn extract_audio_names_the_video_and_the_output_folder() {
    assert_eq!(
        ProcessEngineClient::extract_audio_arguments(p("/tmp/a b.mp4"), p("/tmp/out")),
        ["extract-audio", "/tmp/a b.mp4", "--out-dir", "/tmp/out", "--json"]
    );
}

#[test]
fn extract_audio_streams_stages_then_the_audio_path() {
    let f = Fake::tr("");
    let args = f.path("args.txt");
    let f = Fake::tr(&format!(
        "args {args}\nout {{\"event\": \"stage\", \"stage\": \"extracting_audio\", \"fraction\": null}}\nout {{\"event\": \"stage\", \"stage\": \"extracting_audio\", \"fraction\": 1.0}}\nout {{\"event\": \"result\", \"language\": \"?\", \"duration\": 0, \"audio\": \"/tmp/out/a b-1234abcd.wav\", \"outputs\": [], \"sentences\": []}}"
    ));
    let (events, error) = collect(f.client.extract_audio(p("/tmp/a b.mp4"), p("/tmp/out"), &no_cancel()));
    assert_eq!(error, None);
    assert_eq!(events.len(), 3);
    let EngineEvent::Result(r) = &events[2] else { panic!() };
    assert_eq!(r.audio, "/tmp/out/a b-1234abcd.wav");
    assert_eq!(read(&args), "extract-audio /tmp/a b.mp4 --out-dir /tmp/out --json");
}

#[test]
fn extract_audio_surfaces_the_engines_error() {
    let f = Fake::tr("out {\"event\": \"error\", \"message\": \"Video açılamadı\"}\nexit 1");
    assert_eq!(drain_error(f.client.extract_audio(p("x.mp4"), &f.out_dir(), &no_cancel())), EngineError::Failed("Video açılamadı".into()));
}

// MARK: preview (macOS player fallback; works on every platform)

#[test]
fn preview_reads_the_source_from_stdin() {
    let f = Fake::tr("");
    let (args, stdin) = (f.path("args.txt"), f.path("stdin.txt"));
    let f = Fake::tr(&format!("args {args}\nstdin {stdin}\nout not json is ignored\nout {{\"event\": \"stage\", \"stage\": \"extracting_audio\", \"fraction\": 1}}"));
    let source = f.dir.path().join("source.wmv");
    std::fs::write(&source, b"VIDEO-BYTES").unwrap();
    let output = f.dir.path().join("preview.mp4");
    assert_eq!(f.client.make_preview(&source, &output, &no_cancel()), Ok(()));
    assert_eq!(read(&stdin), "VIDEO-BYTES");
    assert_eq!(read(&args), format!("preview - --output {} --json", output.display()));
    let failing = Fake::tr("drain\nout {\"event\": \"error\", \"message\": \"Biçim desteklenmiyor\"}\nexit 1");
    assert_eq!(failing.client.make_preview(&source, &output, &no_cancel()), Err(EngineError::Failed("Biçim desteklenmiyor".into())));
    assert!(matches!(failing.client.make_preview(&f.dir.path().join("missing.wmv"), &output, &no_cancel()), Err(EngineError::Failed(_))));
}

// MARK: language and launch environment

const ECHO_LANG: &str = "drain\nout {\"event\":\"error\",\"message\":\"lang=${SUBTITLE_ENGINE_LANG}\"}\nexit 1";

#[test]
fn every_command_passes_the_language() {
    for (language, code) in [(UiLanguage::Tr, "tr"), (UiLanguage::En, "en")] {
        let f = Fake::new(ECHO_LANG, language);
        let expected = EngineError::Failed(format!("lang={code}"));
        assert_eq!(drain_error(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel())), expected);
        assert_eq!(drain_error(f.client.translate(&sentences(), "en", None, "madlad-3b", &no_cancel())), expected);
        assert_eq!(f.client.models(&no_cancel()), Err(expected.clone()));
        assert_eq!(f.client.delete_model("small", &no_cancel()), Err(expected.clone()));
    }
    let f = Fake::en("drain\noutraw ${SUBTITLE_ENGINE_LANG}");
    assert_eq!(f.client.render(&sentences(), SubtitleFormat::Srt, &no_cancel()), Ok("en".into()));
}

#[test]
fn environment_keeps_parent_variables_and_overrides_language_and_offline() {
    let base = vec![
        ("PATH".to_owned(), "/usr/bin".to_owned()),
        ("SUBTITLE_ENGINE_LANG".to_owned(), "tr".to_owned()),
        ("SUBTITLE_ENGINE_OFFLINE".to_owned(), "0".to_owned()),
    ];
    let env = ProcessEngineClient::process_environment(
        UiLanguage::En,
        &EngineLaunchSettings { offline: true, external_llm: None },
        base,
        false,
    );
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert_eq!(get("PATH"), Some("/usr/bin"));
    assert_eq!(get("SUBTITLE_ENGINE_LANG"), Some("en"));
    assert_eq!(get("SUBTITLE_ENGINE_OFFLINE"), Some("1"));
    assert_eq!(env.iter().filter(|(n, _)| n == "SUBTITLE_ENGINE_LANG").count(), 1);
}

#[test]
fn launch_passes_offline_mode() {
    for (offline, flag) in [(true, "1"), (false, "0")] {
        let mut f = Fake::tr("drain\nout {\"event\":\"error\",\"message\":\"${SUBTITLE_ENGINE_OFFLINE}\"}\nexit 1");
        f.client.launch_settings = Some(EngineLaunchSettings { offline, external_llm: None });
        assert_eq!(f.client.models(&no_cancel()), Err(EngineError::Failed(flag.into())));
    }
}

#[test]
fn defaults_follow_the_shared_language_and_launch_state() {
    let _guard = GLOBAL_STATE.lock();
    let languages = LanguageState::shared();
    let launch = EngineLaunchState::shared();
    let (saved_language, saved_launch) = (languages.language(), launch.get());

    let mut f = Fake::tr("drain\nout {\"event\":\"error\",\"message\":\"${SUBTITLE_ENGINE_LANG}|${SUBTITLE_ENGINE_OFFLINE}\"}\nexit 1");
    f.client.language = None;
    f.client.launch_settings = None;
    languages.set_language(UiLanguage::En);
    launch.set(EngineLaunchSettings { offline: false, external_llm: None });
    let first = f.client.models(&no_cancel());
    languages.set_language(UiLanguage::Tr);
    launch.set_offline(true);
    let second = f.client.models(&no_cancel());
    // An explicit value wins over the shared one.
    f.client.language = Some(UiLanguage::En);
    f.client.launch_settings = Some(EngineLaunchSettings { offline: false, external_llm: None });
    let explicit = f.client.models(&no_cancel());

    languages.set_language(saved_language);
    launch.set(saved_launch);
    assert_eq!(first, Err(EngineError::Failed("en|0".into())));
    assert_eq!(second, Err(EngineError::Failed("tr|1".into())));
    assert_eq!(explicit, Err(EngineError::Failed("en|0".into())));
    assert_eq!(f.client.resolved_language(UiLanguage::Tr), UiLanguage::En);
}

fn without_external_vars() -> Vec<(String, String)> {
    ProcessEngineClient::current_environment()
        .into_iter()
        .filter(|(k, _)| !k.to_ascii_uppercase().starts_with("SUBTITLE_ENGINE_EXTERNAL_LLM_"))
        .collect()
}

#[test]
fn each_command_is_launched_with_its_own_environment() {
    let script = "drain\nout {\"event\": \"error\", \"message\": \"${ARG1}|${SUBTITLE_ENGINE_OFFLINE}|${SUBTITLE_ENGINE_EXTERNAL_LLM_URL}|${SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY}\"}\nexit 1";
    let mut f = Fake::en(script);
    let mut inherited = without_external_vars();
    // Even a key inherited from the parent's environment does not reach a command that has no use for it.
    inherited.push(("SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY".into(), "inherited".into()));
    f.client.base_environment = Some(inherited.clone());
    f.client.launch_settings = Some(EngineLaunchSettings {
        offline: true,
        external_llm: Some(ExternalLlm { url: "10.0.0.2:11434".into(), model: "m".into(), api_key: "sk-1".into() }),
    });
    let fail = |m: &str| EngineError::Failed(m.into());
    assert_eq!(drain_error(f.client.transcribe(p("/v.mp4"), p("/tmp"), &TranscribeOptions::default(), &no_cancel())), fail("transcribe|1||"));
    assert_eq!(drain_error(f.client.extract_audio(p("/v.mp4"), p("/tmp"), &no_cancel())), fail("extract-audio|1||"));
    assert_eq!(
        drain_error(f.client.translate(&[], "en", None, "madlad-3b", &no_cancel())),
        fail("translate|1|http://10.0.0.2:11434|sk-1")
    );
    assert_eq!(f.client.models(&no_cancel()), Err(fail("models|1||")));
    assert_eq!(f.client.delete_model("small", &no_cancel()), Err(fail("delete-model|1||")));
    let mut renderer =
        Fake::en("drain\noutraw ${ARG1}|${SUBTITLE_ENGINE_OFFLINE}|${SUBTITLE_ENGINE_EXTERNAL_LLM_URL}|${SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY}");
    renderer.client.base_environment = Some(inherited);
    renderer.client.launch_settings = f.client.launch_settings.clone();
    assert_eq!(renderer.client.render(&[], SubtitleFormat::Srt, &no_cancel()), Ok("render|1||".into()));
}

#[test]
fn launch_passes_external_llm_api_key_to_translate_when_enabled() {
    let script = "drain\nout {\"event\": \"error\", \"message\": \"${SUBTITLE_ENGINE_EXTERNAL_LLM_URL}|${SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL}|${SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY}\"}\nexit 1";
    let url = "https://opencode.ai/zen/go/v1";
    let cases = [
        (Some(ExternalLlm { url: url.into(), model: "glm-5".into(), api_key: "sk-opencode-1234".into() }), format!("{url}|glm-5|sk-opencode-1234")),
        (Some(ExternalLlm { url: url.into(), model: "glm-5".into(), api_key: "   ".into() }), format!("{url}|glm-5|")),
        (None, "||".to_owned()),
    ];
    for (llm, expected) in cases {
        let mut f = Fake::tr(script);
        f.client.base_environment = Some(without_external_vars());
        f.client.launch_settings = Some(EngineLaunchSettings { offline: false, external_llm: llm });
        assert_eq!(drain_error(f.client.translate(&sentences(), "en", None, "madlad-3b", &no_cancel())), EngineError::Failed(expected));
        // UNIFIED.md §3.7: the other commands never receive the variables.
        assert_eq!(f.client.models(&no_cancel()), Err(EngineError::Failed("||".into())));
    }
}

#[test]
fn launched_process_receives_the_variables() {
    let mut f = Fake::en(
        "drain\nout {\"event\": \"error\", \"message\": \"${SUBTITLE_ENGINE_OFFLINE}|${SUBTITLE_ENGINE_EXTERNAL_LLM_URL}|${SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL}|${SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY}\"}\nexit 1",
    );
    f.client.base_environment = Some(without_external_vars());
    f.client.launch_settings = Some(EngineLaunchSettings {
        offline: true,
        external_llm: Some(ExternalLlm { url: "10.0.0.2:11434".into(), model: "m".into(), api_key: "sk-1".into() }),
    });
    assert_eq!(
        drain_error(f.client.translate(&[], "en", None, "madlad-3b", &no_cancel())),
        EngineError::Failed("1|http://10.0.0.2:11434|m|sk-1".into())
    );
}

// MARK: diagnostics

#[test]
fn a_translation_run_copies_its_trace_lines_before_the_stderr_file_is_deleted() {
    let marker = uuid::Uuid::new_v4().to_string();
    let mut f = Fake::tr(&format!(
        "drain\nerr progress noise\nerr [translation] {{\"event\": \"request\", \"id\": \"{marker}\", \"key\": \"sk-trace\"}}\nerr Merhaba.\nout {{\"event\": \"translation\", \"language\": \"en\", \"sentences\": []}}"
    ));
    f.client.copy_translation_trace = true;
    f.client.launch_settings = Some(EngineLaunchSettings {
        offline: false,
        external_llm: Some(ExternalLlm { url: "10.0.0.2:1".into(), model: "m".into(), api_key: "sk-trace".into() }),
    });
    f.client.base_environment = Some(without_external_vars());
    let (_, error) = collect(f.client.translate(&sentences(), "en", None, "madlad-3b", &no_cancel()));
    assert_eq!(error, None);
    let lines = f.sink.lines.lock().clone();
    assert_eq!(
        lines,
        vec![("engine".to_owned(), format!("[translation] {{\"event\": \"request\", \"id\": \"{marker}\", \"key\": \"[redacted]\"}}"))]
    );
}

#[test]
fn the_configured_key_is_redacted_from_the_trace_even_without_a_usable_address() {
    // Swift redacts `launchSettings.externalLLM.apiKey` whether or not the engine received it.
    let mut f = Fake::tr(
        "drain\nerr [translation] {\"event\": \"request_failed\", \"note\": \" sk-unused \"}\nout {\"event\": \"translation\", \"language\": \"en\", \"sentences\": []}",
    );
    f.client.copy_translation_trace = true;
    f.client.launch_settings = Some(EngineLaunchSettings {
        offline: false,
        external_llm: Some(ExternalLlm { url: "   ".into(), model: "m".into(), api_key: " sk-unused\n".into() }),
    });
    f.client.base_environment = Some(without_external_vars());
    let (_, error) = collect(f.client.translate(&sentences(), "en", None, "madlad-3b", &no_cancel()));
    assert_eq!(error, None);
    assert_eq!(
        f.sink.lines.lock().clone(),
        vec![("engine".to_owned(), "[translation] {\"event\": \"request_failed\", \"note\": \" [redacted] \"}".to_owned())]
    );
}

#[test]
fn other_commands_do_not_copy_stderr() {
    let mut f = Fake::tr("err [translation] {\"event\": \"request\"}\nout {\"event\": \"stage\", \"stage\": \"transcribing\", \"fraction\": 1}");
    f.client.copy_translation_trace = true;
    let _ = collect(f.client.transcribe(p("v.mp4"), &f.out_dir(), &TranscribeOptions::default(), &no_cancel()));
    let _ = f.client.render(&sentences(), SubtitleFormat::Srt, &no_cancel());
    assert!(f.sink.lines.lock().is_empty());
}

#[test]
fn log_diagnostic_redacts_the_launch_key() {
    let mut f = Fake::tr("");
    f.client.launch_settings = Some(EngineLaunchSettings {
        offline: false,
        external_llm: Some(ExternalLlm { url: "u".into(), model: "m".into(), api_key: " sk-9 ".into() }),
    });
    f.client.log_diagnostic("translation", "background batch of 6 lines failed: HTTP 401: bad sk-9");
    assert_eq!(
        f.sink.lines.lock().clone(),
        vec![("translation".to_owned(), "background batch of 6 lines failed: HTTP 401: bad [redacted]".to_owned())]
    );
}
