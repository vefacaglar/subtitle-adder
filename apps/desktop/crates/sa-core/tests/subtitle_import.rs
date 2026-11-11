//! Subtitle import tests: ports of SubtitleParserTests.swift, SubtitleFolderRulesTests.swift (listing rules),
//! ViewModelFlowTests (parser part), TranslationReviewTests (import part) and the win-core §12 recommended tests.

use std::path::Path;

use sa_core::subtitle_import::{
    ImportFlavor, decode_with, find_companion_video_listing, find_companion_video_windows, find_sidecar_subtitle, is_subtitle_file, parse,
    parse_file_with, parse_with, round3, subtitle_badge_extension, SubtitleBadgeLookup,
};

const W: ImportFlavor = ImportFlavor::Windows;
const M: ImportFlavor = ImportFlavor::Mac;

fn texts(content: &str, ext: Option<&str>) -> Vec<String> {
    parse(content, ext).into_iter().map(|s| s.text).collect()
}

fn touch(dir: &Path, names: &[&str]) {
    for n in names {
        std::fs::write(dir.join(n), b"").unwrap();
    }
}

fn file_name(p: Option<std::path::PathBuf>) -> Option<String> {
    p.map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
}

// ---------------------------------------------------------------- SRT / VTT

#[test]
fn parses_srt() {
    let srt = "1\n00:00:01,000 --> 00:00:02,500\n<i>Merhaba</i> dünya.\nİkinci satır.\n\n2\n00:00:03,000 --> 00:00:04,000\n- Evet.\n";
    let lines = parse(srt, Some("srt"));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].id, 1);
    assert_eq!(lines[0].start, 1.0);
    assert_eq!(lines[0].end, 2.5);
    assert_eq!(lines[0].text, "Merhaba dünya. İkinci satır.");
    assert_eq!(lines[1].id, 2);
    assert_eq!(lines[1].text, "- Evet.");
    assert!(lines[0].words.is_empty() && lines[0].speaker.is_none() && !lines[0].needs_review);
}

#[test]
fn srt_index_is_optional_and_crlf_works() {
    let srt = "00:00:01,000 --> 00:00:02,000\r\nA\r\n\r\n00:00:03,000 --> 00:00:04,000\r\nB\r\n";
    assert_eq!(texts(srt, Some(".SRT")), ["A", "B"]);
    assert_eq!(texts("00:00:01,000 --> 00:00:02,000\rA\r\r00:00:03,000 --> 00:00:04,000\rB", Some(".srt")), ["A", "B"]);
}

#[test]
fn parses_vtt_with_cue_settings_notes_and_short_timestamps() {
    let vtt = "WEBVTT\n\nNOTE this is a comment\nstill the comment\n\nSTYLE\n::cue { color: red }\n\nintro\n00:01.000 --> 00:02.500 align:start\nFirst VTT cue\n\n01:00:00.250 --> 01:00:01.000\n<v Roger>Late cue</v>";
    let lines = parse(vtt, Some("vtt"));
    assert_eq!(lines.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["First VTT cue", "Late cue"]);
    assert_eq!(lines[0].start, 1.0);
    assert_eq!(lines[0].end, 2.5);
    assert_eq!(lines[1].start, 3600.25);
    // ViewModelFlowTests: the minimal VTT cue.
    let one = parse("WEBVTT\n\n00:01.000 --> 00:02.500 align:start\nFirst VTT cue\n", Some(".vtt"));
    assert_eq!((one[0].start, one[0].end, one[0].text.as_str()), (1.0, 2.5, "First VTT cue"));
}

