//! LLM-pipeline tests of `test_translation_quality.py`, `test_translation_markup.py` and
//! `test_translation_profiles.py` (batching, context, retries, review markers, checkpoints, model adapters).
//! Parser and validator unit tests live next to the code (`src/prompts.rs`, `src/validate.rs`).

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use serial_test::serial;

use se_core::{Num, Sentence, Value};
use se_translate::external::{EXTERNAL_LLM_MODEL_ENV_VAR, EXTERNAL_LLM_URL_ENV_VAR};
use se_translate::http::HttpResponse;
use se_translate::llm::{CheckpointMode, LlmOptions, translation_batches};
use se_translate::prompts::build_system_prompt;
use se_translate::{LlamaTranslator, Translator, checkpoint};

/// `cues(*texts)`: `Sentence(id=i+10, start=i*2, end=i*2+2, text, speaker=0)` (ints).
fn cues(texts: &[&str]) -> Vec<Sentence> {
    texts
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut s = int_sentence(i as i64 + 10, i as i64 * 2, i as i64 * 2 + 2, t);
            s.speaker = Some(0);
            s
        })
        .collect()
}

fn batch_texts(source: &[Sentence], limit: usize) -> Vec<Vec<String>> {
    let refs: Vec<&Sentence> = source.iter().collect();
    translation_batches(&refs, limit).into_iter().map(|(a, b)| source[a..b].iter().map(|s| s.text.clone()).collect()).collect()
}

#[test]
fn test_batch_boundary_keeps_continuing_sentence_with_its_start() {
    let source = cues(&["First.", "Second.", "Third.", "Fourth.", "If you", "want to", "come, call me."]);
    assert_eq!(
        batch_texts(&source, 6),
        vec![vec!["First.", "Second.", "Third.", "Fourth."], vec!["If you", "want to", "come, call me."]]
    );
}

#[test]
fn test_unpunctuated_dialogue_still_has_bounded_batches() {
    let source = cues(&["continuing dialogue"; 19]);
    let lens: Vec<usize> = batch_texts(&source, 6).iter().map(Vec::len).collect();
    assert_eq!(lens, vec![6, 6, 6, 1]);
}

#[test]
fn test_batch_boundary_can_follow_a_pause_or_speaker_change() {
    for (gap, speaker) in [(2.0, 0), (0.0, 1)] {
        let mut source = cues(&["unpunctuated dialogue"; 7]);
        source[4].start = Num::Float(source[4].start.f() + gap);
        for s in &mut source[4..] {
            s.speaker = Some(speaker);
        }
        let lens: Vec<usize> = batch_texts(&source, 6).iter().map(Vec::len).collect();
        assert_eq!(lens, vec![4, 3]);
    }
    // limit 1 never changes the end; quotes and brackets are stripped before the terminal check.
    let source = cues(&["a", "b"]);
    assert_eq!(batch_texts(&source, 1).len(), 2);
    let source = cues(&["One.\u{201D}", "two", "three", "four"]);
    assert_eq!(batch_texts(&source, 2)[0].len(), 1);
}

#[test]
#[serial]
fn test_previous_translations_and_following_source_reach_next_batch() {
    let _env = Env::new();
    let (f, calls) = scripted(&["[1] Müdür Hanım geldi.\n[2] Onu bekleyin.", "[1] Buyurun Müdür Hanım."]);
    let mut translator =
        LlamaTranslator::new("gemma-4-e4b", LlmOptions { completion_fn: Some(f), batch_size: 2, ..LlmOptions::default() })
            .unwrap();
    let source = cues(&["The director has arrived.", "Wait for her.", "Please come in, director."]);
    let result = translator.translate(&source, "tr", Some("en"), None, None).unwrap();
    let calls = calls.lock().unwrap();
    assert!(calls[0][1].content.contains("- Please come in, director."));
    assert!(calls[1][1].content.contains("- The director has arrived."));
    assert!(calls[1][1]
        .content
        .contains("Previous translations (terminology reference only, do NOT output):\n- Müdür Hanım geldi."));
    assert!(!calls[1][1].content.contains("=>"));
    for (r, s) in result.iter().zip(&source) {
        assert_eq!((r.id, r.start, r.end, r.speaker), (s.id, s.start, s.end, s.speaker));
        assert!(r.start.is_int() && r.words.is_empty());
    }
}

