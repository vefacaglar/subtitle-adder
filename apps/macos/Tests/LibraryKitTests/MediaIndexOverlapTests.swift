import Foundation
import Testing
@testable import LibraryKit

/// A one-shot latch: `wait()` suspends until `open()`.
private final class Gate: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Never>?
    private var isOpen = false

    func wait() async {
        await withCheckedContinuation { continuation in
            lock.lock()
            if isOpen {
                lock.unlock()
                continuation.resume()
            } else {
                self.continuation = continuation
                lock.unlock()
            }
        }
    }

    func open() {
        lock.lock()
        isOpen = true
        let waiting = continuation
        continuation = nil
        lock.unlock()
        waiting?.resume()
    }
}

/// Answers each listing of a folder from a script, in call order, optionally holding the answer back behind a gate to
/// make a scan finish after one that began later.
private final class ScriptedFolders: @unchecked Sendable {
    struct Step {
        var listing: MediaFolderScanner.Listing
        var gate: Gate?
    }

    private let lock = NSLock()
    private var script: [String: [Step]] = [:]
    private var started: [String: Int] = [:]

    func script(_ path: String, _ steps: Step...) {
        lock.withLock { script[path, default: []].append(contentsOf: steps) }
    }

    func startedCount(_ path: String) -> Int { lock.withLock { started[path, default: 0] } }

    var lister: MediaLibraryIndex.Lister {
        { [self] path, _ in
            let step: Step? = lock.withLock {
                let index = started[path, default: 0]
                started[path] = index + 1
                let steps = script[path] ?? []
                return index < steps.count ? steps[index] : steps.last
            }
            await step?.gate?.wait()
            return step?.listing ?? .missing
        }
    }
}

private func file(_ name: String, in directory: String) -> IndexedMediaFile {
    IndexedMediaFile(
        fullPath: directory + "/" + name, fileName: name, directoryPath: directory, sizeBytes: 1,
        lastWriteTimeUtc: Date(timeIntervalSince1970: 1_700_000_000), hasSidecarSubtitle: false, sidecarExtension: nil,
        indexedAtUtc: Date(timeIntervalSince1970: 1_700_000_000))
}

private func listed(_ directory: String, subdirectories: [String] = [], files: [String] = []) -> MediaFolderScanner.Listing {
    .listed(DirectorySnapshot(
        directoryPath: directory, subdirectories: subdirectories, mediaFiles: files.map { file($0, in: directory) },
        scannedAtUtc: Date()))
}

private func waitUntil(_ condition: @Sendable () -> Bool) async {
    for _ in 0..<400 where !condition() { try? await Task.sleep(for: .milliseconds(5)) }
}

/// Two scans of the same folder in flight at once: whichever listed last wins, not whichever finishes last.
@Suite struct MediaIndexOverlapTests {
    let root = "/virtual/library"

    private func index(_ folders: ScriptedFolders) throws -> (MediaLibraryIndex, MediaIndexSandbox) {
        let sandbox = try MediaIndexSandbox()
        return (MediaLibraryIndex(fileURL: sandbox.indexFile, lister: folders.lister), sandbox)
    }

    @Test func anOlderScanThatFinishesLastDoesNotOverwriteAFresherListing() async throws {
        let folders = ScriptedFolders()
        let gate = Gate()
        folders.script(root, .init(listing: listed(root, files: ["old.mp4"]), gate: gate), .init(listing: listed(root, files: ["new.mp4"])))
        let (index, _) = try index(folders)

        let older = Task { await index.scan(directory: URL(filePath: root, directoryHint: .isDirectory)) }
        await waitUntil { folders.startedCount(root) == 1 }
        let fresher = await index.scan(directory: URL(filePath: root, directoryHint: .isDirectory))
        #expect(fresher.snapshot.mediaFiles.map(\.fileName) == ["new.mp4"])

        gate.open()
        let result = await older.value
        #expect(result.outcome == .scanned)
        #expect(result.snapshot.mediaFiles.map(\.fileName) == ["new.mp4"], "the stale scan reports the fresher listing")
        #expect(await index.cachedDirectory(URL(filePath: root)).map(\.fileName) == ["new.mp4"])
        #expect(await index.search("old").isEmpty)
        #expect(await index.totalIndexedFiles == 1)
    }

    @Test func aLateArrivalOfAnOlderListingOfTwoFilesLeavesTheFresherOneFile() async throws {
        let folders = ScriptedFolders()
        let gate = Gate()
        folders.script(root, .init(listing: listed(root, files: ["a.mp4", "b.mp4"]), gate: gate), .init(listing: listed(root, files: ["a.mp4"])))
        let (index, _) = try index(folders)
        let older = Task { await index.scan(directory: URL(filePath: root)) }
        await waitUntil { folders.startedCount(root) == 1 }
        await index.scan(directory: URL(filePath: root))
        gate.open()
        await older.value
        #expect(await index.cachedDirectory(URL(filePath: root)).map(\.fileName) == ["a.mp4"])
    }

