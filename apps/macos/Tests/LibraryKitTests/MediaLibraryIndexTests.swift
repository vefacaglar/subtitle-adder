import Foundation
import Testing
@testable import LibraryKit

/// Scanning, crawling and querying. The first six tests port the Windows `MediaLibraryIndexTests` (its two view model
/// tests become the index queries a view model makes).
@Suite struct MediaLibraryIndexTests {
    // MARK: Ported from Windows

    @Test func scanPopulatesMediaFilesAndDetectsSidecarSubtitles() async throws {
        let box = try MediaIndexSandbox()
        let videos = try box.folder("Videos")
        _ = try box.folder("Videos/SubfolderA")
        try box.file("Videos/episode01.mp4", size: 2048)
        try box.file("Videos/episode01.srt")
        try box.file("Videos/movie.mkv", size: 4096)
        try box.file("Videos/notes.txt")
        try box.file("Videos/Film.mp4")
        try box.file("Videos/Film.tr.srt")

        let index = box.index()
        let result = await index.scan(directory: videos)

        #expect(result.outcome == .scanned)
        #expect(result.snapshot.subdirectories == [box.path("Videos/SubfolderA")])
        #expect(result.snapshot.mediaFiles.map(\.fileName) == ["episode01.mp4", "Film.mp4", "movie.mkv"])
        #expect(await index.totalIndexedFiles == 3)

        let episode = try #require(result.snapshot.mediaFiles.first { $0.fileName == "episode01.mp4" })
        #expect(episode.hasSidecarSubtitle)
        #expect(episode.sidecarExtension == ".srt")
        #expect(episode.sizeBytes == 2048)
        #expect(episode.fullPath == box.path("Videos/episode01.mp4"))
        #expect(episode.directoryPath == box.path("Videos"))

        // A language-tagged subtitle shows the badge even though `findSidecar` would not auto-load it.
        let film = try #require(result.snapshot.mediaFiles.first { $0.fileName == "Film.mp4" })
        #expect(film.hasSidecarSubtitle)
        #expect(film.sidecarExtension == ".srt")

        let movie = try #require(result.snapshot.mediaFiles.first { $0.fileName == "movie.mkv" })
        #expect(!movie.hasSidecarSubtitle)
        #expect(movie.sidecarExtension == nil)
        #expect(movie.sizeBytes == 4096)
    }

    @Test func persistenceReloadsCachedEntriesInANewInstance() async throws {
        let box = try MediaIndexSandbox()
        let library = try box.folder("Library")
        try box.file("Library/lecture.mp4", size: 1024)
        try box.file("Library/lecture.vtt")

        await box.index().scan(directory: library)
        #expect(FileManager.default.fileExists(atPath: box.indexFile.path))

        let reloaded = box.index()
        #expect(await reloaded.totalIndexedFiles == 1)
        #expect(await reloaded.indexedDirectories.map(MediaPath.normalized) == [box.path("Library")])
        let item = try #require(await reloaded.cachedDirectory(library).first)
        #expect(item.fileName == "lecture.mp4")
        #expect(item.hasSidecarSubtitle)
        #expect(item.sidecarExtension == ".vtt")
    }

    @Test func rescanningPrunesDeletedFiles() async throws {
        let box = try MediaIndexSandbox()
        let folder = try box.folder("PruneTest")
        try box.file("PruneTest/keep.mp4")
        try box.file("PruneTest/remove.mkv")

        let index = box.index()
        #expect(await index.scan(directory: folder).snapshot.mediaFiles.count == 2)
        #expect(await index.totalIndexedFiles == 2)

        try box.remove("PruneTest/remove.mkv")
        let second = await index.scan(directory: folder)
        #expect(second.snapshot.mediaFiles.map(\.fileName) == ["keep.mp4"])
        #expect(await index.totalIndexedFiles == 1)
        #expect(await index.search("remove").isEmpty)
    }