#[test]
#[serial]
fn test_invalid_single_line_is_retried_before_publishing() {
    for bad in ["", "Explanation only.", "[1] Where are you going?"] {
        let _env = Env::new();
        let (f, _) = scripted(&[bad, "[1] Nereye gidiyorsun?"]);
        let mut translator = with_completion("gemma-4-e4b", f);
        let (result, batches, _) = translate_collect(&mut translator, &cues(&["Where are you going?"]), "tr", Some("en"));
        assert_eq!(result.unwrap()[0].text, "Nereye gidiyorsun?");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0][0].text, "Nereye gidiyorsun?");
    }
}

#[test]
#[serial]
fn test_repeated_invalid_response_preserves_source_with_explicit_review_marker() {
    for reply in ["", "[1] Where are you going?", "[1] Selam.\nExplanation."] {
        let _env = Env::new();
        let (f, calls) = scripted(&[reply, reply, reply]);
        let (sink, events) = capture_diagnostics();
        let mut translator = LlamaTranslator::new(
            "gemma-4-e4b",
            LlmOptions { completion_fn: Some(f), diagnostics: Some(sink), ..LlmOptions::default() },
        )
        .unwrap();
        let (result, batches, progress) =
            translate_collect(&mut translator, &cues(&["Where are you going?"]), "tr", Some("en"));
        let result = result.unwrap();
        assert_eq!(calls.lock().unwrap().len(), 3);
        assert_eq!(result[0].text, "Where are you going?");
        assert!(result[0].needs_review);
        assert_eq!(batches.len(), 1);
        assert_eq!(progress, vec![1.0]);
        let names: Vec<String> = events.lock().unwrap().iter().map(|(e, _)| e.clone()).collect();
        assert_eq!(names, vec!["batch_start", "batch_retry", "line_recovery", "batch_complete"]);
        assert_eq!(
            diagnostics_text(&events),
            "[translation] {\"event\": \"batch_start\", \"offset\": 0, \"count\": 1, \"total\": 1}\n\
             [translation] {\"event\": \"batch_retry\", \"offset\": 0, \"invalid_lines\": 1}\n\
             [translation] {\"event\": \"line_recovery\", \"offset\": 0}\n\
             [translation] {\"event\": \"batch_complete\", \"processed\": 1, \"total\": 1}\n"
        );
    }
}

#[test]
#[serial]
fn test_names_numbers_and_short_shared_expressions_are_not_rejected() {
    for text in ["Alice!", "New York", "2026", "OK."] {
        let _env = Env::new();
        let reply = format!("[1] {text}");
        let mut translator = with_completion("gemma-4-e4b", Box::new(move |_| Ok(reply.clone())));
        assert_eq!(translator.translate(&cues(&[text]), "tr", Some("en"), None, None).unwrap()[0].text, text);
    }
}

