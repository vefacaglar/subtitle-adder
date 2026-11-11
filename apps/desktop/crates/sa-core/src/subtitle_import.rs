//! Subtitle file import (win-core §8; mac-enginekit §12 differences resolved in favor of the shared tests,
//! UNIFIED.md §3.8). Subtitle *writing* stays in the engine (`render`).
//!
//! The parser is shared. The few places where the two apps behave differently are an [`ImportFlavor`]; the
//! un-suffixed functions use the flavor of the platform this binary is built for:
//!
//! | | Windows (`SubtitleParser.cs`) | macOS (`SubtitleParser.swift`) |
//! |---|---|---|
//! | JSON ids | kept exactly | renumbered 1…n |
//! | UTF-16 after a BOM | lossy (U+FFFD) | strict, `""` on failure |
//! | companion video | `File.Exists` per candidate | folder listing, NFC/case fold, real on-disk name |
//!
//! The macOS folder-listing rules (`SubtitleBadgeLookup`, the listing-based companion search) are available on every
//! platform so they can be tested here; only the macOS build uses them.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use crate::engine_types::Sentence;
use crate::text::trim_dotnet;

/// Open-picker filter and recognition order.
pub const EXTENSIONS: [&str; 5] = [".srt", ".vtt", ".ass", ".ssa", ".json"];

/// Windows companion video search order (without dots; C# `SubtitleParser.VideoExtensions` first, the additions of
/// [`crate::media_types`] after them).
pub const COMPANION_VIDEO_EXTENSIONS: &[&str] = &crate::media_types::COMPANION_PROBE_ORDER;

/// macOS `MediaFileTypes.media` order (video then audio; mac-enginekit §11), used by the listing-based companion
/// search.
pub const MAC_MEDIA_EXTENSIONS: &[&str] = &crate::media_types::MEDIA_EXTENSIONS;

/// Which app's import behavior to follow (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImportFlavor {
    Windows,
    Mac,
}

impl ImportFlavor {
    /// The flavor of the platform this binary is built for.
    pub const fn native() -> Self {
        if cfg!(target_os = "macos") { Self::Mac } else { Self::Windows }
    }
}

// ------------------------------------------------------------------------------------------------ path helpers

/// Windows: .NET `Path.GetFileName` (the text after the last `\` or `/`, past a `X:` drive prefix; `""` for a
/// trailing separator, so `C:\dir.srt\` is no subtitle file). macOS: `URL.lastPathComponent` (a trailing `/` is
/// ignored), which is what `Path::file_name` returns.
fn file_name_with(path: &Path, flavor: ImportFlavor) -> String {
    match flavor {
        ImportFlavor::Windows => {
            let text = path.as_os_str().to_string_lossy();
            let bytes = text.as_bytes();
            let root = if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' { 2 } else { 0 };
            let start = text.rfind(['\\', '/']).map_or(0, |i| i + 1).max(root);
            text[start..].to_owned()
        }
        ImportFlavor::Mac => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}

/// .NET `Path.GetExtension` of a file name: from the last `.` (incl. the dot); `"a."` and `"a"` → `""`.
fn dotnet_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => &name[i..],
        _ => "",
    }
}

/// .NET `Path.GetFileNameWithoutExtension` of a file name: everything before the last `.` (`".srt"` → `""`).
fn dotnet_stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) => &name[..i],
        None => name,
    }
}

fn is_blank(path: &Path) -> bool {
    path.as_os_str().to_string_lossy().trim().is_empty()
}

/// The parent folder, `None` when there is none (a bare file name).
fn directory(path: &Path) -> Option<&Path> {
    path.parent().filter(|d| !d.as_os_str().is_empty())
}

/// Non-blank and extension in [`EXTENSIONS`] (ASCII case-insensitive), native flavor.
pub fn is_subtitle_file(path: &Path) -> bool {
    is_subtitle_file_with(path, ImportFlavor::native())
}

