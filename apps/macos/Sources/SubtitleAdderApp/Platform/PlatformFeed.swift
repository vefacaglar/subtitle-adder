import EngineKit
import Foundation
import LibraryKit

/// What the video platform asks of the system, replaceable in tests.
struct PlatformServices: Sendable {
    /// Whether the volume at `/Volumes/<name>` is mounted. Only asked about paths below `/Volumes`.
    var isVolumeMounted: @Sendable (String) -> Bool
    var directoryExists: @Sendable (String) -> Bool
    /// Whether the file itself is there (asked only once its folder is known to exist).
    var fileExists: @Sendable (String) -> Bool
    /// The file names in a folder; empty if it cannot be listed.
    var listDirectory: @Sendable (String) -> [String]
    /// How many indexed files "Keşfet" and the search take from the index.
    var discoveredLimit = 500
    /// How often the playback position is reported while a video plays.
    var progressInterval: Duration = .seconds(5)

    static let system = PlatformServices(
        isVolumeMounted: ExplorerServices.system.isVolumeMounted,
        directoryExists: ExplorerServices.system.directoryExists,
        fileExists: { FileManager.default.fileExists(atPath: $0) },
        listDirectory: { path in (try? FileManager.default.contentsOfDirectory(atPath: path)) ?? [] })
}

/// Whether a video's storage is there. A video on a disk that is unplugged, in a folder that is gone or deleted from
/// its folder is hidden from every feed until it is back, instead of showing as a card that cannot play. Cheap on
/// purpose: it never opens the file, and asks about each volume, each folder and each file once per instance (one feed
/// build).
struct StorageAvailability {
    private let services: PlatformServices
    private var volumes: [String: Bool] = [:]
    private var folders: [String: Bool] = [:]
    private var files: [String: Bool] = [:]

    init(services: PlatformServices) {
        self.services = services
    }

    mutating func isAvailable(_ path: String) -> Bool {
        guard !path.isEmpty, let folder = ExplorerPaths.parent(of: path) else { return !path.isEmpty }
        guard isFolderAvailable(folder, of: path) else { return false }
        if let known = files[path] { return known }
        let exists = services.fileExists(path)
        files[path] = exists
        return exists
    }

    private mutating func isFolderAvailable(_ folder: String, of path: String) -> Bool {
        if let known = folders[folder] { return known }
        var available = true
        if let root = ExplorerPaths.volumeRoot(of: path) {
            if volumes[root] == nil { volumes[root] = services.isVolumeMounted(root) }
            available = volumes[root] == true
        }
        if available { available = services.directoryExists(folder) }
        folders[folder] = available
        return available
    }
}

/// The subtitle badge of each video, from one directory listing per folder. One instance serves all the builds of one
/// refresh (the quick one and the one that follows moved files), so a folder is listed once per refresh, not once per
/// card and not once per build. Used from one store queue at a time.
final class SubtitleBadges: @unchecked Sendable {
    private let listDirectory: @Sendable (String) -> [String]
    private let lock = NSLock()
    private var lookups: [String: SubtitleParser.SubtitleBadgeLookup] = [:]
    private var listed = 0
    /// How many folders were listed (what the cache saved shows in tests).
    var listings: Int { lock.withLock { listed } }

    init(listDirectory: @escaping @Sendable (String) -> [String]) {
        self.listDirectory = listDirectory
    }

    func badge(forVideoAt path: String) -> String? {
        guard let folder = ExplorerPaths.parent(of: path) else { return nil }
        let key = folder.lowercased()
        let lookup: SubtitleParser.SubtitleBadgeLookup = lock.withLock {
            if let known = lookups[key] { return known }
            listed += 1
            let made = SubtitleParser.SubtitleBadgeLookup(fileNames: listDirectory(folder))
            lookups[key] = made
            return made
        }
        let stem = ((path as NSString).lastPathComponent as NSString).deletingPathExtension
        return lookup.badgeExtension(forVideoStem: stem)
    }
}

/// Counts over the videos that are visible (their storage is available).
struct ProfileCounts: Equatable, Sendable {
    var watched = 0
    var liked = 0
    var rated = 0
    var views = 0

    mutating func add(_ stats: VideoUserStats) {
        if stats.hasWatched { watched += 1 }
        if stats.isLiked { liked += 1 }
        if stats.rating > 0 { rated += 1 }
        views += stats.viewCount
    }
}

/// The open video's engagement, for the bar on the player.
struct EngagementSnapshot: Equatable, Sendable {
    let url: URL
    var views: Int
    var isLiked: Bool
    var rating: Int
    var listIDs: Set<String>
}

/// What one refresh asks for.
struct FeedRequest: Sendable {
    var filter: VideoFeedFilter
    var search: String
    /// Paths of the indexed files to merge into "Keşfet" and the search.
    var discoveredPaths: [String]
    /// The hub is on screen: build the cards too (the header, lists and the open video are always built).
    var wantsFeed: Bool
    /// The video in the player.
    var playerVideo: URL?
    /// How far finding moved or renamed files goes. `.pathOnly` is quick (no file is opened) and is built first; the
    /// `.compute` pass that follows reads file slices the first time and follows the moves.
    var fingerprints: FingerprintMode = .compute
}

