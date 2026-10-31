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
