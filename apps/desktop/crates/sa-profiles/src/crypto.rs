//! `ProfileCrypto`: byte-compatible primitives of both apps (win-profiles §4, mac-librarykit §4).
//!
//! * PBKDF2-HMAC-SHA256 → 64 bytes = `auth_hash(32) ‖ vault_key(32)` (C# `Rfc2898DeriveBytes.Pbkdf2`).
//! * AES-256-GCM blobs laid out as `nonce(12) ‖ tag(16) ‖ ciphertext` (C# `AesGcm` with a 16-byte tag, RustCrypto
//!   detached mode reassembled in the C# order).
//! * HMAC-SHA256 helpers (custom-list item hashes, storage keys, owner tags, IPC keys).

use aes_gcm::aead::{AeadInOut, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use hmac::{Hmac, Mac};
use sa_core::secrets::SecretKey32;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

pub const SALT_LEN: usize = 16;
pub const HASH_LEN: usize = 32;
pub const PBKDF2_ITERATIONS: u32 = 100_000;
pub const REMEMBER_ITERATIONS: u32 = 10_000;
pub const GCM_NONCE: usize = 12;
pub const GCM_TAG: usize = 16;
/// Bytes a sealed blob adds to its plaintext (`nonce ‖ tag`).
pub const BLOB_OVERHEAD: usize = GCM_NONCE + GCM_TAG;
/// `{"storage":"sqlite-v3"}` (Windows vault marker, 23 bytes).
pub const VAULT_MARKER: &str = "{\"storage\":\"sqlite-v3\"}";

/// `PBKDF2-HMAC-SHA256(password, salt, iterations, 64)` → `(auth_hash, vault_key)`. The password is used exactly as
/// typed (never trimmed).
pub fn derive(password: &str, salt: &[u8], iterations: u32) -> ([u8; 32], SecretKey32) {
    let mut derived = [0u8; 64];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, iterations.max(1), &mut derived);
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&derived[..32]);
    let mut key = [0u8; 32];
    key.copy_from_slice(&derived[32..]);
    derived.zeroize();
    let secret = SecretKey32::new(key);
    key.zeroize();
    (hash, secret)
}

/// Raw PBKDF2-HMAC-SHA256 with an arbitrary output length (remember-me wrap key: 10 000 rounds, 32 bytes).
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    pbkdf2::pbkdf2_hmac::<Sha256>(password, salt, iterations.max(1), out);
}

/// Constant-time compare of the derived hash against the stored one (empty salt/hash → false).
pub fn verify(password: &str, salt: &[u8], stored_hash: &[u8], iterations: u32) -> Option<SecretKey32> {
    if salt.is_empty() || stored_hash.is_empty() {
        return None;
    }
    let (mut hash, key) = derive(password, salt, iterations);
    let equal = hash.len() == stored_hash.len() && bool::from(hash.as_slice().ct_eq(stored_hash));
    hash.zeroize();
    equal.then_some(key)
}

/// `nonce(12) ‖ tag(16) ‖ ciphertext`, AES-256-GCM, no AAD, fresh random nonce.
pub fn seal(plaintext: &[u8], key: &SecretKey32) -> Vec<u8> {
    seal_raw(plaintext, key.expose(), &[])
}

/// Inverse of [`seal`]; `None` for `len < 28`, a bad key or failed authentication. A 28-byte blob opens to `[]`.
pub fn open(blob: &[u8], key: &SecretKey32) -> Option<Vec<u8>> {
    open_raw(blob, key.expose(), &[])
}

/// Same layout with AAD (secret-store sidecars use the entry key as AAD).
pub fn seal_with_aad(plaintext: &[u8], key: &SecretKey32, aad: &[u8]) -> Vec<u8> {
    seal_raw(plaintext, key.expose(), aad)
}

pub fn open_with_aad(blob: &[u8], key: &SecretKey32, aad: &[u8]) -> Option<Vec<u8>> {
    open_raw(blob, key.expose(), aad)
}

