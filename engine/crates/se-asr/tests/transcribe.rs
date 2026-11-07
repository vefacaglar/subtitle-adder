//! Port of `engine/tests/test_transcribe.py` (no network, no models): word assembly from onnx-asr tokens, the
//! onnx-asr transcriber with fake loaders, backend selection, the Whisper CUDA → CPU fallback, decode options,
//! long-WAV chunking with partial words, and the ASR checkpoint key.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use se_asr::chunks::{ChunkConfig, checkpoint_path};
use se_asr::fw::{AudioInput, FwInfo, FwSegment, FwWord, SegmentIter, TranscribeOptions, WhisperBackend};
use se_asr::fw_vad::PyF;
use se_asr::onnx::{OnnxAsrModel, OnnxHooks, OnnxVad, RecognizeOptions, SegmentResult, vad_options};
use se_asr::onnx_asr::vad::OnnxVadOptions;
use se_asr::wave::write_pcm16;
use se_asr::whisper::{Device, FasterWhisperTranscriber, WHISPER_VAD_OPTIONS, WhisperHooks};
use se_asr::{OnnxAsrTranscriber, Transcriber, create_transcriber_with, words_from_tokens};
use se_core::{EngineError, Result};

fn toks(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn texts(words: &[se_core::Word]) -> Vec<String> {
    words.iter().map(|w| w.text.clone()).collect()
}

#[test]
fn test_words_from_subword_tokens_and_trailing_punctuation() {
    let words = words_from_tokens(
        Some(&toks(&["▁Mer", "ha", "ba", "▁dün", "ya", "."])),
        Some(&[0.0, 0.2, 0.3, 0.6, 0.8, 1.0]),
        2.0,
        3.0,
    );
    assert_eq!(texts(&words), vec!["Merhaba", "dünya."]);
    assert_eq!((words[0].start_f(), words[0].end_f()), (2.0, 2.6));
    assert_eq!((words[1].start_f(), words[1].end_f()), (2.6, 3.0));
}

#[test]
fn test_words_handle_space_markers_and_opening_punctuation() {
    let words = words_from_tokens(
        Some(&toks(&[" H", "ola", ",", " ¿", "qué", " tal", "?"])),
        Some(&[0.0, 0.1, 0.3, 0.4, 0.5, 0.7, 0.9]),
        0.0,
        1.0,
    );
    assert_eq!(texts(&words), vec!["Hola,", "¿qué", "tal?"]);
}

#[test]
fn test_words_standalone_marker_starts_new_word() {
    let words = words_from_tokens(
        Some(&toks(&["▁гу", "лять", " ", "гу", "лять"])),
        Some(&[0.0, 0.2, 0.4, 0.5, 0.8]),
        0.0,
        1.0,
    );
    assert_eq!(texts(&words), vec!["гулять", "гулять"]);
    let starts: Vec<f64> = words.iter().map(|w| w.start_f()).collect();
    let mut sorted = starts.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(starts, sorted);
    assert_eq!(words[1].start_f(), 0.5);
}

#[test]
fn test_words_filter_special_tokens() {
    let words =
        words_from_tokens(Some(&toks(&["<|en|>", "▁merhaba", "<blk>", "▁dünya"])), Some(&[0.0, 0.1, 0.2, 0.3]), 0.0, 1.0);
    assert_eq!(texts(&words), vec!["merhaba", "dünya"]);
    assert_eq!(words[0].start_f(), 0.1);
}

#[test]
fn test_words_estimate_timestamps_when_backend_has_none() {
    let words = words_from_tokens(Some(&toks(&[" O", "lá", ",", " tudo", " bem", "?"])), None, 1.0, 3.0);
    assert_eq!(texts(&words), vec!["Olá,", "tudo", "bem?"]);
    assert!(words[0].start_f() == 1.0 && words[words.len() - 1].end_f() == 3.0);
    assert!(words.windows(2).all(|p| p[0].end_f() <= p[1].start_f()));
    assert!(words.iter().all(|w| w.end_f() <= 3.0));
}

/// The `wav_file` fixture: 2 s of 16 kHz mono silence.
fn wav_file(dir: &Path) -> PathBuf {
    let path = dir.join("audio.wav");
    write_pcm16(&path, 16000, 1, &vec![0i16; 32000]).unwrap();
    path
}

#[derive(Default)]
struct AdapterLog {
    vad_options: Option<OnnxVadOptions>,
    kwargs: Option<RecognizeOptions>,
}

struct FakeAdapter {
    segments: Vec<SegmentResult>,
    log: Rc<RefCell<AdapterLog>>,
}

impl OnnxAsrModel for FakeAdapter {
    fn recognize(
        &mut self,
        _vad: &mut dyn OnnxVad,
        vad_options: &OnnxVadOptions,
        _audio: &Path,
        options: &RecognizeOptions,
        on_segment: &mut dyn FnMut(SegmentResult) -> Result<()>,
    ) -> Result<()> {
        let mut log = self.log.borrow_mut();
        log.vad_options = Some(*vad_options);
        log.kwargs = Some(options.clone());
        drop(log);
        for s in self.segments.drain(..) {
            on_segment(s)?;
        }
        Ok(())
    }
}

struct NoVad;
impl OnnxVad for NoVad {
    fn segments(&mut self, _w: &[f32], _o: &OnnxVadOptions) -> Result<Vec<(i64, i64)>> {
        Ok(Vec::new())
    }
}

#[derive(Default, Debug, PartialEq)]
struct Loaded {
    model: Option<(String, Option<String>)>,
    model_providers: Option<Vec<String>>,
    vad: Option<String>,
    vad_providers: Option<Vec<String>>,
}

fn seg(start: f64, end: f64, tokens: &[&str], ts: &[f64]) -> SegmentResult {
    SegmentResult { start, end, tokens: Some(toks(tokens)), timestamps: Some(ts.to_vec()) }
}

fn install_fake_onnx_asr(
    segments: Vec<SegmentResult>,
) -> (OnnxHooks, Rc<RefCell<Loaded>>, Rc<RefCell<AdapterLog>>) {
    let loaded = Rc::new(RefCell::new(Loaded::default()));
    let log = Rc::new(RefCell::new(AdapterLog::default()));
    let segments = RefCell::new(Some(segments));
    let (l1, l2, lg) = (loaded.clone(), loaded.clone(), log.clone());
    let hooks = OnnxHooks {
        load_model: Box::new(move |name, quantization, providers| {
            let mut l = l1.borrow_mut();
            l.model = Some((name.to_string(), quantization.map(str::to_string)));
            l.model_providers = Some(providers.iter().map(|p| p.to_string()).collect());
            Ok(Box::new(FakeAdapter { segments: segments.borrow_mut().take().unwrap_or_default(), log: lg.clone() })
                as Box<dyn OnnxAsrModel>)
        }),
        load_vad: Box::new(move |name, providers| {
            let mut l = l2.borrow_mut();
            l.vad = Some(name.to_string());
            l.vad_providers = Some(providers.iter().map(|p| p.to_string()).collect());
            Ok(Box::new(NoVad) as Box<dyn OnnxVad>)
        }),
    };
    (hooks, loaded, log)
}

#[test]
fn test_onnx_transcriber_uses_vad_timestamps_and_absolute_words() {
    let dir = tempfile::tempdir().unwrap();
    let wav = wav_file(dir.path());
    let (hooks, loaded, log) = install_fake_onnx_asr(vec![
        seg(0.0, 1.0, &["▁Mer", "ha", "ba"], &[0.0, 0.1, 0.2]),
        seg(1.2, 2.0, &["▁dün", "ya", "."], &[0.0, 0.2, 0.4]),
    ]);
    let mut progress = Vec::new();
    let mut on_progress = |f: f64| progress.push(f);
    let mut t = OnnxAsrTranscriber::with_hooks("nemo-canary-1b-v2", Some("int8"), &hooks).unwrap();
    let transcript = t.transcribe(&wav, Some("pt"), Some(&mut on_progress), None).unwrap();

    let cpu = Some(vec!["CPUExecutionProvider".to_string()]);
    assert_eq!(
        *loaded.borrow(),
        Loaded {
            model: Some(("nemo-canary-1b-v2".into(), Some("int8".into()))),
            model_providers: cpu.clone(),
            vad: Some("silero".into()),
            vad_providers: cpu,
        }
    );
    let log = log.borrow();
    assert_eq!(log.vad_options, Some(vad_options()));
    let vo = vad_options();
    assert_eq!((vo.speech_pad_ms, vo.min_silence_duration_ms), (50.0, 200.0));
    assert_eq!(
        log.kwargs,
        Some(RecognizeOptions { language: Some("pt".into()), target_language: Some("pt".into()) })
    );
    assert!(transcript.language == "pt" && transcript.duration == 2.0);
    assert_eq!(texts(&transcript.words), vec!["Merhaba", "dünya."]);
    assert_eq!((transcript.words[1].start_f(), transcript.words[1].end_f()), (1.2, 2.0));
    assert_eq!(progress, vec![0.5, 1.0]);
}

#[test]
fn test_onnx_single_language_model_reports_russian() {
    let dir = tempfile::tempdir().unwrap();
    let wav = wav_file(dir.path());
    let (hooks, _loaded, log) = install_fake_onnx_asr(vec![seg(0.0, 2.0, &["▁При", "вет"], &[0.0, 0.2])]);
    let mut t = OnnxAsrTranscriber::with_hooks("gigaam-v3-e2e-rnnt", Some("int8"), &hooks).unwrap();
    let transcript = t.transcribe(&wav, None, None, None).unwrap();
    assert_eq!(transcript.language, "ru");
    assert_eq!(log.borrow().kwargs, Some(RecognizeOptions { language: Some("ru".into()), target_language: None }));
}

#[test]
fn test_onnx_multilingual_model_without_language_reports_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let wav = wav_file(dir.path());
    let (hooks, _loaded, log) = install_fake_onnx_asr(vec![seg(0.0, 2.0, &["▁Hal", "lo"], &[0.0, 0.2])]);
    let mut t = OnnxAsrTranscriber::with_hooks("nemo-parakeet-tdt-0.6b-v3", Some("int8"), &hooks).unwrap();
    let transcript = t.transcribe(&wav, None, None, None).unwrap();
    assert_eq!(transcript.language, "?");
    assert_eq!(log.borrow().kwargs, Some(RecognizeOptions::default()));
}

