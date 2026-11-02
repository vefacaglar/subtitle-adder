import Foundation
import Testing
@testable import LibraryKit

/// Playback progress is applied in memory at once but written at most every 15 seconds and on an explicit flush.
@Suite final class ProfileProgressTests {
    let folder = TestFolder()
    let clock = ManualClock()
    let secrets = InMemorySecretStore()
    var video: URL { folder.video("movie.mp4") }

    private func makeStore(lockTimeout: TimeInterval = 5) -> ProfileStore {
        folder.makeStore(secrets: secrets, lockTimeout: lockTimeout, clock: { [clock] in clock.date })
    }

    private func loggedInStore() throws -> ProfileStore {
        let store = makeStore()
        try store.register(username: "emre", password: "pw")
        return store
    }

    private func fileData() throws -> Data { try Data(contentsOf: folder.profilesFile) }

    @Test func progressIsWrittenAtMostEveryFifteenSeconds() throws {
        let store = try loggedInStore()
        try store.recordWatch(video, position: 1, duration: 600)
        var previous = try fileData()
        var writeSeconds: [Int] = []

        for second in 1...44 {
            clock.advance(1)
            try store.updatePlaybackProgress(video, position: 1 + Double(second), duration: 600)
            let current = try fileData()
            if current != previous {
                writeSeconds.append(second)
                previous = current
            }
            #expect(store.stats(for: video)?.lastPositionSeconds == 1 + Double(second), "readers see every tick at once")
        }
        #expect(writeSeconds == [15, 30], "one write per 15 seconds, not one per tick")
    }

    @Test func theFirstProgressOfAFreshSessionIsWrittenAtOnceThenThrottled() throws {
        _ = try loggedInStore()
        let fresh = makeStore()
        #expect(fresh.isLoggedIn)
        let before = try fileData()
        try fresh.updatePlaybackProgress(video, position: 20, duration: 600)
        let first = try fileData()
        #expect(first != before, "nothing was written by this store yet, so the throttle does not hold the first one back")
        clock.advance(1)
        try fresh.updatePlaybackProgress(video, position: 25, duration: 600)
        #expect(try fileData() == first)
    }

    @Test func flushWritesTheLatestPositionAtOnceAndASecondFlushIsANoOp() throws {
        let store = try loggedInStore()
        try store.recordWatch(video, position: 5, duration: 600)
        let saved = try fileData()
        #expect(!store.hasUnsavedChanges)

        clock.advance(1)
        try store.updatePlaybackProgress(video, position: 42, duration: 600)
        #expect(try fileData() == saved, "within the interval nothing is written")
        #expect(store.hasUnsavedChanges)

        #expect(try store.flushPlaybackProgress())
        #expect(!store.hasUnsavedChanges)
        let flushed = try fileData()
        #expect(flushed != saved)
        #expect(try store.flushPlaybackProgress() == false)
        #expect(try fileData() == flushed)

        let reopened = makeStore()
        #expect(reopened.isLoggedIn)
        #expect(reopened.stats(for: video)?.lastPositionSeconds == 42)
        #expect(reopened.stats(for: video)?.viewCount == 1)
    }

    @Test func progressAndFlushesNeverRaiseTheLocalSignal() async throws {
        let store = try loggedInStore()
        try store.recordWatch(video, position: 5, duration: 600)
        let log = ChangeLog(store)
        defer { log.stop() }

        for second in 1...20 {
            clock.advance(1)
            try store.updatePlaybackProgress(video, position: 5 + Double(second), duration: 600)
        }
        try store.flushPlaybackProgress()
        try store.persistPendingMigrations()
        try await Task.sleep(for: .milliseconds(150))
        #expect(log.events.isEmpty)
    }

    @Test func anyOtherWriteCarriesTheUnsavedProgressAlong() throws {
        let store = try loggedInStore()
        try store.recordWatch(video, position: 5, duration: 600)
        clock.advance(1)
        try store.updatePlaybackProgress(video, position: 300, duration: 600)
        #expect(store.hasUnsavedChanges)

        try store.setRating(4, for: folder.video("other.mp4"))
        #expect(!store.hasUnsavedChanges)
        let reopened = makeStore()
        #expect(reopened.stats(for: video)?.lastPositionSeconds == 300)
    }

