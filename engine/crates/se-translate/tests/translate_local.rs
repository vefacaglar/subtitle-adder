//! CTranslate2 part of `tests/test_translate.py` (no network; CTranslate2/tokenizer fakes), plus the
//! `check_request` contract. The CLI tests of that file (`test_cli_*`) live with the CLI crate.

use std::cell::RefCell;
use std::rc::Rc;

use serial_test::serial;

use se_core::{Num, Sentence, Word, i18n};
use se_translate::ct2::BackendLoader;
use se_translate::{BatchOptions, Ct2Backend, Ct2Translator, Device, TextTokenizer, Translator, check_request};

/// Splits on whitespace; when decoding it joins the tokens as is.
struct FakeTokenizer;

impl TextTokenizer for FakeTokenizer {
    fn encode(&self, text: &str) -> se_core::Result<Vec<String>> {
        Ok(se_core::pyfmt::strip(text).split(se_core::pyfmt::isspace).filter(|t| !t.is_empty()).map(String::from).collect())
    }

    fn decode(&self, tokens: &[String]) -> se_core::Result<String> {
        Ok(tokens.join(" "))
    }
}

#[derive(Clone, Debug)]
struct Call {
    source: Vec<Vec<String>>,
    target_prefix: Option<Vec<Vec<String>>>,
    options: BatchOptions,
}

#[derive(Default)]
struct FakeState {
    calls: Vec<Call>,
    outputs: Option<Vec<String>>,
    /// Errors returned by the next calls (front first).
    errors: Vec<String>,
}

#[derive(Clone, Default)]
struct FakeCt2(Rc<RefCell<FakeState>>);

impl FakeCt2 {
    fn new(outputs: Option<&[&str]>) -> Self {
        let fake = FakeCt2::default();
        fake.0.borrow_mut().outputs = outputs.map(|o| o.iter().map(|s| s.to_string()).collect());
        fake
    }

    fn calls(&self) -> Vec<Call> {
        self.0.borrow().calls.clone()
    }
}

impl Ct2Backend for FakeCt2 {
    fn translate_batch(
        &mut self,
        sources: &[Vec<String>],
        target_prefix: Option<&[Vec<String>]>,
        options: &BatchOptions,
    ) -> se_core::Result<Vec<Vec<Vec<String>>>> {
        let mut state = self.0.borrow_mut();
        state.calls.push(Call { source: sources.to_vec(), target_prefix: target_prefix.map(<[_]>::to_vec), options: *options });
        if !state.errors.is_empty() {
            let message = state.errors.remove(0);
            return Err(se_core::EngineError::runtime(message));
        }
        let outputs: Vec<String> = match state.outputs.as_mut() {
            None => (0..sources.len()).map(|i| format!("çeviri {i}")).collect(),
            // share the translations given in the test across batches in order
            Some(outputs) => {
                let n = sources.len().min(outputs.len());
                outputs.drain(..n).collect()
            }
        };
        Ok(outputs.into_iter().map(|text| vec![vec![text]]).collect())
    }
}

fn make_translator(model_id: &str, outputs: Option<&[&str]>) -> (Ct2Translator, FakeCt2) {
    let fake = FakeCt2::new(outputs);
    let translator = Ct2Translator::with_parts(model_id, Box::new(FakeTokenizer), Box::new(fake.clone())).unwrap();
    (translator, fake)
}

fn sentence(id: i64, start: f64, end: f64, text: &str) -> Sentence {
    Sentence::new(id, start, end, text)
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

struct LangGuard;

impl LangGuard {
    fn new() -> Self {
        // SAFETY: #[serial] tests.
        unsafe {
            std::env::remove_var(i18n::ENV_VAR);
            std::env::remove_var(se_translate::external::EXTERNAL_LLM_URL_ENV_VAR);
        }
        i18n::set_language(None);
        LangGuard
    }
}

impl Drop for LangGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var(i18n::ENV_VAR);
            std::env::remove_var(se_translate::external::EXTERNAL_LLM_URL_ENV_VAR);
        }
        i18n::set_language(None);
    }
}

