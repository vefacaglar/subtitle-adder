//! Custom lists and the feed of the unlocked profile (mac-librarykit §9.12, §10). List names are trimmed and unique
//! by [`fold`] (case, diacritics, width, ı/i); ids match ignoring case.

use std::path::{Path, PathBuf};

use sa_core::folding::fold;

use super::MacProfileStore;
use super::feed;
use super::paths::{ci, trim_ws_nl};
use super::state::Effect;
use super::vault::VideoProbe;
use crate::{FingerprintMode, ProfileError, UserVideoList, VideoFeedFilter, VideoUserStats};

impl MacProfileStore {
    /// Creates an empty list: `None` logged out; `EmptyListName` (also logged out) / `DuplicateListName`.
    pub fn try_create_list(&self, name: &str) -> Result<Option<UserVideoList>, ProfileError> {
        let clean = trim_ws_nl(name).to_string();
        if clean.is_empty() {
            return Err(ProfileError::EmptyListName);
        }
        self.transact(
            true,
            || None,
            |state| {
                // Read inside the transaction, after the file lock (Swift `clock()` in the body).
                let now = self.now();
                let contents = &mut state.session.as_mut().expect("session checked").contents;
                let folded = fold(&clean);
                if contents.custom_lists.iter().any(|l| fold(&l.name) == folded) {
                    return Err(ProfileError::DuplicateListName);
                }
                let list = UserVideoList {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    name: clean.clone(),
                    video_paths: Vec::new(),
                    created_at: now,
                    updated_at: now,
                };
                contents.custom_lists.push(list.clone());
                Ok((Some(list), Effect::VAULT))
            },
        )
    }

    /// Renames a list (a case-only change of itself is fine); false logged out or for an unknown id.
    pub fn try_rename_list(&self, id: &str, name: &str) -> Result<bool, ProfileError> {
        let clean = trim_ws_nl(name).to_string();
        if clean.is_empty() {
            return Err(ProfileError::EmptyListName);
        }
        self.transact(
            true,
            || false,
            |state| {
                // Read inside the transaction, after the file lock (Swift `clock()` in the body).
                let now = self.now();
                let contents = &mut state.session.as_mut().expect("session checked").contents;
                let Some(index) = contents.list_index(id) else { return Ok((false, Effect::NONE)) };
                let folded = fold(&clean);
                if contents.custom_lists.iter().enumerate().any(|(i, l)| i != index && fold(&l.name) == folded) {
                    return Err(ProfileError::DuplicateListName);
                }
                contents.custom_lists[index].name = clean.clone();
                contents.custom_lists[index].updated_at = now;
                Ok((true, Effect::VAULT))
            },
        )
    }

    pub fn try_delete_list(&self, id: &str) -> Result<bool, ProfileError> {
        self.transact(
            true,
            || false,
            |state| {
                let contents = &mut state.session.as_mut().expect("session checked").contents;
                let Some(index) = contents.list_index(id) else { return Ok((false, Effect::NONE)) };
                contents.custom_lists.remove(index);
                Ok((true, Effect::VAULT))
            },
        )
    }

    /// Adds a video (idempotent); it also gets a record so a later move can be followed.
    pub fn try_add_video(&self, path: &Path, id: &str) -> Result<bool, ProfileError> {
        if !self.logged_in() {
            return Ok(false);
        }
        let Some(probe) = self.lookup_probe(path, FingerprintMode::Compute) else { return Ok(false) };
        self.transact(
            true,
            || false,
            |state| {
                // Read inside the transaction, after the file lock (Swift `clock()` in the body).
                let now = self.now();
                let contents = &mut state.session.as_mut().expect("session checked").contents;
                if contents.list_index(id).is_none() {
                    return Ok((false, Effect::NONE));
                }
                let (_, _, record_changed) = contents.resolve_or_create(&probe, now);
                let Some(index) = contents.list_index(id) else { return Ok((false, Effect::NONE)) };
                let list = &mut contents.custom_lists[index];
                if list.video_paths.iter().any(|p| ci(p) == probe.path_key) {
                    return Ok((true, if record_changed { Effect::VAULT } else { Effect::NONE }));
                }
                list.video_paths.push(probe.path.clone());
                list.updated_at = now;
                Ok((true, Effect::VAULT))
            },
        )
    }

