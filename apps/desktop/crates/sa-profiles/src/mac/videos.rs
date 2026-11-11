//! Engagement of the unlocked profile (mac-librarykit §9.11). Logged out every call returns the logged-out value
//! **before** a probe is built (UNIFIED.md §3.12 FIX-M: no file is read while logged out), takes no lock and writes
//! nothing. The `try_*` methods report write errors like the Swift API; the `ProfileStore` trait swallows them.

use std::path::Path;
use std::time::Duration;

use super::MacProfileStore;
use super::paths::ci;
use super::state::{Effect, PendingProgress};
use super::vault::{MAX_THUMBNAIL_BYTES, VaultContents, placeholder};
use crate::{FingerprintMode, ProfileError, VideoUserStats};

fn sanitized(candidate: f64, fallback: f64) -> f64 {
    if candidate.is_finite() && candidate > 0.0 {
        candidate
    } else if fallback.is_finite() && fallback > 0.0 {
        fallback
    } else {
        0.0
    }
}

/// Position (0 is a valid reset) and duration for a progress update; the position never passes the duration.
fn progress(position: f64, duration: f64, existing: &VideoUserStats) -> (f64, f64) {
    let clean_duration = sanitized(duration, existing.duration_seconds);
    let mut clean_position = (if position.is_finite() { position } else { existing.last_position_seconds }).max(0.0);
    if clean_duration > 0.0 && clean_position > clean_duration {
        clean_position = clean_duration;
    }
    (clean_position, clean_duration)
}

fn is_significant(position: f64, duration: f64, existing: &VideoUserStats) -> bool {
    (position - existing.last_position_seconds).abs() >= 0.5 || (duration - existing.duration_seconds).abs() >= 0.5
}

impl MacProfileStore {
    /// Counts a view (`view_count + 1`, last watched now, resume point). A 0 or non-finite position/duration keeps
    /// the stored one; a position past the duration is cut. Written at once with anything pending.
    pub fn try_record_watch(&self, path: &Path, position: f64, duration: f64) -> Result<Option<VideoUserStats>, ProfileError> {
        self.try_record_watch_in(None, path, position, duration)
    }

    /// [`Self::try_record_watch`], counted only while the session generation still equals `expected` (checked
    /// under the state mutex, inside the transaction): a login switch after the caller captured the generation
    /// records nothing in the new profile.
    pub fn try_record_watch_in(
        &self,
        expected: Option<u64>,
        path: &Path,
        position: f64,
        duration: f64,
    ) -> Result<Option<VideoUserStats>, ProfileError> {
        if !self.logged_in() {
            return Ok(None);
        }
        let Some(probe) = self.lookup_probe(path, FingerprintMode::Compute) else { return Ok(None) };
        self.transact(
            true,
            || None,
            |state| {
                if expected.is_some_and(|g| g != state.session_generation) {
                    return Ok((None, Effect::NONE));
                }
                // Read inside the transaction, after the file lock (Swift `clock()` in the body).
                let now = self.now();
                let contents = &mut state.session.as_mut().expect("session checked").contents;
                let (k, existing, _) = contents.resolve_or_create(&probe, now);
                let clean_duration = sanitized(duration, existing.duration_seconds);
                let mut clean_position = sanitized(position, existing.last_position_seconds);
                if clean_duration > 0.0 && clean_position > clean_duration {
                    clean_position = clean_duration;
                }
                let mut updated = existing;
                updated.view_count += 1;
                updated.last_position_seconds = clean_position;
                updated.duration_seconds = clean_duration;
                updated.last_watched_at = Some(now);
                updated.updated_at = now;
                contents.set_video(updated, &k);
                Ok((contents.stats_for_key(&k), Effect::VAULT))
            },
        )
    }

    /// Stores the resume point without counting a view and never raises `Local`. Applied in memory at once when it
    /// moved by ≥ 0.5 s; written at most every 15 s (`try_flush_playback_progress` writes at once).
    pub fn try_update_playback_progress(
        &self,
        path: &Path,
        position: f64,
        duration: f64,
    ) -> Result<Option<VideoUserStats>, ProfileError> {
        self.try_update_playback_progress_in(None, path, position, duration)
    }

