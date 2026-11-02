import Foundation
import Testing
@testable import LibraryKit

/// A secret store that, on every call, checks that the store under test holds neither its in-process lock nor the
/// cross-process file lock: a keychain access prompt would otherwise freeze every reader (and every other instance).
private final class ProbingSecrets: SecretStore, @unchecked Sendable {
    private let inner: InMemorySecretStore
    private let lock = NSLock()
    private var target: ProfileStore?
    private var seen: [String] = []
    private var count = 0

    init(inner: InMemorySecretStore) { self.inner = inner }

    func watch(_ store: ProfileStore) { lock.withLock { target = store } }

    var violations: [String] { lock.withLock { seen } }
    var calls: Int { lock.withLock { count } }

    private func check(_ what: String) {
        guard let store = lock.withLock({ target }) else { return }
        lock.withLock { count += 1 }
        var problems: [String] = []
        if store.lock.try() {
            store.lock.unlock()
        } else {
            problems.append("\(what) ran with the in-process lock held")
        }
        if let probe = try? ProfileFileLock.acquire(at: store.lockURL, timeout: 0) {
            probe.release()
        } else {
            problems.append("\(what) ran with the file lock held")
        }
        lock.withLock { seen.append(contentsOf: problems) }
    }

    func data(for account: String) -> Data? {
        check("read of \(account)")
        return inner.data(for: account)
    }

    @discardableResult func set(_ data: Data, for account: String) -> Bool {
        check("write of \(account)")
        return inner.set(data, for: account)
    }

    func remove(_ account: String) {
        check("removal of \(account)")
        inner.remove(account)
    }
}

@Suite final class ProfileLockingTests {
    let folder = TestFolder()

    // MARK: - Readers while a writer waits for the file lock (M6a)

    @Test func gettersAreNotHeldUpWhileAWriterPollsForTheFileLock() async throws {
        let video = folder.video("a.mp4")
        let store = folder.makeStore(lockTimeout: 3)
        try store.register(username: "emre", password: "pw", rememberMe: false)
        try store.createList(named: "Liste")

        // Another process holds the file lock; a writer on this store polls for it.
        let held = try ProfileFileLock.acquire(at: store.lockURL, timeout: 1)
        let writer = Task.detached { try? store.recordWatch(video) }
        try await Task.sleep(for: .milliseconds(200))

        let started = ContinuousClock.now
        #expect(store.isLoggedIn)
        #expect(store.currentUsername == "emre")
        #expect(store.registeredUsernames == ["emre"])
        #expect(store.totals() == .zero)
        #expect(store.lists().map(\.name) == ["Liste"])
        #expect(store.stats(for: video) == nil)
        #expect(!store.hasUnsavedChanges)
        let elapsed = ContinuousClock.now - started
        #expect(elapsed < .milliseconds(500), "readers must not wait for the writer's file lock (took \(elapsed))")

        held.release()
        _ = await writer.value
        #expect(store.stats(for: video)?.viewCount == 1, "the writer got the lock once it was free")
    }

    @Test func aWriterWaitingForTheFileLockDoesNotBlockAnotherStoreOfTheSameFile() async throws {
        let video = folder.video("a.mp4")
        let secrets = InMemorySecretStore()
        let a = folder.makeStore(secrets: secrets, lockTimeout: 3)
        try a.register(username: "emre", password: "pw")
        let b = folder.makeStore(secrets: secrets, lockTimeout: 3)
        #expect(b.isLoggedIn)

        let held = try ProfileFileLock.acquire(at: a.lockURL, timeout: 1)
        let writer = Task.detached { try? a.recordWatch(video) }
        try await Task.sleep(for: .milliseconds(150))
        let started = ContinuousClock.now
        #expect(b.currentUsername == "emre")
        #expect(b.totals() == .zero)
        #expect(ContinuousClock.now - started < .milliseconds(500))
        held.release()
        _ = await writer.value
        #expect(b.stats(for: video)?.viewCount == 1)
    }

    // MARK: - Keychain outside the locks (M6c)

    @Test func theKeychainIsNeverAskedWhileALockIsHeld() throws {
        let shared = InMemorySecretStore()
        let first = ProbingSecrets(inner: shared)
        let a = folder.makeStore(secrets: first)
        first.watch(a)

        try a.register(username: "alice", password: "pw1")
        try a.register(username: "bob", password: "pw2")
        try a.login(username: "alice", password: "pw1")
        try a.login(username: "bob", password: "pw2", rememberMe: false)
        try a.login(username: "alice", password: "pw1")
        try a.logout()
        #expect(throws: ProfileError.invalidCredentials) { try a.login(username: "alice", password: "wrong") }
        try a.login(username: "alice", password: "pw1")

        // Another instance restores the remembered login: the key is read on its own thread, outside its locks.
        let second = ProbingSecrets(inner: shared)
        let b = folder.makeStore(secrets: second)
        second.watch(b)
        #expect(b.currentUsername == "alice")
        b.refresh()

        try a.logout()
        b.refresh()
        try a.login(username: "bob", password: "pw2")
        try b.deleteProfile(username: "bob", password: "pw2")

        #expect(first.calls > 8)
        #expect(second.calls > 0)
        #expect(first.violations.isEmpty, "\(first.violations)")
        #expect(second.violations.isEmpty, "\(second.violations)")
    }

    @Test func aLoginThatCannotBeWrittenPutsTheKeychainBackAsItWas() throws {
        let shared = InMemorySecretStore()
        let store = folder.makeStore(secrets: shared, lockTimeout: 0.1)
        try store.register(username: "alice", password: "pw")

        // The key is written before the transaction; when the transaction fails it is restored to what it was.
        shared.set(Data(repeating: 7, count: 32), for: "profile-remember.alice")
        let held = try ProfileFileLock.acquire(at: store.lockURL, timeout: 1)
        #expect(throws: ProfileError.lockUnavailable) { try store.login(username: "alice", password: "pw") }
        held.release()
        #expect(shared.data(for: "profile-remember.alice") == Data(repeating: 7, count: 32))

        shared.remove("profile-remember.alice")
        let held2 = try ProfileFileLock.acquire(at: store.lockURL, timeout: 1)
        #expect(throws: ProfileError.lockUnavailable) { try store.login(username: "alice", password: "pw") }
        held2.release()
        #expect(shared.data(for: "profile-remember.alice") == nil, "no key is left behind by a login that failed")
    }
}
