//! A minimal Hugging Face Hub client that writes the huggingface_hub 2.4.0 cache layout (spec models.md §3.4):
//! `refs/main`, `blobs/<etag>` (+ `<etag>.<8hex>.incomplete` while downloading), `snapshots/<commit>/<file>`
//! as relative symlinks or, where symlinks are unavailable, the moved/copied file itself, `.locks/` and
//! `CACHEDIR.TAG`. Plain HTTP downloads are never published to the shared xet store.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use se_core::json::Value;
use se_core::paths::py_path_str;
use se_core::{EngineError, ErrorKind, Result};

use crate::cache::{hf_endpoint, hf_token, repo_folder, symlinks_disabled};
use crate::fnmatch::filter_repo_objects;
use crate::fsx;
use crate::http::{CHUNK_SIZE, error_text, read_full, user_agent};

/// The Hub operations `ensure_model` needs; tests replace them (Python monkeypatches `snapshot_download`
/// and `_repo_bytes`).
pub trait Hub {
    /// `_repo_bytes(repo, patterns, fallback)`: total size of the matching files (`?blobs=true` model info),
    /// `fallback` on any error or a zero total.
    fn repo_bytes(&self, repo: &str, patterns: &[&str], fallback: u64) -> u64;
    /// `snapshot_download(repo, allow_patterns=patterns)`: downloads the missing matching files of `main`,
    /// calling `on_bytes(bytes written so far for this repo)` after every chunk; returns the snapshot folder.
    fn snapshot_download(&self, repo: &str, patterns: &[&str], on_bytes: &mut dyn FnMut(u64)) -> Result<PathBuf>;
}

/// The real Hub client writing into `cache_dir`.
pub struct HfHub {
    pub cache_dir: PathBuf,
    pub endpoint: String,
}

impl HfHub {
    /// The client for the default cache (`HF_HUB_CACHE`) and endpoint (`HF_ENDPOINT`).
    pub fn new() -> Self {
        HfHub { cache_dir: crate::cache::hub_cache_dir(), endpoint: hf_endpoint() }
    }

    fn agent(&self, timeout: Duration, redirects: u32) -> ureq::Agent {
        crate::http::config_builder()
            .user_agent(user_agent())
            .accept_encoding("identity")
            .timeout_connect(Some(timeout))
            .timeout_recv_response(Some(timeout))
            .timeout_per_read(Some(Duration::from_secs(60)))
            .max_redirects(redirects)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .build()
            .new_agent()
    }

    fn check_online(&self, url: &str) -> Result<()> {
        if se_core::offline::hub_offline() {
            return Err(EngineError::new(ErrorKind::Network, se_core::offline::hub_offline_message(url)));
        }
        Ok(())
    }

