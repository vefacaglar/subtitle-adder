//! Test helpers ported from `tests/test_models.py` / `conftest.py`: fake HF cache layouts.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const MIB: usize = 1024 * 1024;
pub const REV1: &str = "1111111111111111111111111111111111111111";
pub const REV2: &str = "2222222222222222222222222222222222222222";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    Symlink,
    Copy,
    CopyDup,
    Auto,
}

pub fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// conftest `symlinks_supported(dir)`: a dangling relative symlink can be created in `dir`.
pub fn symlinks_supported(dir: &Path) -> bool {
    let _ = fs::create_dir_all(dir);
    let probe = dir.join("_symlink_probe");
    match se_models::fsx::symlink_file(Path::new("target"), &probe) {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// conftest `require_symlinks(dir)`: `false` (and a skip notice) when symlinks are unavailable.
pub fn require_symlinks(dir: &Path) -> bool {
    fs::create_dir_all(dir).unwrap();
    if symlinks_supported(dir) {
        true
    } else {
        eprintln!("skipped: symlinks are not available on this machine (Windows without Developer Mode)");
        false
    }
}

/// `repo_folder` name.
pub fn repo_dir(cache: &Path, repo_id: &str) -> PathBuf {
    cache.join(format!("models--{}", repo_id.replace('/', "--")))
}

fn rel_link(target: &Path, link: &Path) {
    let rel = se_models::fsx::relpath(&se_core::paths::abspath(target), &se_core::paths::abspath(link.parent().unwrap())).unwrap();
    se_models::fsx::symlink_file(&rel, link).unwrap();
}

/// `make_cached_repo(cache, repo_id, files, revision, layout)`; returns `None` when the symlink layout was
/// requested but is unavailable (the test is skipped).
pub fn make_cached_repo(cache: &Path, repo_id: &str, files: &[(&str, usize)], revision: &str, layout: Layout) -> Option<PathBuf> {
    fs::create_dir_all(cache).unwrap();
    let layout = match layout {
        Layout::Auto => {
            if symlinks_supported(cache) {
                Layout::Symlink
            } else {
                Layout::Copy
            }
        }
        l => l,
    };
    if layout == Layout::Symlink && !require_symlinks(cache) {
        return None;
    }
    let repo = repo_dir(cache, repo_id);
    let blobs = repo.join("blobs");
    let snapshot = repo.join("snapshots").join(revision);
    fs::create_dir_all(&blobs).unwrap();
    fs::create_dir_all(&snapshot).unwrap();
    for (name, size) in files {
        let blob = blobs.join(sha(&format!("{repo_id}/{revision}/{name}")));
        fs::write(&blob, vec![b'x'; *size]).unwrap();
        let target = snapshot.join(name);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        match layout {
            Layout::Symlink => rel_link(&blob, &target),
            Layout::Copy => fs::rename(&blob, &target).unwrap(),
            _ => {
                fs::copy(&blob, &target).unwrap();
            }
        }
    }
    fs::create_dir_all(repo.join("refs")).unwrap();
    fs::write(repo.join("refs").join("main"), revision).unwrap();
    Some(repo)
}

/// Shorthand: auto layout, revision `1`*40.
pub fn cached(cache: &Path, repo_id: &str, files: &[(&str, usize)]) -> PathBuf {
    make_cached_repo(cache, repo_id, files, REV1, Layout::Auto).unwrap()
}

/// `make_shared_repo(cache, repo_id, {name: (key, size)}, revision)` (xet shared store layout); `None` when
/// symlinks are unavailable.
pub fn make_shared_repo(cache: &Path, repo_id: &str, files: &[(&str, &str, usize)], revision: &str) -> Option<(PathBuf, PathBuf)> {
    if !require_symlinks(cache) {
        return None;
    }
    let store = cache.join("blobs");
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join(".huggingface-shared-blobs"), "1\n").unwrap();
    let repo = repo_dir(cache, repo_id);
    let blobs = repo.join("blobs");
    let snapshot = repo.join("snapshots").join(revision);
    fs::create_dir_all(&blobs).unwrap();
    fs::create_dir_all(&snapshot).unwrap();
    let repo_name = repo.file_name().unwrap().to_string_lossy().to_string();
    for (name, key, size) in files {
        let digest = sha(key);
        let prefix = store.join(&digest[..2]);
        fs::create_dir_all(&prefix).unwrap();
        let payload = prefix.join(&digest);
        fs::write(&payload, vec![b'x'; *size]).unwrap();
        let refs_path = prefix.join(format!("{digest}.refs"));
        let mut refs = fs::read_to_string(&refs_path).unwrap_or_default();
        refs.push_str(&format!("{repo_name}/blobs/{digest}\n"));
        fs::write(&refs_path, refs).unwrap();
        fs::write(prefix.join(format!("{digest}.lock")), b"").unwrap();
        let link = blobs.join(&digest);
        if !se_models::fsx::is_symlink(&link) {
            rel_link(&payload, &link);
        }
        rel_link(&link, &snapshot.join(name));
    }
    fs::create_dir_all(repo.join("refs")).unwrap();
    fs::write(repo.join("refs").join("main"), revision).unwrap();
    Some((repo, store))
}

/// `_payload(store, key)`.
pub fn payload(store: &Path, key: &str) -> PathBuf {
    let d = sha(key);
    store.join(&d[..2]).join(d)
}

/// Every non-directory entry below `dir` (recursive).
pub fn files_below(dir: &Path) -> Vec<PathBuf> {
    se_models::fsx::walk_all(dir).into_iter().filter(|p| !se_models::fsx::is_dir(p)).collect()
}

/// Sorted snapshot entry names of a repo.
pub fn snapshot_names(repo: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        files_below(&repo.join("snapshots")).iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
    v.sort();
    v
}

/// conftest autouse state: Turkish messages, strict offline off, Hub unlocked.
pub fn reset_env() {
    se_core::offline::reset_for_tests();
    // SAFETY: callers are `#[serial]` tests.
    unsafe { std::env::remove_var(se_core::i18n::ENV_VAR) };
    se_core::i18n::set_language(None);
}
