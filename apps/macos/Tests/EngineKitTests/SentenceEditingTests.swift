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
        let r7 = s.mergeWithNext(at: 0)
        #expect(r7)
        #expect(s.count == 2)
        #expect(s[0].text == "Merhaba. Nasılsın?")
        #expect(s[0].start == 0.5)
        #expect(s[0].end == 4.0)
        #expect(s[0].words.count == 1)
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
}
