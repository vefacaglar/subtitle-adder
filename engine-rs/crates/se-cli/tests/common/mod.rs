//! Shared test helpers: the `tests/conftest.py` fixtures and the fakes the Python tests monkeypatch in.
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use se_asr::Transcriber;
use se_audio::{AudioBackend, FfmpegBackend};
use se_cli::{Io, Services, main_with};
use se_core::json::Value;
use se_core::{EngineError, ProgressFn, Result, Transcript, Word, WordsFn};
use se_diarize::Diarizer;
use se_translate::Translator;

pub type DiarizerMaker = Box<dyn Fn() -> Result<Box<dyn Diarizer>>>;
pub type TranslatorMaker = Box<dyn Fn(&str) -> Result<Box<dyn Translator>>>;

/// The autouse conftest fixtures: no `SUBTITLE_ENGINE_LANG`, no language override, strict offline off, HF
/// offline state restored, an isolated `tempfile.gettempdir()`. Tests using it must be `#[serial]`.
pub struct Guard {
    pub tmp: tempfile::TempDir,
}

impl Guard {
    pub fn new() -> Self {
        reset();
        let tmp = tempfile::tempdir().unwrap();
        let temp_root = tmp.path().join("_tmp");
        std::fs::create_dir_all(&temp_root).unwrap();
        se_core::fsutil::set_tempdir(Some(temp_root));
        Guard { tmp }
    }

    /// `tmp_path` (resolved, like pytest's).
    pub fn path(&self) -> PathBuf {
        se_core::paths::resolve(self.tmp.path())
    }
}

fn reset() {
    // SAFETY: tests touching the environment are #[serial].
    unsafe { std::env::remove_var(se_core::i18n::ENV_VAR) };
    se_core::i18n::set_language(None);
    se_core::offline::reset_for_tests();
    se_core::fsutil::set_pid_override(None);
}

impl Drop for Guard {
    fn drop(&mut self) {
        reset();
        se_core::fsutil::set_tempdir(None);
    }
}

pub fn set_env(name: &str, value: &str) {
    // SAFETY: #[serial] tests.
    unsafe { std::env::set_var(name, value) };
}

/// conftest `make_words` (the `step * 0.8` expression kept literally).
pub fn make_words(text: &str) -> Vec<Word> {
    let step = 0.4;
    text.split_whitespace()
        .enumerate()
        .map(|(i, w)| Word::new(i as f64 * step, i as f64 * step + step * 0.8, w))
        .collect()
}

/// test_cli `FakeTranscriber`: reports 0.5, returns "Merhaba dünya. Görüşürüz." in `tr`; no `on_words`.
pub struct FakeTranscriber {
    pub text: &'static str,
}

impl Transcriber for FakeTranscriber {
    fn transcribe(
        &mut self,
        _audio: &Path,
        _language: Option<&str>,
        on_progress: Option<ProgressFn<'_>>,
        _on_words: Option<WordsFn<'_>>,
    ) -> Result<Transcript> {
        if let Some(p) = on_progress {
            p(0.5);
        }
        Ok(Transcript { language: "tr".into(), duration: 2.0, words: make_words(self.text) })
    }

    fn supports_on_words(&self) -> bool {
        false
    }
}

/// What a fake extractor does when called.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExtractMode {
    /// FakeExtractor: `b"\0" * 64 + f"audio-of:{video}"`.
    Fake,
    /// Writes 100 bytes, then fails with `decode failed`.
    Failing,
    /// `pytest.fail(...)`: must not be called.
    Forbidden,
    /// The real FFmpeg extraction.
    Real,
}

pub type ReplaceHook = Box<dyn Fn(&Path, &Path) -> Option<io::Result<()>>>;

/// `pipeline.extract_audio` / `Path.replace` stand-in.
pub struct FakeExtractor {
    pub mode: Cell<ExtractMode>,
    pub calls: RefCell<Vec<PathBuf>>,
    pub targets: RefCell<Vec<PathBuf>>,
    /// Returns `Some(result)` to override a rename.
    pub replace_hook: RefCell<Option<ReplaceHook>>,
}

impl FakeExtractor {
    pub fn new(mode: ExtractMode) -> Self {
        Self { mode: Cell::new(mode), calls: RefCell::default(), targets: RefCell::default(), replace_hook: RefCell::new(None) }
    }
}