#[test]
fn end_before_start_is_dropped_and_zero_length_gets_minimum_duration() {
    let srt = "00:00:05,000 --> 00:00:04,000\nBackwards\n\n00:00:06,000 --> 00:00:06,000\nInstant\n";
    let lines = parse(srt, Some("srt"));
    assert_eq!(lines.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Instant"]);
    assert_eq!(lines[0].end, 6.05);
    assert_eq!(lines[0].id, 1);
}

#[test]
fn empty_and_garbage_give_no_lines() {
    assert!(parse("", Some("srt")).is_empty());
    assert!(parse("  \n\n", Some("vtt")).is_empty());
    assert!(parse("just text\nno timing", Some("srt")).is_empty());
    assert!(parse("00:00:01,000 --> 00:00:02,000\n<i></i>\n", Some("srt")).is_empty(), "markup only");
}

#[test]
fn markup_is_cleaned_and_entities_kept() {
    assert_eq!(texts("00:00:01,000 --> 00:00:02,000\n{\\an8}<b>Hi</b>   &amp;\tthere\n", Some("srt")), ["Hi &amp; there"]);
}

#[test]
fn timestamps_round_with_bankers_rounding() {
    // 1.0625 * 1000 = 1062.5 and 1.1875 * 1000 = 1187.5 exactly: ties go to even.
    assert_eq!(round3(1.0625), 1.062);
    assert_eq!(round3(1.1875), 1.188);
    assert_eq!(round3(2.0004), 2.0);
    let lines = parse("00:00:01,12 --> 00:00:01,5\nx\n", Some("srt"));
    assert_eq!((lines[0].start, lines[0].end), (1.12, 1.5));
}

// ---------------------------------------------------------------- ASS / SSA

#[test]
fn parses_ass_with_format_line_overrides_and_commas_in_text() {
    let ass = "[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:05.00,0:00:07.50,Default,,0,0,0,,{\\i1}Styled{\\i0}\\NASS line, with comma\nComment: 0,0:00:08.00,0:00:09.00,Default,,0,0,0,,ignored\nDialogue: 0,0:00:10.00,0:00:11.00,Default,,0,0,0,,a\\hb\\nc";
    let lines = parse(ass, Some("ass"));
    assert_eq!(lines.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Styled ASS line, with comma", "a b c"]);
    assert_eq!(lines[0].start, 5.0);
    assert_eq!(lines[0].end, 7.5);
    // win-core §8.7 example.
    let one = parse("[Events]\nDialogue: 0,0:00:05.00,0:00:07.50,Default,,0,0,0,,{\\i1}Styled{\\i0}\\NASS line", Some(".ass"));
    assert_eq!((one[0].start, one[0].end, one[0].text.as_str()), (5.0, 7.5, "Styled ASS line"));
}

#[test]
fn ass_honors_custom_field_order() {
    let lines = parse("[Events]\nFormat: Start, End, Text\nDialogue: 0:00:01.00,0:00:02.00,Hello\n", Some("ssa"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].start, 1.0);
    assert_eq!(lines[0].text, "Hello");
}

#[test]
fn ass_outside_events_and_short_dialogues_are_skipped() {
    let ass = "[V4+ Styles]\nDialogue: 0,0:00:01.00,0:00:02.00,S,,0,0,0,,styles section\n[events]\nDialogue: 0,0:00:01.00\nDialogue: 0,0:00:03.00,0:00:04.00,S,,0,0,0,,kept\n";
    assert_eq!(texts(ass, Some("ass")), ["kept"]);
}

#[test]
fn content_is_sniffed_without_extension() {
    assert_eq!(parse("[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,S,,0,0,0,,Hi", None).len(), 1);
    assert_eq!(parse("00:00:01,000 --> 00:00:02,000\nHi", None).len(), 1);
    let json = r#"[{"id": 1, "start": 0.0, "end": 1.0, "text": "Selam", "words": []}]"#;
    assert_eq!(texts(json, None), ["Selam"]);
    // `[Events]` anywhere routes to the ASS parser, even in a .json or .srt file.
    assert_eq!(texts("[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,S,,0,0,0,,X", Some(".json")), ["X"]);
    assert_eq!(texts("\u{FEFF}  [EVENTS]\nDialogue: 0,0:00:01.00,0:00:02.00,S,,0,0,0,,Y", Some(".srt")), ["Y"]);
}

// ---------------------------------------------------------------- JSON

#[test]
fn parses_engine_json_with_speaker_and_words() {
    let json = r#"[{"id": 1, "start": 0.5, "end": 1.5, "text": "Merhaba.", "speaker": 1,
      "words": [{"start": 0.5, "end": 1.5, "text": "Merhaba."}]}]"#;
    let lines = parse(json, Some("json"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].speaker, Some(1));
    assert_eq!(lines[0].words.len(), 1);
    assert!(parse("{broken", Some("json")).is_empty());
}

