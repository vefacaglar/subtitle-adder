//! faster-whisper `WhisperModel.transcribe` reimplemented on CTranslate2 (spec transcribe.md §4): VAD, log-mel
//! features, language detection, temperature fallback, timestamp splitting, word timestamps via `align`,
//! hallucination-silence skipping and VAD timestamp restoration. Only the engine's option set is supported.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::Path;

use ct2rs::sys::{ComputeType, Config, StorageView, Whisper, WhisperOptions};
use se_core::{EngineError, Result};

use crate::fw_vad::{self, PyF, Speech, SpeechTimestampsMap};
use crate::mel::{FeatureExtractor, Features, N_FRAMES};
use crate::tokenizer::{HfTokenizer, JsonTokenizer, Tokenizer};
use crate::wave::WaveReader;
use crate::whisper::WhisperVadOptions;

pub use se_models::cuda::Device;

const TIME_PRECISION: f64 = 0.02;
const INPUT_STRIDE: usize = 2;
const FRAMES_PER_SECOND: f64 = 100.0;
const TOKENS_PER_SECOND: f64 = 50.0;
const TIME_PER_FRAME: f64 = 0.01;
const MAX_LENGTH: usize = 448;
const TEMPERATURES: [f64; 6] = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];
const COMPRESSION_RATIO_THRESHOLD: f64 = 2.4;
const PREPEND_PUNCTUATIONS: &str = "\"'“¿([{-";
const APPEND_PUNCTUATIONS: &str = "\"'.。,，!！?？:：”)]}、";
const PUNCTUATION: &str = "\"'“¿([{-\"'.。,，!！?？:：”)]}、";
const SENTENCE_END_MARKS: &str = ".。!！?？";

/// The `WhisperModel.transcribe` keyword arguments the engine sets (everything else stays at faster-whisper's
/// defaults). Test fakes record this.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscribeOptions {
    pub word_timestamps: bool,
    pub vad_filter: bool,
    pub vad_parameters: WhisperVadOptions,
    pub condition_on_previous_text: bool,
    pub no_speech_threshold: f64,
    pub log_prob_threshold: f64,
    pub hallucination_silence_threshold: f64,
}

/// Audio handed to the model: the WAV path (whole-file mode) or decoded samples (chunked mode).
#[derive(Clone, Copy, Debug)]
pub enum AudioInput<'a> {
    Path(&'a Path),
    Samples(&'a [f32]),
}

/// A word of a segment (`faster_whisper.transcribe.Word`).
#[derive(Clone, Debug, PartialEq)]
pub struct FwWord {
    pub start: PyF,
    pub end: PyF,
    pub word: String,
    pub probability: f64,
}

/// A yielded segment (only the fields the engine reads plus a few for debugging).
#[derive(Clone, Debug, PartialEq)]
pub struct FwSegment {
    pub id: usize,
    pub seek: usize,
    pub start: PyF,
    pub end: PyF,
    pub text: String,
    pub tokens: Vec<u32>,
    pub temperature: f64,
    pub avg_logprob: f64,
    pub compression_ratio: f64,
    pub no_speech_prob: f64,
    pub words: Vec<FwWord>,
}

/// `TranscriptionInfo` (the fields the engine reads).
#[derive(Clone, Debug, PartialEq)]
pub struct FwInfo {
    pub language: String,
    pub language_probability: f64,
    pub duration: f64,
    pub duration_after_vad: f64,
}

pub type SegmentIter<'a> = Box<dyn Iterator<Item = Result<FwSegment>> + 'a>;

/// The model as the transcriber sees it (`WhisperModel`); real or a test fake.
pub trait WhisperBackend {
    /// `model.transcribe(audio, language=..., **options)`: the eager part runs here, segments are lazy.
    fn transcribe<'a>(
        &'a mut self,
        audio: AudioInput<'_>,
        language: Option<&str>,
        options: &TranscribeOptions,
    ) -> Result<(FwInfo, SegmentIter<'a>)>;
}

fn ct2_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::runtime(e.to_string())
}

/// `compute_type` string → CTranslate2 enum.
pub fn compute_type_from_str(s: &str) -> Result<ComputeType> {
    Ok(match s {
        "default" => ComputeType::DEFAULT,
        "auto" => ComputeType::AUTO,
        "float32" => ComputeType::FLOAT32,
        "int8" => ComputeType::INT8,
        "int8_float32" => ComputeType::INT8_FLOAT32,
        "int8_float16" => ComputeType::INT8_FLOAT16,
        "int8_bfloat16" => ComputeType::INT8_BFLOAT16,
        "int16" => ComputeType::INT16,
        "float16" => ComputeType::FLOAT16,
        "bfloat16" => ComputeType::BFLOAT16,
        other => return Err(EngineError::value(format!("unsupported compute type {other}"))),
    })
}

