import Foundation
import Security

/// Small secrets the app must not write into its JSON files: translation provider API keys and the remembered-login
/// key of a local profile. Values are opaque bytes stored under an account name.
public protocol SecretStore: Sendable {
    func data(for account: String) -> Data?
    /// Stores `data`, replacing any previous value. Returns `false` when the store refused it.
    @discardableResult func set(_ data: Data, for account: String) -> Bool
    func remove(_ account: String)
}

extension SecretStore {
    public func string(for account: String) -> String? {
        data(for: account).map { String(decoding: $0, as: UTF8.self) }
    }

    @discardableResult public func set(_ string: String, for account: String) -> Bool {
        set(Data(string.utf8), for: account)
    }
}

/// Generic-password items in the user's login keychain, readable only while the Mac is unlocked and never synced.
public struct KeychainSecretStore: SecretStore {
    /// The bundle identifier of the packaged app (`scripts/package-macos.sh`); a plain `swift run` has none, so the
    /// service name is fixed rather than read from the bundle.
    public static let defaultService = "app.subtitleadder.mac"

    public let service: String

    public init(service: String = KeychainSecretStore.defaultService) {
        self.service = service
    }

    private func query(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    public func data(for account: String) -> Data? {
        var query = query(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess else { return nil }
        return result as? Data
    }

    @discardableResult public func set(_ data: Data, for account: String) -> Bool {
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
        ]
        let status = SecItemUpdate(query(account) as CFDictionary, attributes as CFDictionary)
        if status == errSecSuccess { return true }
        guard status == errSecItemNotFound else { return false }
        let item = query(account).merging(attributes) { $1 }
        return SecItemAdd(item as CFDictionary, nil) == errSecSuccess
    }

    public func remove(_ account: String) {
        SecItemDelete(query(account) as CFDictionary)
    }
}

/// A process-local store for tests (and as a fallback where the keychain can't be used).
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