#[test]
fn json_with_one_bad_element_gives_nothing_but_a_bad_flag_is_lenient() {
    let bad = r#"[{"id": 1, "start": 0, "end": 1, "text": "A"}, {"id": 2, "start": 0, "end": 1, "text": "B", "speaker": "x"}]"#;
    assert!(parse(bad, Some(".json")).is_empty());
    assert!(parse(r#"[{"id": 1, "start": 0, "end": 1}]"#, Some(".json")).is_empty(), "text is required");
    assert!(parse(r#"{"id": 1}"#, Some(".json")).is_empty(), "non-array root");
    let flag = r#"[{"id": 1, "start": 0, "end": 1, "text": "A", "needs_review": "x", "extra": [1]}]"#;
    let lines = parse(flag, Some("json"));
    assert_eq!(lines.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["A"]);
    assert!(!lines[0].needs_review);
}

#[test]
fn json_ids_are_kept_on_windows_and_renumbered_on_mac() {
    let json = r#"[{"id": 7, "start": 2, "end": 1, "text": "A", "needs_review": true},
       {"id": 0, "start": 1, "end": 2, "text": "B"},
       {"id": 0, "start": 2, "end": 3, "text": "C"}]"#;
    let win = parse_with(json, Some(".json"), W);
    assert_eq!(win.iter().map(|s| s.id).collect::<Vec<_>>(), [7, 0, 0]);
    assert_eq!((win[0].start, win[0].end), (2.0, 1.0), "times kept exactly, end < start kept");
    let mac = parse_with(json, Some("json"), M);
    assert_eq!(mac.iter().map(|s| s.id).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(mac.iter().map(|s| s.needs_review).collect::<Vec<_>>(), [true, false, false]);
}

// ---------------------------------------------------------------- decoding

const SAMPLE: &str = "00:00:01,000 --> 00:00:02,000\nÇağrı şığ İstanbul\n";

fn cp1254(s: &str) -> Vec<u8> {
    let (bytes, _, unmappable) = encoding_rs::WINDOWS_1254.encode(s);
    assert!(!unmappable);
    bytes.into_owned()
}

#[test]
fn decodes_bom_encodings() {
    for flavor in [W, M] {
        let mut utf8 = vec![0xEF, 0xBB, 0xBF];
        utf8.extend_from_slice(SAMPLE.as_bytes());
        assert_eq!(parse_with(&decode_with(&utf8, flavor), Some("srt"), flavor)[0].text, "Çağrı şığ İstanbul");
        let le: Vec<u8> = [0xFF, 0xFE].into_iter().chain(SAMPLE.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(decode_with(&le, flavor), SAMPLE);
        let be: Vec<u8> = [0xFE, 0xFF].into_iter().chain(SAMPLE.encode_utf16().flat_map(u16::to_be_bytes)).collect();
        assert_eq!(decode_with(&be, flavor), SAMPLE);
    }
}

#[test]
fn invalid_utf16_is_lossy_on_windows_and_empty_on_mac() {
    let broken = [0xFF, 0xFE, 0x00, 0xD8, b'a', 0x00]; // unpaired high surrogate, then "a"
    assert_eq!(decode_with(&broken, W), "\u{FFFD}a");
    assert_eq!(decode_with(&broken, M), "");
}

#[test]
fn falls_back_to_windows_1254_when_not_utf8() {
    let data = cp1254(SAMPLE);
    assert!(std::str::from_utf8(&data).is_err());
    for flavor in [W, M] {
        assert_eq!(decode_with(&data, flavor), SAMPLE);
        assert_eq!(decode_with(SAMPLE.as_bytes(), flavor), SAMPLE);
        assert_eq!(decode_with(&[], flavor), "");
    }
    assert_eq!(decode_with(&[0xFE], W), "ş");
}

#[test]
fn parse_file_reads_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.srt");
    std::fs::write(&path, cp1254(SAMPLE)).unwrap();
    for flavor in [W, M] {
        let lines = parse_file_with(&path, flavor).unwrap();
        assert_eq!(lines.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Çağrı şığ İstanbul"]);
    }
    assert!(parse_file_with(&dir.path().join("missing.srt"), W).is_err());
}

// ---------------------------------------------------------------- files

#[test]
fn recognizes_subtitle_extensions() {
    for name in ["a.srt", "a.VTT", "a.ass", "a.ssa", "a.json"] {
        assert!(is_subtitle_file(&Path::new("/x").join(name)), "{name}");
    }
    assert!(!is_subtitle_file(Path::new("/x/a.mp4")));
    assert!(!is_subtitle_file(Path::new("/x/a")));
    assert!(!is_subtitle_file(Path::new("/x/a.")));
    assert!(!is_subtitle_file(Path::new("  ")));
}

#[test]
fn finds_sidecar_in_extension_order_and_ignores_language_suffixes() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("Film.mp4");
    touch(dir.path(), &["Film.mp4"]);
    assert_eq!(find_sidecar_subtitle(&video), None);
    // An exported translation (language-suffixed) never counts as the source subtitle.
    touch(dir.path(), &["Film.es.srt", "Other.srt"]);
    assert_eq!(find_sidecar_subtitle(&video), None);
    touch(dir.path(), &["Film.json"]);
    assert_eq!(file_name(find_sidecar_subtitle(&video)).as_deref(), Some("Film.json"));
    touch(dir.path(), &["Film.ass"]);
    assert_eq!(file_name(find_sidecar_subtitle(&video)).as_deref(), Some("Film.ass"));
    touch(dir.path(), &["Film.srt"]);
    assert_eq!(file_name(find_sidecar_subtitle(&video)).as_deref(), Some("Film.srt"));
    assert_eq!(find_sidecar_subtitle(Path::new("Film.mp4")), None, "a bare file name has no folder");
}

#[test]
fn find_companion_video_matches_exact_and_language_suffixed_subtitles() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), &["Episode01.mkv", "Episode01.srt", "Episode01.tr.srt", "Other.srt"]);
    for find in [find_companion_video_windows, find_companion_video_listing] {
        assert_eq!(file_name(find(&dir.path().join("Episode01.srt"))).as_deref(), Some("Episode01.mkv"));
        assert_eq!(file_name(find(&dir.path().join("Episode01.tr.srt"))).as_deref(), Some("Episode01.mkv"));
        assert_eq!(find(&dir.path().join("Other.srt")), None);
    }
}

#[test]
fn companion_strips_one_language_suffix() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), &["Episode01.mp4", "Show.S01E02.mkv"]);
    for find in [find_companion_video_windows, find_companion_video_listing] {
        let found = |name: &str| file_name(find(&dir.path().join(name)));
        assert_eq!(found("Episode01.tr.srt").as_deref(), Some("Episode01.mp4")); // ".tr" is 3 characters
        assert_eq!(found("Episode01.zh-CN.srt").as_deref(), Some("Episode01.mp4")); // ".zh-CN" is 6
        assert_eq!(found("Episode01.forced.srt"), None); // ".forced" is 7: not a language tag
        assert_eq!(found("Episode01.t.srt"), None); // ".t" is 2
        assert_eq!(found("Show.S01E02.srt").as_deref(), Some("Show.S01E02.mkv"));
        assert_eq!(found("Episode01.tr.forced.srt"), None);
        assert_eq!(found(".tr.srt"), None);
    }
}