/// `WhisperModel` on CTranslate2.
pub struct WhisperModel {
    ct2: Whisper,
    hf: JsonTokenizer,
    fe: FeatureExtractor,
    multilingual: bool,
}

/// Feature-extractor kwargs from `preprocessor_config.json` (only `feature_size` changes for the catalog).
fn feature_size(model_dir: &Path) -> usize {
    let path = model_dir.join("preprocessor_config.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 80;
    };
    match se_core::json::parse(&text) {
        Ok(v) => v.get("feature_size").and_then(|f| f.as_i64()).map(|f| f as usize).unwrap_or(80),
        Err(_) => {
            eprintln!("Could not load preprocessor config: {}", path.display());
            80
        }
    }
}

impl WhisperModel {
    /// `ctranslate2.models.Whisper(path, device, device_index=0, compute_type, intra_threads=0,
    /// inter_threads=1)` plus the tokenizer and feature extractor of the model folder.
    pub fn load(model_dir: &Path, device: Device, compute_type: &str) -> Result<Self> {
        let config = Config {
            device: match device {
                Device::Cuda => ct2rs::sys::Device::CUDA,
                _ => ct2rs::sys::Device::CPU,
            },
            compute_type: compute_type_from_str(compute_type)?,
            ..Config::default()
        };
        let ct2 = Whisper::new(model_dir, config).map_err(ct2_err)?;
        let tokenizer_path = model_dir.join("tokenizer.json");
        if !tokenizer_path.is_file() {
            return Err(EngineError::runtime(format!(
                "tokenizer.json not found in {}",
                se_core::paths::py_path_str(model_dir)
            )));
        }
        let hf = JsonTokenizer::from_file(&tokenizer_path)?;
        let n_mels = feature_size(model_dir);
        let multilingual = ct2.is_multilingual();
        Ok(Self { ct2, hf, fe: FeatureExtractor::new(n_mels), multilingual })
    }

    fn encode(&self, window: &mut [f32]) -> Result<StorageView<'static>> {
        let n_mels = self.fe.n_mels;
        let sv = StorageView::new(&[1, n_mels, N_FRAMES], window, ct2rs::sys::Device::CPU).map_err(ct2_err)?;
        self.ct2.encode(&sv, false).map_err(ct2_err)
    }

    fn detect_language(&self, features: &Features) -> Result<(String, f64)> {
        // features[..., :3000] padded/trimmed; one window (language_detection_segments=1).
        let mut window = features.window(0, N_FRAMES.min(features.frames));
        let enc = self.encode(&mut window)?;
        let results = self.ct2.detect_language(&enc).map_err(ct2_err)?;
        let first = results.into_iter().next().and_then(|r| r.into_iter().next());
        let Some(first) = first else {
            return Err(EngineError::runtime("failed to detect language"));
        };
        let (token, prob) = detection_parts(&first);
        // `token[2:-2]`
        let chars: Vec<char> = token.chars().collect();
        let lang: String = if chars.len() >= 4 { chars[2..chars.len() - 2].iter().collect() } else { String::new() };
        Ok((lang, prob))
    }

    /// The eager part of `WhisperModel.transcribe`.
    pub fn start<'a>(
        &'a self,
        audio: AudioInput<'_>,
        language: Option<&str>,
        options: &TranscribeOptions,
    ) -> Result<(FwInfo, SegmentGenerator<'a>)> {
        let audio: Vec<f32> = match audio {
            AudioInput::Samples(s) => s.to_vec(),
            AudioInput::Path(p) => decode_audio(p)?,
        };
        let duration = audio.len() as f64 / 16000.0;
        let (audio, speeches) = if options.vad_filter {
            let speeches = fw_vad::get_speech_timestamps(&audio, &options.vad_parameters)?;
            (fw_vad::collect_chunks(&audio, &speeches), speeches)
        } else {
            (audio, Vec::new())
        };
        let duration_after_vad = audio.len() as f64 / 16000.0;
        let features = self.fe.compute(&audio);
        drop(audio);
        self.start_features(features, speeches, duration, duration_after_vad, language, options)
    }

    /// The part of `WhisperModel.transcribe` after the VAD and the feature extractor: language detection,
    /// tokenizer and the lazy segment generator over precomputed `features` (of the VAD-concatenated audio) and
    /// the VAD `speeches` (sample positions in the original audio; empty = no VAD). Parity checks feed the Python
    /// feature extractor's output through it.
    pub fn start_features<'a>(
        &'a self,
        features: Features,
        speeches: Vec<fw_vad::Speech>,
        duration: f64,
        duration_after_vad: f64,
        language: Option<&str>,
        options: &TranscribeOptions,
    ) -> Result<(FwInfo, SegmentGenerator<'a>)> {
        let (language, language_probability) = match language {
            None if !self.multilingual => ("en".to_string(), 1.0),
            None => self.detect_language(&features)?,
            Some(l) if !self.multilingual && l != "en" => {
                eprintln!(
                    "The current model is English-only but the language parameter is set to '{l}'; using 'en' instead."
                );
                ("en".to_string(), 1.0)
            }
            Some(l) => (l.to_string(), 1.0),
        };
        let tokenizer = Tokenizer::new(&self.hf, self.multilingual, "transcribe", Some(&language))?;
        let suppress: Vec<i32> = tokenizer.suppressed_tokens().into_iter().map(|t| t as i32).collect();
        let prompt: Vec<String> = tokenizer
            .sot_sequence()
            .iter()
            .map(|&t| self.hf.id_to_token(t).unwrap_or_default())
            .collect();
        let info = FwInfo { language, language_probability, duration, duration_after_vad };
        let content_frames = features.content_frames();
        let generator = SegmentGenerator {
            model: self,
            features,
            tokenizer,
            suppress,
            prompt,
            options: options.clone(),
            speeches: if speeches.is_empty() { None } else { Some(SpeechTimestampsMap::new(&speeches)) },
            content_frames,
            seek: 0,
            all_tokens_len: 0,
            last_speech_timestamp: PyF::py(0.0),
            idx: 0,
            queue: VecDeque::new(),
            done: false,
        };
        Ok((info, generator))
    }
}

