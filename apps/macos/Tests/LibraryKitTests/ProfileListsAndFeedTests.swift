import Foundation
import Testing
@testable import LibraryKit

@Suite final class ProfileListsAndFeedTests {
    let folder = TestFolder()
    let clock = ManualClock()

    private func loggedInStore() throws -> ProfileStore {
        let store = folder.makeStore(clock: { [clock] in clock.date })
        try store.register(username: "emre", password: "pw", rememberMe: false)
        return store
    }

    // MARK: - Lists

    @Test func listNamesAreTrimmedAndUniqueIgnoringCaseAndDiacritics() throws {
        let store = try loggedInStore()
        let first = try #require(try store.createList(named: "  Daha Sonra İzle  "))
        #expect(first.name == "Daha Sonra İzle")
        #expect(first.id.count == 32 && first.id == first.id.lowercased() && !first.id.contains("-"))

        for duplicate in ["daha sonra izle", "DAHA SONRA İZLE", "Daha Sonra Izle", "daha sonra izlé"] {
            #expect(throws: ProfileError.duplicateListName, "\(duplicate)") { try store.createList(named: duplicate) }
        }
        #expect(throws: ProfileError.emptyListName) { try store.createList(named: "   ") }
        #expect(try store.createList(named: "Başka") != nil)
        #expect(store.lists().map(\.name) == ["Daha Sonra İzle", "Başka"])
    }

    @Test func renamingChecksTheOtherListsButAllowsACaseOnlyChange() throws {
        let store = try loggedInStore()
        let a = try #require(try store.createList(named: "Alpha"))
        let b = try #require(try store.createList(named: "Beta"))
        #expect(throws: ProfileError.duplicateListName) { try store.renameList(id: b.id, to: " alpha ") }
        #expect(throws: ProfileError.emptyListName) { try store.renameList(id: b.id, to: "") }
        #expect(try store.renameList(id: a.id, to: "ALPHA"))
        #expect(try store.renameList(id: b.id, to: "Gamma"))
        #expect(try store.renameList(id: "no-such-id", to: "x") == false)
        #expect(store.lists().map(\.name) == ["ALPHA", "Gamma"])
        #expect(try store.renameList(id: a.id.uppercased(), to: "Delta"), "ids match ignoring case")
    }

    @Test func addingIsIdempotentOrderIsKeptAndTogglingAndRemovingWork() throws {
        let store = try loggedInStore()
        let list = try #require(try store.createList(named: "L"))
        let v1 = folder.video("1.mp4"), v2 = folder.video("2.mp4"), v3 = folder.video("3.mp4")

        #expect(try store.addVideo(v2, toList: list.id))
        #expect(try store.addVideo(v1, toList: list.id))
        #expect(try store.addVideo(v2, toList: list.id), "adding again succeeds")
        #expect(try store.addVideo(v3, toList: list.id))
        #expect(store.lists().first?.videoPaths == [normalized(v2), normalized(v1), normalized(v3)])
        #expect(store.feed(.customList(list.id)).map(\.fileName) == ["2.mp4", "1.mp4", "3.mp4"])

        #expect(try store.removeVideo(v1, fromList: list.id))
        #expect(try store.removeVideo(v1, fromList: list.id) == false)
        #expect(try store.toggleVideo(v1, inList: list.id), "toggled back in")
        #expect(try store.toggleVideo(v1, inList: list.id) == false)
        #expect(store.isVideo(v2, inList: list.id))
        #expect(!store.isVideo(v1, inList: list.id))
        #expect(store.lists(containing: v3).map(\.id) == [list.id])
        #expect(store.lists(containing: v1).isEmpty)

        #expect(try store.addVideo(v1, toList: "unknown") == false)
        #expect(try store.toggleVideo(v1, inList: "unknown") == false)
        #expect(try store.deleteList(id: list.id))
        #expect(try store.deleteList(id: list.id) == false)
        #expect(store.lists().isEmpty)
        #expect(store.feed(.customList(list.id)).isEmpty)
    }