#[test]
fn companion_prefers_exact_stem_and_tries_extensions_in_order() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), &["Film.mp4", "Film.tr.mp4", "Talk.mp3", "Talk.wav"]);
    for find in [find_companion_video_windows, find_companion_video_listing] {
        assert_eq!(file_name(find(&dir.path().join("Film.tr.srt"))).as_deref(), Some("Film.tr.mp4"));
        assert_eq!(file_name(find(&dir.path().join("Talk.srt"))).as_deref(), Some("Talk.mp3"));
    }
    touch(dir.path(), &["Talk.mkv"]);
    for find in [find_companion_video_windows, find_companion_video_listing] {
        assert_eq!(file_name(find(&dir.path().join("Talk.srt"))).as_deref(), Some("Talk.mkv"));
    }
}

/// The widened list (`sa_core::media_types`): new extensions are found, and come after the former ones of the same kind
/// (video before audio, as before).
#[test]
fn companion_finds_the_added_extensions_after_the_former_ones() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), &["Dvd.vob", "Dvd.wma", "Old.vob", "Old.flv", "Song.mka", "Song.opus", "Tape.ac3"]);
    for find in [find_companion_video_windows, find_companion_video_listing] {
        let found = |name: &str| file_name(find(&dir.path().join(name)));
        assert_eq!(found("Old.srt").as_deref(), Some("Old.flv"), "former video before an added one");
        assert_eq!(found("Dvd.tr.srt").as_deref(), Some("Dvd.vob"), "videos (added ones too) before audio");
        assert_eq!(found("Song.srt").as_deref(), Some("Song.opus"), "former audio before an added one");
        assert_eq!(found("Tape.srt").as_deref(), Some("Tape.ac3"));
    }
}