    fn get_json(&self, url: &str) -> Result<Value> {
        self.check_online(url)?;
        let mut req = self.agent(Duration::from_secs(10), 10).get(url);
        if let Some(token) = hf_token() {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let response = req.call().map_err(|e| net(error_text(&e)))?;
        let status = response.status().as_u16();
        let mut body = String::new();
        response.into_body().into_reader().read_to_string(&mut body).map_err(|e| net(e.to_string()))?;
        if status >= 400 {
            return Err(net(format!("{status} Client Error for url: {url}")));
        }
        se_core::json::parse(&body).map_err(|e| net(e.to_string()))
    }

    /// `GET /api/models/{repo}/revision/main`: (commit sha, sibling file names).
    fn revision_info(&self, repo: &str) -> Result<(String, Vec<String>)> {
        let url = format!("{}/api/models/{repo}/revision/main", self.endpoint);
        let v = self.get_json(&url)?;
        let sha = v.get("sha").and_then(Value::as_str).ok_or_else(|| net("Repo info returned from server must have a revision sha.".into()))?;
        let files = siblings(&v).into_iter().map(|(n, _)| n).collect();
        Ok((sha.to_string(), files))
    }

    /// `HEAD {endpoint}/{repo}/resolve/{commit}/{file}` following only relative redirects.
    fn file_metadata(&self, repo: &str, commit: &str, file: &str) -> Result<FileMeta> {
        let mut url = format!("{}/{repo}/resolve/{commit}/{}", self.endpoint, quote_path(file));
        self.check_online(&url)?;
        let agent = self.agent(Duration::from_secs(etag_timeout()), 0);
        let mut retried = false;
        for _ in 0..10 {
            let mut req = agent.head(&url);
            if let Some(token) = hf_token() {
                req = req.header("Authorization", format!("Bearer {token}"));
            }
            let response = match req.call() {
                Ok(r) => r,
                Err(e) if !retried => {
                    let _ = e;
                    retried = true;
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
                Err(e) => return Err(net(error_text(&e))),
            };
            let status = response.status().as_u16();
            let header = |k: &str| response.headers().get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
            if (500..600).contains(&status) && !retried {
                retried = true;
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            let is_redirect = (300..400).contains(&status);
            if is_redirect {
                if let Some(loc) = header("Location") {
                    if loc.starts_with('/') {
                        // Relative redirect (repo renamed): follow it on the Hub.
                        url = format!("{}{}", origin(&url), loc);
                        continue;
                    }
                }
            }
            if status >= 400 {
                return Err(net(format!("{status} Client Error for url: {url}")));
            }
            let etag = header("X-Linked-Etag").or_else(|| header("ETag")).map(|e| normalize_etag(&e));
            let size = header("X-Linked-Size")
                .or_else(|| if is_redirect { None } else { header("Content-Length") })
                .and_then(|s| s.trim().parse::<u64>().ok());
            let location = header("Location").unwrap_or_else(|| url.clone());
            let commit_hash = header("X-Repo-Commit");
            let etag = etag.ok_or_else(|| net("Distant resource does not have an ETag, we won't be able to reliably ensure reproducibility.".into()))?;
            return Ok(FileMeta { etag, size, location, commit_hash });
        }
        Err(net(format!("Too many redirects for url: {url}")))
    }

    /// `hf_hub_download(repo, file, revision=commit)` into the cache.
    fn download_file(&self, repo: &str, commit: &str, file: &str, counter: &mut dyn FnMut(u64)) -> Result<PathBuf> {
        let storage = repo_folder(&self.cache_dir, repo);
        let relative: PathBuf = file.split('/').collect();
        let pointer = storage.join("snapshots").join(commit).join(&relative);
        if fsx::exists(&pointer) {
            return Ok(pointer);
        }
        let meta = self.file_metadata(repo, commit, file)?;
        let blob = storage.join("blobs").join(&meta.etag);
        let io = |e: std::io::Error, p: &Path| EngineError::win(&e, Some(p));
        fs::create_dir_all(storage.join("blobs")).map_err(|e| io(e, &storage))?;
        if let Some(parent) = pointer.parent() {
            fs::create_dir_all(parent).map_err(|e| io(e, parent))?;
        }
        create_cachedir_tag(&self.cache_dir);
        let lock_path = self.cache_dir.join(".locks").join(scan_name(&storage)).join(format!("{}.lock", meta.etag));
        let _lock = fsx::FileLock::acquire(&lock_path).map_err(|e| io(e, &lock_path))?;
        if fsx::exists(&blob) {
            if !fsx::exists(&pointer) {
                create_pointer(&blob, &pointer, false).map_err(|e| io(e, &pointer))?;
            }
            return Ok(pointer);
        }
        let tmp = storage.join("blobs").join(format!("{}.{}.incomplete", meta.etag, fsx::rand_hex8()));
        let result = self.fetch(&meta, &tmp, counter);
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = fs::rename(&tmp, &blob) {
            let _ = fs::remove_file(&tmp);
            return Err(io(e, &blob));
        }
        if !fsx::exists(&pointer) {
            create_pointer(&blob, &pointer, true).map_err(|e| io(e, &pointer))?;
        }
        Ok(pointer)
    }

    fn fetch(&self, meta: &FileMeta, tmp: &Path, counter: &mut dyn FnMut(u64)) -> Result<()> {
        self.check_online(&meta.location)?;
        let agent = self.agent(Duration::from_secs(10), 10);
        let mut req = agent.get(&meta.location);
        if meta.location.starts_with(&self.endpoint) {
            if let Some(token) = hf_token() {
                req = req.header("Authorization", format!("Bearer {token}"));
            }
        }
        let response = req.call().map_err(|e| net(error_text(&e)))?;
        let status = response.status().as_u16();
        if status >= 400 {
            return Err(net(format!("{status} Client Error for url: {}", meta.location)));
        }
        let mut reader = response.into_body().into_reader();
        let mut out = fs::File::create(tmp).map_err(|e| EngineError::os(&e, Some(tmp)))?;
        let mut buf = vec![0u8; CHUNK_SIZE];
        let mut written = 0u64;
        loop {
            let n = read_full(&mut reader, &mut buf).map_err(|e| net(e.to_string()))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| EngineError::os(&e, Some(tmp)))?;
            written += n as u64;
            counter(n as u64);
        }
        out.flush().map_err(|e| EngineError::os(&e, Some(tmp)))?;
        if let Some(size) = meta.size {
            if size != written {
                return Err(EngineError::new(
                    ErrorKind::Os,
                    format!("Consistency check failed: file should be of size {size} but has size {written} ({}).", py_path_str(tmp)),
                ));
            }
        }
        Ok(())
    }
}

impl Default for HfHub {
    fn default() -> Self {
        Self::new()
    }
}

fn scan_name(storage: &Path) -> String {
    crate::scan::file_name(storage)
}

fn net(message: String) -> EngineError {
    EngineError::new(ErrorKind::Network, message)
}

fn etag_timeout() -> u64 {
    std::env::var("HF_HUB_ETAG_TIMEOUT").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(10)
}

fn origin(url: &str) -> String {
    match url.find("://") {
        Some(i) => match url[i + 3..].find('/') {
            Some(j) => url[..i + 3 + j].to_string(),
            None => url.to_string(),
        },
        None => url.to_string(),
    }
}

/// `urllib.parse.quote(filename)` (safe `/`).
fn quote_path(file: &str) -> String {
    let mut out = String::new();
    for b in file.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `_normalize_etag`: `etag.lstrip("W/").strip('"')`.
pub fn normalize_etag(etag: &str) -> String {
    etag.trim_start_matches(['W', '/']).trim_matches('"').to_string()
}

struct FileMeta {
    etag: String,
    size: Option<u64>,
    location: String,
    #[allow(dead_code)]
    commit_hash: Option<String>,
}

/// `(rfilename, size)` of every sibling in a model-info JSON.
fn siblings(v: &Value) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    if let Some(list) = v.get("siblings").and_then(Value::as_array) {
        for s in list {
            if let Some(name) = s.get("rfilename").and_then(Value::as_str) {
                let size = s.get("size").and_then(Value::as_i64).unwrap_or(0).max(0) as u64;
                out.push((name.to_string(), size));
            }
        }
    }
    out
}

/// `CACHEDIR.TAG` content (Python writes it in text mode: CRLF on Windows).
pub const CACHEDIR_TAG_CONTENT: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by huggingface_hub.\n# For information about cache directory tags, see:\n#\thttps://bford.info/cachedir/\n";

/// Python text-mode newline translation for written files.
pub fn text_mode(s: &str) -> String {
    if cfg!(windows) { s.replace('\n', "\r\n") } else { s.to_string() }
}

fn create_cachedir_tag(cache_dir: &Path) {
    let tag = cache_dir.join("CACHEDIR.TAG");
    if !fsx::exists(&tag) {
        let _ = fs::write(&tag, text_mode(CACHEDIR_TAG_CONTENT));
    }
}

/// `_cache_commit_hash_for_specific_revision(storage, "main", commit)`: atomic, only when different.
pub fn write_ref(storage: &Path, commit: &str) -> std::io::Result<()> {
    let refs = storage.join("refs");
    fs::create_dir_all(&refs)?;
    let main = refs.join("main");
    let current = fs::read(&main).ok().map(|b| se_core::pyfmt::universal_newlines(&String::from_utf8_lossy(&b)));
    if current.as_deref() == Some(commit) {
        return Ok(());
    }
    let tmp = refs.join(format!("main.{}.tmp", fsx::rand_hex8()));
    fs::write(&tmp, commit.as_bytes())?;
    fs::rename(&tmp, &main).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

static SYMLINK_SUPPORT: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

/// `are_symlinks_supported(dir)`: probes once per (resolved) directory by creating a relative symlink in a
/// temporary folder inside it; always false with `HF_HUB_DISABLE_SYMLINKS`.
pub fn are_symlinks_supported(dir: &Path) -> bool {
    if symlinks_disabled() {
        return false;
    }
    let key = fsx::path_key(&se_core::paths::resolve(dir));
    if let Some(v) = SYMLINK_SUPPORT.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&key)) {
        return *v;
    }
    let _ = fs::create_dir_all(dir);
    let probe_dir = dir.join(format!("tmp{}", fsx::rand_hex8()));
    let supported = match fs::create_dir(&probe_dir) {
        Ok(()) => {
            let src = probe_dir.join("dummy_file_src");
            let _ = fs::write(&src, b"");
            let ok = fsx::symlink_file(Path::new("dummy_file_src"), &probe_dir.join("dummy_file_dst")).is_ok();
            fsx::rmtree_ignore_errors(&probe_dir);
            ok
        }
        Err(_) => false,
    };
    SYMLINK_SUPPORT.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(key, supported);
    supported
}

/// `_create_symlink(blob, pointer, new_blob)`: relative symlink where supported; otherwise move a new blob
/// to the pointer path (so `blobs/` stays empty for it) or copy an existing one.
pub fn create_pointer(blob: &Path, pointer: &Path, new_blob: bool) -> std::io::Result<()> {
    let _ = fs::remove_file(pointer);
    let abs_src = se_core::paths::abspath(blob);
    let abs_dst = se_core::paths::abspath(pointer);
    let dst_dir = abs_dst.parent().unwrap_or(Path::new("")).to_path_buf();
    let relative = fsx::relpath(&abs_src, &dst_dir);
    let common = common_path(&abs_src, &abs_dst);
    let support = match &common {
        Some(c) => are_symlinks_supported(c),
        None => !cfg!(windows) && !symlinks_disabled(),
    };
    if support {
        let src = relative.clone().unwrap_or_else(|| abs_src.clone());
        match fsx::symlink_file(&src, &abs_dst) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let same = fs::canonicalize(&abs_dst).ok() == fs::canonicalize(&abs_src).ok();
                if fsx::is_symlink(&abs_dst) && same {
                    return Ok(());
                }
                return Err(e);
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(e) => return Err(e),
        }
    }
    if new_blob {
        if fs::rename(&abs_src, &abs_dst).is_err() {
            fs::copy(&abs_src, &abs_dst)?;
            fs::remove_file(&abs_src)?;
        }
        Ok(())
    } else {
        fs::copy(&abs_src, &abs_dst).map(|_| ())
    }
}

