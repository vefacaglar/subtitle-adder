//! WAV reuse (`pipeline.py` audio part): `<out_dir>/<stem>-<hash>.wav` plus `<wav>.source.json`.
//!
//! Naming helpers live in [`se_core::paths`] (`wav_path_for`, `safe_out_stem`, `source_hash`, `tmp_wav_path`,
//! `marker_path`, `marker_tmp_path`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use se_core::events::stages::EXTRACTING_AUDIO;
use se_core::fsutil::{pid, unlink_quiet};
use se_core::json::{self, Map, Value};
use se_core::paths::{marker_path, marker_tmp_path, py_path_str, resolve, tmp_wav_path, wav_path_for};
use se_core::{EngineError, ProgressFn, Result, StageFn};

/// `_source_identity(video)`: `{"source": str(video.resolve()), "size": st_size, "mtime_ns": st_mtime_ns}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceIdentity {
    pub source: String,
    pub size: u64,
    /// Nanoseconds since the Unix epoch (negative before 1970).
    pub mtime_ns: i128,
}

/// The two operations tests replace (`pipeline.extract_audio` and `Path.replace`).
pub trait AudioBackend {
    /// Decode `video` into `out_wav` (the real one is [`crate::audio::extract_audio`]).
    fn extract_audio(&self, video: &Path, out_wav: &Path, on_progress: Option<ProgressFn<'_>>) -> Result<PathBuf>;

    /// Rename `src` over `dst` (Python `Path.replace`).
    fn replace(&self, src: &Path, dst: &Path) -> io::Result<()> {
        se_core::fsutil::replace(src, dst)
    }
}

/// The production backend (FFmpeg decode, `std::fs::rename`).
#[derive(Clone, Copy, Debug, Default)]
pub struct FfmpegBackend;

impl AudioBackend for FfmpegBackend {
    fn extract_audio(&self, video: &Path, out_wav: &Path, on_progress: Option<ProgressFn<'_>>) -> Result<PathBuf> {
        crate::audio::extract_audio(video, out_wav, on_progress)
    }
}

/// `os.stat(...).st_mtime_ns`.
pub fn mtime_ns(meta: &fs::Metadata) -> i128 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        (meta.last_write_time() as i128 - 116_444_736_000_000_000) * 100
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128
    }
}

/// `_source_identity(video)`; `None` when the video cannot be stat'ed.
pub fn source_identity(video: &Path) -> Option<SourceIdentity> {
    let resolved = resolve(video);
    let meta = fs::metadata(video).ok()?;
    Some(SourceIdentity { source: py_path_str(&resolved), size: meta.len(), mtime_ns: mtime_ns(&meta) })
}

/// Python `value == n` for a JSON value and an integer (`1 == 1.0 == True`).
fn py_eq_int(value: Option<&Value>, n: i128) -> bool {
    match value {
        Some(Value::Int(i)) => *i as i128 == n,
        Some(Value::Bool(b)) => (*b as i128) == n,
        Some(Value::Float(f)) => f.is_finite() && f.fract() == 0.0 && f.abs() < 1.7e38 && (*f as i128) == n,
        Some(Value::BigInt(s)) => s.parse::<i128>().ok() == Some(n),
        _ => false,
    }
}

/// `_wav_matches_source(wav, identity)`: the WAV is a file larger than 44 bytes and its marker (version 1)
/// matches `identity` and the WAV size.
pub fn wav_matches_source(wav: &Path, identity: Option<&SourceIdentity>) -> bool {
    let Some(identity) = identity else { return false };
    let Ok(meta) = fs::metadata(wav) else { return false };
    if !meta.is_file() {
        return false;
    }
    let wav_size = meta.len();
    if wav_size <= 44 {
        return false;
    }
    let Ok(bytes) = fs::read(marker_path(wav)) else { return false };
    let Ok(text) = std::str::from_utf8(&bytes) else { return false };
    let Ok(marker) = json::parse(&se_core::pyfmt::universal_newlines(text)) else { return false };
    let Value::Object(marker) = marker else { return false };
    if !py_eq_int(marker.get("version"), se_core::paths::SOURCE_MARKER_VERSION as i128) {
        return false;
    }
    py_eq_int(marker.get("wav_size"), wav_size as i128)
        && matches!(marker.get("source"), Some(Value::Str(s)) if *s == identity.source)
        && py_eq_int(marker.get("size"), identity.size as i128)
        && py_eq_int(marker.get("mtime_ns"), identity.mtime_ns)
}

