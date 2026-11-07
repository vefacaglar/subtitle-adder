//! Local Hugging Face cache resolution (`snapshot_download(..., local_files_only=True)` layout:
//! `models--org--name/{refs/main, snapshots/<hash>}`), shared with the Python engine's cache.

use std::path::{Path, PathBuf};

use se_core::{EngineError, ErrorKind, Result};

/// `LocalEntryNotFoundError` text for `local_files_only=True`.
pub const LOCAL_ONLY_MESSAGE: &str = "Cannot find an appropriate cached snapshot folder for the specified revision on the local disk and outgoing traffic has been disabled. To enable repo look-ups and downloads online, pass 'local_files_only=False' as input.";

/// `LocalEntryNotFoundError` text when `HF_HUB_OFFLINE` blocks the lookup.
pub const OFFLINE_MESSAGE: &str = "Cannot find an appropriate cached snapshot folder for the specified revision on the local disk and outgoing traffic has been disabled. To enable repo look-ups and downloads online, set 'HF_HUB_OFFLINE=0' as environment variable.";

fn expand(path: &str) -> PathBuf {
    // os.path.expanduser(os.path.expandvars(...)) for the common forms.
    let mut s = path.to_string();
    if s == "~" || s.starts_with("~/") || s.starts_with("~\\") {
        if let Some(home) = home_dir() {
            s = format!("{}{}", home.display(), &s[1..]);
        }
    }
    PathBuf::from(s)
}

fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `huggingface_hub.constants.HF_HUB_CACHE`.
pub fn hub_cache_dir() -> PathBuf {
    if let Some(v) = env_nonempty("HF_HUB_CACHE").or_else(|| env_nonempty("HUGGINGFACE_HUB_CACHE")) {
        return expand(&v);
    }
    let hf_home = env_nonempty("HF_HOME").map(|v| expand(&v)).unwrap_or_else(|| {
        let xdg = env_nonempty("XDG_CACHE_HOME")
            .map(|v| expand(&v))
            .unwrap_or_else(|| home_dir().unwrap_or_default().join(".cache"));
        xdg.join("huggingface")
    });
    hf_home.join("hub")
}

/// `models--org--name` under the cache.
pub fn repo_folder(cache: &Path, repo_id: &str) -> PathBuf {
    cache.join(format!("models--{}", repo_id.replace('/', "--")))
}

/// The cached snapshot folder of `repo_id` at `refs/main`, if present.
pub fn cached_snapshot(repo_id: &str) -> Option<PathBuf> {
    let storage = repo_folder(&hub_cache_dir(), repo_id);
    let commit = std::fs::read_to_string(storage.join("refs").join("main")).ok()?;
    let folder = storage.join("snapshots").join(commit);
    folder.exists().then_some(folder)
}

/// faster-whisper's `download_model` allow patterns.
pub const WHISPER_ALLOW_PATTERNS: [&str; 5] =
    ["config.json", "preprocessor_config.json", "model.bin", "tokenizer.json", "vocabulary.*"];

/// `snapshot_download(repo_id, allow_patterns, local_files_only=...)`: with `local_files_only` only the cache is
/// consulted; otherwise the Hub is asked for `main` and missing files are downloaded (through
/// `se_models::hub`), falling back to the cached snapshot when the Hub cannot be reached. A missing snapshot gives
/// the `LocalEntryNotFoundError` texts.
pub fn snapshot(repo_id: &str, patterns: &[&str], local_files_only: bool) -> Result<PathBuf> {
    let cached = cached_snapshot(repo_id);
    if local_files_only {
        return cached.ok_or_else(|| EngineError::new(ErrorKind::Model, LOCAL_ONLY_MESSAGE));
    }
    if se_core::offline::hub_offline() {
        return cached.ok_or_else(|| EngineError::new(ErrorKind::Model, OFFLINE_MESSAGE));
    }
    use se_models::hub::Hub as _;
    match se_models::hub::HfHub::new().snapshot_download(repo_id, patterns, &mut |_| {}) {
        Ok(dir) => Ok(dir),
        Err(e) if e.is(ErrorKind::Network) => match cached {
            Some(dir) => Ok(dir),
            None => Err(EngineError::new(
                ErrorKind::Model,
                format!(
                    "Got: {e}
An error happened while trying to locate the files on the Hub, and we cannot find the appropriate snapshot folder for the specified revision on the local disk. Please check your internet connection and try again."
                ),
            )),
        },
        Err(e) => Err(e),
    }
}