fn common_path(a: &Path, b: &Path) -> Option<PathBuf> {
    let ac: Vec<_> = a.components().collect();
    let bc: Vec<_> = b.components().collect();
    let key = |c: &std::path::Component<'_>| fsx::path_key(Path::new(c.as_os_str()));
    if ac.is_empty() || bc.is_empty() || key(&ac[0]) != key(&bc[0]) {
        return None;
    }
    let mut out = PathBuf::new();
    for (x, y) in ac.iter().zip(bc.iter()) {
        if key(x) != key(y) {
            break;
        }
        out.push(x.as_os_str());
    }
    Some(out)
}

impl Hub for HfHub {
    fn repo_bytes(&self, repo: &str, patterns: &[&str], fallback: u64) -> u64 {
        let url = format!("{}/api/models/{repo}?blobs=true", self.endpoint);
        let Ok(v) = self.get_json(&url) else { return fallback };
        let sizes = siblings(&v);
        let names: Vec<String> = sizes.iter().map(|(n, _)| n.clone()).collect();
        let map: HashMap<&str, u64> = sizes.iter().map(|(n, s)| (n.as_str(), *s)).collect();
        let total: u64 = filter_repo_objects(&names, patterns).iter().map(|n| map.get(n.as_str()).copied().unwrap_or(0)).sum();
        if total == 0 { fallback } else { total }
    }

