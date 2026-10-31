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

    /// Kelimenin zaman aralığının ortası; bölme noktasını seçmek için kullanılır.
    var midpoint: Double { (start + end) / 2 }
}

public struct Sentence: Sendable, Hashable, Codable {
    /// Görüntülenen sıra numarası (1'den başlar); birleştirme/silme sonrası yenilenir.
    public var id: Int
    /// Görünüm kimliği ve seçim anahtarı. `id` yenilenirken değişmez ve motora gönderilmez.
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

    // `uid` yalnız uygulama içi bir kimliktir; motor protokolünde yer almaz.
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

    /// İçerik eşitliği; geçici görünüm kimliği (`uid`) karşılaştırmaya girmez.
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

/// Motorun `models --json` çıktısındaki katalog satırı.
public struct EngineModel: Sendable, Equatable, Decodable, Identifiable {
    public var id: String
    /// "transcription" (konuşma tanıma) ya da "translation" (çeviri).
    public var kind: String
    /// "whisper" (tüm dilleri destekler, dil algılar) ya da "onnx" (dil listesi sabit).
    public var backend: String
    /// Desteklenen ISO 639-1 kodları; `nil` tüm diller anlamına gelir.
    public var languages: [String]?
    public var sizeMB: Int
    /// Önbellekte kapladığı gerçek alan (MB); indirilmemişse `nil`.
    public var diskMB: Int?
    public var speed: Int
    public var accuracy: Int
    /// Model kartındaki SPDX kısa adı (ör. "MIT", "CC-BY-NC-4.0").
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

    /// Ticari kullanıma kapalı lisanslar (SPDX adında "NC" geçer).
    public var hasNonCommercialLicense: Bool {
        license?.uppercased().contains("NC") ?? false
    }

    /// Otomatik dil algılamayı yalnız Whisper backend'i yapar.
    public var detectsLanguage: Bool { backend == "whisper" }

    /// `languages` boşsa model her dili destekliyordur.
    public func supports(language code: String) -> Bool {
        languages?.contains(code) ?? true
    }

    private enum CodingKeys: String, CodingKey {
        case id, kind, backend, languages, speed, accuracy, license, downloaded
        case sizeMB = "size_mb"
        case diskMB = "disk_mb"
    }

    // Eski motor çıktılarında kind/license/backend/languages/disk_mb yok;
    // altyazı modeli ve Whisper varsayılanları geriye uyum sağlar.
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
