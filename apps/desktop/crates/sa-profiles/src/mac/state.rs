//! In-memory state of a `MacProfileStore` (mac-librarykit §9.2): the profile table, the header fields, the unlocked
//! session and what the store knows about the file.

use std::collections::HashMap;

use sa_core::secrets::SecretKey32;

use super::document::{ProfileDto, ProfilesDocument};
use super::fileio::{FileSignature, Snapshot};
use super::paths::{ci, trim_ws, trim_ws_nl};
use super::vault::{VaultContents, VideoProbe};
use crate::Ticks;

/// One entry of `profiles.json`: the password check and, until it is opened, the vault ciphertext.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileRecord {
    pub username: String,
    pub salt_base64: String,
    pub password_hash_base64: String,
    pub iterations: i64,
    pub created_at: Ticks,
    pub encrypted_vault_base64: Option<String>,
}

/// Playback progress held in memory but not yet in the file.
#[derive(Debug, Clone)]
pub struct PendingProgress {
    pub probe: VideoProbe,
    pub position: f64,
    pub duration: f64,
    pub at: Ticks,
}

/// The unlocked profile: its vault key (zeroized on drop) and decrypted contents (zeroized on drop).
#[derive(Debug, Clone)]
pub struct ProfileSession {
    /// `ci(username)`, the key into the profile table.
    pub username_key: String,
    /// Registered casing.
    pub username: String,
    pub vault_key: SecretKey32,
    pub contents: VaultContents,
    /// The ciphertext `contents` was loaded from or last saved as.
    pub loaded_ciphertext: Option<String>,
    /// Stands for the shared remember-me login (logging out elsewhere ends it here too).
    pub remembered: bool,
    /// The profile id of the picture cache: the name lowercased exactly as Swift `usernameKey` was (typed name at
    /// login/register, registered name on a restore), **without** NFC, because the cache folder name is a hash of
    /// those bytes (mac-librarykit §2.3 exception).
    pub thumbnail_profile: String,
    pub dirty: bool,
    pub migrated: bool,
    pub pending_progress: HashMap<String, PendingProgress>,
}

impl ProfileSession {
    pub fn new(
        username_key: String,
        username: String,
        vault_key: SecretKey32,
        contents: VaultContents,
        loaded_ciphertext: Option<String>,
        remembered: bool,
    ) -> Self {
        Self {
            thumbnail_profile: username_key.clone(),
            username_key,
            username,
            vault_key,
            contents,
            loaded_ciphertext,
            remembered,
            dirty: false,
            migrated: false,
            pending_progress: HashMap::new(),
        }
    }

    /// Sets the picture-cache profile id from the name Swift lowercased for `usernameKey`.
    pub fn with_thumbnail_profile(mut self, name: &str) -> Self {
        self.thumbnail_profile = name.to_lowercase();
        self
    }
}

/// Why `profiles.json` cannot simply be overwritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileProblem {
    /// Not valid JSON of the expected shape: set aside before the next write.
    Corrupt,
    /// Exists but could not be read: never written over; every sync reads it again.
    Unreadable,
    UnsupportedVersion(i64),
}

#[derive(Debug, Clone)]
pub struct ProfileState {
    pub profiles: HashMap<String, ProfileRecord>,
    pub remember_me: bool,
    pub last_username: Option<String>,
    pub remembered_username: Option<String>,
    pub session: Option<ProfileSession>,
    pub session_generation: u64,
    pub has_synced: bool,
    pub last_snapshot: Option<Snapshot>,
    pub last_signature: Option<FileSignature>,
    pub file_problem: Option<FileProblem>,
    pub sync_generation: i64,
    pub restore_attempted_generation: i64,
    pub last_vault_save_attempt: Option<Ticks>,
}

impl Default for ProfileState {
    fn default() -> Self {
        Self {
            profiles: HashMap::new(),
            remember_me: true,
            last_username: None,
            remembered_username: None,
            session: None,
            session_generation: 0,
            has_synced: false,
            last_snapshot: None,
            last_signature: None,
            file_problem: None,
            sync_generation: 0,
            restore_attempted_generation: -1,
            last_vault_save_attempt: None,
        }
    }
}

impl ProfileState {
    pub fn start_session(&mut self, session: ProfileSession) {
        self.session = Some(session);
        self.session_generation += 1;
    }

    pub fn end_session(&mut self) {
        if self.session.take().is_some() {
            self.session_generation += 1;
        }
    }

    /// The remembered username as the apps treat it: only while remember-me is on.
    pub fn effective_remembered(&self) -> Option<&str> {
        if self.remember_me { self.remembered_username.as_deref() } else { None }
    }

    pub fn document(&self) -> ProfilesDocument {
        ProfilesDocument {
            remember_me: self.remember_me,
            last_username: self.last_username.clone(),
            remembered_username: self.effective_remembered().map(str::to_string),
            profiles: self
                .profiles
                .values()
                .map(|r| ProfileDto {
                    username: r.username.clone(),
                    salt_base64: r.salt_base64.clone(),
                    password_hash_base64: r.password_hash_base64.clone(),
                    iterations: r.iterations,
                    created_at: r.created_at,
                    encrypted_vault_base64: r.encrypted_vault_base64.clone(),
                })
                .collect(),
        }
    }

    /// §5.3: replaces the profile table and the header fields with those of a parsed file.
    pub fn load(&mut self, doc: &ProfilesDocument) {
        self.remember_me = doc.remember_me;
        if let Some(last) = doc.last_username.as_deref().map(trim_ws_nl).filter(|s| !s.is_empty()) {
            self.last_username = Some(last.to_string());
        }
        self.remembered_username =
            doc.remembered_username.as_deref().map(trim_ws_nl).filter(|s| !s.is_empty()).map(str::to_string);
        let mut table = HashMap::new();
        for dto in &doc.profiles {
            let name = trim_ws_nl(&dto.username);
            if name.is_empty() || trim_ws(&dto.salt_base64).is_empty() || trim_ws(&dto.password_hash_base64).is_empty() {
                continue;
            }
            table.insert(
                ci(name),
                ProfileRecord {
                    username: name.to_string(),
                    salt_base64: dto.salt_base64.clone(),
                    password_hash_base64: dto.password_hash_base64.clone(),
                    iterations: if dto.iterations > 0 { dto.iterations } else { super::document::DEFAULT_ITERATIONS },
                    created_at: dto.created_at,
                    encrypted_vault_base64: dto.encrypted_vault_base64.clone(),
                },
            );
        }
        self.profiles = table;
    }
}

/// What a mutation did, so the store knows what to write and whether to tell the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Effect {
    /// Re-encrypt the active vault and save.
    pub vault: bool,
    /// Save `profiles.json` only.
    pub header: bool,
    /// Do not raise `ProfileChange::Local`.
    pub silent: bool,
}

impl Effect {
    pub const NONE: Effect = Effect { vault: false, header: false, silent: false };
    pub const VAULT: Effect = Effect { vault: true, header: false, silent: false };
    pub const HEADER: Effect = Effect { vault: false, header: true, silent: false };
    pub const VAULT_HEADER: Effect = Effect { vault: true, header: true, silent: false };
    pub const VAULT_SILENT: Effect = Effect { vault: true, header: false, silent: true };

    pub fn needs_save(self) -> bool {
        self.vault || self.header
    }
}