/// [`is_subtitle_file`] with an explicit flavor. Windows: .NET `Path.GetExtension` (`.srt` alone counts). macOS:
/// `URL.pathExtension` rules (a leading dot starts no extension).
pub fn is_subtitle_file_with(path: &Path, flavor: ImportFlavor) -> bool {
    if is_blank(path) {
        return false;
    }
    let name = file_name_with(path, flavor);
    match flavor {
        ImportFlavor::Windows => {
            let ext = dotnet_extension(&name);
            EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext))
        }
        ImportFlavor::Mac => mac_extension(&name).is_some_and(|ext| EXTENSIONS.iter().any(|e| e[1..] == ext.to_lowercase())),
    }
}

/// First existing `dir/stem + ext` over [`EXTENSIONS`], native flavor.
pub fn find_sidecar_subtitle(video_path: &Path) -> Option<PathBuf> {
    find_sidecar_subtitle_with(video_path, ImportFlavor::native())
}

/// [`find_sidecar_subtitle`] with an explicit flavor. Windows: `File.Exists` (files only) after the .NET stem.
/// macOS: `FileManager.fileExists(atPath:)` (a folder counts too) after `deletingPathExtension`.
pub fn find_sidecar_subtitle_with(video_path: &Path, flavor: ImportFlavor) -> Option<PathBuf> {
    if is_blank(video_path) {
        return None;
    }
    let dir = directory(video_path)?;
    let name = file_name_with(video_path, flavor);
    let stem = match flavor {
        ImportFlavor::Windows => dotnet_stem(&name),
        ImportFlavor::Mac => mac_stem(&name),
    };
    if stem.is_empty() {
        return None;
    }
    let exists = |p: &Path| match flavor {
        ImportFlavor::Windows => p.is_file(),
        ImportFlavor::Mac => p.exists(),
    };
    EXTENSIONS.iter().map(|ext| dir.join(format!("{stem}{ext}"))).find(|c| exists(c))
}

/// Exact-stem companion video, else the language-suffixed one (`Episode01.tr.srt` → `Episode01.mkv`), following
/// the native flavor.
pub fn find_companion_video(subtitle_path: &Path) -> Option<PathBuf> {
    match ImportFlavor::native() {
        ImportFlavor::Windows => find_companion_video_windows(subtitle_path),
        ImportFlavor::Mac => find_companion_video_listing(subtitle_path),
    }
}

/// Windows (win-core §8.2): `File.Exists` over [`COMPANION_VIDEO_EXTENSIONS`] for the stem, then for the stem
/// without a 3–6 character language suffix.
pub fn find_companion_video_windows(subtitle_path: &Path) -> Option<PathBuf> {
    if is_blank(subtitle_path) {
        return None;
    }
    let dir = directory(subtitle_path)?;
    let name = file_name_with(subtitle_path, ImportFlavor::Windows);
    let stem = dotnet_stem(&name);
    if stem.is_empty() {
        return None;
    }
    let first = |base: &str| COMPANION_VIDEO_EXTENSIONS.iter().map(|ext| dir.join(format!("{base}.{ext}"))).find(|c| c.is_file());
    if let Some(found) = first(stem) {
        return Some(found);
    }
    let stripped = dotnet_stem(stem);
    let suffix_len = dotnet_extension(stem).encode_utf16().count();
    if !stripped.is_empty() && stripped != stem && (3..=6).contains(&suffix_len) {
        return first(stripped);
    }
    None
}

/// macOS (mac-enginekit §12.2): lists the folder once, sorts the names, matches `fold(candidate.ext)` over the
/// media extensions in [`MAC_MEDIA_EXTENSIONS`] order and returns the real on-disk name.
pub fn find_companion_video_listing(subtitle_path: &Path) -> Option<PathBuf> {
    let dir = directory(subtitle_path)?;
    let name = file_name_with(subtitle_path, ImportFlavor::Mac);
    let stem = mac_stem(&name);
    if stem.is_empty() {
        return None;
    }
    let mut names: Vec<String> =
        std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let mut media: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for n in names {
        if mac_extension(&n).is_some_and(|e| MAC_MEDIA_EXTENSIONS.contains(&e.to_lowercase().as_str())) {
            media.entry(fold(&n)).or_insert(n);
        }
    }
    let mut candidates = vec![stem.to_owned()];
    if let Some(dot) = stem.rfind('.')
        && dot > 0
        && (3..=6).contains(&stem[dot..].chars().count())
    {
        candidates.push(stem[..dot].to_owned());
    }
    for candidate in candidates {
        for ext in MAC_MEDIA_EXTENSIONS.iter() {
            if let Some(real) = media.get(&fold(&format!("{candidate}.{ext}"))) {
                return Some(dir.join(real));
            }
        }
    }
    None
}

