//! Content fingerprint `fp:<len>:<24 hex>` (win-profiles §4.6 = mac-librarykit §12), byte-identical on both
//! platforms. The process-wide cache is cleared whenever a session ends (UNIFIED.md §3.12 FIX).

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::LazyLock;

use parking_lot::Mutex;
use sha2::{Digest, Sha256};

/// Slice size.
pub const FP_SLICE: u64 = 65_536;

#[derive(Clone)]
struct CachedFingerprint {
    length: u64,
    last_write: i64,
    fingerprint: String,
}

static CACHE: LazyLock<Mutex<HashMap<String, CachedFingerprint>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Cache key: Windows paths compare OrdinalIgnoreCase (C# `StringComparer.OrdinalIgnoreCase`).
fn cache_key(path: &Path) -> String {
    let s = path.to_string_lossy();
    #[cfg(windows)]
    {
        crate::win::netcompat::upper_invariant(&s)
    }
    #[cfg(not(windows))]
    {
        s.into_owned()
    }
}

/// (length, last-write stamp) of a regular, non-empty file. Windows: .NET `LastWriteTimeUtc.Ticks`.
fn signature(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    #[cfg(windows)]
    let stamp = {
        use std::os::windows::fs::MetadataExt;
        (meta.last_write_time() as i64).wrapping_add(504_911_232_000_000_000)
    };
    #[cfg(not(windows))]
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64 / 100)
        .unwrap_or(0);
    Some((meta.len(), stamp))
}

/// Hashes the length (8 bytes LE) and up to three 64 KiB slices at `[0, max(0, len/2 - 32768), max(0, len - 65536)]`
/// (overlaps hashed again). `None` for a missing, empty, non-regular or unreadable file.
pub fn compute(path: &Path) -> Option<String> {
    let (length, _) = signature(path)?;
    hash_file(path, length)
}

fn hash_file(path: &Path, length: u64) -> Option<String> {
    // Rust's std opens files with FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE on Windows.
    let mut file = std::fs::File::open(path).ok()?;
    let offsets = [0u64, (length / 2).saturating_sub(FP_SLICE / 2), length.saturating_sub(FP_SLICE)];
    let mut sha = Sha256::new();
    sha.update((length as i64).to_le_bytes());
    let mut buf = vec![0u8; FP_SLICE as usize];
    for off in offsets {
        if off >= length {
            continue;
        }
        file.seek(SeekFrom::Start(off)).ok()?;
        let want = FP_SLICE.min(length - off) as usize;
        let mut total = 0usize;
        while total < want {
            match file.read(&mut buf[total..want]) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
        }
        if total > 0 {
            sha.update(&buf[..total]);
        }
    }
    let digest = sha.finalize();
    let hex = crate::crypto::lower_hex(&digest);
    Some(format!("fp:{length}:{}", &hex[..24]))
}

/// Cached value when the file signature (length + mtime) still matches; computes and caches otherwise.
pub fn fingerprint(path: &Path) -> Option<String> {
    let (length, stamp) = signature(path)?;
    let key = cache_key(path);
    if let Some(c) = CACHE.lock().get(&key) {
        if c.length == length && c.last_write == stamp {
            return Some(c.fingerprint.clone());
        }
    }
    let fp = hash_file(path, length)?;
    CACHE.lock().insert(key, CachedFingerprint { length, last_write: stamp, fingerprint: fp.clone() });
    Some(fp)
}

/// The cached value without reading contents (Windows: no file check at all; macOS: one `stat`).
pub fn cached(path: &Path) -> Option<String> {
    let key = cache_key(path);
    let hit = CACHE.lock().get(&key).cloned()?;
    #[cfg(not(windows))]
    {
        let (length, stamp) = signature(path)?;
        if hit.length != length || hit.last_write != stamp {
            return None;
        }
    }
    Some(hit.fingerprint)
}

/// True while the cache remembers `path` (privacy tests).
pub fn is_cached(path: &Path) -> bool {
    CACHE.lock().contains_key(&cache_key(path))
}

/// Forgets one path.
pub fn forget(path: &Path) {
    CACHE.lock().remove(&cache_key(path));
}

/// Forgets every cached path (logout, switch, delete, external lock).
pub fn clear_cache() {
    CACHE.lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(bytes: &[u8]) -> String {
        // Straight port of the C# loop for cross-checking the slicing.
        let len = bytes.len() as u64;
        let mut sha = Sha256::new();
        sha.update((len as i64).to_le_bytes());
        for off in [0u64, (len / 2).saturating_sub(32768), len.saturating_sub(65536)] {
            if off >= len {
                continue;
            }
            let end = (off + 65536).min(len);
            sha.update(&bytes[off as usize..end as usize]);
        }
        format!("fp:{len}:{}", &crate::crypto::lower_hex(&sha.finalize())[..24])
    }

    #[test]
    fn overlapping_slices_are_hashed_again() {
        let dir = tempfile::tempdir().unwrap();
        for size in [5usize, 70_000, 200_000] {
            let p = dir.path().join(format!("f{size}.bin"));
            let data: Vec<u8> = (0..size).map(|i| (i * 31 % 251) as u8).collect();
            std::fs::write(&p, &data).unwrap();
            assert_eq!(compute(&p).unwrap(), reference(&data));
            assert_eq!(fingerprint(&p).unwrap(), reference(&data));
            assert_eq!(cached(&p).unwrap(), reference(&data));
        }
        let empty = dir.path().join("empty.bin");
        std::fs::write(&empty, b"").unwrap();
        assert!(compute(&empty).is_none());
        assert!(compute(&dir.path().join("missing.bin")).is_none());
    }
}
