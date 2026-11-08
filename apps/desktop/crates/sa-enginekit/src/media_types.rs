//! Media file types (mac-enginekit §11). Extension = text after the final `.` of the file name, lower-cased
//! (`Path::extension` semantics; `.mp4` alone has none).

use std::path::Path;

pub const VIDEO: [&str; 14] = ["mp4", "mov", "m4v", "mkv", "avi", "webm", "wmv", "mpg", "mpeg", "ts", "mts", "m2ts", "3gp", "flv"];
pub const AUDIO: [&str; 8] = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma"];

fn ext(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(str::to_lowercase)
}

/// Video or audio.
pub fn is_media(path: &Path) -> bool {
    ext(path).is_some_and(|e| VIDEO.contains(&e.as_str()) || AUDIO.contains(&e.as_str()))
}

pub fn is_audio(path: &Path) -> bool {
    ext(path).is_some_and(|e| AUDIO.contains(&e.as_str()))
}

/// `VIDEO ++ AUDIO` in order.
pub fn media_extensions() -> impl Iterator<Item = &'static str> {
    VIDEO.into_iter().chain(AUDIO)
}