struct Dummy;
impl Transcriber for Dummy {
    fn transcribe(
        &mut self,
        _a: &Path,
        _l: Option<&str>,
        _p: Option<se_core::ProgressFn<'_>>,
        _w: Option<se_core::WordsFn<'_>>,
    ) -> Result<se_core::Transcript> {
        unreachable!()
    }
}

#[test]
fn test_create_transcriber_selects_backend() {
    let calls: RefCell<Vec<(String, String, Option<String>)>> = RefCell::new(Vec::new());
    let mut whisper = |id: &str| -> Result<Box<dyn Transcriber>> {
        calls.borrow_mut().push(("whisper".into(), id.into(), None));
        Ok(Box::new(Dummy))
    };
    let mut onnx = |name: &str, q: Option<&str>| -> Result<Box<dyn Transcriber>> {
        calls.borrow_mut().push(("onnx".into(), name.into(), q.map(str::to_string)));
        Ok(Box::new(Dummy))
    };
    for id in ["small", "turbo", "gigaam-v3"] {
        create_transcriber_with(id, &mut whisper, &mut onnx).unwrap();
    }
    assert_eq!(
        *calls.borrow(),
        vec![
            ("whisper".to_string(), "small".to_string(), None),
            ("whisper".to_string(), "turbo".to_string(), None),
            ("onnx".to_string(), "gigaam-v3-e2e-rnnt".to_string(), Some("int8".to_string())),
        ]
    );
}