pub fn audio_of(video: &Path) -> Vec<u8> {
    format!("audio-of:{}", se_core::paths::py_path_str(video)).into_bytes()
}

impl AudioBackend for FakeExtractor {
    fn extract_audio(&self, video: &Path, out_wav: &Path, on_progress: Option<ProgressFn<'_>>) -> Result<PathBuf> {
        match self.mode.get() {
            ExtractMode::Real => return FfmpegBackend.extract_audio(video, out_wav, on_progress),
            ExtractMode::Forbidden => panic!("extract_audio must not be called"),
            ExtractMode::Failing => {
                std::fs::write(out_wav, vec![0u8; 100]).unwrap();
                return Err(EngineError::runtime("decode failed"));
            }
            ExtractMode::Fake => {}
        }
        self.calls.borrow_mut().push(video.to_path_buf());
        self.targets.borrow_mut().push(out_wav.to_path_buf());
        let mut on_progress = on_progress;
        if let Some(p) = on_progress.as_mut() {
            p(0.5);
        }
        let mut body = vec![0u8; 64];
        body.extend(audio_of(video));
        std::fs::write(out_wav, body).unwrap();
        if let Some(p) = on_progress.as_mut() {
            p(1.0);
        }
        Ok(out_wav.to_path_buf())
    }

    fn replace(&self, src: &Path, dst: &Path) -> io::Result<()> {
        if let Some(hook) = self.replace_hook.borrow().as_ref() {
            if let Some(result) = hook(src, dst) {
                return result;
            }
        }
        se_core::fsutil::replace(src, dst)
    }
}

/// Configurable test services (everything the Python tests monkeypatch).
pub struct Fake {
    /// `models.needs_download` override; `None` → catalog id and not `is_downloaded`.
    pub needs: Option<Box<dyn Fn(&str) -> bool>>,
    /// `models.is_downloaded`.
    pub downloaded: Box<dyn Fn(&str) -> bool>,
    /// Fractions the fake `ensure_model` reports (`None` → it must not be called).
    pub ensure_fractions: Option<Vec<f64>>,
    pub ensured: RefCell<Vec<String>>,
    pub sizes: BTreeMap<String, i64>,
    pub delete_result: Box<dyn Fn(&str) -> Result<i64>>,
    pub deleted: RefCell<Vec<String>>,
    pub onnx_calls: RefCell<Vec<(String, Option<String>)>>,
    pub whisper_calls: RefCell<Vec<String>>,
    pub audio: FakeExtractor,
    pub text: &'static str,
    /// `diarize.create_diarizer` replacement (`None` → the real one).
    pub diarizer: Option<DiarizerMaker>,
    /// `translate.create_translator` replacement (`None` → the real one).
    pub translator: Option<TranslatorMaker>,
    /// `models.require_download` replacement (`None` → built on `needs_download`).
    pub require: Option<Box<dyn Fn(&str) -> Result<bool>>>,
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            needs: Some(Box::new(|_| false)),
            downloaded: Box::new(|_| false),
            ensure_fractions: None,
            ensured: RefCell::default(),
            sizes: BTreeMap::new(),
            delete_result: Box::new(|_| Ok(123)),
            deleted: RefCell::default(),
            onnx_calls: RefCell::default(),
            whisper_calls: RefCell::default(),
            audio: FakeExtractor::new(ExtractMode::Real),
            text: "Merhaba dünya. Görüşürüz.",
            diarizer: None,
            translator: None,
            require: None,
        }
    }
}

impl Services for Fake {
    fn create_whisper_transcriber(&self, model_id: &str) -> Result<Box<dyn Transcriber>> {
        self.whisper_calls.borrow_mut().push(model_id.into());
        Ok(Box::new(FakeTranscriber { text: self.text }))
    }

    fn create_onnx_transcriber(&self, model_name: &str, quantization: Option<&str>) -> Result<Box<dyn Transcriber>> {
        self.onnx_calls.borrow_mut().push((model_name.into(), quantization.map(String::from)));
        Ok(Box::new(FakeTranscriber { text: self.text }))
    }

    fn is_downloaded(&self, model_id: &str) -> bool {
        (self.downloaded)(model_id)
    }

    fn create_diarizer(&self) -> Result<Box<dyn Diarizer>> {
        match &self.diarizer {
            Some(make) => make(),
            None => se_diarize::create_diarizer(),
        }
    }

