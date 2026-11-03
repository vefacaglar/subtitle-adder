import Foundation
import Testing
@testable import LibraryKit

/// Following a moved file while reading changes the result and the in-memory profile, never the file; the move is saved
/// with the next write or an explicit `persistPendingMigrations()`.
@Suite final class ProfileReadsNeverWriteTests {
    let folder = TestFolder()
    let secrets = InMemorySecretStore()

    private func makeStore() -> ProfileStore { folder.makeStore(secrets: secrets) }

    private func fileData() throws -> Data { try Data(contentsOf: folder.profilesFile) }

    private func content(_ seed: UInt8) -> Data { Data((0..<3000).map { UInt8(($0 &+ Int(seed)) % 247) }) }

    /// A profile with a watched, liked file in a list, which is then renamed on disk.
    private func movedSetup() throws -> (store: ProfileStore, original: URL, moved: URL, list: UserVideoList) {
        let store = makeStore()
        try store.register(username: "emre", password: "pw")
        let original = folder.video("clip.mp4")
        try content(1).write(to: original)
        try store.recordWatch(original, position: 7, duration: 70)
        try store.setLiked(true, for: original)
        let list = try #require(try store.createList(named: "Liste"))
        try store.addVideo(original, toList: list.id)
        let moved = folder.video("renamed.mp4")
        try FileManager.default.moveItem(at: original, to: moved)
        return (store, original, moved, list)
    }

    @Test func listsContainingAJustMovedVideoFindItBeforeAnythingElseLooked() throws {
        let (store, _, moved, list) = try movedSetup()
        let saved = try fileData()
        #expect(store.lists(containing: moved).map(\.id) == [list.id])
        #expect(store.lists().first?.videoPaths == [normalized(moved)])
        #expect(try fileData() == saved)
    }

    @Test func isVideoInListFindsAJustMovedVideo() throws {
        let (store, _, moved, list) = try movedSetup()
        #expect(store.isVideo(moved, inList: list.id))
    }

    @Test func statsOfAMovedFileReflectTheMoveWithoutWritingIt() throws {
        let (store, original, moved, _) = try movedSetup()
        let saved = try fileData()
        #expect(!store.hasUnsavedChanges)

        let stats = try #require(store.stats(for: moved))
        #expect(stats.videoPath == normalized(moved))
        #expect(stats.isLiked)
        #expect(stats.viewCount == 1)
        #expect(stats.lastPositionSeconds == 7)
        #expect(store.stats(for: original) == nil || store.stats(for: original)?.videoPath == normalized(moved))
        #expect(try fileData() == saved, "a read never writes")
        #expect(store.hasUnsavedChanges)
    }

    @Test func aFeedFollowsMovedFilesInTheResultButNotInTheFile() throws {
        let (store, _, moved, list) = try movedSetup()
        let saved = try fileData()
        let feed = store.feed(.customList(list.id), discovered: [moved])
        #expect(feed.map(\.videoPath) == [normalized(moved)])
        #expect(feed.first?.isLiked == true)
        #expect(store.feed(.liked, discovered: [moved]).map(\.videoPath) == [normalized(moved)])
        #expect(try fileData() == saved)
        #expect(store.hasUnsavedChanges)
    }

    @Test func persistPendingMigrationsWritesTheFollowedMoveOnce() throws {
        let (store, original, moved, list) = try movedSetup()
        let other = makeStore()
        #expect(other.isLoggedIn)
        _ = store.stats(for: moved)

        // Nobody else sees the move before it is written.
        #expect(other.lists().first?.videoPaths == [normalized(original)])

        let saved = try fileData()
        #expect(try store.persistPendingMigrations())
        #expect(!store.hasUnsavedChanges)
        #expect(try fileData() != saved)
        let written = try fileData()
        #expect(try store.persistPendingMigrations() == false)
        #expect(try fileData() == written)

        #expect(other.lists().first?.videoPaths == [normalized(moved)])
        #expect(other.lists(containing: moved).map(\.id) == [list.id])
        #expect(!other.hasUnsavedChanges)
        let reopened = makeStore()
        #expect(reopened.stats(for: moved)?.isLiked == true)
        #expect(!reopened.hasUnsavedChanges, "already migrated, nothing to follow")
    }

    @Test func aMoveFoundWhileReadingRidesAlongWithTheNextWrite() throws {
        let (store, _, moved, list) = try movedSetup()
        _ = store.lists(containing: moved)
        #expect(store.hasUnsavedChanges)

        try store.setRating(3, for: folder.video("unrelated.mp4"))
        #expect(!store.hasUnsavedChanges)
        let reopened = makeStore()
        #expect(reopened.lists().first?.videoPaths == [normalized(moved)])
        #expect(reopened.feed(.customList(list.id)).map(\.videoPath) == [normalized(moved)])
    }

    @Test func aMoveNotYetWrittenIsFoundAgainAfterAnotherInstancesWrite() throws {
        let (store, _, moved, _) = try movedSetup()
        let other = makeStore()
        _ = store.stats(for: moved)
        try other.setRating(2, for: folder.video("elsewhere.mp4"))

        // This store reloads the other instance's vault, which still has the old path, and follows the file again.
        #expect(store.lists(containing: moved).count == 1)
        #expect(store.stats(for: moved)?.isLiked == true)
    }

    // MARK: - Fingerprint modes

    @Test func pathOnlyLooksFilesUpByPathAndNeverFollowsAMove() throws {
        let (store, _, moved, _) = try movedSetup()
        let stay = folder.video("stay.mp4")
        try content(5).write(to: stay)
        try store.recordWatch(stay)
        let saved = try fileData()

        #expect(store.stats(for: moved, fingerprints: .pathOnly) == nil)
        #expect(store.lists(containing: moved, fingerprints: .pathOnly).isEmpty)
        let placeholders = store.feed(.all, discovered: [moved], fingerprints: .pathOnly)
        let shown = try #require(placeholders.first { $0.videoPath == normalized(moved) })
        #expect(shown.viewCount == 0 && !shown.isLiked && shown.contentFingerprint == nil)
        #expect(!store.hasUnsavedChanges)
        #expect(try fileData() == saved)

        // A file that stayed where it was is found by its path alone.
        #expect(store.stats(for: stay, fingerprints: .pathOnly)?.viewCount == 1)

        // The full lookup, run in the background, then follows the move.
        #expect(store.stats(for: moved)?.isLiked == true)
        #expect(store.hasUnsavedChanges)
    }

    @Test func cachedOnlyUsesWhatAnEarlierFullLookupFingerprintedAndReadsNothingNew() throws {
        let store = makeStore()
        try store.register(username: "emre", password: "pw")
        let original = folder.video("shared.mp4")
        try content(9).write(to: original)
        try store.recordWatch(original, position: 3, duration: 30)
        let copy = folder.video("shared-copy.mp4")
        try FileManager.default.copyItem(at: original, to: copy)

        // The copy was never fingerprinted: with the cache alone it is just another path.
        #expect(store.stats(for: copy, fingerprints: .cachedOnly) == nil)
        // A background pass fingerprints it ...
        #expect(VideoFingerprint.fingerprint(ofNormalizedPath: normalized(copy)) != nil)
        // ... and from then on the cheap lookup recognizes it by content.
        let shared = try #require(store.stats(for: copy, fingerprints: .cachedOnly))
        #expect(shared.viewCount == 1)
        #expect(shared.videoPath == normalized(original), "a copy shares the record, it does not take the path")
    }
}
