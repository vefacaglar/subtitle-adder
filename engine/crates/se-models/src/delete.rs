//! `delete-model`: whole-repo and owned-file deletion, plus the shared (xet) blob store sweep
//! (port of `models.delete_model` & helpers and huggingface_hub `sweep_shared_blob`; spec models.md §4.7).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use se_core::paths::py_path_str;
use se_core::{EngineError, ErrorKind, Result};

use crate::cache::mb;
use crate::catalog::{KATALOG, ModelInfo};
use crate::fsx;
use crate::scan::{self, CachedRepo};

/// Cache-wide shared (xet) blob store folder name.
pub const SHARED_BLOBS_DIR: &str = "blobs";
/// Marker file of the shared store.
pub const SHARED_MARKER: &str = ".huggingface-shared-blobs";

/// Test seams of [`delete_model_with`] (Python tests monkeypatch these).
pub struct DeleteHooks<'a> {
    /// The catalog (`models.KATALOG`).
    pub catalog: &'a [ModelInfo],
    /// `_remove_repo_leftovers(cache_root, repo_path, repo_id)`.
    pub remove_leftovers: &'a dyn Fn(&Path, &Path, &str),
    /// `false` = `_hf_sweep()` unavailable: use `_sweep_locked_fallback`.
    pub hf_sweep: bool,
    /// Replaces `_delete_owned_files` / `_delete_whole_repo` for one repo (tests inject failures):
    /// `Some(result)` short-cuts the real deletion.
    pub delete_override: Option<&'a dyn Fn(&str) -> Option<std::result::Result<u64, DeleteFailure>>>,
}

impl Default for DeleteHooks<'_> {
    fn default() -> Self {
        DeleteHooks { catalog: KATALOG, remove_leftovers: &remove_repo_leftovers, hf_sweep: true, delete_override: None }
    }
}

/// Why deleting one repo failed (`except OSError` / `except ModelError` in `delete_model`).
#[derive(Debug)]
pub enum DeleteFailure {
    /// An `OSError` (its Python `str()`).
    Os(String),
    /// A `ModelError` message.
    Model(String),
}

fn os_text(err: &io::Error, path: &Path) -> DeleteFailure {
    DeleteFailure::Os(EngineError::win(err, Some(path)).message)
}

/// `delete_model(model_id, cache_dir)` with injectable hooks.
pub fn delete_model_with(model_id: &str, cache_dir: &Path, hooks: &DeleteHooks<'_>) -> Result<i64> {
    let Some(info) = hooks.catalog.iter().find(|m| m.id == model_id) else {
        return Err(EngineError::model(se_core::t!("unknown_model", model = model_id)));
    };
    let mine: Vec<&str> = info.parts().iter().map(|(r, _)| *r).collect();
    let shared: Vec<&str> = hooks
        .catalog
        .iter()
        .filter(|o| o.id != info.id && o.parts().iter().any(|(r, _)| mine.contains(r)))
        .map(|o| o.id)
        .collect();
    if !shared.is_empty() {
        return Err(EngineError::model(se_core::t!("model_shared", model = info.id, others = shared.join(", "))));
    }

    let root = se_core::paths::resolve(Path::new(&fsx::expanduser(&py_path_str(cache_dir))));
    let cache = scan::scan_cache_dir(&root);
    let mut repos: Vec<(&str, &[&str], &CachedRepo)> = Vec::new();
    if let Some(found) = &cache {
        for (repo_id, files) in info.parts() {
            for repo in found.iter().filter(|r| r.repo_type == "model" && r.repo_id == repo_id) {
                repos.push((repo_id, files, repo));
            }
        }
    }
    if repos.is_empty() {
        return Err(EngineError::model(se_core::t!("model_not_downloaded_delete", model = info.id)));
    }

    let mut freed: u64 = 0;
    let mut leftovers: Vec<String> = Vec::new();
    for (repo_id, files, repo) in repos {
        let result = match hooks.delete_override.and_then(|f| f(repo_id)) {
            Some(r) => r,
            None if info.owns_files_only => delete_owned_files(&root, repo, repo_id, files, hooks),
            None => delete_whole_repo(&root, repo, repo_id, hooks),
        };
        match result {
            Ok(n) => freed += n,
            Err(DeleteFailure::Os(text)) => leftovers.push(format!("{} ({text})", py_path_str(&repo.repo_path))),
            Err(DeleteFailure::Model(text)) => leftovers.push(text),
        }
    }
    if !leftovers.is_empty() {
        return Err(EngineError::model(se_core::t!(
            "partial_delete",
            name = se_core::i18n::display_name(info.id),
            freed = mb(freed),
            left = leftovers.join("; ")
        )));
    }
    Ok(mb(freed))
}