#[test]
#[serial]
fn test_http_truncated_translation_is_retried_not_published() {
    for ollama in [false, true] {
        let _env = Env::new();
        let count = Arc::new(Mutex::new(0usize));
        let c = count.clone();
        let transport = FakeTransport::new(move |_| {
            let mut n = c.lock().unwrap();
            *n += 1;
            let truncated = *n == 1;
            let content = if truncated { "[1] Yarın size" } else { "[1] Yarın size haber vereceğiz." };
            let reason = if truncated { "length" } else { "stop" };
            let payload = if ollama {
                format!(r#"{{"message": {{"content": "{content}"}}, "done_reason": "{reason}"}}"#)
            } else {
                format!(r#"{{"choices": [{{"message": {{"content": "{content}"}}, "finish_reason": "{reason}"}}]}}"#)
            };
            Ok(HttpResponse { status: 200, body: payload.into_bytes() })
        });
        let url = if ollama { "http://127.0.0.1:11434" } else { "http://127.0.0.1:1234" };
        let mut options = external(url, Some("test-model"), None, transport.clone());
        options.checkpoint = CheckpointMode::Disabled;
        let mut translator = LlamaTranslator::new("gemma-4-e4b", options).unwrap();
        let (result, batches, _) =
            translate_collect(&mut translator, &cues(&["We will let you know tomorrow."]), "tr", Some("en"));
        let result = result.unwrap();
        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].url.ends_with("/api/chat"), ollama);
        assert_eq!(result[0].text, "Yarın size haber vereceğiz.");
        let published: Vec<String> = batches.iter().map(|b| b[0].text.clone()).collect();
        assert_eq!(published, vec![result[0].text.clone()]);
    }
}

#[test]
#[serial]
fn test_checkpoint_identity_includes_scene_metadata_and_server() {
    let env = Env::new();
    let mut source = cues(&["Wait for me."]);
    let path = |s: &[Sentence]| checkpoint::checkpoint_path("gemma-4-e4b", "tr", Some("en"), s, "").unwrap();
    let original = path(&source);
    source[0].speaker = Some(1);
    let changed_speaker = path(&source);
    source[0].start = Num::Float(0.5);
    let changed_time = path(&source);
    env.set(EXTERNAL_LLM_URL_ENV_VAR, "http://127.0.0.1:1234");
    let changed_server = path(&source);
    let all: std::collections::HashSet<_> = [original, changed_speaker, changed_time, changed_server].into_iter().collect();
    assert_eq!(all.len(), 4);
}

#[test]
#[serial]
fn test_checkpoint_identity_follows_the_prompt_and_the_configured_remote_model() {
    let env = Env::new();
    let source = cues(&["Wait for me."]);
    let path = |p: &str| checkpoint::checkpoint_path("gemma-4-e4b", "tr", Some("en"), &source, p).unwrap();
    let base = path("prompt A");
    assert_eq!(path("prompt A"), base);
    assert_ne!(path("prompt B"), base);
    env.set(EXTERNAL_LLM_MODEL_ENV_VAR, "qwen3");
    assert_ne!(path("prompt A"), base);
    // int 0 and float 0.0 hash differently (Python str()).
    let mut floats = source.clone();
    floats[0].start = Num::Float(0.0);
    assert_ne!(
        checkpoint::checkpoint_file_name("gemma-4-e4b", "tr", None, &source, ""),
        checkpoint::checkpoint_file_name("gemma-4-e4b", "tr", None, &floats, "")
    );
}

#[test]
#[serial]
fn test_translators_for_other_servers_or_models_do_not_share_a_checkpoint() {
    let env = Env::new();
    let seen: Arc<Mutex<Vec<std::path::PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let source = cues(&["Wait for me."]);
    let unused = env.temp.path().join("unused.json");
    let run = |url: &str, resolved_model: &str| {
        let rec = seen.clone();
        let unused = unused.clone();
        let mut translator = LlamaTranslator::new(
            "madlad-3b",
            LlmOptions {
                completion_fn: Some(Box::new(|_| Ok("[1] Bekle beni.".to_string()))),
                completion_keeps_checkpoint: true,
                external_url: Some(url.to_string()),
                external_model: Some(resolved_model.to_string()),
                checkpoint: CheckpointMode::Custom(Box::new(move |m, t, s, sents, identity| {
                    rec.lock().unwrap().push(checkpoint::checkpoint_path(m, t, s, sents, identity)?);
                    Ok(Some(unused.clone()))
                })),
                ..LlmOptions::default()
            },
        )
        .unwrap();
        translator.translate(&source, "tr", Some("en"), None, None).unwrap();
    };
    run("http://10.0.0.2:1234", "qwen3");
    run("http://10.0.0.3:1234", "qwen3");
    run("http://10.0.0.2:1234", "gemma4");
    run("http://10.0.0.2:1234", "qwen3");
    let seen = seen.lock().unwrap();
    let first3: std::collections::HashSet<_> = seen[..3].iter().collect();
    assert_eq!(first3.len(), 3);
    assert_eq!(seen[3], seen[0]);
}

