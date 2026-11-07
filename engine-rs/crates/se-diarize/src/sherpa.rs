//! `SherpaDiarizer` (spec diarize.md §9): sherpa-onnx offline speaker diarization through the C API
//! ([`crate::ffi`]), followed by the post-processing of [`crate::postprocess`].
//!
//! Test seams (the Python tests monkeypatch the same points): [`DiarizationEngine`] (sherpa's
//! `OfflineSpeakerDiarization`), [`Embedder`] (`SpeakerEmbeddingExtractor`), the WAV reader, and
//! [`DiarizeHooks`] (merge / verify) in [`run_diarization`].

use std::any::Any;
use std::cell::RefCell;
use std::ffi::{CString, c_void};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use se_core::{EngineError, ErrorKind, ProgressFn, Result, Word};

use crate::assign::py_min;
use crate::ffi::{
    DiarizationConfig, DiarizationSegment, EmbeddingExtractorConfig, FastClusteringConfig, PyannoteModelConfig,
    SegmentationModelConfig, SherpaApi,
};
use crate::postprocess::{
    EmbedFn, SliceCache, compact_speech_samples, merge_similar_speakers, normalize, round3, unmap_compacted_segments,
    verify_short_turns,
};
use crate::{CLUSTER_THRESHOLD, Diarizer, MERGE_SIMILARITY, MODEL_ID, SAMPLE_RATE, SpeakerSegment};

/// A raw sherpa segment (`float start, float end, int32 speaker`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawSegment {
    pub start: f32,
    pub end: f32,
    pub speaker: i32,
}

/// sherpa's `OfflineSpeakerDiarization`: `process(samples, callback).sort_by_start_time()`.
pub trait DiarizationEngine {
    /// Segments sorted by start time; `callback(processed_chunks, total_chunks)` reports progress.
    fn process(&mut self, samples: &[f32], callback: &mut dyn FnMut(i32, i32)) -> Result<Vec<RawSegment>>;
}

/// sherpa's `SpeakerEmbeddingExtractor` (created lazily on the first call).
pub trait Embedder {
    fn embed(&mut self, samples: &[f32]) -> Result<Vec<f64>>;
}

/// Builds an engine: `None` = automatic speaker count, `Some(n)` = exactly `n` clusters.
pub type EngineBuilder = Box<dyn FnMut(Option<u32>) -> Result<Box<dyn DiarizationEngine>>>;

/// Reads the WAV given to `diarize`.
pub type WavReader = fn(&Path) -> Result<Vec<f32>>;

/// Replacements for the merge and verify passes of [`run_diarization`] (tests only; production uses
/// [`merge_similar_speakers`] and [`verify_short_turns`]).
pub struct DiarizeHooks<'h> {
    pub merge: &'h mut dyn FnMut(&[SpeakerSegment], EmbedFn<'_>, &[f32], &mut SliceCache) -> Vec<SpeakerSegment>,
    pub verify:
        &'h mut dyn FnMut(&[SpeakerSegment], &[Word], &[f32], EmbedFn<'_>, &mut SliceCache) -> Vec<SpeakerSegment>,
}

/// Strictly increasing progress reports (`report` in `diarize.py`).
struct Progress<'a> {
    last: f64,
    on_progress: Option<ProgressFn<'a>>,
}

impl Progress<'_> {
    fn report(&mut self, fraction: f64) {
        if let Some(f) = self.on_progress.as_mut()
            && fraction > self.last
        {
            self.last = fraction;
            f(fraction);
        }
    }
}