fn incomplete(repo_path: &Path) -> DeleteFailure {
    DeleteFailure::Model(se_core::t!("delete_incomplete", path = py_path_str(repo_path)))
}

fn delete_whole_repo(root: &Path, repo: &CachedRepo, repo_id: &str, hooks: &DeleteHooks<'_>) -> std::result::Result<u64, DeleteFailure> {
    let repo_path = &repo.repo_path;
    let (local, targets) = inspect_repo_blobs(repo_path, root);
    (hooks.remove_leftovers)(root, repo_path, repo_id);
    if fsx::exists(repo_path) {
        return Err(incomplete(repo_path));
    }
    Ok(local + sweep_shared_blobs(root, &targets, hooks.hf_sweep))
}

fn delete_owned_files(
    root: &Path,
    repo: &CachedRepo,
    repo_id: &str,
    names: &[&str],
    hooks: &DeleteHooks<'_>,
) -> std::result::Result<u64, DeleteFailure> {
    let repo_path = &repo.repo_path;
    let all: Vec<&PathBuf> = repo.revisions.iter().flat_map(|r| r.files.iter().map(|f| &f.file_path)).collect();
    let doomed: Vec<PathBuf> = repo
        .revisions
        .iter()
        .flat_map(|r| r.files.iter())
        .filter(|f| names.contains(&f.file_name.as_str()))
        .map(|f| f.file_path.clone())
        .collect();
    let doomed_keys: Vec<String> = doomed.iter().map(|p| fsx::path_key(p)).collect();
    let kept_blobs: Vec<String> = all
        .iter()
        .filter(|p| !doomed_keys.contains(&fsx::path_key(p)))
        .map(|p| fsx::path_key(&entry_blob(p)))
        .collect();
    let mut freed = 0u64;
    let mut targets: Vec<PathBuf> = Vec::new();
    for link in &doomed {
        let blob = entry_blob(link);
        let own_data = fsx::same_path(&blob, link);
        if own_data {
            if let Ok(m) = fs::metadata(link) {
                freed += m.len();
            }
        }
        fsx::unlink_missing_ok(link).map_err(|e| os_text(&e, link))?;
        if own_data || kept_blobs.contains(&fsx::path_key(&blob)) {
            continue;
        }
        match shared_target(&blob, root) {
            Some(t) => push_unique(&mut targets, t),
            None => {
                if let Ok(m) = fs::metadata(&blob) {
                    freed += m.len();
                }
            }
        }
        fsx::unlink_missing_ok(&blob).map_err(|e| os_text(&e, &blob))?;
    }
    let remaining = fsx::exists(repo_path)
        && fsx::walk_all(&repo_path.join("snapshots")).iter().any(|p| !fsx::is_dir(p));
    if !remaining {
        let (local, more) = inspect_repo_blobs(repo_path, root);
        (hooks.remove_leftovers)(root, repo_path, repo_id);
        if fsx::exists(repo_path) {
            return Err(incomplete(repo_path));
        }
        freed += local;
        for t in more {
            push_unique(&mut targets, t);
        }
    }
    Ok(freed + sweep_shared_blobs(root, &targets, hooks.hf_sweep))
}

fn push_unique(v: &mut Vec<PathBuf>, p: PathBuf) {
    if !v.iter().any(|q| fsx::same_path(q, &p)) {
        v.push(p);
    }
}

/// `_entry_blob(link)`: one symlink hop (lexically normalized), or the entry itself.
pub fn entry_blob(link: &Path) -> PathBuf {
    if fsx::is_symlink(link) {
        fsx::link_target(link).unwrap_or_else(|_| link.to_path_buf())
    } else {
        link.to_path_buf()
    }
}