    @Test func crawlAndSearchFindFilesAcrossNestedFolders() async throws {
        let box = try MediaIndexSandbox()
        try box.file("Root/intro.mp4")
        try box.file("Root/Season1/s01e01_pilot.mkv")
        try box.file("Root/Season2/Extras/behind_the_scenes.mov")

        let index = box.index()
        await index.crawl(from: box.root.appending(path: "Root"), maxDepth: 2)

        #expect(await index.totalIndexedFiles == 3)
        #expect(await index.search("pilot").map(\.fileName) == ["s01e01_pilot.mkv"])
        #expect(await index.search("behind").map(\.fileName) == ["behind_the_scenes.mov"])
        // A folder name finds the files inside it.
        #expect(await index.search("Season1").map(\.fileName) == ["s01e01_pilot.mkv"])
    }

    @Test func navigationQueriesServeCachedFoldersAndGlobalSearch() async throws {
        let box = try MediaIndexSandbox()
        let parent = try box.folder("Parent")
        let child = try box.folder("Parent/Child")
        let sibling = try box.folder("OtherIndexed")
        try box.file("Parent/parent_clip.mp4", size: 512)
        try box.file("Parent/Child/child_movie.mkv", size: 1024)
        try box.file("OtherIndexed/global_special_doc.mp4", size: 2048)

        let index = box.index()
        await index.scan(directory: sibling)
        await index.crawl(from: parent, maxDepth: 2)

        #expect(await index.cachedSubdirectories(parent).map(\.path) == [MediaPath.normalized(child)])
        #expect(await index.cachedDirectory(parent).map(\.fileName) == ["parent_clip.mp4"])
        #expect(await index.cachedDirectory(child).map(\.fileName) == ["child_movie.mkv"])
        #expect(await index.totalIndexedFiles == 3)
        // The search spans folders the current one doesn't contain.
        #expect(await index.search("global_special").map(\.fileName) == ["global_special_doc.mp4"])
    }

    @Test func rescanPicksUpANewSidecar() async throws {
        let box = try MediaIndexSandbox()
        let folder = try box.folder("SyncFolder")
        try box.file("SyncFolder/interview.mp4", size: 1024)

        let index = box.index()
        let before = try #require(await index.scan(directory: folder).snapshot.mediaFiles.first)
        #expect(!before.hasSidecarSubtitle)

        try box.file("SyncFolder/interview.srt")
        let after = try #require(await index.scan(directory: folder).snapshot.mediaFiles.first)
        #expect(after.hasSidecarSubtitle)
        #expect(after.sidecarExtension == ".srt")
        #expect(await index.cachedDirectory(folder).first?.sidecarExtension == ".srt")
    }

    // MARK: Scan rules

    @Test func hiddenDotAndPackageEntriesAreSkipped() async throws {
        let box = try MediaIndexSandbox()
        let folder = try box.folder("Mixed")
        try box.file("Mixed/visible.mp4")
        try box.file("Mixed/.dotfile.mp4")
        try box.file("Mixed/._visible.mp4")
        let flagged = try box.file("Mixed/flagged.mp4")
        var hidden = URLResourceValues()
        hidden.isHidden = true
        var flaggedURL = flagged
        try flaggedURL.setResourceValues(hidden)
        _ = try box.folder("Mixed/Shows")
        _ = try box.folder("Mixed/.git")
        _ = try box.folder("Mixed/Tool.app")
        _ = try box.folder("Mixed/Pictures.photoslibrary")
        try box.file("Mixed/Tool.app/inside.mp4")
        try box.file(".hiddenfolder/secret.mp4")
        try box.file("Mixed/.git/hook.mp4")

        let index = box.index()
        let snapshot = await index.scan(directory: folder).snapshot
        #expect(snapshot.mediaFiles.map(\.fileName) == ["visible.mp4"])
        #expect(snapshot.subdirectories == [box.path("Mixed/Shows")])

        // Crawling never enters them either.
        await index.crawl(from: box.root, maxDepth: 3)
        #expect(await index.totalIndexedFiles == 1)
        #expect(await index.search("secret").isEmpty)
        #expect(await index.search("inside").isEmpty)
        #expect(await index.search("hook").isEmpty)
    }