    fn snapshot_download(&self, repo: &str, patterns: &[&str], on_bytes: &mut dyn FnMut(u64)) -> Result<PathBuf> {
        let storage = repo_folder(&self.cache_dir, repo);
        let (commit, files) = match self.revision_info(repo) {
            Ok(info) => info,
            Err(err) => {
                // ensure_model only runs when files are missing, so a cached (incomplete) snapshot is no answer.
                let message = if se_core::offline::hub_offline() {
                    "Cannot find an appropriate cached snapshot folder for the specified revision on the local disk and outgoing traffic has been disabled. To enable repo look-ups and downloads online, set 'HF_HUB_OFFLINE=0' as environment variable.".to_string()
                } else {
                    format!(
                        "Got: {err}\nAn error happened while trying to locate the files on the Hub, and we cannot find the appropriate snapshot folder for the specified revision on the local disk. Please check your internet connection and try again."
                    )
                };
                return Err(net(message));
            }
        };
        let wanted: Vec<String> = filter_repo_objects(&files, patterns).into_iter().cloned().collect();
        let snapshot = storage.join("snapshots").join(&commit);
        write_ref(&storage, &commit).map_err(|e| EngineError::win(&e, Some(&storage.join("refs").join("main"))))?;
        let mut written = 0u64;
        for file in &wanted {
            let mut count = |n: u64| {
                written += n;
                on_bytes(written);
            };
            self.download_file(repo, &commit, file, &mut count)?;
        }
        fs::create_dir_all(&snapshot).map_err(|e| EngineError::win(&e, Some(&snapshot)))?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_normalization_matches_python_lstrip() {
        assert_eq!(normalize_etag("W/\"abc\""), "abc");
        assert_eq!(normalize_etag("\"abc\""), "abc");
        assert_eq!(normalize_etag("Wabc"), "abc");
    }

    #[test]
    fn ref_written_without_newline_and_only_when_changed() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("models--a--b");
        write_ref(&storage, "abc").unwrap();
        assert_eq!(fs::read(storage.join("refs/main")).unwrap(), b"abc");
        write_ref(&storage, "abc").unwrap();
        write_ref(&storage, "def").unwrap();
        assert_eq!(fs::read(storage.join("refs/main")).unwrap(), b"def");
        assert_eq!(fs::read_dir(storage.join("refs")).unwrap().count(), 1);
    }

