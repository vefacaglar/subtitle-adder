import Foundation

/// Everything here acts on the unlocked profile. While logged out the calls do nothing and return `nil`, `false`,
/// zero or an empty list; they throw only when a real write fails.
extension ProfileStore {
    /// Counts a view: `viewCount + 1`, `lastWatchedAt` now, and the resume point. A `position` or `duration` of 0 (or
    /// any non-finite value) keeps the stored one; a position past the duration is cut to it.
    @discardableResult
    public func recordWatch(_ video: URL, position: Double = 0, duration: Double = 0) throws(ProfileError) -> VideoUserStats? {
        guard let probe = VideoProbe(video) else { return nil }
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

    /// Stores the resume point and duration without counting a view. Saved only when the position or duration moved by
    /// half a second or more since the last save (smaller steps change nothing), and never raises
    /// `ProfileChange.local`, so a player can call it every second without redrawing anything.
    @discardableResult
    public func updatePlaybackProgress(_ video: URL, position: Double, duration: Double) throws(ProfileError) -> VideoUserStats? {
        guard let probe = VideoProbe(video) else { return nil }

        // Fast path: nothing to save, so no file lock and no write.
        let unchanged: VideoUserStats?? = read { state in
            guard var contents = state.session?.contents else { return .some(nil) }
            let now = clock()
            let resolution = contents.resolve(probe, now: now)
            let existing = resolution?.stats ?? VideoUserStats.placeholder(path: probe.path, now: now, fingerprint: probe.fingerprint)
            let (cleanPosition, cleanDuration) = Self.progress(position, duration, existing: existing)
            if resolution?.migrated != true, !Self.isSignificant(cleanPosition, cleanDuration, existing: existing) {
                return .some(resolution.flatMap { state.session?.contents.stats(forKey: $0.key) } ?? existing)
            }
            return .none
        }
        if let unchanged { return unchanged }

        return try transact(fallback: nil) { state in
            let now = clock()
            let (key, existing, changed) = state.session!.contents.resolveOrCreate(probe, now: now)
            let (cleanPosition, cleanDuration) = Self.progress(position, duration, existing: existing)
            guard Self.isSignificant(cleanPosition, cleanDuration, existing: existing) else {
                return (state.session!.contents.stats(forKey: key), changed ? [.vault, .silent] : [])
            }
            var updated = existing
            updated.lastPositionSeconds = cleanPosition
            updated.durationSeconds = cleanDuration
            updated.updatedAt = now
            state.session!.contents.setVideo(updated, forKey: key)
            return (state.session!.contents.stats(forKey: key), [.vault, .silent])
        }
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

    /// The stats of a file, following it first if it was moved or renamed (see `VideoFingerprint`). `nil` while
    /// logged out or if the profile has never touched the file.
    public func stats(for video: URL) -> VideoUserStats? {
        guard let probe = VideoProbe(video) else { return nil }
        return readMigrating([probe]) { session in
            session.contents.find(probe).flatMap { session.contents.stats(forKey: $0) }
        }.flatMap { $0 }
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
            return (true, .vault)
        }
    }

    // MARK: - Thumbnails

    /// Stores an image for a file inside the encrypted vault (never as a plaintext file). Images over 512 KB, empty
    /// ones and ones for a blank path are ignored. Returns whether the image is now stored; never raises
    /// `ProfileChange.local`.
    @discardableResult
    public func setThumbnail(_ data: Data, for video: URL) throws(ProfileError) -> Bool {
        guard !data.isEmpty, data.count <= VaultContents.maxThumbnailBytes, let probe = VideoProbe(video) else { return false }
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
    public func thumbnail(for video: URL) -> Data? {
        guard let probe = VideoProbe(video) else { return nil }
        return read { $0.session?.contents.thumbnail(for: probe) }
    }

    // MARK: - Helpers

    /// One record edit: find or create the file's record, change it, stamp `updatedAt`, save.
    private func update(_ video: URL, _ change: (inout VideoUserStats) -> Void) throws(ProfileError) -> VideoUserStats? {
        guard let probe = VideoProbe(video) else { return nil }
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
