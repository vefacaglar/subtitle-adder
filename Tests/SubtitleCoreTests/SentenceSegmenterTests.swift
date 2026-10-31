import Testing
@testable import SubtitleCore

private func words(_ text: String, step: Double = 0.4) -> [Word] {
    text.split(separator: " ").enumerated().map { i, w in
        Word(start: Double(i) * step, end: Double(i) * step + step * 0.8, text: String(w))
    }
}

@Test func splitsOnSentenceEndPunctuation() {
    let result = SentenceSegmenter().segment(words("Merhaba dünya. Nasılsın? İyiyim!"))
    #expect(result.map(\.text) == ["Merhaba dünya.", "Nasılsın?", "İyiyim!"])
    #expect(result.map(\.id) == [1, 2, 3])
}

@Test func splitsOnLongPause() {
    var input = words("bir iki üç dört")
    for i in 2..<input.count { input[i].start += 3; input[i].end += 3 }
    #expect(SentenceSegmenter().segment(input).map(\.text) == ["bir iki", "üç dört"])
}

@Test func longSentencePrefersCommaBreak() {
    let text = "Bugün sabah erkenden kalktım, kahvaltı yaptım ve sonra uzun bir yürüyüşe çıktım."
    let result = SentenceSegmenter(maxChars: 50, maxDuration: 100).segment(words(text))
    #expect(result.first?.text == "Bugün sabah erkenden kalktım,")
    #expect(result.allSatisfy { $0.text.count <= 50 })
    #expect(result.map(\.text).joined(separator: " ") == text)
}

@Test func longSentenceSplitsOnDuration() {
    let result = SentenceSegmenter(maxChars: 1000, maxDuration: 2).segment(words("a b c d e f g h i j", step: 0.5))
    #expect(result.count > 1)
    #expect(result.allSatisfy { $0.end - $0.start <= 2 })
}

@Test func timesComeFromWords() {
    let input = words("Selam millet.")
    let sentence = SentenceSegmenter().segment(input)[0]
    #expect(sentence.start == input.first!.start)
    #expect(sentence.end == input.last!.end)
}

@Test func emptyInput() {
    #expect(SentenceSegmenter().segment([]).isEmpty)
}
