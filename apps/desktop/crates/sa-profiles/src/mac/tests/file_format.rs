//! `ProfileFileFormatTests` (10): `profiles.json` and the vault stay readable by the other implementations, and the
//! remembered login never ends up in the file.

use aes_gcm::aead::array::Array;
use aes_gcm::{AeadInOut, Aes256Gcm, KeyInit};
use base64::Engine as _;
use sa_core::secrets::SecretKey32;
use serde_json::Value;

use super::*;
use crate::mac::auth::derive_checked;
use crate::{FingerprintMode, ProfileError, ProfileStore};

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(text).unwrap()
}

/// `nonce | tag | ciphertext` built with the library's detached API (the Windows layout), not `crypto::seal`.
pub fn hand_sealed(plaintext: &[u8], key: &SecretKey32) -> Vec<u8> {
    let cipher = Aes256Gcm::new_from_slice(key.expose()).unwrap();
    let mut nonce = [0u8; 12];
    crate::crypto::random_fill(&mut nonce);
    let mut body = plaintext.to_vec();
    let tag = cipher.encrypt_inout_detached(&Array::from(nonce), &[], body.as_mut_slice().into()).unwrap();
    let mut out = nonce.to_vec();
    out.extend_from_slice(tag.as_slice());
    out.extend_from_slice(&body);
    out
}

/// Opens `nonce | tag | ciphertext` by hand.
pub fn hand_open(blob: &[u8], key: &SecretKey32) -> Vec<u8> {
    let cipher = Aes256Gcm::new_from_slice(key.expose()).unwrap();
    let nonce: [u8; 12] = blob[..12].try_into().unwrap();
    let tag: [u8; 16] = blob[12..28].try_into().unwrap();
    let mut body = blob[28..].to_vec();
    cipher.decrypt_inout_detached(&Array::from(nonce), &[], body.as_mut_slice().into(), &Array::from(tag)).unwrap();
    body
}

fn first_profile(folder: &TestFolder) -> serde_json::Map<String, Value> {
    folder.profiles_object()["profiles"].as_array().unwrap()[0].as_object().unwrap().clone()
}

#[test]
fn profiles_json_uses_the_shared_shape() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("Zed", "pw-zed", false).unwrap();
    store.logout_user().unwrap();
    store.register_user("alice", "pw-alice", false).unwrap();

    let root = folder.profiles_object();
    assert_eq!(root["version"], 2);
    assert_eq!(root["rememberMe"], false);
    assert_eq!(root["lastUsername"], "alice");
    assert!(!root.contains_key("rememberedUsername"));
    let raw = folder.raw_profiles();
    assert!(!raw.contains("null"));
    let profiles = root["profiles"].as_array().unwrap();
    let names: Vec<&str> = profiles.iter().map(|p| p["username"].as_str().unwrap()).collect();
    assert_eq!(names, ["alice", "Zed"]);
    let alice = profiles[0].as_object().unwrap();
    assert_eq!(unb64(alice["saltBase64"].as_str().unwrap()).len(), 16);
    assert_eq!(unb64(alice["passwordHashBase64"].as_str().unwrap()).len(), 32);
    assert_eq!(alice["iterations"], 100_000);
    assert!(dates::parse(alice["createdAtUtc"].as_str().unwrap()).is_some());
    assert!(alice["encryptedVaultBase64"].is_string());
    assert!(!raw.contains("\\/"), "slashes must not be escaped");
    // Apple pretty layout.
    assert!(raw.starts_with("{\n  \"lastUsername\" : \"alice\",\n  \"profiles\" : [\n    {\n      \"createdAtUtc\" : "));
    assert!(raw.ends_with("\n  \"rememberMe\" : false,\n  \"version\" : 2\n}"));
}

#[test]
fn vault_is_aes_gcm_with_nonce_tag_ciphertext_and_the_documented_plaintext() {
    let folder = TestFolder::new();
    let video = folder.video("a.mp4");
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    store.try_record_watch(&video, 10.0, 100.0).unwrap();
    let list = store.try_create_list("Liste").unwrap().unwrap();
    store.try_add_video(&video, &list.id).unwrap();
    store.try_set_thumbnail(&video, b"IMG").unwrap();

    let profile = first_profile(&folder);
    let salt = unb64(profile["saltBase64"].as_str().unwrap());
    let (hash, key) = derive_checked("pw", &salt, 100_000).unwrap();
    assert_eq!(hash.to_vec(), unb64(profile["passwordHashBase64"].as_str().unwrap()));
    let blob = unb64(profile["encryptedVaultBase64"].as_str().unwrap());
    let plaintext = hand_open(&blob, &key);

    let vault: Value = serde_json::from_slice(&plaintext).unwrap();
    let record = vault["videos"][0].as_object().unwrap();
    for k in [
        "videoPath",
        "fileName",
        "directoryPath",
        "viewCount",
        "isLiked",
        "rating",
        "lastPositionSeconds",
        "durationSeconds",
        "lastWatchedAtUtc",
        "updatedAtUtc",
    ] {
        assert!(record.contains_key(k), "missing {k}");
    }
    assert!(!record.contains_key("contentFingerprint"), "no fingerprint for a file that does not exist");
    assert!(!record.contains_key("thumbnail"));
    assert_eq!(record["viewCount"], 1);
    let lists = vault["customLists"].as_array().unwrap();
    let id = lists[0]["id"].as_str().unwrap();
    assert_eq!(id, list.id);
    assert!(id.len() == 32 && id.chars().all(|c| "0123456789abcdef".contains(c)));
    assert_eq!(lists[0]["videoPaths"], serde_json::json!([normalized(&video)]));
    assert!(vault["thumbnails"].as_object().unwrap().is_empty());
    // Compact, sorted keys.
    let text = String::from_utf8(plaintext).unwrap();
    assert!(text.starts_with("{\"customLists\":[{\"createdAtUtc\":"));
}

