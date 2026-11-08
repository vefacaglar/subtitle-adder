//! Per-platform parity details found by re-reading the C# and Swift sources: Swift `HexColor.parse` trimming and sign
//! rules, the macOS overlay reset writing defaults, Swift's context-free `lowercased()` in the folder fold, Swift's
//! path-extension rules for a trailing dot, 64-bit/whitespace rules of the macOS timestamp parser, the C# 32-bit wrap,
//! `null` kind/backend in a catalog row, and the macOS diagnostic queue (unbounded) and rotation.

use std::sync::Arc;

use sa_core::appearance::{AppearanceSettings, HexColor, OverlayStyle};
use sa_core::diagnostics::{DiagnosticFormat, DiagnosticLog};
use sa_core::engine_types::EngineModel;
use sa_core::settings::{InMemorySettingsStore, SettingsStore};
use sa_core::subtitle_import::{
    ImportFlavor, SubtitleBadgeLookup, find_companion_video_listing, find_sidecar_subtitle_with, fold, is_subtitle_file_with, parse_with,
};

#[test]
fn hex_color_windows_rules() {
    assert_eq!(HexColor::parse_u32_windows(" #FF8000\n"), Some(0xFF8000));
    assert_eq!(HexColor::parse_u32_windows("\u{2028}00ff00\u{3000}"), Some(0x00FF00));
    assert_eq!(HexColor::parse_u32_windows("+FFFFF"), None);
    assert_eq!(HexColor::parse_u32_windows("-00000"), None);
    assert_eq!(HexColor::parse_u32_windows("##00ff00"), None);
    assert_eq!(HexColor::parse_u32_windows("0x0FFF"), None);
}

#[test]
fn hex_color_mac_rules() {
    // `.whitespaces` = space separators and tab only.
    assert_eq!(HexColor::parse_u32_mac("\t#FF8000\u{00A0}"), Some(0xFF8000));
    assert_eq!(HexColor::parse_u32_mac("#FF8000\n"), None);
    assert_eq!(HexColor::parse_u32_mac("\r00ff00"), None);
    // `UInt32(_:radix:)` takes a sign.
    assert_eq!(HexColor::parse_u32_mac("+FFFFF"), Some(0x0F_FFFF));
    assert_eq!(HexColor::parse_u32_mac("#+00001"), Some(1));
    assert_eq!(HexColor::parse_u32_mac("-00000"), Some(0));
    assert_eq!(HexColor::parse_u32_mac("-00001"), None);
    assert_eq!(HexColor::parse_u32_mac("++FFFF"), None);
    assert_eq!(HexColor::parse_u32_mac("+"), None);
    assert_eq!(HexColor::parse_u32_mac("abcdeF"), Some(0xABCDEF));
    assert_eq!(HexColor::parse_u32_mac("ab cde"), None);
    assert_eq!(HexColor::parse_u32_mac("ğabcde"), None);
    assert_eq!(HexColor::parse_mac("+0000F").map(|c| c.2), Some(15.0 / 255.0));
    assert_eq!(HexColor::parse_windows("+0000F"), None);
}

#[test]
fn native_hex_parse_follows_the_platform() {
    let expected = if cfg!(target_os = "macos") { Some(0x0F_FFFF) } else { None };
    assert_eq!(HexColor::parse("+FFFFF").map(|(r, g, b)| ((r * 255.0) as u32) << 16 | ((g * 255.0) as u32) << 8 | (b * 255.0) as u32), expected);
    let style = OverlayStyle::new(String::new(), 0.05, "+FFFFF".into(), Default::default(), 0.6);
    assert_eq!(style.text_hex, if cfg!(target_os = "macos") { "+FFFFF" } else { "#FFFFFF" });
}

#[test]
fn mac_overlay_reset_writes_the_defaults_and_keeps_theme_and_ui_font() {
    let store = InMemorySettingsStore::new();
    let appearance = AppearanceSettings::new(store.clone());
    appearance.set_theme(sa_core::appearance::ThemeChoice::Dark);
    appearance.set_ui_font_family("Menlo");
    appearance.set_font_family("Avenir");
    appearance.set_size(0.08);
    appearance.set_text_hex("#FF0000");
    appearance.set_background(sa_core::appearance::OverlayBackground::Outline);
    appearance.set_opacity(0.2);

    appearance.reset_overlay_writing_defaults();
    assert_eq!(store.raw("overlayFontFamily"), Some(serde_json::json!("")));
    assert_eq!(store.get_double("overlayFontSize"), Some(0.05));
    assert_eq!(store.get_string("overlayTextColor").as_deref(), Some("#FFFFFF"));
    assert_eq!(store.get_string("overlayBackgroundStyle").as_deref(), Some("box"));
    assert_eq!(store.get_double("overlayBackgroundOpacity"), Some(0.6));
    assert_eq!(appearance.style(), OverlayStyle::standard());
    assert_eq!(store.get_string("appTheme").as_deref(), Some("dark"));
    assert_eq!(store.get_string("appFontFamily").as_deref(), Some("Menlo"));

    appearance.reset_overlay_removing();
    for key in sa_core::appearance::keys::OVERLAY_KEYS {
        assert_eq!(store.raw(key), None, "{key}");
    }
    assert_eq!(appearance.style(), OverlayStyle::standard());
    assert_eq!(store.get_string("appTheme").as_deref(), Some("dark"));
}