    @Test func filesAreSortedByNameWithNumbersAsNumbers() async throws {
        let box = try MediaIndexSandbox()
        let folder = try box.folder("Order")
        for name in ["Episode 10.mp4", "episode 2.mp4", "Episode 1.mp4", "Zeta.mp4", "alpha.mp4"] { try box.file("Order/\(name)") }
        for name in ["b10", "B9", "a"] { _ = try box.folder("Order/\(name)") }

        let snapshot = await box.index().scan(directory: folder).snapshot
        #expect(snapshot.mediaFiles.map(\.fileName) == ["alpha.mp4", "Episode 1.mp4", "episode 2.mp4", "Episode 10.mp4", "Zeta.mp4"])
        #expect(snapshot.subdirectories.map { ($0 as NSString).lastPathComponent } == ["a", "B9", "b10"])
    }

    @Test func namesAreStoredNFCNormalized() async throws {
        let box = try MediaIndexSandbox()
        let folder = try box.folder("Names")
        let nfc = "Çalışma Şarkı.mp4"
        let nfd = nfc.decomposedStringWithCanonicalMapping
        try box.file("Names/\(nfd)")

        let file = try #require(await box.index().scan(directory: folder).snapshot.mediaFiles.first)
        #expect(file.fileName.unicodeScalars.elementsEqual(nfc.precomposedStringWithCanonicalMapping.unicodeScalars))
        #expect(file.fullPath.unicodeScalars.elementsEqual(file.fullPath.precomposedStringWithCanonicalMapping.unicodeScalars))
        // APFS finds the file by either spelling.
        #expect(FileManager.default.fileExists(atPath: file.fullPath))
    }

    @Test func symbolicLinksCountAsTheirTargets() async throws {
        let box = try MediaIndexSandbox()
        let folder = try box.folder("Links")
        let target = try box.file("Elsewhere/real.mp4", size: 777)
        let realFolder = try box.folder("Elsewhere/Season")
        try FileManager.default.createSymbolicLink(at: folder.appending(path: "alias.mp4"), withDestinationURL: target)
        try FileManager.default.createSymbolicLink(at: folder.appending(path: "Season"), withDestinationURL: realFolder)
        try FileManager.default.createSymbolicLink(
            at: folder.appending(path: "dangling.mp4"), withDestinationURL: box.root.appending(path: "nowhere.mp4"))

        let snapshot = await box.index().scan(directory: folder).snapshot
        #expect(snapshot.mediaFiles.map(\.fileName) == ["alias.mp4"])
        #expect(snapshot.mediaFiles.first?.sizeBytes == 777)
        #expect(snapshot.subdirectories == [box.path("Links/Season")])
    }

    // MARK: Missing, vanished and unreadable folders

    @Test func deletedSubtreeIsRemovedFromSearchAndFromItsParent() async throws {
        let box = try MediaIndexSandbox()
        let root = try box.folder("Root")
        try box.file("Root/keep.mp4")
        try box.file("Root/Show/Season1/ep1.mp4")
        try box.file("Root/Show/Season2/ep2.mp4")
        let show = box.root.appending(path: "Root/Show")

        let index = box.index()
        await index.crawl(from: root, maxDepth: 3)
        #expect(await index.totalIndexedFiles == 3)

        try box.remove("Root/Show/Season1")
        let season = await index.scan(directory: box.root.appending(path: "Root/Show/Season1"))
        #expect(season.outcome == .missing)
        #expect(season.snapshot.mediaFiles.isEmpty)
        #expect(await index.search("ep1").isEmpty)
        #expect(await index.cachedSubdirectories(show).map(\.lastPathComponent) == ["Season2"])
        #expect(await index.totalIndexedFiles == 2)

        // Deleting a folder takes the entries of everything below it along, not just its own.
        try box.remove("Root/Show")
        let gone = await index.scan(directory: show)
        #expect(gone.outcome == .missing)
        #expect(await index.search("ep2").isEmpty)
        #expect(await index.cachedSnapshot(show.appending(path: "Season2")) == nil)
        #expect(await index.cachedSubdirectories(root).isEmpty)
        #expect(await index.indexedDirectories.map(MediaPath.normalized) == [box.path("Root")])

        let reloaded = box.index()
        #expect(await reloaded.totalIndexedFiles == 1)
        #expect(await reloaded.indexedDirectories.count == 1)
    }

