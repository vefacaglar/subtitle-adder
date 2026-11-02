import Foundation
import Testing
@testable import LibraryKit

/// Several stores (instances) on one `profiles.json`, as `open -n` or a second process would be.
@Suite final class ProfileMultiInstanceTests {
    let folder = TestFolder()
    let secrets = InMemorySecretStore()

    @Test func twoStoresOnTheSameFileSeeEachOthersWritesWithoutAWatcher() throws {
        let video = folder.video("shared.mp4")
        let a = folder.makeStore()
        let b = folder.makeStore()

        try a.register(username: "alice", password: "pw", rememberMe: false)
        #expect(b.registeredUsernames == ["alice"], "profile list is visible to the other instance")
        try b.login(username: "alice", password: "pw", rememberMe: false)

        try a.recordWatch(video, position: 5, duration: 50)
        #expect(b.stats(for: video)?.viewCount == 1)
        try b.recordWatch(video, position: 9, duration: 50)
        try b.setRating(4, for: video)
        let list = try #require(try b.createList(named: "Ortak"))
        #expect(a.stats(for: video)?.viewCount == 2)
        #expect(a.stats(for: video)?.rating == 4)
        #expect(a.lists().map(\.name) == ["Ortak"])

        // No lost updates: each write starts from what the other one saved.
        try a.addVideo(video, toList: list.id)
        try b.recordWatch(video)
        #expect(a.stats(for: video)?.viewCount == 3)
        #expect(b.lists().first?.videoPaths == [normalized(video)])
    }

    @Test func aListNameTakenInOneInstanceIsADuplicateInTheOther() throws {
        let a = folder.makeStore()
        let b = folder.makeStore()
        try a.register(username: "alice", password: "pw", rememberMe: false)
        try b.login(username: "alice", password: "pw", rememberMe: false)
        try a.createList(named: "Daha Sonra İzle")
        #expect(throws: ProfileError.duplicateListName) { try b.createList(named: "daha sonra izle") }
        #expect(b.lists().count == 1)
    }

    @Test func concurrentWritersFromTwoStoresLoseNothing() throws {
        let video = folder.video("busy.mp4")
        let a = folder.makeStore(secrets: secrets, lockTimeout: 30)
        try a.register(username: "alice", password: "pw")
        let b = folder.makeStore(secrets: secrets, lockTimeout: 30)
        #expect(b.isLoggedIn)

        let failures = Failures()
        DispatchQueue.concurrentPerform(iterations: 8) { worker in
            let store = worker.isMultiple(of: 2) ? a : b
            for _ in 0..<5 {
                do { try store.recordWatch(video) } catch { failures.add("\(error)") }
            }
        }
        #expect(failures.all.isEmpty, "\(failures.all)")
        #expect(a.stats(for: video)?.viewCount == 40)
        #expect(b.stats(for: video)?.viewCount == 40)
    }

    // MARK: - Live change signal

    @Test func aChangeMadeByAnotherInstanceRaisesTheExternalSignalThroughTheWatcher() async throws {
        let video = folder.video("watched.mp4")
        let a = folder.makeStore(secrets: secrets, watches: true)
        try a.register(username: "alice", password: "pw")
        let b = folder.makeStore(secrets: secrets)
        #expect(b.isLoggedIn)

        let log = ChangeLog(a)
        defer { log.stop() }
        try b.recordWatch(video, position: 3, duration: 30)
        try b.setLiked(true, for: video)

        #expect(await eventually { log.events.contains(.external) })
        // The change was applied without anyone calling a.stats(...) first.
        #expect(await eventually { a.totals().liked == 1 })
        #expect(!log.events.contains(.local), "outside changes are not local ones")
    }

    @Test func anotherInstancesProgressOnlyWriteIsMergedSilently() async throws {
        let video = folder.video("watched.mp4")
        let a = folder.makeStore(secrets: secrets, watches: true)
        try a.register(username: "alice", password: "pw")
        try a.recordWatch(video, position: 1, duration: 100)
        let b = folder.makeStore(secrets: secrets)
        #expect(b.isLoggedIn)
        try await Task.sleep(for: .milliseconds(700))

        let log = ChangeLog(a)
        defer { log.stop() }
        try b.updatePlaybackProgress(video, position: 60, duration: 100)
        try b.setThumbnail(Data("T".utf8), for: video)
        try await Task.sleep(for: .milliseconds(900))

        #expect(log.events.isEmpty)
        #expect(a.stats(for: video)?.lastPositionSeconds == 60)
        #expect(a.thumbnail(for: video) == Data("T".utf8))
    }

    // MARK: - Shared remembered login

