import Foundation
@testable import LibraryKit

/// A temporary folder holding a media tree (`root`) next to the index file, removed when the test ends.
final class MediaIndexSandbox {
    let base: URL
    let root: URL
    let indexFile: URL

    init() throws {
        base = FileManager.default.temporaryDirectory.appending(path: "MediaIndexTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        root = base.appending(path: "Media", directoryHint: .isDirectory)
        indexFile = base.appending(path: "media-index.json")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    deinit {
        // Tests that revoke permissions restore them themselves; this only makes a failed run less sticky.
        for case let url as URL in FileManager.default.enumerator(at: base, includingPropertiesForKeys: nil) ?? NSEnumerator() {
            try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        }
        try? FileManager.default.removeItem(at: base)
    }

    func index() -> MediaLibraryIndex {
        MediaLibraryIndex(fileURL: indexFile)
    }

    /// `root/<relative>` (a folder URL).
    func folder(_ relative: String) throws -> URL {
        let url = root.appending(path: relative, directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    /// Creates `root/<relative>` (and its folders) with `size` zero bytes and, if given, that modification time.
    @discardableResult
    func file(_ relative: String, size: Int = 1, modified: Date? = nil) throws -> URL {
        let url = root.appending(path: relative, directoryHint: .notDirectory)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data(count: size).write(to: url)
        if let modified { try FileManager.default.setAttributes([.modificationDate: modified], ofItemAtPath: url.path) }
        return url
    }

    func remove(_ relative: String) throws {
        try FileManager.default.removeItem(at: root.appending(path: relative))
    }

    /// The path the index uses for `root/<relative>`.
    func path(_ relative: String = "") -> String {
        MediaPath.normalized(relative.isEmpty ? root : root.appending(path: relative))
    }
}
