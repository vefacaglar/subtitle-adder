//! Behavior parity with the C# app (Windows): the same inputs go through the real C# `SubtitleAdder.Core` code
//! (fixture tool `apps/desktop/tools/fixtures/CoreCompat`, which compiles the C# app's own sources read-only) and
//! through the Rust port, and the outputs must be identical: model labels and rows, sizes, byte sizes, line times,
//! hex colors and overlay styles, subtitle import and decoding, AtomicFileStore reads, diagnostic path redaction,
//! settings.json typed reads and cross-writes, companion/sidecar lookups.
//!
//! Opt-in: `SA_DOTNET_FIXTURES=1 cargo test -p sa-core --test csharp_compat` (needs the .NET 10 SDK; the tool is
//! built on first use with `dotnet build -c Release`). Without the variable every test returns early.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use sa_core::appearance::{AppearanceSettings, HexColor, OverlayBackground, OverlayStyle, SubtitleDisplay, ThemeChoice};
use sa_core::diagnostics::DiagnosticLog;
use sa_core::engine_types::EngineModel;
use sa_core::l10n::{LanguageState, UiLanguage};
use sa_core::models::{ModelLabel, ModelRow, SubtitleLanguage, fallback_models};
use sa_core::settings::{InMemorySettingsStore, JsonFileSettingsStore, SettingsStore};
use sa_core::subtitle_import::{ImportFlavor, decode_with, find_companion_video_windows, find_sidecar_subtitle, is_subtitle_file, parse_file_with};
use sa_core::text::{format_byte_size_windows, subtitle_time_short};
use sa_core::undo::{redo_title, undo_title};
use serde_json::Value;

static LANGUAGE: Mutex<()> = Mutex::new(());

fn enabled() -> bool {
    let on = std::env::var("SA_DOTNET_FIXTURES").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set SA_DOTNET_FIXTURES=1 to run the C# compatibility tests");
    }
    on
}

fn tool_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/CoreCompat")
}

static BUILD: Mutex<()> = Mutex::new(());

fn tool_exe() -> PathBuf {
    let _guard = BUILD.lock().unwrap_or_else(|e| e.into_inner());
    let exe = tool_dir().join("bin/Release/net10.0/CoreCompat.exe");
    let program = tool_dir().join("Program.cs");
    let stale = match (std::fs::metadata(&exe).and_then(|m| m.modified()), std::fs::metadata(&program).and_then(|m| m.modified())) {
        (Ok(built), Ok(source)) => built < source,
        _ => true,
    };
    if stale {
        let status = Command::new("dotnet")
            .args(["build", "-c", "Release", "-nologo", "-v", "q"])
            .current_dir(tool_dir())
            .status()
            .expect("dotnet is on PATH");
        assert!(status.success(), "building the CoreCompat fixture tool failed");
    }
    exe
}

/// Runs the C# tool (TMP/TEMP point at `temp`, so a touched `DiagnosticLog` never scrubs the real temp folder) and
/// returns one JSON array per output line.
fn csharp(temp: &Path, args: &[&str]) -> Vec<Vec<Value>> {
    let out = Command::new(tool_exe()).args(args).env("TMP", temp).env("TEMP", temp).output().expect("CoreCompat runs");
    assert!(
        out.status.success(),
        "CoreCompat {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("UTF-8 output")
        .lines()
        .map(|l| match serde_json::from_str::<Value>(l).expect("a JSON line") {
            Value::Array(a) => a,
            other => panic!("not an array: {other}"),
        })
        .collect()
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_else(|| panic!("string expected, got {v}")).to_owned()
}

fn opt_s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

/// A C# "R" double.
fn d(v: &Value) -> f64 {
    s(v).parse().unwrap_or_else(|_| panic!("double expected, got {v}"))
}

fn units(text: &str) -> String {
    text.encode_utf16().map(|u| format!("{u:04X}")).collect::<Vec<_>>().join(" ")
}

fn write_lines(path: &Path, lines: &[String]) {
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn json_lines(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| serde_json::to_string(v).unwrap()).collect()
}

fn language(code: &str) -> UiLanguage {
    if code == "en" { UiLanguage::En } else { UiLanguage::Tr }
}

fn model(id: &str, downloaded: bool, disk_mb: Option<i64>) -> EngineModel {
    let mut m = if id == "custom-x" {
        EngineModel::new("custom-x", 12, 0, 0, false)
    } else {
        fallback_models().into_iter().find(|m| m.id == id).unwrap_or_else(|| panic!("unknown fallback id {id}"))
    };
    m.downloaded = downloaded;
    m.disk_mb = disk_mb;
    m
}

#[test]
fn labels_rows_and_titles_match_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let lines = csharp(temp.path(), &["labels"]);
    let _guard = LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
    let saved = LanguageState::shared().language();
    let mut checked = 0;
    for line in &lines {
        let kind = s(&line[0]);
        LanguageState::shared().set_language(language(&s(&line[1])));
        match kind.as_str() {
            "label" => {
                let m = model(&s(&line[2]), line[3].as_bool().unwrap(), line[4].as_i64());
                let rust = [
                    ModelLabel::name(&m.id),
                    ModelLabel::menu_title(&m),
                    ModelLabel::detail(&m),
                    ModelLabel::language_summary(&m),
                    ModelLabel::storage_status(&m),
                    ModelLabel::license_text(&m),
                ];
                let cs: Vec<String> = line[5..11].iter().map(s).collect();
                assert_eq!(rust.to_vec(), cs, "{line:?}");
            }
            "row" => {
                let m = model(&s(&line[2]), line[3].as_bool().unwrap(), line[4].as_i64());
                let row = ModelRow::new(&m, line[5].as_bool().unwrap(), line[6].as_bool().unwrap(), None);
                let rust = vec![
                    Value::from(row.name),
                    row.language_summary.into(),
                    row.storage_status.into(),
                    row.license_text.into(),
                    row.is_non_commercial.into(),
                    row.shows_delete_button.into(),
                    row.can_delete.into(),
                    row.delete_help.into(),
                    row.delete_confirm_title.into(),
                    row.delete_confirm_message.into(),
                ];
                assert_eq!(rust, line[7..17].to_vec(), "{line:?}");
            }
            "language" => {
                let l = SubtitleLanguage::from_code(&s(&line[2])).expect("known code");
                assert_eq!([l.title(), l.short_name(), l.target_name()].to_vec(), line[3..6].iter().map(s).collect::<Vec<_>>());
            }
            "theme" => {
                let t = ThemeChoice::parse(Some(&s(&line[2]))).unwrap();
                assert_eq!(t.title(), s(&line[3]));
            }
            "background" => {
                let b = OverlayBackground::parse(Some(&s(&line[2]))).unwrap();
                assert_eq!(b.title(), s(&line[3]));
            }
            "display" => {
                let v = SubtitleDisplay::parse(Some(&s(&line[2]))).unwrap();
                assert_eq!(v.title(), s(&line[3]));
            }
            "percent" => {
                let store = InMemorySettingsStore::new();
                let appearance = AppearanceSettings::new(store);
                let v = d(&line[2]);
                appearance.set_size(v);
                appearance.set_opacity(v);
                assert_eq!((appearance.size_text(), appearance.opacity_text()), (s(&line[3]), s(&line[4])), "{line:?}");
            }
            "undo" => {
                let key = s(&line[2]);
                assert_eq!((undo_title(Some(&key)), redo_title(Some(&key))), (s(&line[3]), s(&line[4])));
            }
            "undo-empty" => assert_eq!((undo_title(None), redo_title(None)), (s(&line[2]), s(&line[3]))),
            "total" => {
                assert_eq!(sa_core::l!("Toplam %@ disk kullanılıyor.", ModelLabel::size(4339)), s(&line[2]));
                assert_eq!(sa_core::l10n::l("Henüz indirilmiş model yok."), s(&line[3]));
            }
            other => panic!("unknown line kind {other}"),
        }
        checked += 1;
    }
    LanguageState::shared().set_language(saved);
    assert!(checked > 400, "only {checked} lines");
}

