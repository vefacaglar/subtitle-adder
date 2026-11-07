//! `pipeline.py`: video → audio → text → (speakers) → sentences → subtitle files (spec cli.md §5).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use se_asr::Transcriber;
use se_core::paths::{fallback_output_path, output_path, safe_out_stem};
use se_core::{EngineError, ErrorKind, PartialFn, Result, Sentence, StageFn, Word};
use se_diarize::Diarizer;
use se_models::{Backend, ModelKind};
use se_text::SegmenterConfig;

use crate::services::Services;

/// Interval of the partial-event throttle.
pub const PARTIAL_INTERVAL_SECS: f64 = 0.5;

/// `PipelineResult`.
#[derive(Clone, Debug, PartialEq)]
pub struct PipelineResult {
    /// The WAV used (may be a `*.tmp.wav` when the target was locked).
    pub audio: PathBuf,
    pub language: String,
    pub duration: f64,
    pub sentences: Vec<Sentence>,
    pub outputs: Vec<PathBuf>,
}

/// Builds the transcriber after the `loading_model` stage.
pub type TranscriberFactory<'a> = Box<dyn FnOnce() -> Result<Box<dyn Transcriber>> + 'a>;
/// Builds the diarizer after the `diarizing` stage (default: `Services::create_diarizer`).
pub type DiarizerFactory<'a> = Box<dyn FnOnce() -> Result<Box<dyn Diarizer>> + 'a>;

/// Options of [`run`] (Python keyword arguments).
#[derive(Clone, Debug, PartialEq)]
pub struct RunOptions {
    pub language: Option<String>,
    pub model: Option<String>,
    /// Default `["srt"]`; duplicates processed again.
    pub formats: Vec<String>,
    pub config: SegmenterConfig,
    pub diarize: bool,
    /// Validated `>= 1` here (`speakers_min`).
    pub num_speakers: Option<i64>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            language: None,
            model: None,
            formats: vec!["srt".to_string()],
            config: SegmenterConfig::default(),
            diarize: false,
            num_speakers: None,
        }
    }
}

/// `out_dir.mkdir(parents=True, exist_ok=True)`.
fn make_dirs(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| EngineError::win(&e, Some(dir)))
}