#[test]
fn a_profiles_file_written_by_the_windows_app_opens() {
    let folder = TestFolder::new();
    let salt: Vec<u8> = (0..16).collect();
    let (hash, key) = derive_checked("geçerli şifre", &salt, 100_000).unwrap();
    let vault_json = concat!(
        r#"{"videos":[{"contentFingerprint":"fp:5:abcdef","videoPath":"/Movies/film.mkv","fileName":"film.mkv","directoryPath":"/Movies","#,
        r#""viewCount":3,"isLiked":true,"rating":4,"lastPositionSeconds":12.5,"durationSeconds":99,"#,
        r#""lastWatchedAtUtc":"2026-05-01T10:20:30.1234567+00:00","updatedAtUtc":"2026-05-01T10:20:30.1234567+00:00"}],"#,
        r#""customLists":[{"id":"0123456789abcdef0123456789abcdef","name":"Liste","videoPaths":["/Movies/film.mkv"],"#,
        r#""createdAtUtc":"2026-05-01T10:00:00+03:00","updatedAtUtc":"2026-05-01T10:00:00Z"}],"thumbnails":{"fp:5:abcdef":"SU1H"}}"#
    );
    let vault = b64(&hand_sealed(vault_json.as_bytes(), &key));
    let file = format!(
        "{{\n  \"version\": 2,\n  \"rememberMe\": false,\n  \"lastUsername\": \"Ayşe\",\n  \"rememberedProtectedKeyBase64\": \"AAAA\",\n  \"profiles\": [\n    {{\n      \"username\": \"Ayşe\",\n      \"saltBase64\": \"{}\",\n      \"passwordHashBase64\": \"{}\",\n      \"iterations\": 100000,\n      \"createdAtUtc\": \"2026-04-30T08:00:00.0000000+00:00\",\n      \"encryptedVaultBase64\": \"{vault}\"\n    }}\n  ]\n}}",
        b64(&salt),
        b64(&hash)
    );
    std::fs::write(folder.profiles_file(), file).unwrap();

    let store = folder.make_store();
    assert_eq!(store.registered_usernames(), ["Ayşe"]);
    assert_eq!(store.last_username().as_deref(), Some("Ayşe"));
    assert!(!store.is_logged_in());
    store.login_user("ayşe", "geçerli şifre", false).unwrap();

    let film = Path::new("/Movies/film.mkv");
    let stats = store.stats_mode(film, FingerprintMode::Compute).unwrap();
    assert_eq!((stats.view_count, stats.is_liked, stats.rating), (3, true, 4));
    assert_eq!(stats.content_fingerprint.as_deref(), Some("fp:5:abcdef"));
    assert_eq!(stats.thumbnail.as_deref(), Some(&b"IMG"[..]), "a picture an older vault carries is still found");
    assert_eq!(store.thumbnail(film).as_deref(), Some(&b"IMG"[..]));
    assert_eq!(stats.last_watched_at, dates::parse("2026-05-01T10:20:30.1234567+00:00"));
    let list = &store.lists()[0];
    assert_eq!(list.id, "0123456789abcdef0123456789abcdef");
    assert_eq!(Some(list.created_at), dates::parse("2026-05-01T07:00:00Z"));
    assert_eq!(Some(list.updated_at), dates::parse("2026-05-01T10:00:00Z"));
    // The Windows-only field is never written back.
    store.try_set_rating(Path::new("/Movies/other.mkv"), 1).unwrap();
    assert!(!folder.raw_profiles().to_lowercase().contains("protectedkey"));
}

// MARK: - Remember me

