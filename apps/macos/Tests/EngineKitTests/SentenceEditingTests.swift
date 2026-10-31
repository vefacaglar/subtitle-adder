import Foundation
import Testing
@testable import EngineKit

@Suite struct SentenceEditingTests {
    private func sample() -> [Sentence] {
        [
            Sentence(id: 1, start: 0.5, end: 2.0, text: "Merhaba.", words: [Word(start: 0.5, end: 2.0, text: "Merhaba.")]),
            Sentence(id: 2, start: 2.5, end: 4.0, text: "Nasılsın?"),
            Sentence(id: 3, start: 4.0, end: 6.0, text: "İyiyim."),
        ]
    }

    @Test func activeIndexFindsContainingSentence() {
        let s = sample()
        #expect(s.activeIndex(at: 0.2) == nil)
        #expect(s.activeIndex(at: 0.5) == 0)
        #expect(s.activeIndex(at: 1.99) == 0)
        #expect(s.activeIndex(at: 2.2) == nil)
        #expect(s.activeIndex(at: 4.0) == 2)
        #expect(s.activeIndex(at: 6.0) == nil)
    }

    @Test func nudgeMovesByStepWithoutFloatNoise() {
        var s = sample()
        let r1 = s.nudgeStart(at: 0, by: 0.1)
        #expect(r1)
        #expect(s[0].start == 0.6)
        let r2 = s.nudgeEnd(at: 0, by: -0.1)
        #expect(r2)
        #expect(s[0].end == 1.9)
    }

    @Test func startClampsToZeroAndPreviousEnd() {
        var s = sample()
        s.setStart(-3, at: 0)
        #expect(s[0].start == 0)
        s.setStart(0.0, at: 1)
        #expect(s[1].start == 2.0)
        let r3 = s.setStart(0.0, at: 1)
        #expect(!r3)
    }

    @Test func startKeepsMinimumDuration() {
        var s = sample()
        s.setStart(10, at: 0)
        #expect(abs(s[0].start - (2.0 - SentenceEditing.minimumDuration)) < 1e-9)
        #expect(s[0].start < s[0].end)
    }

    @Test func endClampsToNextStartAndMinimumDuration() {
        var s = sample()
        s.setEnd(9, at: 0)
        #expect(s[0].end == 2.5)
        s.setEnd(0, at: 1)
        #expect(abs(s[1].end - (2.5 + SentenceEditing.minimumDuration)) < 1e-9)
        s.setEnd(100, at: 2)
        #expect(s[2].end == 100)
    }

    @Test func invalidIndexIsIgnored() {
        var s = sample()
        let r4 = s.nudgeStart(at: 5, by: 0.1)
        #expect(!r4)
        let r5 = s.mergeWithNext(at: 2)
        #expect(!r5)
        let r6 = s.deleteSentence(at: -1)
        #expect(!r6)
        #expect(s == sample())
    }

    @Test func mergeJoinsTextTimesAndWordsThenRenumbers() {
        var s = sample()
        let firstUID = s[0].uid
        let r7 = s.mergeWithNext(at: 0)
        #expect(r7)
        #expect(s.count == 2)
        #expect(s[0].text == "Merhaba. Nasılsın?")
        #expect(s[0].start == 0.5)
        #expect(s[0].end == 4.0)
        #expect(s[0].words.count == 1)
        #expect(s[0].uid == firstUID)
        #expect(s.map(\.id) == [1, 2])
        #expect(s[1].text == "İyiyim.")
    }

    @Test func deleteRemovesAndRenumbers() {
        var s = sample()
        let r8 = s.deleteSentence(at: 0)
        #expect(r8)
        #expect(s.map(\.text) == ["Nasılsın?", "İyiyim."])
        #expect(s.map(\.id) == [1, 2])
    }

    @Test func setTextReportsChange() {
        var s = sample()
        let r9 = s.setText("Merhaba.", at: 0)
        #expect(!r9)
        let r10 = s.setText("Selam.", at: 0)
        #expect(r10)
        #expect(s[0].text == "Selam.")
    }

    @Test func nudgeNeverMovesAgainstRequestedDirection() {
        var s = [
            Sentence(id: 1, start: 0.0, end: 5.20, text: "Önceki"),
            Sentence(id: 2, start: 5.05, end: 6.00, text: "Sonraki"),
        ]
        // The start is below the previous line's end; asking to go back must not push it forward.
        let backward = s.nudgeStart(at: 1, by: -SentenceEditing.nudgeStep)
        #expect(!backward)
        #expect(s[1].start == 5.05)
        // Movement toward validity is accepted because it is in the requested direction.
        let forward = s.nudgeStart(at: 1, by: SentenceEditing.nudgeStep)
        #expect(forward)
        #expect(s[1].start == 5.20)
    }

    @Test func setStartNeverJumpsAgainstRequestedDirection() {
        var s = [
            Sentence(id: 1, start: 0.0, end: 5.20, text: "Önceki"),
            Sentence(id: 2, start: 5.05, end: 6.00, text: "Sonraki"),
        ]
        let backward = s.setStart(5.0, at: 1)
        #expect(!backward)
        #expect(s[1].start == 5.05)
        let forward = s.setStart(5.15, at: 1)
        #expect(forward)
        #expect(s[1].start == 5.20)
    }