#[test]
#[serial]
fn test_madlad_uses_target_prefix_and_keeps_timing() {
    let _g = LangGuard::new();
    let mut source = sentence(3, 1.25, 2.75, "Merhaba 42 dünya.");
    source.words = vec![Word::new(1.25, 1.5, "Merhaba")];
    let (mut translator, fake) = make_translator("madlad-3b", Some(&["Merhaba 42 dünya çevirisi"]));

    let result = translator.translate(&[source], "es", None, None, None).unwrap();

    let call = &fake.calls()[0];
    // MADLAD puts the target language tag at the start of the source input; it uses no decoder prefix.
    assert_eq!(call.source, vec![s(&["<2es>", "Merhaba", "42", "dünya.", "</s>"])]);
    assert!(call.target_prefix.is_none());
    assert_eq!(call.options.beam_size, 4);
    assert!(call.options.max_decoding_length > 0);
    assert_eq!(call.options.max_batch_size, 16);
    assert_eq!(result[0].id, 3);
    assert_eq!(result[0].start, Num::Float(1.25));
    assert_eq!(result[0].end, Num::Float(2.75));
    assert_eq!(result[0].text, "Merhaba 42 dünya çevirisi");
    assert!(result[0].words.is_empty());
    assert!(!result[0].needs_review);
}

#[test]
#[serial]
fn test_nllb_uses_flores_codes() {
    let _g = LangGuard::new();
    for (source_language, target_language, source_token, target_token) in [
        ("tr", "ru", "tur_Latn", "rus_Cyrl"),
        ("pt", "es", "por_Latn", "spa_Latn"),
        ("en", "tr", "eng_Latn", "tur_Latn"),
        ("de", "tr", "deu_Latn", "tur_Latn"),
    ] {
        let (mut translator, fake) = make_translator("nllb-1.3b", Some(&["çevrilmiş"]));
        let result = translator
            .translate(&[sentence(1, 0.0, 1.0, "merhaba")], target_language, Some(source_language), None, None)
            .unwrap();
        let call = &fake.calls()[0];
        assert_eq!(call.source, vec![s(&[source_token, "merhaba", "</s>"])]);
        assert_eq!(call.target_prefix, Some(vec![s(&[target_token])]));
        assert_eq!(result[0].text, "çevrilmiş");
    }
}

#[test]
#[serial]
fn test_nllb_decoder_prefix_token_is_stripped_from_text() {
    let _g = LangGuard::new();
    // The fake decode keeps the target prefix token CTranslate2 returns at the start of an NLLB hypothesis.
    let (mut translator, _) = make_translator("nllb-1.3b", Some(&["spa_Latn hola"]));
    let result = translator.translate(&[sentence(1, 0.0, 1.0, "merhaba")], "es", Some("tr"), None, None).unwrap();
    assert_eq!(result[0].text, "hola");
}

#[test]
#[serial]
fn test_madlad_ignores_source_language() {
    let _g = LangGuard::new();
    let (mut translator, fake) = make_translator("madlad-3b", Some(&["hola"]));

    let result = translator.translate(&[sentence(1, 0.0, 1.0, "Hallo.")], "es", Some("de"), None, None).unwrap();

    assert_eq!(result[0].text, "hola");
    assert_eq!(fake.calls()[0].source, vec![s(&["<2es>", "Hallo.", "</s>"])]);
}

#[test]
#[serial]
fn test_nllb_requires_source_language() {
    let _g = LangGuard::new();
    let (mut translator, fake) = make_translator("nllb-1.3b", None);
    let err = translator.translate(&[sentence(1, 0.0, 1.0, "merhaba")], "ru", None, None, None).unwrap_err();
    assert!(err.is(se_core::ErrorKind::Model));
    assert!(err.to_string().contains("kaynak dil"), "{err}");
    // An explicit empty --from is falsy too.
    let err = translator.translate(&[sentence(1, 0.0, 1.0, "merhaba")], "ru", Some(""), None, None).unwrap_err();
    assert!(err.to_string().contains("kaynak dil"), "{err}");
    assert!(fake.calls().is_empty());
}

#[test]
#[serial]
fn test_unsupported_language_is_turkish_error() {
    let _g = LangGuard::new();
    let (mut translator, fake) = make_translator("madlad-3b", None);
    let err = translator.translate(&[sentence(1, 0.0, 1.0, "merhaba")], "ko", None, None, None).unwrap_err();
    assert!(err.to_string().contains("desteklemiyor"), "{err}");
    assert_eq!(
        err.to_string(),
        "MADLAD ko dilini desteklemiyor. Desteklenen diller: tr, en, pt, es, ru, de, fr, ja, zh."
    );
    assert!(fake.calls().is_empty());
}

