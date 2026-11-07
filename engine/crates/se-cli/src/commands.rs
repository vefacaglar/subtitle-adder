//! Command handlers (spec cli.md §3.2-§3.8). Each returns the exit code or an error that `main_with` turns
//! into an `error` event / `hata: ...` line.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use se_audio::PreviewSource;
use se_core::events;
use se_core::json::{self, Map, Value};
use se_core::paths::{py_path_str, resolve};
use se_core::{EngineError, ErrorKind, Result, Sentence};
use se_models::Backend;
use se_text::SegmenterConfig;

use crate::Io;
use crate::args::Command;
use crate::pipeline::{self, RunOptions};
use crate::services::Services;

/// Dispatch one parsed command.
pub fn run_command(command: &Command, services: &dyn Services, io: &mut Io<'_>) -> Result<i32> {
    match command {
        Command::ExtractAudio { video, out_dir, json } => extract_audio(services, video, out_dir, *json, io),
        Command::Preview { video, output, .. } => {
            let source =
                if py_path_str(video) == "-" { PreviewSource::Stdin } else { PreviewSource::Path(video.clone()) };
            services.make_preview(source, output)?;
            Ok(0)
        }
        Command::Transcribe { .. } => transcribe(command, services, io),
        Command::Translate { to, source, model, json } => {
            translate(services, to, source.as_deref(), model, *json, io)
        }
        Command::Render { format, out } => render(format, out.as_deref(), io),
        Command::Models { json } => models(services, *json, io),
        Command::DeleteModel { id, json } => {
            let freed = services.delete_model(id)?;
            if *json {
                events::emit(io.stdout, &events::deleted(id, freed))?;
            } else {
                writeln!(io.stdout, "{}", se_core::t!("deleted", id = id, freed = freed))?;
                io.stdout.flush()?;
            }
            Ok(0)
        }
    }
}

/// `on_stage` of `extract-audio` / `transcribe` / `translate`: `--json` → stage event; otherwise
/// `… <stage>` on stderr only when the fraction is `None`.
pub fn stage_line(json: bool, stage: &str, fraction: Option<f64>, io: &mut Io<'_>) -> std::io::Result<()> {
    if json {
        events::emit(io.stdout, &events::stage(stage, fraction))
    } else if fraction.is_none() {
        writeln!(io.stderr, "\u{2026} {stage}")?;
        io.stderr.flush()
    } else {
        Ok(())
    }
}

/// The streams shared by the callbacks of one command. Callback signatures return `()`, so the first write
/// error is kept and reported once the callee returns (Python would raise from inside the callback). It also
/// requests process-wide cancellation, so translation batches and downloads stop at their next check instead
/// of running to the end for a reader that is gone.
struct Shared<'i, 'a> {
    io: RefCell<&'i mut Io<'a>>,
    error: RefCell<Option<EngineError>>,
}

impl<'i, 'a> Shared<'i, 'a> {
    fn new(io: &'i mut Io<'a>) -> Self {
        Self { io: RefCell::new(io), error: RefCell::new(None) }
    }

