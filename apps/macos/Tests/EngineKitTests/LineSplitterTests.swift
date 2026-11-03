import Foundation
import Testing
@testable import EngineKit

/// The engine's stdout is cut into event lines; one line can be megabytes long.
@Suite struct LineSplitterTests {
    private func feed(_ splitter: LineSplitter, _ text: String) -> [String] {
        splitter.feed(Data(text.utf8))
    }

    @Test func cutsLinesAcrossChunkBoundaries() {
        let splitter = LineSplitter()
        #expect(feed(splitter, "one\ntw") == ["one"])
        #expect(feed(splitter, "o\r\nthree") == ["two"])
        #expect(feed(splitter, "\n\nfour\n") == ["three", "", "four"])
        #expect(splitter.rest().isEmpty)
    }

    @Test func aLineWithoutANewlineComesBackAsTheRest() {
        let splitter = LineSplitter()
        #expect(feed(splitter, "{\"event\": ") .isEmpty)
        #expect(feed(splitter, "\"x\"}").isEmpty)
        #expect(splitter.rest() == ["{\"event\": \"x\"}"])
        // Nothing is left behind, and a new line starts clean.
        #expect(splitter.rest().isEmpty)
        #expect(feed(splitter, "next\n") == ["next"])
    }

    @Test func aNewlineRightAtTheStartOfAChunkEndsTheLineOfTheChunkBefore() {
        let splitter = LineSplitter()
        #expect(feed(splitter, "abc").isEmpty)
        #expect(feed(splitter, "\ndef\n") == ["abc", "def"])
        #expect(feed(splitter, "x\r").isEmpty)
        #expect(feed(splitter, "\n") == ["x"])
    }

    @Test func multiByteCharactersSplitByTheChunkSurviveIntact() {
        let splitter = LineSplitter()
        let bytes = Array("Çeviri: ğüşiöç\n".utf8)
        var lines: [String] = []
        for byte in bytes { lines += splitter.feed(Data([byte])) }
        #expect(lines == ["Çeviri: ğüşiöç"])
    }

    /// A `partial` event of a long video is one line of megabytes in 64 KiB (or smaller) chunks. Searching the whole
    /// buffer for every chunk would take minutes here (the cost grows with the square of the line); with a scan offset
    /// the 16 MB below are cut in well under a second.
    @Test func aHugeSingleLineIsCutInLinearTime() {
        let splitter = LineSplitter()
        let chunk = Data(repeating: UInt8(ascii: "a"), count: 256)
        let chunks = 64 * 1024
        let started = ContinuousClock.now
        var lines: [String] = []
        for _ in 0..<chunks { lines += splitter.feed(chunk) }
        #expect(lines.isEmpty)
        lines += splitter.feed(Data("\nnext\n".utf8))
        let elapsed = ContinuousClock.now - started
        #expect(lines.count == 2)
        #expect(lines[0].utf8.count == 256 * chunks && lines[1] == "next")
        #expect(elapsed < .seconds(5), "took \(elapsed)")
    }
}
