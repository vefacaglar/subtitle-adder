//! Secret-store contract and zeroizing secret buffers (UNIFIED.md §5.2). Implementations
//! (`EncryptedSecretStore` over a DPAPI or 0600-key-file protector) live in `sa-providers`; the macOS profile store
//! uses the same contract for remember-me keys. No Keychain anywhere.

use zeroize::{Zeroize, Zeroizing};

/// Result of reading a secret. `Unavailable` is never treated as absent (callers never overwrite or delete on a
/// guess).
pub enum SecretRead {
    Found(SecretBytes),
    Absent,
    Unavailable,
}

impl std::fmt::Debug for SecretRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SecretRead::Found(_) => "Found([redacted])",
            SecretRead::Absent => "Absent",
            SecretRead::Unavailable => "Unavailable",
        })
    }
}

/// Accounts: `translation-provider.<UUID>` (both platforms), `profile-remember.<lower user>` (macOS).
pub trait SecretStore: Send + Sync {
    fn read(&self, account: &str) -> SecretRead;
    /// False on any failure (an invalid data key touches nothing).
    fn set(&self, bytes: &[u8], account: &str) -> bool;
    fn remove(&self, account: &str);
}

/// Zeroized-on-drop bytes; never printed.
#[derive(Clone, Default)]
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBytes([redacted])")
    }
}

/// Zeroized-on-drop string (passwords, API keys); never printed.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    pub fn new(s: impl Into<String>) -> Self {
        Self(Zeroizing::new(s.into()))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&str> for SecretString {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretString([redacted])")
    }
}

/// A 32-byte key (vault key, data key), zeroized on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey32([u8; 32]);

impl SecretKey32 {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        <[u8; 32]>::try_from(bytes).ok().map(Self)
    }
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Drop for SecretKey32 {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for SecretKey32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey32([redacted])")
    }
}

/// In-memory store (tests).
#[derive(Default)]
pub struct InMemorySecretStore {
    map: parking_lot::Mutex<std::collections::HashMap<String, SecretBytes>>,
}

impl SecretStore for InMemorySecretStore {
    fn read(&self, account: &str) -> SecretRead {
        match self.map.lock().get(account) {
            Some(v) => SecretRead::Found(v.clone()),
            None => SecretRead::Absent,
        }
    }
    fn set(&self, bytes: &[u8], account: &str) -> bool {
        self.map.lock().insert(account.to_owned(), SecretBytes::new(bytes.to_vec()));
        true
    }
    fn remove(&self, account: &str) {
        self.map.lock().remove(account);
    }
}