    @Test func nudgeEndDoesNotMoveBackwardsWhenSentenceIsTooShort() {
        var s = [Sentence(id: 1, start: 2.0, end: 2.05, text: "Kısa")]
        let backward = s.nudgeEnd(at: 0, by: -SentenceEditing.nudgeStep)
        #expect(!backward)
        #expect(s[0].end == 2.05)
        let forward = s.nudgeEnd(at: 0, by: SentenceEditing.nudgeStep)
        #expect(forward)
        #expect(s[0].end == 2.15)
    }

    @Test func splitUsesWordBoundaryAtPlayhead() {
        var s = [
            Sentence(
                id: 5, start: 0.0, end: 4.0, text: "Merhaba dünya",
                words: [
                    Word(start: 0.0, end: 1.0, text: "Merhaba"),
                    Word(start: 2.0, end: 4.0, text: "dünya"),
                ]
            ),
        ]
        let firstUID = s[0].uid
        let split = s.splitSentence(at: 0, at: 1.5)
        #expect(split)
        #expect(s.count == 2)
        #expect(s[0].text == "Merhaba")
        #expect(s[1].text == "dünya")
        #expect(s[0].start == 0.0)
        #expect(s[0].end == 1.5)
        #expect(s[1].start == 1.5)
        #expect(s[1].end == 4.0)
        #expect(s[0].uid == firstUID)
        #expect(s[1].uid != firstUID)
        #expect(s.map(\.id) == [1, 2])
        #expect(s[0].words == [Word(start: 0.0, end: 1.0, text: "Merhaba")])
        #expect(s[1].words == [Word(start: 2.0, end: 4.0, text: "dünya")])
    }

    @Test func splitInsideFirstWordStillUsesWordBoundary() {
        var s = [
            Sentence(
                id: 1, start: 0.0, end: 4.0, text: "Merhaba dünya",
                words: [
                    Word(start: 0.0, end: 1.0, text: "Merhaba"),
                    Word(start: 2.0, end: 4.0, text: "dünya"),
                ]
            ),
        ]
        let split = s.splitSentence(at: 0, at: 0.2)
        #expect(split)
        #expect(s[0].text == "Merhaba")
        #expect(s[0].end == 1.5)
        #expect(s[1].start == 1.5)
    }

    @Test func splitWithoutWordsUsesMiddleWhitespaceAndProportionalTime() {
        var s = [Sentence(id: 1, start: 0.0, end: 4.0, text: "Bir iki üç dört")]
        let split = s.splitSentence(at: 0, at: 3.5)
        #expect(split)
        #expect(s.count == 2)
        #expect(s[0].text == "Bir iki")
        #expect(s[1].text == "üç dört")
        #expect(s[0].end == 2.0)
        #expect(s[1].start == 2.0)
        #expect(s[0].words.isEmpty)
        #expect(s[1].words.isEmpty)
    }

    @Test func splitRejectsInvalidRequestsWithoutChangingList() {
        var s = sample()
        let outside = s.splitSentence(at: 0, at: 10.0)
        #expect(!outside)
        let badIndex = s.splitSentence(at: 9, at: 1.0)
        #expect(!badIndex)

        var singleWord = [Sentence(id: 1, start: 0.0, end: 2.0, text: "Tek")]
        let noSpace = singleWord.splitSentence(at: 0, at: 1.0)
        #expect(!noSpace)

        var tooShort = [Sentence(id: 1, start: 0.0, end: 0.15, text: "Bir iki")]
        let short = tooShort.splitSentence(at: 0, at: 0.075)
        #expect(!short)
        #expect(s == sample())
        #expect(singleWord.count == 1)
        #expect(tooShort.count == 1)
    }

    @Test func canSplitMatchesSplitPrerequisites() {
        let splittable = [Sentence(id: 1, start: 0.0, end: 4.0, text: "Bir iki")]
        #expect(splittable.canSplit(at: 0, at: 2.0))
        #expect(!splittable.canSplit(at: 0, at: 5.0))
        let single = [Sentence(id: 1, start: 0.0, end: 4.0, text: "Tek")]
        #expect(!single.canSplit(at: 0, at: 2.0))
    }

    @Test func uidIsStableForSelectionAndStaysOutOfJSON() throws {
        let sentence = Sentence(
            id: 1, start: 0.0, end: 1.0, text: "Merhaba.",
            words: [Word(start: 0.0, end: 1.0, text: "Merhaba.")]
        )
        let data = try JSONEncoder().encode(sentence)
        let json = try #require(String(data: data, encoding: .utf8))
        #expect(!json.contains("uid"))
        let decoded = try JSONDecoder().decode(Sentence.self, from: data)
        #expect(decoded.uid != sentence.uid)
        #expect(decoded == sentence)
        #expect(decoded.text == "Merhaba.")
    }
}
