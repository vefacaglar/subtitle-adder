//! Model downloads with progress (`ensure_model`, spec models.md §4.8) and the download decisions.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use se_core::{EngineError, ProgressFn, Result};

use crate::cache::{MIB, hub_cache_dir, is_info_downloaded_by, try_to_load_from_cache_in};
use crate::catalog::{Backend, ModelInfo, PROGRESS_INTERVAL_SECS, VAD_FILE, VAD_REPO, find};
use crate::hub::{HfHub, Hub};

/// `needs_download(id) = find(id) is not None and not is_downloaded(id)` (names outside the catalog are
/// never "missing").
pub fn needs_download(model_id: &str) -> bool {
    find(model_id).is_some() && !crate::cache::is_downloaded(model_id)
}

/// `require_download(id)`: [`needs_download`]; in strict offline mode a missing model is a localized
/// `ValueError` `offline_missing_model` (with `display_name(id)`). Does not go offline.
pub fn require_download(model_id: &str) -> Result<bool> {
    let missing = needs_download(model_id);
    if missing && se_core::offline::offline_requested() {
        return Err(EngineError::value(se_core::t!(
            "offline_missing_model",
            model = se_core::i18n::display_name(model_id)
        )));
    }
    Ok(missing)
}

/// `ensure_model(model_id, on_progress)`: downloads every missing file of a catalog model into the HF cache
/// (onnx models also fetch the shared silero VAD), reporting downloaded/total bytes in `0..=1` at most ~10 Hz
/// and ending with 1.0. Returns the main snapshot folder (`None` for ids outside the catalog, which are not
/// downloaded here). Fails without I/O when the network is locked ([`se_core::offline::hub_offline`]).
pub fn ensure_model(model_id: &str, on_progress: Option<ProgressFn<'_>>) -> Result<Option<PathBuf>> {
    let hub = HfHub::new();
    let cache = hub.cache_dir.clone();
    ensure_model_with(&hub, &cache, model_id, on_progress, Duration::from_secs_f64(PROGRESS_INTERVAL_SECS))
}

/// [`ensure_model`] with an injectable Hub client, cache folder (for the "already cached" checks) and
/// progress interval (tests set it to 0, like `monkeypatch.setattr(models, "PROGRESS_INTERVAL", 0)`).
pub fn ensure_model_with(
    hub: &dyn Hub,
    cache_dir: &Path,
    model_id: &str,
    on_progress: Option<ProgressFn<'_>>,
    interval: Duration,
) -> Result<Option<PathBuf>> {
    let cached = |repo: &str, name: &str| try_to_load_from_cache_in(cache_dir, repo, name).is_some();
    let Some(info) = find(model_id) else { return Ok(None) };
    if is_info_downloaded_by(info, &cached) {
        return Ok(None);
    }
    let mut on_progress = on_progress;
    let downloads: Vec<(&str, &[&str], u64)> = if info.backend == Backend::Onnx {
        let mut d = Vec::new();
        if !info.files.iter().all(|n| cached(info.repo, n)) {
            d.push((info.repo, info.files, hub.repo_bytes(info.repo, info.files, info.size_mb as u64 * MIB)));
        }
        if !cached(VAD_REPO, VAD_FILE) {
            const VAD: &[&str] = &[VAD_FILE];
            d.push((VAD_REPO, VAD, hub.repo_bytes(VAD_REPO, VAD, 2 * MIB)));
        }
        d
    } else if !info.extra.is_empty() {
        info.parts()
            .into_iter()
            .enumerate()
            .filter(|(_, (repo, files))| !files.iter().all(|n| cached(repo, n)))
            .map(|(i, (repo, files))| (repo, files, hub.repo_bytes(repo, files, fallback_bytes(info, i))))
            .collect()
    } else {
        let fallback = info.part_bytes.first().copied().unwrap_or(info.size_mb as u64 * MIB);
        vec![(info.repo, info.files, hub.repo_bytes(info.repo, info.files, fallback))]
    };

    let total: u64 = downloads.iter().map(|(_, _, s)| s).sum();
    let mut reporter = Reporter::new(total, on_progress.as_deref_mut(), interval);
    let mut model_path = None;
    let mut base = 0u64;
    for (repo, patterns, size) in downloads {
        let mut on_bytes = |n: u64| reporter.report(base + n);
        let path = hub.snapshot_download(repo, patterns, &mut on_bytes)?;
        if repo == info.repo {
            model_path = Some(path);
        }
        base += size; // the estimate, not the bytes actually received (parity)
    }
    if let Some(cb) = on_progress.as_mut() {
        cb(1.0); // files are in place; the last report must not stay at 0.99
    }
    Ok(model_path)
}

/// `_fallback_bytes(info, index)`.
pub fn fallback_bytes(info: &ModelInfo, index: usize) -> u64 {
    if let Some(b) = info.part_bytes.get(index) {
        return *b;
    }
    (info.size_mb / info.parts().len() as i64).max(1) as u64 * MIB
}

/// `_reporter(total, on_progress)`: bytes → monotonic fraction in (0, 0.99], at most once per `interval`
/// (the first report always passes).
pub struct Reporter<'a, 'b> {
    on_progress: Option<&'a mut (dyn FnMut(f64) + 'b)>,
    total: u64,
    fraction: f64,
    at: Option<Instant>,
    interval: Duration,
}

impl<'a, 'b> Reporter<'a, 'b> {
    pub fn new(total: u64, on_progress: Option<&'a mut (dyn FnMut(f64) + 'b)>, interval: Duration) -> Self {
        Reporter { on_progress, total, fraction: 0.0, at: None, interval }
    }

    pub fn report(&mut self, downloaded: u64) {
        if self.total == 0 {
            return;
        }
        let Some(cb) = self.on_progress.as_mut() else { return };
        let fraction = (downloaded as f64 / self.total as f64).clamp(0.0, 0.99);
        if fraction <= self.fraction {
            return;
        }
        let now = Instant::now();
        if let Some(at) = self.at {
            if now.duration_since(at) < self.interval {
                return;
            }
        }
        self.fraction = fraction;
        self.at = Some(now);
        cb(fraction);
    }
}

/// The default cache folder used by [`ensure_model`] (re-export for callers).
pub fn default_cache_dir() -> PathBuf {
    hub_cache_dir()
}
