import Foundation
import Testing
@testable import LibraryKit

/// A keychain that takes its time to answer (a prompt on an ad-hoc signed build).
private final class SlowSecrets: SecretStore, @unchecked Sendable {
    private let inner = InMemorySecretStore()
    private let delay: TimeInterval
    private let lock = NSLock()
    private var slow = false

    init(delay: TimeInterval) { self.delay = delay }

    func makeSlow() { lock.withLock { slow = true } }

    func data(for account: String) -> Data? {
        if lock.withLock({ slow }) { Thread.sleep(forTimeInterval: delay) }
        return inner.data(for: account)
    }

    @discardableResult func set(_ data: Data, for account: String) -> Bool { inner.set(data, for: account) }
    func remove(_ account: String) { inner.remove(account) }
}

@Suite final class ProfileRestoreTests {
    let folder = TestFolder()

    @Test func refreshWaitsForARestoreAnotherThreadHasUnderWay() async throws {
        let secrets = SlowSecrets(delay: 0.4)
        let first = folder.makeStore(secrets: secrets)
        try first.register(username: "emre", password: "pw")

        // A new launch: one thread's plain read starts the restore and waits on the keychain.
        secrets.makeSlow()
        let second = folder.makeStore(secrets: secrets)
        let reader = Task.detached { second.currentUsername }
        try await Task.sleep(for: .milliseconds(100))
        // A plain read meanwhile does not wait: it sees nobody yet.
        #expect(second.isLoggedIn == false)
        // `refresh()` does: when it returns the restore is over.
        second.refresh()
        #expect(second.isLoggedIn)
        #expect(await reader.value == "emre")
    }
}