#[test]
#[serial]
fn test_blank_sentences_are_not_sent_and_times_survive() {
    let _g = LangGuard::new();
    let sentences = [sentence(1, 0.0, 1.0, "Bir."), sentence(2, 1.0, 2.0, "   "), sentence(3, 2.0, 3.0, "İki.")];
    let (mut translator, fake) = make_translator("madlad-3b", Some(&["one", "two"]));

    let result = translator.translate(&sentences, "en", None, None, None).unwrap();

    assert_eq!(fake.calls()[0].source, vec![s(&["<2en>", "Bir.", "</s>"]), s(&["<2en>", "İki.", "</s>"])]);
    let times: Vec<(i64, f64, f64)> = result.iter().map(|s| (s.id, s.start_f(), s.end_f())).collect();
    assert_eq!(times, vec![(1, 0.0, 1.0), (2, 1.0, 2.0), (3, 2.0, 3.0)]);
    let texts: Vec<&str> = result.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["one", "   ", "two"]);
    assert!(result.iter().all(|s| s.words.is_empty()));
}

#[test]
#[serial]
fn test_batches_and_progress() {
    let _g = LangGuard::new();
    let (mut translator, fake) = make_translator("madlad-3b", Some(&["a", "b", "c", "d", "e"]));
    translator.batch_size = 2;
    let mut progress: Vec<f64> = Vec::new();
    let sentences: Vec<Sentence> = (0..5).map(|i| sentence(i, i as f64, i as f64 + 1.0, &format!("cümle {i}"))).collect();

    let result = {
        let mut on_progress = |f: f64| progress.push(f);
        translator.translate(&sentences, "es", None, Some(&mut on_progress), None).unwrap()
    };

    let sizes: Vec<usize> = fake.calls().iter().map(|c| c.source.len()).collect();
    assert_eq!(sizes, [2, 2, 1]);
    assert_eq!(progress, [0.4, 0.8, 1.0]);
    let texts: Vec<&str> = result.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["a", "b", "c", "d", "e"]);
}

#[test]
#[serial]
fn test_incremental_batches_emit_growing_prefix_before_progress() {
    let _g = LangGuard::new();
    // 5 lines with blanks inside and at the end; batch size 2 → batches over pending lines [0,1], [3,4], [5].
    let sentences = [
        sentence(1, 0.0, 1.0, "Bir."),
        sentence(2, 1.0, 2.0, "İki."),
        sentence(3, 2.0, 3.0, " "),
        sentence(4, 3.0, 4.0, "Üç."),
        sentence(5, 4.0, 5.0, "Dört."),
        sentence(6, 5.0, 6.0, "Beş."),
        sentence(7, 6.0, 7.0, ""),
    ];
    let (mut translator, _) = make_translator("madlad-3b", Some(&["one", "two", "three", "four", "five"]));
    translator.batch_size = 2;
    let log: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let result = {
        let mut on_batch = |prefix: &[Sentence]| {
            let texts: Vec<&str> = prefix.iter().map(|s| s.text.as_str()).collect();
            log.borrow_mut().push(format!("batch {texts:?}"));
        };
        let mut on_progress = |f: f64| log.borrow_mut().push(format!("progress {}", se_core::pyfmt::float_repr(f)));
        translator.translate(&sentences, "en", None, Some(&mut on_progress), Some(&mut on_batch)).unwrap()
    };
    assert_eq!(
        log.into_inner(),
        [
            r#"batch ["one", "two"]"#,
            "progress 0.4",
            r#"batch ["one", "two", " ", "three", "four"]"#,
            "progress 0.8",
            r#"batch ["one", "two", " ", "three", "four", "five"]"#,
            "progress 1.0",
        ]
    );
    assert_eq!(result.len(), 7);
    assert_eq!(result[6].text, "");
}

#[test]
#[serial]
fn test_nothing_to_translate_reports_full_progress_without_batches() {
    let _g = LangGuard::new();
    let (mut translator, fake) = make_translator("madlad-3b", None);
    let mut progress = Vec::new();
    let mut batches = 0;
    let mut flagged = sentence(1, 0.0, 1.0, "  ");
    flagged.needs_review = true;
    let result = {
        let mut on_progress = |f: f64| progress.push(f);
        let mut on_batch = |_: &[Sentence]| batches += 1;
        translator.translate(&[flagged], "es", None, Some(&mut on_progress), Some(&mut on_batch)).unwrap()
    };
    assert_eq!(progress, [1.0]);
    assert_eq!(batches, 0);
    assert!(fake.calls().is_empty());
    assert_eq!(result[0].text, "  ");
    // Translators build fresh sentences: an input flag is never copied.
    assert!(!result[0].needs_review);
}