#[test]
fn size_texts_match_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    for (from, to) in [(-5, 20_000), (99_900, 100_100), (999_900, 1_000_100)] {
        let lines = csharp(temp.path(), &["sizes", &from.to_string(), &to.to_string()]);
        assert_eq!(lines.len() as i64, to - from + 1);
        for line in lines {
            let mb = line[0].as_i64().unwrap();
            assert_eq!(ModelLabel::size_text(mb, UiLanguage::Tr), s(&line[1]), "tr {mb}");
            assert_eq!(ModelLabel::size_text(mb, UiLanguage::En), s(&line[2]), "en {mb}");
        }
    }
}

/// A small deterministic generator (no extra dev-dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }
}

#[test]
fn byte_sizes_match_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let mut values: Vec<i64> = vec![-1, 0, 1, 2, 5, 10, 100, 1000, 1023, 1024, 5000, 5243, 10_485, 10_486, 52_428, 52_429, 104_857, 1_048_575];
    const MIB: i64 = 1024 * 1024;
    for k in [1, 2, 3, 10, 100, 999, 1023, 1024, 1025, 2047, 2048, 5120, 20_480, 512_000] {
        values.extend([k * MIB - 1, k * MIB, k * MIB + 1, k * MIB + MIB / 20, k * MIB + MIB / 2, k * MIB + MIB * 95 / 100]);
    }
    // Exact .NET midpoints of the 0.# / 0.## formats.
    values.extend([MIB + MIB / 20, 3 * MIB + MIB / 4 * 1, 1024 * MIB + 1024 * MIB / 200, 1024 * MIB * 5 + 1024 * MIB / 8, MIB / 200 * 1]);
    let mut rng = Lcg(42);
    for _ in 0..3000 {
        let scale = [1_000u64, 1_000_000, 100_000_000, 10_000_000_000, 2_000_000_000_000][(rng.next() % 5) as usize];
        values.push((rng.next() % scale) as i64 + 1);
    }
    let input = temp.path().join("bytes.txt");
    write_lines(&input, &values.iter().map(i64::to_string).collect::<Vec<_>>());
    for line in csharp(temp.path(), &["bytes", input.to_str().unwrap()]) {
        let b: i64 = s(&line[0]).parse().unwrap();
        assert_eq!(format_byte_size_windows(b, UiLanguage::Tr), s(&line[1]), "tr {b}");
        assert_eq!(format_byte_size_windows(b, UiLanguage::En), s(&line[2]), "en {b}");
    }
}

#[test]
fn line_times_match_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let mut values: Vec<f64> = vec![
        0.0, -0.0, 0.004, 0.005, 0.0049999, 0.015, 0.025, 0.125, 1.005, 2.675, 59.994, 59.995, 59.999, 60.0, 65.25, 119.995,
        599.995, 3599.995, 3600.0, 5999.99, 6000.0, 359_999.995, -0.004, -0.005, -1.5, -59.995, -61.25, 1e6 + 0.125, 12.345, 0.1, 0.2,
        0.3, 0.7, 10.005, 10.015, 10.025, 10.035, 10.045, 1.115, 1.125, 2.135, 33.335,
    ];
    let mut rng = Lcg(7);
    for _ in 0..3000 {
        let ms = rng.next() % 10_000_000;
        values.push(ms as f64 / 1000.0);
        values.push((rng.next() % 1_000_000_000) as f64 / 1_000_000.0);
    }
    let input = temp.path().join("times.txt");
    write_lines(&input, &values.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>());
    for line in csharp(temp.path(), &["times", input.to_str().unwrap()]) {
        let v: f64 = s(&line[0]).parse().unwrap();
        assert_eq!(subtitle_time_short(v), s(&line[1]), "{v:?}");
    }
}

