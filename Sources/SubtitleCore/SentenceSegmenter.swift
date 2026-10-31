import Foundation

/// Adım 3: kelime akışını cümlelere, uzun cümleleri ekrana sığan altyazı satırlarına böler.
public struct SentenceSegmenter: Sendable {
    public var pauseGap: Double = 1.2
    public var maxChars: Int = 84
    public var maxDuration: Double = 6.0

    static let sentenceEnd: Set<Character> = [".", "!", "?", "…"]
    static let softBreak: Set<Character> = [",", ";", ":"]

    public init(pauseGap: Double = 1.2, maxChars: Int = 84, maxDuration: Double = 6.0) {
        self.pauseGap = pauseGap
        self.maxChars = maxChars
        self.maxDuration = maxDuration
    }

    /// Cümleleri böler ve ekrana sığdırır.
    public func segment(_ words: [Word]) -> [Sentence] {
        let lines = sentences(words).flatMap(fit)
        return lines.enumerated().map { index, words in Self.join(words, id: index + 1) }
    }

    /// Noktalama ya da uzun sessizlikle biten kelimede cümleyi kapatır.
    func sentences(_ words: [Word]) -> [[Word]] {
        var result: [[Word]] = []
        var current: [Word] = []
        for (i, word) in words.enumerated() {
            current.append(word)
            let next = i + 1 < words.count ? words[i + 1] : nil
            let longPause = next.map { $0.start - word.end >= pauseGap } ?? false
            if Self.endsWith(word, Self.sentenceEnd) || longPause || next == nil {
                result.append(current)
                current = []
            }
        }
        return result
    }

    /// Sınırı aşan cümleyi tercihen virgülde, yoksa sınırda böler.
    func fit(_ words: [Word]) -> [[Word]] {
        guard words.count > 1, tooLong(words[...]) else { return [words] }
        var cut = 1
        while cut < words.count, !tooLong(words[..<(cut + 1)]) { cut += 1 }
        if let soft = (0..<(cut - 1)).last(where: { Self.endsWith(words[$0], Self.softBreak) }),
           soft + 1 >= cut / 2 {
            cut = soft + 1
        }
        return [Array(words[..<cut])] + fit(Array(words[cut...]))
    }

    private func tooLong(_ words: ArraySlice<Word>) -> Bool {
        let chars = words.reduce(0) { $0 + $1.text.count } + words.count - 1
        return chars > maxChars || words.last!.end - words.first!.start > maxDuration
    }

    private static func endsWith(_ word: Word, _ set: Set<Character>) -> Bool {
        word.text.last.map(set.contains) ?? false
    }

    static func join(_ words: [Word], id: Int) -> Sentence {
        Sentence(
            id: id,
            start: words.first!.start,
            end: words.last!.end,
            text: words.map(\.text).joined(separator: " "),
            words: words
        )
    }
}
