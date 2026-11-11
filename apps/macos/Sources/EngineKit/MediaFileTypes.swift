import Foundation

/// The video and audio containers the app offers to open and index (what the engine's decoder can read). Extensions
/// are lower-case without the dot, like `SubtitleParser.extensions`; matching ignores case.
public enum MediaFileTypes {
    /// The original 14 first (their order is the companion search order), then the containers added with the Rust
    /// desktop app's list (`apps/desktop/crates/sa-core/src/media_types.rs`, keep the two in sync). What AVFoundation
    /// cannot play goes through the engine's `preview`.
    public static let videoExtensions = [
        "mp4", "mov", "m4v", "mkv", "avi", "webm", "wmv", "mpg", "mpeg", "ts", "mts", "m2ts", "3gp", "flv",
        "vob", "m2v", "m1v", "mpe", "mpv", "mp2v", "m2t", "tp", "trp", "mod", "tod",
        "ogv", "ogm", "mk3d", "asf", "divx", "xvid", "rm", "rmvb", "f4v",
        "mxf", "dv", "gxf", "wtv", "dvr-ms",
        "3g2", "3gp2", "mj2", "qt", "mpg4", "mp4v",
        "nut", "amv", "y4m", "ivf", "h264", "264", "h265", "265", "hevc", "nsv", "bik", "smk", "roq", "swf",
    ]

    /// The original 8 first, then the additions (same source as `videoExtensions`).
    public static let audioExtensions = [
        "mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma",
        "mka", "ac3", "eac3", "dts", "dtshd", "thd", "truehd", "mlp",
        "aif", "aiff", "aifc", "ape", "wv", "tta", "tak", "w64", "caf", "au", "snd", "voc", "dsf", "dff",
        "amr", "awb", "mp2", "mp1", "m4b", "m4r", "oga", "spx", "mpc", "ra", "weba", "gsm", "aa3", "oma",
    ]

    /// Video first, then audio; also the order in which `SubtitleParser.findCompanionVideo(for:)` tries them.
    public static let mediaExtensions = videoExtensions + audioExtensions

    private static let mediaSet = Set(mediaExtensions)
    private static let audioSet = Set(audioExtensions)

    /// Whether `url` has a video or audio extension.
    public static func isMedia(_ url: URL) -> Bool {
        mediaSet.contains(url.pathExtension.lowercased())
    }

    /// Whether `url` has an audio-only extension (no picture to show).
    public static func isAudio(_ url: URL) -> Bool {
        audioSet.contains(url.pathExtension.lowercased())
    }
}