    fn create_translator(&self, model_id: &str) -> Result<Box<dyn Translator>> {
        match &self.translator {
            Some(make) => make(model_id),
            None => se_translate::create_translator(model_id, se_translate::Device::Auto, "int8"),
        }
    }

    fn require_download(&self, model_id: &str) -> Result<bool> {
        if let Some(f) = &self.require {
            return f(model_id);
        }
        let missing = self.needs_download(model_id);
        if missing && se_core::offline::offline_requested() {
            return Err(EngineError::value(se_core::t!(
                "offline_missing_model",
                model = se_core::i18n::display_name(model_id)
            )));
        }
        Ok(missing)
    }

    fn needs_download(&self, model_id: &str) -> bool {
        match &self.needs {
            Some(f) => f(model_id),
            None => se_models::find(model_id).is_some() && !self.is_downloaded(model_id),
        }
    }

    fn ensure_model(&self, model_id: &str, on_progress: Option<ProgressFn<'_>>) -> Result<()> {
        let fractions = self.ensure_fractions.as_ref().expect("ensure_model must not be called");
        self.ensured.borrow_mut().push(model_id.into());
        let mut on_progress = on_progress;
        for f in fractions {
            if let Some(p) = on_progress.as_mut() {
                p(*f);
            }
        }
        Ok(())
    }

    fn cached_disk_mb(&self) -> BTreeMap<String, i64> {
        self.sizes.clone()
    }

    fn disk_mb(&self, model_id: &str, sizes: &BTreeMap<String, i64>) -> Option<i64> {
        let info = se_models::find(model_id)?;
        if !self.is_downloaded(model_id) {
            return None;
        }
        sizes.get(info.repo).copied()
    }

    fn delete_model(&self, model_id: &str) -> Result<i64> {
        self.deleted.borrow_mut().push(model_id.into());
        (self.delete_result)(model_id)
    }

    fn needs_cuda_download(&self) -> bool {
        false
    }

    fn require_llama_download(&self) -> Result<bool> {
        Ok(false)
    }

    fn audio(&self) -> &dyn AudioBackend {
        &self.audio
    }
}

/// Runs the CLI in-process with `stdin`; returns (exit code, stdout, stderr).
pub fn run_with_stdin(services: &dyn Services, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
    let argv: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut stdin: &[u8] = stdin;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = {
        let mut io = Io { stdin: &mut stdin, stdout: &mut stdout, stderr: &mut stderr };
        main_with(&argv, services, &mut io)
    };
    (code, String::from_utf8(stdout).unwrap(), String::from_utf8(stderr).unwrap())
}

pub fn run(services: &dyn Services, args: &[&str]) -> (i32, String, String) {
    run_with_stdin(services, args, b"")
}

/// Every stdout line parsed as JSON.
pub fn events(stdout: &str) -> Vec<Value> {
    stdout.lines().map(|l| se_core::json::parse(l).unwrap()).collect()
}

pub fn ev_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// `(stage, fraction)` of every stage event.
pub fn stages(events: &[Value]) -> Vec<(String, Option<f64>)> {
    events
        .iter()
        .filter(|e| ev_str(e, "event") == Some("stage"))
        .map(|e| (ev_str(e, "stage").unwrap().to_string(), e.get("fraction").and_then(Value::as_f64)))
        .collect()
}

/// Path of a test ffmpeg executable (devenv's `FFMPEG_DIR/bin`, then PATH).
fn ffmpeg_exe() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    if let Some(dir) = std::env::var_os("FFMPEG_DIR") {
        let p = PathBuf::from(dir).join("bin").join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()))
        .flatten()
}

/// conftest `tone_video`: 2 s mp4, 64x64 10 fps H.264 + 44.1 kHz stereo AAC 440 Hz sine at 0.3. `None` (the
/// test is skipped) when no ffmpeg executable is available to generate it.
pub fn tone_video(dir: &Path) -> Option<PathBuf> {
    let Some(ffmpeg) = ffmpeg_exe() else {
        eprintln!("skipped: no ffmpeg executable to generate tone.mp4");
        return None;
    };
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("tone.mp4");
    let status = std::process::Command::new(ffmpeg)
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=64x64:r=10:d=2", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:sample_rate=44100:duration=2,volume=0.3,aformat=channel_layouts=stereo")
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&path)
        .status()
        .ok()?;
    status.success().then_some(path)
}
