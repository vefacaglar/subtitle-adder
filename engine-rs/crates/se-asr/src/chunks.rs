//! Long-audio chunking for Whisper (>180 s) and the ASR checkpoint
//! (`<temp>/SubtitleAdder/transcriptions/asr-<hash>.json`; spec transcribe.md §2.4-§2.9).

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use se_core::json::{self, Map, Value};
use se_core::{Result, Word};
use sha2::{Digest, Sha256};

use crate::wave::WaveReader;

/// `TRANSCRIBE_CHUNK_SECONDS`, `MAX_CHUNK_EXTENSION_SECONDS`, `CHUNK_SEARCH_TAIL_SECONDS` (tests change them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChunkConfig {
    pub chunk_seconds: f64,
    pub max_extension_seconds: f64,
    pub search_tail_seconds: f64,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self { chunk_seconds: 180.0, max_extension_seconds: 120.0, search_tail_seconds: 45.0 }
    }
}

/// A chunk of a long WAV: index, start offset in seconds and its samples.
#[derive(Clone, Debug, PartialEq)]
pub struct WavChunk {
    pub index: usize,
    pub offset_seconds: f64,
    pub samples: Vec<i16>,
}

impl WavChunk {
    /// `chunk_i16.astype(np.float32) / 32768.0`.
    pub fn samples_f32(&self) -> Vec<f32> {
        self.samples.iter().map(|&s| s as f32 / 32768.0f32).collect()
    }
}

/// numpy `median` of a non-empty f64 slice (mean of the two middle values for an even length).
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 }
}

/// `_find_silence_pause_cut(buf, rate, tail_frames) -> (cut, found)`.
pub fn find_silence_pause_cut(buf: &[i16], rate: u32, tail_frames: usize) -> (usize, bool) {
    let rate_f = rate as f64;
    let pause = ((0.35 * rate_f) as usize).max(1);
    let step = ((0.05 * rate_f) as usize).max(1);
    if buf.len() <= pause * 2 {
        return (buf.len(), true);
    }
    let search_start = buf.len().saturating_sub(tail_frames).max(pause);
    let tail = &buf[search_start.min(buf.len())..];
    if tail.len() < pause {
        return (buf.len(), true);
    }
    let mut csum = Vec::with_capacity(tail.len() + 1);
    csum.push(0.0f64);
    let mut acc = 0.0f64;
    for &s in tail {
        let x = s as f32 / 32768.0f32;
        let sq = x * x;
        acc += sq as f64;
        csum.push(acc);
    }
    let last_start = tail.len() - pause;
    let starts: Vec<usize> = (0..=last_start).step_by(step).collect();
    if starts.is_empty() {
        return (buf.len(), true);
    }
    let means: Vec<f64> = starts.iter().map(|&s| (csum[s + pause] - csum[s]) / pause as f64).collect();
    let mut min_idx = 0;
    for (i, &m) in means.iter().enumerate() {
        if m < means[min_idx] {
            min_idx = i;
        }
    }
    let min_rms = means[min_idx].max(0.0).sqrt();
    let median_rms = median(&means).max(0.0).sqrt();
    let clean = min_rms <= 0.008 || (median_rms > 0.0 && min_rms <= median_rms * 0.25);
    (search_start + starts[min_idx] + pause / 2, clean)
}

/// `_can_stream_wav_chunks(audio, min_duration)`: a PCM16 mono WAV longer than `min_duration`
/// (default `config.chunk_seconds`).
pub fn can_stream_wav_chunks(audio: &Path, config: &ChunkConfig, min_duration: Option<f64>) -> bool {
    let threshold = min_duration.unwrap_or(config.chunk_seconds);
    if !audio.is_file() {
        return false;
    }
    let Ok(wav) = WaveReader::open(audio) else {
        return false;
    };
    if wav.nchannels() != 1 || wav.sampwidth() != 2 || wav.framerate() == 0 {
        return false;
    }
    (wav.nframes() as f64 / wav.framerate() as f64) > threshold
}