#[test]
fn native_overlay_reset_follows_the_platform() {
    let store = InMemorySettingsStore::new();
    let appearance = AppearanceSettings::new(store.clone());
    appearance.set_size(0.08);
    appearance.reset_overlay();
    let expected = if cfg!(target_os = "macos") { Some(serde_json::json!(0.05)) } else { None };
    assert_eq!(store.raw("overlayFontSize"), expected);
}

#[test]
fn fold_lowercases_each_scalar_without_context_like_swift() {
    // Swift `lowercased()` has no final-sigma rule: "ΟΔΟΣ" → "οδοσ".
    assert_eq!(fold("ΟΔΟΣ.srt"), "οδοσ.srt");
    assert_eq!(fold("İstanbul.SRT"), "i\u{307}stanbul.srt");
    assert_eq!(fold("e\u{301}"), "é");
    let lookup = SubtitleBadgeLookup::new(&["ΦΙΛΜΣ.srt"]);
    assert_eq!(lookup.badge_extension("φιλμσ").as_deref(), Some("srt"));
}

#[test]
fn mac_names_ending_in_a_dot_have_no_extension() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    std::fs::write(dir.join("Film.srt"), b"").unwrap();
    std::fs::write(dir.join("Film..srt"), b"").unwrap();
    // Swift: `URL("Film.").deletingPathExtension()` keeps "Film.", so the sidecar is "Film..srt".
    let found = find_sidecar_subtitle_with(&dir.join("Film."), ImportFlavor::Mac).unwrap();
    assert_eq!(found.file_name().unwrap(), "Film..srt");
    // .NET strips the trailing dot: "Film" + ".srt".
    let found = find_sidecar_subtitle_with(&dir.join("Film."), ImportFlavor::Windows).unwrap();
    assert_eq!(found.file_name().unwrap(), "Film.srt");

    assert!(!is_subtitle_file_with(Path::new("dir/.srt"), ImportFlavor::Mac));
    assert!(is_subtitle_file_with(Path::new("dir\\.srt"), ImportFlavor::Windows));
    assert!(!is_subtitle_file_with(Path::new("dir/Film.srt."), ImportFlavor::Mac));

    // A subtitle named "Clip." has the stem "Clip." on macOS: no companion "Clip.mp4".
    std::fs::write(dir.join("Clip.mp4"), b"").unwrap();
    std::fs::write(dir.join("Clip."), b"").unwrap();
    assert_eq!(find_companion_video_listing(&dir.join("Clip.")), None);
}

use std::path::Path;

fn ass_times(body: &str, flavor: ImportFlavor) -> Vec<(f64, f64, String)> {
    let content = format!("[Events]\nFormat: Start, End, Text\n{body}");
    parse_with(&content, Some(".ass"), flavor).into_iter().map(|s| (s.start, s.end, s.text)).collect()
}

#[test]
fn windows_timestamps_wrap_in_32_bit_and_skip_only_ascii_white_space() {
    let w = ImportFlavor::Windows;
    let wrapped = (600_000i32.wrapping_mul(3600)) as f64;
    assert_eq!(ass_times("Dialogue: 600000:00:00.00, 600000:00:01.00, wrapped\n", w), vec![(wrapped, wrapped + 1.0, "wrapped".into())]);
    // A no-break space inside a part fails `int.TryParse` (start 0); an ASCII space does not.
    assert_eq!(ass_times("Dialogue: 0:\u{A0}00:01.00, 0:00:02.00, nbsp\n", w), vec![(0.0, 2.0, "nbsp".into())]);
    assert_eq!(ass_times("Dialogue: 0: 00:01.00, 0:00:02.00, space\n", w), vec![(1.0, 2.0, "space".into())]);
    // Out of the `int` range: the part fails, the time is 0.
    assert_eq!(ass_times("Dialogue: 2147483648:0:0, 0:0:1, over\n", w), vec![(0.0, 1.0, "over".into())]);
}