    @Test func rescanningAParentDropsFoldersThatVanishedFromIt() async throws {
        let box = try MediaIndexSandbox()
        let root = try box.folder("Root")
        try box.file("Root/Old/Deep/clip.mp4")
        try box.file("Root/New/fresh.mp4")

        let index = box.index()
        await index.crawl(from: root, maxDepth: 3)
        #expect(await index.totalIndexedFiles == 2)

        try box.remove("Root/Old")
        let result = await index.scan(directory: root)
        #expect(result.snapshot.subdirectories == [box.path("Root/New")])
        #expect(await index.search("clip").isEmpty)
        #expect(await index.indexedDirectories.map(MediaPath.normalized) == [box.path("Root"), box.path("Root/New")])
    }

    @Test func scanningAFolderThatWasNeverIndexedAndDoesNotExistChangesNothing() async throws {
        let box = try MediaIndexSandbox()
        let index = box.index()
        let result = await index.scan(directory: box.root.appending(path: "Nope"))
        #expect(result.outcome == .missing)
        #expect(await index.indexedDirectories.isEmpty)
        #expect(!FileManager.default.fileExists(atPath: box.indexFile.path))

        // A file is not a folder.
        let file = try box.file("plain.mp4")
        #expect(await index.scan(directory: file).outcome == .missing)
    }