#[test]
fn listing_companion_is_case_insensitive_keeps_the_real_name_and_normalizes() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), &["EPISODE01.MP4"]);
    let found = find_companion_video_listing(&dir.path().join("episode01.srt")).unwrap();
    assert_eq!(found.file_name().unwrap(), "EPISODE01.MP4");
    assert!(found.exists());
    let nfd = "Cafe\u{301}";
    touch(dir.path(), &[&format!("{nfd}.mov")]);
    assert!(find_companion_video_listing(&dir.path().join("Café.srt")).is_some());
    assert_eq!(find_companion_video_listing(&dir.path().join("missing").join("a.srt")), None);
}

// ---------------------------------------------------------------- macOS badge rules

#[test]
fn badge_follows_extension_order_not_listing_order() {
    let names = ["Film.json", "Film.vtt"];
    assert_eq!(subtitle_badge_extension("Film", &names).as_deref(), Some("vtt"));
    assert_eq!(subtitle_badge_extension("Film", &["Film.json", "Film.vtt", "Film.srt"]).as_deref(), Some("srt"));
    assert_eq!(subtitle_badge_extension("Film", &["Film.json", "Film.ssa"]).as_deref(), Some("ssa"));
    assert_eq!(subtitle_badge_extension("Film", &["Film.json"]).as_deref(), Some("json"));
}

#[test]
fn badge_accepts_language_tagged_files_and_ignores_others() {
    assert_eq!(subtitle_badge_extension("Film", &["Film.tr.srt"]).as_deref(), Some("srt"));
    assert_eq!(subtitle_badge_extension("Film", &["Film.tr.forced.vtt"]).as_deref(), Some("vtt"));
    assert_eq!(subtitle_badge_extension("Film", &["Film.vtt", "Film.es.srt"]).as_deref(), Some("srt"));
    assert_eq!(subtitle_badge_extension("Film", &["Filmmaker.srt", "Film.mp4", "Other.srt"]), None);
    assert_eq!(subtitle_badge_extension("", &["a.srt", ".srt", "..srt"]), None);
    assert_eq!(subtitle_badge_extension::<&str>("Film", &[]), None);
    assert_eq!(subtitle_badge_extension("Film", &["Film..srt"]), None);
}

