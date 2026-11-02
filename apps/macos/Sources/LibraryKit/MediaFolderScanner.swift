import EngineKit
import Foundation

/// Lists one folder for `MediaLibraryIndex`. Pure file-system work, so it runs off the index's actor.
enum MediaFolderScanner {
    enum Listing: Sendable {
        case listed(DirectorySnapshot)
        case missing
        case unreadable
    }

    private static let resourceKeys: [URLResourceKey] = [
        .isDirectoryKey, .isPackageKey, .isHiddenKey, .isSymbolicLinkKey, .fileSizeKey, .contentModificationDateKey,
    ]

    /// One level of `path` (already `MediaPath`-normalized). Hidden and dot-prefixed entries are skipped for files and
    /// folders alike, and so are packages (`.app`, `.photoslibrary`, …) because they are files to the user. Every
    /// media file gets its subtitle badge from this single listing, by the rule in `SubtitleParser`.
    static func list(directory path: String, now: Date, fileManager: FileManager = .default) async -> Listing {
        // A vanished folder (or a vanished volume) must be told apart from one we may not read.
        var info = stat()
        guard stat(path, &info) == 0 else { return errno == ENOENT || errno == ENOTDIR ? .missing : .unreadable }
        guard info.st_mode & S_IFMT == S_IFDIR else { return .missing }

        let urls: [URL]
        do {
            urls = try fileManager.contentsOfDirectory(
                at: URL(filePath: path, directoryHint: .isDirectory), includingPropertiesForKeys: resourceKeys)
        } catch {
            return .unreadable
        }

        var subdirectories: [String] = []
        var fileNames: [String] = []
        var media: [(name: String, size: Int64, modified: Date)] = []
        for url in urls {
            let name = url.lastPathComponent
            guard !name.hasPrefix("."), var values = try? url.resourceValues(forKeys: Set(resourceKeys)),
                  values.isHidden != true
            else { continue }
            if values.isSymbolicLink == true {
                // A link counts as what it points to; a dangling one is skipped.
                guard fileManager.fileExists(atPath: url.path),
                      let target = try? url.resolvingSymlinksInPath().resourceValues(forKeys: Set(resourceKeys))
                else { continue }
                values = target
            }
            if values.isDirectory == true {
                if values.isPackage != true { subdirectories.append(MediaPath.child(name, of: path)) }
                continue
            }
            fileNames.append(name)
            if MediaFileTypes.isMedia(url) {
                media.append((name, Int64(values.fileSize ?? 0), values.contentModificationDate ?? Date(timeIntervalSince1970: 0)))
            }
        }

        let badges = SubtitleParser.SubtitleBadgeLookup(fileNames: fileNames)
        let files = media.map { item -> IndexedMediaFile in
            let stem = URL(filePath: item.name).deletingPathExtension().lastPathComponent
            let sidecar = badges.badgeExtension(forVideoStem: stem).map { "." + $0 }
            let fileName = item.name.precomposedStringWithCanonicalMapping
            return IndexedMediaFile(
                fullPath: MediaPath.child(fileName, of: path), fileName: fileName, directoryPath: path,
                sizeBytes: item.size, lastWriteTimeUtc: item.modified,
                hasSidecarSubtitle: sidecar != nil, sidecarExtension: sidecar, indexedAtUtc: now)
        }
        return .listed(DirectorySnapshot(
            directoryPath: path,
            subdirectories: subdirectories.sorted(by: MediaPath.orderedBefore),
            mediaFiles: files.sorted { MediaPath.orderedBefore($0.fileName, $1.fileName) },
            scannedAtUtc: now))
    }
}
