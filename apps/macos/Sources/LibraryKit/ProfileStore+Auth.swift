import CryptoKit
import Foundation

extension ProfileStore {
    /// Creates a profile and logs in as it. The username is trimmed and must be new (ignoring case); the password
    /// is used as typed. Runs one PBKDF2 (tens of milliseconds): call it off the main actor.
    public func register(username: String, password: String, rememberMe: Bool = true) throws(ProfileError) {
        let name = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { throw .emptyUsername }
        guard !password.isEmpty else { throw .emptyPassword }
        if read({ $0.profiles[name.lowercased()] != nil }) { throw .usernameTaken }

        let salt = ProfileCrypto.randomBytes(ProfileCrypto.saltLength)
        let derived = try ProfileCrypto.derive(password: password, salt: salt, iterations: ProfileCrypto.defaultIterations)
        let createdAt = clock()

        var secrets: [(account: String, value: Data?)] = []
        do throws(ProfileError) {
            try transact(requiresSession: false, fallback: ()) { (state: inout ProfileState) throws(ProfileError) -> ((), ProfileEffect) in
                guard state.profiles[name.lowercased()] == nil else { throw .usernameTaken }
                state.profiles[name.lowercased()] = ProfileRecord(
                    username: name, saltBase64: salt.base64EncodedString(),
                    passwordHashBase64: derived.passwordHash.base64EncodedString(),
                    iterations: ProfileCrypto.defaultIterations, createdAt: createdAt, encryptedVaultBase64: nil)
                var session = ProfileSession(
                    usernameKey: name.lowercased(), username: name, vaultKey: derived.vaultKey, contents: VaultContents(),
                    loadedCiphertext: nil, remembered: false)
                secrets = applyRememberChoice(rememberMe, state: &state, session: &session)
                state.session = session
                state.lastUsername = name
                return ((), [.vault, .header])
            }
        } catch {
            restore(secrets)
            throw error
        }
    }

    /// Unlocks a profile. The username match ignores case and the session keeps the registered casing. Throws
    /// `.invalidCredentials` for an unknown user and a wrong password alike (both cost one PBKDF2), and
    /// `.vaultUnreadable` if the password is right but the vault will not decrypt (the stored vault is not touched).
    /// Runs one PBKDF2: call it off the main actor.
    public func login(username: String, password: String, rememberMe: Bool = true) throws(ProfileError) {
        let name = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !password.isEmpty else { throw .missingCredentials }

        let candidate = read { $0.profiles[name.lowercased()] }
        let derived = try verifyPassword(password, against: candidate)

        var secrets: [(account: String, value: Data?)] = []
        do throws(ProfileError) {
            try transact(requiresSession: false, fallback: ()) { (state: inout ProfileState) throws(ProfileError) -> ((), ProfileEffect) in
                // The profile may have changed or vanished while the password was being checked.
                guard let current = state.profiles[name.lowercased()], current.saltBase64 == candidate?.saltBase64,
                    current.passwordHashBase64 == candidate?.passwordHashBase64
                else { throw .invalidCredentials }
                guard case .success(let contents) = Self.openVault(of: current, with: derived.vaultKey) else {
                    throw .vaultUnreadable
                }
                var session = ProfileSession(
                    usernameKey: name.lowercased(), username: current.username, vaultKey: derived.vaultKey,
                    contents: contents, loadedCiphertext: current.encryptedVaultBase64, remembered: false)
                secrets = applyRememberChoice(rememberMe, state: &state, session: &session)
                state.session = session
                state.lastUsername = current.username
                return ((), [.header])
            }
        } catch {
            restore(secrets)
            throw error
        }
    }

