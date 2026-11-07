//! Reading and cleaning the Hugging Face cache (huggingface_hub 2.4.0 layout; spec models.md §3, §4).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use se_core::paths::py_join;
use se_core::{EngineError, Result};

use crate::catalog::{KATALOG, ModelInfo, ModelKind, VAD_FILE, VAD_REPO, find};
use crate::fsx;
use crate::scan::{self, CachedRepo};

pub(crate) const MIB: u64 = 1024 * 1024;

fn env_is_true(name: &str) -> bool {
    std::env::var(name).map(|v| ["1", "ON", "YES", "TRUE"].contains(&v.to_uppercase().as_str())).unwrap_or(false)
}

/// `HF_HOME` as huggingface_hub computes it.
pub fn hf_home() -> String {
    let raw = match std::env::var("HF_HOME") {
        Ok(v) => v,
        Err(_) => {
            let default_home = py_join(&fsx::expanduser("~"), ".cache");
            let base = std::env::var("XDG_CACHE_HOME").unwrap_or(default_home);
            py_join(&base, "huggingface")
        }
    };
    fsx::expandvars(&fsx::expanduser(&raw))
}

/// `HF_HUB_CACHE`: `$HF_HUB_CACHE`, else `$HUGGINGFACE_HUB_CACHE`, else `$HF_HOME/hub` (`HF_HOME` from
/// `$XDG_CACHE_HOME/huggingface` or `~/.cache/huggingface`), with `~` and variables expanded, exactly as
/// huggingface_hub computes it. Read on every call (tests change it).
pub fn hub_cache_dir() -> PathBuf {
    let raw = match std::env::var("HF_HUB_CACHE") {
        Ok(v) => v,
        Err(_) => std::env::var("HUGGINGFACE_HUB_CACHE").unwrap_or_else(|_| py_join(&hf_home(), "hub")),
    };
    PathBuf::from(fsx::expandvars(&fsx::expanduser(&raw)))
}

/// `HF_ENDPOINT` (default `https://huggingface.co`, trailing `/` stripped).
pub fn hf_endpoint() -> String {
    std::env::var("HF_ENDPOINT").unwrap_or_else(|_| "https://huggingface.co".to_string()).trim_end_matches('/').to_string()
}

/// `HF_HUB_DISABLE_SYMLINKS` is true.
pub fn symlinks_disabled() -> bool {
    env_is_true("HF_HUB_DISABLE_SYMLINKS")
}

