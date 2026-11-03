import AppKit
import EngineKit
import Foundation
import LibraryKit
import Observation

/// One subfolder row of the file sidebar.
struct ExplorerFolder: Identifiable, Hashable, Sendable {
    let path: String
    let name: String
    var id: String { path }

    init(path: String) {
        self.path = path
        name = ExplorerPaths.name(of: path)
    }
}

/// Why the current folder shows what it shows.
enum FolderAvailability: Equatable, Sendable {
    case available
    /// The folder lives on a volume (under `/Volumes`) that is no longer mounted. It is not scanned: a scan would find
    /// it missing and drop its cached entries from the index.
    case volumeNotMounted
    /// The last scan found no such folder.
    case notFound
    /// The folder exists but could not be listed (permissions); the cached listing, if any, stays.
    case unreadable
}

/// Path helpers on the strings `MediaPath` produces (standardized, NFC, no trailing slash).
enum ExplorerPaths {
    /// The case-insensitive identity of a path, as the index compares folders.
    static func key(_ path: String) -> String {
        MediaPath.normalized(path).lowercased()
    }

    /// The containing folder of a normalized path; `nil` for `/`.
    static func parent(of path: String) -> String? {
        guard path != "/", path.hasPrefix("/"), let slash = path.lastIndex(of: "/") else { return nil }
        return slash == path.startIndex ? "/" : String(path[..<slash])
    }

    /// The last component, or the path itself for `/`.
    static func name(of path: String) -> String {
        guard path != "/", let slash = path.lastIndex(of: "/") else { return path }
        return String(path[path.index(after: slash)...])
    }

    /// `/Volumes/<name>` for a path below a mounted-volume folder, `nil` for everything else (including `/Volumes`
    /// itself, which always exists).
    static func volumeRoot(of path: String) -> String? {
        let parts = path.split(separator: "/", omittingEmptySubsequences: true)
        guard parts.count >= 2, parts[0] == "Volumes" else { return nil }
        return "/Volumes/" + parts[1]
    }
}

/// How the sidebar's query is matched; pure, so it runs off the main actor.
enum ExplorerSearch {
    /// Case, diacritic and width-insensitive form of `text`; the dotless i of Turkish names becomes a plain i. The
    /// same folding the index uses, so a hit in the folder and a hit in the index agree.
    static func fold(_ text: String) -> String {
        text.folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)
            .replacingOccurrences(of: "\u{131}", with: "i")
    }

    /// The current folder's subfolders and files whose name contains `query`, then the `indexHits` that are not
    /// already listed (compared by path, ignoring case).
    static func merge(
        query: String, folders: [ExplorerFolder], files: [IndexedMediaFile], indexHits: [IndexedMediaFile]
    ) -> (folders: [ExplorerFolder], files: [IndexedMediaFile]) {
        let needle = fold(query.trimmingCharacters(in: .whitespacesAndNewlines))
        guard !needle.isEmpty else { return (folders, files) }
        let matchingFolders = folders.filter { fold($0.name).contains(needle) }
        var seen: Set<String> = []
        var merged: [IndexedMediaFile] = []
        for file in files where fold(file.fileName).contains(needle) {
            if seen.insert(file.fullPath.lowercased()).inserted { merged.append(file) }
        }
        for hit in indexHits where seen.insert(hit.fullPath.lowercased()).inserted {
            merged.append(hit)
        }
        return (matchingFolders, merged)
    }
}

/// What the explorer asks of the system, replaceable in tests.
struct ExplorerServices: Sendable {
    /// Movies, Downloads, Desktop and the mounted volumes.
    var quickLocations: @Sendable () -> [QuickLocation]
    /// Whether the volume at `/Volumes/<name>` is mounted. Only asked about folders below `/Volumes`.
    var isVolumeMounted: @Sendable (String) -> Bool
    var directoryExists: @Sendable (String) -> Bool
    /// The subtitle extension (`"srt"`) a listing of the video's folder offers for it, by the rule the index uses.
    var sidecarBadge: @Sendable (String) -> String?
    var moviesFolder: @Sendable () -> String?
    var homeFolder: @Sendable () -> String
    var searchDelay: Duration
    /// Follow disks being mounted and unmounted (`NSWorkspace`).
    var watchesVolumes: Bool

