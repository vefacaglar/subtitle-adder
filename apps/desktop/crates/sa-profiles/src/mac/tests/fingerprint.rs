//! `VideoFingerprintTests` (7).

use sha2::{Digest, Sha256};

use super::*;
use crate::FingerprintMode;
use crate::mac::vault::VideoProbe;

fn write(folder: &TestFolder, name: &str, count: usize, seed: u32) -> PathBuf {
    let url = folder.video(name);
    if let Some(parent) = url.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut value = seed;
    let bytes: Vec<u8> = (0..count)
        .map(|_| {
            value = value.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (value >> 16) as u8
        })
        .collect();
    std::fs::write(&url, bytes).unwrap();
    url
}

/// The algorithm written out independently: SHA-256 of the little-endian length and the three slices.
fn expected(data: &[u8]) -> String {
    let length = data.len();
    let mut hasher = Sha256::new();
    hasher.update((length as i64).to_le_bytes());
    for offset in [0, (length / 2).saturating_sub(32_768), length.saturating_sub(65_536)] {
        if offset < length {
            hasher.update(&data[offset..(offset + 65_536).min(length)]);
        }
    }
    let digest = hasher.finalize();
    format!("fp:{length}:{}", crate::crypto::lower_hex(&digest[..12]))
}

#[test]
fn fingerprint_of_150000_bytes_starts_with_its_length_and_survives_a_move() {
    let folder = TestFolder::new();
    let original = write(&folder, "a.mp4", 150_000, 1);
    let id = folder.cache.video_id(&original);
    assert!(id.starts_with("fp:150000:"));
    assert_eq!(id.len(), "fp:150000:".len() + 24);
    let moved = folder.video("sub/b.mkv");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&original, &moved).unwrap();
    assert_eq!(folder.cache.video_id(&moved), id);
    assert_eq!(folder.cache.video_id(&original), normalized(&original));
}

#[test]
fn matches_the_documented_algorithm_for_large_and_small_files() {
    let folder = TestFolder::new();
    for count in [1usize, 100, 65_535, 65_536, 65_537, 150_000, 300_001] {
        let url = write(&folder, &format!("f{count}.bin"), count, (count % 200) as u32);
        let data = std::fs::read(&url).unwrap();
        assert_eq!(folder.cache.video_id(&url), expected(&data), "length {count}");
    }
}

fn touch_future(path: &Path, seconds: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() + Duration::from_secs(seconds)).unwrap();
}

#[test]
fn content_change_in_a_slice_changes_the_fingerprint_and_the_cache_notices_the_edit() {
    let folder = TestFolder::new();
    let url = write(&folder, "edit.bin", 150_000, 1);
    let before = folder.cache.video_id(&url);
    let mut data = std::fs::read(&url).unwrap();
    data[10] ^= 0xFF;
    std::fs::write(&url, &data).unwrap();
    touch_future(&url, 5);
    let after = folder.cache.video_id(&url);
    assert_ne!(before, after);
    assert!(after.starts_with("fp:150000:"));
}

#[test]
fn cached_fingerprint_never_reads_a_file_and_notices_edits() {
    let folder = TestFolder::new();
    let url = write(&folder, "cached.bin", 150_000, 1);
    let path = normalized(&url);
    assert_eq!(folder.cache.cached(&path), None, "not seen yet: nothing is computed");
    assert_eq!(folder.cache.cached(&path), None);
    let computed = folder.cache.fingerprint(&path).unwrap();
    assert_eq!(folder.cache.cached(&path), Some(computed));
    let mut data = std::fs::read(&url).unwrap();
    data[7] ^= 0xFF;
    std::fs::write(&url, &data).unwrap();
    touch_future(&url, 9);
    assert_eq!(folder.cache.cached(&path), None, "an edited file is not served from the cache");
    assert_eq!(folder.cache.cached(""), None);
    assert_eq!(folder.cache.cached(&normalized(&folder.video("missing.bin"))), None);
}

#[test]
fn probe_modes_decide_how_much_of_the_disk_is_touched() {
    let folder = TestFolder::new();
    let url = write(&folder, "modes.bin", 90_000, 1);
    let path = normalized(&url);
    let probe = |mode| VideoProbe::new(&url, mode, &folder.cache).unwrap();
    assert_eq!(probe(FingerprintMode::PathOnly).fingerprint, None);
    assert_eq!(probe(FingerprintMode::CachedOnly).fingerprint, None, "not fingerprinted yet");
    let full = probe(FingerprintMode::Compute);
    assert_eq!(full.fingerprint, folder.cache.fingerprint(&path));
    assert_eq!(probe(FingerprintMode::CachedOnly).fingerprint, full.fingerprint);
    assert_eq!(probe(FingerprintMode::PathOnly).fingerprint, None);
    assert_eq!(probe(FingerprintMode::PathOnly).path, path);
}

#[test]
fn empty_missing_and_non_regular_files_fall_back_to_the_normalized_path() {
    let folder = TestFolder::new();
    let empty = folder.video("empty.mp4");
    std::fs::write(&empty, b"").unwrap();
    assert_eq!(folder.cache.video_id(&empty), normalized(&empty));
    let missing = folder.video("missing.mp4");
    assert_eq!(folder.cache.video_id(&missing), normalized(&missing));
    assert_eq!(folder.cache.fingerprint(&normalized(&folder.url)), None);
    assert_eq!(folder.cache.video_id(Path::new("")), "");
}

#[test]
fn the_global_cache_is_shared_and_clearable() {
    let a = FingerprintCache::global();
    let b = FingerprintCache::global();
    assert!(Arc::ptr_eq(&a, &b));
}
