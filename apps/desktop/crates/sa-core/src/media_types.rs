//! The media file types the desktop app accepts: the single source of truth for the open dialog filter, drag and
//! drop, the explorer listing and media index, the companion-video search of a subtitle file, the MIME lookup of the
//! buffered stream and (C#-era subset only) the Windows file associations.
//!
//! Extensions are lower-case ASCII without the dot. Every consumer keeps its own matching rule (Rust `to_lowercase`,
//! .NET `OrdinalIgnoreCase`, `URL.pathExtension`), only the lists come from here.
//!
//! The first [`CSHARP_VIDEO_COUNT`] entries of [`VIDEO_EXTENSIONS`] and the first [`CSHARP_AUDIO_COUNT`] of
//! [`AUDIO_EXTENSIONS`] are the former C# app's lists (`MainWindow.VideoTypes`, `MediaLibraryIndex.SupportedExtensions`,
//! `FileAssociationRegistrar.MediaExtensions`) and the SwiftUI app's original `MediaFileTypes`, in their order. The
//! rest (everything VLC commonly opens that the shipped FFmpeg build demuxes; image formats, playlists, AviSynth
//! scripts, DASH segments without their init segment and MIDI are left out) is appended after them, so searches that
//! return the first match keep their old results.

/// Video containers (and raw video elementary streams): the C#-era 14, then the additions.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    // C#-era
    "mp4", "mov", "m4v", "mkv", "avi", "webm", "wmv", "mpg", "mpeg", "ts", "mts", "m2ts", "3gp", "flv",
    // MPEG program / elementary / transport streams, camcorder variants
    "vob", "m2v", "m1v", "mpe", "mpv", "mp2v", "m2t", "tp", "trp", "mod", "tod",
    // Ogg, Matroska 3D, ASF, AVI flavors, RealMedia, Flash
    "ogv", "ogm", "mk3d", "asf", "divx", "xvid", "rm", "rmvb", "f4v",
    // Professional and broadcast
    "mxf", "dv", "gxf", "wtv", "dvr-ms",
    // ISO/QuickTime family
    "3g2", "3gp2", "mj2", "qt", "mpg4", "mp4v",
    // Other containers and raw streams
    "nut", "amv", "y4m", "ivf", "h264", "264", "h265", "265", "hevc", "nsv", "bik", "smk", "roq", "swf",
];

/// Audio-only containers and raw audio streams: the C#-era 8, then the additions.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    // C#-era
    "mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma",
    // Matroska audio, Dolby / DTS raw streams
    "mka", "ac3", "eac3", "dts", "dtshd", "thd", "truehd", "mlp",
    // Uncompressed and lossless
    "aif", "aiff", "aifc", "ape", "wv", "tta", "tak", "w64", "caf", "au", "snd", "voc", "dsf", "dff",
    // Speech and other lossy codecs
    "amr", "awb", "mp2", "mp1", "m4b", "m4r", "oga", "spx", "mpc", "ra", "weba", "gsm", "aa3", "oma",
];

/// How many leading [`VIDEO_EXTENSIONS`] entries the former C# app accepted.
pub const CSHARP_VIDEO_COUNT: usize = 14;
/// How many leading [`AUDIO_EXTENSIONS`] entries the former C# app accepted.
pub const CSHARP_AUDIO_COUNT: usize = 8;

const MEDIA_LEN: usize = VIDEO_EXTENSIONS.len() + AUDIO_EXTENSIONS.len();
const CSHARP_LEN: usize = CSHARP_VIDEO_COUNT + CSHARP_AUDIO_COUNT;

/// Video then audio (macOS `MediaFileTypes.mediaExtensions` order; the macOS companion search order).
pub const MEDIA_EXTENSIONS: [&str; MEDIA_LEN] = concat(VIDEO_EXTENSIONS, VIDEO_EXTENSIONS.len(), AUDIO_EXTENSIONS, AUDIO_EXTENSIONS.len());

/// The former C# app's 22 (video then audio), which the Windows file associations keep registering: the additions are
/// not associated, see `sa_platform::win::associations`.
pub const CSHARP_MEDIA_EXTENSIONS: [&str; CSHARP_LEN] =
    concat(VIDEO_EXTENSIONS, CSHARP_VIDEO_COUNT, AUDIO_EXTENSIONS, CSHARP_AUDIO_COUNT);