    /// [`Self::try_update_playback_progress`], applied only while the session generation still equals `expected`
    /// (checked under the state mutex): a login switch after the caller captured the generation records nothing in
    /// the new profile.
    pub fn try_update_playback_progress_in(
        &self,
        expected: Option<u64>,
        path: &Path,
        position: f64,
        duration: f64,
    ) -> Result<Option<VideoUserStats>, ProfileError> {
        if !self.logged_in() {
            return Ok(None);
        }
        let Some(probe) = self.lookup_probe(path, FingerprintMode::Compute) else { return Ok(None) };
        self.sync_and_restore();
        let applied = {
            let mut l = self.lock();
            if expected.is_some_and(|g| g != l.state.session_generation) {
                return Ok(None);
            }
            let now = self.now();
            let Some(session) = l.state.session.as_mut() else { return Ok(None) };
            let resolution = session.contents.resolve(&probe, now);
            if resolution.as_ref().is_some_and(|r| r.migrated) {
                session.dirty = true;
                session.migrated = true;
            }
            let existing =
                resolution.as_ref().map(|r| r.stats.clone()).unwrap_or_else(|| placeholder(&probe.path, now, probe.fingerprint.clone()));
            let (clean_position, clean_duration) = progress(position, duration, &existing);
            if !is_significant(clean_position, clean_duration, &existing) {
                resolution.and_then(|r| session.contents.stats_for_key(&r.key)).unwrap_or(existing)
            } else {
                let (k, current, _) = session.contents.resolve_or_create(&probe, now);
                let mut updated = current;
                updated.last_position_seconds = clean_position;
                updated.duration_seconds = clean_duration;
                updated.updated_at = now;
                session.contents.set_video(updated, &k);
                session.dirty = true;
                session.pending_progress.insert(
                    k.clone(),
                    PendingProgress { probe: probe.clone(), position: clean_position, duration: clean_duration, at: now },
                );
                session.contents.stats_for_key(&k).unwrap_or(existing)
            }
        };
        self.persist_dirty(Self::PROGRESS_SAVE_INTERVAL)?;
        Ok(Some(applied))
    }

    /// Writes held progress and followed moves now; false when there is nothing to write. Never raises `Local`.
    pub fn try_flush_playback_progress(&self) -> Result<bool, ProfileError> {
        self.persist_dirty(Duration::ZERO)
    }

    /// Writes moves that reads followed in memory (with any progress); whether anything was written.
    pub fn try_persist_pending_migrations(&self) -> Result<bool, ProfileError> {
        self.persist_dirty(Duration::ZERO)
    }

    /// Flips the favorite flag; the new state (false logged out).
    pub fn try_toggle_like(&self, path: &Path) -> Result<bool, ProfileError> {
        Ok(self.update(path, |v| v.is_liked = !v.is_liked)?.is_some_and(|v| v.is_liked))
    }

    pub fn try_set_liked(&self, path: &Path, liked: bool) -> Result<Option<VideoUserStats>, ProfileError> {
        self.update(path, |v| v.is_liked = liked)
    }

    /// Stars clamped to 0..=5; 0 clears.
    pub fn try_set_rating(&self, path: &Path, rating: i64) -> Result<Option<VideoUserStats>, ProfileError> {
        let rating = rating.clamp(0, 5);
        self.update(path, |v| v.rating = rating)
    }

    /// The stats of a file, following it first if it moved (in memory; saved later). `None` logged out or untouched.
    pub fn stats_mode(&self, path: &Path, mode: FingerprintMode) -> Option<VideoUserStats> {
        if !self.logged_in() {
            return None;
        }
        let probe = self.lookup_probe(path, mode)?;
        let contents = self.read_migrating(std::slice::from_ref(&probe))?;
        contents.find(&probe).and_then(|k| contents.stats_for_key(&k))
    }

