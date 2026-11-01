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

    @Test func refinedLineWithNewTextGetsANewIdentity() {
        // The line number is no longer a match: a line whose text changed is a new line (its translation would be
        // outdated anyway).
        let existing = [line(1, 0, 1, "a"), line(2, 1, 2, "ben")]
        let snapshot = [line(1, 0, 1, "a"), line(2, 1, 2.6, "ben geliyorum")]
        let merged = existing.mergedWithSnapshot(snapshot)
        #expect(merged[0].uid == existing[0].uid)
        #expect(merged[1].uid != existing[1].uid)
        #expect(merged[1].text == "ben geliyorum")
        #expect(merged[1].end == 2.6)
    }

    @Test func sameNumberAloneNeverMatches() {
        let existing = [line(1, 0, 1, "a")]
        let merged = existing.mergedWithSnapshot([line(1, 5, 6, "b")])
        #expect(merged[0].uid != existing[0].uid)
    }

    @Test func duplicateIdenticalLinesEachKeepTheirOwnIdentity() {
        let existing = [line(1, 0, 1, "Evet."), line(2, 0, 1, "Evet."), line(3, 0, 1, "Evet.")]
        let snapshot = [line(1, 0, 1, "Evet."), line(2, 0, 1, "Evet."), line(3, 0, 1, "Evet.")]
        let merged = existing.mergedWithSnapshot(snapshot)
        #expect(merged.map(\.uid) == existing.map(\.uid))
        // A fourth duplicate is new, and the three keep their own.
        let more = existing.mergedWithSnapshot(snapshot + [line(4, 0, 1, "Evet.")])
        #expect(Array(more.prefix(3).map(\.uid)) == existing.map(\.uid))
        #expect(Set(more.map(\.uid)).count == 4)
    }

    @Test func nearMatchKeepsIdentityForTheSameTextWithinOneAndAHalfSeconds() {
        let existing = [line(1, 10, 12, "Merhaba.")]
        let near = existing.mergedWithSnapshot([line(1, 11.5, 13, "Merhaba.")])
        #expect(near[0].uid == existing[0].uid)
        #expect(near[0].start == 11.5)
        let far = existing.mergedWithSnapshot([line(1, 11.6, 13, "Merhaba.")])
        #expect(far[0].uid != existing[0].uid)
        let otherText = existing.mergedWithSnapshot([line(1, 10, 12, "Merhaba!")])
        #expect(otherText[0].uid != existing[0].uid)
    }

    @Test func exactMatchesAreReservedBeforeNearMatches() {
        // The first incoming line is a near match of the existing line, but the second is its exact match.
        let existing = [line(1, 1, 2, "x")]
        let merged = existing.mergedWithSnapshot([line(1, 0.5, 1.5, "x"), line(2, 1, 2, "x")])
        #expect(merged[1].uid == existing[0].uid)
        #expect(merged[0].uid != existing[0].uid)
    }

    @Test func idsShiftingAfterASpeakerSplitDoNotStealIdentities() {
        let existing = [line(1, 0, 4, "Merhaba nasılsın."), line(2, 4, 6, "İyiyim.")]
        // The first sentence is split in two at a speaker change, so every number after it moves by one.
        let snapshot = [line(1, 0, 2, "Merhaba"), line(2, 2, 4, "nasılsın."), line(3, 4, 6, "İyiyim.")]
        let merged = existing.mergedWithSnapshot(snapshot)
        #expect(merged[2].uid == existing[1].uid)
        #expect(merged[0].uid != existing[1].uid && merged[1].uid != existing[1].uid)
        #expect(merged[0].uid != existing[0].uid && merged[1].uid != existing[0].uid)
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
