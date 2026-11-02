import Foundation
import Synchronization

/// The persistent index of the media files on the user's disks, behind the file sidebar and the video platform's
/// "Explore" feed: folders are scanned one level at a time, cached in memory and in `media-index.json`, so browsing
/// shows the last known listing at once, a rescan only corrects it, and search spans every folder seen so far.
/// Port of the Windows app's `MediaLibraryIndex` with the same JSON file.
///
/// An actor, so file work never lands on the main actor: the index file is read on first use (not in `init`), and
/// folders are listed off the actor, which stays responsive to queries during a crawl. Persistence is best effort:
/// the index is a cache, so a failed write loses nothing a rescan can't rebuild. Byte counts are left as numbers;
/// formatting them is the app's job.
public actor MediaLibraryIndex {
    public nonisolated let fileURL: URL

    /// Folders by `MediaPath.key`, with the search keys precomputed so that typing in the search field doesn't fold
    /// every name again.
    private struct Entry {
        var snapshot: DirectorySnapshot
        var foldedPath: String
        var foldedNames: [String]

        init(_ snapshot: DirectorySnapshot) {
            self.snapshot = snapshot
            foldedPath = MediaLibraryIndex.fold(snapshot.directoryPath)
            foldedNames = snapshot.mediaFiles.map { MediaLibraryIndex.fold($0.fileName) }
        }
    }

    private var entries: [String: Entry] = [:]
    private var isLoaded = false
    private let subscribers = ChangeSubscribers()

    /// Lists one folder; `MediaFolderScanner.list` except in tests, which script the order scans finish in.
    typealias Lister = @Sendable (_ path: String, _ now: Date) async -> MediaFolderScanner.Listing
    private let lister: Lister

    /// Scans can overlap (the actor is free while a folder is being listed), and a listing says only how the folder
    /// looked when its scan began. Every scan takes a ticket when it begins; `appliedScan` holds, per folder, the ticket
    /// of the newest scan whose result was applied (a "folder is gone" result included). A result whose ticket is
    /// older than that is stale and dropped, so the order scans finish in never decides which listing wins. A
    /// counter rather than timestamps: it cannot be thrown off by the system clock changing. Not persisted.
    private var scanCounter = 0
    private var appliedScan: [String: Int] = [:]

    /// `<Application Support>/SubtitleAdder/media-index.json`.
    public static var defaultFileURL: URL {
        AppDataDirectory.url().appending(path: "media-index.json", directoryHint: .notDirectory)
    }

    /// Nothing is read or created here; the file is loaded when the index is first asked something.
    public init(fileURL: URL = MediaLibraryIndex.defaultFileURL) {
        self.fileURL = fileURL
        lister = { path, now in await MediaFolderScanner.list(directory: path, now: now) }
    }

    init(fileURL: URL, lister: @escaping Lister) {
        self.fileURL = fileURL
        self.lister = lister
    }

    deinit {
        subscribers.finishAll()
    }

    // MARK: Changes

    /// Yields once after every change to the index (a scan or crawl that replaced or removed entries, written to disk
    /// by then), so a view model can refresh. Every access makes an independent stream; changes that happen while the
    /// consumer is busy coalesce into one.
    public nonisolated var changes: AsyncStream<Void> {
        subscribers.makeStream()
    }

    // MARK: Queries

    /// Every folder the index holds, sorted by path.
    public var indexedDirectories: [URL] {
        ensureLoaded()
        return entries.values.map(\.snapshot.directoryPath).sorted(by: MediaPath.orderedBefore).map {
            URL(filePath: $0, directoryHint: .isDirectory)
        }
    }

    public var totalIndexedFiles: Int {
        ensureLoaded()
        return entries.values.reduce(0) { $0 + $1.snapshot.mediaFiles.count }
    }

    /// The last scan of `directory`, or `nil` if it was never indexed.
    public func cachedSnapshot(_ directory: URL) -> DirectorySnapshot? {
        ensureLoaded()
        return entries[MediaPath.key(directory.path)]?.snapshot
    }

    /// The media files of `directory` as last scanned, for instant display; empty if it was never indexed.
    public func cachedDirectory(_ directory: URL) -> [IndexedMediaFile] {
        cachedSnapshot(directory)?.mediaFiles ?? []
    }

    /// The subfolders of `directory` as last scanned, sorted; empty if it was never indexed.
    public func cachedSubdirectories(_ directory: URL) -> [URL] {
        (cachedSnapshot(directory)?.subdirectories ?? []).map { URL(filePath: $0, directoryHint: .isDirectory) }
    }

    /// Files whose name starts with the trimmed `query` (score 3), contains it (2), or lies in a folder whose path
    /// contains it (1), ignoring case and diacritics (also the Turkish dotless i). Best score first, then newest, then
    /// by name. A blank query finds nothing: use `allFiles` to list everything.
    public func search(_ query: String, limit: Int = 100) -> [IndexedMediaFile] {
        ensureLoaded()
        let needle = Self.fold(query.trimmingCharacters(in: .whitespacesAndNewlines))
        guard !needle.isEmpty, limit > 0 else { return [] }
        var hits: [(file: IndexedMediaFile, score: Int, name: String)] = []
        for entry in entries.values {
            let pathMatches = entry.foldedPath.contains(needle)
            for (file, name) in zip(entry.snapshot.mediaFiles, entry.foldedNames) {
                let score = name.hasPrefix(needle) ? 3 : name.contains(needle) ? 2 : pathMatches ? 1 : 0
                if score > 0 { hits.append((file, score, name)) }
            }
        }
        hits.sort {
            if $0.score != $1.score { return $0.score > $1.score }
            if $0.file.lastWriteTimeUtc != $1.file.lastWriteTimeUtc { return $0.file.lastWriteTimeUtc > $1.file.lastWriteTimeUtc }
            return $0.name != $1.name ? $0.name < $1.name : $0.file.fullPath < $1.file.fullPath
        }
        return hits.prefix(limit).map(\.file)
    }

    /// Every indexed file, most recently modified first (then by name), at most `limit`.
    public func allFiles(limit: Int = .max) -> [IndexedMediaFile] {
        ensureLoaded()
        guard limit > 0 else { return [] }
        let files = entries.values.flatMap(\.snapshot.mediaFiles)
        let sorted = files.sorted {
            if $0.lastWriteTimeUtc != $1.lastWriteTimeUtc { return $0.lastWriteTimeUtc > $1.lastWriteTimeUtc }
            return $0.fileName != $1.fileName ? MediaPath.orderedBefore($0.fileName, $1.fileName) : $0.fullPath < $1.fullPath
        }
        return Array(sorted.prefix(limit))
    }

    // MARK: Scanning

    /// Lists `directory` (one level, no recursion), replaces its entry and writes the index. A folder that no longer
    /// exists is dropped together with everything below it; one that can't be listed keeps its previous entry, so a
    /// permission problem never shrinks the cache. Subfolders that have disappeared from the listing are dropped
    /// with their subtrees too.
    @discardableResult
    public func scan(directory: URL) async -> ScanResult {
        ensureLoaded()
        let (result, changed) = await scanFolder(MediaPath.normalized(directory))
        if changed { persistAndNotify() }
        return result
    }

    /// Breadth-first `scan` of `root` and its subfolders down to `maxDepth` (0...5, `root` is depth 0), at most
    /// `maxFolders` folders. Cancelling the task stops at the next folder boundary and keeps what was scanned: the
    /// index is written once at the end either way. `progress` receives the number of folders handled so far after
    /// each one. Returns the number of folders that were listed.
    @discardableResult
    public func crawl(
        from root: URL, maxDepth: Int = 2, maxFolders: Int = 64, progress: (@Sendable (Int) -> Void)? = nil
    ) async -> Int {
        ensureLoaded()
        let depthLimit = min(max(maxDepth, 0), 5)
        let rootPath = MediaPath.normalized(root)
        var queue = [(path: rootPath, depth: 0)]
        var visited: Set<String> = [MediaPath.key(rootPath)]
        var next = 0, listed = 0, changed = false
        while next < queue.count, next < maxFolders, !Task.isCancelled {
            let (path, depth) = queue[next]
            next += 1
            let (result, didChange) = await scanFolder(path)
            changed = changed || didChange
            progress?(next)
            guard result.outcome == .scanned else { continue }
            listed += 1
            if depth < depthLimit {
                for sub in result.snapshot.subdirectories where visited.insert(MediaPath.key(sub)).inserted {
                    queue.append((sub, depth + 1))
                }
            }
        }
        if changed { persistAndNotify() }
        return listed
    }

    /// Scans one normalized folder into memory without writing; `changed` says whether memory was modified. A result
    /// that a scan begun later has already superseded (see `scanCounter`) changes nothing and reports that newer state.
    private func scanFolder(_ path: String) async -> (result: ScanResult, changed: Bool) {
        let now = Date()
        let empty = DirectorySnapshot(directoryPath: path, subdirectories: [], mediaFiles: [], scannedAtUtc: now)
        guard !path.isEmpty else { return (ScanResult(snapshot: empty, outcome: .missing), false) }
        scanCounter += 1
        let ticket = scanCounter
        let key = MediaPath.key(path)

        let listing = await lister(path, now)

        switch listing {
        case .missing:
            // Gone as of this scan's start; a newer scan that saw the folder (or anything below it) wins.
            if isSuperseded(key, by: ticket) || hasNewerEntry(under: key, than: ticket) { return (current(key, fallback: empty), false) }
            return (ScanResult(snapshot: empty, outcome: .missing), removeSubtree(path, ticket: ticket))
        case .unreadable:
            let kept = entries[key]?.snapshot ?? empty
            return (ScanResult(snapshot: kept, outcome: .unreadable), false)
        case .listed(var snapshot):
            if isSuperseded(key, by: ticket) { return (current(key, fallback: empty), false) }
            let previous = entries[key]?.snapshot.subdirectories ?? []
            let listed = Set(snapshot.subdirectories.map(MediaPath.key))
            // Subfolders this listing no longer shows are dropped with their subtrees, except those a scan that began
            // after this one has found since (they were created after this listing was taken).
            var gone: [String] = []
            var kept: [String] = []
            for old in previous where !listed.contains(MediaPath.key(old)) {
                if hasNewerEntry(under: MediaPath.key(old), than: ticket) { kept.append(old) } else { gone.append(old) }
            }
            if !kept.isEmpty { snapshot.subdirectories = (snapshot.subdirectories + kept).sorted(by: MediaPath.orderedBefore) }
            entries[key] = Entry(snapshot)
            appliedScan[key] = ticket
            for old in gone { removeSubtree(old, ticket: ticket) }
            return (ScanResult(snapshot: snapshot, outcome: .scanned), true)
        }
    }

    private func isSuperseded(_ key: String, by ticket: Int) -> Bool {
        (appliedScan[key] ?? 0) > ticket
    }

    /// Whether a scan that began after `ticket` has applied a listing of the folder `key` or of any folder below it that
    /// is still in the index.
    private func hasNewerEntry(under key: String, than ticket: Int) -> Bool {
        let prefix = key == "/" ? "/" : key + "/"
        return entries.keys.contains { ($0 == key || $0.hasPrefix(prefix)) && (appliedScan[$0] ?? 0) > ticket }
    }

    /// The state a newer scan left behind for a stale result to report.
    private func current(_ key: String, fallback: DirectorySnapshot) -> ScanResult {
        guard let entry = entries[key] else { return ScanResult(snapshot: fallback, outcome: .missing) }
        return ScanResult(snapshot: entry.snapshot, outcome: .scanned)
    }

    /// Drops the entry for `path` and every entry below it, and `path` from its parent's subfolder list. The dropped
    /// folders keep `ticket` as their newest applied scan, so an older listing of them that finishes later is ignored.
    @discardableResult
    private func removeSubtree(_ path: String, ticket: Int) -> Bool {
        let key = MediaPath.key(path)
        let prefix = key == "/" ? "/" : key + "/"
        let doomed = entries.keys.filter { $0 == key || $0.hasPrefix(prefix) }
        for name in doomed {
            entries[name] = nil
            appliedScan[name] = ticket
        }
        appliedScan[key] = ticket
        var changed = !doomed.isEmpty
        if let parent = MediaPath.parent(of: MediaPath.normalized(path)).map(MediaPath.key),
           var entry = entries[parent],
           entry.snapshot.subdirectories.contains(where: { MediaPath.key($0) == key }) {
            entry.snapshot.subdirectories.removeAll { MediaPath.key($0) == key }
            entries[parent] = entry
            changed = true
        }
        return changed
    }

    // MARK: Persistence

    private func ensureLoaded() {
        guard !isLoaded else { return }
        isLoaded = true
        // A missing, unreadable, corrupt or newer-format file starts an empty index; the next scan rewrites it.
        guard let data = try? Data(contentsOf: fileURL),
              let persisted = try? MediaIndexCoding.makeDecoder().decode(PersistedMediaIndex.self, from: data),
              persisted.version <= PersistedMediaIndex.currentVersion
        else { return }
        for var snapshot in persisted.directories {
            snapshot.directoryPath = MediaPath.normalized(snapshot.directoryPath)
            guard !snapshot.directoryPath.isEmpty else { continue }
            snapshot.subdirectories = snapshot.subdirectories.map(MediaPath.normalized).filter { !$0.isEmpty }
                .sorted(by: MediaPath.orderedBefore)
            snapshot.mediaFiles = snapshot.mediaFiles.filter { !$0.fullPath.isEmpty }
                .sorted { MediaPath.orderedBefore($0.fileName, $1.fileName) }
            entries[MediaPath.key(snapshot.directoryPath)] = Entry(snapshot)
        }
    }

    /// Writes atomically (a reader never sees half a file), then tells the subscribers.
    private func persistAndNotify() {
        let payload = PersistedMediaIndex(
            updatedAtUtc: Date(),
            directories: entries.values.map(\.snapshot).sorted { MediaPath.orderedBefore($0.directoryPath, $1.directoryPath) })
        do {
            try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            try MediaIndexCoding.makeEncoder().encode(payload).write(to: fileURL, options: .atomic)
        } catch {
            // Best effort, see the type's documentation.
        }
        subscribers.yield()
    }

    // MARK: Search keys

    /// Case, diacritic and width-insensitive form of `text`; the dotless i of Turkish file names becomes a plain i
    /// (`TextFolding`, shared with the profile search).
    nonisolated static func fold(_ text: String) -> String {
        TextFolding.fold(text)
    }
}

/// The streams handed out by `MediaLibraryIndex.changes`. Kept outside the actor so that creating a stream is
/// synchronous.
private final class ChangeSubscribers: Sendable {
    private let continuations = Mutex<[UUID: AsyncStream<Void>.Continuation]>([:])

    func makeStream() -> AsyncStream<Void> {
        AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
            let id = UUID()
            continuations.withLock { $0[id] = continuation }
            continuation.onTermination = { [self] _ in
                continuations.withLock { $0[id] = nil }
            }
        }
    }

    func yield() {
        let current = continuations.withLock { Array($0.values) }
        for continuation in current { continuation.yield() }
    }

    func finishAll() {
        let current = continuations.withLock { dict in
            defer { dict.removeAll() }
            return Array(dict.values)
        }
        for continuation in current { continuation.finish() }
    }
}
