//! Golden fixtures (no Swift toolchain here): exact bytes the Swift LibraryKit writes, rebuilt by hand from its
//! encoder settings (`JSONEncoder` `.prettyPrinted`/`.sortedKeys`/`.withoutEscapingSlashes`, `ProfileDateCoding`),
//! and hash vectors computed independently (Python `hashlib`/`hmac`) for the picture-cache names and the content
//! fingerprint. They pin the formats so a change that would make the Swift app's files unreadable shows up here.
//!
//! Key order follows Foundation's `JSONWriter` `.sortedKeys` (`.numeric, .caseInsensitive, .forcedOrdering`), so
//! `rememberedUsername` precedes `rememberMe`. Unverifiable without a Mac: Apple's number formatting for very small
//! or very large doubles. Both readers ignore key order and accept either number form.

use sa_core::secrets::SecretKey32;

use super::*;
use crate::mac::document::{self, ProfileDto, ProfilesDocument, VaultPayload};
use crate::mac::thumbnails::ThumbnailCache;
use crate::{UserVideoList, VideoUserStats};

fn date(text: &str) -> Ticks {
    dates::parse(text).unwrap()
}

#[test]
fn profiles_json_is_written_in_the_apple_pretty_layout() {
    let doc = ProfilesDocument {
        remember_me: true,
        last_username: Some("Ayşe".into()),
        remembered_username: Some("Ayşe".into()),
        profiles: vec![
            ProfileDto {
                username: "Zed".into(),
                salt_base64: "c2FsdHNhbHRzYWx0c2FsdA==".into(),
                password_hash_base64: "aGFzaA==".into(),
                iterations: 100_000,
                created_at: date("2026-04-30T08:00:00Z"),
                encrypted_vault_base64: None,
            },
            ProfileDto {
                username: "Ayşe".into(),
                salt_base64: "AAECAwQFBgcICQoLDA0ODw==".into(),
                password_hash_base64: "aGFzaA==".into(),
                iterations: 100_000,
                created_at: date("2026-04-30T08:00:00.1234567+03:00"),
                encrypted_vault_base64: Some("ab/cd+ef==".into()),
            },
        ],
    };
    let expected = "{\n  \"lastUsername\" : \"Ayşe\",\n  \"profiles\" : [\n    {\n      \"createdAtUtc\" : \"2026-04-30T05:00:00.1234567+00:00\",\n      \"encryptedVaultBase64\" : \"ab/cd+ef==\",\n      \"iterations\" : 100000,\n      \"passwordHashBase64\" : \"aGFzaA==\",\n      \"saltBase64\" : \"AAECAwQFBgcICQoLDA0ODw==\",\n      \"username\" : \"Ayşe\"\n    },\n    {\n      \"createdAtUtc\" : \"2026-04-30T08:00:00.0000000+00:00\",\n      \"iterations\" : 100000,\n      \"passwordHashBase64\" : \"aGFzaA==\",\n      \"saltBase64\" : \"c2FsdHNhbHRzYWx0c2FsdA==\",\n      \"username\" : \"Zed\"\n    }\n  ],\n  \"rememberedUsername\" : \"Ayşe\",\n  \"rememberMe\" : true,\n  \"version\" : 2\n}";
    assert_eq!(document::encode_profiles(&doc), expected);
    // An empty table is Apple's empty-array form.
    let empty = ProfilesDocument { remember_me: false, last_username: None, remembered_username: None, profiles: vec![] };
    assert_eq!(
        document::encode_profiles(&empty),
        "{\n  \"profiles\" : [\n\n  ],\n  \"rememberMe\" : false,\n  \"version\" : 2\n}"
    );
}

