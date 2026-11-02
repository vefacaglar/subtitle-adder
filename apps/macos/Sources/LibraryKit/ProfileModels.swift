import Foundation

/// A user's engagement with one local video: views, favorite, stars and the resume point. Stored in the profile's
/// encrypted vault; the JSON field names are shared with the Windows app (`lastWatchedAtUtc`, `updatedAtUtc`).
public struct VideoUserStats: Codable, Sendable, Equatable, Hashable {
    /// `fp:<length>:<24 hex>` (see `VideoFingerprint`); `nil` while the file is missing or unreadable.
    public var contentFingerprint: String?
    public var videoPath: String
    public var fileName: String
    public var directoryPath: String
    public var viewCount: Int
    /// The favorite flag.
    public var isLiked: Bool
    /// 0 (none) to 5.
    public var rating: Int
    public var lastPositionSeconds: Double
    public var durationSeconds: Double
    public var lastWatchedAt: Date?
    public var updatedAt: Date
    /// Not part of the stored record (thumbnails live in a separate map of the vault); the store fills it in when it
    /// hands a value out.
    public var thumbnail: Data?

    enum CodingKeys: String, CodingKey {
        case contentFingerprint, videoPath, fileName, directoryPath, viewCount, isLiked, rating
        case lastPositionSeconds, durationSeconds
        case lastWatchedAt = "lastWatchedAtUtc"
        case updatedAt = "updatedAtUtc"
    }

    public init(
        contentFingerprint: String? = nil, videoPath: String, fileName: String, directoryPath: String,
        viewCount: Int = 0, isLiked: Bool = false, rating: Int = 0, lastPositionSeconds: Double = 0,
        durationSeconds: Double = 0, lastWatchedAt: Date? = nil, updatedAt: Date, thumbnail: Data? = nil
    ) {
        self.contentFingerprint = contentFingerprint
        self.videoPath = videoPath
        self.fileName = fileName
        self.directoryPath = directoryPath
        self.viewCount = viewCount
        self.isLiked = isLiked
        self.rating = rating
        self.lastPositionSeconds = lastPositionSeconds
        self.durationSeconds = durationSeconds
        self.lastWatchedAt = lastWatchedAt
        self.updatedAt = updatedAt
        self.thumbnail = thumbnail
    }

    /// Reads leniently: only `videoPath` is required, so a vault written by another build with fewer fields still opens.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let path = try c.decode(String.self, forKey: .videoPath)
        contentFingerprint = try c.decodeIfPresent(String.self, forKey: .contentFingerprint)
        videoPath = path
        fileName = try c.decodeIfPresent(String.self, forKey: .fileName) ?? ""
        directoryPath = try c.decodeIfPresent(String.self, forKey: .directoryPath) ?? ""
        viewCount = try c.decodeIfPresent(Int.self, forKey: .viewCount) ?? 0
        isLiked = try c.decodeIfPresent(Bool.self, forKey: .isLiked) ?? false
        rating = try c.decodeIfPresent(Int.self, forKey: .rating) ?? 0
        lastPositionSeconds = try c.decodeIfPresent(Double.self, forKey: .lastPositionSeconds) ?? 0
        durationSeconds = try c.decodeIfPresent(Double.self, forKey: .durationSeconds) ?? 0
        lastWatchedAt = try c.decodeIfPresent(Date.self, forKey: .lastWatchedAt)
        updatedAt = try c.decodeIfPresent(Date.self, forKey: .updatedAt) ?? Date(timeIntervalSince1970: 0)
        thumbnail = nil
    }

    public var url: URL { URL(fileURLWithPath: videoPath) }

    public var hasWatched: Bool { viewCount > 0 || lastWatchedAt != nil }

    /// Resume position over duration, 0...1 (0 while the duration is unknown).
    public var progressRatio: Double {
        guard durationSeconds > 0, durationSeconds.isFinite, lastPositionSeconds.isFinite else { return 0 }
        return min(max(lastPositionSeconds / durationSeconds, 0), 1)
    }

    /// A never-watched record for a path (what a discovered file looks like in the feed before it is touched).
    static func placeholder(path: String, now: Date, fingerprint: String?) -> VideoUserStats {
        VideoUserStats(
            contentFingerprint: fingerprint, videoPath: path, fileName: PathParts.fileName(of: path),
            directoryPath: PathParts.directory(of: path), updatedAt: now)
    }
}

/// A named collection of videos a user made in their profile. `videoPaths` keeps insertion order.
public struct UserVideoList: Codable, Sendable, Equatable, Hashable, Identifiable {
    /// A UUID as 32 lowercase hex digits without dashes.
    public var id: String
    public var name: String
    public var videoPaths: [String]
    public var createdAt: Date
    public var updatedAt: Date

    enum CodingKeys: String, CodingKey {
        case id, name, videoPaths
        case createdAt = "createdAtUtc"
        case updatedAt = "updatedAtUtc"
    }

    public init(id: String, name: String, videoPaths: [String] = [], createdAt: Date, updatedAt: Date) {
        self.id = id
        self.name = name
        self.videoPaths = videoPaths
        self.createdAt = createdAt
        self.updatedAt = updatedAt
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        videoPaths = try c.decodeIfPresent([String].self, forKey: .videoPaths) ?? []
        createdAt = try c.decodeIfPresent(Date.self, forKey: .createdAt) ?? Date(timeIntervalSince1970: 0)
        updatedAt = try c.decodeIfPresent(Date.self, forKey: .updatedAt) ?? createdAt
    }
}

/// Which slice of the library the feed shows.
public enum VideoFeedFilter: Sendable, Equatable, Hashable {
    /// Everything the profile knows plus the discovered files, watched or liked ones first.
    case all
    case history
    case liked
    case mostWatched
    case topRated
    /// The list with this id, in the list's own order.
    case customList(String)
}

/// Counts over the active profile's records.
public struct ProfileTotals: Sendable, Equatable {
    /// Videos with at least one view.
    public var watched: Int
    /// Sum of all view counts.
    public var views: Int
    public var liked: Int
    /// Videos with a rating above 0.
    public var rated: Int

    public init(watched: Int, views: Int, liked: Int, rated: Int) {
        self.watched = watched
        self.views = views
        self.liked = liked
        self.rated = rated
    }

    public static let zero = ProfileTotals(watched: 0, views: 0, liked: 0, rated: 0)
}

/// Tells a view model to refresh. Progress-only and thumbnail-only writes never raise it locally.
public enum ProfileChange: Sendable, Equatable {
    /// This store changed something that is worth a redraw (login state, a like, a list, ...).
    case local
    /// `profiles.json` was changed by another instance or process and this store picked it up.
    case external
}

/// Pieces of a path, as plain strings (the stored paths are strings, not URLs).
enum PathParts {
    static func fileName(of path: String) -> String { (path as NSString).lastPathComponent }
    static func directory(of path: String) -> String {
        let parent = (path as NSString).deletingLastPathComponent
        return parent == path ? "" : parent
    }
}
