//! The platform-independent half of LibraryKit's `MediaFolderScanner` (mac-librarykit §15.6 steps 3–7) and the
//! EngineKit rules it uses (mac-enginekit §11 `MediaFileTypes`, §12.2 `SubtitleBadgeLookup`). The macOS lister
//! (`scanner.rs`) gathers [`RawEntry`] values from the file system; tests feed them from a virtual tree.

use std::collections::HashMap;

use super::path;
use crate::schema::{DirectorySnapshot, IndexedMediaFile};
use crate::text::{lower_key, nfc, ordered_cmp};

/// `MediaFileTypes.mediaExtensions` (video then audio, lower-case, without the dot; [`sa_core::media_types`]).
pub const MEDIA_EXTENSIONS: &[&str] = &sa_core::media_types::MEDIA_EXTENSIONS;
/// `SubtitleParser.extensions` (badge lookup order).
pub const SUBTITLE_EXTENSIONS: [&str; 5] = ["srt", "vtt", "ass", "ssa", "json"];

/// `URL.pathExtension` of a file name: the text after the last `.`, none for a leading-dot-only name.
fn path_extension(name: &str) -> Option<&str> {
    let dot = name.rfind('.')?;
    (dot > 0 && dot + 1 < name.len()).then(|| &name[dot + 1..])
}

/// `MediaFileTypes.isMedia`.
pub fn is_media(name: &str) -> bool {
    path_extension(name).is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

/// `URL.deletingPathExtension().lastPathComponent`.
pub fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// EngineKit `SubtitleParser.foldedName`: `NFC(lowercase(NFC(name)))`.
fn folded_name(name: &str) -> String {
    lower_key(&nfc(name))
}

/// `SubtitleParser.SubtitleBadgeLookup`: one folder listing prepared for repeated badge queries.
pub struct SubtitleBadgeLookup {
    names_by_extension: HashMap<&'static str, Vec<String>>,
}

impl SubtitleBadgeLookup {
    pub fn new<'a>(file_names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut names_by_extension: HashMap<&'static str, Vec<String>> = HashMap::new();
        for name in file_names {
            let Some(dot) = name.rfind('.') else { continue };
            let ext = name[dot + 1..].to_lowercase();
            if let Some(known) = SUBTITLE_EXTENSIONS.iter().find(|e| **e == ext) {
                names_by_extension.entry(known).or_default().push(folded_name(name));
            }
        }
        Self { names_by_extension }
    }

    /// The first subtitle extension (lower-case, no dot) for which the listing holds `<stem>.<ext>` or
    /// `<stem>.<anything>.<ext>`.
    pub fn badge_extension(&self, video_stem: &str) -> Option<&'static str> {
        let stem = folded_name(video_stem);
        if stem.is_empty() {
            return None;
        }
        let prefix = format!("{stem}.");
        SUBTITLE_EXTENSIONS.iter().copied().find(|ext| {
            let suffix_chars = ext.chars().count() + 1;
            self.names_by_extension.get(ext).is_some_and(|names| {
                names.iter().any(|name| {
                    let keep = name.chars().count().saturating_sub(suffix_chars);
                    let base: String = name.chars().take(keep).collect();
                    base == stem || (base.starts_with(&prefix) && base.chars().count() > prefix.chars().count())
                })
            })
        })
    }
}

/// One directory entry as the macOS lister saw it (symbolic links already replaced by their target's values).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    pub name: String,
    /// `isHidden` (BSD `UF_HIDDEN` or Finder-hidden).
    pub hidden: bool,
    pub is_dir: bool,
    /// `isPackage` (`.app`, `.photoslibrary`, bundles…).
    pub is_package: bool,
    /// A symbolic link whose target is missing.
    pub dangling: bool,
    pub size: Option<i64>,
    /// UTC ticks.
    pub modified: Option<i64>,
}

impl RawEntry {
    pub fn file(name: &str, size: i64, modified: i64) -> Self {
        Self { name: name.into(), hidden: false, is_dir: false, is_package: false, dangling: false, size: Some(size), modified: Some(modified) }
    }
    pub fn dir(name: &str) -> Self {
        Self { name: name.into(), hidden: false, is_dir: true, is_package: false, dangling: false, size: None, modified: None }
    }
}

/// Steps 3–7 of `MediaFolderScanner.list` for the already normalized folder `path`.
pub fn build_listing(path: &str, entries: &[RawEntry], now: i64) -> DirectorySnapshot {
    let mut subdirectories = Vec::new();
    let mut file_names: Vec<&str> = Vec::new();
    let mut media: Vec<(&str, i64, i64)> = Vec::new();
    for e in entries {
        if e.name.starts_with('.') || e.hidden || e.dangling {
            continue;
        }
        if e.is_dir {
            if !e.is_package {
                subdirectories.push(path::child(&e.name, path));
            }
            continue;
        }
        file_names.push(&e.name);
        if is_media(&e.name) {
            media.push((&e.name, e.size.unwrap_or(0), e.modified.unwrap_or(crate::dates::UNIX_EPOCH_TICKS)));
        }
    }
    let badges = SubtitleBadgeLookup::new(file_names.iter().copied());
    let mut media_files: Vec<IndexedMediaFile> = media
        .into_iter()
        .map(|(name, size, modified)| {
            let sidecar = badges.badge_extension(stem(name)).map(|e| format!(".{e}"));
            let file_name = nfc(name);
            IndexedMediaFile {
                full_path: path::child(&file_name, path),
                file_name,
                directory_path: path.to_owned(),
                size_bytes: size,
                last_write_time_utc: modified,
                has_sidecar_subtitle: sidecar.is_some(),
                sidecar_extension: sidecar,
                indexed_at_utc: now,
            }
        })
        .collect();
    subdirectories.sort_by(|a, b| ordered_cmp(a, b));
    media_files.sort_by(|a, b| ordered_cmp(&a.file_name, &b.file_name));
    DirectorySnapshot { directory_path: path.to_owned(), subdirectories, media_files, scanned_at_utc: now }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_lookup_rules() {
        let names = ["Film.tr.srt", "Film..vtt", "Other.vtt", "Show.tr.forced.vtt", "Show.ass", "x.SRT"];
        let lookup = SubtitleBadgeLookup::new(names.iter().copied());
        assert_eq!(lookup.badge_extension("Film"), Some("srt"));
        assert_eq!(lookup.badge_extension("Other"), Some("vtt"));
        assert_eq!(lookup.badge_extension("Show"), Some("vtt"), "a tagged .vtt beats a plain .ass by extension order");
        assert_eq!(lookup.badge_extension("X"), Some("srt"));
        let dotted = SubtitleBadgeLookup::new(["Film..vtt"]);
        assert_eq!(dotted.badge_extension("Film"), None, "`Film..vtt` minus `.vtt` is `film.`, not longer than the prefix");
        assert_eq!(lookup.badge_extension(""), None);
        assert_eq!(lookup.badge_extension("Missing"), None);
    }

    #[test]
    fn media_types() {
        assert!(is_media("a.MP4"));
        assert!(!is_media(".mp4"));
        assert!(!is_media("mp4"));
        assert!(!is_media("a.mp4.bak"));
        assert!(is_media("a.VOB") && is_media("a.mka") && is_media("a.dvr-ms"));
        assert!(!is_media("a.srt"));
        assert_eq!(stem("Film.tr.srt"), "Film.tr");
        assert_eq!(stem(".hidden"), ".hidden");
    }
}