#[test]
fn the_vault_plaintext_is_compact_with_sorted_keys() {
    let at = date("2026-05-01T10:20:30.1234567+00:00");
    let payload = VaultPayload {
        videos: vec![
            VideoUserStats {
                content_fingerprint: None,
                video_path: "/Movies/b.mp4".into(),
                file_name: "b.mp4".into(),
                directory_path: "/Movies".into(),
                view_count: 0,
                is_liked: false,
                rating: 0,
                last_position_seconds: 0.0,
                duration_seconds: 0.0,
                last_watched_at: None,
                updated_at: at,
                thumbnail: Some(b"never written".to_vec()),
            },
            VideoUserStats {
                content_fingerprint: Some("fp:5:abcdef".into()),
                video_path: "/Movies/film.mkv".into(),
                file_name: "film.mkv".into(),
                directory_path: "/Movies".into(),
                view_count: 3,
                is_liked: true,
                rating: 4,
                last_position_seconds: 12.5,
                duration_seconds: 99.0,
                last_watched_at: Some(at),
                updated_at: at,
                thumbnail: None,
            },
            VideoUserStats {
                video_path: "/Movies/A.mp4".into(),
                file_name: "A.mp4".into(),
                directory_path: "/Movies".into(),
                ..crate::mac::vault::placeholder("/Movies/A.mp4", at, None)
            },
        ],
        custom_lists: vec![UserVideoList {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "Daha Sonra İzle".into(),
            video_paths: vec!["/Movies/film.mkv".into()],
            created_at: date("2026-05-01T07:00:00Z"),
            updated_at: at,
        }],
        thumbnails: vec![],
    };
    let expected = concat!(
        r#"{"customLists":[{"createdAtUtc":"2026-05-01T07:00:00.0000000+00:00","id":"0123456789abcdef0123456789abcdef","#,
        r#""name":"Daha Sonra İzle","updatedAtUtc":"2026-05-01T10:20:30.1234567+00:00","videoPaths":["/Movies/film.mkv"]}],"#,
        r#""thumbnails":{},"videos":["#,
        r#"{"directoryPath":"/Movies","durationSeconds":0,"fileName":"A.mp4","isLiked":false,"lastPositionSeconds":0,"#,
        r#""rating":0,"updatedAtUtc":"2026-05-01T10:20:30.1234567+00:00","videoPath":"/Movies/A.mp4","viewCount":0},"#,
        r#"{"directoryPath":"/Movies","durationSeconds":0,"fileName":"b.mp4","isLiked":false,"lastPositionSeconds":0,"#,
        r#""rating":0,"updatedAtUtc":"2026-05-01T10:20:30.1234567+00:00","videoPath":"/Movies/b.mp4","viewCount":0},"#,
        r#"{"contentFingerprint":"fp:5:abcdef","directoryPath":"/Movies","durationSeconds":99,"fileName":"film.mkv","#,
        r#""isLiked":true,"lastPositionSeconds":12.5,"lastWatchedAtUtc":"2026-05-01T10:20:30.1234567+00:00","rating":4,"#,
        r#""updatedAtUtc":"2026-05-01T10:20:30.1234567+00:00","videoPath":"/Movies/film.mkv","viewCount":3}]}"#
    );
    assert_eq!(document::encode_vault(&payload), expected);
}

#[test]
fn dates_have_the_dot_net_shape() {
    assert_eq!(dates::to_string(dates::from_unix_seconds(1_800_000_000.0)), "2027-01-15T08:00:00.0000000+00:00");
    assert_eq!(dates::to_string(date("2000-02-29T23:59:59.9999999-01:30")), "2000-03-01T01:29:59.9999999+00:00");
    assert_eq!(dates::compact_timestamp(dates::from_unix_seconds(1_800_000_000.9)), "20270115-080000");
}

#[test]
fn picture_cache_names_match_independent_vectors() {
    // SHA-256 of the lowercased username, first 16 bytes.
    assert_eq!(ThumbnailCache::folder_name("Emre"), "c4265f1f1053ca3588f9934f37deb1fd");
    assert_eq!(ThumbnailCache::folder_name("AYŞE"), "370749e90bceb4b51200f92cb75a9d3d");
    // HMAC-SHA256 keyed with the vault key over the lowercased picture key, first 16 bytes (Swift `2f4bc96`).
    let key = SecretKey32::new(std::array::from_fn(|i| i as u8));
    assert_eq!(
        ThumbnailCache::file_name("fp:150000:0123456789ABCDEF01234567", &key),
        "6df6b81eac954dd3587132f7f788e35d.bin"
    );
    assert_eq!(ThumbnailCache::file_name("/Users/Emre/Movies/Film.mp4", &key), "9ca2d6cbd8224adea6429785c8bb555a.bin");
    assert_eq!(
        ThumbnailCache::file_name("/Users/Ayşe/Filmler/Çocuk.mp4", &key),
        "02814eeb0aa3b8188a33cc4d22ad4067.bin"
    );
}

#[test]
fn fingerprints_match_independent_vectors() {
    let folder = TestFolder::new();
    for (count, expected) in [
        (1usize, "fp:1:695fa0f87d8b24e168ddbe03"),
        (100, "fp:100:f04c07a16b1eced4c29e1800"),
        (65_535, "fp:65535:54055082bd303ac6d5a75025"),
        (65_536, "fp:65536:6fac33ab582ae33c1ab28721"),
        (65_537, "fp:65537:12714dfcfc81026c1fb3bfdd"),
        (150_000, "fp:150000:624de026b7bce7a89729c84a"),
        (300_001, "fp:300001:bfc88528c2e9e2db0fdafe3b"),
    ] {
        // LCG `v = v * 1664525 + 1013904223` from 0, byte = `v >> 16`.
        let mut value = 0u32;
        let bytes: Vec<u8> = (0..count)
            .map(|_| {
                value = value.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (value >> 16) as u8
            })
            .collect();
        let url = folder.video(&format!("g{count}.bin"));
        std::fs::write(&url, bytes).unwrap();
        assert_eq!(folder.cache.video_id(&url), expected, "length {count}");
    }
}