    @Test func unsavedProgressSitsOnTopOfAnotherInstancesWrite() throws {
        let a = try loggedInStore()
        try a.recordWatch(video, position: 5, duration: 600)
        let b = makeStore()
        #expect(b.isLoggedIn)

        clock.advance(1)
        try a.updatePlaybackProgress(video, position: 200, duration: 600)
        #expect(a.hasUnsavedChanges)
        clock.advance(1)
        try b.setLiked(true, for: video)

        let seen = try #require(a.stats(for: video))
        #expect(seen.isLiked, "the other instance's change arrives")
        #expect(seen.lastPositionSeconds == 200, "and does not roll the unsaved resume point back")
        #expect(a.hasUnsavedChanges)

        try a.flushPlaybackProgress()
        let fromB = try #require(b.stats(for: video))
        #expect(fromB.lastPositionSeconds == 200)
        #expect(fromB.isLiked)
    }

    @Test func aFailedThrottledWriteKeepsTheProgressAndBacksOffForTheInterval() throws {
        let store = makeStore(lockTimeout: 0.1)
        try store.register(username: "emre", password: "pw")
        try store.recordWatch(video, position: 1, duration: 600)
        clock.advance(20)
        let before = try fileData()

        let held = try ProfileFileLock.acquire(at: store.lockURL, timeout: 1)
        #expect(throws: ProfileError.lockUnavailable) { try store.updatePlaybackProgress(video, position: 50, duration: 600) }
        #expect(store.stats(for: video)?.lastPositionSeconds == 50, "the progress is kept in memory")
        #expect(store.hasUnsavedChanges)

        // The next ticks neither throw nor wait for the lock again.
        for tick in 1...5 {
            clock.advance(1)
            try store.updatePlaybackProgress(video, position: 50 + Double(tick), duration: 600)
        }
        held.release()
        #expect(try fileData() == before)

        clock.advance(15)
        try store.updatePlaybackProgress(video, position: 90, duration: 600)
        #expect(try fileData() != before)
        #expect(makeStore().stats(for: video)?.lastPositionSeconds == 90)
    }

    @Test func loggingOutWritesTheProgressFirst() throws {
        let store = try loggedInStore()
        try store.recordWatch(video, position: 1, duration: 600)
        clock.advance(1)
        try store.updatePlaybackProgress(video, position: 333, duration: 600)
        try store.logout()
        #expect(!store.hasUnsavedChanges)
        try store.login(username: "emre", password: "pw")
        #expect(store.stats(for: video)?.lastPositionSeconds == 333)
    }

    @Test func switchingProfilesWritesTheProgressOfTheOneThatWasLoggedIn() throws {
        let store = try loggedInStore()
        try store.register(username: "other", password: "pw2", rememberMe: false)
        try store.login(username: "emre", password: "pw", rememberMe: false)
        try store.recordWatch(video, position: 1, duration: 600)
        clock.advance(1)
        try store.updatePlaybackProgress(video, position: 77, duration: 600)
        #expect(store.hasUnsavedChanges)

        try store.login(username: "other", password: "pw2", rememberMe: false)
        #expect(!store.hasUnsavedChanges)
        clock.advance(1)
        try store.login(username: "emre", password: "pw", rememberMe: false)
        #expect(store.stats(for: video)?.lastPositionSeconds == 77)

        clock.advance(1)
        try store.updatePlaybackProgress(video, position: 88, duration: 600)
        try store.register(username: "third", password: "pw3", rememberMe: false)
        try store.login(username: "emre", password: "pw", rememberMe: false)
        #expect(store.stats(for: video)?.lastPositionSeconds == 88)
    }

    @Test func progressThatWasNeverFlushedIsNotInTheFile() throws {
        let store = try loggedInStore()
        try store.recordWatch(video, position: 5, duration: 600)
        clock.advance(1)
        try store.updatePlaybackProgress(video, position: 99, duration: 600)
        // A process that ends here loses the last seconds; the file still has the last write.
        #expect(makeStore().stats(for: video)?.lastPositionSeconds == 5)
    }

    @Test func aStepUnderHalfASecondChangesNothingEvenAfterTheInterval() throws {
        let store = try loggedInStore()
        try store.recordWatch(video, position: 10, duration: 100)
        let saved = try fileData()
        clock.advance(60)
        try store.updatePlaybackProgress(video, position: 10.2, duration: 100)
        #expect(!store.hasUnsavedChanges)
        #expect(try fileData() == saved)
    }

    @Test func flushingWhileLoggedOutDoesNothing() throws {
        let store = makeStore()
        #expect(try store.flushPlaybackProgress() == false)
        #expect(try store.persistPendingMigrations() == false)
        #expect(!FileManager.default.fileExists(atPath: folder.profilesFile.path))
    }
}