/// The result of one refresh. Built off the main actor, applied on it.
struct PlatformSnapshot: Sendable {
    var username: String?
    var registeredUsernames: [String] = []
    var lastUsername: String?
    var rememberMe = true
    var counts = ProfileCounts()
    var lists: [UserVideoList] = []
    /// How many videos of each list (by list id) are visible: their storage is available.
    var listCounts: [String: Int] = [:]
    /// The store's session generation when this was read; a different one at apply time means the snapshot belongs to
    /// a login that is over.
    var sessionGeneration = 0
    var engagement: EngagementSnapshot?
    /// `nil` when the feed was not asked for or the tab shows the sign-in card instead of videos.
    var cards: [VideoCard]?
    /// Folders listed for subtitle badges while building (tests).
    var listings = 0
}

enum PlatformFeedBuilder {
    /// Every store read of a refresh, in one pass. Runs on a store queue, never on the main actor.
    static func snapshot(
        store: ProfileStore, request: FeedRequest, services: PlatformServices, badges: SubtitleBadges? = nil,
        seedThumbnail: (@Sendable (String, Data) -> Void)? = nil, shouldContinue: (@Sendable () -> Bool)? = nil
    ) -> PlatformSnapshot {
        var availability = StorageAvailability(services: services)
        var snapshot = PlatformSnapshot(username: store.currentUsername)
        // After the first read, which is the one that restores a remembered login.
        snapshot.sessionGeneration = store.sessionGeneration
        snapshot.registeredUsernames = store.registeredUsernames
        snapshot.lastUsername = store.lastUsername
        snapshot.rememberMe = store.rememberMe
        let loggedIn = snapshot.username != nil
        // Logged out only the indexed files can be listed, by name: no file is opened to follow a move.
        let fingerprints: FingerprintMode = loggedIn ? request.fingerprints : .pathOnly

        if loggedIn {
            // One pass over the profile's own records gives the pill counts of what is visible.
            for stats in store.feed(.all) where availability.isAvailable(stats.videoPath) {
                snapshot.counts.add(stats)
            }
        }
        if let video = request.playerVideo, loggedIn {
            // Before the lists: looking the video up follows it if it moved, and the lists follow with it.
            let stats = store.stats(for: video)
            snapshot.engagement = EngagementSnapshot(
                url: video, views: stats?.viewCount ?? 0, isLiked: stats?.isLiked ?? false,
                rating: min(max(stats?.rating ?? 0, 0), 5), listIDs: Set(store.lists(containing: video).map(\.id)))
        }
        if loggedIn {
            snapshot.lists = store.lists()
            for list in snapshot.lists {
                snapshot.listCounts[list.id] = list.videoPaths.filter { availability.isAvailable($0) }.count
            }
        }

        guard request.wantsFeed else { return snapshot }

        // Logged out only the indexed videos can be shown; a personal tab then shows the sign-in card, unless the user
        // is searching, which looks through the indexed videos.
        let searching = !request.search.isEmpty
        let filter: VideoFeedFilter
        if loggedIn {
            filter = request.filter
        } else if request.filter == .all || searching {
            filter = .all
        } else {
            return snapshot
        }

        let discovered: [URL] = filter == .all
            ? request.discoveredPaths.filter { availability.isAvailable($0) }.map { URL(filePath: $0, directoryHint: .notDirectory) }
            : []
        let items = store.feed(
            filter, search: searching ? request.search : nil, discovered: discovered, fingerprints: fingerprints,
            shouldContinue: shouldContinue
        ).filter { availability.isAvailable($0.videoPath) }
        // Stopped: nobody wants this result any more.
        if let shouldContinue, !shouldContinue() { return snapshot }
        // A move that the feed followed is in memory; write it now rather than with some later write. Playback progress
        // that is pending is not a reason to rewrite the vault on every refresh (it has its own timer).
        if fingerprints == .compute, store.hasUnsavedMigrations { _ = try? store.persistPendingMigrations() }

        var listsByPath: [String: [VideoCard.ListRef]] = [:]
        for list in snapshot.lists {
            let ref = VideoCard.ListRef(id: list.id, name: list.name)
            for path in list.videoPaths { listsByPath[path.lowercased(), default: []].append(ref) }
        }
        let badges = badges ?? SubtitleBadges(listDirectory: services.listDirectory)
        snapshot.cards = items.map { stats in
            // The stored picture goes straight into the thumbnail cache; the card itself stays small.
            if let thumbnail = stats.thumbnail { seedThumbnail?(stats.videoPath, thumbnail) }
            return VideoCard(
                stats: stats, subtitleExtension: badges.badge(forVideoAt: stats.videoPath),
                lists: listsByPath[stats.videoPath.lowercased()] ?? [])
        }
        snapshot.listings = badges.listings
        return snapshot
    }
}