    /// Locks the profile in memory and forgets the remembered login (the stored key and `rememberedUsername`), for
    /// every instance. `rememberMe` and `lastUsername` stay. The local logout always happens; the error, if any, is
    /// only that it could not be written to disk (`.lockUnavailable`, `.writeFailed`).
    public func logout() throws(ProfileError) {
        lock.lock()
        defer { lock.unlock() }
        syncLocked()
        guard state.session != nil || state.rememberedUsername != nil else { return }

        for name in [state.session?.username, state.rememberedUsername].compactMap({ $0 }) {
            secretStore.remove(Self.rememberAccount(for: name))
        }
        state.session = nil
        state.rememberedUsername = nil
        hub.yield(.local)

        let fileLock = try ProfileFileLock.acquire(at: lockURL, timeout: lockTimeout)
        defer { fileLock.release() }
        syncLocked()
        if let name = state.rememberedUsername { secretStore.remove(Self.rememberAccount(for: name)) }
        state.session = nil
        state.rememberedUsername = nil
        var working = state
        try saveLocked(&working, vaultChanged: false)
        state = working
    }

    /// Deletes a profile and everything in its vault after checking its password; ends the session if it is the
    /// active one. Runs one PBKDF2: call it off the main actor.
    public func deleteProfile(username: String, password: String) throws(ProfileError) {
        let name = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !password.isEmpty else { throw .missingCredentials }

        let candidate = read { $0.profiles[name.lowercased()] }
        _ = try verifyPassword(password, against: candidate)

        let key = name.lowercased()
        try transact(requiresSession: false, fallback: ()) { (state: inout ProfileState) throws(ProfileError) -> ((), ProfileEffect) in
            guard let current = state.profiles[key], current.saltBase64 == candidate?.saltBase64,
                current.passwordHashBase64 == candidate?.passwordHashBase64
            else { throw .invalidCredentials }
            state.profiles[key] = nil
            if state.session?.usernameKey == key { state.session = nil }
            if state.rememberedUsername?.lowercased() == key { state.rememberedUsername = nil }
            return ((), [.header])
        }
        secretStore.remove(Self.rememberAccount(for: name))
    }

    // MARK: - Helpers

    private static let decoySalt = ProfileCrypto.randomBytes(ProfileCrypto.saltLength)

    /// Checks a password against a record in constant time. A missing or malformed record still pays for one PBKDF2, so
    /// response time does not reveal whether a username exists.
    func verifyPassword(_ password: String, against record: ProfileRecord?) throws(ProfileError) -> ProfileCrypto.Derived {
        if let record, let salt = Data(base64Encoded: record.saltBase64), !salt.isEmpty,
            let expected = Data(base64Encoded: record.passwordHashBase64), !expected.isEmpty,
            record.iterations <= ProfileCrypto.maxIterations
        {
            let derived = try ProfileCrypto.derive(password: password, salt: salt, iterations: record.iterations)
            guard ProfileCrypto.constantTimeEqual(derived.passwordHash, expected) else { throw .invalidCredentials }
            return derived
        }
        _ = try? ProfileCrypto.derive(password: password, salt: Self.decoySalt, iterations: ProfileCrypto.defaultIterations)
        throw .invalidCredentials
    }

    /// Stores or clears the remembered login for a session that is about to start and returns what the keychain held
    /// before, so a failed save can put it back. With remember-me on and the key stored, `rememberedUsername` becomes
    /// this user (and a previously remembered other user is forgotten); otherwise both are cleared.
    func applyRememberChoice(
        _ rememberMe: Bool, state: inout ProfileState, session: inout ProfileSession
    ) -> [(account: String, value: Data?)] {
        let account = Self.rememberAccount(for: session.username)
        let previous = state.rememberedUsername
        let previousAccount = previous.map(Self.rememberAccount(for:))
        var before: [(account: String, value: Data?)] = [(account, secretStore.data(for: account))]
        if let previousAccount, previousAccount != account { before.append((previousAccount, secretStore.data(for: previousAccount))) }

        state.rememberMe = rememberMe
        if rememberMe, secretStore.set(session.vaultKey.rawData, for: account) {
            if let previousAccount, previousAccount != account { secretStore.remove(previousAccount) }
            state.rememberedUsername = session.username
            session.remembered = true
        } else {
            if let previousAccount { secretStore.remove(previousAccount) }
            secretStore.remove(account)
            state.rememberedUsername = nil
            session.remembered = false
        }
        return before
    }

    func restore(_ secrets: [(account: String, value: Data?)]) {
        for (account, value) in secrets {
            if let value { secretStore.set(value, for: account) } else { secretStore.remove(account) }
        }
    }
}