/// `DetectionResult` → (language token, probability).
fn detection_parts(d: &ct2rs::sys::DetectionResult) -> (String, f64) {
    // The cxx shared struct keeps its fields private; parse its Debug form
    // `DetectionResult { language: "<|en|>", probability: 0.98 }`.
    let s = format!("{d:?}");
    let token = s.split("language: \"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("").to_string();
    let prob = s
        .split("probability: ")
        .nth(1)
        .and_then(|r| r.trim_end_matches(|c| c == '}' || c == ' ').parse::<f64>().ok())
        .unwrap_or(0.0);
    (token, prob)
}

/// `decode_audio(path, 16000)` for the engine's 16 kHz mono 16-bit WAV.
pub fn decode_audio(path: &Path) -> Result<Vec<f32>> {
    let mut wav = WaveReader::open(path)?;
    if wav.nchannels() == 1 && wav.sampwidth() == 2 && wav.framerate() == 16000 {
        let n = wav.nframes();
        let samples = wav.read_i16(n)?;
        return Ok(samples.into_iter().map(|s| s as f32 / 32768.0f32).collect());
    }
    Err(EngineError::audio(format!(
        "unsupported audio for Whisper: {} Hz, {} channel(s), {} byte samples",
        wav.framerate(),
        wav.nchannels(),
        wav.sampwidth()
    )))
}

impl WhisperBackend for WhisperModel {
    fn transcribe<'a>(
        &'a mut self,
        audio: AudioInput<'_>,
        language: Option<&str>,
        options: &TranscribeOptions,
    ) -> Result<(FwInfo, SegmentIter<'a>)> {
        let (info, generator) = self.start(audio, language, options)?;
        Ok((info, Box::new(generator)))
    }
}

#[derive(Clone, Debug)]
struct AlignedWord {
    word: String,
    tokens: Vec<u32>,
    start: PyF,
    end: PyF,
    probability: f64,
}

#[derive(Clone, Debug)]
struct SubSegment {
    seek: usize,
    start: PyF,
    end: PyF,
    tokens: Vec<u32>,
    words: Vec<FwWord>,
}

/// The lazy `generate_segments` (+ `restore_speech_timestamps`) state machine.
pub struct SegmentGenerator<'a> {
    model: &'a WhisperModel,
    features: Features,
    tokenizer: Tokenizer<'a>,
    suppress: Vec<i32>,
    prompt: Vec<String>,
    options: TranscribeOptions,
    speeches: Option<SpeechTimestampsMap>,
    content_frames: usize,
    seek: usize,
    all_tokens_len: usize,
    last_speech_timestamp: PyF,
    idx: usize,
    queue: VecDeque<FwSegment>,
    done: bool,
}

