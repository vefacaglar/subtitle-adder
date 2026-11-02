import Foundation
import Testing
@testable import LibraryKit

/// Damaged files, unreadable vaults, failing disks and the details of what gets written and when.
@Suite final class ProfileRobustnessTests {
    let folder = TestFolder()

    private func asideFiles() -> [URL] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: folder.url.path)) ?? []
        return names.filter { $0.hasPrefix("profiles.corrupt-") && $0.hasSuffix(".json") }.sorted()
            .map { folder.url.appending(path: $0) }
    }

    // MARK: - Corrupt files

    @Test func aCorruptProfilesFileIsMovedAsideBeforeAnythingNewIsWritten() throws {
        let garbage = Data("{ this is not json".utf8)
        try garbage.write(to: folder.profilesFile)

        let store = folder.makeStore()
        #expect(store.registeredUsernames.isEmpty)
        // Merely looking never touches the file.
        #expect(throws: ProfileError.invalidCredentials) { try store.login(username: "nobody", password: "pw") }
        #expect(try Data(contentsOf: folder.profilesFile) == garbage)
        #expect(asideFiles().isEmpty)

        try store.register(username: "emre", password: "pw")
        let aside = asideFiles()
        #expect(aside.count == 1)
        #expect(try aside.first.map { try Data(contentsOf: $0) } == garbage)
        #expect(aside.first?.lastPathComponent.range(of: #"^profiles\.corrupt-\d{8}-\d{6}\.json$"#, options: .regularExpression) != nil)
        #expect(try folder.profilesObject()["version"] as? Int == 2)
        #expect(folder.makeStore().registeredUsernames == ["emre"])
    }

    @Test func anEmptyFileCountsAsCorruptToo() throws {
        try Data().write(to: folder.profilesFile)
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw")
        #expect(asideFiles().count == 1)
    }

    @Test func eachDamagedGenerationGetsItsOwnAsideFile() throws {
        let clock = ManualClock()
        for round in 1...2 {
            try Data("broken \(round)".utf8).write(to: folder.profilesFile)
            let store = folder.makeStore(clock: { clock.date })
            try store.register(username: "user\(round)", password: "pw")
        }
        let aside = asideFiles()
        #expect(aside.count == 2)
        #expect(try aside.map { String(decoding: try Data(contentsOf: $0), as: UTF8.self) }.sorted() == ["broken 1", "broken 2"])
    }

    @Test func aRunningStoreKeepsItsStateAndRestoresTheFileAfterSomeoneCorruptsIt() throws {
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        try store.recordWatch(folder.video("a.mp4"))

        try Data("garbage".utf8).write(to: folder.profilesFile)
        #expect(store.isLoggedIn, "a reload failure must not log the user out")
        try store.setLiked(true, for: folder.video("a.mp4"))
        #expect(asideFiles().count == 1)

        let fresh = folder.makeStore()
        try fresh.login(username: "emre", password: "pw")
        #expect(fresh.stats(for: folder.video("a.mp4"))?.isLiked == true)
    }

    @Test func aNewerFileVersionIsLeftAlone() throws {
        let future = Data(#"{"version": 99, "profiles": []}"#.utf8)
        try future.write(to: folder.profilesFile)
        let store = folder.makeStore()
        #expect(throws: ProfileError.unsupportedFileVersion(99)) { try store.register(username: "emre", password: "pw") }
        #expect(try Data(contentsOf: folder.profilesFile) == future)
        #expect(asideFiles().isEmpty)
        #expect(!store.isLoggedIn)
    }

    @Test func aMissingFileIsFine() throws {
        let store = folder.makeStore()
        #expect(store.registeredUsernames.isEmpty)
        #expect(!FileManager.default.fileExists(atPath: folder.profilesFile.path))
        #expect(store.totals() == .zero)
        #expect(store.feed(.all).isEmpty)
        try store.logout()
    }

    // MARK: - Corrupt vault

    @Test func anUnreadableVaultMakesLoginFailWithVaultUnreadableAndLeavesTheCiphertextAlone() throws {
        let store = folder.makeStore()
        try store.register(username: "alice", password: "pw-alice", rememberMe: false)
        try store.recordWatch(folder.video("a.mp4"))
        try store.logout()

        // Damage alice's vault but keep the file valid.
        var root = try folder.profilesObject()
        var profiles = try #require(root["profiles"] as? [[String: Any]])
        let damaged = ProfileCrypto.randomBytes(80).base64EncodedString()
        profiles[0]["encryptedVaultBase64"] = damaged
        root["profiles"] = profiles
        try JSONSerialization.data(withJSONObject: root).write(to: folder.profilesFile)
        let before = try Data(contentsOf: folder.profilesFile)

        let reopened = folder.makeStore()
        #expect(throws: ProfileError.vaultUnreadable) { try reopened.login(username: "alice", password: "pw-alice") }
        #expect(!reopened.isLoggedIn)
        #expect(throws: ProfileError.invalidCredentials) { try reopened.login(username: "alice", password: "wrong") }
        #expect(try Data(contentsOf: folder.profilesFile) == before)

        // Other profiles keep working, and alice's ciphertext survives their writes untouched.
        try reopened.register(username: "bob", password: "pw-bob")
        let after = try #require((try folder.profilesObject()["profiles"] as? [[String: Any]])?.first { $0["username"] as? String == "alice" })
        #expect(after["encryptedVaultBase64"] as? String == damaged)
        #expect(reopened.registeredUsernames == ["alice", "bob"])
    }

    @Test func aRememberedLoginWithAnUnreadableVaultDoesNotLogIn() throws {
        let secrets = InMemorySecretStore()
        let store = folder.makeStore(secrets: secrets)
        try store.register(username: "alice", password: "pw")

        var root = try folder.profilesObject()
        var profiles = try #require(root["profiles"] as? [[String: Any]])
        profiles[0]["encryptedVaultBase64"] = ProfileCrypto.randomBytes(64).base64EncodedString()
        root["profiles"] = profiles
        try JSONSerialization.data(withJSONObject: root).write(to: folder.profilesFile)

        #expect(!folder.makeStore(secrets: secrets).isLoggedIn)
        #expect(!store.isLoggedIn, "a running session whose vault was replaced by garbage is locked, not kept")
    }

    // MARK: - Credentials

    @Test func anUnknownUserStillPaysForOnePbkdf2() throws {
        let store = folder.makeStore()
        try store.register(username: "alice", password: "pw")
        try store.logout()

        let clock = ContinuousClock()
        let unknown = clock.measure {
            #expect(throws: ProfileError.invalidCredentials) { try store.login(username: "nobody", password: "pw") }
        }
        let blank = clock.measure {
            #expect(throws: ProfileError.missingCredentials) { try store.login(username: "", password: "pw") }
        }
        #expect(unknown > .milliseconds(5), "an unknown user must cost a key derivation, like a wrong password")
        #expect(blank < unknown)
    }

    // MARK: - Failing disk and locks

    @Test func writeFailuresAreReportedAndLeaveTheStateAsItWas() throws {
        let video = folder.video("a.mp4")
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        try store.recordWatch(video)

        chmod(folder.url.path, 0o555)
        defer { chmod(folder.url.path, 0o755) }
        #expect(throws: ProfileError.writeFailed(errno: EACCES)) { try store.setLiked(true, for: video) }
        #expect(store.stats(for: video)?.isLiked == false, "a failed save must not be applied in memory")
        #expect(throws: ProfileError.self) { try store.recordWatch(video) }
        chmod(folder.url.path, 0o755)
        try store.setLiked(true, for: video)
        #expect(store.stats(for: video)?.isLiked == true)
        #expect(store.stats(for: video)?.viewCount == 1)
    }

    @Test func aHeldFileLockFailsFastInsteadOfProceedingUnlocked() throws {
        let video = folder.video("a.mp4")
        let store = folder.makeStore(lockTimeout: 0.2)
        try store.register(username: "emre", password: "pw", rememberMe: false)

        let held = try ProfileFileLock.acquire(at: store.lockURL, timeout: 1)
        #expect(throws: ProfileError.lockUnavailable) { try store.recordWatch(video) }
        #expect(throws: ProfileError.lockUnavailable) { try store.createList(named: "x") }
        // Reads need no lock, and a logout still locks the profile locally even though it cannot be saved.
        #expect(store.isLoggedIn)
        #expect(throws: ProfileError.lockUnavailable) { try store.logout() }
        #expect(!store.isLoggedIn)
        held.release()

        try store.login(username: "emre", password: "pw", rememberMe: false)
        try store.recordWatch(video)
        #expect(store.stats(for: video)?.viewCount == 1)
    }

    @Test func aLoggedOutStoreDoesNotTakeTheLockOrWriteAnything() throws {
        let store = folder.makeStore(lockTimeout: 0.1)
        let held = try ProfileFileLock.acquire(at: store.lockURL, timeout: 1)
        defer { held.release() }
        #expect(try store.recordWatch(folder.video("a.mp4")) == nil)
        #expect(try store.updatePlaybackProgress(folder.video("a.mp4"), position: 5, duration: 10) == nil)
        #expect(try store.setThumbnail(Data("x".utf8), for: folder.video("a.mp4")) == false)
        #expect(!FileManager.default.fileExists(atPath: folder.profilesFile.path))
    }

    // MARK: - Playback progress, thumbnails, ratings

    @Test func progressIsSavedOnlyWhenItMovedHalfASecond() throws {
        let video = folder.video("a.mp4")
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        try store.recordWatch(video, position: 10, duration: 100)
        let saved = try Data(contentsOf: folder.profilesFile)

        let small = try #require(try store.updatePlaybackProgress(video, position: 10.3, duration: 100))
        #expect(small.lastPositionSeconds == 10)
        #expect(try Data(contentsOf: folder.profilesFile) == saved)

        let real = try #require(try store.updatePlaybackProgress(video, position: 10.5, duration: 100))
        #expect(real.lastPositionSeconds == 10.5)
        #expect(real.viewCount == 1, "progress never counts a view")
        #expect(try Data(contentsOf: folder.profilesFile) != saved)

        // A new duration counts as movement too; a position past the duration is cut to it.
        let cut = try #require(try store.updatePlaybackProgress(video, position: 500, duration: 100))
        #expect(cut.lastPositionSeconds == 100)
    }

    @Test func aPositionOfZeroInRecordWatchKeepsTheOldOne() throws {
        let video = folder.video("a.mp4")
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        try store.recordWatch(video, position: 42, duration: 120)
        let again = try #require(try store.recordWatch(video))
        #expect(again.lastPositionSeconds == 42)
        #expect(again.durationSeconds == 120)
        #expect(again.viewCount == 2)
        let nan = try #require(try store.recordWatch(video, position: .nan, duration: .infinity))
        #expect(nan.lastPositionSeconds == 42)
        #expect(nan.durationSeconds == 120)
        let over = try #require(try store.recordWatch(video, position: 999, duration: 0))
        #expect(over.lastPositionSeconds == 120)
    }

    @Test func progressOnAnUntouchedVideoCreatesItOnlyWhenItMatters() throws {
        let video = folder.video("a.mp4")
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        let tiny = try #require(try store.updatePlaybackProgress(video, position: 0.2, duration: 0))
        #expect(tiny.viewCount == 0)
        #expect(store.stats(for: video) == nil)
        try store.updatePlaybackProgress(video, position: 20, duration: 100)
        let stored = try #require(store.stats(for: video))
        #expect(stored.lastPositionSeconds == 20)
        #expect(!stored.hasWatched)
        #expect(stored.progressRatio == 0.2)
        #expect(store.feed(.history).isEmpty)
    }

    @Test func thumbnailsOver512KBAreIgnored() throws {
        let video = folder.video("a.mp4")
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        try store.recordWatch(video)
        let saved = try Data(contentsOf: folder.profilesFile)

        #expect(try store.setThumbnail(Data(repeating: 1, count: 512 * 1024 + 1), for: video) == false)
        #expect(try store.setThumbnail(Data(), for: video) == false)
        #expect(store.thumbnail(for: video) == nil)
        #expect(try Data(contentsOf: folder.profilesFile) == saved)

        let exactly = Data(repeating: 2, count: 512 * 1024)
        #expect(try store.setThumbnail(exactly, for: video))
        #expect(store.thumbnail(for: video) == exactly)
        let reopened = folder.makeStore()
        try reopened.login(username: "emre", password: "pw")
        #expect(reopened.thumbnail(for: video) == exactly)
    }

    @Test func ratingsAreClampedAndZeroClears() throws {
        let video = folder.video("a.mp4")
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        #expect(try store.setRating(9, for: video)?.rating == 5)
        #expect(try store.setRating(-3, for: video)?.rating == 0)
        #expect(try store.setRating(3, for: video)?.rating == 3)
        #expect(store.totals().rated == 1)
        #expect(try store.setRating(0, for: video)?.rating == 0)
        #expect(store.totals().rated == 0)
        #expect(try store.toggleLike(video))
        #expect(try store.toggleLike(video) == false)
    }
}
