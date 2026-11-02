import EngineKit
import Foundation
import LibraryKit

/// Everything a feed card shows, as a plain value that is built off the main actor. Nothing here touches the disk or
/// the profile store: the subtitle badge, the list memberships and the storage check were done by the builder.
struct VideoCard: Identifiable, Equatable, Sendable {
    /// A custom list the video is in.
    struct ListRef: Equatable, Hashable, Sendable {
        let id: String
        let name: String
    }

    /// The normalized path the profile store knows the video by.
    let path: String
    let url: URL
    let fileName: String
    /// The file name without its extension (the whole name when that would leave nothing).
    let title: String
    /// Upper-case extension without the dot ("MP4").
    let extensionLabel: String
    let directoryPath: String
    var viewCount: Int
    var isLiked: Bool
    var rating: Int
    let durationSeconds: Double
    /// Resume position over duration, 0...1.
    let progress: Double
    let lastWatchedAt: Date?
    /// Lower-case extension of the subtitle file the folder offers for the video ("srt"), `nil` without one.
    let subtitleExtension: String?
    var lists: [ListRef]
    let isAudio: Bool

    var id: String { path }

    init(stats: VideoUserStats, subtitleExtension: String?, lists: [ListRef]) {
        path = stats.videoPath
        url = stats.url
        fileName = stats.fileName.isEmpty ? PathText.name(of: stats.videoPath) : stats.fileName
        let stem = (fileName as NSString).deletingPathExtension
        title = stem.isEmpty ? fileName : stem
        extensionLabel = (fileName as NSString).pathExtension.uppercased()
        directoryPath = stats.directoryPath
        viewCount = stats.viewCount
        isLiked = stats.isLiked
        rating = min(max(stats.rating, 0), 5)
        durationSeconds = stats.durationSeconds
        progress = stats.progressRatio
        lastWatchedAt = stats.lastWatchedAt
        self.subtitleExtension = subtitleExtension
        self.lists = lists
        isAudio = MediaFileTypes.isAudio(stats.url)
    }

    var hasProgress: Bool { progress > 0.01 }
    var hasSubtitle: Bool { subtitleExtension != nil }
    var hasWatched: Bool { viewCount > 0 || lastWatchedAt != nil }

    /// "Klasör (/tam/yol)"; the bare path for a folder without a name.
    var folderLine: String { PathText.folderLine(directoryPath) }

    /// "3 görüntüleme" and "Son izleme: …" for a watched video, a single "Henüz izlenmedi" for one that never was.
    var watchBadges: [String] {
        guard hasWatched else { return [L("Henüz izlenmedi")] }
        var badges: [String] = []
        if viewCount > 0 { badges.append(L("%lld görüntüleme", viewCount)) }
        if let lastWatchedAt { badges.append(L("Son izleme: %@", PathText.dateText(lastWatchedAt))) }
        return badges
    }

    var subtitleChipText: String {
        guard let subtitleExtension else { return L("Altyazı yok") }
        return L("Altyazılı") + " (" + subtitleExtension.uppercased() + ")"
    }

    var durationText: String { PathText.durationText(durationSeconds) }
}

/// Small text helpers shared by the cards and the player bar.
enum PathText {
    static func name(of path: String) -> String { (path as NSString).lastPathComponent }

    /// "Folder (full path)", or the bare path when the folder has no name of its own.
    static func folderLine(_ directoryPath: String) -> String {
        var trimmed = directoryPath
        while trimmed.count > 1, trimmed.hasSuffix("/") { trimmed.removeLast() }
        guard !trimmed.isEmpty else { return "" }
        let name = (trimmed as NSString).lastPathComponent
        return name.isEmpty || name == trimmed ? trimmed : "\(name) (\(trimmed))"
    }

    /// "4:07" or "1:02:03"; empty for an unknown duration.
    static func durationText(_ seconds: Double) -> String {
        guard seconds.isFinite, seconds > 0 else { return "" }
        let total = Int(seconds.rounded())
        let hours = total / 3600, minutes = total % 3600 / 60, secs = total % 60
        return hours > 0 ? String(format: "%d:%02d:%02d", hours, minutes, secs) : String(format: "%d:%02d", minutes, secs)
    }

    static func dateText(_ date: Date) -> String {
        date.formatted(Date.FormatStyle(date: .abbreviated, time: .shortened).locale(LanguageState.shared.language.locale))
    }
}
