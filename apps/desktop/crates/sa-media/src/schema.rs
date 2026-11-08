//! `media-index.json` schema shared by both backends (win-media §1.4, mac-librarykit §15.2).
//!
//! The two apps now write different versions of the same schema:
//!
//! * **Windows** (current C# `MediaLibraryIndex`, `PersistedFormatVersion = 2`): indented, C# property order,
//!   `{"version": 2, "directories": [{"directoryPath", "subdirectories", "mediaFiles": […]}]}`. No `updatedAtUtc` and
//!   no `scannedAtUtc` (a scan time recorded when a video was opened), `indexedAtUtc` is
//!   `0001-01-01T00:00:00+00:00`. A file below version 2 is a legacy index and is discarded once by the index.
//!   The reader is System.Text.Json-lenient: missing properties take defaults, unknown ones are ignored.
//! * **macOS** (LibraryKit, version 1): compact with sorted keys,
//!   `{"directories":[…],"updatedAtUtc":"…Z","version":1}` with `scannedAtUtc` per folder and millisecond `Z` dates.
//!   The reader is `JSONDecoder`-strict: every key is required (except `sidecarExtension`), a newer version is
//!   unusable.
//!
//! Both readers accept either date form. Times are UTC .NET ticks (see [`crate::dates`]).

use serde_json::{Map, Value};

use crate::dates;

/// One indexed media file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IndexedMediaFile {
    pub full_path: String,
    pub file_name: String,
    pub directory_path: String,
    pub size_bytes: i64,
    /// UTC, .NET ticks.
    pub last_write_time_utc: i64,
    pub has_sidecar_subtitle: bool,
    /// Lowercase incl. dot (`.srt`), None if no sidecar.
    pub sidecar_extension: Option<String>,
    /// UTC, .NET ticks (0 = `0001-01-01T00:00:00+00:00`, what the Windows app writes).
    pub indexed_at_utc: i64,
}

impl IndexedMediaFile {
    /// `None`, `"CC"` (sidecar without extension) or the upper-cased extension (`SRT`, `VTT`, …).
    pub fn sidecar_badge(&self) -> Option<String> {
        if !self.has_sidecar_subtitle {
            return None;
        }
        // C#: `IsNullOrWhiteSpace(ext) ? "CC" : ext.TrimStart('.').ToUpperInvariant()` (no whitespace trim).
        match self.sidecar_extension.as_deref() {
            None => Some("CC".into()),
            Some(ext) if ext.trim().is_empty() => Some("CC".into()),
            Some(ext) => Some(crate::text::upper_invariant(ext.trim_start_matches('.'))),
        }
    }

    /// `formatted_size` of the Windows explorer rows (win-media §1.6).
    pub fn formatted_size(&self) -> String {
        crate::format_file_size(self.size_bytes)
    }
}

/// One scanned folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySnapshot {
    pub directory_path: String,
    /// Normalized full paths.
    pub subdirectories: Vec<String>,
    pub media_files: Vec<IndexedMediaFile>,
    /// UTC ticks; always 0 on Windows (no scan time is recorded there).
    pub scanned_at_utc: i64,
}

impl DirectorySnapshot {
    /// An empty listing of `directory_path`.
    pub fn empty(directory_path: impl Into<String>, scanned_at_utc: i64) -> Self {
        Self { directory_path: directory_path.into(), subdirectories: Vec::new(), media_files: Vec::new(), scanned_at_utc }
    }
}

/// The whole document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaIndexDocument {
    pub version: i32,
    pub directories: Vec<DirectorySnapshot>,
}

/// Date form (and document shape) written by this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateStyle {
    /// Windows: `2026-11-02T17:06:17.5945068+00:00`, indented C#-ordered v2 shape without scan times.
    DotNet,
    /// macOS: `2026-11-02T17:06:17.594Z`, compact sorted v1 shape with `updatedAtUtc` / `scannedAtUtc`.
    Apple,
}