/// Lazy `_iter_wav_chunks`: chunks cut at silence pauses, never inside speech.
pub struct WavChunks {
    wav: WaveReader,
    rate: u32,
    total: u64,
    total_duration: f64,
    target: usize,
    max_extra: usize,
    step_extend: u64,
    tail_frames: usize,
    cursor: u64,
    index: usize,
    carry: Vec<i16>,
    done: bool,
}

impl WavChunks {
    pub fn open(audio: &Path, config: &ChunkConfig) -> Result<Self> {
        let wav = WaveReader::open(audio)?;
        let rate = wav.framerate();
        let rate_f = rate as f64;
        let total = wav.nframes();
        let total_duration = if rate > 0 { total as f64 / rate_f } else { 0.0 };
        let target = (config.chunk_seconds * rate_f) as usize;
        let max_extra = (config.max_extension_seconds * rate_f) as usize;
        let step_extend = ((30.0 * rate_f) as usize).max(target / 4) as u64;
        let tail_frames = ((config.search_tail_seconds * rate_f) as usize).min((target / 2).max(1));
        Ok(Self {
            wav,
            rate,
            total,
            total_duration,
            target,
            max_extra,
            step_extend,
            tail_frames,
            cursor: 0,
            index: 0,
            carry: Vec::new(),
            done: false,
        })
    }

    /// `total_frames / rate` (0 for a zero rate).
    pub fn total_duration(&self) -> f64 {
        self.total_duration
    }

    fn next_chunk(&mut self) -> Result<Option<WavChunk>> {
        if self.done || !(self.cursor < self.total || !self.carry.is_empty()) {
            return Ok(None);
        }
        let needed = self.target.saturating_sub(self.carry.len());
        let new = if needed > 0 { self.wav.read_i16(needed as u64)? } else { Vec::new() };
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(&new);
        if buf.is_empty() {
            self.done = true;
            return Ok(None);
        }
        let mut extended = 0usize;
        while self.wav.tell() < self.total && extended < self.max_extra {
            let (_, clean) = find_silence_pause_cut(&buf, self.rate, self.tail_frames);
            if clean {
                break;
            }
            let more = self.wav.read_i16(self.step_extend)?;
            if more.is_empty() {
                break;
            }
            extended += more.len();
            buf.extend_from_slice(&more);
        }
        let at_eof = self.wav.tell() >= self.total && buf.len() <= self.target + extended;
        let cut = if at_eof {
            buf.len()
        } else {
            find_silence_pause_cut(&buf, self.rate, self.tail_frames.max(extended)).0
        };
        let carry = buf.split_off(cut.min(buf.len()));
        self.carry = carry;
        let offset_seconds = self.cursor as f64 / self.rate as f64;
        self.cursor += buf.len() as u64;
        let chunk = WavChunk { index: self.index, offset_seconds, samples: buf };
        self.index += 1;
        Ok(Some(chunk))
    }
}

