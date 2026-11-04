import Foundation

/// Small secrets the app must not write into its JSON files: translation provider API keys and the remembered-login
/// key of a local profile. Values are opaque bytes stored under an account name.
public protocol SecretStore: Sendable {
    func data(for account: String) -> Data?
    /// Like `data(for:)`, but tells "nothing stored" from "could not be read".
    /// A caller that is about to overwrite or delete something must not take the second for the
    /// first. The default has no way to tell and reports a missing value as `.absent`.
    func read(_ account: String) -> SecretRead
    /// Stores `data`, replacing any previous value. Returns `false` when the store refused it.
    @discardableResult func set(_ data: Data, for account: String) -> Bool
    func remove(_ account: String)
}

/// What reading a secret found.
public enum SecretRead: Sendable, Equatable {
    case found(Data)
    /// The store answered: there is no such secret.
    case absent
    /// The store could not be read, so nothing is known about the secret.
    case unavailable
}

extension SecretStore {
    public func read(_ account: String) -> SecretRead {
        data(for: account).map(SecretRead.found) ?? .absent
    }

    public func string(for account: String) -> String? {
        data(for: account).map { String(decoding: $0, as: UTF8.self) }
    }

    @discardableResult public func set(_ string: String, for account: String) -> Bool {
        set(Data(string.utf8), for: account)
    }
}

/// Keeps credentials only until the process exits.
public final class InMemorySecretStore: SecretStore, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Data] = [:]

    public init(_ values: [String: Data] = [:]) {
        self.values = values
    }

    public func data(for account: String) -> Data? {
        lock.withLock { values[account] }
    }

    @discardableResult public func set(_ data: Data, for account: String) -> Bool {
        lock.withLock { values[account] = data }
        return true
    }

    public func remove(_ account: String) {
        _ = lock.withLock { values.removeValue(forKey: account) }
    }

    /// Accounts currently stored, for tests.
    public var accounts: Set<String> {
        lock.withLock { Set(values.keys) }
    }
}