/// What the lenient (.NET) reader made of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DotNetRead {
    /// Not valid JSON for the schema (a .NET `JsonException`), or the literal `null`: keep the current state.
    Invalid,
    /// `version` below [`MediaIndexDocument::WINDOWS_VERSION`] (a file without `version` is version 1).
    Legacy(i32),
    /// Current version but `directories` is `null`: keep the current state (a missing `directories` is an empty list,
    /// the C# property initializer).
    NoDirectories,
    Loaded(MediaIndexDocument),
}

impl MediaIndexDocument {
    /// The version the Windows app writes and accepts (`MediaLibraryIndex.PersistedFormatVersion`).
    pub const WINDOWS_VERSION: i32 = 2;
    /// The version the macOS app writes and the newest it accepts (`PersistedMediaIndex.currentVersion`).
    pub const APPLE_VERSION: i32 = 1;

    /// Lenient (.NET) parse; `None` for an invalid document or a current-version file without `directories`. A legacy
    /// file is returned with its version (and its directories) so the caller can discard it.
    pub fn parse(json: &str) -> Option<Self> {
        match Self::read_dotnet(json) {
            DotNetRead::Invalid | DotNetRead::NoDirectories => None,
            DotNetRead::Legacy(version) => {
                let mut doc = dotnet_document(json).ok().flatten().unwrap_or_default();
                doc.version = version;
                Some(doc)
            }
            DotNetRead::Loaded(doc) => Some(doc),
        }
    }

    /// The C# reader in detail (win-media §1.6 "Load from disk").
    pub fn read_dotnet(json: &str) -> DotNetRead {
        let root: Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return DotNetRead::Invalid,
        };
        if root.is_null() {
            return DotNetRead::Invalid;
        }
        let Some(obj) = root.as_object() else { return DotNetRead::Invalid };
        let version = match obj.get("version") {
            None => 1,
            Some(v) => match v.as_i64().and_then(|n| i32::try_from(n).ok()) {
                Some(n) => n,
                None => return DotNetRead::Invalid,
            },
        };
        let directories = match dotnet_document(json) {
            Ok(d) => d,
            Err(()) => return DotNetRead::Invalid,
        };
        if version < Self::WINDOWS_VERSION {
            return DotNetRead::Legacy(version);
        }
        match directories {
            None => DotNetRead::NoDirectories,
            Some(mut doc) => {
                doc.version = version;
                DotNetRead::Loaded(doc)
            }
        }
    }

    /// Strict (LibraryKit `JSONDecoder`) parse; `None` when any required key is missing or mistyped, a date does not
    /// parse, or `version` is newer than [`Self::APPLE_VERSION`].
    pub fn parse_apple(json: &str) -> Option<Self> {
        let root: Value = serde_json::from_str(json).ok()?;
        let obj = root.as_object()?;
        let version = i32::try_from(obj.get("version")?.as_i64()?).ok()?;
        apple_date(obj.get("updatedAtUtc")?)?;
        let mut directories = Vec::new();
        for dir in obj.get("directories")?.as_array()? {
            let dir = dir.as_object()?;
            let directory_path = dir.get("directoryPath")?.as_str()?.to_owned();
            let mut subdirectories = Vec::new();
            for s in dir.get("subdirectories")?.as_array()? {
                subdirectories.push(s.as_str()?.to_owned());
            }
            let mut media_files = Vec::new();
            for f in dir.get("mediaFiles")?.as_array()? {
                let f = f.as_object()?;
                let sidecar_extension = match f.get("sidecarExtension") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(v.as_str()?.to_owned()),
                };
                media_files.push(IndexedMediaFile {
                    full_path: f.get("fullPath")?.as_str()?.to_owned(),
                    file_name: f.get("fileName")?.as_str()?.to_owned(),
                    directory_path: f.get("directoryPath")?.as_str()?.to_owned(),
                    size_bytes: f.get("sizeBytes")?.as_i64()?,
                    last_write_time_utc: apple_date(f.get("lastWriteTimeUtc")?)?,
                    has_sidecar_subtitle: f.get("hasSidecarSubtitle")?.as_bool()?,
                    sidecar_extension,
                    indexed_at_utc: apple_date(f.get("indexedAtUtc")?)?,
                });
            }
            let scanned_at_utc = apple_date(dir.get("scannedAtUtc")?)?;
            directories.push(DirectorySnapshot { directory_path, subdirectories, media_files, scanned_at_utc });
        }
        (version <= Self::APPLE_VERSION).then_some(Self { version, directories })
    }

    /// Serializes in this platform's shape; Apple's `updatedAtUtc` is now.
    pub fn to_json(&self, style: DateStyle) -> String {
        self.to_json_at(style, dates::now_ticks())
    }

    /// [`Self::to_json`] with an explicit `updatedAtUtc` (Apple only; Windows writes none).
    pub fn to_json_at(&self, style: DateStyle, updated_at_utc: i64) -> String {
        match style {
            DateStyle::DotNet => {
                let mut root = Map::new();
                root.insert("version".into(), Value::from(self.version));
                root.insert("directories".into(), Value::Array(self.directories.iter().map(dotnet_directory).collect()));
                let mut out = String::new();
                write_stj_indented(&Value::Object(root), 0, &mut out);
                out
            }
            DateStyle::Apple => {
                // Keys inserted in ascending order (`.sortedKeys`); `preserve_order` keeps them so.
                let mut root = Map::new();
                root.insert("directories".into(), Value::Array(self.directories.iter().map(apple_directory).collect()));
                root.insert("updatedAtUtc".into(), Value::from(dates::format_apple(updated_at_utc)));
                root.insert("version".into(), Value::from(self.version));
                serde_json::to_string(&Value::Object(root)).unwrap_or_default()
            }
        }
    }
}