    /// Removes a video; false if it was not in the list (or unknown list, or logged out).
    pub fn try_remove_video(&self, path: &Path, id: &str) -> Result<bool, ProfileError> {
        if !self.logged_in() {
            return Ok(false);
        }
        let Some(probe) = self.lookup_probe(path, FingerprintMode::Compute) else { return Ok(false) };
        self.transact(
            true,
            || false,
            |state| {
                // Read inside the transaction, after the file lock (Swift `clock()` in the body).
                let now = self.now();
                let contents = &mut state.session.as_mut().expect("session checked").contents;
                let migrated = contents.resolve(&probe, now).is_some_and(|r| r.migrated);
                let quiet = if migrated { Effect::VAULT_SILENT } else { Effect::NONE };
                let Some(index) = contents.list_index(id) else { return Ok((false, quiet)) };
                let list = &mut contents.custom_lists[index];
                let before = list.video_paths.len();
                list.video_paths.retain(|p| ci(p) != probe.path_key);
                if list.video_paths.len() == before {
                    return Ok((false, quiet));
                }
                list.updated_at = now;
                Ok((true, Effect::VAULT))
            },
        )
    }

    /// Adds the video if absent, removes it if present; whether it is in the list now.
    pub fn try_toggle_video(&self, path: &Path, id: &str) -> Result<bool, ProfileError> {
        if !self.logged_in() {
            return Ok(false);
        }
        let Some(probe) = self.lookup_probe(path, FingerprintMode::Compute) else { return Ok(false) };
        self.transact(
            true,
            || false,
            |state| {
                // Read inside the transaction, after the file lock (Swift `clock()` in the body).
                let now = self.now();
                let contents = &mut state.session.as_mut().expect("session checked").contents;
                if contents.list_index(id).is_none() {
                    return Ok((false, Effect::NONE));
                }
                let _ = contents.resolve_or_create(&probe, now);
                let Some(index) = contents.list_index(id) else { return Ok((false, Effect::NONE)) };
                let list = &mut contents.custom_lists[index];
                let is_in = match list.video_paths.iter().position(|p| ci(p) == probe.path_key) {
                    Some(position) => {
                        list.video_paths.remove(position);
                        false
                    }
                    None => {
                        list.video_paths.push(probe.path.clone());
                        true
                    }
                };
                list.updated_at = now;
                Ok((is_in, Effect::VAULT))
            },
        )
    }

    /// All lists in creation order (`[]` logged out).
    pub fn all_lists(&self) -> Vec<UserVideoList> {
        self.read(|s| s.session.as_ref().map(|x| x.contents.custom_lists.clone()).unwrap_or_default())
    }

    /// The lists that contain the video, after following it if it moved (in memory).
    pub fn lists_containing_mode(&self, path: &Path, mode: FingerprintMode) -> Vec<UserVideoList> {
        if !self.logged_in() {
            return Vec::new();
        }
        let Some(probe) = self.lookup_probe(path, mode) else { return Vec::new() };
        let Some(contents) = self.read_migrating(std::slice::from_ref(&probe)) else { return Vec::new() };
        contents.custom_lists.iter().filter(|l| l.video_paths.iter().any(|p| ci(p) == probe.path_key)).cloned().collect()
    }

    /// Whether the video is in the list, after following it if it moved.
    pub fn is_video_in_list(&self, path: &Path, id: &str, mode: FingerprintMode) -> bool {
        if !self.logged_in() {
            return false;
        }
        let Some(probe) = self.lookup_probe(path, mode) else { return false };
        let Some(contents) = self.read_migrating(std::slice::from_ref(&probe)) else { return false };
        let Some(index) = contents.list_index(id) else { return false };
        contents.custom_lists[index].video_paths.iter().any(|p| ci(p) == probe.path_key)
    }

    /// §10.1 with the privacy override: logged out → `[]` for every filter, and nothing is probed.
    /// `should_continue` is asked between files; false → `[]` at once.
    pub fn feed_with(
        &self,
        filter: &VideoFeedFilter,
        search: Option<&str>,
        discovered: &[PathBuf],
        mode: FingerprintMode,
        should_continue: Option<&dyn Fn() -> bool>,
    ) -> Vec<VideoUserStats> {
        if !self.logged_in() {
            return Vec::new();
        }
        let mut seen = std::collections::HashSet::new();
        let mut found: Vec<VideoProbe> = Vec::new();
        for path in discovered {
            if should_continue.is_some_and(|f| !f()) {
                return Vec::new();
            }
            if let Some(probe) = self.probe(path, mode)
                && seen.insert(probe.path_key.clone())
            {
                found.push(probe);
            }
        }
        let probes = self.prepared(found, should_continue);
        if should_continue.is_some_and(|f| !f()) {
            return Vec::new();
        }
        let now = self.now();
        match self.read_migrating(&probes) {
            Some(contents) => feed::narrowed(feed::items(&contents, filter, &probes, now), search),
            None => Vec::new(),
        }
    }
}