/// The body of `SherpaDiarizer.diarize` after the WAV is read and the engine chosen: compaction, sherpa,
/// unmapping, auto-mode merge, noise filter, progress `0.9`, acoustic review, progress `1.0`.
///
/// `explicit` is `num_speakers` truthy (`Some(n)` with `n > 0`): no merge, no noise filter.
pub fn run_diarization(
    engine: &mut dyn DiarizationEngine,
    samples: &[f32],
    num_speakers: Option<u32>,
    on_progress: Option<ProgressFn<'_>>,
    words: Option<&[Word]>,
    embed: EmbedFn<'_>,
    hooks: Option<DiarizeHooks<'_>>,
) -> Result<Vec<SpeakerSegment>> {
    let explicit = matches!(num_speakers, Some(n) if n > 0);
    let mut progress = Progress { last: -1.0, on_progress };

    let (compact, spans) = compact_speech_samples(samples, words);
    let result = {
        let mut callback = |processed: i32, total: i32| {
            if total > 0 {
                progress.report(0.85 * py_min(processed as f64 / total as f64, 1.0));
            }
        };
        engine.process(&compact, &mut callback)?
    };
    drop(compact);
    let mut raw: Vec<SpeakerSegment> = result
        .iter()
        .map(|s| SpeakerSegment { start: round3(s.start as f64), end: round3(s.end as f64), speaker: s.speaker as i64 })
        .collect();
    if let Some(spans) = &spans {
        raw = unmap_compacted_segments(&raw, spans);
    }

    let mut slice_cache = SliceCache::new();
    let (mut merge_hook, mut verify_hook) = match hooks {
        Some(h) => (Some(h.merge), Some(h.verify)),
        None => (None, None),
    };
    if !explicit {
        raw = match merge_hook.as_mut() {
            Some(merge) => merge(&raw, &mut *embed, samples, &mut slice_cache),
            None => merge_similar_speakers(&raw, &mut *embed, samples, MERGE_SIMILARITY, Some(&mut slice_cache)),
        };
    }
    raw = normalize(&raw, !explicit, words);
    progress.report(0.9);
    if let Some(words) = words.filter(|w| !w.is_empty()) {
        let distinct: std::collections::HashSet<i64> = raw.iter().map(|s| s.speaker).collect();
        if distinct.len() >= 2 {
            raw = match verify_hook.as_mut() {
                Some(verify) => verify(&raw, words, samples, &mut *embed, &mut slice_cache),
                None => verify_short_turns(&raw, words, samples, &mut *embed, Some(&mut slice_cache)),
            };
        }
    }
    progress.report(1.0);
    Ok(raw)
}

/// sherpa-onnx offline speaker diarization; model files are read from the HF cache
/// (`speaker-diarization` catalog entry). The auto-mode engine is built lazily once.
pub struct SherpaDiarizer {
    builder: EngineBuilder,
    default: Option<Box<dyn DiarizationEngine>>,
    embedder: Box<dyn Embedder>,
    reader: WavReader,
}

/// The segmentation and embedding model files of the `speaker-diarization` entry in `cache_dir`
/// (`None`: the default HF cache). Either missing → localized `diarize_not_downloaded`.
pub fn model_files(cache_dir: Option<&Path>) -> Result<(PathBuf, PathBuf)> {
    let info = se_models::catalog::find(MODEL_ID).expect("speaker-diarization is in the catalog");
    let (embedding_repo, embedding_files) = info.extra[0];
    let lookup = |repo: &str, file: &str| match cache_dir {
        Some(dir) => se_models::cache::try_to_load_from_cache_in(dir, repo, file),
        None => se_models::cache::try_to_load_from_cache(repo, file),
    };
    let not_downloaded = || EngineError::model(se_core::t!("diarize_not_downloaded"));
    let segmentation = lookup(info.repo, info.files[0]).ok_or_else(not_downloaded)?;
    let embedding = lookup(embedding_repo, embedding_files[0]).ok_or_else(not_downloaded)?;
    Ok((segmentation, embedding))
}

/// `os.cpu_count()`: logical processors of all groups on Windows (`GetActiveProcessorCount`).
fn cpu_count() -> usize {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetActiveProcessorCount(group: u16) -> u32;
        }
        const ALL_PROCESSOR_GROUPS: u16 = 0xffff;
        // SAFETY: plain Win32 query without pointers.
        let n = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) } as usize;
        if n > 0 {
            return n;
        }
    }
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Owned C strings of the model configs (sherpa copies them during the create calls).
struct ModelStrings {
    segmentation: CString,
    embedding: CString,
    provider: CString,
}

fn c_path(path: &Path) -> CString {
    // sherpa-onnx takes UTF-8 paths (the Python binding passes `str` as UTF-8).
    CString::new(path.to_string_lossy().into_owned().into_bytes()).unwrap_or_default()
}

impl ModelStrings {
    fn embedding_config(&self, threads: i32) -> EmbeddingExtractorConfig {
        EmbeddingExtractorConfig {
            model: self.embedding.as_ptr(),
            num_threads: threads,
            debug: 0,
            provider: self.provider.as_ptr(),
        }
    }

