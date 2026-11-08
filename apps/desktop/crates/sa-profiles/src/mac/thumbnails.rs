//! `ThumbnailCache` (mac-librarykit §11, LibraryKit `ThumbnailCache.swift` at `2f4bc96`): the card pictures of each
//! profile, one AES-GCM file per picture, capped per profile.
//!
//! Layout: `<root>/<hex(SHA-256(lower(profile)))[0..32]>/<hex(HMAC-SHA256(vault_key, lower(key)))[0..32]>.bin`.
//! The file names are keyed with the vault key (the Swift app moved from plain SHA-256 names to HMAC names in
//! `2f4bc96`); a picture still stored under the older SHA-256 name is found too and renamed to its keyed name on the
//! first read, so pictures of earlier builds survive. Content = `seal(image, vault_key)` (`nonce|tag|ct`).

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use sa_core::secrets::SecretKey32;
use sha2::{Digest, Sha256};

use crate::crypto;

/// Limits (both clamped to ≥ 1 by [`ThumbnailCache::new`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailCacheLimits {
    pub max_files: usize,
    pub max_bytes: u64,
}

impl Default for ThumbnailCacheLimits {
    fn default() -> Self {
        Self { max_files: 1500, max_bytes: 150 * 1024 * 1024 }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Usage {
    files: usize,
    bytes: u64,
}

/// Encrypted card pictures of the profiles.
pub struct ThumbnailCache {
    root: PathBuf,
    limits: ThumbnailCacheLimits,
    usage: Mutex<HashMap<String, Usage>>,
}

const RENEW_AFTER: Duration = Duration::from_secs(3600);

impl ThumbnailCache {
    pub fn new(root: PathBuf, limits: ThumbnailCacheLimits) -> Self {
        let limits = ThumbnailCacheLimits { max_files: limits.max_files.max(1), max_bytes: limits.max_bytes.max(1) };
        Self { root, limits, usage: Mutex::new(HashMap::new()) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder name of a profile: a hash, so the user's name is not on disk in the clear.
    pub fn folder_name(profile: &str) -> String {
        let digest = Sha256::digest(profile.to_lowercase().as_bytes());
        crypto::lower_hex(&digest[..16])
    }

    /// The file name of a picture key, keyed with the vault key.
    pub fn file_name(key: &str, vault_key: &SecretKey32) -> String {
        let mac = crypto::hmac_sha256(vault_key.expose(), key.to_lowercase().as_bytes());
        format!("{}.bin", crypto::lower_hex(&mac[..16]))
    }

    /// The pre-`2f4bc96` file name (plain SHA-256 of the lowercased key).
    fn legacy_file_name(key: &str) -> String {
        let digest = Sha256::digest(key.to_lowercase().as_bytes());
        format!("{}.bin", crypto::lower_hex(&digest[..16]))
    }

    fn folder(&self, profile: &str) -> PathBuf {
        self.root.join(Self::folder_name(profile))
    }

    /// The picture stored under the first of `keys` that has one. A file that does not open (another key, damage)
    /// is removed and counts as missing; a hit older than an hour gets its date renewed (LRU).
    pub fn data(&self, profile: &str, keys: &[String], vault_key: &SecretKey32) -> Option<Vec<u8>> {
        let folder = self.folder(profile);
        let folder_name = Self::folder_name(profile);
        for key in keys {
            let file = folder.join(Self::file_name(key, vault_key));
            if let Ok(blob) = fs::read(&file) {
                match crypto::open(&blob, vault_key) {
                    Some(plain) => {
                        renew_if_old(&file);
                        return Some(plain);
                    }
                    None => {
                        let mut usage = self.usage.lock();
                        discard(&mut usage, &file, &folder_name);
                        continue;
                    }
                }
            }
            let legacy = folder.join(Self::legacy_file_name(key));
            let Ok(blob) = fs::read(&legacy) else { continue };
            match crypto::open(&blob, vault_key) {
                Some(plain) => {
                    // Move it to its keyed name (the content is unchanged and stays encrypted).
                    let _guard = self.usage.lock();
                    if !file.exists() {
                        let _ = fs::rename(&legacy, &file);
                    }
                    renew_if_old(&file);
                    return Some(plain);
                }
                None => {
                    let mut usage = self.usage.lock();
                    discard(&mut usage, &legacy, &folder_name);
                }
            }
        }
        None
    }

    /// Stores a picture; whether it is on disk now.
    pub fn store(&self, data: &[u8], profile: &str, key: &str, vault_key: &SecretKey32) -> bool {
        let blob = crypto::seal(data, vault_key);
        if blob.is_empty() {
            return false;
        }
        let folder = self.folder(profile);
        let name = Self::folder_name(profile);
        let file_name = Self::file_name(key, vault_key);
        let file = folder.join(&file_name);
        let mut usage = self.usage.lock();
        if !ensure_folder(&folder) {
            return false;
        }
        if !usage.contains_key(&name) {
            let all = entries(&folder);
            usage.insert(name.clone(), Usage { files: all.len(), bytes: all.iter().map(|e| e.size).sum() });
        }
        let previous = fs::metadata(&file).ok().map(|m| m.len());
        if !write_atomically(&blob, &file) {
            return false;
        }
        let current = usage.entry(name.clone()).or_default();
        if previous.is_none() {
            current.files += 1;
        }
        current.bytes = (current.bytes + blob.len() as u64).saturating_sub(previous.unwrap_or(0));
        let over = current.files > self.limits.max_files || current.bytes > self.limits.max_bytes;
        if over {
            let after = self.evict(&folder, &file_name);
            usage.insert(name, after);
        }
        true
    }

    /// Deletes the pictures under `keys` (keyed and legacy names).
    pub fn remove(&self, profile: &str, keys: &[String], vault_key: &SecretKey32) {
        let folder = self.folder(profile);
        let name = Self::folder_name(profile);
        let mut usage = self.usage.lock();
        for key in keys {
            discard(&mut usage, &folder.join(Self::file_name(key, vault_key)), &name);
            discard(&mut usage, &folder.join(Self::legacy_file_name(key)), &name);
        }
    }

    /// Deletes the whole folder of a profile (the profile is gone).
    pub fn remove_all(&self, profile: &str) {
        let folder = self.folder(profile);
        let mut usage = self.usage.lock();
        let _ = fs::remove_dir_all(&folder);
        usage.remove(&Self::folder_name(profile));
    }

    /// Number of pictures of a profile on disk (tests).
    pub fn file_count(&self, profile: &str) -> usize {
        let _usage = self.usage.lock();
        entries(&self.folder(profile)).len()
    }

    /// Removes the least recently used files down to 90 % of the caps, keeping `keep` (the file just written).
    fn evict(&self, folder: &Path, keep: &str) -> Usage {
        let mut all = entries(folder);
        all.sort_by(|a, b| a.modified.cmp(&b.modified).then_with(|| a.name.cmp(&b.name)));
        let mut files = all.len();
        let mut bytes: u64 = all.iter().map(|e| e.size).sum();
        let file_target = self.limits.max_files * 9 / 10;
        let byte_target = self.limits.max_bytes / 10 * 9;
        for entry in &all {
            if files <= file_target && bytes <= byte_target {
                break;
            }
            if entry.name == keep {
                continue;
            }
            if fs::remove_file(folder.join(&entry.name)).is_ok() {
                files -= 1;
                bytes = bytes.saturating_sub(entry.size);
            }
        }
        Usage { files, bytes }
    }
}

struct Entry {
    name: String,
    modified: SystemTime,
    size: u64,
}

/// `.bin` files of a folder; other files are temp leftovers, deleted when older than an hour and never counted.
fn entries(folder: &Path) -> Vec<Entry> {
    let Ok(dir) = fs::read_dir(folder) else { return Vec::new() };
    let mut found = Vec::new();
    for item in dir.flatten() {
        let path = item.path();
        let Ok(meta) = item.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if path.extension().and_then(|e| e.to_str()) != Some("bin") {
            if SystemTime::now().duration_since(modified).unwrap_or_default() > RENEW_AFTER {
                let _ = fs::remove_file(&path);
            }
            continue;
        }
        found.push(Entry { name: item.file_name().to_string_lossy().into_owned(), modified, size: meta.len() });
    }
    found
}

fn discard(usage: &mut HashMap<String, Usage>, file: &Path, folder_name: &str) {
    let Ok(meta) = fs::metadata(file) else { return };
    if fs::remove_file(file).is_err() {
        return;
    }
    if let Some(current) = usage.get_mut(folder_name) {
        current.files = current.files.saturating_sub(1);
        current.bytes = current.bytes.saturating_sub(meta.len());
    }
}

fn renew_if_old(file: &Path) {
    let Ok(meta) = fs::metadata(file) else { return };
    let Ok(modified) = meta.modified() else { return };
    if SystemTime::now().duration_since(modified).unwrap_or_default() > RENEW_AFTER
        && let Ok(handle) = fs::OpenOptions::new().write(true).open(file)
    {
        let _ = handle.set_modified(SystemTime::now());
    }
}

fn ensure_folder(folder: &Path) -> bool {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(folder).is_ok()
}

/// Temp in the same folder (`<name>.tmp-<uuid>`, mode 0600) + rename.
fn write_atomically(data: &[u8], file: &Path) -> bool {
    let temp = file.with_extension(format!("bin.tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let ok = options.open(&temp).and_then(|mut f| f.write_all(data)).is_ok() && fs::rename(&temp, file).is_ok();
    if !ok {
        let _ = fs::remove_file(&temp);
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> Vec<u8> {
        (0..2000u32).map(|i| (i % 251) as u8).collect()
    }

    fn key() -> SecretKey32 {
        let mut bytes = [0u8; 32];
        crypto::random_fill(&mut bytes);
        SecretKey32::new(bytes)
    }

    fn set_age(path: &Path, seconds_ago: u64) {
        let handle = fs::OpenOptions::new().write(true).open(path).unwrap();
        handle.set_modified(SystemTime::now() - Duration::from_secs(seconds_ago)).unwrap();
    }

    #[test]
    fn the_cache_evicts_the_least_recently_used_pictures() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        let cache = ThumbnailCache::new(root.clone(), ThumbnailCacheLimits { max_files: 10, max_bytes: 10_000_000 });
        let k = key();
        let folder = root.join(ThumbnailCache::folder_name("ali"));
        for index in 0..10u64 {
            assert!(cache.store(&picture(), "ali", &format!("k{index}"), &k));
            // k0 is the oldest, k9 the newest.
            set_age(&folder.join(ThumbnailCache::file_name(&format!("k{index}"), &k)), (100 - index) * 60);
        }
        assert_eq!(cache.file_count("ali"), 10);
        assert!(cache.store(&picture(), "ali", "k10", &k));
        assert_eq!(cache.file_count("ali"), 9);
        assert_eq!(cache.data("ali", &["k0".into()], &k), None);
        assert_eq!(cache.data("ali", &["k1".into()], &k), None);
        assert_eq!(cache.data("ali", &["k2".into()], &k), Some(picture()));
        assert_eq!(cache.data("ali", &["k10".into()], &k), Some(picture()));
    }

    #[test]
    fn the_cache_is_capped_in_bytes_too() {
        let dir = tempfile::tempdir().unwrap();
        let each = (picture().len() + 28) as u64;
        let cache =
            ThumbnailCache::new(dir.path().join("cache"), ThumbnailCacheLimits { max_files: 1000, max_bytes: each * 5 + 10 });
        let k = key();
        for index in 0..12 {
            assert!(cache.store(&picture(), "ali", &format!("k{index}"), &k));
        }
        let count = cache.file_count("ali");
        assert!((3..=5).contains(&count), "{count}");
        assert_eq!(cache.data("ali", &["k11".into()], &k), Some(picture()));
    }

    #[test]
    fn reading_keeps_a_picture_alive_against_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        let cache = ThumbnailCache::new(root.clone(), ThumbnailCacheLimits { max_files: 5, max_bytes: 10_000_000 });
        let k = key();
        let folder = root.join(ThumbnailCache::folder_name("ali"));
        for index in 0..5u64 {
            assert!(cache.store(&picture(), "ali", &format!("k{index}"), &k));
            set_age(&folder.join(ThumbnailCache::file_name(&format!("k{index}"), &k)), (10 - index) * 7200);
        }
        assert_eq!(cache.data("ali", &["k0".into()], &k), Some(picture()));
        assert!(cache.store(&picture(), "ali", "k5", &k));
        assert_eq!(cache.data("ali", &["k0".into()], &k), Some(picture()));
        assert_eq!(cache.data("ali", &["k1".into()], &k), None);
    }

    #[test]
    fn pictures_are_sealed_and_names_are_keyed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ThumbnailCache::new(dir.path().to_path_buf(), ThumbnailCacheLimits::default());
        let k = key();
        assert!(cache.store(&picture(), "Emre", "/Movies/Film.mp4", &k));
        let folder = dir.path().join(ThumbnailCache::folder_name("emre"));
        let file = folder.join(ThumbnailCache::file_name("/movies/film.mp4", &k));
        let bytes = fs::read(&file).unwrap();
        assert_eq!(bytes.len(), picture().len() + 28);
        assert!(!bytes.windows(64).any(|w| w == &picture()[..64]));
        assert_ne!(ThumbnailCache::file_name("k", &k), ThumbnailCache::file_name("k", &key()));
        // Another key does not open it; the file is dropped instead of being served.
        let other = key();
        let name_under_other = ThumbnailCache::file_name("/movies/film.mp4", &other);
        fs::copy(&file, folder.join(&name_under_other)).unwrap();
        assert_eq!(cache.data("emre", &["/movies/film.mp4".into()], &other), None);
        assert!(!folder.join(&name_under_other).exists());
        assert_eq!(cache.data("emre", &["/movies/film.mp4".into()], &k), Some(picture()));
    }

    #[test]
    fn a_picture_under_its_older_sha256_name_is_found_and_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ThumbnailCache::new(dir.path().to_path_buf(), ThumbnailCacheLimits::default());
        let k = key();
        let folder = dir.path().join(ThumbnailCache::folder_name("ali"));
        fs::create_dir_all(&folder).unwrap();
        let legacy = folder.join(ThumbnailCache::legacy_file_name("fp:9:abc"));
        fs::write(&legacy, crypto::seal(&picture(), &k)).unwrap();
        assert_eq!(cache.data("ali", &["fp:9:abc".into()], &k), Some(picture()));
        assert!(!legacy.exists());
        assert!(folder.join(ThumbnailCache::file_name("fp:9:abc", &k)).exists());
        cache.remove("ali", &["fp:9:abc".into()], &k);
        assert_eq!(cache.file_count("ali"), 0);
    }
}
