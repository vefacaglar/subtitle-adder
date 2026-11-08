//! Authentication (mac-librarykit §9.8–§9.10): register, login, logout, delete, password checks and the
//! remember-me ↔ secret store steps. The secret store is never called while the state mutex or the file lock is
//! held; PBKDF2 never runs under either.

use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine as _;
use sa_core::secrets::{SecretBytes, SecretKey32, SecretRead};

use super::paths::{ci, trim_ws_nl};
use super::state::{Effect, FileProblem, ProfileRecord, ProfileSession, ProfileState};
use super::vault::VaultContents;
use super::{MacProfileStore, open_vault};
use crate::ProfileError;
use crate::crypto;

/// `profile-remember.<lowercased username>`.
pub fn remember_account(username: &str) -> String {
    format!("profile-remember.{}", username.to_lowercase())
}

/// A process-wide random salt for the decoy derivation of unknown users.
static DECOY_SALT: LazyLock<[u8; 16]> = LazyLock::new(crypto::random_salt);

/// `ProfileCrypto.derive` with the LibraryKit rules: `iterations <= 0` → 100 000; more than 10 000 000 or an empty
/// salt → `CryptoFailure`.
pub fn derive_checked(password: &str, salt: &[u8], iterations: i64) -> Result<([u8; 32], SecretKey32), ProfileError> {
    let rounds = if iterations > 0 { iterations } else { i64::from(MacProfileStore::DEFAULT_ITERATIONS) };
    if rounds > MacProfileStore::MAX_ITERATIONS || salt.is_empty() {
        return Err(ProfileError::CryptoFailure);
    }
    Ok(crypto::derive(password, salt, rounds as u32))
}

/// Compares without returning early (false for different lengths).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

/// What the secret store held for an account before a login touched it, and whether the key is stored now.
struct RememberStep {
    account: String,
    before: Before,
    stored: bool,
}

enum Before {
    Found(SecretBytes),
    Absent,
    Unavailable,
}

impl MacProfileStore {
    /// Checks a password in constant time; an unknown user or malformed record still pays for one PBKDF2.
    fn verify_password(&self, password: &str, record: Option<&ProfileRecord>) -> Result<SecretKey32, ProfileError> {
        if let Some(record) = record
            && let Some(salt) = unb64(&record.salt_base64).filter(|s| !s.is_empty())
            && let Some(expected) = unb64(&record.password_hash_base64).filter(|h| !h.is_empty())
            && record.iterations <= MacProfileStore::MAX_ITERATIONS
        {
            let (hash, key) = derive_checked(password, &salt, record.iterations)?;
            return if constant_time_eq(&hash, &expected) { Ok(key) } else { Err(ProfileError::InvalidCredentials) };
        }
        let _ = crypto::derive(password, &*DECOY_SALT, MacProfileStore::DEFAULT_ITERATIONS);
        Err(ProfileError::InvalidCredentials)
    }

    /// Stores the vault key for a remembered login (or only notes what was there). Outside every lock.
    fn store_remember_key(&self, remember: bool, username: &str, key: &SecretKey32) -> RememberStep {
        let account = remember_account(username);
        let before = match self.secrets.read(&account) {
            SecretRead::Found(bytes) => Before::Found(bytes),
            SecretRead::Absent => Before::Absent,
            SecretRead::Unavailable => Before::Unavailable,
        };
        let stored = remember && self.secrets.set(key.expose(), &account);
        RememberStep { account, before, stored }
    }

    /// The in-memory half of remember-me, inside the transaction.
    fn apply_remember_choice(remember: bool, step: &RememberStep, state: &mut ProfileState, session: &mut ProfileSession) {
        state.remember_me = remember;
        if step.stored {
            state.remembered_username = Some(session.username.clone());
            session.remembered = true;
        } else {
            state.remembered_username = None;
            session.remembered = false;
        }
    }

    /// Puts the secret store back as it was when the transaction failed.
    fn roll_back(&self, step: &RememberStep) {
        match &step.before {
            Before::Found(bytes) => {
                self.secrets.set(bytes.expose(), &step.account);
            }
            Before::Absent => self.secrets.remove(&step.account),
            // Never delete what could not be read; only undo our own write.
            Before::Unavailable => {
                if step.stored {
                    self.secrets.remove(&step.account);
                }
            }
        }
    }

