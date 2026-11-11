//! .NET `System.IO.Path` semantics on Windows (win-media §0.1, §1.2, §1.6).

use std::path::Path;

use crate::text::{eq_ordinal_ignore_case, upper_ordinal};

/// `MediaLibraryIndex.SupportedExtensions` (ordered; without dots): the C# list widened by [`sa_core::media_types`].
pub const SUPPORTED_EXTENSIONS: &[&str] = &sa_core::media_types::MEDIA_EXTENSIONS;
/// Audio extensions (without dots).
pub const AUDIO_EXTENSIONS: &[&str] = sa_core::media_types::AUDIO_EXTENSIONS;
/// `SubtitleParser.Extensions` (sidecar lookup order).
pub const SIDECAR_EXTENSIONS: [&str; 5] = [".srt", ".vtt", ".ass", ".ssa", ".json"];

fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

/// `Path.GetFileName`: the text after the last `\`, `/` (or the `:` of a `C:name` volume prefix).
pub fn file_name(path: &str) -> &str {
    let mut start = path.rfind(is_sep).map_or(0, |i| i + 1);
    if start == 0 && path.len() >= 2 && path.as_bytes()[1] == b':' && path.as_bytes()[0].is_ascii_alphabetic() {
        start = 2;
    }
    &path[start..]
}

/// `Path.GetExtension`: from the last `.` of the file name, including the dot; empty when there is no dot or the
/// dot is the last character.
pub fn extension(path: &str) -> &str {
    let name = file_name(path);
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => &name[i..],
        _ => "",
    }
}

/// `Path.GetFileNameWithoutExtension`.
pub fn file_stem(path: &str) -> &str {
    let name = file_name(path);
    match name.rfind('.') {
        Some(i) => &name[..i],
        None => name,
    }
}

/// `Path.GetDirectoryName` for the absolute paths the index stores (`None` for a root or a bare name).
pub fn directory_name(path: &str) -> Option<&str> {
    let root_len = path_root(path).len();
    let name_start = path.rfind(is_sep)?;
    if name_start < root_len {
        // The parent is the root itself (`C:\a` → `C:\`), unless the path is the root.
        return if path.len() > root_len { Some(&path[..root_len]) } else { None };
    }
    let mut end = name_start;
    while end > root_len && path[..end].ends_with(is_sep) {
        end -= 1;
    }
    Some(&path[..end])
}

/// The extension (without its dot) is in `set` (dot-less), `OrdinalIgnoreCase`.
fn ext_in(path: &str, set: &[&str]) -> bool {
    let ext = extension(path);
    !ext.is_empty() && set.iter().any(|e| eq_ordinal_ignore_case(e, &ext[1..]))
}

/// `MediaLibraryIndex.IsMediaFile`.
pub fn is_media_file(path: &str) -> bool {
    !path.trim().is_empty() && ext_in(path, SUPPORTED_EXTENSIONS)
}

/// `MediaLibraryIndex.IsAudioFile`.
pub fn is_audio_file(path: &str) -> bool {
    !path.trim().is_empty() && ext_in(path, AUDIO_EXTENSIONS)
}

/// `Path.GetFullPath` (`GetFullPathNameW` through `std::path::absolute`); `None` where .NET throws.
pub fn full_path(path: &str) -> Option<String> {
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    std::path::absolute(Path::new(path)).ok()?.to_str().map(str::to_owned)
}

/// `Path.GetPathRoot` for Windows paths: `C:\`, `C:`, `\`, `\\server\share`, `\\?\C:\`, `\\?\UNC\server\share`,
/// `\\.\device\`; `""` for a relative path.
pub fn path_root(path: &str) -> &str {
    let b = path.as_bytes();
    let sep = |i: usize| b.get(i).is_some_and(|c| *c == b'\\' || *c == b'/');
    // Device paths `\\?\` and `\\.\`.
    if b.len() >= 4 && sep(0) && sep(1) && (b[2] == b'?' || b[2] == b'.') && sep(3) {
        let rest = &path[4..];
        if rest.len() >= 4 && rest[..3].eq_ignore_ascii_case("UNC") && sep(7) {
            return &path[..unc_root_end(path, 8)];
        }
        // `\\?\C:\` or `\\.\device\`.
        let end = path[4..].find(['\\', '/']).map_or(path.len(), |i| 4 + i + 1);
        return &path[..end];
    }
    if sep(0) && sep(1) {
        return &path[..unc_root_end(path, 2)];
    }
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return if sep(2) { &path[..3] } else { &path[..2] };
    }
    if sep(0) {
        return &path[..1];
    }
    ""
}

/// End of `server\share` starting at `start` (the separator after the share is not part of the root).
fn unc_root_end(path: &str, start: usize) -> usize {
    let mut seps = 0;
    for (i, c) in path[start..].char_indices() {
        if is_sep(c) {
            seps += 1;
            if seps == 2 {
                return start + i;
            }
        }
    }
    path.len()
}

