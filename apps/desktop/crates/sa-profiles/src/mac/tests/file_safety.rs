//! `ProfileFileSafetyTests` (10): newer formats, unreadable files and remembered logins nothing can vouch for.

use serde_json::Value;

use super::*;
use crate::{ProfileError, ProfileStore};

#[test]
fn a_newer_file_whose_shape_differs_is_left_alone_not_treated_as_corrupt() {
    for json in [
        r#"{"version": 3, "accounts": {"alice": {"salt": 5}}, "profiles": "a different shape"}"#,
        r#"{"version": 99, "profiles": {"alice": {"username": 5}}}"#,
        r#"{"version": 4, "profiles": [{"username": ["not", "a", "string"]}], "rememberMe": "yes"}"#,
        r#"{"version": 3}"#,
    ] {
        let folder = TestFolder::new();
        std::fs::write(folder.profiles_file(), json).unwrap();
        let store = folder.make_store();
        assert!(store.registered_usernames().is_empty());
        assert!(store.register_user("bob", "pw", true).is_err(), "{json}");
        assert_eq!(folder.file_data(), json.as_bytes(), "never replaced by a version 2 file");
        assert!(folder.aside_files().is_empty(), "never set aside either");
        store.refresh();
        assert!(store.register_user("bob", "pw", true).is_err());
        assert_eq!(folder.file_data(), json.as_bytes());
        assert!(!store.is_logged_in());
    }
}

#[test]
fn a_windows_version3_file_is_refused_and_left_byte_identical_next_to_its_database() {
    let folder = TestFolder::new();
    let stub = br#"{"version": 3, "database": "profiles.db"}"#;
    let database = b"SQLite format 3\0";
    let database_path = folder.url.join("profiles.db");
    std::fs::write(folder.profiles_file(), stub).unwrap();
    std::fs::write(&database_path, database).unwrap();
    let store = folder.make_store();
    assert!(store.registered_usernames().is_empty());
    assert_eq!(store.register_user("bob", "pw", true), Err(ProfileError::UnsupportedFileVersion(3)));
    store.refresh();
    assert_eq!(store.register_user("bob", "pw", true), Err(ProfileError::UnsupportedFileVersion(3)));
    assert_eq!(folder.file_data(), stub);
    assert_eq!(std::fs::read(&database_path).unwrap(), database);
    assert!(folder.aside_files().is_empty());
}

#[test]
fn the_error_names_the_newer_version() {
    let folder = TestFolder::new();
    std::fs::write(folder.profiles_file(), r#"{"version": 7, "profiles": 12}"#).unwrap();
    let store = folder.make_store();
    assert_eq!(store.register_user("bob", "pw", true), Err(ProfileError::UnsupportedFileVersion(7)));
}

#[test]
fn a_newer_file_left_alone_by_a_running_store_stays_untouched_by_its_writes() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("emre", "pw", false).unwrap();
    let future = br#"{"version": 5, "vaults": []}"#;
    std::fs::write(folder.profiles_file(), future).unwrap();
    assert_eq!(store.try_create_list("x"), Err(ProfileError::UnsupportedFileVersion(5)));
    assert_eq!(folder.file_data(), future);
    assert!(folder.aside_files().is_empty());
}