#[test]
#[serial]
fn test_language_tag_does_not_leak_into_text() {
    let _g = LangGuard::new();
    let (mut translator, _) = make_translator("madlad-3b", Some(&["<2es> hola"]));
    let result = translator.translate(&[sentence(1, 0.0, 1.0, "Merhaba.")], "es", None, None, None).unwrap();
    assert_eq!(result[0].text, "hola");
    // Only ' ', ':' and ',' are removed after the tag.
    let (mut translator, _) = make_translator("madlad-3b", Some(&["<2es>: , hola"]));
    let result = translator.translate(&[sentence(1, 0.0, 1.0, "Merhaba.")], "es", None, None, None).unwrap();
    assert_eq!(result[0].text, "hola");
}

#[test]
#[serial]
fn test_empty_hypothesis_retries_then_marks_line_for_review() {
    let _g = LangGuard::new();
    let (mut translator, fake) = make_translator("madlad-3b", Some(&[""]));
    let result = translator.translate(&[sentence(1, 0.0, 1.0, " Metin. ")], "es", None, None, None).unwrap();
    // The fallback is the original, unstripped text.
    assert_eq!(result[0].text, " Metin. ");
    assert!(result[0].needs_review);
    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].source, vec![s(&["<2es>", "Metin.", "</s>"])]);
    assert_eq!(calls[1].options.max_batch_size, 1);
    assert_eq!(calls[1].options.beam_size, 4);
    let value = se_core::json::dumps(&result[0].to_value(), false);
    assert!(value.ends_with(r#""speaker": null, "needs_review": true}"#), "{value}");
}

#[test]
#[serial]
fn test_empty_hypothesis_retry_can_recover_the_line() {
    let _g = LangGuard::new();
    // A decode equal to the tag is empty too.
    let (mut translator, fake) = make_translator("madlad-3b", Some(&["<2es>", "hola"]));
    let result = translator.translate(&[sentence(1, 0.0, 1.0, "Merhaba.")], "es", None, None, None).unwrap();
    assert_eq!(result[0].text, "hola");
    assert!(!result[0].needs_review);
    assert_eq!(fake.calls().len(), 2);
}

#[test]
#[serial]
fn test_result_count_mismatch_is_translation_invalid() {
    let _g = LangGuard::new();
    let (mut translator, _) = make_translator("madlad-3b", Some(&["only one"]));
    let err = translator
        .translate(&[sentence(7, 0.0, 1.0, "Bir."), sentence(8, 1.0, 2.0, "İki.")], "es", None, None, None)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "7 numaralı altyazı için geçerli çeviri alınamadı. Çeviri tamamlanmadı; yeniden deneyin veya farklı bir çeviri modeli seçin."
    );
}

#[test]
#[serial]
fn test_translate_keeps_speaker() {
    let _g = LangGuard::new();
    let (mut translator, _) = make_translator("madlad-3b", None);
    let mut sentences = [sentence(1, 0.0, 1.0, "Merhaba."), sentence(2, 1.0, 2.0, " "), sentence(3, 2.0, 3.0, "Selam.")];
    sentences[0].speaker = Some(0);
    sentences[1].speaker = Some(1);
    let result = translator.translate(&sentences, "es", None, None, None).unwrap();
    let speakers: Vec<Option<i64>> = result.iter().map(|s| s.speaker).collect();
    assert_eq!(speakers, [Some(0), Some(1), None]);
}

#[test]
#[serial]
fn test_int_times_survive_translation() {
    let _g = LangGuard::new();
    let (mut translator, _) = make_translator("madlad-3b", Some(&["Hola."]));
    let input = Sentence { start: Num::Int(0), end: Num::Int(2), ..sentence(1, 0.0, 2.0, "Merhaba.") };
    let result = translator.translate(&[input], "es", None, None, None).unwrap();
    assert_eq!(
        se_core::json::dumps(&result[0].to_value(), false),
        r#"{"id": 1, "start": 0, "end": 2, "text": "Hola.", "words": [], "speaker": null}"#
    );
}

#[test]
#[serial]
fn test_cuda_library_error_falls_back_to_cpu_once() {
    let _g = LangGuard::new();
    let fake = FakeCt2::new(Some(&["hola", "adiós"]));
    fake.0.borrow_mut().errors.push("CUDA failed with error cublas64_12.dll is not found".to_string());
    let devices: Rc<RefCell<Vec<Device>>> = Rc::default();
    let loader: BackendLoader = {
        let fake = fake.clone();
        let devices = devices.clone();
        Box::new(move |device| {
            devices.borrow_mut().push(device);
            Ok(Box::new(fake.clone()) as Box<dyn Ct2Backend>)
        })
    };
    let mut translator = Ct2Translator::with_loader("madlad-3b", Box::new(FakeTokenizer), Device::Cuda, loader).unwrap();
    let result = translator.translate(&[sentence(1, 0.0, 1.0, "Merhaba.")], "es", None, None, None).unwrap();
    assert_eq!(result[0].text, "hola");
    assert_eq!(*devices.borrow(), [Device::Cuda, Device::Cpu]);
    assert_eq!(translator.device(), Device::Cpu);
    assert_eq!(fake.calls().len(), 2);
}

