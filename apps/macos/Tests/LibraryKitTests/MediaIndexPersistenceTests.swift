import Foundation
import Testing
@testable import LibraryKit

/// The index file: schema, lazy loading, tolerance for odd files, and the change stream.
@Suite struct MediaIndexPersistenceTests {
    @Test func initDoesNoFileWork() async throws {
        let box = try MediaIndexSandbox()
        let folder = box.base.appending(path: "not/yet/created", directoryHint: .isDirectory)
        let file = folder.appending(path: "media-index.json")
        let index = MediaLibraryIndex(fileURL: file)
        #expect(index.fileURL == file)
        #expect(!FileManager.default.fileExists(atPath: folder.path))

        // Reading never writes either; only a change does, and then it creates the folder.
        #expect(await index.totalIndexedFiles == 0)
        #expect(await index.search("x").isEmpty)
        #expect(!FileManager.default.fileExists(atPath: folder.path))
        try box.file("a.mp4")
        await index.scan(directory: box.root)
        #expect(FileManager.default.fileExists(atPath: file.path))
    }

    @Test func defaultFileIsMediaIndexJSONInTheAppDataFolder() {
        #expect(MediaLibraryIndex.defaultFileURL.lastPathComponent == "media-index.json")
        #expect(MediaLibraryIndex.defaultFileURL.deletingLastPathComponent().standardizedFileURL
            == AppDataDirectory.url().standardizedFileURL)
    }

    @Test func fileFollowsTheWindowsSchema() async throws {
        let box = try MediaIndexSandbox()
        let folder = try box.folder("Shows")
        try box.file("Shows/b.mp4", size: 10)
        try box.file("Shows/b.srt")
        try box.file("Shows/a.mkv", size: 20)
        _ = try box.folder("Shows/Zed")
        _ = try box.folder("Shows/Alpha")
        await box.index().scan(directory: folder)

        let json = try #require(JSONSerialization.jsonObject(with: Data(contentsOf: box.indexFile)) as? [String: Any])
        #expect(Set(json.keys) == ["version", "updatedAtUtc", "directories"])
        #expect(json["version"] as? Int == 1)
        let updated = try #require(json["updatedAtUtc"] as? String)
        #expect(isISO8601UTC(updated))

        let directories = try #require(json["directories"] as? [[String: Any]])
        let directory = try #require(directories.first)
        #expect(directories.count == 1)
        #expect(Set(directory.keys) == ["directoryPath", "subdirectories", "mediaFiles", "scannedAtUtc"])
        #expect(directory["directoryPath"] as? String == box.path("Shows"))
        #expect(directory["subdirectories"] as? [String] == [box.path("Shows/Alpha"), box.path("Shows/Zed")])
        #expect(isISO8601UTC(try #require(directory["scannedAtUtc"] as? String)))

        let files = try #require(directory["mediaFiles"] as? [[String: Any]])
        #expect(files.map { $0["fileName"] as? String } == ["a.mkv", "b.mp4"])
        let a = files[0], b = files[1]
        let keys: Set<String> = [
            "fullPath", "fileName", "directoryPath", "sizeBytes", "lastWriteTimeUtc", "hasSidecarSubtitle", "sidecarExtension",
            "indexedAtUtc",
        ]
        #expect(Set(a.keys) == keys && Set(b.keys) == keys)
        #expect(a["fullPath"] as? String == box.path("Shows/a.mkv"))
        #expect(a["directoryPath"] as? String == box.path("Shows"))
        #expect(a["sizeBytes"] as? Int == 20)
        #expect(a["hasSidecarSubtitle"] as? Bool == false)
        #expect(a["sidecarExtension"] is NSNull)
        #expect(b["hasSidecarSubtitle"] as? Bool == true)
        #expect(b["sidecarExtension"] as? String == ".srt")
        #expect(isISO8601UTC(try #require(b["lastWriteTimeUtc"] as? String)))
        #expect(isISO8601UTC(try #require(b["indexedAtUtc"] as? String)))
    }

    @Test func readsAFileTheWindowsAppWrote() async throws {
        let box = try MediaIndexSandbox()
        let json = """
        {
          "version": 1,
          "updatedAtUtc": "2026-01-02T03:04:05.1234567+00:00",
          "directories": [
            {
              "directoryPath": "/Volumes/Movies/",
              "subdirectories": ["/Volumes/Movies/b", "/Volumes/Movies/A/", ""],
              "mediaFiles": [
                {
                  "fullPath": "/Volumes/Movies/zeta.mp4", "fileName": "zeta.mp4", "directoryPath": "/Volumes/Movies",
                  "sizeBytes": 5000000000, "lastWriteTimeUtc": "2026-01-02T03:04:05.1234567+00:00",
                  "hasSidecarSubtitle": true, "sidecarExtension": ".srt", "indexedAtUtc": "2026-01-02T03:04:05Z"
                },
                {
                  "fullPath": "/Volumes/Movies/alpha.mkv", "fileName": "alpha.mkv", "directoryPath": "/Volumes/Movies",
                  "sizeBytes": 1, "lastWriteTimeUtc": "2026-01-02T03:04:05+03:00",
                  "hasSidecarSubtitle": false, "sidecarExtension": null, "indexedAtUtc": "2026-01-02T03:04:05.5Z"
                }
              ],
              "scannedAtUtc": "2026-01-02T03:04:05.1234567+00:00"
            },
            { "directoryPath": "", "subdirectories": [], "mediaFiles": [], "scannedAtUtc": "2026-01-02T03:04:05Z" }
          ]
        }
        """
        try Data(json.utf8).write(to: box.indexFile)

        let index = box.index()
        let movies = URL(filePath: "/Volumes/movies")   // other case, no trailing slash
        let files = await index.cachedDirectory(movies)
        #expect(files.map(\.fileName) == ["alpha.mkv", "zeta.mp4"])
        #expect(files[1].sizeBytes == 5_000_000_000)
        #expect(files[1].sidecarExtension == ".srt")
        #expect(files[0].sidecarExtension == nil)
        #expect(await index.cachedSubdirectories(movies).map(\.path) == ["/Volumes/Movies/A", "/Volumes/Movies/b"])
        #expect(await index.totalIndexedFiles == 2)
        #expect(await index.indexedDirectories.map(\.path) == ["/Volumes/Movies"])

        let base = try Date.ISO8601FormatStyle().parse("2026-01-02T03:04:05Z")
        #expect(abs(files[1].lastWriteTimeUtc.timeIntervalSince(base) - 0.1234567) < 0.001)
        #expect(files[0].lastWriteTimeUtc == base.addingTimeInterval(-3 * 3600))
        #expect(await index.allFiles().map(\.fileName) == ["zeta.mp4", "alpha.mkv"])
    }

    @Test func unusableFilesStartAnEmptyIndexThatTheNextScanRewrites() async throws {
        let box = try MediaIndexSandbox()
        let bad: [String] = [
            "not json at all", "", "{\"version\": 1}", "[]", "{\"version\": 1, \"updatedAtUtc\": \"yesterday\", \"directories\": []}",
            "{\"version\": 2, \"updatedAtUtc\": \"2026-01-02T03:04:05Z\", \"directories\": [{\"directoryPath\": \"/x\", "
                + "\"subdirectories\": [], \"mediaFiles\": [], \"scannedAtUtc\": \"2026-01-02T03:04:05Z\"}]}",
        ]
        for contents in bad {
            try Data(contents.utf8).write(to: box.indexFile)
            let index = box.index()
            #expect(await index.indexedDirectories.isEmpty, "\(contents)")
            #expect(await index.totalIndexedFiles == 0)
        }

        try box.file("a.mp4")
        let index = box.index()
        await index.scan(directory: box.root)
        #expect(await box.index().totalIndexedFiles == 1)
    }

    @Test func interleavedScansOfDifferentFoldersAllEndUpInTheFile() async throws {
        let box = try MediaIndexSandbox()
        let names = (1...8).map { "Folder\($0)" }
        for name in names { try box.file("\(name)/clip.mp4") }
        let index = box.index()
        await withTaskGroup(of: Void.self) { group in
            for name in names {
                let url = box.root.appending(path: name)
                group.addTask { await index.scan(directory: url) }
            }
        }
        #expect(await index.totalIndexedFiles == 8)
        #expect(await box.index().totalIndexedFiles == 8)
    }

    // MARK: Changes

    @Test func changesYieldAfterTheWriteAndReachEverySubscriber() async throws {
        let box = try MediaIndexSandbox()
        try box.file("a.mp4")
        let index = box.index()
        var first = index.changes.makeAsyncIterator()
        var second = index.changes.makeAsyncIterator()

        await index.scan(directory: box.root)
        #expect(await first.next() != nil)
        #expect(await second.next() != nil)
        // By the time a subscriber hears of a change, it is on disk.
        #expect(await box.index().totalIndexedFiles == 1)

        try box.file("b.mp4")
        await index.crawl(from: box.root)
        #expect(await first.next() != nil)
        #expect(await box.index().totalIndexedFiles == 2)
    }

    @Test func manyChangesWhileBusyCoalesce() async throws {
        let box = try MediaIndexSandbox()
        try box.file("a.mp4")
        let index = box.index()
        var iterator = index.changes.makeAsyncIterator()
        for _ in 0..<5 { await index.scan(directory: box.root) }
        #expect(await iterator.next() != nil)

        // Only one event was kept: the next one arrives with the next change.
        try box.file("b.mp4")
        await index.scan(directory: box.root)
        #expect(await iterator.next() != nil)
        #expect(await box.index().totalIndexedFiles == 2)
    }

    @Test func changesEndWhenTheIndexIsGone() async throws {
        let box = try MediaIndexSandbox()
        func orphanedStream() -> AsyncStream<Void> {
            let index = box.index()
            return index.changes
        }
        var iterator = orphanedStream().makeAsyncIterator()
        #expect(await iterator.next() == nil)
    }

    // MARK: Paths

    @Test func pathsAreStandardizedWithoutTrailingSlashAndNFC() {
        #expect(MediaPath.normalized("/") == "/")
        #expect(MediaPath.normalized("/a/b/") == "/a/b")
        #expect(MediaPath.normalized("/a/b/../c/./d") == "/a/c/d")
        #expect(MediaPath.normalized("") == "")
        #expect(MediaPath.normalized(URL(string: "https://example.com/a")!) == "")
        #expect(MediaPath.normalized(URL(filePath: "/tmp/x/", directoryHint: .isDirectory)) == "/tmp/x")
        let nfd = "/a/Çalışma".decomposedStringWithCanonicalMapping
        #expect(MediaPath.normalized(nfd).unicodeScalars.elementsEqual("/a/Çalışma".precomposedStringWithCanonicalMapping.unicodeScalars))
        #expect(MediaPath.key("/A/B/") == MediaPath.key("/a/b"))
        #expect(MediaPath.parent(of: "/a/b") == "/a")
        #expect(MediaPath.parent(of: "/a") == "/")
        #expect(MediaPath.parent(of: "/") == nil)
        #expect(MediaPath.child("x", of: "/") == "/x")
        #expect(MediaPath.child("x", of: "/a") == "/a/x")
    }

    private func isISO8601UTC(_ text: String) -> Bool {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return text.hasSuffix("Z") && formatter.date(from: text) != nil
    }
}