    @Test func aMovedVideoReplacesItsOldPathInEveryList() throws {
        let store = try loggedInStore()
        let original = folder.video("clip.mp4")
        try Data((0..<2000).map { UInt8($0 % 251) }).write(to: original)
        let one = try #require(try store.createList(named: "One"))
        let two = try #require(try store.createList(named: "Two"))
        try store.addVideo(original, toList: one.id)
        try store.addVideo(original, toList: two.id)
        let moved = folder.video("moved.mp4")
        try FileManager.default.moveItem(at: original, to: moved)

        #expect(Set(store.lists(containing: moved).map(\.name)) == ["One", "Two"])
        #expect(store.lists().map(\.videoPaths) == [[normalized(moved)], [normalized(moved)]])
        #expect(store.lists(containing: original).isEmpty)
    }

    @Test func migrationDeduplicatesAListThatAlreadyHadTheNewPath() throws {
        let file = folder.video("here.mp4")
        try Data((0..<3000).map { UInt8($0 % 241) }).write(to: file)
        let probe = try #require(VideoProbe(file))
        let oldPath = normalized(folder.video("old/there.mp4"))
        let now = Date()

        var contents = VaultContents()
        contents.setVideo(
            VideoUserStats(
                contentFingerprint: probe.fingerprint, videoPath: oldPath, fileName: "there.mp4",
                directoryPath: PathParts.directory(of: oldPath), viewCount: 3, updatedAt: now),
            forKey: probe.fingerprint!)
        contents.customLists = [
            UserVideoList(id: "a", name: "A", videoPaths: ["/x", oldPath, probe.path, "/y"], createdAt: now, updatedAt: now),
            UserVideoList(id: "b", name: "B", videoPaths: [probe.path, oldPath.uppercased()], createdAt: now, updatedAt: now),
        ]
        let resolved = contents.resolve(probe, now: now)
        let resolution = try #require(resolved)
        #expect(resolution.migrated)
        #expect(resolution.stats.videoPath == probe.path)
        #expect(resolution.stats.viewCount == 3)
        #expect(contents.customLists[0].videoPaths == ["/x", probe.path, "/y"])
        #expect(contents.customLists[1].videoPaths == [probe.path])
        // A second look finds it in place.
        let again = contents.resolve(probe, now: now)
        #expect(again?.migrated == false)
    }

    @Test func aCopyOfAWatchedFileDoesNotStealItsRecordUntilTheOriginalIsGone() throws {
        let store = try loggedInStore()
        let original = folder.video("original.mp4")
        try Data((0..<4000).map { UInt8($0 % 233) }).write(to: original)
        try store.recordWatch(original, position: 7, duration: 70)
        let copy = folder.video("copy.mp4")
        try FileManager.default.copyItem(at: original, to: copy)

        let saved = try Data(contentsOf: folder.profilesFile)
        for _ in 0..<2 {
            let feed = store.feed(.all, discovered: [original, copy])
            #expect(feed.first { $0.fileName == "original.mp4" }?.viewCount == 1)
        }
        #expect(store.stats(for: original)?.videoPath == normalized(original))
        #expect(store.stats(for: copy)?.viewCount == 1, "the copy shares the record by content")
        #expect(store.stats(for: copy)?.videoPath == normalized(original), "but does not take its path")
        #expect(try Data(contentsOf: folder.profilesFile) == saved, "looking never rewrites the file")

        try FileManager.default.removeItem(at: original)
        let followed = try #require(store.stats(for: copy))
        #expect(followed.videoPath == normalized(copy))
        #expect(followed.viewCount == 1)
    }

    // MARK: - Feed ordering