#[test]
#[serial]
fn test_translategemma_uses_supported_user_template_and_discards_context_translations() {
    for model in ["translategemma:4b", "google/translate-gemma-4b-it"] {
        let _env = Env::new();
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let rec = calls.clone();
        let mut translator = LlamaTranslator::new(
            "gemma-4-e4b",
            LlmOptions {
                completion_fn: Some(Box::new(move |m| {
                    rec.lock().unwrap().push(m.to_vec());
                    Ok("[1] Merhaba.\n[2] Nasılsın?\n[3] İyiyim.".to_string())
                })),
                external_url: Some("http://127.0.0.1:11434".into()),
                external_model: Some(model.into()),
                batch_size: 2,
                ..LlmOptions::default()
            },
        )
        .unwrap();
        let source = cues(&["Hello.", "How are you?", "I'm fine."]);
        let (result, batches, _) = translate_collect(&mut translator, &source, "tr", Some("en"));
        let result = result.unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        for messages in calls.iter() {
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].role, "user");
            let prompt = &messages[0].content;
            assert!(prompt.contains("English (en) to Turkish (tr)"));
            assert!(prompt.contains("\n\n\n[1] Hello.\n[2] How are you?\n[3] I'm fine."));
            assert!(!prompt.contains("Previous lines") && !prompt.contains("Following lines"));
        }
        let texts: Vec<&str> = result.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["Merhaba.", "Nasılsın?", "İyiyim."]);
        assert_eq!(batches[0].iter().map(|s| s.id).collect::<Vec<_>>(), vec![10, 11]);
        assert_eq!(result.iter().map(|s| s.id).collect::<Vec<_>>(), vec![10, 11, 12]);
    }
}

#[test]
#[serial]
fn test_translategemma_without_source_fails_before_sending_text() {
    let _env = Env::new();
    let mut translator = LlamaTranslator::new(
        "gemma-4-e4b",
        LlmOptions {
            completion_fn: Some(Box::new(|_| panic!("no text may be sent"))),
            external_url: Some("http://127.0.0.1:11434".into()),
            external_model: Some("translategemma:4b".into()),
            ..LlmOptions::default()
        },
    )
    .unwrap();
    let err = translator.translate(&cues(&["Hello there."]), "tr", None, None, None).unwrap_err().to_string();
    assert!(err.contains("TranslateGemma için kaynak dil gerekli"), "{err}");
}

#[test]
#[serial]
fn test_failed_later_batch_keeps_only_valid_checkpoint_and_resumes_with_context() {
    let env = Env::new();
    let path = env.temp.path().join("translation.json");
    let replies: Arc<Mutex<Vec<se_core::Result<String>>>> = Arc::new(Mutex::new(vec![
        Err(se_core::EngineError::model("server disconnected")),
        Ok("[1] Müdür Hanım geldi.".to_string()),
    ]));
    let r = replies.clone();
    let mut translator = LlamaTranslator::new(
        "gemma-4-e4b",
        LlmOptions {
            completion_fn: Some(Box::new(move |_| r.lock().unwrap().pop().expect("reply"))),
            completion_keeps_checkpoint: true,
            checkpoint: CheckpointMode::Fixed(path.clone()),
            batch_size: 1,
            ..LlmOptions::default()
        },
    )
    .unwrap();
    let source = cues(&["The director has arrived.", "Please come in, director."]);
    assert!(translator.translate(&source, "tr", Some("en"), None, None).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"10": "Müdür Hanım geldi."}"#);

    let (f, calls) = scripted(&["[1] Buyurun Müdür Hanım."]);
    let mut translator_1 = LlamaTranslator::new(
        "gemma-4-e4b",
        LlmOptions {
            completion_fn: Some(f),
            completion_keeps_checkpoint: true,
            checkpoint: CheckpointMode::Fixed(path.clone()),
            batch_size: 1,
            ..LlmOptions::default()
        },
    )
    .unwrap();
    let result = translator_1.translate(&source, "tr", Some("en"), None, None).unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0][1]
        .content
        .contains("Previous translations (terminology reference only, do NOT output):\n- Müdür Hanım geldi."));
    assert_eq!(result[0].text, "Müdür Hanım geldi.");
    assert_eq!(result[1].text, "Buyurun Müdür Hanım.");
    assert!(!path.exists());
}