/// numpy pairwise summation for small arrays (`n < 8`: sequential; `<= 128`: 8 accumulators).
fn np_sum(v: &[f64]) -> f64 {
    let n = v.len();
    if n < 8 {
        let mut s = -0.0f64;
        for &x in v {
            s += x;
        }
        if n == 0 { 0.0 } else { s }
    } else if n <= 128 {
        let mut r = [0f64; 8];
        r.copy_from_slice(&v[..8]);
        let mut i = 8;
        while i + 8 <= n {
            for k in 0..8 {
                r[k] += v[i + k];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += v[i];
            i += 1;
        }
        res
    } else {
        let mut m = n / 2;
        m -= m % 8;
        np_sum(&v[..m]) + np_sum(&v[m..])
    }
}

fn np_mean(v: &[f64]) -> f64 {
    if v.is_empty() { f64::NAN } else { np_sum(v) / v.len() as f64 }
}

fn np_median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    if n % 2 == 1 { s[n / 2] } else { np_mean(&s[n / 2 - 1..n / 2 + 1]) }
}

/// zlib compression ratio (`len(text) / len(zlib.compress(text))`).
pub fn compression_ratio(text: &str) -> f64 {
    let bytes = text.as_bytes();
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    enc.write_all(bytes).expect("in-memory write");
    let compressed = enc.finish().expect("in-memory write");
    bytes.len() as f64 / compressed.len() as f64
}

/// Python `max(a, b)` (first wins on ties).
fn py_max(a: PyF, b: PyF) -> PyF {
    if b.v > a.v { b } else { a }
}

/// Python `min(a, b)` (first wins on ties).
fn py_min(a: PyF, b: PyF) -> PyF {
    if b.v < a.v { b } else { a }
}

/// Python `max(0, x)`: the int `0` (a Python value) unless `x > 0`.
fn max0(x: PyF) -> PyF {
    if x.v > 0.0 { x } else { PyF::py(0.0) }
}

fn word_anomaly_score(w: &FwWord) -> f64 {
    let d = w.end.v - w.start.v;
    let mut score = 0.0;
    if w.probability < 0.15 {
        score += 1.0;
    }
    if d < 0.133 {
        score += (0.133 - d) * 15.0;
    }
    if d > 2.0 {
        score += d - 2.0;
    }
    score
}

fn is_segment_anomaly(seg: Option<&SubSegment>) -> bool {
    let Some(seg) = seg else {
        return false;
    };
    if seg.words.is_empty() {
        return false;
    }
    let words: Vec<&FwWord> = seg.words.iter().filter(|w| !PUNCTUATION.contains(w.word.as_str())).take(8).collect();
    let score: f64 = words.iter().map(|w| word_anomaly_score(w)).sum();
    score >= 3.0 || score + 0.01 >= words.len() as f64
}

fn next_words_segment(segs: &[SubSegment]) -> Option<&SubSegment> {
    segs.iter().find(|s| !s.words.is_empty())
}

fn get_end(segs: &[SubSegment]) -> Option<PyF> {
    for s in segs.iter().rev() {
        if let Some(w) = s.words.last() {
            return Some(w.end);
        }
    }
    segs.last().map(|s| s.end)
}

/// Python `round(x)` (ties to even) to an int.
fn py_round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

fn merge_punctuations(alignment: &mut [AlignedWord]) {
    if alignment.len() >= 2 {
        let mut i = alignment.len() as isize - 2;
        let mut j = alignment.len() - 1;
        while i >= 0 {
            let iu = i as usize;
            let prev_word = alignment[iu].word.clone();
            if prev_word.starts_with(' ') && PREPEND_PUNCTUATIONS.contains(se_core::pyfmt::strip(&prev_word)) {
                let prev_tokens = std::mem::take(&mut alignment[iu].tokens);
                let foll = &mut alignment[j];
                foll.word = format!("{prev_word}{}", foll.word);
                let mut t = prev_tokens;
                t.extend_from_slice(&foll.tokens);
                foll.tokens = t;
                alignment[iu].word = String::new();
            } else {
                j = iu;
            }
            i -= 1;
        }
    }
    let mut i = 0;
    let mut j = 1;
    while j < alignment.len() {
        let foll_word = alignment[j].word.clone();
        if !alignment[i].word.ends_with(' ') && APPEND_PUNCTUATIONS.contains(foll_word.as_str()) {
            let foll_tokens = std::mem::take(&mut alignment[j].tokens);
            alignment[j].word = String::new();
            let prev = &mut alignment[i];
            prev.word.push_str(&foll_word);
            prev.tokens.extend(foll_tokens);
        } else {
            i = j;
        }
        j += 1;
    }
}