    @Test func feedOrdersFollowTheDocumentedRules() throws {
        let store = try loggedInStore()
        let a = folder.video("a.mp4"), b = folder.video("b.mp4"), c = folder.video("c.mp4"), d = folder.video("d.mp4")
        let idle = folder.video("idle.mp4")

        try store.recordWatch(a); clock.advance()
        try store.recordWatch(b); clock.advance()
        try store.recordWatch(b); clock.advance()
        try store.recordWatch(c); clock.advance()
        try store.setLiked(true, for: a); clock.advance()
        try store.setRating(5, for: d); clock.advance()
        try store.setRating(5, for: c); clock.advance()
        try store.setLiked(true, for: d)
        let discovered = [idle]

        func names(_ filter: VideoFeedFilter) -> [String] { store.feed(filter, discovered: discovered).map(\.fileName) }

        // History: most recently watched first (a was watched first but liked later; liking does not change last-watched).
        #expect(names(.history) == ["c.mp4", "b.mp4", "a.mp4"])
        // Most watched: b has 2 views, then c (newer) before a.
        #expect(names(.mostWatched) == ["b.mp4", "c.mp4", "a.mp4"])
        // Liked: most recently updated first: d was liked last, a before it.
        #expect(names(.liked) == ["d.mp4", "a.mp4"])
        // Top rated: rating, then views, then recency: c (5, 1 view) before d (5, 0 views).
        #expect(names(.topRated) == ["c.mp4", "d.mp4"])
        // All: watched/liked/rated first (last watched, then views, then name), then the rest by name.
        #expect(names(.all) == ["c.mp4", "b.mp4", "a.mp4", "d.mp4", "idle.mp4"])
    }

    @Test func discoveredFilesAreNeverPersistedUntilTouched() throws {
        let store = try loggedInStore()
        let seen = folder.video("seen.mp4")
        let before = try Data(contentsOf: folder.profilesFile)
        let feed = store.feed(.all, discovered: [seen, seen, folder.video("other.mp4")])
        #expect(feed.map(\.fileName) == ["other.mp4", "seen.mp4"])
        #expect(feed.allSatisfy { !$0.hasWatched && $0.viewCount == 0 })
        #expect(try Data(contentsOf: folder.profilesFile) == before)
        #expect(store.stats(for: seen) == nil)
    }

    @Test func feedSearchUsesTheLibrarySearchRules() throws {
        let store = try loggedInStore()
        let files = [folder.video("Sezon_2_Bolum_05.mkv"), folder.video("Belgesel Uzay.mp4"), folder.video("İstanbul Notları.mov")]
        #expect(store.feed(.all, search: "sezon 2 bolum", discovered: files).count == 1)
        #expect(store.feed(.all, search: "UZAY", discovered: files).map(\.fileName) == ["Belgesel Uzay.mp4"])
        #expect(store.feed(.all, search: "istanbul not", discovered: files).count == 1)
        #expect(store.feed(.all, search: "  ", discovered: files).count == 3)
        #expect(store.feed(.all, search: "yok boyle", discovered: files).isEmpty)
    }

    // MARK: - matches(search:)

    @Test func matchesSearchWholeStringThenEveryToken() {
        let video = VideoUserStats(
            videoPath: "/Videolar/Dizi Arşivi/Sezon_2_Bolum_05.mkv", fileName: "Sezon_2_Bolum_05.mkv",
            directoryPath: "/Videolar/Dizi Arşivi", updatedAt: Date())
        #expect(video.matches(search: nil))
        #expect(video.matches(search: ""))
        #expect(video.matches(search: "bolum_05"))
        #expect(video.matches(search: "sezon 2 bolum"))
        #expect(video.matches(search: "bolum-05.mkv sezon"))
        #expect(video.matches(search: "dizi arsivi"), "diacritics are ignored")
        #expect(video.matches(search: "ARŞİVİ"), "so is case, including the Turkish dotted capital")
        #expect(video.matches(search: "videolar sezon"), "tokens may match the folder")
        #expect(!video.matches(search: "sezon 3"))
        #expect(!video.matches(search: "bolum 06"))
        #expect(video.matches(search: "..."), "a query of only separators matches everything")
    }
}
