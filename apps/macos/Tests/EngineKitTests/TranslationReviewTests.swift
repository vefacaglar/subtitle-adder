import Foundation
import Testing
@testable import EngineKit

/// `needs_review` marks a line the engine could not translate; it round-trips only when true.
@Suite struct TranslationReviewTests {
    private func decode(_ json: String) throws -> Sentence {
        try JSONDecoder().decode(Sentence.self, from: Data(json.utf8))
    }

    @Test func translationEventCarriesTheFlagAndItSurvivesReEncoding() throws {
        let line = """
        {"event": "translation", "language": "en", "sentences": [\
        {"id": 1, "start": 0, "end": 1, "text": "Merhaba.", "speaker": null, "words": [], "needs_review": true},\
        {"id": 2, "start": 1, "end": 2, "text": "Hello.", "speaker": null, "words": []}]}
        """
        guard case .translation(_, let sentences) = try EngineEvent.decode(line) else {
            Issue.record("expected a translation event")
            return
        }
        #expect(sentences.map(\.needsReview) == [true, false])
        let data = try JSONEncoder().encode(sentences)
        let again = try JSONDecoder().decode([Sentence].self, from: data)
        #expect(again.map(\.needsReview) == [true, false])
    }

    @Test func aFalseFlagIsOmittedWhenEncoding() throws {
        let plain = try String(decoding: JSONEncoder().encode(Sentence(id: 1, start: 0, end: 1, text: "A")), as: UTF8.self)
        #expect(!plain.contains("needs_review"))
        let flagged = try String(
            decoding: JSONEncoder().encode(Sentence(id: 1, start: 0, end: 1, text: "A", needsReview: true)),
            as: UTF8.self
        )
        #expect(flagged.contains("\"needs_review\":true"))
    }

    @Test func onlyAJSONTrueSetsTheFlagAndNothingThrows() throws {
        let base = #""id": 1, "start": 0, "end": 1, "text": "A""#
        #expect(try decode("{\(base)}").needsReview == false)
        #expect(try decode("{\(base), \"needs_review\": null}").needsReview == false)
        #expect(try decode("{\(base), \"needs_review\": \"yes\"}").needsReview == false)
        #expect(try decode("{\(base), \"needs_review\": 1}").needsReview == false)
        #expect(try decode("{\(base), \"needs_review\": false}").needsReview == false)
        #expect(try decode("{\(base), \"needs_review\": true}").needsReview == true)
    }

    @Test func aMalformedFlagDoesNotDropAnImportedJSONFile() {
        let json = #"[{"id": 1, "start": 0, "end": 1, "text": "A", "needs_review": "x"}]"#
        #expect(SubtitleParser.parse(json, extension: "json").map(\.text) == ["A"])
    }

    @Test func equalityAndHashIncludeTheFlag() {
        let a = Sentence(id: 1, start: 0, end: 1, text: "A")
        var b = a
        b.needsReview = true
        #expect(a != b)
        #expect(Set([a, b]).count == 2)
    }

    @Test func snapshotMergeKeepsTheIncomingFlag() {
        let old = [Sentence(id: 1, start: 0, end: 1, text: "A")]
        let incoming = [Sentence(id: 1, start: 0, end: 1, text: "A", needsReview: true)]
        #expect(old.mergedWithSnapshot(incoming).first?.needsReview == true)
    }
}