impl<'a> SegmentGenerator<'a> {
    fn generate_with_fallback(
        &self,
        enc: &StorageView<'_>,
    ) -> Result<(ct2rs::sys::WhisperGenerationResult, f64, f64, f64)> {
        let mut all: Vec<(ct2rs::sys::WhisperGenerationResult, f64, f64, f64)> = Vec::new();
        let mut below_cr: Vec<usize> = Vec::new();
        let mut last_temperature = 0.0;
        for &temperature in &TEMPERATURES {
            last_temperature = temperature;
            let mut opts = WhisperOptions {
                length_penalty: 1.0,
                repetition_penalty: 1.0,
                no_repeat_ngram_size: 0,
                max_length: MAX_LENGTH,
                return_scores: true,
                return_no_speech_prob: true,
                suppress_blank: true,
                suppress_tokens: self.suppress.clone(),
                max_initial_timestamp_index: 50,
                ..WhisperOptions::default()
            };
            if temperature > 0.0 {
                opts.beam_size = 1;
                opts.num_hypotheses = 5;
                opts.sampling_topk = 0;
                opts.sampling_temperature = temperature as f32;
            } else {
                opts.beam_size = 5;
                opts.patience = 1.0;
            }
            let prompts = vec![self.prompt.clone()];
            let r = self
                .model
                .ct2
                .generate(enc, &prompts, &opts)
                .map_err(|e| EngineError::runtime(e.to_string().trim_start_matches("failed to generate: ").to_string()))?
                .into_iter()
                .next()
                .ok_or_else(|| EngineError::runtime("empty generation result"))?;
            let tokens: Vec<u32> = r.sequences_ids.first().map(|s| s.iter().map(|&t| t as u32).collect()).unwrap_or_default();
            let seq_len = tokens.len() as f64;
            let score = r.scores.first().copied().unwrap_or(0.0) as f64;
            let avg_logprob = score * seq_len / (seq_len + 1.0);
            let text = self.tokenizer.decode(&tokens);
            let cr = compression_ratio(se_core::pyfmt::strip(&text));
            let no_speech = r.no_speech_prob as f64;
            all.push((r, avg_logprob, temperature, cr));
            let mut needs_fallback = false;
            if cr > COMPRESSION_RATIO_THRESHOLD {
                needs_fallback = true;
            } else {
                below_cr.push(all.len() - 1);
            }
            if avg_logprob < self.options.log_prob_threshold {
                needs_fallback = true;
            }
            if no_speech > self.options.no_speech_threshold && avg_logprob < self.options.log_prob_threshold {
                needs_fallback = false;
            }
            if !needs_fallback {
                return Ok(all.pop().unwrap());
            }
        }
        let candidates: Vec<usize> = if below_cr.is_empty() { (0..all.len()).collect() } else { below_cr };
        let mut best = candidates[0];
        for &c in &candidates[1..] {
            if all[c].1 > all[best].1 {
                best = c;
            }
        }
        let (r, lp, _, cr) = all.swap_remove(best);
        Ok((r, lp, last_temperature, cr))
    }

    fn split_segments_by_timestamps(
        &self,
        tokens: &[u32],
        time_offset: f64,
        segment_size: usize,
        segment_duration: f64,
        seek: usize,
    ) -> (Vec<SubSegment>, usize, bool) {
        let tb = self.tokenizer.timestamp_begin();
        let n = tokens.len();
        let single_ending = n >= 2 && tokens[n - 2] < tb && tb <= tokens[n - 1];
        let consecutive: Vec<usize> = (1..n).filter(|&i| tokens[i] >= tb && tokens[i - 1] >= tb).collect();
        let mut segs = Vec::new();
        let mut new_seek = seek;
        if !consecutive.is_empty() {
            let mut slices = consecutive.clone();
            if single_ending {
                slices.push(n);
            }
            let mut last = 0;
            for &cur in &slices {
                let st = &tokens[last..cur];
                let start = time_offset + (st[0] as f64 - tb as f64) * TIME_PRECISION;
                let end = time_offset + (st[st.len() - 1] as f64 - tb as f64) * TIME_PRECISION;
                segs.push(SubSegment { seek, start: PyF::py(start), end: PyF::py(end), tokens: st.to_vec(), words: Vec::new() });
                last = cur;
            }
            if single_ending {
                new_seek += segment_size;
            } else {
                let pos = tokens[last - 1] as i64 - tb as i64;
                new_seek = (new_seek as i64 + pos * INPUT_STRIDE as i64) as usize;
            }
        } else {
            let mut duration = segment_duration;
            let ts: Vec<u32> = tokens.iter().copied().filter(|&t| t >= tb).collect();
            if let Some(&last) = ts.last() {
                if last != tb {
                    duration = (last - tb) as f64 * TIME_PRECISION;
                }
            }
            segs.push(SubSegment {
                seek,
                start: PyF::py(time_offset),
                end: PyF::py(time_offset + duration),
                tokens: tokens.to_vec(),
                words: Vec::new(),
            });
            new_seek += segment_size;
        }
        (segs, new_seek, single_ending)
    }

