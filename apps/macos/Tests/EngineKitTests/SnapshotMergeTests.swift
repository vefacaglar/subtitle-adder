import Foundation
import Testing
@testable import EngineKit

/// Live `partial` snapshots replace the whole list; lines that are still the same keep their identity.
@Suite struct SnapshotMergeTests {
    private func line(_ id: Int, _ start: Double, _ end: Double, _ text: String) -> Sentence {
        Sentence(id: id, start: start, end: end, text: text)
    }

    @Test func firstSnapshotIsTakenAsIs() {
        let snapshot = [line(1, 0, 1, "a")]
        #expect([Sentence]().mergedWithSnapshot(snapshot).map(\.uid) == snapshot.map(\.uid))
    }

    @Test func unchangedLinesKeepTheirIdentity() {
        let existing = [line(1, 0, 1, "a"), line(2, 1, 2, "b")]
        let snapshot = [line(1, 0, 1, "a"), line(2, 1, 2, "b"), line(3, 2, 3, "c")]
        let merged = existing.mergedWithSnapshot(snapshot)
        #expect(merged.map(\.uid).prefix(2) == existing.map(\.uid)[...])
        #expect(merged[2].uid == snapshot[2].uid)
        #expect(merged.map(\.text) == ["a", "b", "c"])
    }

    @Test func refinedLastLineKeepsIdentityByNumber() {
        let existing = [line(1, 0, 1, "a"), line(2, 1, 2, "ben")]
        let snapshot = [line(1, 0, 1, "a"), line(2, 1, 2.6, "ben geliyorum")]
        let merged = existing.mergedWithSnapshot(snapshot)
        #expect(merged.map(\.uid) == existing.map(\.uid))
        #expect(merged[1].text == "ben geliyorum")
        #expect(merged[1].end == 2.6)
    }

    @Test func eachIdentityIsUsedOnce() {
        let existing = [line(1, 0, 1, "a")]
        let snapshot = [line(1, 0, 1, "a"), line(2, 0, 1, "a")]
        let merged = existing.mergedWithSnapshot(snapshot)
        #expect(merged[0].uid == existing[0].uid)
        #expect(merged[1].uid != existing[0].uid)
    }

    @Test func timeMatchWinsOverNumberWhenLinesShift() {
        let existing = [line(1, 0, 1, "a"), line(2, 1, 2, "b")]
        // A new first line pushes the others down by one number.
        let snapshot = [line(1, -1, 0, "z"), line(2, 0, 1, "a"), line(3, 1, 2, "b")]
        let merged = existing.mergedWithSnapshot(snapshot)
        #expect(merged[1].uid == existing[0].uid)
        #expect(merged[2].uid == existing[1].uid)
        #expect(merged[0].uid != existing[0].uid)
    }

    @Test func contentAndSpeakerComeFromTheSnapshot() {
        let existing = [Sentence(id: 1, start: 0, end: 1, text: "a", speaker: 1)]
        let merged = existing.mergedWithSnapshot([Sentence(id: 1, start: 0, end: 1, text: "a", speaker: 0)])
        #expect(merged[0].speaker == 0)
    }
}