    static let system = ExplorerServices(
        quickLocations: { QuickLocations.current() },
        isVolumeMounted: { root in
            let fileManager = FileManager.default
            let mounted = fileManager.mountedVolumeURLs(includingResourceValuesForKeys: nil, options: []) ?? []
            if mounted.contains(where: { MediaPath.normalized($0) == root }) { return true }
            // The startup disk is listed as a link (`/Volumes/Macintosh HD` → `/`).
            return (try? fileManager.destinationOfSymbolicLink(atPath: root)) != nil
        },
        directoryExists: { path in
            var isDirectory: ObjCBool = false
            return FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory) && isDirectory.boolValue
        },
        sidecarBadge: { path in
            let url = URL(filePath: path, directoryHint: .notDirectory)
            let names = (try? FileManager.default.contentsOfDirectory(atPath: url.deletingLastPathComponent().path)) ?? []
            return SubtitleParser.subtitleBadgeExtension(
                forVideoStem: url.deletingPathExtension().lastPathComponent, in: names)
        },
        moviesFolder: {
            FileManager.default.urls(for: .moviesDirectory, in: .userDomainMask).first.map { MediaPath.normalized($0) }
        },
        homeFolder: { MediaPath.normalized(FileManager.default.homeDirectoryForCurrentUser) },
        searchDelay: .milliseconds(200),
        watchesVolumes: true)
}

/// State of the file sidebar: the folder being browsed, its listing, search, history and the highlight of the open
/// video. The listings come from `MediaLibraryIndex`, which does all file-system work off the main actor; this model only
/// decides what to show and when to ask the index to scan.
///
/// Navigating to a folder clears the old rows at once, fills in the cached listing, scans the folder and then crawls its
/// subtree in the background so that search spans it. Up, back and refresh scan only.
@MainActor
@Observable
final class FileExplorerModel {
    /// Same names as the Windows app's settings.
    nonisolated static let lastFolderKey = "explorerLastFolder"
    nonisolated static let showSidebarKey = "showFileExplorer"
    private static let crawlDepth = 2
    private static let crawlFolders = 64
    private static let searchLimit = 100

    let index: MediaLibraryIndex

    /// The folder being browsed (a `MediaPath`-normalized path).
    private(set) var currentFolder: String
    /// Folders left by navigating, most recent last. There is no forward stack.
    private(set) var backStack: [String] = []
    /// What the list shows: the current folder's rows, or the search result.
    private(set) var folders: [ExplorerFolder] = []
    private(set) var files: [IndexedMediaFile] = []
    private(set) var isScanning = false
    private(set) var availability: FolderAvailability = .available
    private(set) var totalIndexedFiles = 0
    private(set) var quickLocations: [QuickLocation] = []

    /// The sidebar is shown (when the window is not full screen); persistent, on by default.
    var isSidebarVisible: Bool {
        didSet {
            guard isSidebarVisible != oldValue else { return }
            defaults.set(isSidebarVisible, forKey: Self.showSidebarKey)
        }
    }