    #[test]
    fn pointer_is_a_relative_symlink_or_the_moved_blob() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("models--a--b");
        let blob = storage.join("blobs").join("e1");
        let pointer = storage.join("snapshots").join("c1").join("sub").join("f.bin");
        fs::create_dir_all(blob.parent().unwrap()).unwrap();
        fs::create_dir_all(pointer.parent().unwrap()).unwrap();
        fs::write(&blob, b"data").unwrap();
        create_pointer(&blob, &pointer, true).unwrap();
        assert_eq!(fs::read(&pointer).unwrap(), b"data");
        if are_symlinks_supported(dir.path()) {
            assert!(fsx::is_symlink(&pointer));
            assert_eq!(fsx::read_link(&pointer).unwrap(), Path::new("..").join("..").join("..").join("blobs").join("e1"));
        } else {
            assert!(!blob.exists(), "a new blob is moved into the snapshot without symlinks");
        }
        // An existing blob is copied (never moved away) without symlinks.
        let pointer2 = storage.join("snapshots").join("c2").join("f.bin");
        fs::create_dir_all(pointer2.parent().unwrap()).unwrap();
        fs::write(&blob, b"data").unwrap();
        create_pointer(&blob, &pointer2, false).unwrap();
        assert!(blob.exists());
        assert_eq!(fs::read(&pointer2).unwrap(), b"data");
    }
}
