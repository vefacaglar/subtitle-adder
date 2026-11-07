//! Port of huggingface_hub's `scan_cache_dir` (2.4.0): the parts `cached_disk_mb` and `delete_model` need,
//! including which repos count as corrupted (invisible) and which errors abort the whole scan.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::fsx;

/// Names skipped everywhere (`FILES_TO_IGNORE`).
pub const FILES_TO_IGNORE: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];
/// Repo folder prefixes (`REPO_TYPES_MAPPING`).
const REPO_TYPES: &[(&str, &str)] = &[("datasets", "dataset"), ("kernels", "kernel"), ("models", "model"), ("spaces", "space")];

/// `CachedFileInfo` (the fields the engine uses).
#[derive(Clone, Debug)]
pub struct CachedFile {
    /// Basename of the snapshot entry.
    pub file_name: String,
    /// The snapshot entry (symlink or real file).
    pub file_path: PathBuf,
    /// One symlink hop from `file_path` (`abspath`, not resolved), or `file_path` itself.
    pub blob_path: PathBuf,
    pub size_on_disk: u64,
}

/// `CachedRevisionInfo`.
#[derive(Clone, Debug)]
pub struct CachedRevision {
    pub commit_hash: String,
    pub files: Vec<CachedFile>,
    pub size_on_disk: u64,
}

/// `CachedRepoInfo`.
#[derive(Clone, Debug)]
pub struct CachedRepo {
    pub repo_id: String,
    /// `"model"`, `"dataset"`, `"space"`, `"kernel"`.
    pub repo_type: String,
    pub repo_path: PathBuf,
    pub size_on_disk: u64,
    pub revisions: Vec<CachedRevision>,
}

enum Failure {
    /// `CorruptedCacheException`: the repo is skipped.
    Corrupted,
    /// Any other exception: the whole scan fails (the engine then sees "no cache").
    Fatal,
}

