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

/// The Security framework calls `KeychainSecretStore` makes, so a test can make them fail.
protocol KeychainBackend: Sendable {
    func copyData(_ query: [String: Any]) -> Data?
    func update(_ query: [String: Any], with attributes: [String: Any]) -> OSStatus
    func add(_ item: [String: Any]) -> OSStatus
    func delete(_ query: [String: Any]) -> OSStatus
}

struct SecurityKeychain: KeychainBackend {
    func copyData(_ query: [String: Any]) -> Data? {
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess else { return nil }
        return result as? Data
    }

    func update(_ query: [String: Any], with attributes: [String: Any]) -> OSStatus {
        SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
    }

    func add(_ item: [String: Any]) -> OSStatus {
        SecItemAdd(item as CFDictionary, nil)
    }

    func delete(_ query: [String: Any]) -> OSStatus {
        SecItemDelete(query as CFDictionary)
    }
}

/// Generic-password items (service `defaultService`, one account per secret) in the user's login keychain.
///
/// What is and is not enforced. The app is not sandboxed and has no keychain-access-groups entitlement, so these are
/// items of the legacy file-based keychain. Its protection is the keychain's own: the items are encrypted with the
/// login keychain, which is locked with the user's login (and, with the default settings, when the Mac sleeps), and
/// the access control list of an item names the app that created it, so another app asking for it gets a prompt.
/// `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` is passed along but has **no effect** on that keychain: it only
/// counts for the data protection keychain (`kSecUseDataProtectionKeychain`), which needs an entitlement the app
/// does not have. File-based keychain items are not synced through iCloud Keychain either way. An ad-hoc signed
/// rebuild is a different "app" to the ACL, so after a rebuild the system may ask once before handing a key over;
/// callers therefore never call this while holding a lock.
public struct KeychainSecretStore: SecretStore {
    /// The bundle identifier of the packaged app (`scripts/package-macos.sh`); a plain `swift run` has none, so the
    /// service name is fixed rather than read from the bundle.
    public static let defaultService = "app.subtitleadder.mac"

    public let service: String
    let backend: any KeychainBackend

    public init(service: String = KeychainSecretStore.defaultService) {
        self.init(service: service, backend: SecurityKeychain())
    }

    init(service: String, backend: any KeychainBackend) {
        self.service = service
        self.backend = backend
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
        return backend.copyData(query)
    }

    /// Updates the item or adds it. When the update fails for a reason other than "no such item" (an item left by
    /// another build with a different access list, for example) the old item is deleted and a new one added, once.
    @discardableResult public func set(_ data: Data, for account: String) -> Bool {
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
        ]
        let status = backend.update(query(account), with: attributes)
        if status == errSecSuccess { return true }
        if status != errSecItemNotFound { _ = backend.delete(query(account)) }
        let item = query(account).merging(attributes) { $1 }
        return backend.add(item) == errSecSuccess
    }

    public func remove(_ account: String) {
        _ = backend.delete(query(account))
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
