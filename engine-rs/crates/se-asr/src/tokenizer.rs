//! faster-whisper `Tokenizer` (`faster_whisper/tokenizer.py`; spec transcribe.md §4.5) on HF `tokenizers`.

use std::collections::BTreeSet;
use std::path::Path;

use se_core::{EngineError, Result};

pub const TASKS: [&str; 2] = ["transcribe", "translate"];

/// `_LANGUAGE_CODES` (order used in the error text).
pub const LANGUAGE_CODES: [&str; 100] = [
    "af", "am", "ar", "as", "az", "ba", "be", "bg", "bn", "bo", "br", "bs", "ca", "cs", "cy", "da", "de", "el", "en",
    "es", "et", "eu", "fa", "fi", "fo", "fr", "gl", "gu", "ha", "haw", "he", "hi", "hr", "ht", "hu", "hy", "id", "is",
    "it", "ja", "jw", "ka", "kk", "km", "kn", "ko", "la", "lb", "ln", "lo", "lt", "lv", "mg", "mi", "mk", "ml", "mn",
    "mr", "ms", "mt", "my", "ne", "nl", "nn", "no", "oc", "pa", "pl", "ps", "pt", "ro", "ru", "sa", "sd", "si", "sk",
    "sl", "sn", "so", "sq", "sr", "su", "sv", "sw", "ta", "te", "tg", "th", "tk", "tl", "tr", "tt", "uk", "ur", "uz",
    "vi", "yi", "yo", "zh", "yue",
];

/// Python `string.punctuation`.
const PUNCTUATION: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/// The HF tokenizer operations faster-whisper uses (a seam for tests).
pub trait HfTokenizer {
    fn token_to_id(&self, token: &str) -> Option<u32>;
    fn id_to_token(&self, id: u32) -> Option<String>;
    /// `encode(text, add_special_tokens=False).ids`
    fn encode(&self, text: &str) -> Vec<u32>;
    /// `decode(ids)` with special tokens skipped.
    fn decode(&self, ids: &[u32]) -> String;
}

/// `tokenizer.json` loaded with the `tokenizers` crate.
pub struct JsonTokenizer(tokenizers::Tokenizer);

impl JsonTokenizer {
    pub fn from_file(path: &Path) -> Result<Self> {
        tokenizers::Tokenizer::from_file(path)
            .map(Self)
            .map_err(|e| EngineError::runtime(e.to_string()))
    }
}

impl HfTokenizer for JsonTokenizer {
    fn token_to_id(&self, token: &str) -> Option<u32> {
        self.0.token_to_id(token)
    }

    fn id_to_token(&self, id: u32) -> Option<String> {
        self.0.id_to_token(id)
    }

    fn encode(&self, text: &str) -> Vec<u32> {
        self.0.encode(text, false).map(|e| e.get_ids().to_vec()).unwrap_or_default()
    }

    fn decode(&self, ids: &[u32]) -> String {
        self.0.decode(ids, true).unwrap_or_default()
    }
}

/// The faster-whisper tokenizer wrapper.
pub struct Tokenizer<'a> {
    pub hf: &'a dyn HfTokenizer,
    pub task: Option<u32>,
    pub language: Option<u32>,
    pub language_code: String,
    pub sot: u32,
    pub eot: u32,
    pub transcribe: u32,
    pub translate: u32,
    pub sot_lm: u32,
    pub sot_prev: u32,
    pub no_timestamps: u32,
    pub no_speech: u32,
}

fn id(hf: &dyn HfTokenizer, token: &str) -> u32 {
    hf.token_to_id(token).unwrap_or(u32::MAX)
}

impl<'a> Tokenizer<'a> {
    /// `Tokenizer(hf, multilingual, task="transcribe", language)`.
    pub fn new(hf: &'a dyn HfTokenizer, multilingual: bool, task: &str, language: Option<&str>) -> Result<Self> {
        let (task_id, lang_id, code) = if multilingual {
            if !TASKS.contains(&task) {
                return Err(EngineError::value(format!(
                    "'{task}' is not a valid task (accepted tasks: {})",
                    TASKS.join(", ")
                )));
            }
            let lang = language.unwrap_or("None");
            if language.is_none() || !LANGUAGE_CODES.contains(&lang) {
                return Err(EngineError::value(format!(
                    "'{lang}' is not a valid language code (accepted language codes: {})",
                    LANGUAGE_CODES.join(", ")
                )));
            }
            (hf.token_to_id(&format!("<|{task}|>")), hf.token_to_id(&format!("<|{lang}|>")), lang.to_string())
        } else {
            (None, None, "en".to_string())
        };
        let no_speech = hf
            .token_to_id("<|nospeech|>")
            .filter(|&i| i != 0)
            .or_else(|| hf.token_to_id("<|nocaptions|>"))
            .unwrap_or(u32::MAX);
        Ok(Self {
            hf,
            task: task_id,
            language: lang_id,
            language_code: code,
            sot: id(hf, "<|startoftranscript|>"),
            eot: id(hf, "<|endoftext|>"),
            transcribe: id(hf, "<|transcribe|>"),
            translate: id(hf, "<|translate|>"),
            sot_lm: id(hf, "<|startoflm|>"),
            sot_prev: id(hf, "<|startofprev|>"),
            no_timestamps: id(hf, "<|notimestamps|>"),
            no_speech,
        })
    }

