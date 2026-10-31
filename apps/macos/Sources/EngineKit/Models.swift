import Foundation

public struct Word: Sendable, Hashable, Codable {
    public var start: Double
    public var end: Double
    public var text: String

    public init(start: Double, end: Double, text: String) {
        self.start = start
        self.end = end
        self.text = text
    }

    /// Midpoint of the word's time range; used to pick the split point.
    var midpoint: Double { (start + end) / 2 }
}

public struct Sentence: Sendable, Hashable, Codable {
    /// Displayed sequence number (starts at 1); renumbered after merging/deleting.
    public var id: Int
    /// View identity and selection key. Does not change when `id` is renumbered and is not sent to the engine.
    public let uid: UUID
    public var start: Double
    public var end: Double
    public var text: String
    public var words: [Word]

    public init(id: Int, uid: UUID = UUID(), start: Double, end: Double, text: String, words: [Word] = []) {
        self.id = id
        self.uid = uid
        self.start = start
        self.end = end
        self.text = text
        self.words = words
    }

    // `uid` is an app-internal identity only; it is not part of the engine protocol.
    private enum CodingKeys: String, CodingKey {
        case id, start, end, text, words
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(Int.self, forKey: .id)
        uid = UUID()
        start = try container.decode(Double.self, forKey: .start)
        end = try container.decode(Double.self, forKey: .end)
        text = try container.decode(String.self, forKey: .text)
        words = try container.decodeIfPresent([Word].self, forKey: .words) ?? []
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(start, forKey: .start)
        try container.encode(end, forKey: .end)
        try container.encode(text, forKey: .text)
        try container.encode(words, forKey: .words)
    }

    /// Content equality; the transient view identity (`uid`) is not part of the comparison.
    public static func == (lhs: Sentence, rhs: Sentence) -> Bool {
        lhs.id == rhs.id && lhs.start == rhs.start && lhs.end == rhs.end
            && lhs.text == rhs.text && lhs.words == rhs.words
    }

    public func hash(into hasher: inout Hasher) {
        hasher.combine(id)
        hasher.combine(start)
        hasher.combine(end)
        hasher.combine(text)
        hasher.combine(words)
    }
}

public enum SubtitleFormat: String, CaseIterable, Sendable, Identifiable {
    case srt, vtt, json
    public var id: String { rawValue }
}

/// A catalog row from the engine's `models --json` output.
public struct EngineModel: Sendable, Equatable, Decodable, Identifiable {
    public var id: String
    /// "transcription" (speech recognition) or "translation".
    public var kind: String
    /// "whisper" (supports all languages, detects language) or "onnx" (fixed language list).
    public var backend: String
    /// Supported ISO 639-1 codes; `nil` means all languages.
    public var languages: [String]?
    public var sizeMB: Int
    /// Actual space taken in the cache (MB); `nil` if not downloaded.
    public var diskMB: Int?
    public var speed: Int
    public var accuracy: Int
    /// SPDX short name from the model card (e.g. "MIT", "CC-BY-NC-4.0").
    public var license: String?
    public var downloaded: Bool

    public init(
        id: String,
        kind: String = "transcription",
        backend: String = "whisper",
        languages: [String]? = nil,
        sizeMB: Int,
        diskMB: Int? = nil,
        speed: Int,
        accuracy: Int,
        license: String? = nil,
        downloaded: Bool
    ) {
        self.id = id
        self.kind = kind
        self.backend = backend
        self.languages = languages
        self.sizeMB = sizeMB
        self.diskMB = diskMB
        self.speed = speed
        self.accuracy = accuracy
        self.license = license
        self.downloaded = downloaded
    }

    public var isTranslation: Bool { kind == "translation" }

    /// Licenses that forbid commercial use ("NC" appears in the SPDX name).
    public var hasNonCommercialLicense: Bool {
        license?.uppercased().contains("NC") ?? false
    }

    /// Only the Whisper backend does automatic language detection.
    public var detectsLanguage: Bool { backend == "whisper" }

    /// If `languages` is empty, the model supports every language.
    public func supports(language code: String) -> Bool {
        languages?.contains(code) ?? true
    }

    private enum CodingKeys: String, CodingKey {
        case id, kind, backend, languages, speed, accuracy, license, downloaded
        case sizeMB = "size_mb"
        case diskMB = "disk_mb"
    }

    // Older engine output has no kind/license/backend/languages/disk_mb;
    // defaults for a subtitle model and Whisper keep it backward compatible.
    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? "transcription"
        backend = try container.decodeIfPresent(String.self, forKey: .backend) ?? "whisper"
        languages = try container.decodeIfPresent([String].self, forKey: .languages)
        sizeMB = try container.decode(Int.self, forKey: .sizeMB)
        diskMB = try container.decodeIfPresent(Int.self, forKey: .diskMB)
        speed = try container.decode(Int.self, forKey: .speed)
        accuracy = try container.decode(Int.self, forKey: .accuracy)
        license = try container.decodeIfPresent(String.self, forKey: .license)
        downloaded = try container.decode(Bool.self, forKey: .downloaded)
    }
}