/// `MediaLibraryIndex.NormalizeDirectoryPath` (win-media §0.1): full path, roots keep their trailing `\`, everything
/// else loses trailing separators; on failure the trimmed input.
pub fn normalize_directory_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match full_path(trimmed) {
        Some(full) => {
            let root = path_root(&full);
            if !root.is_empty() && eq_ordinal_ignore_case(&full, root) {
                full
            } else {
                full.trim_end_matches(is_sep).to_owned()
            }
        }
        None => trimmed.to_owned(),
    }
}

/// `LocalProfileManager.NormalizeVideoPath` (win-media §0.1): trims whitespace and quotes, full path without trailing
/// separators (no root exception: `C:\` → `C:`).
pub fn normalize_video_path(path: &str) -> String {
    let trimmed = path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return String::new();
    }
    match full_path(trimmed) {
        Some(full) => full.trim_end_matches(is_sep).to_owned(),
        None => trimmed.replace('/', "\\").trim_end_matches('\\').to_owned(),
    }
}

/// `File.Exists` (`GetFileAttributesEx` without following links: true for anything that is not a directory).
pub fn file_exists(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_attributes() & 0x10 == 0)
}

/// `SubtitleParser.FindSidecarSubtitle`: the first existing `dir\stem + ext` over [`SIDECAR_EXTENSIONS`].
pub fn find_sidecar_subtitle(video_path: &str) -> Option<(String, &'static str)> {
    if video_path.trim().is_empty() {
        return None;
    }
    let dir = directory_name(video_path)?;
    let stem = file_stem(video_path);
    if dir.is_empty() || stem.is_empty() {
        return None;
    }
    SIDECAR_EXTENSIONS.iter().find_map(|ext| {
        let candidate = Path::new(dir).join(format!("{stem}{ext}"));
        file_exists(&candidate).then(|| (candidate.to_string_lossy().into_owned(), *ext))
    })
}

/// The OrdinalIgnoreCase key of a path (for hash maps).
pub fn ci(path: &str) -> String {
    upper_ordinal(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_path_parts() {
        assert_eq!(file_name("C:\\a\\b.mp4"), "b.mp4");
        assert_eq!(extension("C:\\a\\b.MP4"), ".MP4");
        assert_eq!(extension("C:\\a\\.mp4"), ".mp4");
        assert_eq!(extension("C:\\a.b\\c"), "");
        assert_eq!(extension("C:\\a\\b."), "");
        assert_eq!(file_stem("C:\\a\\b.tr.srt"), "b.tr");
        assert_eq!(directory_name("C:\\a\\b.mp4"), Some("C:\\a"));
        assert_eq!(directory_name("C:\\b.mp4"), Some("C:\\"));
        assert_eq!(directory_name("C:\\"), None);
        assert_eq!(directory_name("\\\\server\\share\\x.mp4"), Some("\\\\server\\share"));
        assert!(is_media_file("x.MKV"));
        assert!(!is_media_file("notes.txt"));
        assert!(!is_media_file("   "));
        assert!(is_audio_file("a.Flac"));
        assert!(!is_audio_file("a.mp4"));
        assert!(is_media_file("x.VOB") && !is_audio_file("x.vob"));
        assert!(is_media_file("x.mka") && is_audio_file("x.AIFF"));
        assert!(!is_media_file("x.") && !is_media_file("x.srt"));
    }

    #[test]
    fn roots() {
        assert_eq!(path_root("C:\\a\\b"), "C:\\");
        assert_eq!(path_root("C:"), "C:");
        assert_eq!(path_root("\\\\server\\share\\dir"), "\\\\server\\share");
        assert_eq!(path_root("\\\\?\\C:\\dir"), "\\\\?\\C:\\");
        assert_eq!(path_root("\\\\?\\UNC\\server\\share\\dir"), "\\\\?\\UNC\\server\\share");
        assert_eq!(path_root("relative\\x"), "");
    }

    #[test]
    fn normalizers() {
        assert_eq!(normalize_directory_path("  "), "");
        assert_eq!(normalize_directory_path("C:\\"), "C:\\");
        assert_eq!(normalize_directory_path("c:\\"), "c:\\");
        assert_eq!(normalize_directory_path("C:\\Videos\\"), "C:\\Videos");
        assert_eq!(normalize_directory_path("C:/Videos/Sub/../x//"), "C:\\Videos\\x");
        assert_eq!(normalize_video_path(" \"C:\\Videos\\a.mp4\" "), "C:\\Videos\\a.mp4");
        assert_eq!(normalize_video_path("C:\\"), "C:");
        assert_eq!(normalize_video_path("\"\""), "");
    }
}