    /// Resets the history: records neither liked, rated nor listed are dropped with their pictures; the others
    /// lose views, last-watched and resume point.
    pub fn try_clear_watch_history(&self) -> Result<bool, ProfileError> {
        let mut forgotten: Vec<String> = Vec::new();
        let mut owner = None;
        let cleared = self.transact(
            true,
            || false,
            |state| {
                // Read inside the transaction, after the file lock (Swift `clock()` in the body).
                let now = self.now();
                let session = state.session.as_mut().expect("session checked");
                owner = Some((session.thumbnail_profile.clone(), session.vault_key.clone()));
                let listed: std::collections::HashSet<String> =
                    session.contents.custom_lists.iter().flat_map(|l| l.video_paths.iter().map(|p| ci(p))).collect();
                let records: Vec<(String, VideoUserStats)> =
                    session.contents.videos().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                for (k, video) in records {
                    if !video.is_liked && video.rating <= 0 && !listed.contains(&ci(&video.video_path)) {
                        session.contents.remove_video(&k);
                        session.contents.remove_thumbnail(&k);
                        forgotten.extend(VaultContents::thumbnail_keys_for_record(&video));
                    } else {
                        let mut reset = video;
                        reset.view_count = 0;
                        reset.last_watched_at = None;
                        reset.last_position_seconds = 0.0;
                        reset.updated_at = now;
                        session.contents.set_video(reset, &k);
                    }
                }
                session.pending_progress.clear();
                Ok((true, Effect::VAULT))
            },
        )?;
        if cleared
            && let Some((profile, key)) = owner
            && !forgotten.is_empty()
        {
            self.thumbnails.remove(&profile, &forgotten, &key);
        }
        Ok(cleared)
    }

    /// Stores a card picture in the encrypted cache (never the vault, never a record, never `Local`). Ignored when
    /// empty, over 512 KiB, logged out or for a blank path.
    pub fn try_set_thumbnail(&self, path: &Path, bytes: &[u8]) -> Result<bool, ProfileError> {
        self.try_set_thumbnail_in(None, path, bytes)
    }

    /// [`Self::try_set_thumbnail`], stored only when the session generation still equals `expected` at the moment
    /// the profile and key are taken (under the state mutex; the picture then goes to that profile's cache even if
    /// a switch follows).
    pub fn try_set_thumbnail_in(&self, expected: Option<u64>, path: &Path, bytes: &[u8]) -> Result<bool, ProfileError> {
        if bytes.is_empty() || bytes.len() > MAX_THUMBNAIL_BYTES {
            return Ok(false);
        }
        let session = self.read(|s| {
            if expected.is_some_and(|g| g != s.session_generation) {
                return None;
            }
            s.session.as_ref().map(|x| (x.thumbnail_profile.clone(), x.vault_key.clone()))
        });
        let Some((profile, key)) = session else { return Ok(false) };
        let Some(probe) = self.probe(path, FingerprintMode::Compute) else { return Ok(false) };
        let cache_key = VaultContents::thumbnail_keys_for_probe(&probe).remove(0);
        Ok(self.thumbnails.store(bytes, &profile, &cache_key, &key))
    }

    /// The stored picture: from the cache, else from a legacy vault map. `None` logged out without touching the file.
    pub fn thumbnail_mode(&self, path: &Path, mode: FingerprintMode) -> Option<Vec<u8>> {
        let (profile, key) = self.thumbnail_session()?;
        let probe = self.probe(path, mode)?;
        if let Some(data) = self.thumbnails.data(&profile, &VaultContents::thumbnail_keys_for_probe(&probe), &key) {
            return Some(data);
        }
        self.read(|s| s.session.as_ref().and_then(|x| x.contents.thumbnail(&probe)))
    }

    fn thumbnail_session(&self) -> Option<(String, sa_core::secrets::SecretKey32)> {
        self.read(|s| s.session.as_ref().map(|x| (x.thumbnail_profile.clone(), x.vault_key.clone())))
    }

    /// One record edit: find or create, change, stamp, save.
    fn update(&self, path: &Path, change: impl FnOnce(&mut VideoUserStats)) -> Result<Option<VideoUserStats>, ProfileError> {
        if !self.logged_in() {
            return Ok(None);
        }
        let Some(probe) = self.lookup_probe(path, FingerprintMode::Compute) else { return Ok(None) };
        self.transact(
            true,
            || None,
            |state| {
                // Read inside the transaction, after the file lock (Swift `clock()` in the body).
                let now = self.now();
                let contents = &mut state.session.as_mut().expect("session checked").contents;
                let (k, existing, _) = contents.resolve_or_create(&probe, now);
                let mut updated = existing;
                change(&mut updated);
                updated.updated_at = now;
                contents.set_video(updated, &k);
                Ok((contents.stats_for_key(&k), Effect::VAULT))
            },
        )
    }
}