    @Test func aRememberedLoginMadeElsewhereLogsInAnInstanceThatHasNoUserLive() async throws {
        let a = folder.makeStore(secrets: secrets, watches: true)
        try a.register(username: "alice", password: "pw", rememberMe: false)
        try a.logout()
        let b = folder.makeStore(secrets: secrets, watches: true)
        #expect(!b.isLoggedIn)

        let log = ChangeLog(b)
        defer { log.stop() }
        try a.login(username: "alice", password: "pw", rememberMe: true)

        // Even before anyone asks b anything, its watcher picks the login up and tells the view model.
        #expect(await eventually { log.events.contains(.external) })
        #expect(b.currentUsername == "alice")
        #expect(b.isLoggedIn)
    }

    @Test func aRememberedLoginIsAlsoPickedUpByTheNextCallWithoutAWatcher() throws {
        let a = folder.makeStore(secrets: secrets)
        try a.register(username: "alice", password: "pw", rememberMe: false)
        try a.logout()
        let b = folder.makeStore(secrets: secrets)
        #expect(!b.isLoggedIn)

        try a.login(username: "alice", password: "pw", rememberMe: true)
        #expect(b.currentUsername == "alice")

        // But not if the key is not on this machine's keychain.
        let c = folder.makeStore(secrets: InMemorySecretStore())
        #expect(!c.isLoggedIn)
    }

    @Test func loggingOutInOneInstanceLogsOutTheOneThatFollowedTheRememberedLogin() async throws {
        let a = folder.makeStore(secrets: secrets, watches: true)
        try a.register(username: "alice", password: "pw")
        let b = folder.makeStore(secrets: secrets, watches: true)
        #expect(b.currentUsername == "alice")

        let log = ChangeLog(b)
        defer { log.stop() }
        try a.logout()

        #expect(await eventually { log.events.contains(.external) })
        #expect(!b.isLoggedIn)
        #expect(secrets.accounts.isEmpty)
        #expect(b.lastUsername == "alice")
    }

    @Test func loggingOutElsewhereIsAlsoSeenByTheNextCallWithoutAWatcher() throws {
        let a = folder.makeStore(secrets: secrets)
        try a.register(username: "alice", password: "pw")
        let b = folder.makeStore(secrets: secrets)
        #expect(b.isLoggedIn)
        try a.logout()
        #expect(b.currentUsername == nil)
        #expect(try b.recordWatch(folder.video("x.mp4")) == nil, "writes by a logged-out instance are no-ops")
    }

    @Test func anInstanceLoggedInWithoutRememberMeIsNotLoggedOutByAnUnattributableChange() throws {
        let a = folder.makeStore(secrets: secrets)
        try a.register(username: "alice", password: "pw")
        let b = folder.makeStore(secrets: secrets)
        #expect(b.isLoggedIn)

        // b logs in itself without remember-me: its session no longer stands for the shared login.
        try b.login(username: "alice", password: "pw", rememberMe: false)
        #expect(a.currentUsername == nil, "that did end the remembered login for a")

        // Someone else remembers alice again and then logs out; b never followed that login.
        try a.login(username: "alice", password: "pw", rememberMe: true)
        #expect(b.currentUsername == "alice")
        #expect(b.rememberedUsername == "alice")
        try a.logout()
        #expect(b.currentUsername == "alice")
        #expect(b.rememberedUsername == nil)
        try b.recordWatch(folder.video("still-works.mp4"))
        #expect(b.stats(for: folder.video("still-works.mp4"))?.viewCount == 1)
    }

    @Test func aStoreWithoutAnyUserIsNotLoggedInByItsOwnLogout() throws {
        let a = folder.makeStore(secrets: secrets)
        try a.register(username: "alice", password: "pw")
        try a.logout()
        // The key is gone and so is the remembered name: nothing to restore, now or after any reload.
        a.refresh()
        #expect(!a.isLoggedIn)
        #expect(!folder.makeStore(secrets: secrets).isLoggedIn)
    }

    @Test func deletingTheActiveProfileElsewhereLocksThisInstance() throws {
        let a = folder.makeStore()
        let b = folder.makeStore()
        try a.register(username: "alice", password: "pw", rememberMe: false)
        try b.login(username: "alice", password: "pw", rememberMe: false)
        try a.deleteProfile(username: "alice", password: "pw")
        #expect(!b.isLoggedIn)
        #expect(b.registeredUsernames.isEmpty)
    }

    @Test func eachChangeStreamGetsItsOwnEvents() async throws {
        let store = folder.makeStore()
        let first = ChangeLog(store)
        let second = ChangeLog(store)
        defer {
            first.stop()
            second.stop()
        }
        try store.register(username: "alice", password: "pw", rememberMe: false)
        try store.logout()
        #expect(await eventually { first.events == [.local, .local] && second.events == [.local, .local] })
        first.stop()
        try store.login(username: "alice", password: "pw", rememberMe: false)
        #expect(await eventually { second.events.count == 3 })
    }
}

private final class Failures: @unchecked Sendable {
    private let lock = NSLock()
    private var messages: [String] = []
    func add(_ message: String) { lock.withLock { messages.append(message) } }
    var all: [String] { lock.withLock { messages } }
}