/// Windows companion-video probe order (C# `SubtitleParser.VideoExtensions`): `mp4`, `mkv`, then the rest of
/// [`MEDIA_EXTENSIONS`] in order.
pub const COMPANION_PROBE_ORDER: [&str; MEDIA_LEN] = promote(MEDIA_EXTENSIONS, &["mp4", "mkv"]);

/// The first `a_len` of `a`, then the first `b_len` of `b` (`N` must be `a_len + b_len`).
const fn concat<const N: usize>(a: &[&'static str], a_len: usize, b: &[&'static str], b_len: usize) -> [&'static str; N] {
    assert!(a_len <= a.len() && b_len <= b.len() && a_len + b_len == N);
    let mut out = [""; N];
    let mut i = 0;
    while i < a_len {
        out[i] = a[i];
        i += 1;
    }
    let mut j = 0;
    while j < b_len {
        out[a_len + j] = b[j];
        j += 1;
    }
    out
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn contains(list: &[&str], item: &str) -> bool {
    let mut i = 0;
    while i < list.len() {
        if str_eq(list[i], item) {
            return true;
        }
        i += 1;
    }
    false
}

/// `first` (each must be in `list`), then the remaining entries of `list` in order.
const fn promote<const N: usize>(list: [&'static str; N], first: &[&'static str]) -> [&'static str; N] {
    let mut out = [""; N];
    let mut n = 0;
    while n < first.len() {
        assert!(contains(&list, first[n]));
        out[n] = first[n];
        n += 1;
    }
    let mut i = 0;
    while i < N {
        if !contains(first, list[i]) {
            out[n] = list[i];
            n += 1;
        }
        i += 1;
    }
    assert!(n == N);
    out
}

/// `ext` (lower-case, no dot) is a video extension.
pub fn is_video_extension(ext: &str) -> bool {
    VIDEO_EXTENSIONS.contains(&ext)
}

/// `ext` (lower-case, no dot) is an audio-only extension.
pub fn is_audio_extension(ext: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&ext)
}

/// `ext` (lower-case, no dot) is a video or audio extension.
pub fn is_media_extension(ext: &str) -> bool {
    is_video_extension(ext) || is_audio_extension(ext)
}

/// The display name of the open dialog's media filter (the pattern list itself is too long to show).
pub fn media_filter_label() -> String {
    crate::l!("Video ve ses dosyaları")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_lowercase_ascii_without_dots_and_without_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for ext in MEDIA_EXTENSIONS {
            assert!(!ext.is_empty() && !ext.starts_with('.'), "{ext:?}");
            assert!(ext.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'), "{ext:?}");
            assert!(seen.insert(ext), "duplicate {ext:?}");
        }
        for ext in crate::subtitle_import::EXTENSIONS {
            assert!(!is_media_extension(&ext[1..]), "{ext} is a subtitle extension");
        }
    }

    #[test]
    fn derived_lists_keep_the_csharp_order_first() {
        let csharp_video = ["mp4", "mov", "m4v", "mkv", "avi", "webm", "wmv", "mpg", "mpeg", "ts", "mts", "m2ts", "3gp", "flv"];
        let csharp_audio = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma"];
        assert_eq!(VIDEO_EXTENSIONS[..CSHARP_VIDEO_COUNT], csharp_video);
        assert_eq!(AUDIO_EXTENSIONS[..CSHARP_AUDIO_COUNT], csharp_audio);
        assert_eq!(CSHARP_MEDIA_EXTENSIONS.to_vec(), [&csharp_video[..], &csharp_audio[..]].concat());
        assert_eq!(MEDIA_EXTENSIONS.to_vec(), [VIDEO_EXTENSIONS, AUDIO_EXTENSIONS].concat());
        // C# `SubtitleParser.VideoExtensions` probed mp4, mkv, mov, m4v, … (the rest in the media order).
        assert_eq!(COMPANION_PROBE_ORDER[..5], ["mp4", "mkv", "mov", "m4v", "avi"]);
        let mut a = COMPANION_PROBE_ORDER.to_vec();
        let mut b = MEDIA_EXTENSIONS.to_vec();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);
        assert!(is_video_extension("vob") && !is_audio_extension("vob"));
        assert!(is_audio_extension("mka") && is_media_extension("mka") && !is_video_extension("mka"));
        assert!(!is_media_extension("MP4"), "callers lower-case first");
    }
}
