//! `ProfileCryptoTests` (9): PBKDF2 vectors and the `nonce | tag | ciphertext` AES-GCM layout shared with the Windows
//! app, through the LibraryKit rules of `derive_checked` (iterations ≤ 0 → 100 000, more than 10 000 000 refused).

use aes_gcm::aead::array::Array;
use aes_gcm::{AeadInOut, Aes256Gcm, KeyInit};
use sa_core::secrets::SecretKey32;

use super::file_format::hand_sealed;
use crate::ProfileError;
use crate::crypto;
use crate::mac::auth::{constant_time_eq, derive_checked};

fn hex(bytes: &[u8]) -> String {
    crypto::lower_hex(bytes)
}

fn random_key() -> SecretKey32 {
    let mut bytes = [0u8; 32];
    crypto::random_fill(&mut bytes);
    SecretKey32::new(bytes)
}

#[test]
fn pbkdf2_matches_published_hmac_sha256_vectors() {
    let (one, _) = derive_checked("password", b"salt", 1).unwrap();
    assert_eq!(hex(&one), "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b");
    let (many, key) = derive_checked("password", b"salt", 4096).unwrap();
    assert_eq!(hex(&many), "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a");
    assert_eq!(key.expose().len(), 32);
    assert_ne!(key.expose(), &many);
}

#[test]
fn non_positive_iterations_fall_back_to_the_default() {
    let salt = [7u8; 16];
    let (fallback_hash, fallback_key) = derive_checked("pw", &salt, 0).unwrap();
    let (explicit_hash, explicit_key) = derive_checked("pw", &salt, 100_000).unwrap();
    assert_eq!(fallback_hash, explicit_hash);
    assert_eq!(fallback_key.expose(), explicit_key.expose());
    let (negative_hash, _) = derive_checked("pw", &salt, -5).unwrap();
    assert_eq!(negative_hash, explicit_hash);
}

#[test]
fn password_is_used_as_typed_without_trimming() {
    let salt = [1u8; 16];
    let (plain, _) = derive_checked("pw", &salt, 10).unwrap();
    let (padded, _) = derive_checked(" pw ", &salt, 10).unwrap();
    assert_ne!(plain, padded);
}

#[test]
fn absurd_iteration_counts_are_refused() {
    assert_eq!(derive_checked("pw", &[1u8; 16], i64::MAX).unwrap_err(), ProfileError::CryptoFailure);
    assert_eq!(derive_checked("pw", &[1u8; 16], 10_000_001).unwrap_err(), ProfileError::CryptoFailure);
    // RUST: an empty salt is refused too (Swift `ProfileCrypto.derive` guard).
    assert_eq!(derive_checked("pw", &[], 10).unwrap_err(), ProfileError::CryptoFailure);
}

#[test]
fn sealed_vault_uses_the_windows_layout_nonce_tag_ciphertext() {
    let key = random_key();
    let plaintext = b"{\"videos\":[]}";
    let blob = crypto::seal(plaintext, &key);
    assert_eq!(blob.len(), 12 + 16 + plaintext.len());
    // Parse it by hand the way the Windows app does: nonce | tag | ciphertext.
    let cipher = Aes256Gcm::new_from_slice(key.expose()).unwrap();
    let nonce: [u8; 12] = blob[..12].try_into().unwrap();
    let tag: [u8; 16] = blob[12..28].try_into().unwrap();
    let mut body = blob[28..].to_vec();
    cipher.decrypt_inout_detached(&Array::from(nonce), &[], body.as_mut_slice().into(), &Array::from(tag)).unwrap();
    assert_eq!(body, plaintext);
    assert_eq!(crypto::open(&blob, &key).as_deref(), Some(&plaintext[..]));
}

#[test]
fn a_vault_built_by_hand_in_the_windows_layout_opens() {
    let key = random_key();
    let plaintext = b"{\"videos\":[],\"customLists\":[],\"thumbnails\":{}}";
    let hand_built = hand_sealed(plaintext, &key);
    assert_eq!(crypto::open(&hand_built, &key).as_deref(), Some(&plaintext[..]));
    // The library's "combined" order (nonce | ciphertext | tag) is not what the apps write.
    let mut combined = hand_built[..12].to_vec();
    combined.extend_from_slice(&hand_built[28..]);
    combined.extend_from_slice(&hand_built[12..28]);
    assert_eq!(crypto::open(&combined, &key), None);
}

#[test]
fn each_save_uses_a_fresh_nonce() {
    let key = random_key();
    let a = crypto::seal(b"x", &key);
    let b = crypto::seal(b"x", &key);
    assert_ne!(a[..12], b[..12]);
}

#[test]
fn wrong_key_tampering_and_short_blobs_are_rejected() {
    let key = random_key();
    let mut blob = crypto::seal(b"secret", &key);
    assert_eq!(crypto::open(&blob, &random_key()), None);
    let last = blob.len() - 1;
    blob[last] ^= 0x01;
    assert_eq!(crypto::open(&blob, &key), None);
    assert_eq!(crypto::open(&[0u8; 27], &key), None);
    assert_eq!(crypto::open(&[], &key), None);
}

#[test]
fn constant_time_equal_compares_content_and_length() {
    assert!(constant_time_eq(&[1, 2, 3], &[1, 2, 3]));
    assert!(!constant_time_eq(&[1, 2, 3], &[1, 2, 4]));
    assert!(!constant_time_eq(&[1, 2, 3], &[1, 2]));
    assert!(constant_time_eq(&[], &[]));
}