impl Iterator for WavChunks {
    type Item = Result<WavChunk>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_chunk() {
            Ok(Some(c)) => Some(Ok(c)),
            Ok(None) => None,
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

/// `_iter_wav_chunks(audio, chunk_seconds)`: chunks cut at silence pauses, never inside speech (collected).
pub fn iter_wav_chunks(audio: &Path, config: &ChunkConfig) -> Result<Vec<WavChunk>> {
    WavChunks::open(audio, config)?.collect()
}

/// `_audio_fingerprint(audio, size, blocks=8, block_bytes=16384)`.
pub fn audio_fingerprint(audio: &Path, size: u64) -> Result<String> {
    const BLOCKS: i128 = 8;
    const BLOCK_BYTES: i128 = 16_384;
    let mut digest = Sha256::new();
    digest.update(size.to_string().as_bytes());
    let mut file = fs::File::open(audio).map_err(|e| se_core::EngineError::os(&e, Some(audio)))?;
    let mut buf = vec![0u8; BLOCK_BYTES as usize];
    for i in 0..BLOCKS {
        // Python floor division; a negative offset is clamped to 0.
        let pos = ((size as i128 - BLOCK_BYTES) * i).div_euclid(BLOCKS - 1).max(0);
        file.seek(SeekFrom::Start(pos as u64))?;
        let mut filled = 0;
        while filled < buf.len() {
            let n = file.read(&mut buf[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        digest.update(&buf[..filled]);
    }
    Ok(hex::encode(digest.finalize()))
}

/// The default checkpoint folder: `gettempdir()/SubtitleAdder/transcriptions`.
pub fn default_checkpoint_root() -> PathBuf {
    se_core::fsutil::app_temp_dir().join("transcriptions")
}

/// `_transcribe_checkpoint_path(audio, model_size, language)` under `root`
/// (`<temp>/SubtitleAdder/transcriptions` by default); `None` when the WAV cannot be stat'ed or read, or the
/// folder cannot be created.
pub fn checkpoint_path(root: &Path, audio: &Path, model_size: &str, language: Option<&str>) -> Option<PathBuf> {
    let size = fs::metadata(audio).ok()?.len();
    let name = se_core::paths::py_name(&se_core::paths::py_path_str(audio)).to_string();
    let fingerprint = audio_fingerprint(audio, size).ok()?;
    let key = format!("{name}|{fingerprint}|{model_size}|{}", language.unwrap_or(""));
    let digest = hex::encode(Sha256::digest(key.as_bytes()));
    fs::create_dir_all(root).ok()?;
    Some(root.join(format!("asr-{}.json", &digest[..20])))
}

/// Python `float(x)` for a JSON value (numbers, bools and numeric strings).
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::BigInt(s) => s.parse::<f64>().ok().filter(|f| f.is_finite()),
        Value::Str(s) => {
            let t = se_core::pyfmt::strip(s).replace('_', "");
            t.parse::<f64>().ok()
        }
        _ => None,
    }
}

/// Python `int(str)` for a dict key.
fn py_int_key(k: &str) -> Option<i64> {
    se_core::pyfmt::strip(k).replace('_', "").parse::<i64>().ok()
}

fn load_words(list: &Value) -> Option<Vec<Word>> {
    let items: Vec<&Value> = match list {
        Value::Array(a) => a.iter().collect(),
        Value::Str(s) if s.is_empty() => Vec::new(),
        Value::Object(m) if m.is_empty() => Vec::new(),
        _ => return None,
    };
    let mut words = Vec::with_capacity(items.len());
    for w in items {
        let obj = w.as_object()?;
        let start = py_float(obj.get("start")?)?;
        let end = py_float(obj.get("end")?)?;
        let text = obj.get("text")?.py_str();
        words.push(Word::new(start, end, text));
    }
    Some(words)
}

fn load_inner(path: &Path) -> Option<(Option<String>, BTreeMap<i64, Vec<Word>>)> {
    let bytes = fs::read(path).ok()?;
    let text = json::decode_utf8_strict(&bytes).ok()?;
    let raw = json::parse(text).ok()?;
    let obj = raw.as_object()?;
    let language = match obj.get("language") {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let mut chunks = BTreeMap::new();
    match obj.get("chunks") {
        None => {}
        Some(v) if !v.is_truthy() => {}
        Some(Value::Object(m)) => {
            for (k, list) in m.iter() {
                chunks.insert(py_int_key(k)?, load_words(list)?);
            }
        }
        Some(_) => return None,
    }
    Some((language, chunks))
}

/// `_load_transcribe_checkpoint(path) -> (language, {chunk index: words})` (lenient; missing/corrupt → empty,
/// all or nothing). A non-string `language` is read as `None`.
pub fn load_checkpoint(path: Option<&Path>) -> (Option<String>, BTreeMap<i64, Vec<Word>>) {
    let Some(path) = path else {
        return (None, BTreeMap::new());
    };
    if !path.is_file() {
        return (None, BTreeMap::new());
    }
    load_inner(path).unwrap_or((None, BTreeMap::new()))
}

fn tmp_path(path: &Path) -> PathBuf {
    // Path.with_suffix(".json.tmp")
    path.with_extension("json.tmp")
}

/// `_save_transcribe_checkpoint(path, language, chunks)` (atomic; errors ignored).
pub fn save_checkpoint(path: Option<&Path>, language: Option<&str>, chunks: &BTreeMap<i64, Vec<Word>>) {
    let Some(path) = path else {
        return;
    };
    let mut chunk_map = Map::new();
    for (k, words) in chunks {
        let list: Vec<Value> = words
            .iter()
            .map(|w| {
                Value::Object(
                    Map::new()
                        .with("start", w.start.to_value())
                        .with("end", w.end.to_value())
                        .with("text", w.text.as_str()),
                )
            })
            .collect();
        chunk_map.insert(k.to_string(), Value::Array(list));
    }
    let payload = Map::new()
        .with("language", language.map(|s| Value::from(s)).unwrap_or(Value::Null))
        .with("chunks", Value::Object(chunk_map));
    let text = json::dumps(&Value::Object(payload), false);
    let tmp = tmp_path(path);
    if fs::write(&tmp, text.as_bytes()).is_ok() {
        let _ = se_core::fsutil::replace(&tmp, path);
    }
}

/// `_delete_transcribe_checkpoint(path)` (errors ignored). Unused by the engine, like in Python.
pub fn delete_checkpoint(path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    se_core::fsutil::unlink_quiet(path);
    se_core::fsutil::unlink_quiet(&tmp_path(path));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wave::write_pcm16;

    #[test]
    fn silent_buffer_is_a_clean_cut() {
        let buf = vec![0i16; 16000 * 3];
        let (cut, clean) = find_silence_pause_cut(&buf, 16000, 16000 * 2);
        assert!(clean);
        assert_eq!(cut, 16000 + 2800);
    }

    #[test]
    fn short_tail_returns_the_whole_buffer() {
        let buf = vec![0i16; 16000];
        assert_eq!(find_silence_pause_cut(&buf, 16000, 3200), (16000, true));
    }

    #[test]
    fn chunks_of_a_silent_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.wav");
        write_pcm16(&path, 16000, 1, &vec![0i16; 48000]).unwrap();
        let cfg = ChunkConfig { chunk_seconds: 1.0, search_tail_seconds: 0.2, ..Default::default() };
        assert!(can_stream_wav_chunks(&path, &cfg, None));
        let chunks = iter_wav_chunks(&path, &cfg).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(
            chunks.iter().map(|c| (c.index, c.offset_seconds, c.samples.len())).collect::<Vec<_>>(),
            vec![(0, 0.0, 16000), (1, 1.0, 16000), (2, 2.0, 16000)]
        );
    }

    #[test]
    fn checkpoint_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asr-x.json");
        let mut chunks = BTreeMap::new();
        chunks.insert(0, vec![Word::new(0.1, 0.4, "olá")]);
        chunks.insert(1, vec![]);
        save_checkpoint(Some(&path), Some("pt"), &chunks);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            r#"{"language": "pt", "chunks": {"0": [{"start": 0.1, "end": 0.4, "text": "olá"}], "1": []}}"#
        );
        let (lang, loaded) = load_checkpoint(Some(&path));
        assert_eq!(lang.as_deref(), Some("pt"));
        assert_eq!(loaded, chunks);
        fs::write(&path, r#"{"language": "pt", "chunks": {"x": []}}"#).unwrap();
        assert_eq!(load_checkpoint(Some(&path)), (None, BTreeMap::new()));
        fs::write(&path, r#"{"language": null, "chunks": {" 2 ": [{"start": "1.5", "end": 2, "text": 7}]}}"#)
            .unwrap();
        let (lang, loaded) = load_checkpoint(Some(&path));
        assert_eq!(lang, None);
        assert_eq!(loaded[&2], vec![Word::new(1.5, 2.0, "7")]);
    }
}