    pub fn timestamp_begin(&self) -> u32 {
        self.no_timestamps + 1
    }

    pub fn sot_sequence(&self) -> Vec<u32> {
        let mut seq = vec![self.sot];
        seq.extend(self.language);
        seq.extend(self.task);
        seq
    }

    pub fn encode(&self, text: &str) -> Vec<u32> {
        self.hf.encode(text)
    }

    /// `decode(tokens)`: text tokens only (`< eot`).
    pub fn decode(&self, tokens: &[u32]) -> String {
        let text: Vec<u32> = tokens.iter().copied().filter(|&t| t < self.eot).collect();
        self.hf.decode(&text)
    }

    pub fn decode_with_timestamps(&self, tokens: &[u32]) -> String {
        let tb = self.timestamp_begin();
        let mut out = String::new();
        let mut run: Vec<u32> = Vec::new();
        for &t in tokens {
            if t >= tb {
                out.push_str(&self.hf.decode(&run));
                run.clear();
                out.push_str(&format!("<|{:.2}|>", (t - tb) as f64 * 0.02));
            } else {
                run.push(t);
            }
        }
        out.push_str(&self.hf.decode(&run));
        out
    }

    /// `non_speech_tokens` (sorted).
    pub fn non_speech_tokens(&self) -> Vec<u32> {
        let mut symbols: Vec<String> = "\"#()*+/:;<=>@[\\]^_`{|}~「」『』".chars().map(|c| c.to_string()).collect();
        symbols.extend(
            "<< >> <<< >>> -- --- -( -[ (' (\" (( )) ((( ))) [[ ]] {{ }} ♪♪ ♪♪♪"
                .split_whitespace()
                .map(str::to_string),
        );
        let misc: Vec<String> = "♩♪♫♬♭♮♯".chars().map(|c| c.to_string()).collect();
        let mut result = BTreeSet::new();
        if let Some(&t) = self.encode(" -").first() {
            result.insert(t);
        }
        if let Some(&t) = self.encode(" '").first() {
            result.insert(t);
        }
        for (symbol, is_misc) in symbols.iter().map(|s| (s, false)).chain(misc.iter().map(|s| (s, true))) {
            for tokens in [self.encode(symbol), self.encode(&format!(" {symbol}"))] {
                if (tokens.len() == 1 || is_misc) && !tokens.is_empty() {
                    result.insert(tokens[0]);
                }
            }
        }
        result.into_iter().collect()
    }

    /// `get_suppressed_tokens(tokenizer, [-1])`.
    pub fn suppressed_tokens(&self) -> Vec<u32> {
        let mut set: BTreeSet<u32> = self.non_speech_tokens().into_iter().collect();
        set.extend([self.transcribe, self.translate, self.sot, self.sot_prev, self.sot_lm, self.no_speech]);
        set.into_iter().collect()
    }

    pub fn split_to_word_tokens(&self, tokens: &[u32]) -> (Vec<String>, Vec<Vec<u32>>) {
        if ["zh", "ja", "th", "lo", "my", "yue"].contains(&self.language_code.as_str()) {
            self.split_tokens_on_unicode(tokens)
        } else {
            self.split_tokens_on_spaces(tokens)
        }
    }

    pub fn split_tokens_on_unicode(&self, tokens: &[u32]) -> (Vec<String>, Vec<Vec<u32>>) {
        let full: Vec<char> = self.decode_with_timestamps(tokens).chars().collect();
        let mut words = Vec::new();
        let mut word_tokens = Vec::new();
        let mut current: Vec<u32> = Vec::new();
        let mut offset = 0usize;
        for &t in tokens {
            current.push(t);
            let decoded = self.decode_with_timestamps(&current);
            let index = decoded.chars().position(|c| c == '\u{FFFD}').map(|i| i + offset);
            let complete = match index {
                None => true,
                Some(i) => i < full.len() && full[i] == '\u{FFFD}',
            };
            if complete {
                offset += decoded.chars().count();
                words.push(decoded);
                word_tokens.push(std::mem::take(&mut current));
            }
        }
        (words, word_tokens)
    }

    pub fn split_tokens_on_spaces(&self, tokens: &[u32]) -> (Vec<String>, Vec<Vec<u32>>) {
        let (subwords, subword_tokens) = self.split_tokens_on_unicode(tokens);
        let mut words: Vec<String> = Vec::new();
        let mut word_tokens: Vec<Vec<u32>> = Vec::new();
        for (sub, toks) in subwords.into_iter().zip(subword_tokens) {
            let special = toks[0] >= self.eot;
            let with_space = sub.starts_with(' ');
            let punctuation = PUNCTUATION.contains(se_core::pyfmt::strip(&sub));
            if special || with_space || punctuation || words.is_empty() {
                words.push(sub);
                word_tokens.push(toks);
            } else {
                words.last_mut().unwrap().push_str(&sub);
                word_tokens.last_mut().unwrap().extend(toks);
            }
        }
        (words, word_tokens)
    }
}