    /// After a successful transaction: drop a key that did not become the remembered one and the previous one.
    fn finish(&self, step: &RememberStep, previously: Option<String>) {
        if !step.stored {
            self.secrets.remove(&step.account);
        }
        if let Some(previous) = previously {
            let account = remember_account(&previous);
            if account != step.account {
                self.secrets.remove(&account);
            }
        }
    }

    /// Creates a profile and logs in as it (one PBKDF2; call off the UI thread).
    pub fn register_user(&self, username: &str, password: &str, remember: bool) -> Result<(), ProfileError> {
        let name = trim_ws_nl(username).to_string();
        if name.is_empty() {
            return Err(ProfileError::EmptyUsername);
        }
        if password.is_empty() {
            return Err(ProfileError::EmptyPassword);
        }
        let name_key = ci(&name);
        if self.read(|s| s.profiles.contains_key(&name_key)) {
            return Err(ProfileError::UsernameTaken);
        }
        let salt = crypto::random_salt();
        let (hash, key) = derive_checked(password, &salt, i64::from(MacProfileStore::DEFAULT_ITERATIONS))?;
        let created = self.now();

        let _ = self.persist_dirty(Duration::ZERO);
        let step = self.store_remember_key(remember, &name, &key);
        let mut previously = None;
        let result = self.transact(
            false,
            || (),
            |state| {
                if state.profiles.contains_key(&name_key) {
                    return Err(ProfileError::UsernameTaken);
                }
                state.profiles.insert(
                    name_key.clone(),
                    ProfileRecord {
                        username: name.clone(),
                        salt_base64: b64(&salt),
                        password_hash_base64: b64(&hash),
                        iterations: i64::from(MacProfileStore::DEFAULT_ITERATIONS),
                        created_at: created,
                        encrypted_vault_base64: None,
                    },
                );
                let mut session = ProfileSession::new(name_key.clone(), name.clone(), key.clone(), VaultContents::new(), None, false)
                    .with_thumbnail_profile(&name);
                previously = state.remembered_username.clone();
                Self::apply_remember_choice(remember, &step, state, &mut session);
                state.start_session(session);
                state.last_username = Some(name.clone());
                Ok(((), Effect::VAULT_HEADER))
            },
        );
        match result {
            Err(e) => {
                self.roll_back(&step);
                Err(e)
            }
            Ok(()) => {
                self.inner.lock().user_logged_out = false;
                self.finish(&step, previously);
                Ok(())
            }
        }
    }

    /// Unlocks a profile (case-insensitive name, registered casing kept; one PBKDF2).
    pub fn login_user(&self, username: &str, password: &str, remember: bool) -> Result<(), ProfileError> {
        let name = trim_ws_nl(username).to_string();
        if name.is_empty() || password.is_empty() {
            return Err(ProfileError::MissingCredentials);
        }
        let name_key = ci(&name);
        let (candidate, problem) = self.read(|s| (s.profiles.get(&name_key).cloned(), s.file_problem));
        if candidate.is_none() && problem == Some(FileProblem::Unreadable) {
            return Err(ProfileError::FileUnreadable);
        }
        let key = self.verify_password(password, candidate.as_ref())?;

        let _ = self.persist_dirty(Duration::ZERO);
        let account_name = candidate.as_ref().map(|c| c.username.clone()).unwrap_or_else(|| name.clone());
        let step = self.store_remember_key(remember, &account_name, &key);
        let mut previously = None;
        let result = self.transact(
            false,
            || (),
            |state| {
                let current = match state.profiles.get(&name_key) {
                    Some(c)
                        if Some(&c.salt_base64) == candidate.as_ref().map(|x| &x.salt_base64)
                            && Some(&c.password_hash_base64) == candidate.as_ref().map(|x| &x.password_hash_base64) =>
                    {
                        c.clone()
                    }
                    _ => return Err(ProfileError::InvalidCredentials),
                };
                let contents = open_vault(&current, &key).map_err(|_| ProfileError::VaultUnreadable)?;
                let mut session = ProfileSession::new(
                    name_key.clone(),
                    current.username.clone(),
                    key.clone(),
                    contents,
                    current.encrypted_vault_base64.clone(),
                    false,
                )
                .with_thumbnail_profile(&name);
                previously = state.remembered_username.clone();
                Self::apply_remember_choice(remember, &step, state, &mut session);
                state.start_session(session);
                state.last_username = Some(current.username.clone());
                Ok(((), Effect::HEADER))
            },
        );
        match result {
            Err(e) => {
                self.roll_back(&step);
                Err(e)
            }
            Ok(()) => {
                self.inner.lock().user_logged_out = false;
                self.finish(&step, previously);
                Ok(())
            }
        }
    }