/// Swift `deletingPathExtension().lastPathComponent`: the name without its last extension. A name has no extension
/// when its last dot is its first character (`.srt`) or its last one (`Film.`), so those stay whole.
fn mac_stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => &name[..i],
        _ => name,
    }
}

fn mac_extension(name: &str) -> Option<&str> {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => Some(&name[i + 1..]),
        _ => None,
    }
}

/// macOS folder-listing fold: `NFC(lowercase(NFC(name)))`. Swift `String.lowercased()` maps every scalar on its own
/// (Unicode full lowercase mapping, no Turkish rules and no context: a final `Σ` becomes `σ`, never `ς`), so this
/// lower-cases per `char` rather than with `str::to_lowercase`.
pub fn fold(name: &str) -> String {
    let nfc: String = name.nfc().collect();
    let lower: String = nfc.chars().flat_map(char::to_lowercase).collect();
    lower.nfc().collect()
}

/// macOS sidebar badge rule (mac-enginekit §12.2): which subtitle extension a listing holds for a video stem
/// (`Film.tr.srt` counts, extension order beats listing order).
#[derive(Debug, Clone, Default)]
pub struct SubtitleBadgeLookup {
    /// Folded names per lower-case extension (without the dot), in [`EXTENSIONS`] order.
    by_ext: Vec<(&'static str, Vec<String>)>,
}

impl SubtitleBadgeLookup {
    pub fn new<S: AsRef<str>>(file_names: &[S]) -> Self {
        let mut by_ext: Vec<(&'static str, Vec<String>)> = EXTENSIONS.iter().map(|e| (&e[1..], Vec::new())).collect();
        for name in file_names {
            let name = name.as_ref();
            let Some(dot) = name.rfind('.') else { continue };
            let ext = name[dot + 1..].to_lowercase();
            if let Some((_, list)) = by_ext.iter_mut().find(|(e, _)| *e == ext) {
                list.push(fold(name));
            }
        }
        Self { by_ext }
    }

    /// The badge extension (lower case, no dot) for a video stem, or `None`.
    pub fn badge_extension(&self, stem: &str) -> Option<String> {
        let stem = fold(stem);
        if stem.is_empty() {
            return None;
        }
        let prefix = format!("{stem}.");
        for (ext, names) in &self.by_ext {
            let cut = ext.chars().count() + 1;
            let hit = names.iter().any(|n| {
                let count = n.chars().count();
                if count < cut {
                    return false;
                }
                let base: String = n.chars().take(count - cut).collect();
                base == stem || (base.starts_with(&prefix) && base.chars().count() > prefix.chars().count())
            });
            if hit {
                return Some((*ext).to_owned());
            }
        }
        None
    }
}

/// `SubtitleBadgeLookup::new(names).badge_extension(stem)`.
pub fn subtitle_badge_extension<S: AsRef<str>>(stem: &str, names: &[S]) -> Option<String> {
    SubtitleBadgeLookup::new(names).badge_extension(stem)
}

// ------------------------------------------------------------------------------------------------ reading

/// Reads, decodes and parses a subtitle file. I/O errors propagate.
pub fn parse_file(path: &Path) -> std::io::Result<Vec<Sentence>> {
    parse_file_with(path, ImportFlavor::native())
}

/// [`parse_file`] with an explicit flavor.
pub fn parse_file_with(path: &Path, flavor: ImportFlavor) -> std::io::Result<Vec<Sentence>> {
    let bytes = std::fs::read(path)?;
    let text = decode_with(&bytes, flavor);
    let name = file_name_with(path, flavor);
    Ok(parse_with(&text, Some(dotnet_extension(&name)), flavor))
}

/// BOM → UTF-8/UTF-16 LE/BE; else strict UTF-8; else Windows-1254.
pub fn decode(bytes: &[u8]) -> String {
    decode_with(bytes, ImportFlavor::native())
}

/// [`decode`] with an explicit flavor (UTF-16 lossy on Windows, strict with `""` on failure on macOS).
pub fn decode_with(bytes: &[u8], flavor: ImportFlavor) -> String {
    let utf16 = |rest: &[u8], be: bool| match flavor {
        ImportFlavor::Windows => crate::atomic_file::decode_utf16_lossy(rest, be),
        ImportFlavor::Mac => crate::atomic_file::decode_utf16_strict(rest, be).unwrap_or_default(),
    };
    match bytes {
        [] => String::new(),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => encoding_rs::WINDOWS_1254.decode_without_bom_handling(bytes).0.into_owned(),
        },
    }
}

// ------------------------------------------------------------------------------------------------ parsing

/// Dispatches to the ASS, JSON or SRT/VTT parser (win-core §8.4–8.7), native flavor.
pub fn parse(content: &str, extension: Option<&str>) -> Vec<Sentence> {
    parse_with(content, extension, ImportFlavor::native())
}

/// [`parse`] with an explicit flavor. `extension` may carry a leading dot or not (`".SRT"`, `"srt"`).
pub fn parse_with(content: &str, extension: Option<&str>, flavor: ImportFlavor) -> Vec<Sentence> {
    if trim_dotnet(content).is_empty() {
        return Vec::new();
    }
    let trimmed = content.trim_start_matches(['\u{FEFF}', ' ', '\t', '\r', '\n']);
    let ext = trim_dotnet(extension.unwrap_or("")).trim_start_matches('.').to_lowercase();
    if ext == "ass" || ext == "ssa" || trimmed.to_ascii_lowercase().contains("[events]") {
        return parse_ass(trimmed, flavor);
    }
    if ext == "json" || (trimmed.starts_with('[') && trim_dotnet(trimmed).ends_with(']')) {
        return parse_json(trimmed, flavor);
    }
    parse_srt_or_vtt(trimmed, flavor)
}

/// `.NET Math.Round(x, 3)` (banker's rounding).
pub fn round3(x: f64) -> f64 {
    (x * 1000.0).round_ties_even() / 1000.0
}

fn parse_json(json: &str, flavor: ImportFlavor) -> Vec<Sentence> {
    match flavor {
        ImportFlavor::Windows => parse_json_windows(json).unwrap_or_default(),
        ImportFlavor::Mac => {
            let mut list = Sentence::decode_list(json).unwrap_or_default();
            for (i, s) in list.iter_mut().enumerate() {
                s.id = i as i64 + 1;
            }
            list
        }
    }
}

/// The C# `JsonSerializer.Deserialize<List<Sentence>>` rules (C# fixture verified): a repeated key keeps its last
/// value; `id` and `speaker` must fit an `int`; a word object may omit `start`/`end` (0) or `text` (C# `null`,
/// here `""`), because `Word` is a record struct bound by its constructor. Anything else is the shared decoding.
fn parse_json_windows(json: &str) -> Option<Vec<Sentence>> {
    // `Value` keeps the last of repeated keys, like the C# reader.
    let mut value: serde_json::Value = serde_json::from_str(json).ok()?;
    let fits_i32 = |v: Option<&serde_json::Value>| match v {
        None | Some(serde_json::Value::Null) => true,
        Some(v) => v.as_i64().is_some_and(|i| i32::try_from(i).is_ok()),
    };
    for sentence in value.as_array_mut()? {
        let object = sentence.as_object_mut()?;
        if !fits_i32(object.get("id")) || !fits_i32(object.get("speaker")) {
            return None;
        }
        if let Some(serde_json::Value::Array(words)) = object.get_mut("words") {
            for word in words {
                let word = word.as_object_mut()?;
                for key in ["start", "end"] {
                    word.entry(key).or_insert(serde_json::Value::from(0));
                }
                word.entry("text").or_insert(serde_json::Value::from(""));
            }
        }
    }
    serde_json::from_value(value).ok()
}

/// SRT/VTT line trimming: Windows `string.Trim()` (all Unicode white space); macOS
/// `trimmingCharacters(in: .whitespaces)` (space separators and tab only, so a line holding just U+2028, a form feed
/// or a vertical tab is not blank there).
fn trim_line(line: &str, flavor: ImportFlavor) -> &str {
    match flavor {
        ImportFlavor::Windows => trim_dotnet(line),
        ImportFlavor::Mac => line.trim_matches(|c: char| c == '\t' || (c.is_whitespace() && !is_line_break_class(c))),
    }
}

/// White space that Foundation's `.whitespaces` leaves out: `\n`, VT, FF, `\r`, NEL, LS, PS (everything else in
/// Unicode `White_Space` is a space separator or tab).
fn is_line_break_class(c: char) -> bool {
    matches!(c, '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

fn split_lines(content: &str) -> Vec<&str> {
    // `\r\n` → `\n`, lone `\r` → `\n`, then split on `\n` (empty lines kept).
    let mut out = Vec::new();
    let bytes = content.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push(&content[start..i]);
                start = i + 1;
            }
            b'\r' => {
                out.push(&content[start..i]);
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&content[start..]);
    out
}

fn timing() -> &'static Regex {
    static TIMING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(?P<start>(?:\d{1,2}:)?\d{1,2}:\d{2}[,\.]\d{1,3})\s*-->\s*(?P<end>(?:\d{1,2}:)?\d{1,2}:\d{2}[,\.]\d{1,3})")
            .expect("timing regex")
    });
    &TIMING
}