/// [`seal`] over a raw key slice (must be 32 bytes; anything else panics in debug and yields an empty blob).
pub fn seal_raw(plaintext: &[u8], key: &[u8], aad: &[u8]) -> Vec<u8> {
    let Ok(cipher) = Aes256Gcm::new_from_slice(key) else {
        debug_assert!(false, "AES-256-GCM key must be 32 bytes");
        return Vec::new();
    };
    let mut out = vec![0u8; BLOB_OVERHEAD + plaintext.len()];
    random_fill(&mut out[..GCM_NONCE]);
    out[BLOB_OVERHEAD..].copy_from_slice(plaintext);
    let nonce = Nonce::<aes_gcm::aead::consts::U12>::try_from(&out[..GCM_NONCE]).expect("12-byte nonce");
    let (_, body) = out.split_at_mut(BLOB_OVERHEAD);
    let tag = cipher.encrypt_inout_detached(&nonce, aad, body.into()).expect("AES-GCM encrypt");
    out[GCM_NONCE..BLOB_OVERHEAD].copy_from_slice(tag.as_slice());
    out
}

/// [`open`] over a raw key slice; `None` for a key that is not 32 bytes.
pub fn open_raw(blob: &[u8], key: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    if blob.len() < BLOB_OVERHEAD || key.len() != 32 {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let nonce = Nonce::<aes_gcm::aead::consts::U12>::try_from(&blob[..GCM_NONCE]).ok()?;
    let tag = Tag::<aes_gcm::aead::consts::U16>::try_from(&blob[GCM_NONCE..BLOB_OVERHEAD]).ok()?;
    let mut plain = blob[BLOB_OVERHEAD..].to_vec();
    match cipher.decrypt_inout_detached(&nonce, aad, plain.as_mut_slice().into(), &tag) {
        Ok(()) => Some(plain),
        Err(_) => {
            plain.zeroize();
            None
        }
    }
}

/// `HMAC-SHA256(key, data)`.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    let out = mac.finalize().into_bytes();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// `lowercase_hex(HMAC-SHA256(vault_key, input))[0..32]` (Windows custom-list item hash input is built by the
/// caller with `upper_invariant`).
pub fn hmac_hex32(key: &SecretKey32, input: &[u8]) -> String {
    let mac = hmac_sha256(key.expose(), input);
    lower_hex(&mac[..16])
}

/// Lowercase hex of `bytes` (C# `Convert.ToHexStringLower`).
pub fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xF) as usize] as char);
    }
    s
}

/// 16 random bytes from the OS CSPRNG.
pub fn random_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    random_fill(&mut salt);
    salt
}

/// Fills `buf` from the OS CSPRNG (panics only if the OS has no randomness source at all).
pub fn random_fill(buf: &mut [u8]) {
    getrandom::fill(buf).expect("OS random number generator unavailable");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_layout_is_nonce_tag_ciphertext_and_round_trips() {
        let key = SecretKey32::new([7u8; 32]);
        let blob = seal(b"hello", &key);
        assert_eq!(blob.len(), 28 + 5);
        assert_eq!(open(&blob, &key).as_deref(), Some(&b"hello"[..]));
        let empty = seal(b"", &key);
        assert_eq!(empty.len(), 28);
        assert_eq!(open(&empty, &key), Some(Vec::new()));
        let mut bad = blob.clone();
        bad[13] ^= 1; // tag byte
        assert_eq!(open(&bad, &key), None);
        assert_eq!(open(&blob[..27], &key), None);
        assert_eq!(open_raw(&blob, &[0u8; 16], &[]), None);
    }

    #[test]
    fn aad_must_match() {
        let key = SecretKey32::new([1u8; 32]);
        let blob = seal_with_aad(b"x", &key, b"a");
        assert!(open_with_aad(&blob, &key, b"a").is_some());
        assert!(open_with_aad(&blob, &key, b"b").is_none());
        assert!(open(&blob, &key).is_none());
    }

    #[test]
    fn verify_rejects_empty_inputs() {
        assert!(verify("pw", &[], &[1], 1).is_none());
        assert!(verify("pw", &[1], &[], 1).is_none());
        let salt = [3u8; 16];
        let (hash, key) = derive("pw", &salt, 10);
        assert_eq!(verify("pw", &salt, &hash, 10).map(|k| *k.expose()), Some(*key.expose()));
        assert!(verify("pw ", &salt, &hash, 10).is_none());
    }
}