#[test]
fn mac_timestamps_are_64_bit_and_reject_white_space_in_parts() {
    let m = ImportFlavor::Mac;
    assert_eq!(
        ass_times("Dialogue: 600000:00:00.00, 600000:00:01.00, big\n", m),
        vec![(2_160_000_000.0, 2_160_000_001.0, "big".into())]
    );
    assert_eq!(ass_times("Dialogue: 0: 00:01.00, 0:00:02.00, space\n", m), vec![(0.0, 2.0, "space".into())]);
    assert_eq!(ass_times("Dialogue: 0:00 :01.00, 0:00:02.00, space before colon\n", m), vec![(0.0, 2.0, "space before colon".into())]);
    assert_eq!(ass_times("Dialogue: +0:00:01.00, 0:00:+2.5, signs\n", m), vec![(1.0, 2.5, "signs".into())]);
    // SRT timings come from the regex (digits only), so both apps agree there.
    let srt = "1\n01:00:00,500 --> 01:00:01,000\nx\n";
    assert_eq!(parse_with(srt, Some(".srt"), m)[0].start, 3600.5);
    assert_eq!(parse_with(srt, Some(".srt"), ImportFlavor::Windows)[0].start, 3600.5);
}

#[test]
fn catalog_rows_with_null_kind_or_backend_take_the_defaults() {
    let row: EngineModel = serde_json::from_str(
        r#"{"id":"x","kind":null,"backend":null,"languages":null,"size_mb":1,"disk_mb":null,"speed":1,"accuracy":1,"license":null,"downloaded":false}"#,
    )
    .unwrap();
    assert_eq!(row.kind, "transcription");
    assert_eq!(row.backend, "whisper");
    assert!(row.detects_language());
    let absent: EngineModel = serde_json::from_str(r#"{"id":"y","size_mb":1,"speed":1,"accuracy":1,"downloaded":true}"#).unwrap();
    assert_eq!((absent.kind.as_str(), absent.backend.as_str()), ("transcription", "whisper"));
    assert!(serde_json::from_str::<EngineModel>(r#"{"id":"z","kind":1,"size_mb":1,"speed":1,"accuracy":1,"downloaded":true}"#).is_err());
}

#[test]
fn mac_log_keeps_every_line_of_a_burst() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("diagnostics.log");
    let log = DiagnosticLog::new(path.clone(), DiagnosticFormat::Mac, Box::new(Vec::new));
    let lines: Vec<String> = (0..3000).map(|i| format!("{{\"event\":\"batch_start\",\"n\":{i}}}")).collect();
    log.write_engine_lines(&lines, &[]);
    log.flush();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 3000);
    assert!(text.lines().last().unwrap().ends_with("\"n\":2999}"));
    drop(log);
}

#[test]
fn mac_rotation_failure_drops_the_batch_instead_of_growing_the_log() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("diagnostics.log");
    std::fs::write(&path, vec![b'x'; 64]).unwrap();
    // A non-empty directory at `<log>.old` can be neither removed by `remove_file` nor replaced by the rename.
    let old = temp.path().join("diagnostics.log.old");
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("keep"), b"").unwrap();
    let log = DiagnosticLog::with_max_bytes(path.clone(), DiagnosticFormat::Mac, 16, Box::new(Vec::new));
    log.write_redacting("t", "dropped", &[]);
    log.flush();
    assert_eq!(std::fs::read(&path).unwrap(), vec![b'x'; 64]);

    std::fs::remove_dir_all(&old).unwrap();
    log.write_redacting("t", "after", &[]);
    log.flush();
    assert!(std::fs::read_to_string(&path).unwrap().ends_with("[t] after\n"));
    assert_eq!(std::fs::read(&old).unwrap(), vec![b'x'; 64]);
    let _ = Arc::strong_count(&log);
}

#[test]
fn a_line_of_only_a_line_separator_ends_a_cue_on_windows_but_not_on_mac() {
    let srt = "1\n00:00:01,000 --> 00:00:02,000\nfirst\n\u{2028}\nsecond\n\n2\n00:00:03,000 --> 00:00:04,000\nthird\n";
    let texts = |flavor| parse_with(srt, Some(".srt"), flavor).into_iter().map(|s| s.text).collect::<Vec<_>>();
    assert_eq!(texts(ImportFlavor::Windows), ["first", "third"]);
    assert_eq!(texts(ImportFlavor::Mac), ["first second", "third"]);
    // A no-break space line is blank for both (`.whitespaces` holds the space separators).
    let nbsp = "00:00:01,000 --> 00:00:02,000\nfirst\n\u{A0}\nsecond\n";
    assert_eq!(parse_with(nbsp, Some(".srt"), ImportFlavor::Mac)[0].text, "first");
    assert_eq!(parse_with(nbsp, Some(".srt"), ImportFlavor::Windows)[0].text, "first");
}
