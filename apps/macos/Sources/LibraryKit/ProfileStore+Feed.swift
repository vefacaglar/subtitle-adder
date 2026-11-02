import Foundation

extension ProfileStore {
    /// The library as a user sees it: the profile's records, the files just `discovered` on disk and the videos of
    /// its lists, filtered, ordered and narrowed by `search` (see `VideoUserStats.matches(search:)`).
    ///
    /// Discovered files that were never touched appear as default records and are not persisted. Discovered files that
    /// moved or were renamed are followed first (history, likes, list entries and the thumbnail go with them): the
    /// result reflects the move at once, and it is saved with the next write or `persistPendingMigrations()`; a feed
    /// never writes. While logged out only `.all` returns anything: the discovered files, by name.
    ///
    /// Following moves needs each discovered file's fingerprint, which reads up to 192 KB of a file the first time
    /// (afterwards one `stat`, cached for the process): with `fingerprints: .compute`, the default, run this from a
    /// background task. For a refresh on the main actor pass `.cachedOnly` (one `stat` per file) or `.pathOnly` (no
    /// file access at all; files are matched to records by path only, so a moved file shows as new until a `.compute`
    /// call has followed it). The usual pattern is `.pathOnly` at once, then `.compute` in the background.
    ///
    /// Ordering, ties broken by file name (ignoring case):
    /// - `.history`: watched; last watched (or updated) first, then view count.
    /// - `.liked`: favorites; most recently updated, then view count.
    /// - `.mostWatched`: view count, then last watched.
    /// - `.topRated`: rating, then view count, then update time.
    /// - `.all`: watched, liked or rated first, then last watched, then view count.
    /// - `.customList`: the list's own order.
    public func feed(
        _ filter: VideoFeedFilter = .all, search: String? = nil, discovered: [URL] = [],
        fingerprints: FingerprintMode = .compute
    ) -> [VideoUserStats] {
        var seen = Set<String>()
        let found = discovered.compactMap { VideoProbe($0, mode: fingerprints) }.filter { seen.insert($0.pathKey).inserted }
        let probes = prepared(found)
        let now = clock()
        if let contents = readMigrating(probes) {
            return FeedBuilder.narrowed(FeedBuilder.items(in: contents, filter: filter, probes: probes, now: now), by: search)
        }
        guard filter == .all else { return [] }
        let defaults = probes.map { VideoUserStats.placeholder(path: $0.path, now: now, fingerprint: $0.fingerprint) }
        return FeedBuilder.narrowed(defaults.sorted(by: FeedBuilder.byName), by: search)
    }
}

enum FeedBuilder {
    static func items(in contents: VaultContents, filter: VideoFeedFilter, probes: [VideoProbe], now: Date) -> [VideoUserStats] {
        // Every known video by path: records, then discovered files, then list entries nothing else covers.
        var merged: [String: VideoUserStats] = [:]
        for (key, video) in contents.videos where !video.videoPath.isEmpty {
            merged[video.videoPath.lowercased()] = contents.stats(forKey: key)
        }
        for probe in probes where merged[probe.pathKey] == nil {
            merged[probe.pathKey] = .placeholder(path: probe.path, now: now, fingerprint: probe.fingerprint)
        }
        for list in contents.customLists {
            for path in list.videoPaths where merged[path.lowercased()] == nil {
                merged[path.lowercased()] = .placeholder(path: path, now: now, fingerprint: nil)
            }
        }

        switch filter {
        case .customList(let id):
            guard let index = contents.listIndex(id: id) else { return [] }
            return contents.customLists[index].videoPaths.compactMap { merged[$0.lowercased()] }
        case .history:
            return merged.values.filter(\.hasWatched).sorted {
                ordered($0.lastWatchedAt ?? $0.updatedAt, $1.lastWatchedAt ?? $1.updatedAt, descending: true)
                    ?? ordered($0.viewCount, $1.viewCount, descending: true) ?? byName($0, $1)
            }
        case .liked:
            return merged.values.filter(\.isLiked).sorted {
                ordered($0.updatedAt, $1.updatedAt, descending: true)
                    ?? ordered($0.viewCount, $1.viewCount, descending: true) ?? byName($0, $1)
            }
        case .mostWatched:
            return merged.values.filter { $0.viewCount > 0 }.sorted {
                ordered($0.viewCount, $1.viewCount, descending: true)
                    ?? ordered($0.lastWatchedAt ?? $0.updatedAt, $1.lastWatchedAt ?? $1.updatedAt, descending: true)
                    ?? byName($0, $1)
            }
        case .topRated:
            return merged.values.filter { $0.rating > 0 }.sorted {
                ordered($0.rating, $1.rating, descending: true)
                    ?? ordered($0.viewCount, $1.viewCount, descending: true)
                    ?? ordered($0.updatedAt, $1.updatedAt, descending: true) ?? byName($0, $1)
            }
        case .all:
            return merged.values.sorted {
                ordered($0.isEngaged ? 1 : 0, $1.isEngaged ? 1 : 0, descending: true)
                    ?? ordered($0.lastWatchedAt ?? .distantPast, $1.lastWatchedAt ?? .distantPast, descending: true)
                    ?? ordered($0.viewCount, $1.viewCount, descending: true) ?? byName($0, $1)
            }
        }
    }

    static func narrowed(_ items: [VideoUserStats], by search: String?) -> [VideoUserStats] {
        guard let search, !search.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return items }
        return items.filter { $0.matches(search: search) }
    }

    /// File name ignoring case, then path, so the order never depends on dictionary iteration.
    static func byName(_ a: VideoUserStats, _ b: VideoUserStats) -> Bool {
        let result = a.fileName.localizedCaseInsensitiveCompare(b.fileName)
        if result != .orderedSame { return result == .orderedAscending }
        return a.videoPath.lowercased() < b.videoPath.lowercased()
    }

    /// `nil` when equal, so comparators chain with `??`.
    private static func ordered<T: Comparable>(_ a: T, _ b: T, descending: Bool) -> Bool? {
        guard a != b else { return nil }
        return descending ? a > b : a < b
    }
}

private extension VideoUserStats {
    /// Watched, liked or rated: what sorts first in the "all" feed.
    var isEngaged: Bool { lastWatchedAt != nil || isLiked || rating > 0 }
}