#[test]
#[serial]
fn test_non_cuda_errors_and_cpu_errors_propagate() {
    let _g = LangGuard::new();
    // On CUDA, an error that does not name a CUDA library propagates unchanged.
    let fake = FakeCt2::new(None);
    fake.0.borrow_mut().errors.push("out of memory".to_string());
    let loader: BackendLoader = {
        let fake = fake.clone();
        Box::new(move |_| Ok(Box::new(fake.clone()) as Box<dyn Ct2Backend>))
    };
    let mut translator = Ct2Translator::with_loader("madlad-3b", Box::new(FakeTokenizer), Device::Cuda, loader).unwrap();
    let err = translator.translate(&[sentence(1, 0.0, 1.0, "Merhaba.")], "es", None, None, None).unwrap_err();
    assert_eq!(err.to_string(), "out of memory");
    // On CPU even a CUDA-looking error propagates.
    let (mut translator, fake) = make_translator("madlad-3b", None);
    fake.0.borrow_mut().errors.push("cuda driver".to_string());
    let err = translator.translate(&[sentence(1, 0.0, 1.0, "Merhaba.")], "es", None, None, None).unwrap_err();
    assert_eq!(err.to_string(), "cuda driver");
    assert_eq!(fake.calls().len(), 1);
}

#[test]
#[serial]
fn test_ct2_translator_rejects_non_translation_models() {
    let _g = LangGuard::new();
    let err = Ct2Translator::with_parts("base", Box::new(FakeTokenizer), Box::new(FakeCt2::default())).err().unwrap();
    assert_eq!(err.to_string(), "Bilinmeyen çeviri modeli: base.");
}

#[test]
#[serial]
fn test_check_request_order_and_messages() {
    let _g = LangGuard::new();
    assert_eq!(
        check_request("nope", "es", None).unwrap_err().to_string(),
        "Bilinmeyen çeviri modeli: nope. Geçerli kimlikleri 'models' komutu listeler."
    );
    assert_eq!(
        check_request("speaker-diarization", "es", None).unwrap_err().to_string(),
        "Bilinmeyen çeviri modeli: speaker-diarization. Geçerli kimlikleri 'models' komutu listeler."
    );
    // Target first, before the NLLB source checks.
    assert!(check_request("nllb-1.3b", "ko", None).unwrap_err().to_string().starts_with("NLLB ko dilini"));
    assert_eq!(
        check_request("nllb-1.3b", "es", Some("ko")).unwrap_err().to_string(),
        "NLLB ko dilinden çeviri yapamıyor. Desteklenen diller: tr, en, pt, es, ru, de, fr, ja, zh."
    );
    // A missing or empty source (Python `not source`) is the same error.
    for source in [None, Some("")] {
        assert_eq!(
            check_request("nllb-1.3b", "es", source).unwrap_err().to_string(),
            "NLLB için kaynak dil (--from) zorunlu; ör. --from tr."
        );
    }
    assert_eq!(check_request("nllb-1.3b", "es", Some("tr")).unwrap().id, "nllb-1.3b");
    assert_eq!(check_request("madlad-3b", "es", Some("xx")).unwrap().id, "madlad-3b");
    assert_eq!(check_request("gemma-4-e4b", "zh", None).unwrap().id, "gemma-4-e4b");
    unsafe { std::env::set_var(i18n::ENV_VAR, "en") };
    assert_eq!(
        check_request("madlad-3b", "ko", None).unwrap_err().to_string(),
        "MADLAD does not support the language ko. Supported languages: tr, en, pt, es, ru, de, fr, ja, zh."
    );
    assert_eq!(
        check_request("nllb-1.3b", "es", Some("ko")).unwrap_err().to_string(),
        "NLLB cannot translate from ko. Supported languages: tr, en, pt, es, ru, de, fr, ja, zh."
    );
    assert_eq!(
        check_request("nllb-1.3b", "es", None).unwrap_err().to_string(),
        "NLLB requires a source language (--from), e.g. --from tr."
    );
}