    fn find_alignment(&self, text_tokens: &[u32], enc: &StorageView<'_>, num_frames: usize) -> Result<Vec<AlignedWord>> {
        let tok = &self.tokenizer;
        let sot: Vec<usize> = tok.sot_sequence().iter().map(|&t| t as usize).collect();
        let tt: Vec<usize> = text_tokens.iter().map(|&t| t as usize).collect();
        let results = self.model.ct2.align(enc, &sot, &[tt], &[num_frames], 7).map_err(ct2_err)?;
        let Some(res) = results.into_iter().next() else {
            return Ok(Vec::new());
        };
        let probs: Vec<f64> = res.text_token_probs.iter().map(|&p| p as f64).collect();
        let text_idx: Vec<i64> = res.alignments.iter().map(|a| a.token_x).collect();
        let time_idx: Vec<i64> = res.alignments.iter().map(|a| a.frame_x).collect();
        let mut with_eot = text_tokens.to_vec();
        with_eot.push(tok.eot);
        let (words, word_tokens) = tok.split_to_word_tokens(&with_eot);
        if word_tokens.len() <= 1 {
            return Ok(Vec::new());
        }
        let mut bounds = vec![0usize];
        let mut acc = 0;
        for t in &word_tokens[..word_tokens.len() - 1] {
            acc += t.len();
            bounds.push(acc);
        }
        if bounds.len() <= 1 {
            return Ok(Vec::new());
        }
        let mut jump_times = Vec::new();
        for k in 0..time_idx.len() {
            if k == 0 || text_idx[k] != text_idx[k - 1] {
                jump_times.push(time_idx[k] as f64 / TOKENS_PER_SECOND);
            }
        }
        let at = |i: usize| -> f64 { jump_times.get(i).copied().unwrap_or_else(|| jump_times.last().copied().unwrap_or(0.0)) };
        let mut out = Vec::new();
        for k in 0..bounds.len() - 1 {
            let (a, b) = (bounds[k], bounds[k + 1]);
            let slice = &probs[a.min(probs.len())..b.min(probs.len())];
            out.push(AlignedWord {
                word: words[k].clone(),
                tokens: word_tokens[k].clone(),
                start: PyF::np(at(a)),
                end: PyF::np(at(b)),
                probability: np_mean(slice),
            });
        }
        Ok(out)
    }