    @Test func aStaleListingDoesNotDropAFolderAFresherScanFound() async throws {
        let folders = ScriptedFolders()
        let gate = Gate()
        let a = root + "/a", c = root + "/c"
        // The first listing of the root knows both subfolders; the later, slow one no longer shows c.
        folders.script(root, .init(listing: listed(root, subdirectories: [a, c])), .init(listing: listed(root, subdirectories: [a]), gate: gate))
        folders.script(c, .init(listing: listed(c, files: ["early.mp4"])), .init(listing: listed(c, files: ["fresh.mp4"])))
        let (index, _) = try index(folders)

        await index.scan(directory: URL(filePath: root))
        await index.scan(directory: URL(filePath: c))
        #expect(await index.cachedDirectory(URL(filePath: c)).map(\.fileName) == ["early.mp4"])

        // A slow rescan of the root starts; while it is listing, c is scanned again and is found.
        let slow = Task { await index.scan(directory: URL(filePath: root)) }
        await waitUntil { folders.startedCount(root) == 2 }
        await index.scan(directory: URL(filePath: c))

        gate.open()
        let result = await slow.value
        #expect(result.outcome == .scanned)
        #expect(await index.cachedDirectory(URL(filePath: c)).map(\.fileName) == ["fresh.mp4"], "the fresher folder survives")
        #expect(await index.cachedSubdirectories(URL(filePath: root)).map { MediaPath.normalized($0) } == [a, c])
        #expect(result.snapshot.subdirectories == [a, c])
    }

    @Test func aFolderNoScanHasSeenSinceIsStillDroppedWhenALaterListingLacksIt() async throws {
        let folders = ScriptedFolders()
        let a = root + "/a", c = root + "/c"
        folders.script(root, .init(listing: listed(root, subdirectories: [a, c])), .init(listing: listed(root, subdirectories: [a])))
        folders.script(c, .init(listing: listed(c, files: ["x.mp4"])))
        let (index, _) = try index(folders)
        await index.scan(directory: URL(filePath: root))
        await index.scan(directory: URL(filePath: c))
        await index.scan(directory: URL(filePath: root))
        #expect(await index.cachedSnapshot(URL(filePath: c)) == nil)
        #expect(await index.cachedSubdirectories(URL(filePath: root)).map { MediaPath.normalized($0) } == [a])
    }

    @Test func aStaleGoneResultDoesNotRemoveAFolderTheFresherScanFound() async throws {
        let folders = ScriptedFolders()
        let gate = Gate()
        folders.script(root, .init(listing: .missing, gate: gate), .init(listing: listed(root, files: ["back.mp4"])))
        let (index, _) = try index(folders)

        let stale = Task { await index.scan(directory: URL(filePath: root)) }
        await waitUntil { folders.startedCount(root) == 1 }
        await index.scan(directory: URL(filePath: root))
        gate.open()
        let result = await stale.value
        #expect(result.outcome == .scanned)
        #expect(await index.cachedDirectory(URL(filePath: root)).map(\.fileName) == ["back.mp4"])
    }

    @Test func aStaleListingDoesNotBringBackAFolderAFresherScanFoundMissing() async throws {
        let folders = ScriptedFolders()
        let gate = Gate()
        let sub = root + "/sub"
        folders.script(root, .init(listing: listed(root, subdirectories: [sub])))
        folders.script(sub, .init(listing: listed(sub, files: ["x.mp4"]), gate: gate), .init(listing: .missing))
        let (index, _) = try index(folders)
        await index.scan(directory: URL(filePath: root))

        // The slow scan of sub listed the folder before it was deleted; the fresher one found it gone.
        let stale = Task { await index.scan(directory: URL(filePath: sub)) }
        await waitUntil { folders.startedCount(sub) == 1 }
        let gone = await index.scan(directory: URL(filePath: sub))
        #expect(gone.outcome == .missing)
        gate.open()
        let result = await stale.value
        #expect(result.outcome == .missing, "the stale scan reports the fresher state")
        #expect(await index.cachedSnapshot(URL(filePath: sub)) == nil)
        #expect(await index.cachedSubdirectories(URL(filePath: root)).isEmpty)
    }

    @Test func scansOfDifferentFoldersNeverSupersedeEachOther() async throws {
        let folders = ScriptedFolders()
        let gate = Gate()
        let other = "/virtual/other"
        folders.script(root, .init(listing: listed(root, files: ["a.mp4"]), gate: gate))
        folders.script(other, .init(listing: listed(other, files: ["b.mp4"])))
        let (index, _) = try index(folders)
        let slow = Task { await index.scan(directory: URL(filePath: root)) }
        await waitUntil { folders.startedCount(root) == 1 }
        await index.scan(directory: URL(filePath: other))
        gate.open()
        await slow.value
        #expect(await index.totalIndexedFiles == 2)
    }
}