#[test]
fn the_remembered_key_lives_in_the_secret_store_and_never_in_the_file() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let store = folder.store_with(&secrets);
    store.register_user("Emre", "pw", true).unwrap();
    assert_eq!(secrets.accounts(), ["profile-remember.emre"]);
    let key = secrets.data("profile-remember.emre").unwrap();
    assert_eq!(key.len(), 32);
    let raw = folder.raw_profiles();
    assert!(!raw.contains(&b64(&key)));
    assert!(!raw.to_lowercase().contains("protectedkey"));
    let root = folder.profiles_object();
    assert_eq!(root["rememberMe"], true);
    assert_eq!(root["rememberedUsername"], "Emre");
    assert_eq!(store.remembered_username().as_deref(), Some("Emre"));
    let blob = unb64(first_profile(&folder)["encryptedVaultBase64"].as_str().unwrap());
    assert!(crate::crypto::open(&blob, &SecretKey32::from_slice(&key).unwrap()).is_some());
}

#[test]
fn logout_clears_the_stored_key_and_remembered_username_but_keeps_remember_me_and_last_username() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let store = folder.store_with(&secrets);
    store.register_user("emre", "pw", true).unwrap();
    store.logout_user().unwrap();
    assert!(secrets.accounts().is_empty());
    let root = folder.profiles_object();
    assert!(!root.contains_key("rememberedUsername"));
    assert_eq!(root["rememberMe"], true);
    assert_eq!(root["lastUsername"], "emre");
    assert_eq!(store.remembered_username(), None);
    assert!(store.remember_me_flag());
    assert!(!store.remember_me(), "no remembered session any more");
    assert_eq!(store.last_username().as_deref(), Some("emre"));
}

#[test]
fn logging_in_with_remember_me_off_clears_the_remembered_login() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let store = folder.store_with(&secrets);
    store.register_user("emre", "pw", true).unwrap();
    assert!(!secrets.accounts().is_empty());
    store.login_user("emre", "pw", false).unwrap();
    assert!(secrets.accounts().is_empty());
    assert!(!store.remember_me_flag());
    assert!(!folder.profiles_object().contains_key("rememberedUsername"));
    assert!(!folder.store_with(&secrets).is_logged_in());
    store.logout_user().unwrap();
    store.register_user("second", "pw", false).unwrap();
    assert!(secrets.accounts().is_empty());
}

#[test]
fn switching_the_remembered_user_forgets_the_previous_key() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let store = folder.store_with(&secrets);
    store.register_user("one", "pw1", true).unwrap();
    store.register_user("two", "pw2", true).unwrap();
    assert_eq!(secrets.accounts(), ["profile-remember.two"]);
    assert_eq!(store.remembered_username().as_deref(), Some("two"));
}

#[test]
fn deleting_a_profile_removes_its_key_vault_and_session() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let store = folder.store_with(&secrets);
    store.register_user("emre", "pw", true).unwrap();
    store.register_user("other", "pw2", false).unwrap();
    store.login_user("emre", "pw", true).unwrap();
    assert_eq!(secrets.accounts(), ["profile-remember.emre"]);
    assert_eq!(store.delete_user("emre", "nope"), Err(ProfileError::InvalidCredentials));
    assert_eq!(store.registered_usernames(), ["emre", "other"]);
    store.delete_user("EMRE", "pw").unwrap();
    assert!(secrets.accounts().is_empty());
    assert_eq!(store.registered_usernames(), ["other"]);
    assert!(!store.is_logged_in());
    assert!(!folder.store_with(&secrets).is_logged_in());
    assert_eq!(store.login_user("emre", "pw", true), Err(ProfileError::InvalidCredentials));
}

#[test]
fn register_rules_are_enforced() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    assert_eq!(store.register_user("   ", "pw", true), Err(ProfileError::EmptyUsername));
    assert_eq!(store.register_user("emre", "", true), Err(ProfileError::EmptyPassword));
    store.register_user("  emre  ", " pw ", true).unwrap();
    assert_eq!(store.current_username().as_deref(), Some("emre"));
    assert_eq!(store.register_user("EMRE", "x", true), Err(ProfileError::UsernameTaken));
    assert_eq!(store.login_user("", "x", true), Err(ProfileError::MissingCredentials));
    assert_eq!(store.login_user("emre", "", true), Err(ProfileError::MissingCredentials));
    store.logout_user().unwrap();
    assert_eq!(store.login_user("emre", "pw", true), Err(ProfileError::InvalidCredentials));
    store.login_user("emre", " pw ", true).unwrap();
}

#[test]
fn the_trait_takes_secret_strings() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    let pw = sa_core::secrets::SecretString::new("pw");
    ProfileStore::register(&*store, "emre", &pw, false).unwrap();
    ProfileStore::logout(&*store).unwrap();
    ProfileStore::login(&*store, "Emre", &pw, false).unwrap();
    assert_eq!(store.current_username().as_deref(), Some("emre"));
    ProfileStore::delete_profile(&*store, "emre", &pw).unwrap();
    assert!(store.registered_usernames().is_empty());
}