// ---- .NET reader ------------------------------------------------------------------------------------------------

/// A string-typed property: missing or `null` → `None`, a string → `Some`, anything else is a `JsonException`.
fn dn_string(obj: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(()),
    }
}

fn dn_i64(obj: &Map<String, Value>, key: &str) -> Result<i64, ()> {
    match obj.get(key) {
        None => Ok(0),
        Some(v) => v.as_i64().ok_or(()),
    }
}

fn dn_bool(obj: &Map<String, Value>, key: &str) -> Result<bool, ()> {
    match obj.get(key) {
        None => Ok(false),
        Some(v) => v.as_bool().ok_or(()),
    }
}

fn dn_date(obj: &Map<String, Value>, key: &str) -> Result<i64, ()> {
    match obj.get(key) {
        None => Ok(0),
        Some(Value::String(s)) => dates::parse_dotnet(s).ok_or(()),
        Some(_) => Err(()),
    }
}

/// An array property: missing or `null` → `None`.
fn dn_array<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<Option<&'a Vec<Value>>, ()> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => Ok(Some(a)),
        Some(_) => Err(()),
    }
}

fn dn_file(v: &Value) -> Result<Option<IndexedMediaFile>, ()> {
    let f = match v {
        Value::Null => return Ok(None),
        Value::Object(f) => f,
        _ => return Err(()),
    };
    Ok(Some(IndexedMediaFile {
        full_path: dn_string(f, "fullPath")?.unwrap_or_default(),
        file_name: dn_string(f, "fileName")?.unwrap_or_default(),
        directory_path: dn_string(f, "directoryPath")?.unwrap_or_default(),
        size_bytes: dn_i64(f, "sizeBytes")?,
        last_write_time_utc: dn_date(f, "lastWriteTimeUtc")?,
        has_sidecar_subtitle: dn_bool(f, "hasSidecarSubtitle")?,
        sidecar_extension: dn_string(f, "sidecarExtension")?,
        indexed_at_utc: dn_date(f, "indexedAtUtc")?,
    }))
}

