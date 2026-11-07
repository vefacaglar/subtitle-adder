//! Port of `tests/test_translation_reference_leak.py` (Qwen emitting the `source => translation` pair). The
//! pure pair-detection tests live next to the validators (`src/validate.rs`); they are repeated here through
//! the public API.

mod common;

use common::*;
use serial_test::serial;

use se_core::Sentence;
use se_translate::llm::{CheckpointMode, LlmOptions};
use se_translate::validate::{clean_translation_pair, has_source_translation_pair, looks_untranslated};
use se_translate::LlamaTranslator;

const SOURCE: &str = "eu não consigo abrir minha boca daquele jeito";
const TARGET: &str = "ben o şekilde ağzımı açamıyorum";
const ARROWS: [&str; 4] = ["=>", "->", "→", "⇒"];

fn joined(arrow: &str, reverse: bool) -> String {
    let parts = if reverse { [TARGET, SOURCE] } else { [SOURCE, TARGET] };
    parts.join(&format!(" {arrow} "))
}

#[test]
fn test_bilingual_pair_is_detected_even_when_target_text_dilutes_source_overlap() {
    for arrow in ARROWS {
        for reverse in [false, true] {
            assert!(looks_untranslated(SOURCE, &joined(arrow, reverse), Some("pt"), Some("tr")));
        }
    }
}

#[test]
fn test_pair_detection_ignores_case_quotes_and_punctuation() {
    assert!(has_source_translation_pair(SOURCE, &format!("\"{}.\" => {TARGET}", SOURCE.to_uppercase())));
}

#[test]
fn test_unambiguous_pair_is_cleaned_in_either_order() {
    for arrow in ARROWS {
        for reverse in [false, true] {
            assert_eq!(clean_translation_pair(SOURCE, &joined(arrow, reverse)), TARGET);
        }
    }
}

#[test]
fn test_literal_or_ambiguous_arrows_are_not_cut() {
    assert_eq!(clean_translation_pair("A => B", "A => B"), "A => B");
    let ambiguous = format!("{SOURCE} => {TARGET} => extra");
    assert_eq!(clean_translation_pair(SOURCE, &ambiguous), ambiguous);
}

#[test]
fn test_real_dialogue_and_literal_arrows_are_not_rejected() {
    for (source, output) in [
        (SOURCE, TARGET),
        ("The sign says A => B.", "Tabelada A => B yazıyor."),
        ("A -> B", "A → B"),
        ("Alice!", "Alice!"),
        ("2026", "2026"),
        ("Use the arrow symbol.", "=> simgesini kullan."),
    ] {
        assert!(!has_source_translation_pair(source, output));
    }
}

#[test]
#[serial]
fn test_qwen_recovers_screenshot_example_without_extra_inference() {
    let _env = Env::new();
    let first = format!("[1] {SOURCE} => {TARGET}");
    let second = format!("[1] {TARGET}");
    let (f, calls) = scripted(&[&first, &second]);
    let mut translator = with_completion("qwen3.5-4b", f);
    let (result, batches, _) = translate_collect(&mut translator, &[int_sentence(7, 10, 13, SOURCE)], "tr", Some("pt"));
    let result = result.unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(!result[0].needs_review);
    let texts: Vec<String> = batches.iter().flatten().map(|s| s.text.clone()).collect();
    assert_eq!(texts, vec![TARGET.to_string()]);
    assert_eq!((result[0].id, result[0].start, result[0].end), (7, se_core::Num::Int(10), se_core::Num::Int(13)));
    assert!(result[0].start.is_int());
}

#[test]
#[serial]
fn test_repeated_pair_is_cleaned_and_saved_as_translation() {
    let env = Env::new();
    let path = env.temp.path().join("checkpoint.json");
    let reply = format!("[1] {SOURCE} => {TARGET}");
    let mut translator = with_patched_complete(
        "qwen3.5-4b",
        Box::new(move |_| Ok(reply.clone())),
        CheckpointMode::Fixed(path.clone()),
    );
    let (result, batches, progress) =
        translate_collect(&mut translator, &[int_sentence(7, 10, 13, SOURCE)], "tr", Some("pt"));
    let result = result.unwrap();
    assert!(!path.exists());
    assert_eq!(result[0].text, TARGET);
    assert!(!result[0].needs_review);
    assert_eq!(batches.len(), 1);
    assert_eq!(progress, vec![1.0]);
}

#[test]
#[serial]
fn test_leaked_pair_in_checkpoint_is_retranslated() {
    let env = Env::new();
    let path = env.temp.path().join("checkpoint.json");
    std::fs::write(&path, format!(r#"{{"7": "{SOURCE} => {TARGET}"}}"#)).unwrap();
    let reply = format!("[1] {TARGET}");
    let (f, calls) = scripted(&[&reply]);
    let mut translator = with_patched_complete("qwen3.5-4b", f, CheckpointMode::Fixed(path.clone()));
    let result = se_translate::Translator::translate(
        &mut translator,
        &[int_sentence(7, 10, 13, SOURCE)],
        "tr",
        Some("pt"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(result[0].text, TARGET);
    assert!(!path.exists());
}

#[test]
#[serial]
fn test_copy_of_a_different_context_line_pair_is_rejected() {
    let _env = Env::new();
    let (f, calls) = scripted(&["[1] Merhaba.", "[1] Hello. => Merhaba.", "[1] Nasılsın?"]);
    let mut translator = LlamaTranslator::new(
        "qwen3.5-4b",
        LlmOptions { completion_fn: Some(f), batch_size: 1, ..LlmOptions::default() },
    )
    .unwrap();
    let sentences: Vec<Sentence> = vec![int_sentence(1, 0, 1, "Hello."), int_sentence(2, 1, 2, "How are you?")];
    let result =
        se_translate::Translator::translate(&mut translator, &sentences, "tr", Some("en"), None, None).unwrap();
    assert_eq!(calls.lock().unwrap().len(), 3);
    let texts: Vec<&str> = result.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["Merhaba.", "Nasılsın?"]);
}
