//! `VaultContents` (mac-librarykit §8): the decrypted vault of the active profile — records keyed by fingerprint (or
//! path while the file has none), custom lists, legacy thumbnails and two lookup indexes — and `VideoProbe` (§8.7).
//!
//! All keys are [`ci`] (NFC of the lowercased text), the Rust equivalent of Swift's lowercased dictionary keys under
//! canonical equivalence. Dropping a value zeroizes the strings and pictures it held.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use base64::Engine as _;
use zeroize::Zeroize;

use super::fingerprints::FingerprintCache;
use super::paths::{ci, directory, file_name, normalized_path, trim_ws, trim_ws_nl};
use super::document::VaultPayload;
use crate::{FingerprintMode, Ticks, UserVideoList, VideoUserStats};

/// The largest picture accepted (512 KiB).
pub const MAX_THUMBNAIL_BYTES: usize = 512 * 1024;

/// A never-watched record for a path (what a discovered file looks like before it is touched).
pub fn placeholder(path: &str, now: Ticks, fingerprint: Option<String>) -> VideoUserStats {
    VideoUserStats {
        content_fingerprint: fingerprint,
        video_path: path.to_string(),
        file_name: file_name(path),
        directory_path: directory(path),
        view_count: 0,
        is_liked: false,
        rating: 0,
        last_position_seconds: 0.0,
        duration_seconds: 0.0,
        last_watched_at: None,
        updated_at: now,
        thumbnail: None,
    }
}

/// A video as the store looks it up: its normalized path and, if readable, its content fingerprint. Built before
/// any lock is taken (construction is the disk I/O).
#[derive(Debug, Clone)]
pub struct VideoProbe {
    pub path: String,
    /// `ci(path)`: identity in the vault maps.
    pub path_key: String,
    /// `path.to_lowercase()` without NFC: what thumbnail file names were hashed from (§2.3 exception).
    pub path_key_raw: String,
    pub fingerprint: Option<String>,
    pub mode: FingerprintMode,
    /// Fingerprints, by `ci` path, of recorded paths this probe may take over (filled before locking).
    pub recorded_fingerprints: HashMap<String, String>,
}

impl VideoProbe {
    /// `None` for an empty normalized path.
    pub fn new(path: &Path, mode: FingerprintMode, cache: &FingerprintCache) -> Option<Self> {
        let path = normalized_path(path);
        Self::from_normalized(path, mode, cache)
    }

    pub fn from_normalized(path: String, mode: FingerprintMode, cache: &FingerprintCache) -> Option<Self> {
        if path.is_empty() {
            return None;
        }
        let fingerprint = match mode {
            FingerprintMode::Compute => cache.fingerprint(&path),
            FingerprintMode::CachedOnly => cache.cached(&path),
            FingerprintMode::PathOnly => None,
        };
        Some(VideoProbe {
            path_key: ci(&path),
            path_key_raw: path.to_lowercase(),
            path,
            fingerprint,
            mode,
            recorded_fingerprints: HashMap::new(),
        })
    }

    pub fn with_recorded_fingerprint(mut self, fingerprint: String, recorded_path: &str) -> Self {
        self.recorded_fingerprints.insert(ci(recorded_path), fingerprint);
        self
    }
}

/// Result of [`VaultContents::resolve`].
#[derive(Debug, Clone)]
pub struct Resolution {
    pub key: String,
    pub stats: VideoUserStats,
    /// The record's path, key or fingerprint was updated.
    pub migrated: bool,
}

#[derive(Debug, Clone, Default)]
pub struct VaultContents {
    videos: HashMap<String, VideoUserStats>,
    pub custom_lists: Vec<UserVideoList>,
    thumbnails: HashMap<String, Vec<u8>>,
    /// `ci` key of a legacy picture → its key lowercased without NFC, the form Swift hashed the cache file name from
    /// (mac-librarykit §2.3 exception), used when the picture moves to the cache.
    thumbnail_raw_keys: HashMap<String, String>,
    key_by_fingerprint: HashMap<String, String>,
    key_by_path: HashMap<String, String>,
}

