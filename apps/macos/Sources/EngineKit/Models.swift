import Foundation

public struct Word: Sendable, Hashable, Codable {
    public var start: Double
    public var end: Double
    public var text: String
}

public struct Sentence: Sendable, Hashable, Codable, Identifiable {
    public var id: Int
    public var start: Double
    public var end: Double
    public var text: String
    public var words: [Word]

    public init(id: Int, start: Double, end: Double, text: String, words: [Word] = []) {
        self.id = id
        self.start = start
        self.end = end
        self.text = text
        self.words = words
    }
}

public enum SubtitleFormat: String, CaseIterable, Sendable, Identifiable {
    case srt, vtt, json
    public var id: String { rawValue }
}