    /// The text in the search field. Matching starts shortly after typing stops.
    var searchQuery = "" {
        didSet {
            guard searchQuery != oldValue else { return }
            applyFilter(debounce: true)
        }
    }

    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let services: ExplorerServices
    /// The folder stored by the last session, as stored; `start()` checks that it is still there.
    @ObservationIgnored private let savedFolderPath: String?
    /// No folder was ever chosen (nothing saved, nothing navigated to), so the folder shown is only a default and the
    /// first video that is opened takes the explorer to its folder.
    @ObservationIgnored private var followsVideo: Bool
    /// Lower-case path of the open video, compared with the (already normalized) paths of the rows.
    @ObservationIgnored private(set) var activeVideoKey: String?
    @ObservationIgnored private var currentSubfolders: [ExplorerFolder] = []
    @ObservationIgnored private var currentFiles: [IndexedMediaFile] = []
    /// A scan of the current folder has been requested, by `start()` or by the user.
    @ObservationIgnored private var hasScanned = false
    @ObservationIgnored private var scanTask: Task<Void, Never>?
    @ObservationIgnored private var scanGeneration = 0
    @ObservationIgnored private var filterTask: Task<Void, Never>?
    @ObservationIgnored private var filterGeneration = 0
    @ObservationIgnored private var startTask: Task<Void, Never>?
    @ObservationIgnored private var changesTask: Task<Void, Never>?
    @ObservationIgnored private var volumeObservers: [NSObjectProtocol] = []

    init(
        index: MediaLibraryIndex = MediaLibraryIndex(), defaults: UserDefaults = .standard,
        services: ExplorerServices = .system
    ) {
        self.index = index
        self.defaults = defaults
        self.services = services
        isSidebarVisible = defaults.object(forKey: Self.showSidebarKey) as? Bool ?? true
        // Nothing here touches the file system: the saved folder may be on a network mount that no longer answers, and
        // the app's launch must not wait for it. The folder shown until `start()` has looked at it is the saved one as it
        // was stored, or the home folder.
        let saved = defaults.string(forKey: Self.lastFolderKey).flatMap { $0.isEmpty ? nil : $0 }
        savedFolderPath = saved
        followsVideo = saved == nil
        currentFolder = saved ?? services.homeFolder()
        isScanning = true
    }

    // MARK: Derived state

    var canGoBack: Bool { !backStack.isEmpty }

    var canGoUp: Bool { ExplorerPaths.parent(of: currentFolder) != nil }

    var isSearching: Bool { !trimmedQuery.isEmpty }

    var isEmpty: Bool { folders.isEmpty && files.isEmpty }

