import Foundation

/// A video as the store looks it up: its normalized path and, if the file is readable, its content fingerprint.
/// Both are computed before any lock is taken, so a slow disk never stalls other callers.
struct VideoProbe: Sendable {
    let path: String
    let pathKey: String
    let fingerprint: String?

    init?(_ url: URL) {
        let path = VideoFingerprint.normalizedPath(url)
        guard !path.isEmpty else { return nil }
        self.path = path
        pathKey = path.lowercased()
        fingerprint = VideoFingerprint.fingerprint(ofNormalizedPath: path)
    }
}

/// The decrypted vault of the active profile: records keyed by fingerprint (or by path while the file has none),
/// custom lists and thumbnails. Keys are lowercased, matching the case-insensitive dictionaries of the Windows app.
///
/// Two small indexes (fingerprint to key, path to key) keep lookups O(1) while a feed resolves thousands of files.
struct VaultContents: Sendable {
    private(set) var videos: [String: VideoUserStats] = [:]
    var customLists: [UserVideoList] = []
    private(set) var thumbnails: [String: Data] = [:]
    private var keyByFingerprint: [String: String] = [:]
    private var keyByPath: [String: String] = [:]

    /// The largest image the vault accepts (matches the cap the Windows app now applies).
    static let maxThumbnailBytes = 512 * 1024

    init() {}

    /// Builds the in-memory form from a decrypted payload, repairing what a hand-edited or older vault may get wrong
    /// (ratings out of range, negative counts, empty file names, duplicate list entries). Paths are kept as stored.
    init(payload: VaultPayload) {
        for var video in payload.videos {
            let path = video.videoPath.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !path.isEmpty else { continue }
            var fingerprint = video.contentFingerprint?.trimmingCharacters(in: .whitespacesAndNewlines)
            if fingerprint?.isEmpty ?? true { fingerprint = VideoFingerprint.fingerprint(ofNormalizedPath: path) }
            video.contentFingerprint = fingerprint
            video.videoPath = path
            if video.fileName.trimmingCharacters(in: .whitespaces).isEmpty { video.fileName = PathParts.fileName(of: path) }
            if video.directoryPath.isEmpty { video.directoryPath = PathParts.directory(of: path) }
            video.rating = min(max(video.rating, 0), 5)
            video.viewCount = max(video.viewCount, 0)
            setVideo(video, forKey: fingerprint ?? path)
        }
        for var list in payload.customLists {
            let name = list.name.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !list.id.trimmingCharacters(in: .whitespaces).isEmpty, !name.isEmpty else { continue }
            var seen = Set<String>()
            list.name = name
            list.videoPaths = list.videoPaths
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                .filter { !$0.isEmpty && seen.insert($0.lowercased()).inserted }
            customLists.append(list)
        }
        for (key, encoded) in payload.thumbnails {
            if let data = Data(base64Encoded: encoded), !data.isEmpty { thumbnails[key.lowercased()] = data }
        }
    }

    /// The payload to encrypt: records sorted by path, thumbnails base64 encoded.
    func payload() -> VaultPayload {
        VaultPayload(
            videos: videos.values.sorted { $0.videoPath.lowercased() < $1.videoPath.lowercased() },
            customLists: customLists,
            thumbnails: thumbnails.mapValues { $0.base64EncodedString() })
    }

    // MARK: - Records

    static func key(for text: String) -> String { text.lowercased() }

    mutating func setVideo(_ stats: VideoUserStats, forKey key: String) {
        let key = Self.key(for: key)
        if let old = videos[key] { removeFromIndexes(old, key: key) }
        var stored = stats
        stored.thumbnail = nil
        videos[key] = stored
        if let fingerprint = stored.contentFingerprint, !fingerprint.isEmpty { keyByFingerprint[fingerprint.lowercased()] = key }
        keyByPath[stored.videoPath.lowercased()] = key
    }

    @discardableResult
    mutating func removeVideo(forKey key: String) -> VideoUserStats? {
        let key = Self.key(for: key)
        guard let old = videos.removeValue(forKey: key) else { return nil }
        removeFromIndexes(old, key: key)
        return old
    }

    private mutating func removeFromIndexes(_ stats: VideoUserStats, key: String) {
        if let fingerprint = stats.contentFingerprint?.lowercased(), keyByFingerprint[fingerprint] == key {
            keyByFingerprint[fingerprint] = nil
        }
        let path = stats.videoPath.lowercased()
        if keyByPath[path] == key { keyByPath[path] = nil }
    }

    /// The record with its thumbnail attached.
    func stats(forKey key: String) -> VideoUserStats? {
        let key = Self.key(for: key)
        guard var stats = videos[key] else { return nil }
        stats.thumbnail = thumbnails[key]
        return stats
    }

    /// The key of the record that stands for this file: by fingerprint first, then by path.
    func find(_ probe: VideoProbe) -> String? {
        if let fingerprint = probe.fingerprint.map(Self.key(for:)) {
            if videos[fingerprint] != nil { return fingerprint }
            if let key = keyByFingerprint[fingerprint] { return key }
        }
        if videos[probe.pathKey] != nil { return probe.pathKey }
        return keyByPath[probe.pathKey]
    }

    struct Resolution {
        let key: String
        let stats: VideoUserStats
        /// The record's path, key or fingerprint was updated (the file was moved, renamed or became fingerprintable).
        let migrated: Bool
    }