    fn add_word_timestamps(&self, segs: &mut [SubSegment], enc: &StorageView<'_>, num_frames: usize) -> Result<()> {
        let eot = self.tokenizer.eot;
        let per_sub: Vec<Vec<u32>> =
            segs.iter().map(|s| s.tokens.iter().copied().filter(|&t| t < eot).collect()).collect();
        let text_tokens: Vec<u32> = per_sub.iter().flatten().copied().collect();
        let mut alignment = self.find_alignment(&text_tokens, enc, num_frames)?;

        let durations: Vec<f64> =
            alignment.iter().map(|w| w.end.v - w.start.v).filter(|&d| d != 0.0).collect();
        let median = if durations.is_empty() { 0.0 } else { np_median(&durations) };
        // Python `min(0.7, median)`: `median` only when it is smaller (NaN → 0.7).
        let median = if median < 0.7 { median } else { 0.7 };
        let max_d = median * 2.0;
        if !durations.is_empty() {
            for i in 1..alignment.len() {
                if alignment[i].end.v - alignment[i].start.v > max_d {
                    if SENTENCE_END_MARKS.contains(alignment[i].word.as_str()) {
                        alignment[i].end = PyF::np(alignment[i].start.v + max_d);
                    } else if SENTENCE_END_MARKS.contains(alignment[i - 1].word.as_str()) {
                        alignment[i].start = PyF::np(alignment[i].end.v - max_d);
                    }
                }
            }
        }
        merge_punctuations(&mut alignment);

        let mut last_speech = self.last_speech_timestamp;
        let mut word_index = 0;
        let time_offset = segs.first().map(|s| s.seek as f64 / FRAMES_PER_SECOND).unwrap_or(0.0);
        for (j, sub) in segs.iter_mut().enumerate() {
            let mut saved = 0;
            let mut words: Vec<FwWord> = Vec::new();
            while word_index < alignment.len() && saved < per_sub[j].len() {
                let timing = &alignment[word_index];
                if !timing.word.is_empty() {
                    words.push(FwWord {
                        word: timing.word.clone(),
                        start: PyF { v: time_offset + timing.start.v, np: timing.start.np }.round(2),
                        end: PyF { v: time_offset + timing.end.v, np: timing.end.np }.round(2),
                        probability: timing.probability,
                    });
                }
                saved += timing.tokens.len();
                word_index += 1;
            }
            if !words.is_empty() {
                let n = words.len();
                if words[0].end.v - last_speech.v > median * 4.0
                    && (words[0].end.v - words[0].start.v > max_d
                        || (n > 1 && words[1].end.v - words[0].start.v > max_d * 2.0))
                {
                    if n > 1 && words[1].end.v - words[1].start.v > max_d {
                        let e1 = words[1].end;
                        let boundary = py_max(PyF { v: e1.v / 2.0, np: e1.np }, PyF { v: e1.v - max_d, np: e1.np });
                        words[0].end = boundary;
                        words[1].start = boundary;
                    }
                    let e0 = words[0].end;
                    words[0].start = max0(PyF { v: e0.v - max_d, np: e0.np });
                }
                if sub.start.v < words[0].end.v && sub.start.v - 0.5 > words[0].start.v {
                    let e0 = words[0].end;
                    words[0].start = max0(py_min(PyF { v: e0.v - median, np: e0.np }, sub.start));
                } else {
                    sub.start = words[0].start;
                }
                let last = n - 1;
                if sub.end.v > words[last].start.v && sub.end.v + 0.5 < words[last].end.v {
                    let s = words[last].start;
                    words[last].end = py_max(PyF { v: s.v + median, np: s.np }, sub.end);
                } else {
                    sub.end = words[last].end;
                }
                last_speech = sub.end;
            }
            sub.words = words;
        }
        Ok(())
    }