    fn diarization_config(&self, threads: i32, num_speakers: Option<u32>, threshold: f64) -> DiarizationConfig {
        DiarizationConfig {
            segmentation: SegmentationModelConfig {
                pyannote: PyannoteModelConfig { model: self.segmentation.as_ptr(), window_shift_ratio: 0.1 },
                num_threads: threads,
                debug: 0,
                provider: self.provider.as_ptr(),
            },
            embedding: self.embedding_config(threads),
            clustering: FastClusteringConfig {
                num_clusters: match num_speakers {
                    Some(n) if n > 0 => i32::try_from(n).unwrap_or(i32::MAX),
                    _ => -1,
                },
                threshold: threshold as f32,
                compute_confidence: 0,
            },
            min_duration_on: 0.3,
            min_duration_off: 0.5,
        }
    }
}

/// A live `SherpaOnnxOfflineSpeakerDiarization`.
struct SherpaEngine {
    api: Arc<SherpaApi>,
    handle: *const c_void,
}

impl Drop for SherpaEngine {
    fn drop(&mut self) {
        // SAFETY: created by `create_diarization`, destroyed exactly once.
        unsafe { (self.api.destroy_diarization)(self.handle) }
    }
}

struct CallbackState<'a> {
    callback: &'a mut dyn FnMut(i32, i32),
    panic: Option<Box<dyn Any + Send>>,
}