/// Deserializes the directories (`Ok(None)` when `directories` is `null`). Null directory and file
/// entries are skipped; unknown properties (`updatedAtUtc`, `scannedAtUtc` of v1 files) are ignored.
fn dotnet_document(json: &str) -> Result<Option<MediaIndexDocument>, ()> {
    let root: Value = serde_json::from_str(json).map_err(|_| ())?;
    let obj = root.as_object().ok_or(())?;
    // `Directories` has the initializer `= []`: a missing property is an empty list, only an explicit `null` is null.
    let dirs = match obj.get("directories") {
        None => return Ok(Some(MediaIndexDocument::default())),
        Some(_) => match dn_array(obj, "directories")? {
            Some(dirs) => dirs,
            None => return Ok(None),
        },
    };
    let mut directories = Vec::with_capacity(dirs.len());
    for d in dirs {
        let d = match d {
            Value::Null => continue,
            Value::Object(d) => d,
            _ => return Err(()),
        };
        let directory_path = dn_string(d, "directoryPath")?.unwrap_or_default();
        let mut subdirectories = Vec::new();
        if let Some(subs) = dn_array(d, "subdirectories")? {
            for s in subs {
                match s {
                    Value::Null => subdirectories.push(String::new()),
                    Value::String(s) => subdirectories.push(s.clone()),
                    _ => return Err(()),
                }
            }
        }
        let mut media_files = Vec::new();
        if let Some(files) = dn_array(d, "mediaFiles")? {
            for f in files {
                if let Some(file) = dn_file(f)? {
                    media_files.push(file);
                }
            }
        }
        directories.push(DirectorySnapshot { directory_path, subdirectories, media_files, scanned_at_utc: 0 });
    }
    Ok(Some(MediaIndexDocument { version: 0, directories }))
}

// ---- writers -----------------------------------------------------------------------------------------------------

/// `System.Text.Json` with `WriteIndented = true` on Windows, byte for byte: two-space indentation, `"key": value`,
/// `Environment.NewLine` (`\r\n`) between lines and none at the end, `[]`/`{}` for empty containers, and the default
/// encoder's escaping (see [`write_stj_string`]).
/// Properties holding a `DateTimeOffset` (written by its converter without the string encoder).
const DATE_KEYS: [&str; 4] = ["lastWriteTimeUtc", "indexedAtUtc", "scannedAtUtc", "updatedAtUtc"];

fn write_stj_indented(v: &Value, depth: usize, out: &mut String) {
    const NEWLINE: &str = "\r\n";
    let indent = |out: &mut String, depth: usize| out.extend(std::iter::repeat_n("  ", depth));
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(text) => write_stj_string(text, out),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                out.push_str(if i == 0 { NEWLINE } else { ",\r\n" });
                indent(out, depth + 1);
                write_stj_indented(item, depth + 1, out);
            }
            out.push_str(NEWLINE);
            indent(out, depth);
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                out.push_str(if i == 0 { NEWLINE } else { ",\r\n" });
                indent(out, depth + 1);
                write_stj_string(key, out);
                out.push_str(": ");
                match item {
                    // The DateTimeOffset converter writes its text without the encoder: `+00:00` keeps its plus.
                    Value::String(date) if DATE_KEYS.contains(&key.as_str()) => {
                        out.push('"');
                        out.push_str(date);
                        out.push('"');
                    }
                    _ => write_stj_indented(item, depth + 1, out),
                }
            }
            out.push_str(NEWLINE);
            indent(out, depth);
            out.push('}');
        }
    }
}

/// A JSON string as `System.Text.Json`'s default encoder writes it: printable ASCII stays except `"`, `&`, `'`, `+`,
/// `<`, `>`, `` ` `` and DEL; a backslash, `\b`, `\t`, `\n`, `\f`, `\r` use short escapes; everything else (other control
/// characters, `"`, the HTML-sensitive characters, every non-ASCII character as its UTF-16 code units) is `\uXXXX`
/// with upper-case hex.
fn write_stj_string(text: &str, out: &mut String) {
    use std::fmt::Write as _;
    out.push('"');
    let mut units = [0u16; 2];
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            ' '..='~' if !matches!(c, '"' | '&' | '\'' | '+' | '<' | '>' | '`') => out.push(c),
            _ => {
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04X}");
                }
            }
        }
    }
    out.push('"');
}