    @Test(.enabled(if: getuid() != 0, "root ignores file permissions"))
    func failedEnumerationKeepsTheCachedEntry() async throws {
        let box = try MediaIndexSandbox()
        let locked = try box.folder("Locked")
        try box.file("Locked/keep.mp4")
        _ = try box.folder("Locked/Sub")
        let never = try box.folder("Never")
        try box.file("Never/x.mp4")

        let index = box.index()
        #expect(await index.scan(directory: locked).snapshot.mediaFiles.count == 1)
        let before = try #require(await index.cachedSnapshot(locked))

        try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: locked.path)
        try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: never.path)
        defer {
            try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: locked.path)
            try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: never.path)
        }

        let result = await index.scan(directory: locked)
        #expect(result.outcome == .unreadable)
        #expect(result.snapshot == before)
        #expect(await index.cachedSnapshot(locked) == before)
        #expect(await index.cachedDirectory(locked).map(\.fileName) == ["keep.mp4"])
        #expect(await index.cachedSubdirectories(locked).map(\.lastPathComponent) == ["Sub"])
        #expect(await index.totalIndexedFiles == 1)

        // Never indexed and unreadable: nothing to keep, and nothing invented.
        let fresh = await index.scan(directory: never)
        #expect(fresh.outcome == .unreadable)
        #expect(fresh.snapshot.mediaFiles.isEmpty)
        #expect(await index.cachedSnapshot(never) == nil)

        // Once readable again the next scan takes over.
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: locked.path)
        try box.file("Locked/second.mp4")
        let again = await index.scan(directory: locked)
        #expect(again.outcome == .scanned)
        #expect(again.snapshot.mediaFiles.map(\.fileName) == ["keep.mp4", "second.mp4"])
    }

    // MARK: Crawl

    @Test func crawlHonorsDepthAndFolderLimits() async throws {
        let box = try MediaIndexSandbox()
        let root = try box.folder("Deep")
        try box.file("Deep/a.mp4")
        try box.file("Deep/L1/b.mp4")
        try box.file("Deep/L1/L2/c.mp4")
        try box.file("Deep/L1/L2/L3/d.mp4")
        try box.file("Deep/L1/L2/L3/L4/e.mp4")
        try box.file("Deep/L1/L2/L3/L4/L5/f.mp4")
        try box.file("Deep/L1/L2/L3/L4/L5/L6/g.mp4")

        let shallow = box.index()
        #expect(await shallow.crawl(from: root, maxDepth: 0) == 1)
        #expect(await shallow.totalIndexedFiles == 1)
        #expect(await shallow.crawl(from: root, maxDepth: 1) == 2)
        #expect(await shallow.totalIndexedFiles == 2)

        // Depth is clamped to 5: the folder six levels down is never reached.
        let deep = MediaLibraryIndex(fileURL: box.base.appending(path: "other.json"))
        #expect(await deep.crawl(from: root, maxDepth: 99) == 6)
        #expect(await deep.search("f.mp4").count == 1)
        #expect(await deep.search("g.mp4").isEmpty)

        let negative = MediaLibraryIndex(fileURL: box.base.appending(path: "negative.json"))
        #expect(await negative.crawl(from: root, maxDepth: -3) == 1)

        let limited = MediaLibraryIndex(fileURL: box.base.appending(path: "limited.json"))
        #expect(await limited.crawl(from: root, maxDepth: 5, maxFolders: 3) == 3)
        #expect(await limited.totalIndexedFiles == 3)
    }

    @Test func crawlGoesBreadthFirst() async throws {
        let box = try MediaIndexSandbox()
        let root = try box.folder("Wide")
        try box.file("Wide/A/Deep/a.mp4")
        try box.file("Wide/B/b.mp4")
        try box.file("Wide/C/c.mp4")

        let index = box.index()
        // Root, A, B, C come before A/Deep.
        #expect(await index.crawl(from: root, maxDepth: 5, maxFolders: 4) == 4)
        #expect(await index.search("a.mp4").isEmpty)
        #expect(await index.search("b.mp4").count == 1)
        #expect(await index.search("c.mp4").count == 1)
    }

    @Test func crawlOfAMissingRootIsHarmless() async throws {
        let box = try MediaIndexSandbox()
        let index = box.index()
        #expect(await index.crawl(from: box.root.appending(path: "Gone")) == 0)
        #expect(!FileManager.default.fileExists(atPath: box.indexFile.path))
    }

    @Test func cancelledCrawlPersistsWhatWasScanned() async throws {
        let box = try MediaIndexSandbox()
        let root = try box.folder("Root")
        try box.file("Root/first.mp4")
        for name in ["One", "Two", "Three"] { try box.file("Root/\(name)/\(name.lowercased()).mp4") }

        let index = box.index()
        let task = Task {
            // Cancels its own task right after the first folder, deterministically.
            await index.crawl(from: root, maxDepth: 1) { _ in withUnsafeCurrentTask { $0?.cancel() } }
        }
        #expect(await task.value == 1)
        #expect(await index.totalIndexedFiles == 1)

        let reloaded = box.index()
        #expect(await reloaded.cachedDirectory(root).map(\.fileName) == ["first.mp4"])
        #expect(await reloaded.cachedSubdirectories(root).count == 3)
        #expect(await reloaded.totalIndexedFiles == 1)
        #expect(await reloaded.indexedDirectories.count == 1)
    }

    @Test func crawlReportsProgressPerFolder() async throws {
        let box = try MediaIndexSandbox()
        let root = try box.folder("Root")
        try box.file("Root/A/a.mp4")
        try box.file("Root/B/b.mp4")
        let counts = Counts()
        await box.index().crawl(from: root) { counts.append($0) }
        #expect(counts.values == [1, 2, 3])
    }

    // MARK: Queries

    @Test func searchRanksPrefixThenNameThenFolderThenNewest() async throws {
        let box = try MediaIndexSandbox()
        let old = Date(timeIntervalSince1970: 1_000_000), mid = Date(timeIntervalSince1970: 2_000_000), new = Date(timeIntervalSince1970: 3_000_000)
        try box.file("Season Alpha/beta.mp4", modified: new)         // 1: only the folder matches
        try box.file("misc/the alpha.mp4", modified: old)            // 2: name contains
        try box.file("misc/alpha one.mp4", modified: old)            // 3: name starts with
        try box.file("misc/alpha b.mp4", modified: mid)              // 3, newer than "alpha one"
        try box.file("misc/alpha a.mp4", modified: mid)              // 3, same time: by name
        try box.file("misc/unrelated.mp4", modified: new)

        let index = box.index()
        await index.crawl(from: box.root)
        let names = await index.search("  Alpha ").map(\.fileName)
        #expect(names == ["alpha a.mp4", "alpha b.mp4", "alpha one.mp4", "the alpha.mp4", "beta.mp4"])
        #expect(await index.search("alpha", limit: 2).map(\.fileName) == ["alpha a.mp4", "alpha b.mp4"])
        #expect(await index.search("alpha", limit: 0).isEmpty)
    }

    @Test func blankQueryFindsNothing() async throws {
        let box = try MediaIndexSandbox()
        try box.file("a.mp4")
        let index = box.index()
        await index.crawl(from: box.root)
        #expect(await index.search("").isEmpty)
        #expect(await index.search("   \n").isEmpty)
        #expect(await index.allFiles().count == 1)
    }

    @Test func searchIgnoresCaseAndDiacritics() async throws {
        let box = try MediaIndexSandbox()
        try box.file("Işık Şarkı.mp4")
        try box.file("İstanbul Gezisi.mp4")
        try box.file("Crème Brûlée.mkv")
        let index = box.index()
        await index.crawl(from: box.root)
        #expect(await index.search("isik").map(\.fileName) == ["Işık Şarkı.mp4"])
        #expect(await index.search("SARKI").map(\.fileName) == ["Işık Şarkı.mp4"])
        #expect(await index.search("ışık şarkı").map(\.fileName) == ["Işık Şarkı.mp4"])
        #expect(await index.search("istanbul").map(\.fileName) == ["İstanbul Gezisi.mp4"])
        #expect(await index.search("İSTANBUL").map(\.fileName) == ["İstanbul Gezisi.mp4"])
        #expect(await index.search("creme brulee").map(\.fileName) == ["Crème Brûlée.mkv"])
    }

    @Test func allFilesListsEverythingNewestFirst() async throws {
        let box = try MediaIndexSandbox()
        try box.file("one/oldest.mp4", modified: Date(timeIntervalSince1970: 1_000))
        try box.file("two/newest.mkv", modified: Date(timeIntervalSince1970: 3_000))
        try box.file("one/middle.mov", modified: Date(timeIntervalSince1970: 2_000))
        try box.file("two/also middle.mp3", modified: Date(timeIntervalSince1970: 2_000))

        let index = box.index()
        #expect(await index.allFiles().isEmpty)
        await index.crawl(from: box.root)

        #expect(await index.allFiles().map(\.fileName) == ["newest.mkv", "also middle.mp3", "middle.mov", "oldest.mp4"])
        #expect(await index.allFiles(limit: 2).map(\.fileName) == ["newest.mkv", "also middle.mp3"])
        #expect(await index.allFiles(limit: 0).isEmpty)
    }

    @Test func directoriesAreMatchedCaseInsensitivelyButKeepTheirCase() async throws {
        let box = try MediaIndexSandbox()
        let folder = try box.folder("Videos")
        try box.file("Videos/a.mp4")
        let index = box.index()
        await index.scan(directory: folder)

        let shouting = box.root.appending(path: "VIDEOS")
        #expect(await index.cachedDirectory(shouting).map(\.fileName) == ["a.mp4"])
        #expect(await index.cachedDirectory(folder.appending(path: "../Videos/")).map(\.fileName) == ["a.mp4"])
        #expect(await index.indexedDirectories.map(MediaPath.normalized) == [box.path("Videos")])
        #expect(await index.cachedDirectory(box.root.appending(path: "Other")).isEmpty)
        #expect(await index.cachedSubdirectories(box.root.appending(path: "Other")).isEmpty)
    }
}

/// Collects progress callbacks, which are `@Sendable`.
private final class Counts: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: [Int] = []
    func append(_ value: Int) { lock.withLock { storage.append(value) } }
    var values: [Int] { lock.withLock { storage } }
}