#[test]
fn a_file_of_the_current_format_that_cannot_be_parsed_is_still_damage_and_set_aside() {
    for json in [r#"{"version": "two"}"#, r#"{"version": 2, "profiles": "x"}"#, "[1, 2, 3]", ""] {
        let folder = TestFolder::new();
        std::fs::write(folder.profiles_file(), json).unwrap();
        let store = folder.make_store();
        store.register_user("emre", "pw", false).unwrap();
        assert_eq!(folder.aside_files().len(), 1, "{json}");
        assert_eq!(folder.profiles_object()["version"], 2);
    }
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_is_never_replaced_by_the_empty_view_of_a_store_that_could_not_read_it() {
    if !permissions_apply() {
        return;
    }
    let folder = TestFolder::new();
    let first = folder.make_store();
    first.register_user("alice", "pw-alice", false).unwrap();
    first.logout_user().unwrap();
    let original = folder.file_data();
    chmod(&folder.profiles_file(), 0o000);
    let blind = folder.make_store();
    assert!(blind.registered_usernames().is_empty(), "it cannot see the profiles");
    assert_eq!(blind.register_user("bob", "pw-bob", true), Err(ProfileError::FileUnreadable));
    assert_eq!(blind.login_user("alice", "pw-alice", true), Err(ProfileError::FileUnreadable));
    assert_eq!(blind.delete_user("alice", "pw-alice"), Err(ProfileError::FileUnreadable));
    assert!(!blind.is_logged_in());
    chmod(&folder.profiles_file(), 0o644);
    assert_eq!(folder.file_data(), original, "nothing was written");
    assert!(folder.aside_files().is_empty());
    assert_eq!(blind.registered_usernames(), ["alice"]);
    blind.register_user("bob", "pw-bob", false).unwrap();
    assert_eq!(blind.registered_usernames(), ["alice", "bob"]);
    assert_eq!(folder.make_store().registered_usernames(), ["alice", "bob"]);
    blind.login_user("alice", "pw-alice", false).unwrap();
}

#[cfg(unix)]
#[test]
fn a_file_that_becomes_readable_is_read_again_even_though_its_signature_never_changed() {
    if !permissions_apply() {
        return;
    }
    let folder = TestFolder::new();
    let first = folder.make_store();
    first.register_user("alice", "pw", false).unwrap();
    chmod(&folder.profiles_file(), 0o000);
    let blind = folder.make_store();
    assert!(blind.registered_usernames().is_empty());
    assert!(blind.registered_usernames().is_empty(), "still unreadable, still empty");
    chmod(&folder.profiles_file(), 0o644);
    assert_eq!(blind.registered_usernames(), ["alice"]);
}

#[test]
fn a_remembered_login_never_opens_a_profile_that_has_no_vault() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let setup = folder.store_with(&secrets);
    setup.register_user("ghost", "pw", true).unwrap();
    setup.logout_user().unwrap();

    let mut root = folder.profiles_object();
    let profiles = root.get_mut("profiles").unwrap().as_array_mut().unwrap();
    profiles[0].as_object_mut().unwrap().remove("encryptedVaultBase64");
    root.insert("rememberMe".into(), Value::Bool(true));
    root.insert("rememberedUsername".into(), Value::String("ghost".into()));
    folder.write_profiles_object(&root);
    secrets.put("profile-remember.ghost", &crate::crypto::random_salt().repeat(2));

    let planted = folder.store_with(&secrets);
    assert!(!planted.is_logged_in(), "nothing can confirm the key, so it must not unlock anything");
    assert_eq!(planted.current_username(), None);
    assert_eq!(planted.remembered_username().as_deref(), Some("ghost"));
    planted.refresh();
    assert!(!planted.is_logged_in());

    planted.login_user("ghost", "pw", true).unwrap();
    assert_eq!(planted.current_username().as_deref(), Some("ghost"));
    planted.try_set_liked(&folder.video("a.mp4"), true).unwrap();
    assert!(folder.store_with(&secrets).is_logged_in(), "with a vault written, remember-me works again");
}

#[test]
fn a_wrong_key_for_a_profile_with_a_vault_still_does_not_log_in() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let setup = folder.store_with(&secrets);
    setup.register_user("alice", "pw", true).unwrap();
    secrets.put("profile-remember.alice", &crate::crypto::random_salt().repeat(2));
    assert!(!folder.store_with(&secrets).is_logged_in());
}

#[test]
fn a_missing_or_short_key_does_not_log_in() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let setup = folder.store_with(&secrets);
    setup.register_user("alice", "pw", true).unwrap();
    secrets.put("profile-remember.alice", &[1u8; 16]);
    assert!(!folder.store_with(&secrets).is_logged_in());
    secrets.remove("profile-remember.alice");
    assert!(!folder.store_with(&secrets).is_logged_in());
}