    private var trimmedQuery: String { searchQuery.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// The folder's name for the header; the app's own name for Movies, Downloads and Desktop.
    var currentFolderName: String {
        if let match = quickLocations.first(where: { ExplorerPaths.key($0.url.path) == ExplorerPaths.key(currentFolder) }) {
            return match.title
        }
        return ExplorerPaths.name(of: currentFolder)
    }

    var currentFolderURL: URL {
        URL(filePath: currentFolder, directoryHint: .isDirectory)
    }

    /// "12 medya · indekste 340 dosya".
    var statusSummary: String {
        L("%lld medya · indekste %lld dosya", files.count, totalIndexedFiles)
    }

    func isActive(_ file: IndexedMediaFile) -> Bool {
        guard let activeVideoKey else { return false }
        return file.fullPath.lowercased() == activeVideoKey
    }

    /// Whether `file` sits outside the folder being browsed (search results), so its row names the folder.
    func isOutsideCurrentFolder(_ file: IndexedMediaFile) -> Bool {
        file.directoryPath.lowercased() != currentFolder.lowercased()
    }

    // MARK: Launch

    /// Shows the last folder from the cache and rescans just that folder (no crawl); starts following the index and the
    /// mounted volumes. Runs once, however often and from wherever it is called, and returns when that scan is over.
    func start() async {
        if let startTask {
            await startTask.value
            return
        }
        let task = Task { await performStart() }
        startTask = task
        await task.value
    }

    private func performStart() async {
        observeIndexChanges()
        if services.watchesVolumes { observeVolumes() }
        Task { await refreshQuickLocations() }
        // The saved folder is looked at here, off the main actor: a stale network mount can make that wait for a long time.
        // The user may have been faster than the launch; a folder they chose stays.
        if !hasScanned {
            let saved = savedFolderPath
            let services = services
            let launch = await Task.detached { Self.launchFolder(saved: saved, services: services) }.value
            if !hasScanned {
                currentFolder = launch.folder
                followsVideo = launch.followsVideo
            }
        }
        totalIndexedFiles = await index.totalIndexedFiles
        guard !hasScanned else { return }
        await runScan(currentFolder, showCached: true, crawl: false)
    }

    // MARK: Navigation

    /// Opens `url`: clears the old rows, shows the cached listing, scans the folder and (by default) crawls below it.
    func navigate(to url: URL, crawl: Bool = true) async {
        await go(to: MediaPath.normalized(url), push: true, crawl: crawl)
    }

    func goUp() async {
        guard let parent = ExplorerPaths.parent(of: currentFolder) else { return }
        await go(to: parent, push: true, crawl: false)
    }

    func goBack() async {
        guard let previous = backStack.popLast() else { return }
        await go(to: previous, push: false, crawl: false)
    }

    /// Rescans the current folder (and re-reads the quick locations); the rows stay until the result is in.
    func refresh() async {
        Task { await refreshQuickLocations() }
        await rescanCurrent()
    }

    private func rescanCurrent() async {
        await runScan(currentFolder, showCached: false, crawl: false)
    }

    private func go(to folder: String, push: Bool, crawl: Bool) async {
        guard !folder.isEmpty else { return }
        if push, ExplorerPaths.key(currentFolder) != ExplorerPaths.key(folder) {
            backStack.append(currentFolder)
        }
        followsVideo = false
        // A query belongs to the folder it was typed in.
        if !searchQuery.isEmpty { searchQuery = "" }
        currentFolder = folder
        defaults.set(folder, forKey: Self.lastFolderKey)
        await runScan(folder, showCached: true, crawl: crawl)
    }

    // MARK: Following the open video

    /// Brings the explorer in step with the video that was opened (or closed, `nil`).
    ///
    /// - The row is highlighted in every case.
    /// - Its folder is current and the file is listed with the subtitle the folder offers now: nothing more happens.
    /// - Its folder is current otherwise: the folder is rescanned, so that a new file or subtitle shows.
    /// - No folder was ever chosen (only the default one is shown): the explorer goes there.
    /// - Another folder is current: that folder is scanned into the index and the explorer stays where it is.
    func sync(toVideo url: URL?) async {
        await start()
        guard let url else {
            activeVideoKey = nil
            return
        }
        let path = MediaPath.normalized(url)
        activeVideoKey = path.lowercased()
        guard let folder = ExplorerPaths.parent(of: path), isMounted(folder) else { return }
        if followsVideo, ExplorerPaths.key(currentFolder) != ExplorerPaths.key(folder) {
            await go(to: folder, push: false, crawl: true)
            return
        }
        followsVideo = false
        guard ExplorerPaths.key(currentFolder) == ExplorerPaths.key(folder) else {
            await scanOther(folder)
            return
        }
        if availability == .available, let listed = currentFiles.first(where: { $0.fullPath.lowercased() == path.lowercased() }) {
            let badge = services.sidecarBadge
            let offered = await Task.detached { badge(path) }.value
            let known = listed.sidecarExtension.map { ext in String(ext.drop { $0 == "." }) }
            if known == offered { return }
        }
        await rescanCurrent()
    }

    /// A subtitle was written next to a video: rescan that folder so the badge appears.
    func subtitleSaved(at url: URL) async {
        await start()
        guard let folder = ExplorerPaths.parent(of: MediaPath.normalized(url)), isMounted(folder) else { return }
        if ExplorerPaths.key(currentFolder) == ExplorerPaths.key(folder) {
            await rescanCurrent()
        } else {
            await scanOther(folder)
        }
    }

    /// Disks came or went: refresh the shortcuts, and follow the current folder's volume.
    func volumesChanged() async {
        await refreshQuickLocations()
        let folder = currentFolder
        let mounted = Self.mountAvailability(of: folder, services: services)
        if mounted == .volumeNotMounted, availability != .volumeNotMounted {
            scanTask?.cancel()
            scanGeneration &+= 1
            isScanning = false
            availability = mounted
            setListing(folders: [], files: [])
        } else if mounted == .available, availability == .volumeNotMounted {
            await runScan(folder, showCached: true, crawl: false)
        }
    }

    // MARK: Scanning

    /// Scans `folder` into the index without touching what the list shows.
    private func scanOther(_ folder: String) async {
        await index.scan(directory: URL(filePath: folder, directoryHint: .isDirectory))
        totalIndexedFiles = await index.totalIndexedFiles
        if isSearching { applyFilter(debounce: false) }
    }

    /// Starts the scan of the current folder, superseding one in progress, and returns when it is over (or was
    /// superseded). With `showCached` the old rows go first and the cached listing takes their place; without it the
    /// rows stay until the scan has replaced them.
    private func runScan(_ folder: String, showCached: Bool, crawl: Bool) async {
        hasScanned = true
        scanTask?.cancel()
        scanGeneration &+= 1
        let generation = scanGeneration
        availability = Self.mountAvailability(of: folder, services: services)
        if showCached || availability == .volumeNotMounted { setListing(folders: [], files: []) }
        guard availability == .available else {
            isScanning = false
            scanTask = nil
            return
        }
        isScanning = true
        let task = Task { await scan(folder, generation: generation, showCached: showCached, crawl: crawl) }
        scanTask = task
        await task.value
    }

    private func scan(_ folder: String, generation: Int, showCached: Bool, crawl: Bool) async {
        defer { if generation == scanGeneration { isScanning = false } }
        let url = URL(filePath: folder, directoryHint: .isDirectory)
        if showCached {
            let cached = await index.cachedSnapshot(url)
            guard generation == scanGeneration else { return }
            if let cached { setListing(cached) }
        }
        guard generation == scanGeneration, !Task.isCancelled else { return }
        let result = await index.scan(directory: url)
        guard generation == scanGeneration else { return }
        switch result.outcome {
        case .scanned: availability = .available
        case .missing: availability = .notFound
        case .unreadable: availability = .unreadable
        }
        setListing(result.snapshot)
        totalIndexedFiles = await index.totalIndexedFiles
        guard crawl, result.outcome == .scanned, generation == scanGeneration, !Task.isCancelled else { return }
        await index.crawl(from: url, maxDepth: Self.crawlDepth, maxFolders: Self.crawlFolders)
        guard generation == scanGeneration else { return }
        totalIndexedFiles = await index.totalIndexedFiles
        // The crawl may have added hits to the open search.
        if isSearching { applyFilter(debounce: false) }
    }

    private func setListing(_ snapshot: DirectorySnapshot) {
        setListing(folders: snapshot.subdirectories.map(ExplorerFolder.init(path:)), files: snapshot.mediaFiles)
    }

    private func setListing(folders: [ExplorerFolder], files: [IndexedMediaFile]) {
        currentSubfolders = folders
        currentFiles = files
        applyFilter(debounce: false)
    }

    // MARK: Search

    /// Shows the current listing, or with a query its matches merged with the index's. The matching happens off the main
    /// actor; typing restarts the wait, and a result that is overtaken by a newer one is dropped.
    private func applyFilter(debounce: Bool) {
        filterTask?.cancel()
        filterTask = nil
        filterGeneration &+= 1
        let query = trimmedQuery
        guard !query.isEmpty else {
            assign(folders: currentSubfolders, files: currentFiles)
            return
        }
        let generation = filterGeneration
        let localFolders = currentSubfolders
        let localFiles = currentFiles
        let delay = debounce ? services.searchDelay : nil
        let index = index
        filterTask = Task {
            if let delay { try? await Task.sleep(for: delay) }
            guard !Task.isCancelled, generation == filterGeneration else { return }
            let hits = await index.search(query, limit: Self.searchLimit)
            guard !Task.isCancelled, generation == filterGeneration else { return }
            let merged = await Task.detached(priority: .userInitiated) {
                ExplorerSearch.merge(query: query, folders: localFolders, files: localFiles, indexHits: hits)
            }.value
            guard !Task.isCancelled, generation == filterGeneration else { return }
            assign(folders: merged.folders, files: merged.files)
        }
    }

    /// Returns when the search in progress (if any) has been applied or dropped. A new query, a scan or a change of the
    /// index replaces the task while it is waited for, so it keeps waiting until the task it just finished is still the
    /// current one.
    func waitForSearch() async {
        while let task = filterTask {
            await task.value
            if filterTask == task { return }
        }
    }

    private func assign(folders newFolders: [ExplorerFolder], files newFiles: [IndexedMediaFile]) {
        if folders != newFolders { folders = newFolders }
        if files != newFiles { files = newFiles }
    }

    // MARK: Index and volumes

    private func observeIndexChanges() {
        guard changesTask == nil else { return }
        let changes = index.changes
        changesTask = Task { [weak self] in
            for await _ in changes {
                guard let self else { return }
                await indexChanged()
            }
        }
    }

    private func indexChanged() async {
        totalIndexedFiles = await index.totalIndexedFiles
        if isSearching { applyFilter(debounce: false) }
    }

    private func observeVolumes() {
        guard volumeObservers.isEmpty else { return }
        let center = NSWorkspace.shared.notificationCenter
        for name in [NSWorkspace.didMountNotification, NSWorkspace.didUnmountNotification] {
            volumeObservers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    Task { await self.volumesChanged() }
                }
            })
        }
    }

    func refreshQuickLocations() async {
        let provider = services.quickLocations
        quickLocations = await Task.detached { provider() }.value
    }

    // MARK: Helpers

    private func isMounted(_ folder: String) -> Bool {
        Self.mountAvailability(of: folder, services: services) == .available
    }

    private nonisolated static func mountAvailability(of folder: String, services: ExplorerServices) -> FolderAvailability {
        guard let root = ExplorerPaths.volumeRoot(of: folder), !services.isVolumeMounted(root) else { return .available }
        return .volumeNotMounted
    }

    /// Where the explorer starts: the last folder if it still exists (or sits on a disk that is only unplugged), else the
    /// default one, in which case the first video that is opened takes it to its own folder. Reads the file system, so it
    /// runs off the main actor.
    private nonisolated static func launchFolder(
        saved: String?, services: ExplorerServices
    ) -> (folder: String, followsVideo: Bool) {
        if let saved {
            let folder = MediaPath.normalized(saved)
            if !folder.isEmpty {
                let unplugged = mountAvailability(of: folder, services: services) == .volumeNotMounted
                if unplugged || services.directoryExists(folder) { return (folder, false) }
            }
        }
        return (defaultFolder(services: services), true)
    }

    /// Movies, else the home folder.
    private nonisolated static func defaultFolder(services: ExplorerServices) -> String {
        if let movies = services.moviesFolder(), services.directoryExists(movies) { return movies }
        return services.homeFolder()
    }
}

// MARK: - Titles

extension QuickLocation {
    /// The app-language name of the shortcut.
    var title: String {
        switch kind {
        case .movies: L("Filmler")
        case .downloads: L("İndirilenler")
        case .desktop: L("Masaüstü")
        case .volume(let name): name
        }
    }

    var symbolName: String {
        switch kind {
        case .movies: "film"
        case .downloads: "arrow.down.circle"
        case .desktop: "menubar.dock.rectangle"
        case .volume: "externaldrive"
        }
    }
}