#[test]
fn hex_colors_and_overlay_styles_match_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let inputs = [
        "#FFFFFF", "#ff8000", "ff8000", " #FF8000 ", "\t#00ff00\n", "##FF8000", "#FFF", "#FFFFFFF", "#GGGGGG", "", " ", "#", "+FFFFF",
        "-00000", "0x1234", "#12345", "#1234567", "#ＦＦＦＦＦＦ", "#ff80 0", "\u{00A0}#123456\u{2003}", "#aBcDeF", "#000000",
        "#7f7f7f", "ß12345",
    ];
    let input = temp.path().join("hex.txt");
    write_lines(&input, &json_lines(&inputs));
    for line in csharp(temp.path(), &["hex", input.to_str().unwrap()]) {
        let text = s(&line[0]);
        let parsed = HexColor::parse(&text);
        let cs_parsed = line[1].as_array().map(|a| (d(&a[0]), d(&a[1]), d(&a[2])));
        assert_eq!(parsed, cs_parsed, "{text:?}");
        assert_eq!(HexColor::parse_rgb(&text).map(|c| c.hex()), opt_s(&line[2]), "{text:?}");
        let style = OverlayStyle::new(String::new(), 0.05, text.clone(), OverlayBackground::Box, 0.6);
        assert_eq!(style.text_hex, s(&line[3]), "{text:?}");
        assert_eq!(style.text_color().hex(), s(&line[4]), "{text:?}");
    }

    let values: Vec<String> = [
        -1.0, 0.0, 0.01, 0.03, 0.0300001, 0.05, 0.07, 0.09, 0.1, 0.5, 0.6, 1.0, 1.5, 0.001, 0.0019, 0.002, 0.00196, 0.998, 0.99804,
        0.5019607843137255, 0.50196, 1e-9,
    ]
    .iter()
    .map(|v: &f64| format!("{v:?}"))
    .chain(["NaN".to_owned(), "Infinity".to_owned(), "-Infinity".to_owned()])
    .collect();
    let input = temp.path().join("style.txt");
    write_lines(&input, &values);
    for line in csharp(temp.path(), &["style", input.to_str().unwrap()]) {
        let raw = s(&line[0]);
        let v: f64 = match raw.as_str() {
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            other => other.parse().unwrap(),
        };
        let style = OverlayStyle::from_stored("", v, "#FFFFFF", "nope", v);
        let rust = [
            style.size_fraction,
            style.background_opacity,
            style.font_size(720.0),
            style.outline_thickness(720.0),
            style.outline_thickness(100.0),
            style.shadow_opacity(),
        ];
        let cs = [d(&line[1]), d(&line[2]), d(&line[4]), d(&line[5]), d(&line[6]), d(&line[7])];
        assert_eq!(rust.map(f64::to_bits), cs.map(f64::to_bits), "{raw}");
        assert_eq!(style.background.raw(), s(&line[3]));
        assert_eq!(HexColor::format(v, v / 2.0, 1.0 - v), s(&line[8]), "{raw}");
    }
}

