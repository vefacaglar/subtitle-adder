//! Port of `tests/test_cli.py`.

mod common;

use std::path::{Path, PathBuf};

use serial_test::serial;

use common::*;
use se_core::json::Value;
use se_core::paths::{py_path_str, wav_path_for};

fn s(p: &Path) -> String {
    py_path_str(p)
}

#[test]
#[serial]
fn test_transcribe_json_event_stream() {
    let g = Guard::new();
    let tmp = g.path();
    let Some(tone) = tone_video(&tmp) else { return };
    let fake = Fake::default();
    let (code, out, err) = run(&fake, &["transcribe", &s(&tone), "-o", &s(&tmp), "-f", "srt", "vtt", "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let events = events(&out);
    let st: Vec<String> = stages(&events).into_iter().map(|(s, _)| s).collect();
    assert_eq!(st[0], "extracting_audio");
    assert!(st.contains(&"transcribing".to_string()));
    assert_eq!(st.last().unwrap(), "segmenting");
    let result = events.last().unwrap();
    assert_eq!(ev_str(result, "event"), Some("result"));
    assert_eq!(ev_str(result, "language"), Some("tr"));
    let texts: Vec<&str> =
        result.get("sentences").unwrap().as_array().unwrap().iter().map(|s| ev_str(s, "text").unwrap()).collect();
    assert_eq!(texts, ["Merhaba dünya.", "Görüşürüz."]);
    let outputs = result.get("outputs").unwrap().as_array().unwrap();
    assert_eq!(outputs.len(), 2);
    assert!(outputs.iter().all(|p| Path::new(p.as_str().unwrap()).is_file()));
}

#[test]
#[serial]
fn test_errors_become_events() {
    let g = Guard::new();
    let tmp = g.path();
    let fake = Fake::default();
    let (code, out, _) = run(&fake, &["transcribe", &s(&tmp.join("yok.mp4")), "-o", &s(&tmp), "--json"]);
    assert_eq!(code, 1);
    let events = events(&out);
    assert_eq!(events.len(), 2, "{out}");
    assert_eq!(se_core::json::dumps(&events[0], false), r#"{"event": "stage", "stage": "extracting_audio", "fraction": null}"#);
    assert_eq!(ev_str(&events[1], "event"), Some("error"));
}

#[test]
#[serial]
fn test_render_from_stdin() {
    let _g = Guard::new();
    let payload = r#"[{"id": 1, "start": 0, "end": 1, "text": "Düzenlendi.", "words": []}]"#;
    let (code, out, err) = run_with_stdin(&Fake::default(), &["render", "-f", "srt"], payload.as_bytes());
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "1\n00:00:00,000 --> 00:00:01,000\nDüzenlendi.\n");
}

#[test]
#[serial]
fn test_render_stdin_errors_are_reported_on_stderr() {
    let _g = Guard::new();
    for (stdin, message) in [
        ("", "hata: Expecting value: line 1 column 1 (char 0)\n"),
        ("5", "hata: 'int' object is not iterable\n"),
        (r#"[{"id":1}]"#, "hata: 'start'\n"),
    ] {
        let (code, out, err) = run_with_stdin(&Fake::default(), &["render"], stdin.as_bytes());
        assert_eq!((code, out.as_str(), err.as_str()), (1, "", message), "{stdin:?}");
    }
    let (code, out, _) = run_with_stdin(&Fake::default(), &["render"], b"{}");
    assert_eq!((code, out.as_str()), (0, ""));
}

fn models_fake(downloaded: &'static [&'static str], sizes: &[(&str, i64)]) -> Fake {
    Fake {
        downloaded: Box::new(move |id| downloaded.contains(&id)),
        sizes: sizes.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        ..Fake::default()
    }
}

#[test]
#[serial]
fn test_models_json_lists_catalog_and_cache_state() {
    let _g = Guard::new();
    let fake = models_fake(
        &["base", "small"],
        &[("Systran/faster-whisper-base", 141), ("Systran/faster-whisper-small", 464)],
    );
    let (code, out, _) = run(&fake, &["models", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 1);
    let event = &events(&out)[0];
    assert_eq!(ev_str(event, "event"), Some("models"));
    let rows = event.get("models").unwrap().as_array().unwrap();
    let ids: Vec<&str> = rows.iter().map(|m| ev_str(m, "id").unwrap()).collect();
    assert_eq!(
        ids,
        [
            "base", "gigaam-v3", "small", "parakeet-v3", "canary-1b-v2", "medium", "large-v3-turbo", "large-v3",
            "nllb-1.3b", "qwen3.5-4b", "madlad-3b", "gemma-4-e4b", "qwen3.5-9b", "gemma-4-12b", "speaker-diarization",
        ]
    );
    let keys: Vec<&str> = rows[0].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["id", "kind", "backend", "languages", "size_mb", "disk_mb", "speed", "accuracy", "license", "downloaded"]);
    assert_eq!(ev_str(&rows[1], "backend"), Some("onnx"));
    assert_eq!(se_core::json::dumps(rows[1].get("languages").unwrap(), false), r#"["ru"]"#);
    assert_eq!(rows[0].get("languages"), Some(&Value::Null));
    assert_eq!(ev_str(&rows[0], "kind"), Some("transcription"));
    assert_eq!(ev_str(&rows[0], "license"), Some("MIT"));
    assert_eq!(ev_str(&rows[8], "kind"), Some("translation"));
    assert_eq!(ev_str(&rows[8], "license"), Some("CC-BY-NC-4.0"));
    assert_eq!(ev_str(&rows[9], "backend"), Some("llama"));
    assert_eq!(ev_str(&rows[9], "license"), Some("Apache-2.0"));
    let last = rows.last().unwrap();
    assert_eq!(ev_str(last, "kind"), Some("diarization"));
    assert_eq!(ev_str(last, "backend"), Some("sherpa"));
    assert_eq!(ev_str(last, "license"), Some("MIT AND Apache-2.0"));
    let downloaded: Vec<bool> = rows.iter().map(|m| m.get("downloaded").unwrap().as_bool().unwrap()).collect();
    let mut expected = vec![true, false, true];
    expected.extend([false; 12]);
    assert_eq!(downloaded, expected);
    let disk: Vec<Option<i64>> = rows.iter().map(|m| m.get("disk_mb").unwrap().as_i64()).collect();
    let mut expected = vec![Some(141), None, Some(464)];
    expected.extend([None; 12]);
    assert_eq!(disk, expected);
}

#[test]
#[serial]
fn test_models_table_without_json() {
    let _g = Guard::new();
    let fake = models_fake(&["base"], &[("Systran/faster-whisper-base", 141)]);
    let (code, out, _) = run(&fake, &["models"]);
    assert_eq!(code, 0);
    let first = out.lines().next().unwrap();
    assert!(first.starts_with("model") && first.contains("disk"));
    assert!(out.contains("base") && out.contains("141 MB") && out.contains('—'));
    assert!(out.contains("var") && out.contains("yok"));
    // Exact layout captured from the Python engine.
    assert_eq!(first, "model                            tür     boyut      disk   hız  doğruluk              lisans  önbellek");
    assert_eq!(
        out.lines().nth(1).unwrap(),
        "base                   transcription    145 MB    141 MB     5         2                 MIT       var"
    );
    assert_eq!(
        out.lines().nth(2).unwrap(),
        "gigaam-v3              transcription    216 MB         —     5         4                 MIT       yok"
    );
}

#[test]
#[serial]
fn test_delete_model_json_event() {
    let _g = Guard::new();
    let fake = Fake::default();
    let (code, out, _) = run(&fake, &["delete-model", "base", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(*fake.deleted.borrow(), ["base"]);
    assert_eq!(out, "{\"event\": \"deleted\", \"id\": \"base\", \"freed_mb\": 123}\n");
}

#[test]
#[serial]
fn test_delete_model_error_becomes_event() {
    let _g = Guard::new();
    let fake = Fake {
        delete_result: Box::new(|id| Err(se_core::EngineError::model(format!("{id} modeli indirilmemiş; silinecek dosya yok.")))),
        ..Fake::default()
    };
    let (code, out, _) = run(&fake, &["delete-model", "base", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(out, "{\"event\": \"error\", \"message\": \"base modeli indirilmemiş; silinecek dosya yok.\"}\n");
}

#[test]
#[serial]
fn test_delete_model_human_output() {
    let _g = Guard::new();
    let (code, out, _) = run(&Fake::default(), &["delete-model", "base"]);
    assert_eq!(code, 0);
    assert_eq!(out, "base silindi, 123 MB boşaldı\n");
}

#[test]
#[serial]
fn test_downloading_model_stage_precedes_loading() {
    let g = Guard::new();
    let tmp = g.path();
    let Some(tone) = tone_video(&tmp) else { return };
    let fake = Fake {
        needs: Some(Box::new(|id| id == "medium")),
        ensure_fractions: Some(vec![0.25, 0.5, 0.99, 1.0]),
        ..Fake::default()
    };
    let (code, out, err) = run(&fake, &["transcribe", &s(&tone), "-o", &s(&tmp), "-m", "medium", "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let events = events(&out);
    let st = stages(&events);
    let download: Vec<f64> = st.iter().filter(|(s, _)| s == "downloading_model").map(|(_, f)| f.unwrap()).collect();
    assert_eq!(download, [0.0, 0.25, 0.5, 0.99, 1.0]);
    assert_eq!(*fake.ensured.borrow(), ["medium"]);
    let pos = |stage: &str, f: Option<f64>| st.iter().position(|(s, x)| s == stage && *x == f).unwrap();
    assert!(pos("downloading_model", Some(0.0)) < pos("loading_model", None));
    assert_eq!(ev_str(events.last().unwrap(), "event"), Some("result"));
    assert_eq!(*fake.whisper_calls.borrow(), ["medium"]);
}

#[test]
#[serial]
fn test_onnx_catalog_model_uses_onnx_transcriber() {
    let g = Guard::new();
    let tmp = g.path();
    let Some(tone) = tone_video(&tmp) else { return };
    let fake = Fake::default();
    let (code, out, err) = run(&fake, &["transcribe", &s(&tone), "-m", "parakeet-v3", "-l", "pt", "-o", &s(&tmp), "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(ev_str(events(&out).last().unwrap(), "event"), Some("result"));
    assert_eq!(*fake.onnx_calls.borrow(), [("nemo-parakeet-tdt-0.6b-v3".to_string(), Some("int8".to_string()))]);
}

#[test]
#[serial]
fn test_language_mismatch_is_turkish_error() {
    let g = Guard::new();
    let tmp = g.path();
    // No fakes at all: validation fails before any model or filesystem access.
    let (code, out, _) =
        run(&se_cli::Real, &["transcribe", &s(&tmp.join("yok.mp4")), "-m", "gigaam-v3", "-l", "tr", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(out, "{\"event\": \"error\", \"message\": \"GigaAM yalnız Rusça destekler.\"}\n");
}

#[test]
#[serial]
fn test_cached_model_skips_downloading_stage() {
    let g = Guard::new();
    let tmp = g.path();
    let Some(tone) = tone_video(&tmp) else { return };
    let fake = Fake::default(); // needs_download → false; ensure_model panics if called
    let (code, out, err) = run(&fake, &["transcribe", &s(&tone), "-o", &s(&tmp), "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(stages(&events(&out)).iter().all(|(s, _)| s != "downloading_model"));
}

#[test]
#[ignore = "slow: real STT (samples/demo.mp4, base model)"]
#[serial]
fn test_real_stt_on_sample() {
    let g = Guard::new();
    let sample = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../samples/demo.mp4");
    if !sample.exists() {
        return;
    }
    let (code, _, err) = run(&se_cli::Real, &["transcribe", &s(&sample), "-m", "base", "-l", "tr", "-o", &s(&g.path())]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn test_render_utf8_pipes_regardless_of_locale_encoding() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let payload = r#"[{"id": 1, "start": 0.0, "end": 1.0, "text": "こんにちは dünya İş", "words": []}]"#;
    let mut child = Command::new(env!("CARGO_BIN_EXE_subtitle-engine"))
        .arg("render")
        .env("PYTHONIOENCODING", "cp1254")
        .env("PYTHONUTF8", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8(output.stdout).unwrap().contains("こんにちは dünya İş"));
}

#[test]
#[serial]
fn test_extract_audio_cli_emits_stage_and_result_and_transcribe_reuses_wav() {
    let g = Guard::new();
    let tmp = g.path();
    let Some(tone) = tone_video(&tmp) else { return };
    let fake = Fake::default();
    let (code, out, err) = run(&fake, &["extract-audio", &s(&tone), "-o", &s(&tmp), "--json"]);
    assert_eq!(code, 0, "{err}");
    let ev = events(&out);
    assert_eq!(se_core::json::dumps(&ev[0], false), r#"{"event": "stage", "stage": "extracting_audio", "fraction": null}"#);
    let result = ev.last().unwrap();
    assert_eq!(ev_str(result, "event"), Some("result"));
    let wav = PathBuf::from(ev_str(result, "audio").unwrap());
    assert_eq!(wav, wav_path_for(&tone, &tmp));
    let name = wav.file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.starts_with("tone-") && name.ends_with(".wav"));
    assert!(wav.is_file() && std::fs::metadata(&wav).unwrap().len() > 44);
    assert!(!std::fs::read_dir(&tmp).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".tmp.wav")));

    fake.audio.mode.set(ExtractMode::Forbidden);
    let (code, out, err) = run(&fake, &["transcribe", &s(&tone), "-o", &s(&tmp), "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let ev = events(&out);
    assert_eq!(se_core::json::dumps(&ev[0], false), r#"{"event": "stage", "stage": "extracting_audio", "fraction": 1.0}"#);
    assert_eq!(ev_str(ev.last().unwrap(), "event"), Some("result"));
    assert_eq!(ev_str(ev.last().unwrap(), "audio"), Some(s(&wav).as_str()));
}

#[test]
#[serial]
fn test_extract_audio_cli_reports_an_error_event_for_a_missing_video() {
    let g = Guard::new();
    let tmp = g.path();
    let out_dir = tmp.join("out");
    let (code, out, _) = run(&Fake::default(), &["extract-audio", &s(&tmp.join("yok.mp4")), "-o", &s(&out_dir), "--json"]);
    assert_eq!(code, 1);
    let ev = events(&out);
    let kinds: Vec<&str> = ev.iter().map(|e| ev_str(e, "event").unwrap()).collect();
    assert_eq!(kinds, ["stage", "error"]);
    let message = ev_str(&ev[1], "message").unwrap();
    assert!(message.starts_with("Video bulunamadı:") && message.contains("yok.mp4"), "{message}");
    if out_dir.exists() {
        for e in std::fs::read_dir(&out_dir).unwrap() {
            let n = e.unwrap().file_name().to_string_lossy().to_string();
            assert!(!n.ends_with(".wav") && !n.contains(".tmp"), "{n}");
        }
    }
}

#[test]
#[serial]
fn test_extract_audio_cli_missing_video_without_json_prints_the_message_on_stderr() {
    let g = Guard::new();
    let tmp = g.path();
    let (code, out, err) = run(&Fake::default(), &["--lang", "en", "extract-audio", &s(&tmp.join("yok.mp4")), "-o", &s(&tmp)]);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(err.contains("Video not found:") && err.contains("yok.mp4"), "{err}");
    assert!(err.starts_with("\u{2026} extracting_audio\nerror: Video not found: "), "{err}");
}

#[test]
#[serial]
fn test_speakers_zero_with_diarize_is_rejected_before_any_stage() {
    let _g = Guard::new();
    let (code, out, _) = run(&Fake::default(), &["transcribe", "x.mp4", "--diarize", "--speakers", "0", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(out, "{\"event\": \"error\", \"message\": \"Konuşmacı sayısı en az 1 olmalı.\"}\n");
}

#[test]
#[serial]
fn test_not_transcription_model_uses_raw_id() {
    let _g = Guard::new();
    let (code, out, _) =
        run(&Fake::default(), &["--lang", "en", "transcribe", "x.mp4", "-m", "speaker-diarization", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        "{\"event\": \"error\", \"message\": \"speaker-diarization is not a speech recognition model; the 'models' command lists the valid models.\"}\n"
    );
}

#[test]
#[serial]
fn test_usage_error_exits_2_without_json_event() {
    let _g = Guard::new();
    let (code, out, err) = run(&Fake::default(), &["transcribe", "--offline", "x.mp4", "--json"]);
    assert_eq!(code, 2);
    assert_eq!(out, "");
    assert!(err.ends_with("subtitle-engine: error: unrecognized arguments: --offline\n"));
    let (code, out, _) = run(&Fake::default(), &["--version"]);
    assert_eq!((code, out.as_str()), (0, "subtitle-engine protocol 1\n"));
}
