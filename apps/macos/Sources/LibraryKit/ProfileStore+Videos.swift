import Foundation

/// Everything here acts on the unlocked profile. While logged out the calls do nothing and return `nil`, `false`,
/// zero or an empty list; they throw only when a real write fails.
///
/// Every method takes a file URL and fingerprints the file (disk I/O, see `ProfileStore`): call them from a
/// background task. The reading ones take `fingerprints:` to stay cheap on the main actor.
extension ProfileStore {
    /// Counts a view: `viewCount + 1`, `lastWatchedAt` now, and the resume point. A `position` or `duration` of 0 (or
    /// any non-finite value) keeps the stored one; a position past the duration is cut to it. Written at once, together
    /// with any progress and moves still held in memory.
    @discardableResult
    public func recordWatch(_ video: URL, position: Double = 0, duration: Double = 0) throws(ProfileError) -> VideoUserStats? {
        guard let probe = lookupProbe(for: video) else { return nil }
        return try transact(fallback: nil) { state in
            let now = clock()
            let (key, existing, _) = state.session!.contents.resolveOrCreate(probe, now: now)
            let cleanDuration = Self.sanitizedSeconds(duration, fallback: existing.durationSeconds)
            var cleanPosition = Self.sanitizedSeconds(position, fallback: existing.lastPositionSeconds)
            if cleanDuration > 0, cleanPosition > cleanDuration { cleanPosition = cleanDuration }

            var updated = existing
            updated.viewCount += 1
            updated.lastPositionSeconds = cleanPosition
            updated.durationSeconds = cleanDuration
            updated.lastWatchedAt = now
            updated.updatedAt = now
            state.session!.contents.setVideo(updated, forKey: key)
            return (state.session!.contents.stats(forKey: key), .vault)
        }
    }

    /// Stores the resume point and duration without counting a view, and never raises `ProfileChange.local`, so a
    /// player can call it every second without redrawing anything.
    ///
    /// The change is applied in memory at once (`stats(for:)` shows it) when the position or duration moved by half a
    /// second or more since the last one (smaller steps change nothing). It is *written* at most every
    /// `progressSaveInterval` seconds, because each write encrypts the whole vault, thumbnails included: call
    /// `flushPlaybackProgress()` on pause, seek, close and quit to put the latest point on disk. Another process sees
    /// progress only after it is written.
    @discardableResult
    public func updatePlaybackProgress(_ video: URL, position: Double, duration: Double) throws(ProfileError) -> VideoUserStats? {
        guard let probe = lookupProbe(for: video) else { return nil }
        syncAndRestore()

        // In memory only, under the in-process lock: no file lock, no write.
        let applied: VideoUserStats?? = lock.withLock {
            guard state.session != nil else { return .some(nil) }
            let now = clock()
            let resolution = state.session!.contents.resolve(probe, now: now)
            if resolution?.migrated == true { state.session!.dirty = true }
            let existing = resolution?.stats ?? VideoUserStats.placeholder(path: probe.path, now: now, fingerprint: probe.fingerprint)
            let (cleanPosition, cleanDuration) = Self.progress(position, duration, existing: existing)
            guard Self.isSignificant(cleanPosition, cleanDuration, existing: existing) else {
                return .some(resolution.flatMap { state.session!.contents.stats(forKey: $0.key) } ?? existing)
            }

            let (key, current, _) = state.session!.contents.resolveOrCreate(probe, now: now)
            var updated = current
            updated.lastPositionSeconds = cleanPosition
            updated.durationSeconds = cleanDuration
            updated.updatedAt = now
            state.session!.contents.setVideo(updated, forKey: key)
            state.session!.dirty = true
            state.session!.pendingProgress[key] = PendingProgress(probe: probe, position: cleanPosition, duration: cleanDuration, at: now)
            return .some(state.session!.contents.stats(forKey: key))
        }
        guard let applied else { return nil }

        // Throttled write of whatever is pending (this progress, earlier progress, followed moves).
        try persistDirty(minInterval: Self.progressSaveInterval)
        return applied
    }

    /// Writes the playback progress (and followed moves) that is held in memory, now, whatever the throttle says. Call
    /// it when playback pauses, seeks or stops, when a window closes and on quit. Does nothing (and returns `false`)
    /// when there is nothing to write. Never raises `ProfileChange.local`. Disk I/O: call it off the main actor, or
    /// from the termination path where blocking is fine.
    @discardableResult
    public func flushPlaybackProgress() throws(ProfileError) -> Bool {
        try persistDirty(minInterval: 0)
    }

    /// Writes the moves of renamed or moved files that reads (`stats(for:)`, `feed`, `lists(containing:)`) followed in
    /// memory, together with any playback progress. A background task can call it after a refresh that followed files;
    /// otherwise they are saved with the next write. Returns whether anything was written.
    @discardableResult
    public func persistPendingMigrations() throws(ProfileError) -> Bool {
        try persistDirty(minInterval: 0)
    }

    /// Flips the favorite flag and returns the new state (`false` while logged out).
    @discardableResult
    public func toggleLike(_ video: URL) throws(ProfileError) -> Bool {
        try update(video) { $0.isLiked.toggle() }?.isLiked ?? false
    }