/// Subtitle inputs that exercise every parser branch (and its edge cases).
fn subtitle_inputs() -> Vec<(&'static str, Vec<u8>)> {
    let t = |s: &str| s.as_bytes().to_vec();
    let mut files: Vec<(&'static str, Vec<u8>)> = vec![
        ("01-basic.srt", t("1\n00:00:01,000 --> 00:00:02,500\n<i>Merhaba</i> dünya.\nİkinci satır.\n\n2\n00:00:03,000 --> 00:00:04,000\n- Evet.\n")),
        ("02-crlf.srt", t("00:00:01,000 --> 00:00:02,000\r\nA\r\n\r\n00:00:03,000 --> 00:00:04,000\r\nB\r\n")),
        ("03-cr.srt", t("00:00:01,000 --> 00:00:02,000\rA\r\r00:00:03,000 --> 00:00:04,000\rB")),
        ("04-cues.vtt", t("WEBVTT\n\nNOTE this is a comment\nstill the comment\n\nSTYLE\n::cue { color: red }\n\nintro\n00:01.000 --> 00:02.500 align:start\nFirst VTT cue\n\n01:00:00.250 --> 01:00:01.000\n<v Roger>Late cue</v>")),
        ("05-tight.srt", t("1\n00:00:05,000 --> 00:00:05,010\nshort\n\n2\n00:00:07,000 --> 00:00:06,000\nbackwards\n\n3\n00:00:08,1 --> 00:00:09,12\nfractions\n\n4\n0:0:10.5 --> 0:0:11.25\nshort fields\n")),
        ("06-markup.srt", t("1\n00:00:01,000 --> 00:00:02,000\n{\\an8}<font color=\"red\">Bir</font>   iki\t\tüç &amp; <b></b>\n<i></i>\n\n2\n00:00:03,000 --> 00:00:04,000\n<>{}<  >\n")),
        ("07-rounding.srt", t("1\n00:00:01,0005 --> 00:00:02,0015\nfour digits\n\n2\n00:00:01,2345 --> 00:00:01,2346\nx\n\n3\n00:00:00,001 --> 00:00:00,0015\ny\n")),
        ("08-no-blank.srt", t("1\n00:00:01,000 --> 00:00:02,000\nA\n2\n00:00:03,000 --> 00:00:04,000\nB\n")),
        ("09-bom.srt", [&[0xEF, 0xBB, 0xBF][..], b"\n\n 1\n00:00:01,000 --> 00:00:02,000\nBOM\n"].concat()),
        ("10-webvtt-mixed.vtt", t("webvtt kind\n\n00:00:01.000-->00:00:02.000\nno spaces\n\n  00:00:03.000   -->   00:00:04.000  \n  padded  \n")),
        ("11-note-lower.vtt", t("WEBVTT\n\nnote not a note\n00:00:01.000 --> 00:00:02.000\ncue after lower note\n\nNOTES\nx\n\n00:00:03.000 --> 00:00:04.000\nafter NOTES\n")),
        (
            "12-basic.ass",
            t("[Script Info]\nTitle: x\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:02.50,Default,,0,0,0,,{\\i1}Merhaba{\\i0}\\Ndünya, nasılsın?\nComment: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,skip\nDialogue: 0,0:00:05.00,0:00:04.00,Default,,0,0,0,,backwards\nDialogue: 0,0:00:06.00,0:00:06.01,Default,,0,0,0,,a\\hb\\nc\n"),
        ),
        ("13-reordered.ssa", t("[Events]\nFormat: Text, End, Start\nDialogue: first, text,0:00:02.00,0:00:01.00\nDialogue: too,few\ndialogue: lower,0:00:04.00,0:00:03.00\n[Other]\nDialogue: x,0:00:06.00,0:00:05.00\n")),
        ("14-sniffed-ass.srt", t("[events]\nFormat: Start, End, Text\nDialogue: 0:00:01.00, 0:00:02.00, sniffed\n")),
        ("15-default-format.ass", t("[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,no format line, with comma\nDialogue: 0,1:2,3:4,x,,0,0,0,,odd times\nDialogue: 0, 0:00:05.00 , 0:00:06.00 ,x,,0,0,0,,spaced\n")),
        ("16-engine.json", t(r#"[{"id":7,"start":1.5,"end":2,"text":"Merhaba","words":[{"start":1.5,"end":2,"text":"Merhaba"}],"speaker":1,"needs_review":true},{"id":3,"start":2,"end":3,"text":"x"}]"#)),
        ("17-lenient.json", t(r#"  [{"id":1,"start":0,"end":1,"text":"a","words":null,"speaker":null,"needs_review":"true","extra":{"x":[1,2]}}]  "#)),
        ("18-bad.json", t(r#"[{"id":1,"start":0,"end":1}]"#)),
        ("19-float-id.json", t(r#"[{"id":1.0,"start":0,"end":1,"text":"a"}]"#)),
        ("20-garbage.json", t("[not json")),
        ("21-sniffed.txt", t(r#"[{"id":2,"start":0.25,"end":1,"text":"sniffed json"}]"#)),
        ("22-null.json", t("null")),
        ("23-object.json", t(r#"{"id":1}"#)),
        ("24-empty.srt", t("")),
        ("25-blank.srt", t(" \r\n\t\n")),
        ("26-unknown.xyz", t("1\n00:00:01,000 --> 00:00:02,000\nunknown extension\n")),
        ("27-hours.srt", t("1\n99:59:59,999 --> 99:59:59,999\nlast\n\n2\n00:61:00,000 --> 00:62:00,000\nbig minutes\n")),
        ("28-arrow-in-text.srt", t("1\n00:00:01,000 --> 00:00:02,000\nnot a --> timing\n00:00:03,000 --> 00:00:04,000\ntext line as timing\n")),
        ("29-words-bad.json", t(r#"[{"id":1,"start":0,"end":1,"text":"a","words":[{"start":0,"text":"w"}]}]"#)),
        ("30-dup-key.json", t(r#"[{"id":1,"id":2,"start":0,"end":1,"text":"a"}]"#)),
        ("31-big-id.json", t(r#"[{"id":3000000000,"start":0,"end":1,"text":"a"}]"#)),
        ("32-string-num.json", t(r#"[{"id":1,"start":"0","end":1,"text":"a"}]"#)),
        ("33-trailing.json", t(r#"[{"id":1,"start":0,"end":1,"text":"a"}] x"#)),
        ("34-speaker-float.json", t(r#"[{"id":1,"start":0,"end":1,"text":"a","speaker":1.5}]"#)),
        ("35-unicode-ws.srt", "1\n00:00:01,000 --> 00:00:02,000\n\u{00A0}nbsp\u{2003}line\u{3000}\n\u{2028}\n\n".as_bytes().to_vec()),
        ("36-legacy-1254.srt", [&b"1\n00:00:01,000 --> 00:00:02,000\n"[..], &[0xD0, 0xFE, 0xDD, 0xF0, 0xFD, 0xDE, 0x80, 0x9E, 0x8E], b"\n"].concat()),
        ("37-utf16le.srt", [vec![0xFF, 0xFE], "1\r\n00:00:01,000 --> 00:00:02,000\r\nÇift bayt\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect()].concat()),
        ("38-utf16be.vtt", [vec![0xFE, 0xFF], "WEBVTT\n\n00:01.000 --> 00:02.000\nBüyük uçlu\n".encode_utf16().flat_map(u16::to_be_bytes).collect()].concat()),
        ("39-events-in-text.srt", t("1\n00:00:01,000 --> 00:00:02,000\nthe [Events] tag\n")),
        ("40-json-ext-not-array.json", t("1\n00:00:01,000 --> 00:00:02,000\nsrt in a json file\n")),
        // Deliberate deviation (not compared): C# also parses `Infinity`/`NaN` seconds and keeps a line ending at +∞,
        // which its own engine writer then rejects; the port reads such a timestamp as 0 (see `parse_timestamp`).
        (
            "41-exp-signs.ass",
            t("[Events]\nFormat: Start, End, Text\nDialogue: 0:00:01.00, 0:00:1e1, exp\nDialogue: +0:00:01.00, 0:00:+2.5, signs\nDialogue: 0:00:01.00, 0:00:02.00 ,  padded text  \nDialogue: 0:00:01.00, 0:00:2.5E+0, upper exp\nDialogue: 0:00:01.00, 0:00:.5e1, leading point\nDialogue: 0:00:01.00, 0:00:3., trailing point\nDialogue: 0:00:01.00, -0:00:-1, negative\n"),
        ),
        ("42-timing-next-line.vtt", t("WEBVTT\ncue-id\n00:00:01.000 --> 00:00:02.000\nafter id without blank\n")),
    ];
    // Every single byte value after a valid prefix (exercises the Windows-1254 table).
    let mut all = b"1\n00:00:01,000 --> 00:00:02,000\n".to_vec();
    all.extend((0x20u8..=0xFF).filter(|b| *b != b'\n' && *b != b'\r'));
    all.push(b'\n');
    files.push(("43-all-bytes.srt", all));
    // C# computes `h * 3600 + m * 60` in wrapping 32-bit `int`, and its number parsing skips only ASCII white space.
    files.push((
        "44-huge-hours.ass",
        "[Events]\nFormat: Start, End, Text\nDialogue: 600000:00:00.00, 600000:00:01.00, wrapped\nDialogue: 0:\u{00A0}00:01.00, 0:00:02.00, nbsp part\nDialogue: 0: 00:01.00, 0:00:02.00 , ascii space part\nDialogue: 2147483647:0:0, 2147483647:0:1, int max\nDialogue: 2147483648:0:0, 0:0:1, int overflow\nDialogue: 1:\u{2003}2, 1:3, em space minutes\n"
            .as_bytes()
            .to_vec(),
    ));
    files
}

fn rust_sentence_rows(sentences: &[sa_core::engine_types::Sentence]) -> Vec<Value> {
    sentences
        .iter()
        .map(|s| {
            serde_json::json!([
                s.id,
                s.start,
                s.end,
                s.text,
                s.words.iter().map(|w| serde_json::json!([w.start, w.end, w.text])).collect::<Vec<_>>(),
                s.speaker,
                s.needs_review
            ])
        })
        .collect()
}

fn csharp_sentence_rows(v: &Value) -> Vec<Value> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let r = row.as_array().unwrap();
            let words: Vec<Value> =
                r[4].as_array().unwrap().iter().map(|w| serde_json::json!([d(&w[0]), d(&w[1]), w[2].clone()])).collect();
            serde_json::json!([r[0], d(&r[1]), d(&r[2]), r[3], words, r[5], r[6]])
        })
        .collect()
}

#[test]
fn subtitle_import_matches_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("subs");
    std::fs::create_dir(&dir).unwrap();
    for (name, bytes) in subtitle_inputs() {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    let lines = csharp(temp.path(), &["parse", dir.to_str().unwrap()]);
    assert_eq!(lines.len(), subtitle_inputs().len());
    for line in lines {
        let name = s(&line[0]);
        let rust = parse_file_with(&dir.join(&name), ImportFlavor::Windows).unwrap();
        assert_eq!(rust_sentence_rows(&rust), csharp_sentence_rows(&line[1]), "{name}");
    }

    let decoded = csharp(temp.path(), &["decode", dir.to_str().unwrap()]);
    for line in decoded {
        let name = s(&line[0]);
        let bytes = std::fs::read(dir.join(&name)).unwrap();
        assert_eq!(units(&decode_with(&bytes, ImportFlavor::Windows)), s(&line[1]), "{name}");
    }
}

#[test]
fn decoding_and_atomic_reads_match_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("enc");
    std::fs::create_dir(&dir).unwrap();
    let utf16 = |s: &str, be: bool| -> Vec<u8> { s.encode_utf16().flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() }).collect() };
    let utf32 = |s: &str, be: bool| -> Vec<u8> { s.chars().flat_map(|c| if be { (c as u32).to_be_bytes() } else { (c as u32).to_le_bytes() }).collect() };
    let text = "ğüşıöç İ 😀 {\"a\":1}";
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("a-plain", text.as_bytes().to_vec()),
        ("b-utf8-bom", [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat()),
        ("c-utf16le", [vec![0xFF, 0xFE], utf16(text, false)].concat()),
        ("d-utf16be", [vec![0xFE, 0xFF], utf16(text, true)].concat()),
        ("e-utf32le", [vec![0xFF, 0xFE, 0, 0], utf32(text, false)].concat()),
        ("f-utf32be", [vec![0, 0, 0xFE, 0xFF], utf32(text, true)].concat()),
        ("g-invalid-utf8", vec![b'a', 0xC3, b'b', 0xE2, 0x82, b'c', 0xF0, 0x9F, 0x98, 0xED, 0xA0, 0x80, 0xC0, 0xAF, 0xFF, b'z']),
        ("h-utf16le-odd", [vec![0xFF, 0xFE], utf16("ab", false), vec![0x41]].concat()),
        ("i-utf16le-lone-surrogate", [vec![0xFF, 0xFE], vec![0x3D, 0xD8, 0x41, 0x00, 0x00, 0xDC]].concat()),
        ("j-utf32le-bad", [vec![0xFF, 0xFE, 0, 0], vec![0x00, 0xD8, 0, 0, 0x41, 0, 0, 0, 0x00, 0x00, 0x11, 0x00, 0x42]].concat()),
        ("k-empty", vec![]),
        ("l-bom-only", vec![0xEF, 0xBB, 0xBF]),
        ("m-ff-fe-only", vec![0xFF, 0xFE]),
        ("n-utf8-bom-invalid", vec![0xEF, 0xBB, 0xBF, 0x80, b'x']),
        ("o-1254-text", vec![b'D', 0xFD, b's', 0xE7, 0xF0, 0xFE, 0x81, 0x8D, 0x8F, 0x90, 0x9D]),
        ("p-cp-ff-fe-00-01", vec![0xFF, 0xFE, 0x00, 0x01]),
        ("q-ef-bb", vec![0xEF, 0xBB]),
    ];
    for (name, bytes) in &files {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    for line in csharp(temp.path(), &["atomic-read", dir.to_str().unwrap()]) {
        let name = s(&line[0]);
        let rust = sa_core::atomic_file::read_all_text_shared(&dir.join(&name), 5);
        assert_eq!(rust.as_deref().map(units), opt_s(&line[1]), "atomic {name}");
    }
    for line in csharp(temp.path(), &["decode", dir.to_str().unwrap()]) {
        let name = s(&line[0]);
        let bytes = std::fs::read(dir.join(&name)).unwrap();
        assert_eq!(units(&decode_with(&bytes, ImportFlavor::Windows)), s(&line[1]), "decode {name}");
    }
}

#[test]
fn diagnostic_path_redaction_matches_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let inputs = [
        "",
        "plain message",
        "open failed: C:\\Users\\Özcan\\Videos\\film.mp4 (code 5)",
        "C:/Users/x/a b.mkv",
        "path 'D:\\x\\y.srt' quoted and \"E:\\q\" double",
        "unc \\\\server\\share\\dir\\f.mp4 end",
        "unc no trailing \\\\server",
        "file:///C:/a/b.mp4 and file://host/x",
        "file:/single",
        "AC:\\not-a-word-boundary",
        "_C:\\underscore",
        "é:\\unicode-letter",
        "1:\\digit",
        "x C:relative",
        "ratio 3:4 and time 10:20",
        "two C:\\a then C:\\b",
        "tab\tC:\\a\tafter",
        "emoji 😀 C:\\😀\\x",
        "C:\\",
        "c:\\lower",
        "url https://example.com/C:\\x",
        "line\\nC:\\escaped-newline",
        "ÇC:\\turkish-letter-before",
        "(C:\\in parens)",
        "\\\\?\\C:\\long",
    ];
    let input = temp.path().join("redact.txt");
    write_lines(&input, &json_lines(&inputs));
    let lines = csharp(temp.path(), &["redact", input.to_str().unwrap()]);
    assert_eq!(lines.len(), inputs.len());
    for (raw, line) in inputs.iter().zip(lines) {
        assert_eq!(DiagnosticLog::redact_paths(raw), s(&line[0]), "{raw:?}");
    }
}

#[test]
fn sidecar_and_companion_lookups_match_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("media");
    std::fs::create_dir(&dir).unwrap();
    for name in ["Film.mkv", "Film.srt", "Episode01.mp4", "Episode01.tr.srt", "Show.S01.mkv", "Show.S01.E02.vtt", "audio.MP3", "audio.ass",
        "x.longsuffix.srt", "x.mkv", "a.b.c.srt", "a.mov", "only.json", ".srt", "noext", "dir.srt"]
    {
        std::fs::write(dir.join(name), b"").unwrap();
    }
    std::fs::create_dir(dir.join("sub.mp4")).unwrap();
    std::fs::write(dir.join("sub.srt"), b"").unwrap();
    let base = dir.to_str().unwrap().to_owned();
    let paths: Vec<String> = [
        "Film.mkv", "Film.srt", "Episode01.tr.srt", "Episode01.mp4", "Show.S01.E02.vtt", "audio.ass", "audio.MP3", "x.longsuffix.srt",
        "a.b.c.srt", "only.json", ".srt", "noext", "missing.srt", "sub.srt", "Film.SRT", "FILM.mkv", "dir.srt\\", "Film.srt.",
    ]
    .iter()
    .map(|n| format!("{base}\\{n}"))
    .chain(["".to_owned(), "   ".to_owned(), "film.srt".to_owned(), format!("{base}/Film.srt"), "C:\\".to_owned()])
    .collect();
    let input = temp.path().join("paths.txt");
    write_lines(&input, &paths.iter().map(|p| serde_json::to_string(p).unwrap()).collect::<Vec<_>>());
    let lines = csharp(temp.path(), &["paths", input.to_str().unwrap()]);
    assert_eq!(lines.len(), paths.len());
    // .NET normalizes `/` to `\` in GetDirectoryName; compare the found file, not the separator spelling.
    let norm = |p: Option<PathBuf>| p.map(|p| p.to_string_lossy().replace('/', "\\"));
    let cs_norm = |v: &Value| opt_s(v).map(|p| p.replace('/', "\\"));
    for (path, line) in paths.iter().zip(lines) {
        let p = Path::new(path);
        assert_eq!(Value::from(is_subtitle_file(p)), line[1], "is_subtitle_file {path:?}");
        assert_eq!(norm(find_sidecar_subtitle(p)), cs_norm(&line[2]), "sidecar {path:?}");
        assert_eq!(norm(find_companion_video_windows(p)), cs_norm(&line[3]), "companion {path:?}");
    }
}

fn typed_reads(store: &dyn SettingsStore, key: &str) -> Vec<Value> {
    vec![
        key.into(),
        store.get_string(key).map(Value::from).unwrap_or(Value::Null),
        store.get_bool(key).map(Value::from).unwrap_or(Value::Null),
        store.get_int(key).map(Value::from).unwrap_or(Value::Null),
        store.get_double(key).map(|d| Value::from(format!("{d:?}"))).unwrap_or(Value::Null),
    ]
}

/// C# prints doubles with "R" (`3`, `1E-05`); compare them as numbers.
fn normalize_double(mut row: Vec<Value>) -> Vec<Value> {
    if let Some(text) = row[4].as_str() {
        row[4] = Value::from(text.parse::<f64>().unwrap().to_bits());
    }
    row
}

#[test]
fn settings_typed_reads_match_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{
  "s": "text", "empty": "", "esc": "C:\\Users\\\u00D6zcan", "t": true, "f": false, "strTrue": "true",
  "i": 3, "neg": -42, "f3": 3.0, "f35": 3.5, "e2": 1e2, "big": 3000000000, "min": -2147483648, "max": 2147483647,
  "over": 2147483648, "under": -2147483649, "huge": 1e300, "tiny": 1E-05, "zero": 0, "negzero": -0.0, "nul": null,
  "arr": [1, 2], "obj": {"a": 1}, "num-str": "12", "u64": 18446744073709551615, "exp-int": 2E3, "frac-int": 4.000
}"#,
    )
    .unwrap();
    let store = JsonFileSettingsStore::open(path.clone(), Duration::from_millis(50), None);
    for line in csharp(temp.path(), &["settings-read", path.to_str().unwrap()]) {
        let key = s(&line[0]);
        assert_eq!(normalize_double(typed_reads(store.as_ref(), &key)), normalize_double(line), "{key}");
    }
}

/// Typed reads of values written in memory (not parsed from a file) and the `Changed` count, op by op: covers the
/// cross-type reads (`set_int` then `get_double`, `set_double(3.0)` then `get_int`) and the deep-equality no-op
/// (`set_double(3.0)` after `set_int(3)`).
#[test]
fn in_memory_settings_ops_match_csharp() {
    if !enabled() {
        return;
    }
    let ops = [
        "int a 3",
        "int a 3",
        "double a 3",
        "double a 3.5",
        "double a 3",
        "int a 3",
        "double b 2147483648",
        "double b -0",
        "double b 0",
        "double b 1E-05",
        "double b NaN",
        "double b NaN",
        "string c 3",
        "string c true",
        "bool c true",
        "bool c true",
        "string c ",
        "remove c",
        "remove c",
        "int d -2147483648",
        "double d -2147483648",
        "double e 1E+300",
    ];
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("ops.txt");
    write_lines(&file, &ops.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    let store = InMemorySettingsStore::new();
    let changed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = changed.clone();
    let _sub = store.subscribe(Box::new(move |_| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    let lines = csharp(temp.path(), &["settings-ops", file.to_str().unwrap()]);
    assert_eq!(lines.len(), ops.len());
    for (index, (op, line)) in ops.iter().zip(lines).enumerate() {
        let parts: Vec<&str> = op.splitn(3, ' ').collect();
        let key = parts[1];
        match parts[0] {
            "int" => store.set_int(key, parts[2].parse().unwrap()),
            "double" => store.set_double(key, parts[2].parse().unwrap()),
            "string" => store.set_string(key, parts[2]),
            "bool" => store.set_bool(key, parts[2] == "true"),
            "remove" => store.remove(key),
            other => panic!("unknown op {other}"),
        }
        let mut rust = typed_reads(store.as_ref(), key);
        rust[0] = Value::from(changed.load(std::sync::atomic::Ordering::SeqCst));
        let mut cs = line;
        cs.remove(0);
        // Deliberate, unobservable deviation: a value written in memory by C# `SetInt` (a `JsonValue<int>`) reads as
        // `null` through `GetDouble` until the file is reloaded, while the port reads it as a number (as C# does after
        // a restart). No key of either app is written as an int and read as a double.
        if matches!(index, 0..=2 | 19 | 20) {
            assert_eq!(cs[4], Value::Null, "{op}: the C# quirk changed");
            cs[4] = rust[4].clone();
        }
        assert_eq!(normalize_double(rust), normalize_double(cs), "{op}");
    }
}

#[test]
fn settings_files_cross_read_between_csharp_and_rust() {
    if !enabled() {
        return;
    }
    // C# writes, Rust reads.
    let temp = tempfile::tempdir().unwrap();
    let cs_path = temp.path().join("cs").join("settings.json");
    csharp(temp.path(), &["settings-write", cs_path.to_str().unwrap()]);
    let raw = std::fs::read(&cs_path).unwrap();
    assert!(!raw.starts_with(&[0xEF, 0xBB, 0xBF]), "no BOM");
    let store = JsonFileSettingsStore::open(cs_path.clone(), Duration::from_millis(50), None);
    assert_eq!(store.get_string("uiLanguage").as_deref(), Some("en"));
    assert_eq!(store.get_string("explorerLastFolder").as_deref(), Some("C:\\Users\\Özcan\\Videos <&> \"q\""));
    assert_eq!(store.get_double("overlayFontSize"), Some(0.07));
    assert_eq!(store.get_double("tiny"), Some(1e-5));
    assert_eq!(store.get_double("whole"), Some(3.0));
    assert_eq!(store.get_int("whole"), Some(3));
    assert_eq!(store.get_double("nan"), None);
    assert_eq!(store.get_int("speakerCount"), Some(3));
    assert_eq!(store.get_int("negative"), Some(-42));
    assert_eq!(store.get_bool("offlineMode"), Some(false));
    assert_eq!(store.get_string("emoji").as_deref(), Some("😀 ğüşıöç İ"));
    let keys: Vec<String> = store.snapshot().keys().cloned().collect();
    assert_eq!(keys, ["uiLanguage", "explorerLastFolder", "overlayFontSize", "tiny", "whole", "speakerCount", "negative", "offlineMode", "emoji"]);
    drop(store);

    // Rust writes (on top of the C# file: unknown keys kept), C# reads back the same typed values.
    let store = JsonFileSettingsStore::open(cs_path.clone(), Duration::from_millis(10), None);
    store.set_string("appTheme", "dark");
    store.set_double("overlayBackgroundOpacity", 0.35);
    store.set_double("whole", 4.0);
    store.set_int("videoCacheMaxSizeGb", 500);
    store.set_bool("autoPlayVideo", false);
    store.set_string("emoji", "\u{2028}<script>&\"'\\");
    store.remove("tiny");
    store.flush();
    let rust_rows: Vec<Vec<Value>> = store.snapshot().keys().map(|k| normalize_double(typed_reads(store.as_ref(), k))).collect();
    drop(store);
    let cs_rows: Vec<Vec<Value>> = csharp(temp.path(), &["settings-read", cs_path.to_str().unwrap()]).into_iter().map(normalize_double).collect();
    assert_eq!(rust_rows, cs_rows);
}

/// Undo context: four cells; every change registers its inverse under the swapped key (`A` ↔ `A~`), so the test
/// also sees that a group keeps its original name while its actions register under another one.
type Cells = [i64; 4];

fn swap(key: &str) -> String {
    key.strip_suffix('~').map(str::to_owned).unwrap_or_else(|| format!("{key}~"))
}

fn set_cell(cells: &mut Cells, stack: &mut sa_core::undo::UndoStack<Cells>, i: usize, v: i64, key: String) {
    let old = cells[i];
    cells[i] = v;
    let next = swap(&key);
    stack.register(&key, Box::new(move |c: &mut Cells, s: &mut sa_core::undo::UndoStack<Cells>| set_cell(c, s, i, old, next)));
}

#[test]
fn undo_stack_replays_like_csharp() {
    if !enabled() {
        return;
    }
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let temp = tempfile::tempdir().unwrap();
    let mut rng = Lcg(2026);
    let mut script = Vec::new();
    for _ in 0..4000 {
        let op = match rng.next() % 16 {
            0..=5 => format!("set {} {} {}", rng.next() % 4, rng.next() % 100, ["A", "B", "C"][(rng.next() % 3) as usize]),
            6 | 7 => "begin".to_owned(),
            8 | 9 => "end".to_owned(),
            10..=12 => "undo".to_owned(),
            13 | 14 => "redo".to_owned(),
            _ => if rng.next() % 4 == 0 { "clear".to_owned() } else { "undo".to_owned() },
        };
        script.push(op);
    }
    let input = temp.path().join("undo.txt");
    write_lines(&input, &script);
    let lines = csharp(temp.path(), &["undo", input.to_str().unwrap()]);
    assert_eq!(lines.len(), script.len());

    let mut stack = sa_core::undo::UndoStack::<Cells>::new();
    let mut cells: Cells = [0; 4];
    let changed = Arc::new(AtomicUsize::new(0));
    let counter = changed.clone();
    let _sub = stack.subscribe(move |_| {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    let mut open_scopes = 0usize;
    for (op, line) in script.iter().zip(lines) {
        let parts: Vec<&str> = op.split(' ').collect();
        match parts[0] {
            "set" => set_cell(&mut cells, &mut stack, parts[1].parse().unwrap(), parts[2].parse().unwrap(), parts[3].to_owned()),
            "begin" => {
                stack.begin_group();
                open_scopes += 1;
            }
            "end" => {
                if open_scopes > 0 {
                    open_scopes -= 1;
                    stack.end_group();
                }
            }
            "undo" => stack.undo(&mut cells),
            "redo" => stack.redo(&mut cells),
            "clear" => stack.clear(),
            other => panic!("{other}"),
        }
        let rust = serde_json::json!([
            op,
            cells,
            stack.can_undo(),
            stack.can_redo(),
            stack.undo_action_key(),
            stack.redo_action_key(),
            stack.is_undoing(),
            stack.is_redoing(),
            changed.load(Ordering::Relaxed)
        ]);
        assert_eq!(rust, Value::Array(line), "after {op}");
    }
}

#[test]
fn startup_scrub_and_line_format_match_csharp() {
    if !enabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let seed = |dir: &Path| {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SubtitleAdder-diagnostics.log"),
            "2025-01-01T00:00:00.0000000+03:00 [T1] [open] C:\\Users\\Özcan\\film.mp4\r\nclean line\r\n'\\\\nas\\share\\x.mkv' done\n",
        )
        .unwrap();
        std::fs::write(dir.join("SubtitleAdder-diagnostics.log.old"), [&[0xEF, 0xBB, 0xBF][..], "old D:/a.srt\nno path\n".as_bytes()].concat())
            .unwrap();
        std::fs::write(dir.join("SubtitleAdder-crash.log"), "crash at file:///C:/x.mp4\n").unwrap();
        std::fs::write(dir.join("SubtitleAdder-player.log"), "C:\\v.mp4 failed\n").unwrap();
        std::fs::write(dir.join("unrelated.log"), "C:\\keep\n").unwrap();
    };
    let cs_dir = temp.path().join("cs");
    let rust_dir = temp.path().join("rust");
    seed(&cs_dir);
    seed(&rust_dir);
    csharp(&cs_dir, &["scrub"]);

    let log = DiagnosticLog::new(rust_dir.join("SubtitleAdder-diagnostics.log"), sa_core::diagnostics::DiagnosticFormat::Windows, Box::new(Vec::new));
    log.wait_for_startup_scrub();
    use sa_core::diagnostics::DiagnosticSink;
    log.write("t", "x C:\\a\\b.mp4");
    log.flush();
    drop(log);

    let line_shape = regex::Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{7}[+-]\d\d:\d\d \[T\d+\] \[t\] x <path>\r\n$").unwrap();
    for name in ["SubtitleAdder-diagnostics.log", "SubtitleAdder-diagnostics.log.old", "SubtitleAdder-crash.log", "unrelated.log"] {
        let read = |dir: &Path| std::fs::read(dir.join(name)).unwrap();
        let (mut cs, mut rust) = (read(&cs_dir), read(&rust_dir));
        if name == "SubtitleAdder-diagnostics.log" {
            // The appended line differs in its timestamp and thread number: compare its shape, then the rest.
            let split = |bytes: &mut Vec<u8>| -> String {
                let text = String::from_utf8(bytes.clone()).unwrap();
                let cut = text[..text.len() - 2].rfind('\n').map_or(0, |i| i + 1);
                bytes.truncate(cut);
                text[cut..].to_owned()
            };
            let (cs_line, rust_line) = (split(&mut cs), split(&mut rust));
            assert!(line_shape.is_match(&cs_line), "C# line {cs_line:?}");
            assert!(line_shape.is_match(&rust_line), "Rust line {rust_line:?}");
        }
        assert_eq!(String::from_utf8_lossy(&rust), String::from_utf8_lossy(&cs), "{name}");
    }
    assert!(!cs_dir.join("SubtitleAdder-player.log").exists());
    assert!(!rust_dir.join("SubtitleAdder-player.log").exists());
}
