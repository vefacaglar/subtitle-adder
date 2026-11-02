import Foundation

/// The video and audio containers the app offers to open and index (what the engine's decoder can read). Extensions
/// are lower-case without the dot, like `SubtitleParser.extensions`; matching ignores case.
public enum MediaFileTypes {
    public static let videoExtensions = [
        "mp4", "mov", "m4v", "mkv", "avi", "webm", "wmv", "mpg", "mpeg", "ts", "mts", "m2ts", "3gp", "flv",
    ]

    public static let audioExtensions = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma"]

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
