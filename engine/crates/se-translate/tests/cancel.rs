//! Translator loops stop at the next batch once cancellation is requested (the CLI's stdout is gone).

mod common;

use common::*;
use serial_test::serial;

use se_translate::Translator;

#[test]
#[serial]
fn test_llama_translator_stops_after_the_batch_that_saw_the_cancel() {
    let _env = Env::new();
    se_core::cancel::reset();
    let (f, calls) = scripted(&["[1] Bir.", "[1] Iki.", "[1] Uc."]);
    let mut translator = se_translate::LlamaTranslator::new(
        "gemma-4-e4b",
        se_translate::llm::LlmOptions { completion_fn: Some(f), batch_size: 1, ..Default::default() },
    )
    .unwrap();
    let sentences = vec![sentence(1, 0.0, 1.0, "One."), sentence(2, 1.0, 2.0, "Two."), sentence(3, 2.0, 3.0, "Three.")];
    let mut on_batch = |_: &[se_core::Sentence]| se_core::cancel::request("[Errno 32] Broken pipe");
    let result = translator.translate(&sentences, "tr", Some("en"), None, Some(&mut on_batch));
    se_core::cancel::reset();
    let err = result.unwrap_err();
    assert_eq!(err.message, "[Errno 32] Broken pipe");
    assert_eq!(calls.lock().unwrap().len(), 1);
}