#[test]
#[serial]
fn test_external_llm_url_skips_nllb_source_checks() {
    let _g = LangGuard::new();
    unsafe { std::env::set_var(se_translate::external::EXTERNAL_LLM_URL_ENV_VAR, "  ") };
    assert!(check_request("nllb-1.3b", "es", None).is_err(), "a blank URL means disabled");
    unsafe { std::env::set_var(se_translate::external::EXTERNAL_LLM_URL_ENV_VAR, "192.168.1.42:1234") };
    assert_eq!(check_request("nllb-1.3b", "es", None).unwrap().id, "nllb-1.3b");
    // The target is still checked against the catalog.
    assert!(check_request("nllb-1.3b", "ko", None).is_err());
}

#[test]
#[serial]
fn test_create_translator_rejects_unknown_ids() {
    let _g = LangGuard::new();
    let err = se_translate::create_translator("base", Device::Cpu, "int8").err().unwrap();
    assert_eq!(err.to_string(), "Bilinmeyen çeviri modeli: base. Geçerli kimlikleri 'models' komutu listeler.");
}

/// `_HfTokenizer` on a tiny `tokenizer.json` saved by Python `tokenizers` (expected values from Python).
#[test]
fn test_hf_tokenizer_matches_python_tokenizers() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny_tokenizer.json");
    let tokenizer = se_translate::ct2::HfTokenizer::open(&path).unwrap();
    assert_eq!(tokenizer.encode("hola mundo!").unwrap(), s(&["▁hola", "<unk>"]));
    assert_eq!(tokenizer.encode("spa_Latn hola").unwrap(), s(&["spa_Latn", "▁hola"]));
    // Unknown strings vanish, special tokens (language codes, </s>) are skipped.
    assert_eq!(tokenizer.decode(&s(&["spa_Latn", "▁hola", "NOPE", "▁mundo", "</s>"])).unwrap(), "hola mundo");
}

// --- Real model (equivalent of `-m slow`)---------------------------------------------------------------

fn real_dir() -> Option<std::path::PathBuf> {
    se_models::cache::model_dir("madlad-3b").ok()
}

#[test]
#[ignore = "real model (slow): needs madlad-3b in the HF cache"]
fn test_real_madlad_translates_tr_es_and_ru_pt() {
    let _ = real_dir().expect("madlad-3b is downloaded");
    let mut translator = Ct2Translator::new("madlad-3b", Device::Cpu, "int8").unwrap();
    for (source_language, target_language, text) in [("tr", "es", "Merhaba, nasılsın?"), ("ru", "pt", "Привет, как дела?")] {
        let result = translator
            .translate(&[sentence(1, 0.0, 2.0, text)], target_language, Some(source_language), None, None)
            .unwrap();
        let translated = se_core::pyfmt::strip(&result[0].text).to_string();
        assert!(!translated.is_empty());
        assert_ne!(translated.to_lowercase(), text.to_lowercase());
    }
    // Numbers and proper names must be preserved.
    let result = translator
        .translate(&[sentence(2, 0.0, 2.0, "Oda numarası 42, Mehmet ile görüşün.")], "es", Some("tr"), None, None)
        .unwrap();
    assert!(result[0].text.contains("42") && result[0].text.contains("Mehmet"), "{}", result[0].text);
}

