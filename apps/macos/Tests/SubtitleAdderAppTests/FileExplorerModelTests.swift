import Foundation
import LibraryKit
import Synchronization
import Testing
@testable import SubtitleAdderApp

/// The `/Volumes/<name>` roots the injected mount check reports as mounted.
private final class Mounts: Sendable {
    private let roots = Mutex<Set<String>>([])

    func mount(_ root: String) { roots.withLock { _ = $0.insert(root) } }
    func unmountAll() { roots.withLock { $0.removeAll() } }
    func isMounted(_ root: String) -> Bool { roots.withLock { $0.contains(root) } }
}

/// A temporary media tree with its own index file and defaults suite, removed when the test ends.
@MainActor
private final class ExplorerSandbox {
    let base: URL
    let root: URL
    let indexFile: URL
    let defaults: UserDefaults
    private let suiteName = "explorer-test-\(UUID().uuidString)"
    let mounts = Mounts()

    init() throws {
        base = FileManager.default.temporaryDirectory.appending(path: "ExplorerTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        root = base.appending(path: "Root", directoryHint: .isDirectory)
        indexFile = base.appending(path: "media-index.json")
        defaults = UserDefaults(suiteName: suiteName)!
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    deinit {
        try? FileManager.default.removeItem(at: base)
        UserDefaults(suiteName: suiteName)?.removePersistentDomain(forName: suiteName)
    }

    var moviesFolder: String { path("Movies") }
    var homeFolder: String { path("Home") }

    /// The normalized path of `root/<relative>`.
    func path(_ relative: String) -> String {
        MediaPath.normalized(root.appending(path: relative))
    }

    func url(_ relative: String) -> URL {
        root.appending(path: relative)
    }

    func folder(_ relative: String) throws {
        try FileManager.default.createDirectory(at: root.appending(path: relative), withIntermediateDirectories: true)
    }

    func file(_ relative: String, size: Int = 1) throws {
        let url = root.appending(path: relative)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data(count: size).write(to: url)
    }

    func services(volumesWatched: Bool = false) -> ExplorerServices {
        let mounts = mounts
        let movies = moviesFolder
        let home = homeFolder
        var services = ExplorerServices.system
        services.quickLocations = { [] }
        services.isVolumeMounted = { mounts.isMounted($0) }
        services.moviesFolder = { movies }
        services.homeFolder = { home }
        services.searchDelay = .milliseconds(20)
        services.watchesVolumes = volumesWatched
        return services
    }

    func index() -> MediaLibraryIndex {
        MediaLibraryIndex(fileURL: indexFile)
    }

    func model(index: MediaLibraryIndex? = nil, defaults: UserDefaults? = nil) -> FileExplorerModel {
        FileExplorerModel(index: index ?? self.index(), defaults: defaults ?? self.defaults, services: services())
    }
}

private func names(_ files: [IndexedMediaFile]) -> [String] { files.map(\.fileName) }

@MainActor
@Suite struct FileExplorerModelTests {
    // MARK: - Where it starts

    @Test func startsInMoviesWhenNothingIsSaved() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/a.mp4")
        let model = box.model()
        await model.start()
        #expect(model.currentFolder == box.moviesFolder)
        #expect(names(model.files) == ["a.mp4"])
        #expect(!model.isScanning)
    }

    @Test func fallsBackToTheHomeFolderWithoutMovies() async throws {
        let box = try ExplorerSandbox()
        try box.file("Home/h.mp4")
        let model = box.model()
        await model.start()
        #expect(model.currentFolder == box.homeFolder)
        #expect(names(model.files) == ["h.mp4"])
    }

    @Test func aSavedFolderThatIsGoneFallsBackToTheDefault() async throws {
        let box = try ExplorerSandbox()
        try box.folder("Movies")
        box.defaults.set(box.path("Deleted"), forKey: FileExplorerModel.lastFolderKey)
        let model = box.model()
        await model.start()
        #expect(model.currentFolder == box.moviesFolder)
    }

    /// A saved folder on a network mount that no longer answers must not hold up the app's launch: creating the model
    /// asks the file system nothing, `start()` does, off the main actor.
    @Test func creatingTheModelDoesNotTouchTheFileSystem() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/a.mp4")
        try box.file("Saved/s.mp4")
        box.defaults.set(box.path("Saved"), forKey: FileExplorerModel.lastFolderKey)
        let asked = Mutex<[String]>([])
        let onMain = Mutex<[Bool]>([])
        var services = box.services()
        let inner = services.directoryExists
        services.directoryExists = { path in
            asked.withLock { $0.append(path) }
            onMain.withLock { $0.append(Thread.isMainThread) }
            Thread.sleep(forTimeInterval: 0.3)
            return inner(path)
        }
        services.isVolumeMounted = { _ in
            asked.withLock { $0.append("mount-check") }
            return true
        }
        let started = ContinuousClock.now
        let model = FileExplorerModel(index: box.index(), defaults: box.defaults, services: services)
        #expect(ContinuousClock.now - started < .milliseconds(200), "the slow check was not waited for")
        #expect(asked.withLock { $0 }.isEmpty)
        // Until start() has looked, the folder is the saved one as it was stored.
        #expect(model.currentFolder == box.path("Saved") && model.isScanning)

        await model.start()
        #expect(asked.withLock { $0 }.contains(box.path("Saved")))
        #expect(onMain.withLock { $0 }.allSatisfy { !$0 })
        #expect(model.currentFolder == box.path("Saved"))
        #expect(names(model.files) == ["s.mp4"])
    }