unsafe extern "C" fn progress_trampoline(done: i32, total: i32, arg: *mut c_void) -> i32 {
    // SAFETY: `arg` is the `CallbackState` passed to `process_with_callback`, alive for the whole call.
    let state = unsafe { &mut *(arg as *mut CallbackState<'_>) };
    if state.panic.is_none()
        && let Err(payload) = catch_unwind(AssertUnwindSafe(|| (state.callback)(done, total)))
    {
        state.panic = Some(payload);
    }
    0 // keep going (2.0.1 ignores the value)
}

impl DiarizationEngine for SherpaEngine {
    fn process(&mut self, samples: &[f32], callback: &mut dyn FnMut(i32, i32)) -> Result<Vec<RawSegment>> {
        let n = i32::try_from(samples.len())
            .map_err(|_| EngineError::runtime("audio is too long for sherpa-onnx diarization (int32 samples)"))?;
        let mut state = CallbackState { callback, panic: None };
        // SAFETY: valid handle; `samples` outlives the call; `state` outlives the call.
        let result = unsafe {
            (self.api.process_with_callback)(
                self.handle,
                samples.as_ptr(),
                n,
                Some(progress_trampoline),
                &mut state as *mut CallbackState<'_> as *mut c_void,
            )
        };
        if let Some(payload) = state.panic.take() {
            if !result.is_null() {
                // SAFETY: result from process_with_callback.
                unsafe { (self.api.destroy_result)(result) };
            }
            resume_unwind(payload);
        }
        if result.is_null() {
            return Err(EngineError::runtime("sherpa-onnx speaker diarization returned no result"));
        }
        // SAFETY: `result` is a valid result; the segment array holds `count` entries.
        unsafe {
            let count = (self.api.result_num_segments)(result).max(0) as usize;
            let mut out = Vec::with_capacity(count);
            if count > 0 {
                let segs = (self.api.result_sort_by_start_time)(result);
                if !segs.is_null() {
                    let slice: &[DiarizationSegment] = std::slice::from_raw_parts(segs, count);
                    out.extend(slice.iter().map(|s| RawSegment { start: s.start, end: s.end, speaker: s.speaker }));
                    (self.api.destroy_segment)(segs);
                }
            }
            (self.api.destroy_result)(result);
            Ok(out)
        }
    }
}

/// The lazily created `SherpaOnnxSpeakerEmbeddingExtractor`.
struct SherpaEmbedder {
    api: Arc<SherpaApi>,
    strings: Arc<ModelStrings>,
    threads: i32,
    extractor: *const c_void,
}

impl Drop for SherpaEmbedder {
    fn drop(&mut self) {
        if !self.extractor.is_null() {
            // SAFETY: created by `create_extractor`, destroyed once.
            unsafe { (self.api.destroy_extractor)(self.extractor) }
        }
    }
}

impl Embedder for SherpaEmbedder {
    fn embed(&mut self, samples: &[f32]) -> Result<Vec<f64>> {
        let api = &self.api;
        if self.extractor.is_null() {
            let config = self.strings.embedding_config(self.threads);
            // SAFETY: config strings are alive (owned by `self.strings`).
            let ex = unsafe { (api.create_extractor)(&config) };
            if ex.is_null() {
                return Err(EngineError::model(se_core::t!("diarize_load_failed")));
            }
            self.extractor = ex;
        }
        let ex = self.extractor;
        let n = i32::try_from(samples.len()).unwrap_or(i32::MAX);
        // SAFETY: valid extractor; stream and embedding are freed below.
        unsafe {
            let stream = (api.extractor_create_stream)(ex);
            if stream.is_null() {
                return Ok(Vec::new());
            }
            (api.stream_accept_waveform)(stream, SAMPLE_RATE as i32, samples.as_ptr(), n);
            (api.stream_input_finished)(stream);
            let mut out = Vec::new();
            if (api.extractor_is_ready)(ex, stream) != 0 {
                let dim = (api.extractor_dim)(ex).max(0) as usize;
                let v = (api.extractor_compute)(ex, stream);
                if !v.is_null() {
                    out.extend(std::slice::from_raw_parts(v, dim).iter().map(|&x| x as f64));
                    (api.extractor_destroy_embedding)(v);
                }
            }
            (api.destroy_stream)(stream);
            Ok(out)
        }
    }
}

impl SherpaDiarizer {
    /// `SherpaDiarizer(num_threads=None, threshold=0.5)`: `num_threads` defaults to the logical CPU count.
    /// Errors: `diarize_not_downloaded` when the model files are not cached (checked first), then
    /// `diarize_load_failed` when the C API library cannot be loaded (detail on stderr).
    pub fn new(num_threads: Option<usize>, threshold: f64) -> Result<Self> {
        let (segmentation, embedding) = model_files(None)?;
        let api = Arc::new(SherpaApi::load()?);
        Ok(Self::with_api(api, &segmentation, &embedding, num_threads, threshold))
    }

    /// A diarizer on an already loaded C API and explicit model files.
    pub fn with_api(
        api: Arc<SherpaApi>,
        segmentation: &Path,
        embedding: &Path,
        num_threads: Option<usize>,
        threshold: f64,
    ) -> Self {
        let threads = num_threads.filter(|&n| n > 0).unwrap_or_else(|| cpu_count().max(1));
        let threads = i32::try_from(threads).unwrap_or(i32::MAX);
        let strings = Arc::new(ModelStrings {
            segmentation: c_path(segmentation),
            embedding: c_path(embedding),
            provider: CString::new("cpu").expect("no NUL"),
        });
        let builder_api = Arc::clone(&api);
        let builder_strings = Arc::clone(&strings);
        let builder: EngineBuilder = Box::new(move |num_speakers| {
            let config = builder_strings.diarization_config(threads, num_speakers, threshold);
            // SAFETY: config strings are alive for the call (sherpa copies them).
            let handle = unsafe { (builder_api.create_diarization)(&config) };
            if handle.is_null() {
                // Python: `config.validate()` is false → ModelError(diarize_load_failed).
                return Err(EngineError::model(se_core::t!("diarize_load_failed")));
            }
            Ok(Box::new(SherpaEngine { api: Arc::clone(&builder_api), handle }) as Box<dyn DiarizationEngine>)
        });
        let embedder = Box::new(SherpaEmbedder { api, strings, threads, extractor: std::ptr::null() });
        Self::with_parts(builder, embedder, read_wav)
    }

    /// `_embed(samples)`: one speaker embedding (creates the extractor on first use).
    pub fn embed(&mut self, samples: &[f32]) -> Result<Vec<f64>> {
        self.embedder.embed(samples)
    }

    /// A diarizer from its parts (test seam: fake engines, embedders and WAV readers).
    pub fn with_parts(builder: EngineBuilder, embedder: Box<dyn Embedder>, reader: WavReader) -> Self {
        Self { builder, default: None, embedder, reader }
    }
}

impl Diarizer for SherpaDiarizer {
    fn diarize(
        &mut self,
        audio: &Path,
        num_speakers: Option<u32>,
        on_progress: Option<ProgressFn<'_>>,
        words: Option<&[Word]>,
    ) -> Result<Vec<SpeakerSegment>> {
        let samples = (self.reader)(audio)?;
        let Self { builder, default, embedder, .. } = self;
        let mut fresh;
        let engine: &mut dyn DiarizationEngine = match num_speakers {
            Some(n) if n > 0 => {
                fresh = builder(Some(n))?;
                fresh.as_mut()
            }
            _ => {
                if default.is_none() {
                    *default = Some(builder(None)?);
                }
                default.as_mut().expect("built above").as_mut()
            }
        };
        // An extractor error (first embedding call) ends the run with that error, as the Python exception
        // would: the remaining post-processing still runs on empty vectors, but no further progress is
        // reported, so the observable events (stages, then the error) are the same as Python's.
        let embed_error: RefCell<Option<EngineError>> = RefCell::new(None);
        let mut embed = |chunk: &[f32]| -> Vec<f64> {
            if embed_error.borrow().is_some() {
                return Vec::new();
            }
            match embedder.embed(chunk) {
                Ok(v) => v,
                Err(e) => {
                    *embed_error.borrow_mut() = Some(e);
                    Vec::new()
                }
            }
        };
        let mut guarded;
        let on_progress: Option<ProgressFn<'_>> = match on_progress {
            Some(f) => {
                guarded = |fraction: f64| {
                    if embed_error.borrow().is_none() {
                        f(fraction);
                    }
                };
                Some(&mut guarded)
            }
            None => None,
        };
        let result = run_diarization(engine, &samples, num_speakers, on_progress, words, &mut embed, None);
        if let Some(e) = embed_error.into_inner() {
            return Err(e);
        }
        result
    }
}

/// `create_diarizer()` = `SherpaDiarizer::new(None, 0.5)`.
pub fn create_diarizer() -> Result<Box<dyn Diarizer>> {
    Ok(Box::new(SherpaDiarizer::new(None, CLUSTER_THRESHOLD)?))
}

/// A Python `wave.Error` / `EOFError` / `RuntimeError` (its `str()` is the message).
fn wave_error(message: impl Into<String>) -> EngineError {
    EngineError::new(ErrorKind::Other, message)
}

/// Python `wave._Chunk` for the outer RIFF chunk: reads are bounded by its size, an odd-sized chunk's pad
/// byte is consumed when a read reaches its end, and seeks past its end raise (an empty `RuntimeError`).
struct RiffChunk<'f> {
    file: &'f mut File,
    size: u64,
    size_read: u64,
}