fn is_hash64(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `_shared_target(link, root)`: the store payload `<root>/blobs/<2 hex>/<sha256>` a symlink points to.
pub fn shared_target(link: &Path, root: &Path) -> Option<PathBuf> {
    if !fsx::is_symlink(link) {
        return None;
    }
    let target = fsx::link_target(link).ok()?;
    let name = scan::file_name(&target);
    let parent = target.parent()?;
    let grand = parent.parent()?;
    if !fsx::same_path(grand, &root.join(SHARED_BLOBS_DIR)) || !is_hash64(&name) || scan::file_name(parent) != name[..2] {
        return None;
    }
    Some(target)
}

/// `_inspect_repo_blobs(repo_path, root)`: (bytes of real files owned by the repo, shared-store targets).
pub fn inspect_repo_blobs(repo_path: &Path, root: &Path) -> (u64, Vec<PathBuf>) {
    let mut local = 0u64;
    let mut targets = Vec::new();
    for entry in fsx::walk_all(&repo_path.join("snapshots")) {
        // os.walk(followlinks=False): "files" are the entries that are not directories (following links).
        if fsx::is_dir(&entry) {
            continue;
        }
        if !fsx::is_symlink(&entry) {
            if let Ok(m) = fs::metadata(&entry) {
                local += m.len();
            }
        }
    }
    let entries: Vec<PathBuf> = match fs::read_dir(repo_path.join("blobs")) {
        Ok(rd) => match rd.map(|e| e.map(|e| e.path())).collect::<io::Result<Vec<_>>>() {
            Ok(v) => v,
            Err(_) => return (0, targets),
        },
        Err(_) => return (0, targets),
    };
    for entry in entries {
        if let Some(t) = shared_target(&entry, root) {
            push_unique(&mut targets, t);
            continue;
        }
        if fsx::is_file(&entry) && !fsx::is_symlink(&entry) {
            if let Ok(m) = fs::metadata(&entry) {
                local += m.len();
            }
        }
    }
    (local, targets)
}

/// `_referenced_shared_blobs(root)`: every store target linked from any `*--*/blobs/` folder; `None` when a
/// folder cannot be read (then nothing is deleted).
pub fn referenced_shared_blobs(root: &Path) -> Option<Vec<PathBuf>> {
    let mut referenced = Vec::new();
    let mut dirs = Vec::new();
    for entry in fs::read_dir(root).ok()? {
        let p = entry.ok()?.path();
        let name = scan::file_name(&p);
        if name != SHARED_BLOBS_DIR && fsx::is_dir_strict(&p).ok()? && name.contains("--") {
            dirs.push(p);
        }
    }
    for repo_dir in dirs {
        let blobs = repo_dir.join("blobs");
        if !fsx::is_dir_strict(&blobs).ok()? {
            continue;
        }
        for entry in fs::read_dir(&blobs).ok()? {
            let p = entry.ok()?.path();
            if let Some(t) = shared_target(&p, root) {
                push_unique(&mut referenced, t);
            }
        }
    }
    Some(referenced)
}

/// `_marker_ok(root)`.
pub fn marker_ok(root: &Path) -> bool {
    scan::marker_text_ok(&root.join(SHARED_BLOBS_DIR).join(SHARED_MARKER))
}

/// `_sweep_shared_blobs(root, candidates)`: deletes the candidates no other repo links; returns the bytes.
pub fn sweep_shared_blobs(root: &Path, candidates: &[PathBuf], hf_sweep: bool) -> u64 {
    if candidates.is_empty() || !marker_ok(root) {
        return 0;
    }
    let Some(referenced) = referenced_shared_blobs(root) else { return 0 };
    let mut freed = 0;
    for store_path in candidates {
        if referenced.iter().any(|r| fsx::same_path(r, store_path)) {
            continue;
        }
        let result = if hf_sweep { sweep_shared_blob(store_path, root) } else { sweep_locked_fallback(root, store_path) };
        if let Ok(n) = result {
            freed += n;
        }
    }
    freed
}

fn sibling(store_path: &Path, suffix: &str) -> PathBuf {
    store_path.with_file_name(format!("{}{suffix}", scan::file_name(store_path)))
}

/// `_sweep_locked_fallback(root, store_path)`.
fn sweep_locked_fallback(root: &Path, store_path: &Path) -> io::Result<u64> {
    let _lock = fsx::FileLock::acquire(&sibling(store_path, ".lock"))?;
    let Some(referenced) = referenced_shared_blobs(root) else { return Ok(0) };
    if referenced.iter().any(|r| fsx::same_path(r, store_path)) {
        return Ok(0);
    }
    if fsx::is_symlink(store_path) || !fsx::is_file(store_path) {
        return Ok(0);
    }
    let size = fs::metadata(store_path)?.len();
    fs::remove_file(store_path)?;
    fsx::unlink_missing_ok(&sibling(store_path, ".refs"))?;
    Ok(size)
}

/// `_path_for_comparison`: absolute, without the Windows extended-length prefix.
fn comparable(path: &Path) -> PathBuf {
    let s = py_path_str(path);
    let s = if s.len() >= 8 && s[..8].eq_ignore_ascii_case("\\\\?\\unc\\") {
        format!("\\\\{}", &s[8..])
    } else if let Some(rest) = s.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        s
    };
    se_core::paths::abspath(Path::new(&s))
}