    @Test func aFolderChosenBeforeStartIsNotReplacedByTheLaunchLookup() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/a.mp4")
        try box.file("Other/o.mp4")
        let model = box.model()
        await model.navigate(to: box.url("Other"))
        await model.start()
        #expect(model.currentFolder == box.path("Other"))
        #expect(names(model.files) == ["o.mp4"])
    }

    @Test func theSidebarIsShownByDefaultAndTheChoiceIsStored() throws {
        let box = try ExplorerSandbox()
        let model = box.model()
        #expect(model.isSidebarVisible)
        #expect(box.defaults.object(forKey: "showFileExplorer") == nil)
        model.isSidebarVisible = false
        #expect(box.defaults.bool(forKey: "showFileExplorer") == false)
        #expect(!box.model().isSidebarVisible)
        model.isSidebarVisible = true
        #expect(box.model().isSidebarVisible)
    }

    // MARK: - Last folder

    @Test func theLastFolderIsStoredAndShownAtLaunchWithoutACrawl() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        try box.file("Shows/e1.mp4")
        try box.file("Shows/Season 1/deep.mp4")

        let first = box.model()
        await first.navigate(to: box.url("Shows"))
        #expect(box.defaults.string(forKey: "explorerLastFolder") == box.path("Shows"))

        // A new run with an empty index: the saved folder is listed, nothing below it is crawled.
        let freshIndex = MediaLibraryIndex(fileURL: box.base.appending(path: "other-index.json"))
        let second = box.model(index: freshIndex)
        #expect(second.currentFolder == box.path("Shows"))
        #expect(second.files.isEmpty)
        await second.start()
        #expect(names(second.files) == ["e1.mp4"])
        #expect(second.folders.map(\.name) == ["Season 1"])
        #expect(await freshIndex.cachedSnapshot(box.url("Shows/Season 1")) == nil)
        #expect(!second.canGoBack)
    }

    @Test func theLaunchScanDoesNotOverrideAnEarlierNavigation() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        try box.file("A/a.mp4")
        let model = box.model()
        await model.navigate(to: box.url("A"))
        try box.file("A/later.mp4")
        await model.start()
        #expect(model.currentFolder == box.path("A"))
        #expect(names(model.files) == ["a.mp4"])
    }

    @Test func launchShowsTheCachedListingOfAFolderThatCannotBeReadNow() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/old.mp4")
        let index = box.index()
        await index.scan(directory: box.url("Movies"))

        // The rescan cannot list the folder, so what the index remembers is what the sidebar shows.
        let folder = box.url("Movies")
        try FileManager.default.setAttributes([.posixPermissions: 0o000], ofItemAtPath: folder.path)
        defer { try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: folder.path) }
        let model = box.model(index: index)
        await model.start()
        #expect(names(model.files) == ["old.mp4"])
        #expect(model.availability == .unreadable)
        #expect(!model.isScanning)
    }

    // MARK: - Navigation

    @Test func navigatingListsTheFolderAndPushesTheBackStack() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        try box.file("A/one.mp4", size: 5)
        try box.file("A/B/two.mp4")
        try box.folder("A/Zeta")
        let model = box.model()
        await model.start()

        await model.navigate(to: box.url("A"))
        #expect(model.currentFolder == box.path("A"))
        #expect(model.folders.map(\.name) == ["B", "Zeta"])
        #expect(names(model.files) == ["one.mp4"])
        #expect(model.backStack == [box.moviesFolder])
        #expect(model.canGoBack)
        #expect(!model.isScanning)

        // Opening the folder already being shown is not a step.
        await model.navigate(to: box.url("A"))
        #expect(model.backStack == [box.moviesFolder])

        await model.navigate(to: box.url("A/B"))
        #expect(model.backStack == [box.moviesFolder, box.path("A")])
        #expect(names(model.files) == ["two.mp4"])
    }

    @Test func upAndBackMoveThroughTheHistoryWithoutAForwardStack() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        try box.file("A/one.mp4")
        try box.file("A/B/two.mp4")
        let model = box.model()
        await model.start()
        await model.navigate(to: box.url("A/B"))

        await model.goUp()
        #expect(model.currentFolder == box.path("A"))
        #expect(names(model.files) == ["one.mp4"])
        // Up is a step, so back returns to where it started.
        #expect(model.backStack == [box.moviesFolder, box.path("A/B")])

        await model.goBack()
        #expect(model.currentFolder == box.path("A/B"))
        #expect(names(model.files) == ["two.mp4"])
        #expect(model.backStack == [box.moviesFolder])

        await model.goBack()
        #expect(model.currentFolder == box.moviesFolder)
        #expect(!model.canGoBack)

        // Going back with nothing left does nothing.
        await model.goBack()
        #expect(model.currentFolder == box.moviesFolder)
    }

    @Test func theRootHasNoParent() async throws {
        let box = try ExplorerSandbox()
        let model = box.model()
        #expect(model.canGoUp)
        #expect(ExplorerPaths.parent(of: "/") == nil)
        #expect(ExplorerPaths.parent(of: "/Users") == "/")
        #expect(ExplorerPaths.parent(of: "/Users/me/Movies") == "/Users/me")
        #expect(ExplorerPaths.name(of: "/Users/me/Movies") == "Movies")
        #expect(ExplorerPaths.volumeRoot(of: "/Volumes/Disk/Films") == "/Volumes/Disk")
        #expect(ExplorerPaths.volumeRoot(of: "/Volumes") == nil)
        #expect(ExplorerPaths.volumeRoot(of: "/Users/me") == nil)
    }

    @Test func navigatingCrawlsBelowButUpBackAndRefreshOnlyScan() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        try box.file("A/B/Deep/deep.mp4")
        try box.file("Sibling/Inner/inner.mp4")
        let index = box.index()
        let model = box.model(index: index)
        await model.start()

        await model.navigate(to: box.url("A"))
        #expect(await index.cachedSnapshot(box.url("A/B/Deep"))?.mediaFiles.count == 1)

        // Up shows the root's listing; its other subfolders are only named, not entered.
        await model.goUp()
        #expect(model.folders.map(\.name).contains("Sibling"))
        #expect(await index.cachedSnapshot(box.url("Sibling")) == nil)
        await model.goBack()
        await model.refresh()
        #expect(await index.cachedSnapshot(box.url("Sibling")) == nil)
        #expect(model.totalIndexedFiles == (await index.totalIndexedFiles))
    }

    @Test func theOldRowsNeverShowUnderTheNewFolder() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        try box.file("A/a.mp4")
        try box.file("B/b.mp4")
        let model = box.model()
        await model.navigate(to: box.url("A"))
        #expect(names(model.files) == ["a.mp4"])

        let pending = Task { await model.navigate(to: box.url("B")) }
        await Task.yield()
        // Whatever the scan has reached by now, a.mp4 is not in B.
        #expect(!names(model.files).contains("a.mp4"))
        await pending.value
        #expect(names(model.files) == ["b.mp4"])
    }

    @Test func aMissingFolderIsReportedAndEmpty() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        let model = box.model()
        await model.start()
        await model.navigate(to: box.url("Nowhere"))
        #expect(model.availability == .notFound)
        #expect(model.isEmpty)
        #expect(!model.isScanning)
        await model.goBack()
        #expect(model.availability == .available)
        #expect(names(model.files) == ["m.mp4"])
    }

    // MARK: - Unmounted volumes

    @Test func aFolderOnAnUnmountedVolumeIsUnavailableAndNotScanned() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        let index = box.index()
        let model = box.model(index: index)
        await model.start()
        #expect(names(model.files) == ["m.mp4"])
        let scannedBefore = await index.indexedDirectories

        let gone = URL(filePath: "/Volumes/ExplorerTestDisk-\(UUID().uuidString)/Films", directoryHint: .isDirectory)
        await model.navigate(to: gone)
        #expect(model.availability == .volumeNotMounted)
        #expect(model.isEmpty)
        #expect(!model.isScanning)
        #expect(model.currentFolder == MediaPath.normalized(gone))
        // A scan would have found the folder missing and could have dropped cached entries below it.
        #expect(await index.cachedSnapshot(gone) == nil)
        #expect(await index.indexedDirectories == scannedBefore)

        // The folder is remembered, and shown as unavailable again at the next launch.
        #expect(box.defaults.string(forKey: "explorerLastFolder") == MediaPath.normalized(gone))
        let relaunched = box.model(index: index)
        #expect(relaunched.currentFolder == MediaPath.normalized(gone))
        await relaunched.start()
        #expect(relaunched.currentFolder == MediaPath.normalized(gone))
        #expect(relaunched.availability == .volumeNotMounted)
        #expect(!relaunched.isScanning)
    }

    @Test func aVolumeThatComesBackIsScannedAndOneThatGoesIsCleared() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        let model = box.model()
        await model.start()

        let volume = "ExplorerTestDisk-\(UUID().uuidString)"
        let folder = URL(filePath: "/Volumes/\(volume)/Films", directoryHint: .isDirectory)
        await model.navigate(to: folder)
        #expect(model.availability == .volumeNotMounted)

        // Mounted (the folder itself does not exist here, so the scan says so; it did run).
        box.mounts.mount("/Volumes/\(volume)")
        await model.volumesChanged()
        #expect(model.availability == .notFound)

        box.mounts.unmountAll()
        await model.volumesChanged()
        #expect(model.availability == .volumeNotMounted)
        #expect(model.isEmpty)
    }

    @Test func aVideoOnAnUnmountedVolumeStartsNoScan() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        let index = box.index()
        let model = box.model(index: index)
        await model.start()
        let before = await index.indexedDirectories
        await model.sync(toVideo: URL(filePath: "/Volumes/ExplorerTestDisk-\(UUID().uuidString)/x.mp4"))
        #expect(await index.indexedDirectories == before)
        #expect(model.currentFolder == box.moviesFolder)
    }

    // MARK: - Search

    private func searchTree(_ box: ExplorerSandbox) throws {
        try box.file("Movies/alpha.mp4")
        try box.file("Movies/beta.mp4")
        try box.file("Movies/alphabet/inner.mp4")
        try box.file("Other/alpha2.mp4")
        try box.file("Other/sub/alpha3.mov")
        try box.file("Other/unrelated.mp4")
    }

    @Test func searchMergesFolderAndIndexHitsWithoutDuplicates() async throws {
        let box = try ExplorerSandbox()
        try searchTree(box)
        let index = box.index()
        await index.crawl(from: box.url("Other"))
        let model = box.model(index: index)
        await model.navigate(to: box.url("Movies"))

        model.searchQuery = "  ALPH  "
        await model.waitForSearch()
        // The folder's own rows first, then the other folders' files from the index; alpha.mp4 is in both and shown once.
        #expect(model.folders.map(\.name) == ["alphabet"])
        #expect(names(model.files).first == "alpha.mp4")
        // inner.mp4 lives in a folder whose path matches.
        #expect(Set(names(model.files)) == ["alpha.mp4", "alpha2.mp4", "alpha3.mov", "inner.mp4"])
        #expect(model.files.count == Set(model.files.map { $0.fullPath.lowercased() }).count)
        #expect(model.isSearching)

        // Only the files outside the folder name their folder.
        let outside = model.files.filter(model.isOutsideCurrentFolder)
        #expect(Set(names(outside)) == ["alpha2.mp4", "alpha3.mov", "inner.mp4"])
    }

    @Test func searchIgnoresCaseAndDiacriticsAndTheTurkishDotlessI() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/Çalışma Notları.mp4")
        try box.file("Movies/ışık.mp4")
        try box.file("Movies/other.mp4")
        let model = box.model()
        await model.start()
        model.searchQuery = "calisma"
        await model.waitForSearch()
        #expect(names(model.files) == ["Çalışma Notları.mp4"])
        model.searchQuery = "ISIK"
        await model.waitForSearch()
        #expect(names(model.files) == ["ışık.mp4"])
    }

    @Test func typingWaitsAndOnlyTheLastQueryIsApplied() async throws {
        let box = try ExplorerSandbox()
        try searchTree(box)
        let model = box.model()
        await model.navigate(to: box.url("Movies"))
        #expect(names(model.files) == ["alpha.mp4", "beta.mp4"])

        for query in ["b", "be", "bet", "beta"] { model.searchQuery = query }
        // The debounce has not elapsed: the list is still the whole folder.
        #expect(names(model.files) == ["alpha.mp4", "beta.mp4"])
        await model.waitForSearch()
        #expect(names(model.files) == ["beta.mp4"])

        // Clearing the field restores the folder at once.
        model.searchQuery = ""
        #expect(names(model.files) == ["alpha.mp4", "beta.mp4"])
        #expect(model.folders.map(\.name) == ["alphabet"])
        #expect(!model.isSearching)
    }

    @Test func aBlankQueryIsNotASearch() async throws {
        let box = try ExplorerSandbox()
        try searchTree(box)
        let model = box.model()
        await model.navigate(to: box.url("Movies"))
        model.searchQuery = "   "
        await model.waitForSearch()
        #expect(!model.isSearching)
        #expect(names(model.files) == ["alpha.mp4", "beta.mp4"])
    }

    @Test func navigatingEndsTheSearch() async throws {
        let box = try ExplorerSandbox()
        try searchTree(box)
        let model = box.model()
        await model.navigate(to: box.url("Movies"))
        model.searchQuery = "alph"
        await model.waitForSearch()
        await model.navigate(to: box.url("Other"))
        #expect(model.searchQuery.isEmpty)
        #expect(names(model.files) == ["alpha2.mp4", "unrelated.mp4"])
    }

    @Test func mergeKeepsTheFolderRowsFirstAndDropsRepeatsByPath() {
        func file(_ path: String) -> IndexedMediaFile {
            let url = URL(filePath: path)
            return IndexedMediaFile(
                fullPath: path, fileName: url.lastPathComponent, directoryPath: url.deletingLastPathComponent().path,
                sizeBytes: 1, lastWriteTimeUtc: .distantPast, hasSidecarSubtitle: false, sidecarExtension: nil,
                indexedAtUtc: .distantPast)
        }
        let local = [file("/m/Episode 1.mp4"), file("/m/Episode 2.mp4"), file("/m/Movie.mp4")]
        let hits = [file("/o/episode 3.mp4"), file("/M/EPISODE 1.mp4"), file("/m/Episode 2.mp4")]
        let merged = ExplorerSearch.merge(
            query: "episode", folders: [ExplorerFolder(path: "/m/Episodes"), ExplorerFolder(path: "/m/Misc")],
            files: local, indexHits: hits)
        #expect(merged.folders.map(\.name) == ["Episodes"])
        #expect(merged.files.map(\.fullPath) == ["/m/Episode 1.mp4", "/m/Episode 2.mp4", "/o/episode 3.mp4"])
    }

    // MARK: - Following the open video

    @Test func openingAListedVideoOnlyMovesTheHighlight() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/v.mp4")
        try box.file("Movies/w.mp4")
        let index = box.index()
        let model = box.model(index: index)
        await model.navigate(to: box.url("Movies"))
        let scanned = try #require(await index.cachedSnapshot(box.url("Movies"))?.scannedAtUtc)

        // Something appears in the folder; opening an already listed video must not go and look.
        try box.file("Movies/appeared.mp4")
        await model.sync(toVideo: box.url("Movies/v.mp4"))
        #expect(names(model.files) == ["v.mp4", "w.mp4"])
        #expect(await index.cachedSnapshot(box.url("Movies"))?.scannedAtUtc == scanned)
        #expect(!model.isScanning)
        let active = model.files.filter(model.isActive)
        #expect(names(active) == ["v.mp4"])

        await model.sync(toVideo: box.url("Movies/w.mp4"))
        #expect(names(model.files.filter(model.isActive)) == ["w.mp4"])
        #expect(names(model.files) == ["v.mp4", "w.mp4"])
        #expect(await index.cachedSnapshot(box.url("Movies"))?.scannedAtUtc == scanned)

        await model.sync(toVideo: nil)
        #expect(model.files.filter(model.isActive).isEmpty)
    }

    @Test func aVideoThatIsNotListedYetRescansItsFolder() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/v.mp4")
        let model = box.model()
        await model.navigate(to: box.url("Movies"))
        try box.file("Movies/new.mp4")
        await model.sync(toVideo: box.url("Movies/new.mp4"))
        #expect(names(model.files) == ["new.mp4", "v.mp4"])
        #expect(names(model.files.filter(model.isActive)) == ["new.mp4"])
    }

    @Test func aChangedSubtitleStatusRescansItsFolder() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/v.mp4")
        let model = box.model()
        await model.navigate(to: box.url("Movies"))
        #expect(model.files.first?.hasSidecarSubtitle == false)

        try box.file("Movies/v.srt")
        try box.file("Movies/other.mp4")
        await model.sync(toVideo: box.url("Movies/v.mp4"))
        #expect(names(model.files) == ["other.mp4", "v.mp4"])
        let video = try #require(model.files.first { $0.fileName == "v.mp4" })
        #expect(video.sidecarExtension == ".srt")

        // The badge now matches what the folder offers, so the next switch to it is free.
        try box.file("Movies/later.mp4")
        await model.sync(toVideo: box.url("Movies/v.mp4"))
        #expect(!names(model.files).contains("later.mp4"))
    }

    @Test func aTranslatedSubtitleNameCountsAsTheSameStatusAsTheIndexSaw() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/v.mp4")
        try box.file("Movies/v.tr.srt")
        let model = box.model()
        await model.navigate(to: box.url("Movies"))
        #expect(model.files.first { $0.fileName == "v.mp4" }?.sidecarExtension == ".srt")
        try box.file("Movies/later.mp4")
        // `v.tr.srt` is a badge for the index and for the sync check alike: no rescan loop.
        await model.sync(toVideo: box.url("Movies/v.mp4"))
        #expect(!names(model.files).contains("later.mp4"))
    }

    @Test func aVideoInAnotherFolderScansThatFolderAndStaysPut() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        try box.file("Elsewhere/x.mp4")
        let index = box.index()
        let model = box.model(index: index)
        await model.navigate(to: box.url("Movies"))
        let history = model.backStack

        await model.sync(toVideo: box.url("Elsewhere/x.mp4"))
        #expect(model.currentFolder == box.moviesFolder)
        #expect(model.backStack == history)
        #expect(names(model.files) == ["m.mp4"])
        #expect(await index.cachedSnapshot(box.url("Elsewhere"))?.mediaFiles.map(\.fileName) == ["x.mp4"])
        #expect(model.totalIndexedFiles == 2)
        #expect(model.files.filter(model.isActive).isEmpty)
        // Searching finds it now.
        model.searchQuery = "x.mp"
        await model.waitForSearch()
        #expect(names(model.files) == ["x.mp4"])
        #expect(names(model.files.filter(model.isActive)) == ["x.mp4"])
    }

    @Test func theFirstVideoTakesAFreshExplorerToItsFolder() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        try box.file("Docs/talk.mp4")
        try box.file("Docs/Sub/more.mp4")
        let index = box.index()
        let model = box.model(index: index)
        await model.start()
        #expect(model.currentFolder == box.moviesFolder)

        await model.sync(toVideo: box.url("Docs/talk.mp4"))
        #expect(model.currentFolder == box.path("Docs"))
        #expect(names(model.files) == ["talk.mp4"])
        #expect(names(model.files.filter(model.isActive)) == ["talk.mp4"])
        #expect(model.backStack.isEmpty)
        #expect(await index.cachedSnapshot(box.url("Docs/Sub")) != nil)
        #expect(box.defaults.string(forKey: "explorerLastFolder") == box.path("Docs"))

        // From now on the explorer stays where the user is.
        await model.navigate(to: box.url("Movies"))
        await model.sync(toVideo: box.url("Docs/talk.mp4"))
        #expect(model.currentFolder == box.moviesFolder)
    }

    @Test func aChosenFolderIsNotLeftForAVideo() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/m.mp4")
        try box.file("Docs/talk.mp4")
        box.defaults.set(box.path("Movies"), forKey: FileExplorerModel.lastFolderKey)
        let model = box.model()
        await model.start()
        await model.sync(toVideo: box.url("Docs/talk.mp4"))
        #expect(model.currentFolder == box.moviesFolder)
    }

    @Test func aSavedSubtitleGivesTheVideoItsBadge() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/v.mp4")
        try box.file("Elsewhere/e.mp4")
        let index = box.index()
        let model = box.model(index: index)
        await model.navigate(to: box.url("Movies"))
        await model.sync(toVideo: box.url("Movies/v.mp4"))
        #expect(model.files.first?.hasSidecarSubtitle == false)

        try box.file("Movies/v.vtt")
        await model.subtitleSaved(at: box.url("Movies/v.vtt"))
        #expect(model.files.first?.sidecarExtension == ".vtt")

        // Next to a video in a folder that is not on show, the index learns of it.
        await index.scan(directory: box.url("Elsewhere"))
        try box.file("Elsewhere/e.srt")
        await model.subtitleSaved(at: box.url("Elsewhere/e.srt"))
        #expect(await index.cachedDirectory(box.url("Elsewhere")).first?.sidecarExtension == ".srt")
        #expect(model.currentFolder == box.moviesFolder)
    }

    @Test func theStatusLineCountsTheShownAndTheIndexedFiles() async throws {
        let box = try ExplorerSandbox()
        try box.file("Movies/a.mp4")
        try box.file("Movies/b.mp4")
        try box.file("Movies/Sub/c.mp4")
        let model = box.model()
        await model.navigate(to: box.url("Movies"))
        #expect(model.files.count == 2)
        #expect(model.totalIndexedFiles == 3)
        let summary = model.statusSummary
        #expect(summary.contains("2") && summary.contains("3"))
    }
}