// ---- Whisper fakes -------------------------------------------------------------------------------------------

fn fw_segment(end: f64, words: &[(f64, f64, &str)]) -> FwSegment {
    FwSegment {
        id: 0,
        seek: 0,
        start: PyF::py(0.0),
        end: PyF::py(end),
        text: String::new(),
        tokens: Vec::new(),
        temperature: 0.0,
        avg_logprob: 0.0,
        compression_ratio: 0.0,
        no_speech_prob: 0.0,
        words: words
            .iter()
            .map(|&(s, e, w)| FwWord { start: PyF::py(s), end: PyF::py(e), word: w.to_string(), probability: 1.0 })
            .collect(),
    }
}

type Script = Box<dyn FnMut(Device, AudioInput<'_>, &TranscribeOptions) -> Result<(FwInfo, Vec<Result<FwSegment>>)>>;

/// A script whose model call returns no segments.
fn empty_script() -> Script {
    Box::new(|_: Device, _: AudioInput<'_>, _: &TranscribeOptions| Ok((info("en", 1.0), Vec::new())))
}

struct FakeWhisperModel {
    device: Device,
    script: Rc<RefCell<Script>>,
}

impl WhisperBackend for FakeWhisperModel {
    fn transcribe<'a>(
        &'a mut self,
        audio: AudioInput<'_>,
        _language: Option<&str>,
        options: &TranscribeOptions,
    ) -> Result<(FwInfo, SegmentIter<'a>)> {
        let (info, segs) = (self.script.borrow_mut())(self.device, audio, options)?;
        Ok((info, Box::new(segs.into_iter())))
    }
}

fn info(language: &str, duration: f64) -> FwInfo {
    FwInfo { language: language.into(), language_probability: 1.0, duration, duration_after_vad: duration }
}

/// Hooks with a fake `WhisperModel` and a recorded `(device, compute_type, local_files_only)` per load.
fn fake_hooks(cuda_usable: bool, script: Script) -> (WhisperHooks, Rc<RefCell<Vec<(String, String, bool)>>>) {
    let loads = Rc::new(RefCell::new(Vec::new()));
    let script = Rc::new(RefCell::new(script));
    let l = loads.clone();
    let hooks = WhisperHooks {
        cuda_usable: Box::new(move || cuda_usable),
        register_libraries: Box::new(|| {}),
        is_downloaded: Box::new(|_| true),
        model_factory: Box::new(move |_size, device, compute_type, local_files_only| {
            let name = match device {
                Device::Cuda => "cuda",
                _ => "cpu",
            };
            l.borrow_mut().push((name.to_string(), compute_type.to_string(), local_files_only));
            Ok(Box::new(FakeWhisperModel { device, script: script.clone() }) as Box<dyn WhisperBackend>)
        }),
    };
    (hooks, loads)
}

fn pairs(loads: &[(String, String, bool)]) -> Vec<(String, String)> {
    loads.iter().map(|(d, c, _)| (d.clone(), c.clone())).collect()
}

#[test]
fn test_whisper_falls_back_to_cpu_when_cuda_libraries_fail() {
    let dir = tempfile::tempdir().unwrap();
    let wav = wav_file(dir.path());
    let (hooks, loads) = fake_hooks(
        true,
        Box::new(|device, _a, _o| {
            if device == Device::Cuda {
                return Err(EngineError::runtime("Library cublas64_12.dll is not found or cannot be loaded"));
            }
            Ok((info("en", 1.0), vec![Ok(fw_segment(0.5, &[(0.0, 0.5, " Hello")]))]))
        }),
    );
    let mut t = FasterWhisperTranscriber::with_hooks("base", Device::Auto, None, hooks).unwrap();
    let transcript = t.run(&wav, None, None, None).unwrap();
    assert_eq!(
        pairs(&loads.borrow()),
        vec![("cuda".to_string(), "float16".to_string()), ("cpu".to_string(), "int8".to_string())]
    );
    assert_eq!(texts(&transcript.words), vec!["Hello"]);
}

#[test]
fn test_cpu_fallback_after_a_mid_run_cuda_error_keeps_progress_monotonic() {
    let dir = tempfile::tempdir().unwrap();
    let wav = wav_file(dir.path());
    let segment = |end: f64| fw_segment(end, &[(end - 0.1, end, " w")]);
    let (hooks, _loads) = fake_hooks(
        true,
        Box::new(move |device, _a, _o| {
            let mut segs = vec![Ok(segment(2.0)), Ok(segment(6.0))];
            if device == Device::Cuda {
                segs.push(Err(EngineError::runtime("CUDA failed with error out of memory")));
            } else {
                segs.push(Ok(segment(10.0)));
            }
            Ok((info("en", 10.0), segs))
        }),
    );
    let mut progress = Vec::new();
    let mut on_progress = |f: f64| progress.push(f);
    let mut t = FasterWhisperTranscriber::with_hooks("base", Device::Auto, None, hooks).unwrap();
    let transcript = t.run(&wav, None, Some(&mut on_progress), None).unwrap();
    assert_eq!(progress, vec![0.2, 0.6, 0.6, 1.0]);
    assert_eq!(transcript.words.len(), 3);
}

#[test]
fn test_whisper_uses_cpu_when_cuda_is_not_usable() {
    let (hooks, loads) = fake_hooks(false, empty_script());
    FasterWhisperTranscriber::with_hooks("base", Device::Auto, None, hooks).unwrap();
    assert_eq!(pairs(&loads.borrow()), vec![("cpu".to_string(), "int8".to_string())]);
}

#[test]
fn test_unrelated_runtime_errors_are_not_treated_as_cuda_failures() {
    assert!(se_models::cuda::is_cuda_library_error("Library cudnn_ops64_9.dll is not found"));
    assert!(!se_models::cuda::is_cuda_library_error("Invalid input features shape"));
}

#[test]
fn test_cached_whisper_model_loads_without_contacting_the_hub() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    for downloaded in [true, false] {
        let c = calls.clone();
        let hooks = WhisperHooks {
            cuda_usable: Box::new(|| false),
            register_libraries: Box::new(|| {}),
            is_downloaded: Box::new(move |_| downloaded),
            model_factory: Box::new(move |_size, _device, _ct, local_files_only| {
                c.borrow_mut().push(local_files_only);
                Ok(Box::new(FakeWhisperModel {
                    device: Device::Cpu,
                    script: Rc::new(RefCell::new(empty_script())),
                }) as Box<dyn WhisperBackend>)
            }),
        };
        FasterWhisperTranscriber::with_hooks("base", Device::Auto, None, hooks).unwrap();
    }
    assert_eq!(*calls.borrow(), vec![true, false]);
}