fn starts_with_ignore_ascii_case(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(prefix))
}

fn make_sentence(count: usize, start: f64, end: f64, text: String) -> Sentence {
    Sentence::new(count as i64 + 1, round3(start), round3(end.max(start + 0.05)), text)
}

fn parse_srt_or_vtt(content: &str, flavor: ImportFlavor) -> Vec<Sentence> {
    let lines = split_lines(content);
    let n = lines.len();
    let mut results: Vec<Sentence> = Vec::new();
    let mut i = 0;
    while i < n {
        let line = trim_line(lines[i], flavor);
        if line.is_empty() || starts_with_ignore_ascii_case(line, "WEBVTT") {
            i += 1;
            continue;
        }
        if line.starts_with("NOTE") || line.starts_with("STYLE") {
            i += 1;
            while i < n && !trim_line(lines[i], flavor).is_empty() {
                i += 1;
            }
            continue;
        }
        let mut caps = timing().captures(line);
        if caps.is_none() && i + 1 < n {
            let next = timing().captures(trim_line(lines[i + 1], flavor));
            if next.is_some() {
                caps = next;
                i += 1;
            }
        }
        let Some(caps) = caps else {
            i += 1;
            continue;
        };
        let start = parse_timestamp(&caps["start"], flavor);
        let end = parse_timestamp(&caps["end"], flavor);
        i += 1;
        let mut text_lines = Vec::new();
        while i < n && !trim_line(lines[i], flavor).is_empty() {
            let cleaned = clean_markup(trim_line(lines[i], flavor));
            if !cleaned.is_empty() {
                text_lines.push(cleaned);
            }
            i += 1;
        }
        let text = trim_dotnet(&text_lines.join(" ")).to_owned();
        if !text.is_empty() && end >= start {
            let count = results.len();
            results.push(make_sentence(count, start, end, text));
        }
    }
    results
}