    fn with(&self, f: impl FnOnce(&mut Io<'a>) -> io::Result<()>) {
        if self.error.borrow().is_some() {
            return;
        }
        let mut io = self.io.borrow_mut();
        if let Err(e) = f(&mut io) {
            let e = EngineError::from(e);
            se_core::cancel::request(e.to_string());
            *self.error.borrow_mut() = Some(e);
        }
    }

    fn stage(&self, json: bool, stage: &str, fraction: Option<f64>) {
        self.with(|io| stage_line(json, stage, fraction, io));
    }

    fn emit(&self, payload: &Value) {
        self.with(|io| events::emit(io.stdout, payload));
    }

    /// The first callback write error, if any.
    fn check(&self) -> Result<()> {
        match self.error.borrow_mut().take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

fn extract_audio(services: &dyn Services, video: &Path, out_dir: &Path, json: bool, io: &mut Io<'_>) -> Result<i32> {
    let shared = Shared::new(io);
    let audio = {
        let mut on_stage = |stage: &str, fraction: Option<f64>| shared.stage(json, stage, fraction);
        pipeline::extract_video_audio(services, &resolve(video), &resolve(out_dir), &mut on_stage)
    };
    shared.check()?;
    let audio = audio?;
    let io = shared.io.into_inner();
    let audio = py_path_str(&audio);
    if json {
        events::emit(io.stdout, &events::result("?", 0.0, &audio, &[], &[]))?;
    } else {
        writeln!(io.stdout, "-> {audio}")?;
        io.stdout.flush()?;
    }
    Ok(0)
}

fn transcribe(command: &Command, services: &dyn Services, io: &mut Io<'_>) -> Result<i32> {
    let Command::Transcribe {
        video,
        out_dir,
        language,
        model,
        formats,
        max_chars,
        max_duration,
        diarize,
        speakers,
        json,
    } = command
    else {
        unreachable!("transcribe handler")
    };
    let json = *json;
    if speakers.is_some() && !diarize {
        return Err(EngineError::value(se_core::t!("speakers_needs_diarize")));
    }
    let options = RunOptions {
        language: language.clone(),
        model: Some(model.clone()),
        formats: formats.clone(),
        config: SegmenterConfig { max_chars: *max_chars, max_duration: *max_duration, ..SegmenterConfig::default() },
        diarize: *diarize,
        num_speakers: *speakers,
    };
    let shared = Shared::new(io);
    let result = {
        let mut on_stage = |stage: &str, fraction: Option<f64>| shared.stage(json, stage, fraction);
        let mut on_partial =
            |sentences: &[Sentence], lang: &str| shared.emit(&events::partial(lang, sentences));
        let model_id = model.clone();
        let factory: pipeline::TranscriberFactory<'_> = Box::new(move || services.create_transcriber(&model_id));
        pipeline::run(
            services,
            &resolve(video),
            factory,
            &resolve(out_dir),
            &options,
            &mut on_stage,
            None,
            if json { Some(&mut on_partial) } else { None },
        )
    };
    shared.check()?;
    let result = result?;
    let io = shared.io.into_inner();
    let outputs: Vec<String> = result.outputs.iter().map(|p| py_path_str(p)).collect();
    if json {
        events::emit(
            io.stdout,
            &events::result(&result.language, result.duration, &py_path_str(&result.audio), &outputs, &result.sentences),
        )?;
    } else {
        writeln!(
            io.stdout,
            "{}",
            se_core::t!("summary", language = result.language, count = result.sentences.len())
        )?;
        for path in &outputs {
            writeln!(io.stdout, "-> {path}")?;
        }
        io.stdout.flush()?;
    }
    Ok(0)
}

/// `json.load(sys.stdin)` with a strict UTF-8 text stdin (universal newlines) → `[Sentence.from_dict(d) ...]`.
fn read_sentences(io: &mut Io<'_>) -> Result<Vec<Sentence>> {
    let mut bytes = Vec::new();
    io.stdin.read_to_end(&mut bytes)?;
    let text = json::decode_utf8_strict(&bytes).map_err(|m| EngineError::new(ErrorKind::Unicode, m))?;
    let text = se_core::pyfmt::universal_newlines(text);
    let value = json::parse(&text)?;
    se_core::types::sentences_from_value(&value)
}

fn render(format: &str, out: Option<&Path>, io: &mut Io<'_>) -> Result<i32> {
    let sentences = read_sentences(io)?;
    match out {
        Some(out) => {
            // `Path("")` is `Path(".")` (truthy).
            let out: PathBuf = if out.as_os_str().is_empty() { PathBuf::from(".") } else { out.to_path_buf() };
            se_text::write(&sentences, &out, format)?;
        }
        None => {
            let text = se_text::render(&sentences, format)?;
            io.stdout.write_all(text.as_bytes())?;
            io.stdout.flush()?;
        }
    }
    Ok(0)
}

fn translate(
    services: &dyn Services,
    to: &str,
    source: Option<&str>,
    model: &str,
    json: bool,
    io: &mut Io<'_>,
) -> Result<i32> {
    let info = se_translate::check_request(model, to, source)?;
    let external_url = se_translate::external_llm_url();
    if let Some(url) = &external_url {
        se_translate::external::check_external_llm(url, se_translate::external_llm_api_key().as_deref())?;
    }
    let sentences = read_sentences(io)?;
    let shared = Shared::new(io);
    let translated = (|| -> Result<Vec<Sentence>> {
        let on_stage = |stage: &str, fraction: Option<f64>| shared.stage(json, stage, fraction);
        if external_url.is_none() {
            if services.require_download(model)? {
                on_stage("downloading_model", Some(0.0));
                let mut progress = |f: f64| on_stage("downloading_model", Some(f));
                services.ensure_model(model, Some(&mut progress))?;
            }
            if info.backend == Backend::Llama {
                if services.require_llama_download()? {
                    on_stage("downloading_model", Some(0.0));
                    let mut progress = |f: f64| on_stage("downloading_model", Some(f));
                    services.ensure_llama(Some(&mut progress))?;
                }
            } else if services.needs_cuda_download() {
                on_stage("downloading_model", Some(0.0));
                let mut progress = |f: f64| on_stage("downloading_model", Some(f));
                services.ensure_cuda(Some(&mut progress));
            }
        }
        se_core::offline::go_offline();
        on_stage("loading_model", None);
        let mut translator = services.create_translator(model)?;
        on_stage("translating", None);
        let result = {
            let mut on_progress = |f: f64| shared.stage(json, "translating", Some(f));
            let mut on_batch = |prefix: &[Sentence]| shared.emit(&events::translation(to, prefix));
            translator.translate(
                &sentences,
                to,
                source,
                Some(&mut on_progress),
                if json { Some(&mut on_batch) } else { None },
            )
        };
        translator.close();
        result
    })();
    shared.check()?;
    let translated = translated?;
    let io = shared.io.into_inner();
    if json {
        events::emit(io.stdout, &events::translation(to, &translated))?;
    } else {
        let text = se_text::render(&translated, "srt")?;
        io.stdout.write_all(text.as_bytes())?;
        io.stdout.flush()?;
    }
    Ok(0)
}

/// Pad like Python `f"{value:<width}"` / `f"{value:>width}"` (code points).
fn left(value: &str, width: usize) -> String {
    format!("{value:<width$}")
}
fn right(value: &str, width: usize) -> String {
    format!("{value:>width$}")
}

fn models(services: &dyn Services, json: bool, io: &mut Io<'_>) -> Result<i32> {
    let sizes = services.cached_disk_mb();
    let rows: Vec<(Map, &se_models::ModelInfo, Option<i64>, bool)> = se_models::KATALOG
        .iter()
        .map(|info| {
            let disk = services.disk_mb(info.id, &sizes);
            let downloaded = services.is_downloaded(info.id);
            let languages = match info.languages {
                Some(list) => Value::Array(list.iter().map(|l| Value::from(*l)).collect()),
                None => Value::Null,
            };
            let row = Map::new()
                .with("id", info.id)
                .with("kind", info.kind.as_str())
                .with("backend", info.backend.as_str())
                .with("languages", languages)
                .with("size_mb", info.size_mb)
                .with("disk_mb", disk)
                .with("speed", info.speed)
                .with("accuracy", info.accuracy)
                .with("license", info.license)
                .with("downloaded", downloaded);
            (row, info, disk, downloaded)
        })
        .collect();
    if json {
        events::emit(io.stdout, &events::models(rows.into_iter().map(|(row, ..)| Value::Object(row)).collect()))?;
        return Ok(0);
    }
    let t = |key: &str| se_core::i18n::t(key, &[]);
    let mut out = String::new();
    out.push_str(&left(&t("col_model"), 22));
    out.push_str(&right(&t("col_kind"), 14));
    out.push_str(&right(&t("col_size"), 10));
    out.push_str(&right(&t("col_disk"), 10));
    out.push_str(&right(&t("col_speed"), 6));
    out.push_str(&right(&t("col_accuracy"), 10));
    out.push_str(&right(&t("col_license"), 20));
    out.push_str(&right(&t("col_cache"), 10));
    out.push('\n');
    for (_, info, disk, downloaded) in &rows {
        let disk = match disk {
            None => "\u{2014}".to_string(),
            Some(mb) => format!("{mb} MB"),
        };
        let cached = if *downloaded { t("cached_yes") } else { t("cached_no") };
        out.push_str(&left(info.id, 22));
        out.push_str(&right(info.kind.as_str(), 14));
        out.push_str(&right(&info.size_mb.to_string(), 7));
        out.push_str(" MB");
        out.push_str(&right(&disk, 10));
        out.push_str(&right(&info.speed.to_string(), 6));
        out.push_str(&right(&info.accuracy.to_string(), 10));
        out.push_str(&right(info.license, 20));
        out.push_str(&right(&cached, 10));
        out.push('\n');
    }
    io.stdout.write_all(out.as_bytes())?;
    io.stdout.flush()?;
    Ok(0)
}