#[test]
fn test_whisper_passes_fast_dialogue_vad_and_decoding_options() {
    let dir = tempfile::tempdir().unwrap();
    let wav = wav_file(dir.path());
    let captured: Rc<RefCell<Option<TranscribeOptions>>> = Rc::new(RefCell::new(None));
    let c = captured.clone();
    let (hooks, _loads) = fake_hooks(
        false,
        Box::new(move |_d, _a, o| {
            *c.borrow_mut() = Some(o.clone());
            Ok((info("pt", 1.0), Vec::new()))
        }),
    );
    let mut t = FasterWhisperTranscriber::with_hooks("large-v3-turbo", Device::Auto, None, hooks).unwrap();
    t.run(&wav, Some("pt"), None, None).unwrap();
    let o = captured.borrow().clone().unwrap();
    assert_eq!(o.vad_parameters, WHISPER_VAD_OPTIONS);
    assert_eq!(o.vad_parameters.min_silence_duration_ms, 250);
    assert_eq!(o.vad_parameters.max_speech_duration_s, 14.0);
    assert!(!o.condition_on_previous_text);
    assert_eq!(o.no_speech_threshold, 0.8);
    assert_eq!(o.log_prob_threshold, -1.2);
    assert!(o.word_timestamps && o.vad_filter);
    assert_eq!(o.hallucination_silence_threshold, 1.5);
}