/// File offset of the RIFF chunk data (right after `RIFF` + size).
const RIFF_DATA: u64 = 8;

impl RiffChunk<'_> {
    /// `file.read(n)` at the chunk's current position (the file position Python would be at).
    fn raw_read(&mut self, n: u64) -> Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(RIFF_DATA + self.size_read))?;
        let mut buf = Vec::with_capacity(n.min(1 << 30) as usize);
        (&mut *self.file).take(n).read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// `_Chunk.read(n)`.
    fn read(&mut self, n: u64) -> Result<Vec<u8>> {
        if self.size_read >= self.size {
            return Ok(Vec::new());
        }
        let n = n.min(self.size - self.size_read);
        let data = self.raw_read(n)?;
        self.size_read += data.len() as u64;
        if self.size_read == self.size && self.size & 1 == 1 {
            let dummy = self.raw_read(1)?;
            self.size_read += dummy.len() as u64;
        }
        Ok(data)
    }

    /// `_Chunk.seek(n, 1)`.
    fn seek_relative(&mut self, n: u64) -> Result<()> {
        let pos = self.size_read + n;
        if pos > self.size {
            return Err(wave_error(""));
        }
        self.size_read = pos;
        Ok(())
    }
}

/// Python `wave._Chunk` nested in the RIFF chunk (every read goes through the RIFF chunk).
struct SubChunk {
    size: u64,
    size_read: u64,
}

impl SubChunk {
    /// `_Chunk(riff)`: `None` at the end of the chunks (EOFError: short name or size).
    fn open(riff: &mut RiffChunk<'_>) -> Result<Option<([u8; 4], SubChunk)>> {
        let name = riff.read(4)?;
        if name.len() < 4 {
            return Ok(None);
        }
        let size = riff.read(4)?;
        if size.len() < 4 {
            return Ok(None);
        }
        let size = u32::from_le_bytes(size[..4].try_into().expect("4 bytes")) as u64;
        Ok(Some((name[..4].try_into().expect("4 bytes"), SubChunk { size, size_read: 0 })))
    }