    @discardableResult
    public func setLiked(_ liked: Bool, for video: URL) throws(ProfileError) -> VideoUserStats? {
        try update(video) { $0.isLiked = liked }
    }

    /// Sets the stars, clamped to 0...5; 0 clears the rating.
    @discardableResult
    public func setRating(_ rating: Int, for video: URL) throws(ProfileError) -> VideoUserStats? {
        try update(video) { $0.rating = min(max(rating, 0), 5) }
    }

    /// The stats of a file, following it first if it was moved or renamed (see `VideoFingerprint`; the move is applied
    /// in memory and saved later, see `ProfileStore`). `nil` while logged out or if the profile has never touched the
    /// file. `fingerprints` decides how much disk access that costs.
    public func stats(for video: URL, fingerprints: FingerprintMode = .compute) -> VideoUserStats? {
        guard let probe = lookupProbe(for: video, mode: fingerprints) else { return nil }
        guard let contents = readMigrating([probe]) else { return nil }
        return contents.find(probe).flatMap { contents.stats(forKey: $0) }
    }

    /// Resets the watch history: records that are not liked, not rated and in no list are dropped (with their
    /// thumbnails); the others keep their favorite, rating and list membership but lose views, last-watched time and
    /// resume position. Returns `false` while logged out.
    @discardableResult
    public func clearWatchHistory() throws(ProfileError) -> Bool {
        try transact(fallback: false) { state in
            let now = clock()
            var contents = state.session!.contents
            let listed = Set(contents.customLists.flatMap(\.videoPaths).map { $0.lowercased() })
            for (key, video) in contents.videos {
                if !video.isLiked, video.rating <= 0, !listed.contains(video.videoPath.lowercased()) {
                    contents.removeVideo(forKey: key)
                    contents.removeThumbnail(forKey: key)
                } else {
                    var reset = video
                    reset.viewCount = 0
                    reset.lastWatchedAt = nil
                    reset.lastPositionSeconds = 0
                    reset.updatedAt = now
                    contents.setVideo(reset, forKey: key)
                }
            }
            state.session!.contents = contents
            state.session!.pendingProgress = [:]
            return (true, .vault)
        }
    }

    // MARK: - Thumbnails

    /// Stores an image for a file inside the encrypted vault (never as a plaintext file). Images over 512 KB, empty
    /// ones and ones for a blank path are ignored. Returns whether the image is now stored; never raises
    /// `ProfileChange.local`.
    @discardableResult
    public func setThumbnail(_ data: Data, for video: URL) throws(ProfileError) -> Bool {
        guard !data.isEmpty, data.count <= VaultContents.maxThumbnailBytes, let probe = lookupProbe(for: video) else { return false }
        return try transact(fallback: false) { state in
            let now = clock()
            let (key, _, changed) = state.session!.contents.resolveOrCreate(probe, now: now)
            let unchanged = state.session!.contents.stats(forKey: key)?.thumbnail == data
            if unchanged, !changed { return (true, []) }
            state.session!.contents.setThumbnail(data, forKey: key)
            return (true, [.vault, .silent])
        }
    }

    /// The stored image for a file, if any. Does not follow moved files (a following `stats(for:)` does).
    public func thumbnail(for video: URL, fingerprints: FingerprintMode = .compute) -> Data? {
        guard let probe = VideoProbe(video, mode: fingerprints) else { return nil }
        return read { $0.session?.contents.thumbnail(for: probe) }
    }

    // MARK: - Helpers

    /// One record edit: find or create the file's record, change it, stamp `updatedAt`, save.
    private func update(_ video: URL, _ change: (inout VideoUserStats) -> Void) throws(ProfileError) -> VideoUserStats? {
        guard let probe = lookupProbe(for: video) else { return nil }
        return try transact(fallback: nil) { state in
            let now = clock()
            let (key, existing, _) = state.session!.contents.resolveOrCreate(probe, now: now)
            var updated = existing
            change(&updated)
            updated.updatedAt = now
            state.session!.contents.setVideo(updated, forKey: key)
            return (state.session!.contents.stats(forKey: key), .vault)
        }
    }

    private static func sanitizedSeconds(_ candidate: Double, fallback: Double) -> Double {
        if candidate.isFinite, candidate > 0 { return candidate }
        return fallback.isFinite && fallback > 0 ? fallback : 0
    }

    /// Position (0 is a valid reset here) and duration for a progress update, cut so the position never passes the duration.
    private static func progress(_ position: Double, _ duration: Double, existing: VideoUserStats) -> (Double, Double) {
        let cleanDuration = sanitizedSeconds(duration, fallback: existing.durationSeconds)
        var cleanPosition = max(0, position.isFinite ? position : existing.lastPositionSeconds)
        if cleanDuration > 0, cleanPosition > cleanDuration { cleanPosition = cleanDuration }
        return (cleanPosition, cleanDuration)
    }

    private static func isSignificant(_ position: Double, _ duration: Double, existing: VideoUserStats) -> Bool {
        abs(position - existing.lastPositionSeconds) >= 0.5 || abs(duration - existing.durationSeconds) >= 0.5
    }
}
