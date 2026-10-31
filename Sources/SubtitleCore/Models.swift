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
}

public struct Transcript: Sendable {
    public var language: String
    public var words: [Word]

    public init(language: String, words: [Word]) {
        self.language = language
        self.words = words
    }
}

public struct Sentence: Sendable, Hashable, Codable, Identifiable {
    public var id: Int
    public var start: Double
    public var end: Double
    public var text: String
    public var words: [Word]

    public init(id: Int, start: Double, end: Double, text: String, words: [Word]) {
        self.id = id
        self.start = start
        self.end = end
        self.text = text
        self.words = words
    }
}