    /// One iteration of the `generate_segments` loop; pushes finished segments to the queue.
    fn step(&mut self) -> Result<()> {
        if self.seek >= self.content_frames {
            self.done = true;
            return Ok(());
        }
        let threshold = self.options.hallucination_silence_threshold;
        let seek = self.seek;
        let time_offset = seek as f64 * TIME_PER_FRAME;
        let window_end_time = (seek + N_FRAMES) as f64 * TIME_PER_FRAME;
        let segment_size = N_FRAMES.min(self.content_frames - seek);
        let segment_duration = segment_size as f64 * TIME_PER_FRAME;
        let content_duration = self.content_frames as f64 * TIME_PER_FRAME;
        let mut window = self.features.window(seek, segment_size);
        let enc = self.model.encode(&mut window)?;
        let (result, avg_logprob, temperature, compression_ratio) = self.generate_with_fallback(&enc)?;
        let no_speech_prob = result.no_speech_prob as f64;

        let mut should_skip = no_speech_prob > self.options.no_speech_threshold;
        if avg_logprob > self.options.log_prob_threshold {
            should_skip = false;
        }
        if should_skip {
            self.seek += segment_size;
            return Ok(());
        }
        let tokens: Vec<u32> =
            result.sequences_ids.first().map(|s| s.iter().map(|&t| t as u32).collect()).unwrap_or_default();
        let previous_seek = seek;
        let (mut segs, mut new_seek, single_ending) =
            self.split_segments_by_timestamps(&tokens, time_offset, segment_size, segment_duration, seek);

        if self.options.word_timestamps {
            self.add_word_timestamps(&mut segs, &enc, segment_size)?;
            if !single_ending {
                if let Some(lwe) = get_end(&segs) {
                    if lwe.v > time_offset {
                        new_seek = py_round_int(lwe.v * FRAMES_PER_SECOND).max(0) as usize;
                    }
                }
            }
            let first = next_words_segment(&segs);
            if first.is_some() && is_segment_anomaly(first) {
                let gap = first.unwrap().start.v - time_offset;
                if gap > threshold {
                    self.seek = (previous_seek as i64 + py_round_int(gap * FRAMES_PER_SECOND)) as usize;
                    return Ok(());
                }
            }
            let mut hal_last_end = self.last_speech_timestamp.v;
            let mut truncate_at = None;
            for si in 0..segs.len() {
                if segs[si].words.is_empty() {
                    continue;
                }
                if is_segment_anomaly(Some(&segs[si])) {
                    let next = next_words_segment(&segs[si + 1..]);
                    let hal_next_start = match next {
                        Some(n) => n.words[0].start.v,
                        None => time_offset + segment_duration,
                    };
                    let s = &segs[si];
                    let silence_before = s.start.v - hal_last_end > threshold
                        || s.start.v < threshold
                        || s.start.v - time_offset < 2.0;
                    let silence_after = hal_next_start - s.end.v > threshold
                        || is_segment_anomaly(next)
                        || window_end_time - s.end.v < 2.0;
                    if silence_before && silence_after {
                        new_seek = py_round_int((time_offset + 1.0).max(s.start.v) * FRAMES_PER_SECOND).max(0) as usize;
                        if content_duration - s.end.v < threshold {
                            new_seek = self.content_frames;
                        }
                        truncate_at = Some(si);
                        break;
                    }
                }
                hal_last_end = segs[si].end.v;
            }
            if let Some(si) = truncate_at {
                segs.truncate(si);
            }
            if let Some(lwe) = get_end(&segs) {
                self.last_speech_timestamp = lwe;
            }
        }
        self.seek = new_seek;

        for s in segs {
            let text = self.tokenizer.decode(&s.tokens);
            if s.start.v == s.end.v || se_core::pyfmt::strip(&text).is_empty() {
                continue;
            }
            self.all_tokens_len += s.tokens.len();
            self.idx += 1;
            let mut seg = FwSegment {
                id: self.idx,
                seek: previous_seek,
                start: s.start,
                end: s.end,
                text,
                tokens: s.tokens,
                temperature,
                avg_logprob,
                compression_ratio,
                no_speech_prob,
                words: s.words,
            };
            if let Some(map) = &self.speeches {
                restore_segment(map, &mut seg);
            }
            self.queue.push_back(seg);
        }
        Ok(())
    }
}

/// `restore_speech_timestamps` for one segment.
fn restore_segment(map: &SpeechTimestampsMap, seg: &mut FwSegment) {
    if !seg.words.is_empty() {
        for w in &mut seg.words {
            let middle = (w.start.v + w.end.v) / 2.0;
            let ci = map.get_chunk_index(middle, false);
            w.start = map.get_original_time(w.start, Some(ci), false);
            w.end = map.get_original_time(w.end, Some(ci), false);
        }
        seg.start = seg.words[0].start;
        seg.end = seg.words[seg.words.len() - 1].end;
    } else {
        seg.start = map.get_original_time(seg.start, None, false);
        seg.end = map.get_original_time(seg.end, None, true);
    }
}

impl Iterator for SegmentGenerator<'_> {
    type Item = Result<FwSegment>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(s) = self.queue.pop_front() {
                return Some(Ok(s));
            }
            if self.done {
                return None;
            }
            if let Err(e) = self.step() {
                self.done = true;
                return Some(Err(e));
            }
        }
    }
}

/// Unused helper kept for API symmetry with Python's speech map type.
pub fn speeches_of(map: &[Speech]) -> usize {
    map.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_ratio_of_empty_text_is_zero() {
        assert_eq!(compression_ratio(""), 0.0);
    }

    #[test]
    fn numpy_reductions() {
        assert_eq!(np_median(&[0.3, 0.1, 0.2, 0.4]), 0.25);
        assert_eq!(np_mean(&[1.0, 2.0]), 1.5);
    }

    fn word(w: &str) -> AlignedWord {
        AlignedWord { word: w.to_string(), tokens: vec![1], start: PyF::np(0.0), end: PyF::np(0.0), probability: 1.0 }
    }

    #[test]
    fn punctuation_is_merged_into_neighbours() {
        let mut a = vec![word(" \""), word(" Hello"), word(","), word(" world"), word(".")];
        merge_punctuations(&mut a);
        let words: Vec<&str> = a.iter().map(|w| w.word.as_str()).collect();
        assert_eq!(words, vec!["", " \" Hello,", "", " world.", ""]);
        assert_eq!(a[1].tokens.len(), 3);
    }
}