fn parse_ass(content: &str, flavor: ImportFlavor) -> Vec<Sentence> {
    let mut results: Vec<Sentence> = Vec::new();
    let mut in_events = false;
    let (mut start_idx, mut end_idx, mut text_idx, mut field_count) = (1usize, 2usize, 9usize, 10usize);
    for raw in split_lines(content) {
        let line = trim_dotnet(raw);
        if line.starts_with('[') && line.ends_with(']') {
            in_events = line.eq_ignore_ascii_case("[Events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if starts_with_ignore_ascii_case(line, "Format:") {
            let fields: Vec<String> = line["Format:".len()..].split(',').map(|f| trim_dotnet(f).to_lowercase()).collect();
            field_count = fields.len();
            if let Some(i) = fields.iter().position(|f| f == "start") {
                start_idx = i;
            }
            if let Some(i) = fields.iter().position(|f| f == "end") {
                end_idx = i;
            }
            if let Some(i) = fields.iter().position(|f| f == "text") {
                text_idx = i;
            }
            continue;
        }
        if !starts_with_ignore_ascii_case(line, "Dialogue:") {
            continue;
        }
        let payload = line["Dialogue:".len()..].trim_start_matches(char::is_whitespace);
        let parts: Vec<&str> = payload.splitn(field_count.max(1), ',').collect();
        if parts.len() <= start_idx.max(end_idx).max(text_idx) {
            continue;
        }
        let start = parse_timestamp(trim_dotnet(parts[start_idx]), flavor);
        let end = parse_timestamp(trim_dotnet(parts[end_idx]), flavor);
        let raw_text = parts[text_idx].replace("\\N", " ").replace("\\n", " ").replace("\\h", " ");
        let text = clean_markup(&raw_text);
        if !text.is_empty() && end >= start {
            let count = results.len();
            results.push(make_sentence(count, start, end, text));
        }
    }
    results
}

/// `\{[^}]*\}` and `<[^>]+>` removed, whitespace runs collapsed to one space, trimmed. Entities are not decoded.
fn clean_markup(text: &str) -> String {
    static ASS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[^}]*\}").expect("ass regex"));
    static HTML: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").expect("html regex"));
    static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("ws regex"));
    let a = ASS.replace_all(text, "");
    let b = HTML.replace_all(&a, "");
    let c = WS.replace_all(&b, " ");
    trim_dotnet(&c).to_owned()
}

