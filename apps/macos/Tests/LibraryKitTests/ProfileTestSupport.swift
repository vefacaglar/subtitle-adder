import Foundation
import Testing
@testable import LibraryKit

/// A throwaway folder for one test, removed when the test's suite instance goes away.
final class TestFolder: @unchecked Sendable {
    let url: URL

    init() {
        url = FileManager.default.temporaryDirectory
            .appending(path: "LibraryKitTests-\(UUID().uuidString)", directoryHint: .isDirectory)
            .standardizedFileURL
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    }

    deinit {
        // A test may have made the folder read-only.
        chmod(url.path, 0o755)
        try? FileManager.default.removeItem(at: url)
    }

    var profilesFile: URL { url.appending(path: "profiles.json") }

    /// A path inside the folder (the file need not exist).
    func video(_ name: String) -> URL { url.appending(path: name) }

    func makeStore(
        secrets: any SecretStore = InMemorySecretStore(), watches: Bool = false, lockTimeout: TimeInterval = 5,
        clock: @escaping @Sendable () -> Date = { Date() }
    ) -> ProfileStore {
        ProfileStore(fileURL: profilesFile, secretStore: secrets, watchesFile: watches, lockTimeout: lockTimeout, clock: clock)
    }

    /// The files of the picture cache (`thumbnails` next to the profiles file), at any depth.
    func cacheFiles() throws -> [URL] {
        let root = url.appending(path: "thumbnails", directoryHint: .isDirectory)
        guard let walker = FileManager.default.enumerator(at: root, includingPropertiesForKeys: [.isRegularFileKey]) else { return [] }
        return walker.compactMap { $0 as? URL }.filter { (try? $0.resourceValues(forKeys: [.isRegularFileKey]))?.isRegularFile == true }
    }

    func rawProfiles() throws -> String {
        String(decoding: try Data(contentsOf: profilesFile), as: UTF8.self)
    }

    func profilesObject() throws -> [String: Any] {
        try #require(JSONSerialization.jsonObject(with: Data(contentsOf: profilesFile)) as? [String: Any])
    }
}

/// The path a store reports for a URL (normalized, absolute).
func normalized(_ url: URL) -> String { VideoFingerprint.normalizedPath(url) }

/// Collects what a store's change stream delivers.
final class ChangeLog: @unchecked Sendable {
    private let lock = NSLock()
    private var items: [ProfileChange] = []
    private var task: Task<Void, Never>?

    init(_ store: ProfileStore) {
        let stream = store.changes()
        task = Task { [weak self] in
            for await change in stream { self?.lock.withLock { self?.items.append(change) } }
        }
    }

    var events: [ProfileChange] { lock.withLock { items } }

    func stop() { task?.cancel() }

    deinit { task?.cancel() }
}

/// Polls until `condition` holds or `timeout` passes.
func eventually(timeout: TimeInterval = 4, _ condition: () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(for: .milliseconds(25))
    }
    return condition()
}

/// A clock the test advances by hand.
final class ManualClock: @unchecked Sendable {
    private let lock = NSLock()
    private var now: Date

    init(_ start: Date = Date(timeIntervalSince1970: 1_800_000_000)) { now = start }

    func advance(_ seconds: TimeInterval = 1) { lock.withLock { now = now.addingTimeInterval(seconds) } }

    var date: Date { lock.withLock { now } }
}