#[test]
#[serial]
fn test_valid_neighbors_survive_bad_retry_and_single_line_recovery() {
    let _env = Env::new();
    let (f, calls) = scripted(&["[1] Merhaba.\n[2] Where are you going?", "", "[1] Nereye gidiyorsun?"]);
    let mut translator = with_completion("qwen3.5-4b", f);
    let result = translator.translate(&cues(&["Hello.", "Where are you going?"]), "tr", Some("en"), None, None).unwrap();
    let texts: Vec<&str> = result.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["Merhaba.", "Nereye gidiyorsun?"]);
    assert!(!result.iter().any(|s| s.needs_review));
    let calls = calls.lock().unwrap();
    let last = &calls.last().unwrap().last().unwrap().content;
    assert!(last.contains("[1] Where are you going?"));
    assert!(last.contains("Previous lines (for context only, do NOT translate):\n- Hello."));
}

#[test]
#[serial]
fn test_unrecoverable_line_does_not_block_following_batch() {
    let _env = Env::new();
    let (f, _) = scripted(&["", "", "", "[1] Nereye gidiyorsun?"]);
    let mut translator =
        LlamaTranslator::new("qwen3.5-4b", LlmOptions { completion_fn: Some(f), batch_size: 1, ..LlmOptions::default() })
            .unwrap();
    let result = translator.translate(&cues(&["Hello there.", "Where are you going?"]), "tr", Some("en"), None, None).unwrap();
    assert!(result[0].needs_review && result[0].text == "Hello there.");
    assert!(result[1].text == "Nereye gidiyorsun?" && !result[1].needs_review);
    assert!(Sentence::from_value(&result[0].to_value()).unwrap().needs_review);
    assert!(result[1].to_value().get("needs_review").is_none());
}

// ---- test_translation_markup.py -----------------------------------------------------------------------

const MARKUP_SOURCE: &str = "Sua língua, véia, horrorosa.";
const BAD: &str = "*Kafan* *kötü* *kadın* *kıskanç* *yapıyor*.";

#[test]
#[serial]
fn test_screenshot_output_is_retried_instead_of_cleaned_and_published() {
    let _env = Env::new();
    let first = format!("[1] {BAD}");
    let (f, _) = scripted(&[&first, "[1] Test çevirisi."]);
    let mut translator = with_completion("gemma-4-e4b", f);
    let (result, batches, _) = translate_collect(&mut translator, &[int_sentence(7, 10, 12, MARKUP_SOURCE)], "tr", Some("pt"));
    let result = result.unwrap();
    assert_eq!(result[0].text, "Test çevirisi.");
    assert!(!result[0].needs_review);
    let texts: Vec<String> = batches.iter().flatten().map(|s| s.text.clone()).collect();
    assert_eq!(texts, vec!["Test çevirisi."]);
}

#[test]
#[serial]
fn test_repeated_markup_remains_saveable_but_is_marked_for_review() {
    let _env = Env::new();
    let bad = format!("[1] {BAD}");
    let (f, calls) = scripted(&[&bad, &bad, &bad]);
    let mut translator = with_completion("gemma-4-e4b", f);
    let result = translator.translate(&[int_sentence(7, 10, 12, MARKUP_SOURCE)], "tr", Some("pt"), None, None).unwrap();
    assert_eq!(calls.lock().unwrap().len(), 3);
    assert_eq!(result[0].text, MARKUP_SOURCE);
    assert!(result[0].needs_review);
}