#[test]
#[ignore = "real model (slow): needs madlad-3b in the HF cache"]
fn test_real_madlad_golden_batch() {
    let dir = real_dir().expect("madlad-3b is downloaded");
    let tokenizer = se_translate::ct2::SentencePieceTokenizer::open(&dir.join("spiece.model")).unwrap();
    // Golden encode/decode vectors from sentencepiece 0.2.x (spec translate-local.md §5.2).
    assert_eq!(tokenizer.encode("<2es> Merhaba 42 dünya.").unwrap(), s(&["▁", "<2es>", "▁Mer", "haba", "▁", "4", "2", "▁dünya", "."]));
    assert_eq!(tokenizer.encode("<2en> İki.").unwrap(), s(&["▁", "<2en>", "▁İki", "."]));
    assert_eq!(tokenizer.encode("<2tr> Hello,  world!  ").unwrap(), s(&["▁", "<2tr>", "▁Hello", ",", "▁", "▁world", "!", "▁", "▁"]));
    assert_eq!(tokenizer.encode("<2ja> こんにちは").unwrap(), s(&["▁", "<2ja>", "▁", "こ", "ん", "に", "ち", "は"]));
    assert_eq!(tokenizer.encode("<2zh> Line\nbreak").unwrap(), s(&["▁", "<2zh>", "▁Line", "\n", "break"]));
    assert_eq!(tokenizer.decode(&s(&["<2es>", "▁hola", "▁mundo"])).unwrap(), "<2es> hola mundo");
    assert_eq!(tokenizer.decode(&s(&["▁hola", "NOT_A_PIECE", "</s>", "<pad>", "<unk>"])).unwrap(), "hola ⁇  ⁇  ⁇ ");

    let mut translator = Ct2Translator::new("madlad-3b", Device::Cpu, "int8").unwrap();
    let sentences = [
        sentence(1, 0.0, 1.0, "Merhaba, nasılsın?"),
        sentence(2, 1.0, 2.0, "Привет, как дела?"),
        sentence(3, 2.0, 3.0, "Oda numarası 42, Mehmet ile görüşün."),
    ];
    // One batch of 3 in Python used a single target; translate per target here.
    let es = translator.translate(&[sentences[0].clone(), sentences[2].clone()], "es", None, None, None).unwrap();
    assert_eq!(es[0].text, "Hola, ¿cómo estás?");
    assert_eq!(es[1].text, "Habitación 42, reúnase con Mehmet.");
    let pt = translator.translate(&sentences[1..2], "pt", None, None, None).unwrap();
    assert_eq!(pt[0].text, "Olá, como estás?");
}

/// Byte parity with the Python engine on the real model: the `translation` / `translating` lines that
/// `subtitle-engine translate --to es --json` printed for `fixtures/madlad_parity_in.json`
/// (`fixtures/madlad_parity_es.jsonl`, generated with the Python engine on this machine).
#[test]
#[ignore = "real model (slow): needs madlad-3b in the HF cache"]
fn test_real_madlad_events_match_the_python_engine() {
    let _ = real_dir().expect("madlad-3b is downloaded");
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let input = std::fs::read_to_string(base.join("madlad_parity_in.json")).unwrap();
    let expected = std::fs::read_to_string(base.join("madlad_parity_es.jsonl")).unwrap().replace("\r\n", "\n");
    let sentences = se_core::types::sentences_from_value(&se_core::json::parse(&input).unwrap()).unwrap();
    let mut translator = Ct2Translator::new("madlad-3b", Device::Cpu, "int8").unwrap();
    let out: RefCell<Vec<u8>> = RefCell::new(Vec::new());
    let mut on_batch = |prefix: &[Sentence]| {
        se_core::events::emit(&mut *out.borrow_mut(), &se_core::events::translation("es", prefix)).unwrap();
    };
    let mut on_progress = |f: f64| {
        se_core::events::emit(&mut *out.borrow_mut(), &se_core::events::stage("translating", Some(f))).unwrap();
    };
    let result = translator.translate(&sentences, "es", None, Some(&mut on_progress), Some(&mut on_batch)).unwrap();
    se_core::events::emit(&mut *out.borrow_mut(), &se_core::events::translation("es", &result)).unwrap();
    assert_eq!(String::from_utf8(out.into_inner()).unwrap(), expected);
}