fn dotnet_directory(d: &DirectorySnapshot) -> Value {
    let mut m = Map::new();
    m.insert("directoryPath".into(), Value::from(d.directory_path.clone()));
    m.insert("subdirectories".into(), Value::from(d.subdirectories.clone()));
    m.insert("mediaFiles".into(), Value::Array(d.media_files.iter().map(dotnet_file).collect()));
    Value::Object(m)
}

fn dotnet_file(f: &IndexedMediaFile) -> Value {
    let mut m = Map::new();
    m.insert("fullPath".into(), Value::from(f.full_path.clone()));
    m.insert("fileName".into(), Value::from(f.file_name.clone()));
    m.insert("directoryPath".into(), Value::from(f.directory_path.clone()));
    m.insert("sizeBytes".into(), Value::from(f.size_bytes));
    m.insert("lastWriteTimeUtc".into(), Value::from(dates::format_dotnet(f.last_write_time_utc)));
    m.insert("hasSidecarSubtitle".into(), Value::from(f.has_sidecar_subtitle));
    m.insert("sidecarExtension".into(), f.sidecar_extension.clone().map_or(Value::Null, Value::from));
    m.insert("indexedAtUtc".into(), Value::from(dates::format_dotnet(f.indexed_at_utc)));
    Value::Object(m)
}

fn apple_directory(d: &DirectorySnapshot) -> Value {
    let mut m = Map::new();
    m.insert("directoryPath".into(), Value::from(d.directory_path.clone()));
    m.insert("mediaFiles".into(), Value::Array(d.media_files.iter().map(apple_file).collect()));
    m.insert("scannedAtUtc".into(), Value::from(dates::format_apple(d.scanned_at_utc)));
    m.insert("subdirectories".into(), Value::from(d.subdirectories.clone()));
    Value::Object(m)
}

fn apple_file(f: &IndexedMediaFile) -> Value {
    let mut m = Map::new();
    m.insert("directoryPath".into(), Value::from(f.directory_path.clone()));
    m.insert("fileName".into(), Value::from(f.file_name.clone()));
    m.insert("fullPath".into(), Value::from(f.full_path.clone()));
    m.insert("hasSidecarSubtitle".into(), Value::from(f.has_sidecar_subtitle));
    m.insert("indexedAtUtc".into(), Value::from(dates::format_apple(f.indexed_at_utc)));
    m.insert("lastWriteTimeUtc".into(), Value::from(dates::format_apple(f.last_write_time_utc)));
    m.insert("sidecarExtension".into(), f.sidecar_extension.clone().map_or(Value::Null, Value::from));
    m.insert("sizeBytes".into(), Value::from(f.size_bytes));
    Value::Object(m)
}