#[test]
#[serial]
fn test_marked_up_checkpoint_does_not_bypass_validation() {
    let env = Env::new();
    let path = env.temp.path().join("checkpoint.json");
    std::fs::write(&path, format!(r#"{{"7": "{BAD}"}}"#)).unwrap();
    let (f, calls) = scripted(&["[1] Test çevirisi."]);
    let mut translator = with_patched_complete("gemma-4-e4b", f, CheckpointMode::Fixed(path));
    let result = translator.translate(&[int_sentence(7, 10, 12, MARKUP_SOURCE)], "tr", Some("pt"), None, None).unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(result[0].text, "Test çevirisi.");
}

#[test]
#[serial]
fn test_valid_checkpoint_skips_the_request() {
    let env = Env::new();
    let path = env.temp.path().join("checkpoint.json");
    std::fs::write(&path, r#"{"7": "Kaydedilmiş çeviri."}"#).unwrap();
    let mut translator =
        with_patched_complete("gemma-4-e4b", Box::new(|_| panic!("cached")), CheckpointMode::Fixed(path.clone()));
    let (result, batches, progress) =
        translate_collect(&mut translator, &[int_sentence(7, 10, 12, MARKUP_SOURCE)], "tr", Some("pt"));
    assert_eq!(result.unwrap()[0].text, "Kaydedilmiş çeviri.");
    assert_eq!((batches.len(), progress), (1, vec![1.0]));
    assert!(!path.exists());
}

// ---- test_translation_profiles.py (pipeline parts) ----------------------------------------------------

#[test]
#[serial]
fn test_chat_translation_and_retry_keep_model_roles_and_context() {
    for (model, roles, soft_switch) in [
        ("gemma3:27b", vec!["user"], false),
        ("gemma-4-e4b", vec!["system", "user"], false),
        ("qwen3:8b", vec!["system", "user"], true),
        ("qwen3.5:4b", vec!["system", "user"], false),
        ("custom-model", vec!["system", "user"], false),
    ] {
        let _env = Env::new();
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let rec = calls.clone();
        let mut replies = vec!["[1] Merhaba.".to_string(), "invalid response".to_string()];
        let mut translator = LlamaTranslator::new(
            "gemma-4-e4b",
            LlmOptions {
                completion_fn: Some(Box::new(move |m| {
                    rec.lock().unwrap().push(m.to_vec());
                    Ok(replies.pop().unwrap())
                })),
                external_url: Some("http://127.0.0.1:1234".into()),
                external_model: Some(model.into()),
                ..LlmOptions::default()
            },
        )
        .unwrap();
        let result = translator.translate(&[int_sentence(42, 1, 2, "Hello.")], "tr", Some("en"), None, None).unwrap();
        assert_eq!((result[0].id, result[0].text.as_str()), (42, "Merhaba."));
        let calls = calls.lock().unwrap();
        for messages in calls.iter() {
            assert_eq!(messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(), roles, "{model}");
            let last = &messages.last().unwrap().content;
            assert!(last.contains("[1] Hello."));
            assert_eq!(last.ends_with("/no_think"), soft_switch, "{model}");
            assert!(messages[0].content.contains(&build_system_prompt("tr", Some("en"))));
        }
        assert!(calls[1].last().unwrap().content.contains("previous attempt"));
    }
}

#[test]
#[serial]
fn test_http_options_match_model_and_server_not_toolbar_selection() {
    for (url, ollama) in [("http://127.0.0.1:1234", false), ("http://127.0.0.1:11434", true), ("http://127.0.0.1:9000/api/chat", true)] {
        for (model, thinking, hy_mt) in [
            ("qwen3.5:4b", true, false),
            ("gemma4:12b", true, false),
            ("gemma3:27b", false, false),
            ("translategemma:4b", false, false),
            ("hy-mt1.5:7b", false, true),
            ("custom-model", false, false),
        ] {
            let _env = Env::new();
            let payload = if ollama {
                r#"{"message": {"content": "[1] Merhaba."}}"#
            } else {
                r#"{"choices": [{"message": {"content": "[1] Merhaba."}}]}"#
            };
            let transport = FakeTransport::json(payload);
            let mut options = external(url, Some(model), None, transport.clone());
            options.resolver = identity_resolver();
            let mut translator = LlamaTranslator::new("gemma-4-e4b", options).unwrap();
            assert_eq!(translator.complete(&[se_translate::ChatMessage::user("test")]).unwrap(), "[1] Merhaba.");
            let request = &transport.requests()[0];
            let body = request.json_body().unwrap();
            let ctx = format!("{url} {model}");
            assert_eq!(body.get("model").and_then(Value::as_str), Some(model));
            assert_eq!(request.url.ends_with("/api/chat"), ollama, "{ctx}");
            assert_eq!(body.get("think").is_some(), thinking && ollama, "{ctx}");
            assert_eq!(body.get("chat_template_kwargs").is_some(), thinking && !ollama, "{ctx}");
            if thinking {
                let off = if ollama {
                    body.get("think").cloned()
                } else {
                    body.get("chat_template_kwargs").and_then(|k| k.get("enable_thinking")).cloned()
                };
                assert_eq!(off, Some(Value::Bool(false)));
            }
            let sampling = if ollama { body.get("options").unwrap().clone() } else { body.clone() };
            assert_eq!(sampling.get("temperature").and_then(Value::as_f64), Some(if hy_mt { 0.7 } else { 0.2 }));
            if hy_mt {
                assert_eq!(sampling.get("top_p").and_then(Value::as_f64), Some(0.6));
                if ollama {
                    assert_eq!(sampling.get("top_k").and_then(Value::as_i64), Some(20));
                    assert_eq!(sampling.get("repeat_penalty").and_then(Value::as_f64), Some(1.05));
                } else {
                    assert!(sampling.get("top_k").is_none() && sampling.get("repetition_penalty").is_none());
                    assert!(sampling.get("repeat_penalty").is_none());
                }
            }
            if ollama {
                assert_eq!(sampling.get("num_predict").and_then(Value::as_i64), Some(1536));
                assert!(body.get("max_tokens").is_none() && body.get("temperature").is_none());
            } else {
                assert_eq!(body.get("max_tokens").and_then(Value::as_i64), Some(1536));
                assert!(body.get("options").is_none());
            }
            assert_eq!(body.get("stream"), Some(&Value::Bool(false)));
        }
    }
}

#[test]
#[serial]
fn test_local_qwen_uses_catalog_profile_without_external_server() {
    let _env = Env::new();
    let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = captured.clone();
    let port = serve(move |r| {
        rec.lock().unwrap().push(se_core::json::parse(std::str::from_utf8(&r.body).unwrap()).unwrap());
        Reply::json(r#"{"choices":[{"message":{"content":"[1] hello"}}]}"#)
    });
    // Python: `completion_fn=lambda _: ""`, then `_completion_fn = None`, `_process`/`_port` faked.
    let mut translator =
        LlamaTranslator::new("qwen3.5-4b", LlmOptions { local_port: Some(port), ..LlmOptions::default() }).unwrap();
    translator.complete(&[se_translate::ChatMessage::user("test")]).unwrap();
    let captured = captured.lock().unwrap();
    assert_eq!(
        captured[0].get("chat_template_kwargs").map(|v| se_core::json::dumps(v, false)).as_deref(),
        Some(r#"{"enable_thinking": false}"#)
    );
    assert!(captured[0].get("options").is_none() && captured[0].get("think").is_none());
    assert!(captured[0].get("model").is_none() && captured[0].get("stream").is_none());
    assert_eq!(captured[0].get("cache_prompt"), Some(&Value::Bool(false)));
}

#[test]
#[serial]
fn test_hy_mt15_context_is_reference_not_extra_translation() {
    let _env = Env::new();
    let (f, calls) = scripted(&["[1] Müdür Hanım geldi.\n[2] Onu bekleyin.", "[1] Buyurun Müdür Hanım."]);
    let mut translator = LlamaTranslator::new(
        "gemma-4-e4b",
        LlmOptions {
            completion_fn: Some(f),
            external_url: Some("http://127.0.0.1:11434".into()),
            external_model: Some("hy-mt1.5:7b".into()),
            batch_size: 2,
            ..LlmOptions::default()
        },
    )
    .unwrap();
    let source = vec![
        int_sentence(10, 0, 2, "The director has arrived."),
        int_sentence(11, 2, 4, "Wait for her."),
        int_sentence(12, 4, 6, "Please come in, director."),
    ];
    let result = translator.translate(&source, "tr", Some("en"), None, None).unwrap();
    let calls = calls.lock().unwrap();
    assert!(calls.iter().all(|m| m.len() == 1 && m[0].role == "user"));
    let prompt = &calls[1][0].content;
    assert!(prompt.contains("The director has arrived."));
    assert!(prompt.contains("Previous translations (terminology reference only):\nMüdür Hanım geldi."));
    assert!(!prompt.contains("=>"));
    assert!(prompt.contains("不需要翻译上文"));
    assert!(prompt.ends_with("[1] Please come in, director."));
    assert_eq!(result.iter().map(|s| s.id).collect::<Vec<_>>(), vec![10, 11, 12]);
    assert_eq!(result[2].text, "Buyurun Müdür Hanım.");
}

#[test]
#[serial]
fn test_hunyuan_legacy_window_preserves_subtitle_ids() {
    let _env = Env::new();
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let rec = calls.clone();
    let mut translator = LlamaTranslator::new(
        "gemma-4-e4b",
        LlmOptions {
            completion_fn: Some(Box::new(move |m| {
                rec.lock().unwrap().push(m.to_vec());
                Ok("[1] Merhaba.\n[2] Hoş geldin.".to_string())
            })),
            external_url: Some("http://127.0.0.1:1234".into()),
            external_model: Some("tencent/Hunyuan-MT-7B".into()),
            batch_size: 1,
            ..LlmOptions::default()
        },
    )
    .unwrap();
    let result = translator
        .translate(&[int_sentence(10, 0, 1, "Hello."), int_sentence(11, 1, 2, "Welcome.")], "tr", Some("en"), None, None)
        .unwrap();
    assert_eq!(result.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["Merhaba.", "Hoş geldin."]);
    let calls = calls.lock().unwrap();
    assert!(calls.iter().all(|m| m.len() == 1));
    assert!(calls[0][0].content.starts_with("Translate the following segment into Turkish,"));
}

#[test]
#[serial]
fn test_discovery_uses_the_first_listed_model_and_falls_back_to_the_catalog_id() {
    let _env = Env::new();
    let transport = FakeTransport::new(|req| {
        if req.url.ends_with("/models") {
            Ok(HttpResponse { status: 200, body: br#"{"models": [{"name": " llama3:8b "}]}"#.to_vec() })
        } else {
            Ok(HttpResponse { status: 200, body: CHAT_REPLY.as_bytes().to_vec() })
        }
    });
    let mut translator = LlamaTranslator::new("madlad-3b", external("http://127.0.0.1:11434/api/chat", None, None, transport.clone())).unwrap();
    assert_eq!(translator.discover_external_model().unwrap(), "llama3:8b");
    let r = transport.requests();
    assert_eq!(r[0].url, "http://127.0.0.1:11434/v1/models");
    assert_eq!(r[0].method, "GET");

    let failing = FakeTransport::new(|_| Err(se_translate::http::HttpFailure::Other("timed out".into())));
    let mut translator = LlamaTranslator::new("madlad-3b", external("http://127.0.0.1:1234", None, None, failing.clone())).unwrap();
    assert_eq!(translator.discover_external_model().unwrap(), "madlad-3b");
    assert_eq!(translator.discover_external_model().unwrap(), "madlad-3b");
    assert_eq!(failing.requests().len(), 1); // cached
}
