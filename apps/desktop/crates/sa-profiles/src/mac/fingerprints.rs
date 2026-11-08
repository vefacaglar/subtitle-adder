//! `VideoFingerprint` of LibraryKit (mac-librarykit §12): the shared `fp:<len>:<24 hex>` algorithm
//! ([`crate::fingerprint::compute`]) behind a memo keyed by [`ci`](super::paths::ci) path and
//! `{length, mtime seconds, mtime nanoseconds}`.
//!
//! Privacy (UNIFIED.md §3.12 FIX): the memo is a list of paths the profile looked at, so the store clears it whenever
//! a session ends.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;

use super::paths::{ci, normalized_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Signature {
    length: u64,
    seconds: i64,
    nanoseconds: i64,
}

/// Process-wide (by default) memo of fingerprints, shared by every store so a library is read once.
#[derive(Default)]
pub struct FingerprintCache {
    entries: Mutex<HashMap<String, (Signature, String)>>,
}

static GLOBAL: LazyLock<Arc<FingerprintCache>> = LazyLock::new(|| Arc::new(FingerprintCache::default()));

impl FingerprintCache {
    /// The process-wide cache.
    pub fn global() -> Arc<FingerprintCache> {
        GLOBAL.clone()
    }

    /// `fp:…` for an existing, non-empty, readable regular file (symlinks followed), else `None`. Cached by path and
    /// signature: an unchanged file costs one `stat`, the first look reads up to 192 KB.
    pub fn fingerprint(&self, normalized: &str) -> Option<String> {
        let signature = signature(normalized)?;
        let key = ci(normalized);
        if let Some((s, fp)) = self.entries.lock().get(&key)
            && *s == signature
        {
            return Some(fp.clone());
        }
        let computed = crate::fingerprint::compute(Path::new(normalized))?;
        self.entries.lock().insert(key, (signature, computed.clone()));
        Some(computed)
    }

    /// The memoized value if the file did not change since; one `stat`, never reads contents.
    pub fn cached(&self, normalized: &str) -> Option<String> {
        let signature = signature(normalized)?;
        let entries = self.entries.lock();
        match entries.get(&ci(normalized)) {
            Some((s, fp)) if *s == signature => Some(fp.clone()),
            _ => None,
        }
    }

    /// The fingerprint if the file has one, otherwise the normalized path (`""` for an empty path).
    pub fn video_id(&self, path: &Path) -> String {
        let normalized = normalized_path(path);
        if normalized.is_empty() {
            return normalized;
        }
        self.fingerprint(&normalized).unwrap_or(normalized)
    }

    /// Forgets every path (session end).
    pub fn clear(&self) {
        self.entries.lock().clear();
    }

    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn signature(path: &str) -> Option<Signature> {
    if path.is_empty() {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let (seconds, nanoseconds) = super::fileio::mtime(&meta);
    Some(Signature { length: meta.len(), seconds, nanoseconds })
}