/// Byte parity on two batches (16 + 7 lines) with blank, whitespace-padded, multi-line, emoji, CJK and
/// speaker lines, for a Latin and a CJK target (`fixtures/madlad_parity2_*.jsonl`, Python engine output of
/// `translate --to <L> --json` without the CLI's own `loading_model` / `translating null` lines).
#[test]
#[ignore = "real model (slow): needs madlad-3b in the HF cache"]
fn test_real_madlad_two_batch_events_match_the_python_engine() {
    let _ = real_dir().expect("madlad-3b is downloaded");
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let input = std::fs::read_to_string(base.join("madlad_parity2_in.json")).unwrap();
    let sentences = se_core::types::sentences_from_value(&se_core::json::parse(&input).unwrap()).unwrap();
    let mut translator = Ct2Translator::new("madlad-3b", Device::Cpu, "int8").unwrap();
    for target in ["es", "zh"] {
        let expected = std::fs::read_to_string(base.join(format!("madlad_parity2_{target}.jsonl")))
            .unwrap()
            .replace("\r\n", "\n");
        let out: RefCell<Vec<u8>> = RefCell::new(Vec::new());
        let mut on_batch = |prefix: &[Sentence]| {
            se_core::events::emit(&mut *out.borrow_mut(), &se_core::events::translation(target, prefix)).unwrap();
        };
        let mut on_progress = |f: f64| {
            se_core::events::emit(&mut *out.borrow_mut(), &se_core::events::stage("translating", Some(f))).unwrap();
        };
        let result =
            translator.translate(&sentences, target, None, Some(&mut on_progress), Some(&mut on_batch)).unwrap();
        se_core::events::emit(&mut *out.borrow_mut(), &se_core::events::translation(target, &result)).unwrap();
        let actual = String::from_utf8(out.into_inner()).unwrap();
        if let Ok(dump) = std::env::var("SE_PARITY_DUMP") {
            std::fs::write(format!("{dump}/rust_{target}.jsonl"), &actual).unwrap();
        }
        // Beam search on borderline lines differs from the Python wheel because the GEMM kernels differ
        // (onednn-src 3.14.1 vs the oneDNN 3.1.1 the ctranslate2 4.8.4 wheel links; the Rust build is also
        // sensitive to ONEDNN_MAX_CPU_ISA while the wheel is not). Observed on a Ryzen 7500F: es 2/26 (ids 12,
        // 25), zh 6/26 texts differ. Those texts may differ,
        // everything else (ids, times, speakers, blank lines, batch prefixes, fractions) must be byte-identical.
        let texts = |line: &str| -> Vec<(i64, String)> {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            v["sentences"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| (s["id"].as_i64().unwrap(), s["text"].as_str().unwrap().to_string()))
                .collect()
        };
        let rust_texts = texts(actual.lines().last().unwrap());
        let python_texts = texts(expected.lines().last().unwrap());
        assert_eq!(rust_texts.len(), python_texts.len());
        let mut patched = actual.clone();
        let mut differing = Vec::new();
        for ((id, rust), (_, python)) in rust_texts.iter().zip(&python_texts) {
            if rust != python {
                differing.push(format!("id {id}: rust {rust:?} / python {python:?}"));
                let enc = |t: &str| format!("\"text\": {}", serde_json::to_string(t).unwrap());
                patched = patched.replace(&enc(rust), &enc(python));
            }
        }
        eprintln!("target {target}: {} of {} texts differ: {differing:#?}", differing.len(), rust_texts.len());
        assert!(differing.len() * 4 <= rust_texts.len(), "target {target}: {differing:#?}");
        assert_eq!(patched, expected, "target {target}");
    }
}

/// `_SentencePieceTokenizer` encode/decode parity with Python sentencepiece 0.2.x on the real `spiece.model`
/// (`fixtures/spm_golden.json`: tagged subtitle lines, normalization, control and special-piece cases).
#[test]
#[ignore = "real model (slow): needs madlad-3b in the HF cache"]
fn test_real_sentencepiece_matches_python() {
    let dir = real_dir().expect("madlad-3b is downloaded");
    let tokenizer = se_translate::ct2::SentencePieceTokenizer::open(&dir.join("spiece.model")).unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spm_golden.json");
    let golden: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect()
    };
    for case in golden["encode"].as_array().unwrap() {
        let text = case[0].as_str().unwrap();
        assert_eq!(tokenizer.encode(text).unwrap(), strings(&case[1]), "encode {text:?}");
    }
    for case in golden["decode"].as_array().unwrap() {
        let pieces = strings(&case[0]);
        assert_eq!(tokenizer.decode(&pieces).unwrap(), case[1].as_str().unwrap(), "decode {pieces:?}");
    }
}

/// `_SentencePieceTokenizer` load failures carry Python sentencepiece's RuntimeError text (the CLI emits
/// `str(exc)`); values observed with sentencepiece 0.2.2 on Windows.
#[test]
fn test_sentencepiece_load_errors_match_python() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope").join("spiece.model");
    let err = se_translate::ct2::SentencePieceTokenizer::open(&missing).err().unwrap();
    let shown = se_core::paths::py_path_str(&missing);
    assert_eq!(err.to_string(), format!("NOT_FOUND: \"{shown}\": No such file or directory Error #2"));
    let folder = dir.path().join("dir.model");
    std::fs::create_dir(&folder).unwrap();
    let err = se_translate::ct2::SentencePieceTokenizer::open(&folder).err().unwrap();
    let shown = se_core::paths::py_path_str(&folder);
    if cfg!(windows) {
        assert_eq!(err.to_string(), format!("NOT_FOUND: \"{shown}\": Permission denied Error #13"));
    }
    let bad = dir.path().join("bad.model");
    std::fs::write(&bad, b"garbage").unwrap();
    let err = se_translate::ct2::SentencePieceTokenizer::open(&bad).err().unwrap();
    let shown = se_core::paths::py_path_str(&bad);
    assert_eq!(err.to_string(), format!("INTERNAL: could not parse ModelProto from {shown}"));
}