impl Drop for VaultContents {
    fn drop(&mut self) {
        for (mut k, mut v) in self.videos.drain() {
            k.zeroize();
            zeroize_stats(&mut v);
        }
        for list in &mut self.custom_lists {
            list.name.zeroize();
            list.id.zeroize();
            for p in &mut list.video_paths {
                p.zeroize();
            }
        }
        for (mut k, mut v) in self.thumbnails.drain() {
            k.zeroize();
            v.zeroize();
        }
        for (mut k, mut v) in
            self.key_by_fingerprint.drain().chain(self.key_by_path.drain()).chain(self.thumbnail_raw_keys.drain())
        {
            k.zeroize();
            v.zeroize();
        }
    }
}

fn zeroize_stats(v: &mut VideoUserStats) {
    v.video_path.zeroize();
    v.file_name.zeroize();
    v.directory_path.zeroize();
    if let Some(fp) = &mut v.content_fingerprint {
        fp.zeroize();
    }
    if let Some(t) = &mut v.thumbnail {
        t.zeroize();
    }
}

/// The map key of a fingerprint or path.
pub fn key(text: &str) -> String {
    ci(text)
}

impl VaultContents {
    pub fn new() -> Self {
        Self::default()
    }

    /// §8.1: repairs what a hand-edited or older vault gets wrong; reads no file; merges records of one file.
    pub fn from_payload(payload: &VaultPayload) -> Self {
        let mut contents = VaultContents::default();
        for original in &payload.videos {
            let mut video = original.clone();
            let path = trim_ws_nl(&video.video_path).to_string();
            if path.is_empty() {
                continue;
            }
            let fingerprint =
                video.content_fingerprint.as_deref().map(trim_ws_nl).filter(|s| !s.is_empty()).map(str::to_string);
            video.content_fingerprint = fingerprint.clone();
            video.video_path = path.clone();
            if trim_ws(&video.file_name).is_empty() {
                video.file_name = file_name(&path);
            }
            if video.directory_path.is_empty() {
                video.directory_path = directory(&path);
            }
            video.rating = video.rating.clamp(0, 5);
            video.view_count = video.view_count.max(0);
            video.thumbnail = None;
            let k = key(fingerprint.as_deref().unwrap_or(&path));
            if let Some(existing) = contents.videos.get(&k).cloned() {
                contents.set_video(merged(&existing, &video), &k);
                for recorded in [&existing.video_path, &video.video_path] {
                    contents.key_by_path.insert(ci(recorded), k.clone());
                }
            } else {
                contents.set_video(video, &k);
            }
        }
        for list in &payload.custom_lists {
            let name = trim_ws_nl(&list.name).to_string();
            if trim_ws(&list.id).is_empty() || name.is_empty() {
                continue;
            }
            let mut seen = HashSet::new();
            let video_paths = list
                .video_paths
                .iter()
                .map(|p| trim_ws_nl(p).to_string())
                .filter(|p| !p.is_empty() && seen.insert(ci(p)))
                .collect();
            contents.custom_lists.push(UserVideoList { name, video_paths, ..list.clone() });
        }
        for (k, encoded) in &payload.thumbnails {
            if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(encoded)
                && !data.is_empty()
            {
                contents.thumbnail_raw_keys.insert(ci(k), k.to_lowercase());
                contents.thumbnails.insert(ci(k), data);
            }
        }
        contents
    }

    /// §8.2: records (sorted by the encoder), lists as is, thumbnails base64.
    pub fn payload(&self) -> VaultPayload {
        VaultPayload {
            videos: self.videos.values().cloned().collect(),
            custom_lists: self.custom_lists.clone(),
            thumbnails: self
                .thumbnails
                .iter()
                .map(|(k, v)| (k.clone(), base64::engine::general_purpose::STANDARD.encode(v)))
                .collect(),
        }
    }

    pub fn videos(&self) -> &HashMap<String, VideoUserStats> {
        &self.videos
    }