fn apple_date(v: &Value) -> Option<i64> {
    dates::parse_apple(v.as_str()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, dir: &str) -> IndexedMediaFile {
        IndexedMediaFile {
            full_path: format!("{dir}\\{name}"),
            file_name: name.into(),
            directory_path: dir.into(),
            size_bytes: 2048,
            last_write_time_utc: dates::parse_dotnet("2026-11-01T08:12:44.5+00:00").unwrap(),
            has_sidecar_subtitle: true,
            sidecar_extension: Some(".srt".into()),
            indexed_at_utc: 0,
        }
    }

    #[test]
    fn dotnet_reader_accepts_escapes_fractions_nulls_and_missing_properties() {
        let json = r#"{
          "version": 2,
          "directories": [
            {
              "directoryPath": "C:\\Vid\u00E9o",
              "subdirectories": [ "C:\\Vid\u00E9o\\a\u002Bb", null ],
              "mediaFiles": [
                { "fullPath": "C:\\Vid\u00E9o\\a\u002Bb\u0027\u003C\u015F\u003E.mp4", "fileName": "a\u002Bb\u0027\u003C\u015F\u003E.mp4",
                  "directoryPath": "C:\\Vid\u00E9o", "sizeBytes": 5000000000,
                  "lastWriteTimeUtc": "2026-11-01T08:12:44.1234567+00:00", "hasSidecarSubtitle": false,
                  "sidecarExtension": null, "indexedAtUtc": "0001-01-01T00:00:00+00:00", "futureField": [1, 2] },
                { "fullPath": "C:\\Vid\u00E9o\\b.mkv" },
                null
              ],
              "scannedAtUtc": "2026-11-07T10:00:00.12+00:00"
            },
            null
          ]
        }"#;
        let doc = MediaIndexDocument::parse(json).unwrap();
        assert_eq!(doc.version, 2);
        assert_eq!(doc.directories.len(), 1);
        let d = &doc.directories[0];
        assert_eq!(d.directory_path, "C:\\Vidéo");
        assert_eq!(d.subdirectories, ["C:\\Vidéo\\a+b", ""]);
        assert_eq!(d.media_files.len(), 2);
        let a = &d.media_files[0];
        assert_eq!(a.file_name, "a+b'<ş>.mp4");
        assert_eq!(a.size_bytes, 5_000_000_000);
        assert_eq!(dates::format_dotnet(a.last_write_time_utc), "2026-11-01T08:12:44.1234567+00:00");
        assert_eq!(a.sidecar_extension, None);
        let b = &d.media_files[1];
        assert_eq!((b.file_name.as_str(), b.size_bytes, b.last_write_time_utc, b.has_sidecar_subtitle), ("", 0, 0, false));
    }

    #[test]
    fn dotnet_reader_classifies_legacy_and_invalid_files() {
        assert_eq!(MediaIndexDocument::read_dotnet("{\"directories\": []}"), DotNetRead::Legacy(1));
        assert_eq!(MediaIndexDocument::read_dotnet("{\"version\": 1, \"directories\": []}"), DotNetRead::Legacy(1));
        assert_eq!(
            MediaIndexDocument::read_dotnet("{\"version\": 2}"),
            DotNetRead::Loaded(MediaIndexDocument { version: 2, directories: Vec::new() })
        );
        assert_eq!(MediaIndexDocument::read_dotnet("{\"version\": 2, \"directories\": null}"), DotNetRead::NoDirectories);
        for bad in ["", "null", "[]", "not json", "{\"version\": 2, \"directories\": [],}", "{\"version\": \"2\"}",
            "{\"version\": 2, \"directories\": [{\"mediaFiles\": [{\"sizeBytes\": 1.5}]}]}",
            "{\"version\": 2, \"directories\": [{\"mediaFiles\": [{\"lastWriteTimeUtc\": null}]}]}",
            "{\"version\": 1, \"directories\": [{\"mediaFiles\": [{\"lastWriteTimeUtc\": \"yesterday\"}]}]}"] {
            assert_eq!(MediaIndexDocument::read_dotnet(bad), DotNetRead::Invalid, "{bad}");
        }
    }

    #[test]
    fn dotnet_writer_matches_the_csharp_v2_shape() {
        let doc = MediaIndexDocument {
            version: 2,
            directories: vec![DirectorySnapshot {
                directory_path: "C:\\Users\\me\\Videos".into(),
                subdirectories: vec!["C:\\Users\\me\\Videos\\SubfolderA".into()],
                media_files: vec![file("episode01.mp4", "C:\\Users\\me\\Videos")],
                scanned_at_utc: 0,
            }],
        };
        let json = doc.to_json(DateStyle::DotNet);
        assert!(!json.ends_with('\n'));
        let json = json.replace("\r\n", "\n");
        assert!(!json.contains('\r'), "every line break is CRLF");
        let expected = r#"{
  "version": 2,
  "directories": [
    {
      "directoryPath": "C:\\Users\\me\\Videos",
      "subdirectories": [
        "C:\\Users\\me\\Videos\\SubfolderA"
      ],
      "mediaFiles": [
        {
          "fullPath": "C:\\Users\\me\\Videos\\episode01.mp4",
          "fileName": "episode01.mp4",
          "directoryPath": "C:\\Users\\me\\Videos",
          "sizeBytes": 2048,
          "lastWriteTimeUtc": "2026-11-01T08:12:44.5+00:00",
          "hasSidecarSubtitle": true,
          "sidecarExtension": ".srt",
          "indexedAtUtc": "0001-01-01T00:00:00+00:00"
        }
      ]
    }
  ]
}"#;
        assert_eq!(json, expected);
        assert_eq!(MediaIndexDocument::parse(&json).unwrap(), doc);
        let empty = MediaIndexDocument { version: 2, directories: Vec::new() }.to_json(DateStyle::DotNet);
        assert_eq!(empty, "{\r\n  \"version\": 2,\r\n  \"directories\": []\r\n}");
    }

    /// The escaping System.Text.Json (.NET 10, default encoder) produced for U+0000..U+007F followed by `é`, U+2028,
    /// U+FEFF, U+1F600 and `ſ` (generated with the C# app's serializer options).
    #[test]
    fn dotnet_writer_escapes_like_system_text_json() {
        let mut text: String = (0u8..0x80).map(char::from).collect();
        text.push_str("\u{e9}\u{2028}\u{feff}\u{1F600}\u{17f}");
        let mut out = String::new();
        write_stj_string(&text, &mut out);
        // `§` stands for a backslash.
        let expected = concat!(
            r#""§u0000§u0001§u0002§u0003§u0004§u0005§u0006§u0007§b§t§n§u000B§f§r§u000E§u000F§u0010§u0011§u0012§u001"#,
            r#"3§u0014§u0015§u0016§u0017§u0018§u0019§u001A§u001B§u001C§u001D§u001E§u001F !§u0022#$%§u0026§u0027()*§"#,
            r#"u002B,-./0123456789:;§u003C=§u003E?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[§§]^_§u0060abcdefghijklmnopqrstuvwxyz"#,
            r#"{|}~§u007F§u00E9§u2028§uFEFF§uD83D§uDE00§u017F""#,
        )
        .replace('§', "\\");
        assert_eq!(out, expected);
        let back: String = serde_json::from_str(&out).unwrap();
        assert_eq!(back, text);
    }

    #[test]
    fn apple_writer_is_compact_sorted_and_round_trips() {
        let mut f = file("b.mp4", "/Shows");
        f.full_path = "/Shows/b.mp4".into();
        f.indexed_at_utc = dates::parse_apple("2026-01-02T03:04:05.123Z").unwrap();
        f.last_write_time_utc = dates::parse_apple("2026-01-02T03:04:05Z").unwrap();
        let mut g = f.clone();
        g.file_name = "a.mkv".into();
        g.full_path = "/Shows/a.mkv".into();
        g.has_sidecar_subtitle = false;
        g.sidecar_extension = None;
        let doc = MediaIndexDocument {
            version: 1,
            directories: vec![DirectorySnapshot {
                directory_path: "/Shows".into(),
                subdirectories: vec!["/Shows/Alpha".into()],
                media_files: vec![g, f],
                scanned_at_utc: dates::parse_apple("2026-01-02T03:04:05Z").unwrap(),
            }],
        };
        let json = doc.to_json_at(DateStyle::Apple, dates::parse_apple("2026-01-02T03:04:06Z").unwrap());
        assert_eq!(
            json,
            "{\"directories\":[{\"directoryPath\":\"/Shows\",\"mediaFiles\":[{\"directoryPath\":\"/Shows\",\
             \"fileName\":\"a.mkv\",\"fullPath\":\"/Shows/a.mkv\",\"hasSidecarSubtitle\":false,\
             \"indexedAtUtc\":\"2026-01-02T03:04:05.123Z\",\"lastWriteTimeUtc\":\"2026-01-02T03:04:05.000Z\",\
             \"sidecarExtension\":null,\"sizeBytes\":2048},{\"directoryPath\":\"/Shows\",\"fileName\":\"b.mp4\",\
             \"fullPath\":\"/Shows/b.mp4\",\"hasSidecarSubtitle\":true,\"indexedAtUtc\":\"2026-01-02T03:04:05.123Z\",\
             \"lastWriteTimeUtc\":\"2026-01-02T03:04:05.000Z\",\"sidecarExtension\":\".srt\",\"sizeBytes\":2048}],\
             \"scannedAtUtc\":\"2026-01-02T03:04:05.000Z\",\"subdirectories\":[\"/Shows/Alpha\"]}],\
             \"updatedAtUtc\":\"2026-01-02T03:04:06.000Z\",\"version\":1}"
        );
        assert!(!json.contains("\\/"));
        assert_eq!(MediaIndexDocument::parse_apple(&json).unwrap(), doc);
    }

    /// Byte for byte what LibraryKit's `JSONEncoder` (`[.sortedKeys, .withoutEscapingSlashes]`, ISO 8601 with
    /// milliseconds) writes for a Turkish NFC name with a quote and a backslash: raw UTF-8, `\"` and `\\` escapes only.
    #[test]
    fn apple_writer_matches_a_swift_written_file_byte_for_byte() {
        let swift = "{\"directories\":[{\"directoryPath\":\"/Users/me/Movies/Şarkılar\",\"mediaFiles\":[{\"directoryPath\":\
                     \"/Users/me/Movies/Şarkılar\",\"fileName\":\"Işık \\\"canlı\\\" \\\\ 2.mp4\",\"fullPath\":\
                     \"/Users/me/Movies/Şarkılar/Işık \\\"canlı\\\" \\\\ 2.mp4\",\"hasSidecarSubtitle\":true,\
                     \"indexedAtUtc\":\"2026-11-02T17:06:17.594Z\",\"lastWriteTimeUtc\":\"2026-10-30T09:00:00.000Z\",\
                     \"sidecarExtension\":\".vtt\",\"sizeBytes\":5000000000}],\"scannedAtUtc\":\"2026-11-02T17:06:17.594Z\",\
                     \"subdirectories\":[\"/Users/me/Movies/Şarkılar/Canlı\"]}],\"updatedAtUtc\":\"2026-11-02T17:06:17.601Z\",\
                     \"version\":1}";
        let doc = MediaIndexDocument::parse_apple(swift).expect("Swift file reads");
        let f = &doc.directories[0].media_files[0];
        assert_eq!(f.file_name, "Işık \"canlı\" \\ 2.mp4");
        assert_eq!(f.size_bytes, 5_000_000_000);
        assert_eq!(dates::format_apple(f.indexed_at_utc), "2026-11-02T17:06:17.594Z");
        let updated = dates::parse_apple("2026-11-02T17:06:17.601Z").unwrap();
        assert_eq!(doc.to_json_at(DateStyle::Apple, updated), swift, "the Rust writer is byte-identical");
    }

    #[test]
    fn apple_reader_rejects_unusable_files() {
        let bad = [
            "not json at all",
            "",
            "{\"version\": 1}",
            "[]",
            "{\"version\": 1, \"updatedAtUtc\": \"yesterday\", \"directories\": []}",
            "{\"version\": 2, \"updatedAtUtc\": \"2026-01-02T03:04:05Z\", \"directories\": [{\"directoryPath\": \"/x\", \
             \"subdirectories\": [], \"mediaFiles\": [], \"scannedAtUtc\": \"2026-01-02T03:04:05Z\"}]}",
            "{\"version\": 1, \"updatedAtUtc\": \"2026-01-02T03:04:05\", \"directories\": []}",
        ];
        for b in bad {
            assert_eq!(MediaIndexDocument::parse_apple(b), None, "{b}");
        }
        let ok = "{\"version\": 1, \"updatedAtUtc\": \"2026-01-02T03:04:05.1234567+00:00\", \"directories\": []}";
        assert_eq!(MediaIndexDocument::parse_apple(ok).unwrap().directories.len(), 0);
    }

    #[test]
    fn badge() {
        let mut f = file("a.mp4", "C:\\x");
        assert_eq!(f.sidecar_badge().as_deref(), Some("SRT"));
        f.sidecar_extension = None;
        assert_eq!(f.sidecar_badge().as_deref(), Some("CC"));
        f.has_sidecar_subtitle = false;
        assert_eq!(f.sidecar_badge(), None);
        assert_eq!(f.formatted_size(), "2 KB");
    }
}