    /// `_Chunk.read(n)`.
    fn read(&mut self, riff: &mut RiffChunk<'_>, n: u64) -> Result<Vec<u8>> {
        if self.size_read >= self.size {
            return Ok(Vec::new());
        }
        let n = n.min(self.size - self.size_read);
        let data = riff.read(n)?;
        self.size_read += data.len() as u64;
        if self.size_read == self.size && self.size & 1 == 1 {
            let dummy = riff.read(1)?;
            self.size_read += dummy.len() as u64;
        }
        Ok(data)
    }

    /// `_Chunk.skip()` on a seekable file: `riff.seek(rest + pad, 1)` (past the RIFF end → `RuntimeError`).
    fn skip(&mut self, riff: &mut RiffChunk<'_>) -> Result<()> {
        // n = chunksize - size_read (+1 for an odd size); never negative (size_read exceeds the size only
        // by an already consumed pad byte, which makes n 0).
        let n = (self.size + (self.size & 1)).saturating_sub(self.size_read);
        riff.seek_relative(n)?;
        self.size_read += n;
        Ok(())
    }
}

/// `read_wav(path)`: a 16 kHz mono 16-bit WAV as `f32` in `[-1, 1)`; other formats → localized `ModelError`
/// `diarize_wav_format`. Parsing follows Python 3.11 `wave` (PCM only: other format tags give
/// `unknown format: N`, a missing chunk `fmt chunk and/or data chunk missing`, a truncated header an empty
/// `EOFError`, a chunk running past the RIFF end an empty `RuntimeError`, ...).
pub fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut file = File::open(path).map_err(|e| EngineError::os(&e, Some(path)))?;

    // `_Chunk(file)`: a short header is an EOFError (empty message), raised before the RIFF id check.
    let mut header = Vec::new();
    (&mut file).take(8).read_to_end(&mut header)?;
    if header.len() < 8 {
        return Err(wave_error(""));
    }
    if &header[..4] != b"RIFF" {
        return Err(wave_error("file does not start with RIFF id"));
    }
    let riff_size = u32::from_le_bytes(header[4..8].try_into().expect("4 bytes")) as u64;
    let mut riff = RiffChunk { file: &mut file, size: riff_size, size_read: 0 };
    if riff.read(4)? != b"WAVE" {
        return Err(wave_error("not a WAVE file"));
    }

    let mut format: Option<(u32, u16, u16)> = None; // (framerate, channels, sampwidth)
    let mut data: Option<SubChunk> = None;
    while let Some((name, mut chunk)) = SubChunk::open(&mut riff)? {
        if &name == b"fmt " {
            let fmt = chunk.read(&mut riff, 14)?;
            if fmt.len() < 14 {
                return Err(wave_error(""));
            }
            let tag = u16::from_le_bytes([fmt[0], fmt[1]]);
            let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            let framerate = u32::from_le_bytes(fmt[4..8].try_into().expect("4 bytes"));
            if tag != 1 {
                return Err(wave_error(format!("unknown format: {tag}")));
            }
            let width = chunk.read(&mut riff, 2)?;
            if width.len() < 2 {
                return Err(wave_error(""));
            }
            let sampwidth = (u16::from_le_bytes([width[0], width[1]]) as u32).div_ceil(8) as u16;
            if sampwidth == 0 {
                return Err(wave_error("bad sample width"));
            }
            if channels == 0 {
                return Err(wave_error("bad # of channels"));
            }
            format = Some((framerate, channels, sampwidth));
        } else if &name == b"data" {
            if format.is_none() {
                return Err(wave_error("data chunk before fmt chunk"));
            }
            data = Some(chunk);
            break;
        }
        chunk.skip(&mut riff)?;
    }
    let (Some((framerate, channels, sampwidth)), Some(mut data)) = (format, data) else {
        return Err(wave_error("fmt chunk and/or data chunk missing"));
    };
    if framerate != SAMPLE_RATE || channels != 1 || sampwidth != 2 {
        return Err(EngineError::model(se_core::t!("diarize_wav_format")));
    }
    // readframes(nframes): nframes = chunksize // framesize (framesize = 2 here).
    let nframes = data.size / 2;
    let bytes = if nframes == 0 { Vec::new() } else { data.read(&mut riff, nframes * 2)? };
    if bytes.len() % 2 != 0 {
        return Err(EngineError::value("buffer size must be a multiple of element size"));
    }
    Ok(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect())
}