    pub fn has_thumbnails(&self) -> bool {
        !self.thumbnails.is_empty()
    }

    pub fn set_video(&mut self, stats: VideoUserStats, k: &str) {
        let k = key(k);
        if let Some(old) = self.videos.get(&k).cloned() {
            self.remove_from_indexes(&old, &k);
        }
        let mut stored = stats;
        stored.thumbnail = None;
        if let Some(fp) = stored.content_fingerprint.as_deref()
            && !fp.is_empty()
        {
            self.key_by_fingerprint.insert(ci(fp), k.clone());
        }
        self.key_by_path.insert(ci(&stored.video_path), k.clone());
        self.videos.insert(k, stored);
    }

    pub fn remove_video(&mut self, k: &str) -> Option<VideoUserStats> {
        let k = key(k);
        let old = self.videos.remove(&k)?;
        self.remove_from_indexes(&old, &k);
        Some(old)
    }

    fn remove_from_indexes(&mut self, stats: &VideoUserStats, k: &str) {
        if let Some(fp) = stats.content_fingerprint.as_deref() {
            let fp = ci(fp);
            if self.key_by_fingerprint.get(&fp).map(String::as_str) == Some(k) {
                self.key_by_fingerprint.remove(&fp);
            }
        }
        let path = ci(&stats.video_path);
        if self.key_by_path.get(&path).map(String::as_str) == Some(k) {
            self.key_by_path.remove(&path);
        }
    }

    /// The record with its legacy thumbnail attached.
    pub fn stats_for_key(&self, k: &str) -> Option<VideoUserStats> {
        let k = key(k);
        let mut stats = self.videos.get(&k)?.clone();
        stats.thumbnail = self.thumbnails.get(&k).cloned();
        Some(stats)
    }

    /// The key of the record for this file: by fingerprint first, then by path.
    pub fn find(&self, probe: &VideoProbe) -> Option<String> {
        if let Some(fp) = probe.fingerprint.as_deref().map(key) {
            if self.videos.contains_key(&fp) {
                return Some(fp);
            }
            if let Some(k) = self.key_by_fingerprint.get(&fp) {
                return Some(k.clone());
            }
        }
        if self.videos.contains_key(&probe.path_key) {
            return Some(probe.path_key.clone());
        }
        self.key_by_path.get(&probe.path_key).cloned()
    }

    /// The key of a record by path only (no probe, no I/O).
    pub fn find_by_path_key(&self, path_key: &str) -> Option<String> {
        if self.videos.contains_key(path_key) {
            return Some(path_key.to_string());
        }
        self.key_by_path.get(path_key).cloned()
    }

    /// The recorded path of the record `probe` would take over from another location (to be fingerprinted outside
    /// the locks before `resolve` can tell a move from a copy).
    pub fn takeover_path(&self, probe: &VideoProbe) -> Option<String> {
        probe.fingerprint.as_ref()?;
        let found = self.videos.get(&self.find(probe)?)?;
        if found.video_path.is_empty() || ci(&found.video_path) == probe.path_key {
            return None;
        }
        Some(found.video_path.clone())
    }