#[test]
fn badge_is_case_insensitive_normalized_and_answers_many_stems() {
    assert_eq!(subtitle_badge_extension("FILM", &["film.SRT"]).as_deref(), Some("srt"));
    assert_eq!(subtitle_badge_extension("film", &["FILM.Tr.Ass"]).as_deref(), Some("ass"));
    let nfc = "Caf\u{e9}";
    let nfd = "Cafe\u{301}";
    assert_eq!(subtitle_badge_extension(nfc, &[format!("{nfd}.srt")]).as_deref(), Some("srt"));
    assert_eq!(subtitle_badge_extension(nfd, &[format!("{nfc}.tr.vtt")]).as_deref(), Some("vtt"));
    let lookup = SubtitleBadgeLookup::new(&["A.srt", "B.vtt", "notes.txt"]);
    assert_eq!(lookup.badge_extension("A").as_deref(), Some("srt"));
    assert_eq!(lookup.badge_extension("B").as_deref(), Some("vtt"));
    assert_eq!(lookup.badge_extension("C"), None);
    assert_eq!(lookup.badge_extension("notes"), None);
}

// ---------------------------------------------------------------- per-platform path rules (parity review)

#[test]
fn windows_paths_follow_dotnet_file_name_rules() {
    use sa_core::subtitle_import::{find_sidecar_subtitle_with, is_subtitle_file_with};
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), &["Film.srt", ".srt.srt"]);
    std::fs::create_dir(dir.path().join("Clip.vtt")).unwrap();
    // A trailing separator means "no file name" in .NET.
    let trailing = format!("{}\\Film.srt\\", dir.path().display());
    assert!(!is_subtitle_file_with(Path::new(&trailing), W));
    assert!(is_subtitle_file_with(Path::new(&trailing), M), "URL.lastPathComponent ignores the trailing slash");
    assert!(is_subtitle_file_with(Path::new("C:x.SRT"), W), "past the drive prefix");
    assert!(is_subtitle_file_with(Path::new(".srt"), W));
    assert!(!is_subtitle_file_with(Path::new(".srt"), M), "a leading dot is no extension on macOS");
    // Sidecars: Windows wants a file, macOS accepts anything that exists.
    assert_eq!(find_sidecar_subtitle_with(&dir.path().join("Clip.mp4"), W), None);
    assert_eq!(find_sidecar_subtitle_with(&dir.path().join("Clip.mp4"), M), Some(dir.path().join("Clip.vtt")));
    // `.srt` alone has an empty .NET stem; on macOS it is the stem itself.
    assert_eq!(find_sidecar_subtitle_with(&dir.path().join(".srt"), W), None);
    assert_eq!(find_sidecar_subtitle_with(&dir.path().join(".srt"), M), Some(dir.path().join(".srt.srt")));
}

#[test]
fn windows_json_import_follows_the_csharp_reader() {
    // Repeated keys: the last wins; ids beyond `int` fail the file; words may omit fields.
    let dup = parse_with(r#"[{"id":1,"id":5,"start":0,"end":1,"text":"a"}]"#, Some(".json"), W);
    assert_eq!(dup.len(), 1);
    assert_eq!(dup[0].id, 5);
    assert!(parse_with(r#"[{"id":3000000000,"start":0,"end":1,"text":"a"}]"#, Some(".json"), W).is_empty());
    assert_eq!(parse_with(r#"[{"id":3000000000,"start":0,"end":1,"text":"a"}]"#, Some(".json"), M).len(), 1);
    let words = parse_with(r#"[{"id":1,"start":0,"end":1,"text":"a","words":[{"start":0.5}]}]"#, Some(".json"), W);
    assert_eq!(words[0].words.len(), 1);
    assert_eq!((words[0].words[0].start, words[0].words[0].end, words[0].words[0].text.as_str()), (0.5, 0.0, ""));
    assert!(parse_with(r#"[{"id":1,"start":0,"end":1,"text":"a","words":[{"start":0.5}]}]"#, Some(".json"), M).is_empty());
}