#[test]
fn test_whisper_streams_chunks_and_emits_partial_words_on_long_wav() {
    let dir = tempfile::tempdir().unwrap();
    let long_wav = dir.path().join("long.wav");
    write_pcm16(&long_wav, 16000, 1, &vec![0i16; 16000 * 3]).unwrap();
    let calls = Rc::new(RefCell::new(0usize));
    let c = calls.clone();
    let (hooks, _loads) = fake_hooks(
        false,
        Box::new(move |_d, audio, _o| {
            assert!(matches!(audio, AudioInput::Samples(_)), "chunks are passed as samples");
            *c.borrow_mut() += 1;
            let idx = *c.borrow();
            let word = format!(" chunk{idx}");
            Ok((info("pt", 1.0), vec![Ok(fw_segment(0.4, &[(0.1, 0.4, word.as_str())]))]))
        }),
    );
    let mut t = FasterWhisperTranscriber::with_hooks("base", Device::Auto, None, hooks).unwrap();
    t.chunks = ChunkConfig { chunk_seconds: 1.0, search_tail_seconds: 0.2, ..ChunkConfig::default() };
    t.checkpoint_root = Some(dir.path().join("_tmp").join("SubtitleAdder").join("transcriptions"));
    let mut partial_batches: Vec<Vec<String>> = Vec::new();
    let mut progress = Vec::new();
    let mut on_progress = |f: f64| progress.push(f);
    let mut on_words = |words: &[se_core::Word], _lang: &str| partial_batches.push(texts(words));
    let transcript = t.run(&long_wav, None, Some(&mut on_progress), Some(&mut on_words)).unwrap();

    let n = *calls.borrow();
    assert!(n >= 3);
    let expected: Vec<String> = (1..=n).map(|i| format!("chunk{i}")).collect();
    assert_eq!(texts(&transcript.words), expected);
    assert!(transcript.words[1].start_f() > transcript.words[0].end_f());
    assert_eq!(partial_batches[0], vec!["chunk1".to_string()]);
    assert_eq!(*partial_batches.last().unwrap(), expected);
    assert_eq!(*progress.last().unwrap(), 1.0);
    assert_eq!(transcript.language, "pt");
}

#[test]
fn test_asr_checkpoint_key_tells_apart_audio_of_the_same_name_and_size() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ckpt");
    let write_wav = |folder: &str, sample: i16| -> PathBuf {
        let d = dir.path().join(folder);
        std::fs::create_dir(&d).unwrap();
        let path = d.join("clip.wav");
        write_pcm16(&path, 16000, 1, &vec![sample; 16000 * 200]).unwrap();
        path
    };
    let first = write_wav("a", 0);
    let same = write_wav("b", 0);
    // Same name, same size, different sound (b"\x05\x01" little-endian = 0x0105).
    let different = write_wav("c", 0x0105);
    assert_eq!(std::fs::metadata(&first).unwrap().len(), std::fs::metadata(&different).unwrap().len());
    let key = |p: &Path, m: &str, l: Option<&str>| checkpoint_path(&root, p, m, l);
    assert_eq!(key(&first, "base", None), key(&same, "base", None));
    assert_ne!(key(&first, "base", None), key(&different, "base", None));
    assert_ne!(key(&first, "base", None), key(&first, "small", None));
    assert_ne!(key(&first, "base", None), key(&first, "base", Some("tr")));
}