    /// §8.5: finds the record and follows a moved/renamed file; never creates.
    pub fn resolve(&mut self, probe: &VideoProbe, now: Ticks) -> Option<Resolution> {
        let found_key = self.find(probe)?;
        let found = self.videos.get(&found_key)?.clone();
        let fingerprint_key = probe.fingerprint.as_deref().map(key);
        let canonical = fingerprint_key
            .clone()
            .or_else(|| found.content_fingerprint.as_deref().map(key))
            .unwrap_or_else(|| found_key.clone());
        let path_changed = ci(&found.video_path) != probe.path_key;
        if path_changed
            && let Some(fp) = &probe.fingerprint
            && !found.video_path.is_empty()
            && probe.recorded_fingerprints.get(&ci(&found.video_path)) == Some(fp)
        {
            return Some(Resolution { key: found_key, stats: found, migrated: false });
        }
        let key_changed = found_key != canonical;
        let fingerprint_added = fingerprint_key.is_some() && found.content_fingerprint.as_deref().map(ci) != fingerprint_key;
        if !path_changed && !key_changed && !fingerprint_added {
            return Some(Resolution { key: found_key, stats: found, migrated: false });
        }
        let mut updated = found.clone();
        updated.content_fingerprint = probe.fingerprint.clone().or_else(|| found.content_fingerprint.clone());
        updated.video_path = probe.path.clone();
        updated.file_name = file_name(&probe.path);
        updated.directory_path = directory(&probe.path);
        if key_changed {
            self.remove_video(&found_key);
            if let Some(picture) = self.thumbnails.remove(&found_key) {
                self.thumbnail_raw_keys.remove(&found_key);
                self.thumbnail_raw_keys.insert(canonical.clone(), canonical.clone());
                self.thumbnails.insert(canonical.clone(), picture);
            }
        }
        self.set_video(updated.clone(), &canonical);
        if path_changed && !found.video_path.is_empty() {
            self.replace_list_path(&found.video_path, &probe.path, now);
        }
        Some(Resolution { key: canonical, stats: updated, migrated: true })
    }

    /// The record for a file, creating a default one if there is none; `changed` = created or migrated.
    pub fn resolve_or_create(&mut self, probe: &VideoProbe, now: Ticks) -> (String, VideoUserStats, bool) {
        if let Some(r) = self.resolve(probe, now) {
            return (r.key, r.stats, r.migrated);
        }
        let k = key(probe.fingerprint.as_deref().unwrap_or(&probe.path));
        let created = placeholder(&probe.path, now, probe.fingerprint.clone());
        self.set_video(created.clone(), &k);
        (k, created, true)
    }

    fn replace_list_path(&mut self, old: &str, new: &str, now: Ticks) {
        let old_key = ci(old);
        for list in &mut self.custom_lists {
            if !list.video_paths.iter().any(|p| ci(p) == old_key) {
                continue;
            }
            let mut seen = HashSet::new();
            list.video_paths = list
                .video_paths
                .iter()
                .map(|p| if ci(p) == old_key { new.to_string() } else { p.clone() })
                .filter(|p| seen.insert(ci(p)))
                .collect();
            list.updated_at = now;
        }
    }

    // ----- thumbnails (legacy vault map)

    /// By the record, then by fingerprint, then by path; never migrates.
    pub fn thumbnail(&self, probe: &VideoProbe) -> Option<Vec<u8>> {
        if let Some(k) = self.find(probe)
            && let Some(data) = self.thumbnails.get(&k)
        {
            return Some(data.clone());
        }
        if let Some(fp) = &probe.fingerprint
            && let Some(data) = self.thumbnails.get(&key(fp))
        {
            return Some(data.clone());
        }
        self.thumbnails.get(&probe.path_key).cloned()
    }

    pub fn remove_thumbnail(&mut self, k: &str) -> Option<Vec<u8>> {
        let k = key(k);
        self.thumbnail_raw_keys.remove(&k);
        self.thumbnails.remove(&k)
    }

    /// Cache keys of a probe's picture, best first: lowercased fingerprint, then the lowercased path.
    pub fn thumbnail_keys_for_probe(probe: &VideoProbe) -> Vec<String> {
        let mut keys = Vec::with_capacity(2);
        if let Some(fp) = &probe.fingerprint {
            keys.push(fp.to_lowercase());
        }
        keys.push(probe.path_key_raw.clone());
        keys
    }

    /// Cache keys of a record's picture, best first.
    pub fn thumbnail_keys_for_record(video: &VideoUserStats) -> Vec<String> {
        let mut keys = Vec::with_capacity(2);
        if let Some(fp) = video.content_fingerprint.as_deref()
            && !fp.is_empty()
        {
            keys.push(fp.to_lowercase());
        }
        keys.push(video.video_path.to_lowercase());
        keys
    }

