import Foundation

/// Custom lists of the unlocked profile. List names are trimmed and unique ignoring case and diacritics, so
/// "daha sonra izle" is the same name as "Daha Sonra İzle". Videos keep the order they were added in.
extension ProfileStore {
    /// Creates an empty list. Returns `nil` while logged out; throws `.emptyListName` for a blank name and
    /// `.duplicateListName` if the profile already has a list with that name.
    @discardableResult
    public func createList(named name: String) throws(ProfileError) -> UserVideoList? {
        let clean = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !clean.isEmpty else { throw .emptyListName }
        return try transact(fallback: nil) { (state: inout ProfileState) throws(ProfileError) -> (UserVideoList?, ProfileEffect) in
            let folded = TextFolding.fold(clean)
            let taken = state.session!.contents.customLists.contains(where: { TextFolding.fold($0.name) == folded })
            if taken { throw .duplicateListName }
            let now = clock()
            let list = UserVideoList(
                id: UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased(), name: clean, createdAt: now,
                updatedAt: now)
            state.session!.contents.customLists.append(list)
            return (list, .vault)
        }
    }

    /// Renames a list (a case-only change is fine). `false` while logged out or for an unknown id; throws
    /// `.emptyListName` or `.duplicateListName` like `createList`.
    @discardableResult
    public func renameList(id: String, to name: String) throws(ProfileError) -> Bool {
        let clean = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !clean.isEmpty else { throw .emptyListName }
        return try transact(fallback: false) { (state: inout ProfileState) throws(ProfileError) -> (Bool, ProfileEffect) in
            guard let index = state.session!.contents.listIndex(id: id) else { return (false, []) }
            let folded = TextFolding.fold(clean)
            let lists = state.session!.contents.customLists
            let taken = lists.enumerated().contains(where: { $0.offset != index && TextFolding.fold($0.element.name) == folded })
            if taken { throw .duplicateListName }
            state.session!.contents.customLists[index].name = clean
            state.session!.contents.customLists[index].updatedAt = clock()
            return (true, .vault)
        }
    }

    @discardableResult
    public func deleteList(id: String) throws(ProfileError) -> Bool {
        try transact(fallback: false) { state in
            guard let index = state.session!.contents.listIndex(id: id) else { return (false, []) }
            state.session!.contents.customLists.remove(at: index)
            return (true, .vault)
        }
    }

    /// Adds a video to a list; adding one that is already in it succeeds without changing anything. The video also
    /// gets a record so a later move or rename can be followed. `false` for an unknown list or while logged out.
    @discardableResult
    public func addVideo(_ video: URL, toList id: String) throws(ProfileError) -> Bool {
        guard let probe = VideoProbe(video) else { return false }
        return try transact(fallback: false) { state in
            guard state.session!.contents.listIndex(id: id) != nil else { return (false, []) }
            let now = clock()
            let (_, _, recordChanged) = state.session!.contents.resolveOrCreate(probe, now: now)
            // Resolving may have rewritten list paths, so look the list up again.
            guard let index = state.session!.contents.listIndex(id: id) else { return (false, []) }
            if state.session!.contents.customLists[index].videoPaths.contains(where: { $0.lowercased() == probe.pathKey }) {
                return (true, recordChanged ? .vault : [])
            }
            state.session!.contents.customLists[index].videoPaths.append(probe.path)
            state.session!.contents.customLists[index].updatedAt = now
            return (true, .vault)
        }
    }

    /// Removes a video from a list. `false` if it was not in it (or the list is unknown, or nobody is logged in).
    @discardableResult
    public func removeVideo(_ video: URL, fromList id: String) throws(ProfileError) -> Bool {
        guard let probe = VideoProbe(video) else { return false }
        return try transact(fallback: false) { state in
            let now = clock()
            let migrated = state.session!.contents.resolve(probe, now: now)?.migrated == true
            guard let index = state.session!.contents.listIndex(id: id) else { return (false, migrated ? [.vault, .silent] : []) }
            let before = state.session!.contents.customLists[index].videoPaths
            let after = before.filter { $0.lowercased() != probe.pathKey }
            guard after.count != before.count else { return (false, migrated ? [.vault, .silent] : []) }
            state.session!.contents.customLists[index].videoPaths = after
            state.session!.contents.customLists[index].updatedAt = now
            return (true, .vault)
        }
    }

    /// Adds the video if it is not in the list, removes it if it is. Returns whether it is in the list now (`false` for
    /// an unknown list or while logged out).
    @discardableResult
    public func toggleVideo(_ video: URL, inList id: String) throws(ProfileError) -> Bool {
        guard let probe = VideoProbe(video) else { return false }
        return try transact(fallback: false) { state in
            guard state.session!.contents.listIndex(id: id) != nil else { return (false, []) }
            let now = clock()
            _ = state.session!.contents.resolveOrCreate(probe, now: now)
            guard let index = state.session!.contents.listIndex(id: id) else { return (false, []) }
            var paths = state.session!.contents.customLists[index].videoPaths
            let isIn: Bool
            if let position = paths.firstIndex(where: { $0.lowercased() == probe.pathKey }) {
                paths.remove(at: position)
                isIn = false
            } else {
                paths.append(probe.path)
                isIn = true
            }
            state.session!.contents.customLists[index].videoPaths = paths
            state.session!.contents.customLists[index].updatedAt = now
            return (isIn, .vault)
        }
    }

    /// All lists of the unlocked profile in creation order (empty while logged out).
    public func lists() -> [UserVideoList] {
        read { $0.session?.contents.customLists ?? [] }
    }

    /// The lists that contain the video, after following the video if it moved.
    public func lists(containing video: URL) -> [UserVideoList] {
        guard let probe = VideoProbe(video) else { return [] }
        return readMigrating([probe]) { session in
            session.contents.customLists.filter { $0.videoPaths.contains { $0.lowercased() == probe.pathKey } }
        } ?? []
    }

    /// Whether the video is in the list, after following the video if it moved.
    public func isVideo(_ video: URL, inList id: String) -> Bool {
        guard let probe = VideoProbe(video) else { return false }
        return readMigrating([probe]) { session in
            guard let index = session.contents.listIndex(id: id) else { return false }
            return session.contents.customLists[index].videoPaths.contains { $0.lowercased() == probe.pathKey }
        } ?? false
    }
}