/// `ts(raw)`: `h:m:s` or `m:s` with `,` as an alternative decimal separator; anything else → 0.
///
/// Windows (C# `ParseTimestamp`): `int.TryParse`/`double.TryParse` with the invariant culture, which allow only ASCII
/// white space (`\t`…`\r`, space) around a part, and `h * 3600 + m * 60` computed in wrapping 32-bit `int`
/// arithmetic before the seconds are added. macOS (Swift `parseTimestamp`): `Int(_:)` / `Double(_:)`, 64-bit, no
/// white space inside a part at all. Both: decimal number syntax only (C# would also read `Infinity`/`NaN` and Swift
/// `inf`/`nan`/hex floats; such a time cannot be written back, so the port reads it as 0).
fn parse_timestamp(raw: &str, flavor: ImportFlavor) -> f64 {
    let normalized = trim_dotnet(raw).replace(',', ".");
    let parts: Vec<&str> = normalized.split(':').collect();
    let part = |s: &'_ str| -> Option<String> {
        match flavor {
            ImportFlavor::Windows => Some(s.trim_matches(|c: char| matches!(c, '\t'..='\r' | ' ')).to_owned()),
            ImportFlavor::Mac => (!s.starts_with(char::is_whitespace) && !s.ends_with(char::is_whitespace)).then(|| s.to_owned()),
        }
    };
    let int = |s: &str| part(s)?.parse::<i64>().ok().filter(|v| flavor == ImportFlavor::Mac || i32::try_from(*v).is_ok());
    let float = |s: &str| {
        let t = part(s)?;
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E')) {
            return None;
        }
        t.parse::<f64>().ok()
    };
    let whole = |h: i64, m: i64| -> f64 {
        match flavor {
            ImportFlavor::Windows => (h as i32).wrapping_mul(3600).wrapping_add((m as i32).wrapping_mul(60)) as f64,
            ImportFlavor::Mac => h.wrapping_mul(3600).wrapping_add(m.wrapping_mul(60)) as f64,
        }
    };
    match parts.as_slice() {
        [h, m, s] => match (int(h), int(m), float(s)) {
            (Some(h), Some(m), Some(s)) => whole(h, m) + s,
            _ => 0.0,
        },
        [m, s] => match (int(m), float(s)) {
            (Some(m), Some(s)) => whole(0, m) + s,
            _ => 0.0,
        },
        _ => 0.0,
    }
}