    /// Takes every legacy picture out (to move into the cache) and drops records that only existed to hold one.
    pub fn extract_thumbnails(&mut self) -> Vec<(String, Vec<u8>)> {
        if self.thumbnails.is_empty() {
            return Vec::new();
        }
        let listed: HashSet<String> = self.custom_lists.iter().flat_map(|l| l.video_paths.iter().map(|p| ci(p))).collect();
        let mut extracted = Vec::new();
        let pictures: Vec<(String, Vec<u8>)> = self.thumbnails.drain().collect();
        for (k, data) in pictures {
            let Some(video) = self.videos.get(&k).cloned() else {
                // Not tied to a record: its key is already a fingerprint or a path (cached under its raw form).
                let raw = self.thumbnail_raw_keys.remove(&k).unwrap_or_else(|| k.clone());
                extracted.push((raw, data));
                continue;
            };
            extracted.push((Self::thumbnail_keys_for_record(&video).remove(0), data));
            let engaged = video.view_count > 0
                || video.last_watched_at.is_some()
                || video.is_liked
                || video.rating > 0
                || video.last_position_seconds > 0.0
                || listed.contains(&ci(&video.video_path));
            if !engaged {
                self.remove_video(&k);
            }
        }
        self.thumbnail_raw_keys.clear();
        extracted
    }

    // ----- lists

    /// First list whose id matches ignoring case.
    pub fn list_index(&self, id: &str) -> Option<usize> {
        let id = ci(id);
        self.custom_lists.iter().position(|l| ci(&l.id) == id)
    }

    /// Whether a view of this vault would look different from one of `other` (position, duration, update time,
    /// list timestamps and thumbnails ignored; last-watched compared to the millisecond).
    pub fn differs_visibly(&self, other: &VaultContents) -> bool {
        if self.videos.len() != other.videos.len() || self.custom_lists.len() != other.custom_lists.len() {
            return true;
        }
        for (mine, theirs) in self.custom_lists.iter().zip(&other.custom_lists) {
            if mine.id != theirs.id || mine.name != theirs.name || mine.video_paths != theirs.video_paths {
                return true;
            }
        }
        for (k, mine) in &self.videos {
            let Some(theirs) = other.videos.get(k) else { return true };
            let mut a = mine.clone();
            let mut b = theirs.clone();
            for v in [&mut a, &mut b] {
                v.last_position_seconds = 0.0;
                v.duration_seconds = 0.0;
                v.updated_at = Ticks::MIN;
            }
            if let (Some(x), Some(y)) = (a.last_watched_at, b.last_watched_at)
                && (x.0 - y.0).abs() < 10_000
            {
                b.last_watched_at = Some(x);
            }
            if a != b {
                return true;
            }
        }
        false
    }
}

/// §8.3: two records of one file become one.
pub fn merged(a: &VideoUserStats, b: &VideoUserStats) -> VideoUserStats {
    let newer = if a.updated_at >= b.updated_at { a } else { b };
    let watched_last = match (a.last_watched_at, b.last_watched_at) {
        (Some(x), Some(y)) => {
            if x >= y {
                a
            } else {
                b
            }
        }
        (Some(_), None) => a,
        (None, Some(_)) => b,
        (None, None) => newer,
    };
    let mut out = newer.clone();
    out.content_fingerprint =
        newer.content_fingerprint.clone().or_else(|| a.content_fingerprint.clone()).or_else(|| b.content_fingerprint.clone());
    out.view_count = a.view_count + b.view_count;
    out.is_liked = a.is_liked || b.is_liked;
    out.rating = a.rating.max(b.rating);
    out.last_watched_at = match (a.last_watched_at, b.last_watched_at) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    };
    out.last_position_seconds = watched_last.last_position_seconds;
    out.duration_seconds = if watched_last.duration_seconds > 0.0 {
        watched_last.duration_seconds
    } else {
        a.duration_seconds.max(b.duration_seconds)
    };
    out.updated_at = a.updated_at.max(b.updated_at);
    out.thumbnail = None;
    out
}