const REPO_PREFIXES: &[&str] = &["datasets", "kernels", "models", "spaces"];

/// `_relative_blob_path(blob_path, cache_dir)`: `"<type>--<name>/blobs/<file>"` or `None`.
fn relative_blob_path(blob_path: &Path, cache_dir: &Path) -> Option<String> {
    let parts = fsx::relative_parts(&comparable(blob_path), &comparable(cache_dir))?;
    if parts.len() != 3 || parts[1] != "blobs" || parts[2].is_empty() {
        return None;
    }
    let ok_repo = REPO_PREFIXES.iter().any(|p| parts[0].len() > p.len() + 2 && parts[0].starts_with(&format!("{p}--")));
    if !ok_repo {
        return None;
    }
    let rel = parts.join("/");
    (!rel.contains('\n') && !rel.contains('\r')).then_some(rel)
}

/// huggingface_hub `_shared_blob_target(blob_path, cache_dir, raise_on_io_error)`.
fn hf_shared_blob_target(blob_path: &Path, cache_dir: &Path, raise_on_io_error: bool) -> io::Result<Option<PathBuf>> {
    relative_blob_path(blob_path, cache_dir).map_or(Ok(None), |_| {
        let meta = match fs::symlink_metadata(blob_path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return if raise_on_io_error { Err(e) } else { Ok(None) },
        };
        if !meta.file_type().is_symlink() {
            return Ok(None);
        }
        let link_target = match fsx::read_link(blob_path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return if raise_on_io_error { Err(e) } else { Ok(None) },
        };
        let comparable_blob = comparable(blob_path);
        let target = if link_target.is_absolute() {
            link_target
        } else {
            comparable_blob.parent().unwrap_or(Path::new("")).join(link_target)
        };
        let target = comparable(&target);
        let store_dir = comparable(&cache_dir.join(SHARED_BLOBS_DIR));
        let Some(rel) = fsx::relative_parts(&target, &store_dir) else { return Ok(None) };
        if rel.len() != 2 || !is_hash64(&rel[1]) || rel[0] != rel[1][..2] {
            return Ok(None);
        }
        Ok(Some(cache_dir.join(SHARED_BLOBS_DIR).join(&rel[0]).join(&rel[1])))
    })
}

/// `_read_valid_manifest_references(store_path, cache_dir)`.
fn read_valid_manifest_references(store_path: &Path, cache_dir: &Path) -> Option<Vec<PathBuf>> {
    let manifest = sibling(store_path, ".refs");
    if !fsx::is_regular_nofollow(&manifest) {
        return None;
    }
    let bytes = fs::read(&manifest).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let cache_dir = se_core::paths::abspath(cache_dir);
    let mut refs: Vec<PathBuf> = Vec::new();
    for line in py_splitlines(text) {
        let rel = Path::new(line);
        if rel.has_root() || rel.is_absolute() || rel.components().any(|c| c.as_os_str() == "..") {
            continue;
        }
        let candidate = cache_dir.join(rel);
        if relative_blob_path(&candidate, &cache_dir).is_none() {
            continue;
        }
        let target = hf_shared_blob_target(&candidate, &cache_dir, true).ok()?;
        if let Some(t) = target {
            if fsx::same_path(&t, store_path) && !refs.iter().any(|r| fsx::same_path(r, &candidate)) {
                refs.push(candidate);
            }
        }
    }
    Some(refs)
}