/// `models._scan_cache(cache_dir)`: `None` when the folder is missing, a file, or any non-"corrupted" error
/// happens while scanning (as `scan_cache_dir` raising `CacheNotFound`/`OSError`/`ValueError`).
pub fn scan_cache_dir(cache_dir: &Path) -> Option<Vec<CachedRepo>> {
    let root = se_core::paths::resolve(Path::new(&fsx::expanduser(&se_core::paths::py_path_str(cache_dir))));
    if !fsx::exists(&root) || fsx::is_file(&root) {
        return None;
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(&root).ok()?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>().ok()?;
    entries.sort();
    let mut repos = Vec::new();
    for repo_path in entries {
        let name = file_name(&repo_path);
        if FILES_TO_IGNORE.contains(&name.as_str()) || name == ".locks" || name == "CACHEDIR.TAG" {
            continue;
        }
        if name == "blobs" && is_shared_blobs_dir(&repo_path) {
            continue;
        }
        match scan_repo(&repo_path) {
            Ok(repo) => repos.push(repo),
            Err(Failure::Corrupted) => {}
            Err(Failure::Fatal) => return None,
        }
    }
    Some(repos)
}

pub(crate) fn file_name(p: &Path) -> String {
    p.file_name().map(|n| se_core::paths::os_to_py_string(n)).unwrap_or_default()
}

/// huggingface_hub `is_shared_blobs_dir`: a real directory with a regular marker file whose text is `"1\n"`.
pub fn is_shared_blobs_dir(store: &Path) -> bool {
    fsx::is_dir_nofollow(store) && marker_text_ok(&store.join(crate::delete::SHARED_MARKER))
}

/// The marker is a regular file (lstat) and its text (universal newlines) is exactly `"1\n"`.
pub(crate) fn marker_text_ok(marker: &Path) -> bool {
    if !fsx::is_regular_nofollow(marker) {
        return false;
    }
    match fs::read(marker) {
        Ok(bytes) => match std::str::from_utf8(&bytes) {
            Ok(s) => se_core::pyfmt::universal_newlines(s) == "1\n",
            Err(_) => false,
        },
        Err(_) => false,
    }
}

fn os_fail<T>(r: std::io::Result<T>) -> Result<T, Failure> {
    r.map_err(|_| Failure::Fatal)
}

fn scan_repo(repo_path: &Path) -> Result<CachedRepo, Failure> {
    if !fsx::is_dir(repo_path) {
        return Err(Failure::Corrupted);
    }
    let name = file_name(repo_path);
    let Some((prefix, rest)) = name.split_once("--") else { return Err(Failure::Corrupted) };
    let repo_id = rest.replace("--", "/");
    let Some((_, repo_type)) = REPO_TYPES.iter().find(|(p, _)| *p == prefix) else { return Err(Failure::Corrupted) };

    let snapshots = repo_path.join("snapshots");
    let refs = repo_path.join("refs");
    if !fsx::exists(&snapshots) || !fsx::is_dir(&snapshots) {
        return Err(Failure::Corrupted);
    }

    let mut refs_by_hash: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if fsx::exists(&refs) {
        if fsx::is_file(&refs) {
            return Err(Failure::Corrupted);
        }
        for ref_path in fsx::walk_all(&refs) {
            if fsx::is_dir(&ref_path) || FILES_TO_IGNORE.contains(&file_name(&ref_path).as_str()) {
                continue;
            }
            let bytes = os_fail(fs::read(&ref_path))?;
            // Text mode: a decoding error is a ValueError (aborts the scan); newlines are translated.
            let text = std::str::from_utf8(&bytes).map_err(|_| Failure::Fatal)?;
            let commit = se_core::pyfmt::universal_newlines(text);
            refs_by_hash.entry(commit).or_default().push(file_name(&ref_path));
        }
    }

    let mut blob_sizes: HashMap<String, u64> = HashMap::new();
    let mut revisions = Vec::new();
    let mut revision_dirs: Vec<PathBuf> =
        os_fail(fs::read_dir(&snapshots))?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>().map_err(|_| Failure::Fatal)?;
    revision_dirs.sort();
    for revision_path in revision_dirs {
        let rev_name = file_name(&revision_path);
        if FILES_TO_IGNORE.contains(&rev_name.as_str()) {
            continue;
        }
        if fsx::is_file(&revision_path) {
            return Err(Failure::Corrupted);
        }
        let mut files = Vec::new();
        let mut rev_blobs: Vec<String> = Vec::new();
        for file_path in fsx::walk_all(&revision_path) {
            if fsx::is_dir(&file_path) {
                continue;
            }
            let blob_path = if fsx::is_symlink(&file_path) {
                let target = os_fail(fsx::read_link(&file_path))?;
                se_core::paths::abspath(&file_path.parent().unwrap_or(Path::new("")).join(target))
            } else {
                file_path.clone()
            };
            if !fsx::exists(&blob_path) {
                return Err(Failure::Corrupted);
            }
            let key = fsx::path_key(&blob_path);
            let size = match blob_sizes.get(&key) {
                Some(s) => *s,
                None => {
                    let s = os_fail(fs::metadata(&blob_path))?.len();
                    blob_sizes.insert(key.clone(), s);
                    s
                }
            };
            if !rev_blobs.contains(&key) {
                rev_blobs.push(key);
            }
            files.push(CachedFile { file_name: file_name(&file_path), file_path, blob_path, size_on_disk: size });
        }
        let size_on_disk = rev_blobs.iter().map(|k| blob_sizes[k]).sum();
        refs_by_hash.remove(&rev_name);
        revisions.push(CachedRevision { commit_hash: rev_name, files, size_on_disk });
    }
    if !refs_by_hash.is_empty() {
        return Err(Failure::Corrupted);
    }
    Ok(CachedRepo {
        repo_id,
        repo_type: repo_type.to_string(),
        repo_path: repo_path.to_path_buf(),
        size_on_disk: blob_sizes.values().sum(),
        revisions,
    })
}
