import Foundation

/// A media file recorded by `MediaLibraryIndex`. Field names and formats are those of the Windows app's
/// `media-index.json`: paths are plain strings (see `MediaPath`), times are UTC.
public struct IndexedMediaFile: Codable, Sendable, Hashable, Identifiable {
    public var fullPath: String
    public var fileName: String
    public var directoryPath: String
    public var sizeBytes: Int64
    public var lastWriteTimeUtc: Date
    public var hasSidecarSubtitle: Bool
    /// Lower-case with the dot (`".srt"`), from `SubtitleParser.subtitleBadgeExtension`; `nil` without a subtitle.
    public var sidecarExtension: String?
    public var indexedAtUtc: Date

    public var id: String { fullPath }
    public var url: URL { URL(filePath: fullPath, directoryHint: .notDirectory) }

    public init(
        fullPath: String, fileName: String, directoryPath: String, sizeBytes: Int64, lastWriteTimeUtc: Date,
        hasSidecarSubtitle: Bool, sidecarExtension: String?, indexedAtUtc: Date
    ) {
        self.fullPath = fullPath
        self.fileName = fileName
        self.directoryPath = directoryPath
        self.sizeBytes = sizeBytes
        self.lastWriteTimeUtc = lastWriteTimeUtc
        self.hasSidecarSubtitle = hasSidecarSubtitle
        self.sidecarExtension = sidecarExtension
        self.indexedAtUtc = indexedAtUtc
    }

    /// Writes `sidecarExtension` as an explicit `null` when absent, as the Windows app does.
    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(fullPath, forKey: .fullPath)
        try container.encode(fileName, forKey: .fileName)
        try container.encode(directoryPath, forKey: .directoryPath)
        try container.encode(sizeBytes, forKey: .sizeBytes)
        try container.encode(lastWriteTimeUtc, forKey: .lastWriteTimeUtc)
        try container.encode(hasSidecarSubtitle, forKey: .hasSidecarSubtitle)
        try container.encode(sidecarExtension, forKey: .sidecarExtension)
        try container.encode(indexedAtUtc, forKey: .indexedAtUtc)
    }
}

/// One scanned folder: its immediate subfolders and media files. This is also the unit the index persists.
public struct DirectorySnapshot: Codable, Sendable, Hashable {
    public var directoryPath: String
    /// Full paths, sorted.
    public var subdirectories: [String]
    /// Sorted by file name.
    public var mediaFiles: [IndexedMediaFile]
    public var scannedAtUtc: Date

    public var directoryURL: URL { URL(filePath: directoryPath, directoryHint: .isDirectory) }

    public init(directoryPath: String, subdirectories: [String], mediaFiles: [IndexedMediaFile], scannedAtUtc: Date) {
        self.directoryPath = directoryPath
        self.subdirectories = subdirectories
        self.mediaFiles = mediaFiles
        self.scannedAtUtc = scannedAtUtc
    }
}

/// What a scan found out about a folder; the app decides how (and whether) to tell the user.
public enum ScanOutcome: Sendable, Equatable {
    /// The folder was listed and its entry replaced.
    case scanned
    /// The folder is gone (or is not a folder any more); it and everything below it was dropped from the index.
    case missing
    /// The folder exists but could not be listed (permissions, a flaky volume); the previous entry was kept.
    case unreadable
}

public struct ScanResult: Sendable, Equatable {
    /// The new entry for `.scanned`, the kept one for `.unreadable` (empty if the folder was never indexed), empty for
    /// `.missing`.
    public let snapshot: DirectorySnapshot
    public let outcome: ScanOutcome

    public init(snapshot: DirectorySnapshot, outcome: ScanOutcome) {
        self.snapshot = snapshot
        self.outcome = outcome
    }
}

/// The paths the index stores and compares: standardized (`.` and `..` resolved), no trailing slash except for `/`,
/// and NFC-normalized so that the same name typed on different file systems is one name. Folders are compared
/// case-insensitively (`key`), but keep the case they were scanned with.
public enum MediaPath {
    /// `""` for a non-file URL.
    public static func normalized(_ url: URL) -> String {
        url.isFileURL ? normalized(url.path) : ""
    }

    /// `""` for an empty path.
    public static func normalized(_ path: String) -> String {
        guard !path.isEmpty else { return "" }
        return URL(fileURLWithPath: path).standardizedFileURL.path.precomposedStringWithCanonicalMapping
    }

    /// The case-insensitive identity of a folder.
    static func key(_ path: String) -> String {
        normalized(path).lowercased()
    }

    static func child(_ name: String, of directory: String) -> String {
        (directory == "/" ? "" : directory) + "/" + name.precomposedStringWithCanonicalMapping
    }

    /// The containing folder of a normalized path, `nil` for `/`.
    static func parent(of path: String) -> String? {
        guard path != "/", let slash = path.lastIndex(of: "/") else { return nil }
        return slash == path.startIndex ? "/" : String(path[..<slash])
    }

    /// File names order case-insensitively with digit runs as numbers (`Episode 2` before `Episode 10`), independent of
    /// the user's locale so that persisted order and tests don't depend on it. Ties fall back to the exact characters.
    static func orderedBefore(_ a: String, _ b: String) -> Bool {
        switch a.compare(b, options: [.caseInsensitive, .numeric]) {
        case .orderedAscending: true
        case .orderedDescending: false
        case .orderedSame: a < b
        }
    }
}

/// The on-disk form of `MediaLibraryIndex`: `{"version": 1, "updatedAtUtc": …, "directories": […]}`.
struct PersistedMediaIndex: Codable {
    static let currentVersion = 1

    var version = PersistedMediaIndex.currentVersion
    var updatedAtUtc: Date
    var directories: [DirectorySnapshot]
}

/// ISO 8601 UTC with fractional seconds on write; on read also whatever .NET writes (`…12:00:00.1234567+00:00`).
enum MediaIndexCoding {
    static func makeEncoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        encoder.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            try container.encode(date.formatted(Date.ISO8601FormatStyle(includingFractionalSeconds: true)))
        }
        return encoder
    }

    static func makeDecoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let text = try decoder.singleValueContainer().decode(String.self)
            if let date = try? Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse(text) { return date }
            if let date = try? Date.ISO8601FormatStyle().parse(text) { return date }
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Not an ISO 8601 date: \(text)"))
        }
        return decoder
    }
}