/// The Hub token: `$HF_TOKEN` (stripped, non-empty), else the `HF_TOKEN_PATH` / `<HF_HOME>/token` file.
pub fn hf_token() -> Option<String> {
    if let Ok(t) = std::env::var("HF_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let path = match std::env::var("HF_TOKEN_PATH") {
        Ok(p) => fsx::expandvars(&fsx::expanduser(&p)),
        Err(_) => py_join(&hf_home(), "token"),
    };
    let t = fs::read_to_string(path).ok()?.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// `models--{org}--{name}` folder of `repo_id` inside `cache_dir`.
pub fn repo_folder(cache_dir: &Path, repo_id: &str) -> PathBuf {
    cache_dir.join(format!("models--{}", repo_id.replace('/', "--")))
}

/// `huggingface_hub.try_to_load_from_cache(repo_id, filename)` for revision `main` (no network): the
/// snapshot file path, or `None` (not cached, `.no_exist` marker, unknown revision). `refs/main` is read
/// without stripping.
pub fn try_to_load_from_cache(repo_id: &str, filename: &str) -> Option<PathBuf> {
    try_to_load_from_cache_in(&hub_cache_dir(), repo_id, filename)
}

/// [`try_to_load_from_cache`] in an explicit cache folder.
pub fn try_to_load_from_cache_in(cache_dir: &Path, repo_id: &str, filename: &str) -> Option<PathBuf> {
    let repo_cache = repo_folder(cache_dir, repo_id);
    if !fsx::is_dir(&repo_cache) {
        return None;
    }
    let mut revision = "main".to_string();
    let refs = repo_cache.join("refs");
    let main = refs.join("main");
    if fsx::is_dir(&refs) && fsx::is_file(&main) {
        // `open(ref).read()`: text mode, universal newlines, not stripped.
        let bytes = fs::read(&main).ok()?;
        revision = se_core::pyfmt::universal_newlines(&String::from_utf8_lossy(&bytes));
    }
    let relative: PathBuf = filename.split('/').collect();
    if fsx::is_file(&repo_cache.join(".no_exist").join(&revision).join(&relative)) {
        return None;
    }
    let snapshots = repo_cache.join("snapshots");
    if !fsx::exists(&snapshots) {
        return None;
    }
    let listed = fs::read_dir(&snapshots).ok()?.flatten().any(|e| e.file_name().to_string_lossy() == revision);
    if !listed {
        return None;
    }
    let path = snapshots.join(&revision).join(&relative);
    fsx::is_file(&path).then_some(path)
}

/// `is_downloaded` with an explicit file lookup (`_cached(repo, name)`), as `models.py` decides.
pub fn is_downloaded_by(model_id: &str, cached: &dyn Fn(&str, &str) -> bool) -> bool {
    let Some(info) = find(model_id) else { return false };
    is_info_downloaded_by(info, cached)
}

pub(crate) fn is_info_downloaded_by(info: &ModelInfo, cached: &dyn Fn(&str, &str) -> bool) -> bool {
    match info.kind {
        ModelKind::Diarization => info.parts().iter().all(|(repo, files)| files.iter().all(|n| cached(repo, n))),
        ModelKind::Translation => info.files.iter().all(|n| cached(info.repo, n)),
        ModelKind::Transcription if info.backend == crate::catalog::Backend::Onnx => {
            info.files.iter().all(|n| cached(info.repo, n)) && cached(VAD_REPO, VAD_FILE)
        }
        ModelKind::Transcription => cached(info.repo, "model.bin"),
    }
}

/// `is_downloaded` in an explicit cache folder.
pub fn is_downloaded_in(cache_dir: &Path, model_id: &str) -> bool {
    is_downloaded_by(model_id, &|repo, name| try_to_load_from_cache_in(cache_dir, repo, name).is_some())
}

/// `is_downloaded(model_id)` (disk only, every part complete, as `models.py` decides). Unknown ids → false.
pub fn is_downloaded(model_id: &str) -> bool {
    if find(model_id).is_none() {
        return false;
    }
    is_downloaded_in(&hub_cache_dir(), model_id)
}

/// `_mb(size) = round(size / MiB)` (Python `round`: half to even).
pub fn mb(size: u64) -> i64 {
    (size as f64 / MIB as f64).round_ties_even() as i64
}

/// `cached_disk_mb(cache_dir)`: repo id → MB on disk (`round(bytes / MiB)` half-even) for every cached repo,
/// plus `"<repo>#<file>"` entries for the files of `owns_files_only` models.
pub fn cached_disk_mb(cache_dir: Option<&Path>) -> BTreeMap<String, i64> {
    cached_disk_mb_with(cache_dir, KATALOG)
}

pub(crate) fn cached_disk_mb_with(cache_dir: Option<&Path>, catalog: &[ModelInfo]) -> BTreeMap<String, i64> {
    let dir = cache_dir.map(Path::to_path_buf).unwrap_or_else(hub_cache_dir);
    let Some(repos) = scan::scan_cache_dir(&dir) else { return BTreeMap::new() };
    sizes_from_scan(&repos, catalog)
}

fn sizes_from_scan(repos: &[CachedRepo], catalog: &[ModelInfo]) -> BTreeMap<String, i64> {
    let mut sizes = BTreeMap::new();
    for repo in repos.iter().filter(|r| r.repo_type == "model") {
        sizes.insert(repo.repo_id.clone(), mb(repo.size_on_disk));
    }
    let mut owned: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for info in catalog.iter().filter(|i| i.owns_files_only) {
        for (repo, files) in info.parts() {
            let entry = owned.entry(repo).or_default();
            // `{repo: set(files)}` built in a dict comprehension: a later entry for the same repo replaces it.
            *entry = files.to_vec();
        }
    }
    for repo in repos.iter().filter(|r| r.repo_type == "model") {
        let Some(names) = owned.get(repo.repo_id.as_str()) else { continue };
        let mut seen: BTreeMap<String, u64> = BTreeMap::new();
        for revision in &repo.revisions {
            for file in &revision.files {
                if names.contains(&file.file_name.as_str()) {
                    seen.insert(file.file_name.clone(), file.size_on_disk);
                }
            }
        }
        for (name, size) in seen {
            sizes.insert(format!("{}#{}", repo.repo_id, name), mb(size));
        }
    }
    sizes
}

/// `disk_mb(model_id, sizes)`: the model's disk usage in MB (owned files only for `owns_files_only`;
/// shared VAD excluded), or `None` if not downloaded. `sizes` = a precomputed [`cached_disk_mb`].
pub fn disk_mb(model_id: &str, sizes: Option<&BTreeMap<String, i64>>) -> Option<i64> {
    disk_mb_by(model_id, sizes, &is_downloaded)
}

/// [`disk_mb`] with an explicit download check (tests).
pub fn disk_mb_by(
    model_id: &str,
    sizes: Option<&BTreeMap<String, i64>>,
    downloaded: &dyn Fn(&str) -> bool,
) -> Option<i64> {
    let info = find(model_id)?;
    if !downloaded(model_id) {
        return None;
    }
    let computed;
    let table = match sizes {
        Some(s) => s,
        None => {
            computed = cached_disk_mb(None);
            &computed
        }
    };
    let found: Vec<Option<i64>> = if info.owns_files_only {
        info.parts()
            .iter()
            .flat_map(|(repo, files)| files.iter().map(move |n| table.get(&format!("{repo}#{n}")).copied()))
            .collect()
    } else {
        info.parts().iter().map(|(repo, _)| table.get(*repo).copied()).collect()
    };
    found.into_iter().sum()
}

/// `delete_model(model_id, cache_dir)`: deletes the model's repo(s) (or only its owned files), sweeps
/// unreferenced shared blobs and returns the freed MB. Localized `ModelError`s: `unknown_model`,
/// `model_not_downloaded_delete`, `model_shared`, `partial_delete`, `delete_incomplete`.
pub fn delete_model(model_id: &str, cache_dir: Option<&Path>) -> Result<i64> {
    let dir = cache_dir.map(Path::to_path_buf).unwrap_or_else(hub_cache_dir);
    crate::delete::delete_model_with(model_id, &dir, &crate::delete::DeleteHooks::default())
}

/// `model_dir(model_id)`: the snapshot folder holding `model.bin` (no network). Errors: `unknown_model`,
/// `model_not_downloaded` (localized `ModelError`, `model=info.id`).
pub fn model_dir(model_id: &str) -> Result<PathBuf> {
    model_dir_in(&hub_cache_dir(), model_id)
}

/// [`model_dir`] in an explicit cache folder.
pub fn model_dir_in(cache_dir: &Path, model_id: &str) -> Result<PathBuf> {
    let info = find(model_id).ok_or_else(|| EngineError::model(se_core::t!("unknown_model", model = model_id)))?;
    let path = try_to_load_from_cache_in(cache_dir, info.repo, "model.bin")
        .ok_or_else(|| EngineError::model(se_core::t!("model_not_downloaded", model = info.id)))?;
    Ok(path.parent().map(Path::to_path_buf).unwrap_or_default())
}

/// `model_file(model_id)`: the cached path of `files[0]` (GGUF models). Errors as [`model_dir`].
pub fn model_file(model_id: &str) -> Result<PathBuf> {
    model_file_in(&hub_cache_dir(), model_id)
}

/// [`model_file`] in an explicit cache folder.
pub fn model_file_in(cache_dir: &Path, model_id: &str) -> Result<PathBuf> {
    let info = find(model_id)
        .filter(|i| !i.files.is_empty())
        .ok_or_else(|| EngineError::model(se_core::t!("unknown_model", model = model_id)))?;
    try_to_load_from_cache_in(cache_dir, info.repo, info.files[0])
        .ok_or_else(|| EngineError::model(se_core::t!("model_not_downloaded", model = info.id)))
}