/// The marker JSON text (`json.dumps(payload)`, ensure_ascii=True, no newline).
pub fn marker_text(identity: &SourceIdentity, wav_size: i64) -> String {
    let payload = Map::new()
        .with("version", se_core::paths::SOURCE_MARKER_VERSION)
        .with("source", identity.source.as_str())
        .with("size", identity.size)
        .with("mtime_ns", identity.mtime_ns)
        .with("wav_size", wav_size);
    json::dumps(&Value::Object(payload), true)
}

/// `_write_source_marker(wav, identity, wav_size)`: `json.dumps({"version": 1, "source", "size", "mtime_ns",
/// "wav_size"})` (ensure_ascii=True, no newline) via `<marker>.<pid>.tmp` + rename. Never fails: on any error
/// both the temp file and the marker are removed (errors ignored). `identity == None` → nothing happens.
pub fn write_source_marker(wav: &Path, identity: Option<&SourceIdentity>, wav_size: i64) {
    let Some(identity) = identity else { return };
    let marker = marker_path(wav);
    let tmp = marker_tmp_path(wav, pid());
    let result = fs::write(&tmp, marker_text(identity, wav_size)).and_then(|()| se_core::fsutil::replace(&tmp, &marker));
    if result.is_err() {
        unlink_quiet(&tmp);
        unlink_quiet(&marker);
    }
}

/// `extract_video_audio(video, out_dir, on_stage)` with the FFmpeg backend.
pub fn extract_video_audio(video: &Path, out_dir: &Path, on_stage: StageFn<'_>) -> Result<PathBuf> {
    extract_video_audio_with(&FfmpegBackend, video, out_dir, on_stage)
}

/// Removes the temp WAV unless disarmed (errors, and also unwinding panics).
struct TempGuard<'a> {
    path: &'a Path,
    armed: bool,
}

impl Drop for TempGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            unlink_quiet(self.path);
        }
    }
}

/// `extract_video_audio` (spec cli.md §5.5): reuse (single `extracting_audio 1.0`) or extract to
/// `<stem>-<hash>.<pid>.tmp.wav` (`extracting_audio null`, then the decoder's fractions) and rename into place;
/// on a failed rename return the existing WAV if it now matches, else the temp WAV. The temp file is removed
/// on every error. Returns the WAV path to use.
pub fn extract_video_audio_with(
    backend: &dyn AudioBackend,
    video: &Path,
    out_dir: &Path,
    on_stage: StageFn<'_>,
) -> Result<PathBuf> {
    fs::create_dir_all(out_dir).map_err(|e| EngineError::win(&e, Some(out_dir)))?;
    let wav = wav_path_for(video, out_dir);
    let identity = source_identity(video);
    if wav_matches_source(&wav, identity.as_ref()) {
        on_stage(EXTRACTING_AUDIO, Some(1.0));
        return Ok(wav);
    }
    on_stage(EXTRACTING_AUDIO, None);
    let tmp = tmp_wav_path(&wav, pid());
    let mut guard = TempGuard { path: &tmp, armed: true };
    let extracted = {
        let mut progress = |f: f64| on_stage(EXTRACTING_AUDIO, Some(f));
        backend.extract_audio(video, &tmp, Some(&mut progress))?
    };
    if !tmp.is_file() {
        guard.armed = false;
        return Ok(extracted);
    }
    let wav_size = fs::metadata(&tmp).map(|m| m.len() as i64).unwrap_or(-1);
    guard.armed = false;
    match backend.replace(&tmp, &wav) {
        Err(_) => {
            if wav_matches_source(&wav, identity.as_ref()) {
                unlink_quiet(&tmp);
                Ok(wav)
            } else {
                Ok(tmp.clone())
            }
        }
        Ok(()) => {
            write_source_marker(&wav, identity.as_ref(), wav_size);
            Ok(wav)
        }
    }
}