    /// Locks the profile in memory and forgets the remembered login for every instance; the local logout always
    /// happens, the error only says it could not be written. Blocks automatic restores until the next login.
    pub fn logout_user(&self) -> Result<(), ProfileError> {
        self.inner.lock().user_logged_out = true;
        let _ = self.persist_dirty(Duration::ZERO);

        // 1. Lock and forget in memory.
        let forgotten: Vec<String> = {
            let mut l = self.lock();
            self.sync_locked(&mut l, false);
            if l.state.session.is_none() && l.state.remembered_username.is_none() {
                return Ok(());
            }
            let names = [l.state.session.as_ref().map(|s| s.username.clone()), l.state.remembered_username.clone()]
                .into_iter()
                .flatten()
                .collect();
            l.state.end_session();
            l.state.remembered_username = None;
            l.events.push(crate::ProfileChange::Local);
            names
        };
        // 2. Remove the keys, outside every lock.
        for name in &forgotten {
            self.secrets.remove(&remember_account(name));
        }
        // 3. Write the forgotten state (file lock first, without the mutex).
        let mut file_lock = super::fileio::FileLock::acquire(&self.lock_path, self.lock_timeout)?;
        let (meanwhile, result) = {
            let mut l = self.lock();
            self.sync_locked(&mut l, false);
            let meanwhile = l.state.remembered_username.clone();
            l.state.end_session();
            l.state.remembered_username = None;
            let mut working = l.state.clone();
            let result = self.save_locked(&mut working, false);
            if result.is_ok() {
                l.state = working;
            }
            (meanwhile, result)
        };
        file_lock.release();
        // A login another instance remembered meanwhile was cleared too; its key goes either way.
        if let Some(name) = meanwhile {
            self.secrets.remove(&remember_account(&name));
        }
        result
    }

    /// Deletes a profile after checking its password; ends the session if it is the active one.
    pub fn delete_user(&self, username: &str, password: &str) -> Result<(), ProfileError> {
        let name = trim_ws_nl(username).to_string();
        if name.is_empty() || password.is_empty() {
            return Err(ProfileError::MissingCredentials);
        }
        let name_key = ci(&name);
        let (candidate, problem) = self.read(|s| (s.profiles.get(&name_key).cloned(), s.file_problem));
        if candidate.is_none() && problem == Some(FileProblem::Unreadable) {
            return Err(ProfileError::FileUnreadable);
        }
        let _ = self.verify_password(password, candidate.as_ref())?;
        self.transact(
            false,
            || (),
            |state| {
                match state.profiles.get(&name_key) {
                    Some(c)
                        if Some(&c.salt_base64) == candidate.as_ref().map(|x| &x.salt_base64)
                            && Some(&c.password_hash_base64) == candidate.as_ref().map(|x| &x.password_hash_base64) => {}
                    _ => return Err(ProfileError::InvalidCredentials),
                }
                state.profiles.remove(&name_key);
                if state.session.as_ref().is_some_and(|s| s.username_key == name_key) {
                    state.end_session();
                }
                if state.remembered_username.as_deref().map(ci).as_deref() == Some(name_key.as_str()) {
                    state.remembered_username = None;
                }
                Ok(((), Effect::HEADER))
            },
        )?;
        self.thumbnails.remove_all(&name.to_lowercase());
        self.secrets.remove(&remember_account(&name));
        Ok(())
    }
}