/// Python `str.splitlines()` (the line boundaries relevant for text files).
fn py_splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i < bytes.len() {
        let (pos, c) = bytes[i];
        let boundary = matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if boundary {
            out.push(&text[start..pos]);
            let mut next = pos + c.len_utf8();
            if c == '\r' && i + 1 < bytes.len() && bytes[i + 1].1 == '\n' {
                next += 1;
                i += 1;
            }
            start = next;
        }
        i += 1;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// `_rewrite_manifest(manifest_path, references, cache_dir)`.
fn rewrite_manifest(manifest: &Path, refs: &[PathBuf], cache_dir: &Path) -> io::Result<()> {
    let tmp = manifest.with_file_name(format!("{}.{}.tmp", scan::file_name(manifest), fsx::rand_hex8()));
    let cache_dir = se_core::paths::abspath(cache_dir);
    let mut rels: Vec<String> = refs
        .iter()
        .filter_map(|p| fsx::relative_parts(p, &cache_dir).map(|parts| parts.join("/")))
        .collect();
    rels.sort();
    let content: String = rels.iter().map(|r| format!("{r}\n")).collect();
    let result = (|| {
        fs::write(&tmp, content.as_bytes())?;
        fs::File::open(&tmp)?.sync_all()?;
        fs::rename(&tmp, manifest)
    })();
    let _ = fsx::unlink_missing_ok(&tmp);
    result
}

/// huggingface_hub `sweep_shared_blob(store_path, cache_dir)`: under the payload's lock, deletes it (and its
/// manifest) when its manifest lists no valid reference; rewrites the manifest otherwise. Missing or
/// unreadable metadata never leads to deletion. Returns the freed bytes.
pub fn sweep_shared_blob(store_path: &Path, cache_dir: &Path) -> io::Result<u64> {
    let store_path = se_core::paths::abspath(store_path);
    let cache_dir = se_core::paths::abspath(cache_dir);
    let store_dir = se_core::paths::abspath(&cache_dir.join(SHARED_BLOBS_DIR));
    let Some(rel) = fsx::relative_parts(&store_path, &store_dir) else { return Ok(0) };
    let name = scan::file_name(&store_path);
    let parent_name = store_path.parent().map(scan::file_name).unwrap_or_default();
    if !scan::is_shared_blobs_dir(&store_dir) || rel.len() != 2 || !is_hash64(&name) || parent_name != name[..2] {
        return Ok(0);
    }
    Ok((|| -> io::Result<u64> {
        let lock_path = sibling(&store_path, ".lock");
        {
            let f = fs::OpenOptions::new().write(true).create(true).truncate(false).open(&lock_path)?;
            if !f.metadata()?.is_file() {
                return Err(io::Error::other(format!("Shared blob lock is not a regular file: '{}'.", py_path_str(&lock_path))));
            }
        }
        let _lock = fsx::FileLock::acquire(&lock_path)?;
        let Some(refs) = read_valid_manifest_references(&store_path, &cache_dir) else { return Ok(0) };
        let manifest = sibling(&store_path, ".refs");
        if !refs.is_empty() {
            rewrite_manifest(&manifest, &refs, &cache_dir)?;
            return Ok(0);
        }
        if !fsx::is_regular_nofollow(&store_path) {
            return Ok(0);
        }
        let size = fs::symlink_metadata(&store_path)?.len();
        fs::remove_file(&store_path)?;
        fsx::unlink_missing_ok(&manifest)?;
        Ok(size)
    })()
    .unwrap_or(0))
}

/// `_remove_repo_leftovers(cache_root, repo_path, repo_id)`: removes the repo folder (only when it is named for
/// `repo_id` and resolves inside the cache), retrying up to 5 times (0.1 s .. 0.5 s apart).
pub fn remove_repo_leftovers(cache_root: &Path, repo_path: &Path, repo_id: &str) {
    let expected = format!("models--{}", repo_id.replace('/', "--"));
    if scan::file_name(repo_path) != expected || !fsx::exists(repo_path) {
        return;
    }
    let resolved = se_core::paths::resolve(repo_path);
    if !fsx::is_relative_to(&resolved, cache_root) {
        return;
    }
    for attempt in 0..5u64 {
        fsx::rmtree_ignore_errors(repo_path);
        if !fsx::exists(repo_path) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
    }
}

/// Is `err` a "not downloaded" deletion error (helper for callers)?
pub fn is_model_error(err: &EngineError) -> bool {
    err.kind == ErrorKind::Model
}

