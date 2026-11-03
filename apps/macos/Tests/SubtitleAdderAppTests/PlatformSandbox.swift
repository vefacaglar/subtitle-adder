import Foundation
import LibraryKit
import Synchronization
@testable import SubtitleAdderApp

/// A temporary media tree with its own profiles file, secret store and media index, removed when the test ends. Nothing
/// here touches the real Application Support folder or the keychain.
@MainActor
final class PlatformSandbox {
    let base: URL
    let media: URL
    let profilesFile: URL
    let indexFile: URL
    let secrets = InMemorySecretStore()
    private let mounts = MountSet()
    private let listings = ListingCounts()

    init() throws {
        base = FileManager.default.temporaryDirectory.appending(path: "PlatformTests-\(UUID().uuidString)", directoryHint: .isDirectory)
            .standardizedFileURL
        media = base.appending(path: "Media", directoryHint: .isDirectory)
        profilesFile = base.appending(path: "profiles.json")
        indexFile = base.appending(path: "media-index.json")
        try FileManager.default.createDirectory(at: media, withIntermediateDirectories: true)
    }

    deinit {
        try? FileManager.default.removeItem(at: base)
    }

    // MARK: Files

    /// Creates `Media/<relative>` with content of its own (so every file has its own fingerprint).
    @discardableResult
    func file(_ relative: String, content: String? = nil) throws -> URL {
        let url = media.appending(path: relative)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data((content ?? relative).utf8).write(to: url)
        return url
    }

    func mount(_ root: String) { mounts.roots.withLock { _ = $0.insert(root) } }
    func unmount(_ root: String) { mounts.roots.withLock { _ = $0.remove(root) } }

    /// How many times the folder at `path` was listed for subtitle badges.
    func listingCount(of path: String) -> Int { listings.counts.withLock { $0[path] ?? 0 } }

    // MARK: Objects

    func makeStore(watches: Bool = false, secrets: (any SecretStore)? = nil) -> ProfileStore {
        ProfileStore(fileURL: profilesFile, secretStore: secrets ?? self.secrets, watchesFile: watches)
    }

    func makeIndex() -> MediaLibraryIndex { MediaLibraryIndex(fileURL: indexFile) }

    func services(discoveredLimit: Int = 500, progressInterval: Duration = .milliseconds(30)) -> PlatformServices {
        let mounts = mounts, listings = listings
        return PlatformServices(
            isVolumeMounted: { root in mounts.roots.withLock { $0.contains(root) } },
            directoryExists: { path in
                // Pretend every folder below /Volumes exists; whether its volume is mounted is the mount check's business.
                if path.hasPrefix("/Volumes/") { return true }
                var isDirectory: ObjCBool = false
                return FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory) && isDirectory.boolValue
            },
            fileExists: { path in
                // Like the folders below /Volumes, the files there are taken to exist while their volume is mounted.
                if path.hasPrefix("/Volumes/") { return true }
                return FileManager.default.fileExists(atPath: path)
            },
            listDirectory: { path in
                listings.counts.withLock { $0[path, default: 0] += 1 }
                return (try? FileManager.default.contentsOfDirectory(atPath: path)) ?? []
            },
            discoveredLimit: discoveredLimit, progressInterval: progressInterval)
    }

    func model(
        store: ProfileStore? = nil, index: MediaLibraryIndex? = nil, services: PlatformServices? = nil,
        thumbnailGenerator: @escaping ThumbnailService.Generator = { _ in nil }
    ) -> VideoPlatformModel {
        VideoPlatformModel(
            store: store ?? makeStore(), index: index ?? makeIndex(), services: services ?? self.services(),
            thumbnailGenerator: thumbnailGenerator)
    }

    /// A model with the hub open and a profile registered and logged in.
    func loggedInModel(
        username: String = "Ayşe", password: String = "pw", index: MediaLibraryIndex? = nil,
        services: PlatformServices? = nil
    ) async -> VideoPlatformModel {
        let model = model(index: index, services: services)
        model.isHubActive = true
        await model.start()
        _ = await model.register(username: username, password: password)
        return model
    }
}

private final class MountSet: Sendable {
    let roots = Mutex<Set<String>>([])
}

private final class ListingCounts: Sendable {
    let counts = Mutex<[String: Int]>([:])
}

/// Polls on the main actor until `condition` holds or `timeout` seconds pass.
@MainActor
func waitFor(timeout: Double = 5, _ condition: @MainActor () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(for: .milliseconds(20))
    }
    return condition()
}
