//! Media file types (mac-enginekit §11). Extension = text after the final `.` of the file name, lower-cased
//! (`Path::extension` semantics; `.mp4` alone has none). The lists are [`sa_core::media_types`].

use std::path::Path;

pub use sa_core::media_types::{AUDIO_EXTENSIONS as AUDIO, VIDEO_EXTENSIONS as VIDEO};

fn ext(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(str::to_lowercase)
}

/// Video or audio.
pub fn is_media(path: &Path) -> bool {
    ext(path).is_some_and(|e| sa_core::media_types::is_media_extension(&e))
}

pub fn is_audio(path: &Path) -> bool {
    ext(path).is_some_and(|e| sa_core::media_types::is_audio_extension(&e))
}

/// `VIDEO ++ AUDIO` in order.
pub fn media_extensions() -> impl Iterator<Item = &'static str> {
    sa_core::media_types::MEDIA_EXTENSIONS.into_iter()
}