/// `pipeline.run(video, transcriber_factory, out_dir, ...)` (steps 1-20 of spec cli.md §5.6).
#[allow(clippy::too_many_arguments)]
pub fn run(
    services: &dyn Services,
    video: &Path,
    transcriber_factory: TranscriberFactory<'_>,
    out_dir: &Path,
    options: &RunOptions,
    on_stage: StageFn<'_>,
    diarizer_factory: Option<DiarizerFactory<'_>>,
    on_partial: Option<PartialFn<'_>>,
) -> Result<PipelineResult> {
    let mut on_partial = on_partial;
    // 1-4: validation (no stage before these errors).
    if options.num_speakers.is_some_and(|n| n < 1) {
        return Err(EngineError::value(se_core::t!("speakers_min")));
    }
    let model = options.model.as_deref().filter(|m| !m.is_empty());
    let info = model.and_then(se_models::find);
    if let Some(info) = info {
        if info.kind != ModelKind::Transcription {
            return Err(EngineError::value(se_core::t!("not_transcription_model", model = model.unwrap())));
        }
    }
    let language = options.language.as_deref();
    if let (Some(lang), Some(info)) = (language.filter(|l| !l.is_empty()), info) {
        if let Some(supported) = info.languages {
            if !supported.contains(&lang) {
                return Err(EngineError::value(se_models::support_error(info, lang)));
            }
        }
    }
    // 5: strict offline mode reports a missing model before any work starts.
    if let Some(model) = model {
        services.require_download(model)?;
    }
    if options.diarize {
        services.require_download(se_diarize::MODEL_ID)?;
    }

    // 6-7: audio.
    make_dirs(out_dir)?;
    let safe_stem = safe_out_stem(video, out_dir);
    let mut audio = extract_video_audio(services, video, out_dir, &mut *on_stage)?;

    // 8-10: downloads.
    if let Some(model) = model {
        if services.needs_download(model) {
            on_stage("downloading_model", Some(0.0));
            let mut progress = |f: f64| on_stage("downloading_model", Some(f));
            services.ensure_model(model, Some(&mut progress))?;
        }
    }
    if options.diarize && services.needs_download(se_diarize::MODEL_ID) {
        on_stage("downloading_model", Some(0.0));
        let mut progress = |f: f64| on_stage("downloading_model", Some(f));
        services.ensure_model(se_diarize::MODEL_ID, Some(&mut progress))?;
    }
    if info.is_none_or(|i| i.backend == Backend::Whisper) && services.needs_cuda_download() {
        on_stage("downloading_model", Some(0.0));
        let mut progress = |f: f64| on_stage("downloading_model", Some(f));
        services.ensure_cuda(Some(&mut progress));
    }
    // 11: names outside the catalog may still be fetched by the Whisper backend.
    if info.is_some() || se_core::offline::offline_requested() {
        se_core::offline::go_offline();
    }

    // 12-13: model, then self-heal a WAV removed while loading.
    on_stage("loading_model", None);
    let mut transcriber = transcriber_factory()?;
    if !audio.is_file() && video.is_file() {
        audio = extract_video_audio(services, video, out_dir, &mut *on_stage)?;
    }

    // 14-15: transcription with throttled partial snapshots.
    on_stage("transcribing", Some(0.0));
    let config = &options.config;
    let mut partial_error: Option<EngineError> = None;
    let transcript = {
        let mut on_progress = |f: f64| on_stage("transcribing", Some(f));
        let install_words = on_partial.is_some() && transcriber.supports_on_words();
        let mut last_emit: Option<Instant> = None;
        let interval = Duration::from_secs_f64(PARTIAL_INTERVAL_SECS);
        let partial = &mut on_partial;
        let error = &mut partial_error;
        let mut handle_words = |words: &[Word], detected: &str| {
            if error.is_some() {
                return;
            }
            let now = services.now();
            if let Some(last) = last_emit {
                if now.saturating_duration_since(last) < interval {
                    return;
                }
            }
            last_emit = Some(now);
            match se_text::segment(words, config, None) {
                Ok(sentences) if !sentences.is_empty() => {
                    if let Some(p) = partial.as_mut() {
                        p(&sentences, detected);
                    }
                }
                Ok(_) => {}
                Err(e) => *error = Some(e),
            }
        };
        transcriber.transcribe(
            &audio,
            language,
            Some(&mut on_progress),
            if install_words { Some(&mut handle_words) } else { None },
        )
    };
    let transcript = transcript?;
    if let Some(e) = partial_error {
        return Err(e);
    }

    // 16: diarization.
    let mut speakers: Option<Vec<Option<i64>>> = None;
    if options.diarize {
        if let Some(p) = on_partial.as_mut() {
            let snapshot = se_text::segment(&transcript.words, config, None)?;
            p(&snapshot, &transcript.language);
        }
        on_stage("diarizing", None);
        drop(transcriber); // free the ASR model before the diarizer loads
        if !audio.is_file() && video.is_file() {
            audio = extract_video_audio(services, video, out_dir, &mut *on_stage)?;
            on_stage("diarizing", None);
        }
        let mut diarizer = match diarizer_factory {
            Some(factory) => factory()?,
            None => services.create_diarizer()?,
        };
        let num_speakers = options.num_speakers.map(|n| u32::try_from(n).unwrap_or(u32::MAX));
        let segments = {
            let mut progress = |f: f64| on_stage("diarizing", if f > 0.0 { Some(f) } else { None });
            diarizer.diarize(&audio, num_speakers, Some(&mut progress), Some(&transcript.words))?
        };
        speakers = Some(se_diarize::assign_speakers(&transcript.words, &segments));
    } else {
        drop(transcriber);
    }

    // 17-19: sentences and subtitle files.
    on_stage("segmenting", None);
    let sentences = se_text::segment(&transcript.words, config, speakers.as_deref())?;
    make_dirs(out_dir)?;
    let mut outputs = Vec::new();
    for fmt in &options.formats {
        let target = output_path(out_dir, &safe_stem, fmt);
        match se_text::write(&sentences, &target, fmt) {
            Ok(path) => outputs.push(path),
            Err(e) if e.is(ErrorKind::Os) => {
                let fallback = fallback_output_path(out_dir, &safe_stem, se_core::fsutil::pid(), fmt);
                outputs.push(se_text::write(&sentences, &fallback, fmt)?);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(PipelineResult {
        audio,
        language: transcript.language,
        duration: transcript.duration,
        sentences,
        outputs,
    })
}

/// `pipeline.extract_video_audio(video, out_dir, on_stage)` with the services' audio backend.
pub fn extract_video_audio(services: &dyn Services, video: &Path, out_dir: &Path, on_stage: StageFn<'_>) -> Result<PathBuf> {
    se_audio::extract_video_audio_with(services.audio(), video, out_dir, on_stage)
}