    /// Finds the record for a file and, if the file moved or was renamed, follows it: the record takes the new path,
    /// name and folder, is re-keyed by fingerprint together with its thumbnail, and every custom list swaps the old
    /// path for the new one (deduplicated). A file whose content is also still at the recorded path is a copy, not a
    /// move, and does not take the record. Nothing is created.
    mutating func resolve(_ probe: VideoProbe, now: Date) -> Resolution? {
        guard let foundKey = find(probe), let found = videos[foundKey] else { return nil }
        let fingerprintKey = probe.fingerprint.map(Self.key(for:))
        let canonicalKey = fingerprintKey ?? found.contentFingerprint.map(Self.key(for:)) ?? foundKey

        let pathChanged = found.videoPath.lowercased() != probe.pathKey
        // The recorded path still holds the same content: this is a second copy (or a link), not a move. Both paths
        // share the record and neither may take it from the other, or every refresh would flip it back and forth.
        if pathChanged, let fingerprint = probe.fingerprint, !found.videoPath.isEmpty,
            VideoFingerprint.fingerprint(ofNormalizedPath: found.videoPath) == fingerprint
        {
            return Resolution(key: foundKey, stats: found, migrated: false)
        }
        let keyChanged = foundKey != canonicalKey
        let fingerprintAdded = fingerprintKey != nil && found.contentFingerprint?.lowercased() != fingerprintKey
        guard pathChanged || keyChanged || fingerprintAdded else { return Resolution(key: foundKey, stats: found, migrated: false) }

        var updated = found
        updated.contentFingerprint = probe.fingerprint ?? found.contentFingerprint
        updated.videoPath = probe.path
        updated.fileName = PathParts.fileName(of: probe.path)
        updated.directoryPath = PathParts.directory(of: probe.path)
        if keyChanged {
            removeVideo(forKey: foundKey)
            if let thumbnail = thumbnails.removeValue(forKey: foundKey) { thumbnails[canonicalKey] = thumbnail }
        }
        setVideo(updated, forKey: canonicalKey)
        if pathChanged, !found.videoPath.isEmpty { replaceListPath(found.videoPath, with: probe.path, now: now) }
        return Resolution(key: canonicalKey, stats: updated, migrated: true)
    }

    /// The record for a file, creating a default one if there is none yet. `changed` tells whether that created or
    /// migrated a record (and so needs saving).
    mutating func resolveOrCreate(_ probe: VideoProbe, now: Date) -> (key: String, stats: VideoUserStats, changed: Bool) {
        if let resolution = resolve(probe, now: now) { return (resolution.key, resolution.stats, resolution.migrated) }
        let key = Self.key(for: probe.fingerprint ?? probe.path)
        let created = VideoUserStats.placeholder(path: probe.path, now: now, fingerprint: probe.fingerprint)
        setVideo(created, forKey: key)
        return (key, created, true)
    }

    private mutating func replaceListPath(_ old: String, with new: String, now: Date) {
        let oldKey = old.lowercased()
        for index in customLists.indices where customLists[index].videoPaths.contains(where: { $0.lowercased() == oldKey }) {
            var seen = Set<String>()
            customLists[index].videoPaths = customLists[index].videoPaths
                .map { $0.lowercased() == oldKey ? new : $0 }
                .filter { seen.insert($0.lowercased()).inserted }
            customLists[index].updatedAt = now
        }
    }

    // MARK: - Thumbnails

    /// The thumbnail stored for a file (by its record, then by fingerprint, then by path); never migrates anything.
    func thumbnail(for probe: VideoProbe) -> Data? {
        if let key = find(probe), let data = thumbnails[key] { return data }
        if let fingerprint = probe.fingerprint, let data = thumbnails[Self.key(for: fingerprint)] { return data }
        return thumbnails[probe.pathKey]
    }

    mutating func setThumbnail(_ data: Data, forKey key: String) {
        thumbnails[Self.key(for: key)] = data
    }

    @discardableResult
    mutating func removeThumbnail(forKey key: String) -> Data? {
        thumbnails.removeValue(forKey: Self.key(for: key))
    }

    // MARK: - Lists

    func listIndex(id: String) -> Int? {
        let id = id.lowercased()
        return customLists.firstIndex { $0.id.lowercased() == id }
    }

    // MARK: - Comparison

    /// Whether a view that shows this vault would look different from one showing `other`: any list or record change
    /// except resume position, duration and the update time (which the player rewrites constantly), list timestamps
    /// and thumbnails. Instants are compared to the millisecond because a value that went through the file has only
    /// 100 ns of precision.
    func differsVisibly(from other: VaultContents) -> Bool {
        guard videos.count == other.videos.count, customLists.count == other.customLists.count else { return true }
        for (mine, theirs) in zip(customLists, other.customLists) {
            if mine.id != theirs.id || mine.name != theirs.name || mine.videoPaths != theirs.videoPaths { return true }
        }
        for (key, mine) in videos {
            guard let theirs = other.videos[key] else { return true }
            var a = mine
            var b = theirs
            for record in [\VideoUserStats.lastPositionSeconds, \.durationSeconds] {
                a[keyPath: record] = 0
                b[keyPath: record] = 0
            }
            a.updatedAt = .distantPast
            b.updatedAt = .distantPast
            if let x = a.lastWatchedAt, let y = b.lastWatchedAt, abs(x.timeIntervalSince(y)) < 0.001 { b.lastWatchedAt = x }
            if a != b { return true }
        }
        return false
    }
}
