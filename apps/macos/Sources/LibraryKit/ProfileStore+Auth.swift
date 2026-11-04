import CryptoKit
import Foundation

extension ProfileStore {
    /// Creates a profile and logs in as it. The username is trimmed and must be new (ignoring case); the password
    /// is used as typed. Runs one PBKDF2 (tens of milliseconds) and talks to the secret store: call it off the main actor.
    public func register(username: String, password: String, rememberMe: Bool = true) throws(ProfileError) {
        let name = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { throw .emptyUsername }
        guard !password.isEmpty else { throw .emptyPassword }
        if read({ $0.profiles[name.lowercased()] != nil }) { throw .usernameTaken }

        let salt = ProfileCrypto.randomBytes(ProfileCrypto.saltLength)
        let derived = try ProfileCrypto.derive(password: password, salt: salt, iterations: ProfileCrypto.defaultIterations)
        let createdAt = clock()

        // Whoever is logged in now keeps the progress they have not saved yet; the secret store is asked before any lock is taken.
        _ = try? persistDirty(minInterval: 0)
        let step = storeRememberKey(rememberMe, username: name, key: derived.vaultKey)
        var previouslyRemembered: String?
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
                previouslyRemembered = state.rememberedUsername
                applyRememberChoice(rememberMe, step: step, state: &state, session: &session)
                state.startSession(session)
                state.lastUsername = name
                return ((), [.vault, .header])
            }
        } catch {
            rollBack(step)
            throw error
        }
        finish(step, previouslyRemembered: previouslyRemembered)
    }

    /// Unlocks a profile. The username match ignores case and the session keeps the registered casing. Throws
    /// `.invalidCredentials` for an unknown user and a wrong password alike (both cost one PBKDF2), and
    /// `.vaultUnreadable` if the password is right but the vault will not decrypt (the stored vault is not touched);
    /// `.fileUnreadable` if `profiles.json` cannot be read at all right now. Runs one PBKDF2 and talks to the
    /// secret store: call it off the main actor.
    public func login(username: String, password: String, rememberMe: Bool = true) throws(ProfileError) {
        let name = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !password.isEmpty else { throw .missingCredentials }

        let (candidate, problem) = read { ($0.profiles[name.lowercased()], $0.fileProblem) }
        if candidate == nil, problem == .unreadable { throw .fileUnreadable }
        let derived = try verifyPassword(password, against: candidate)

        _ = try? persistDirty(minInterval: 0)
        let step = storeRememberKey(rememberMe, username: candidate?.username ?? name, key: derived.vaultKey)
        var previouslyRemembered: String?
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
                previouslyRemembered = state.rememberedUsername
                applyRememberChoice(rememberMe, step: step, state: &state, session: &session)
                state.startSession(session)
                state.lastUsername = current.username
                return ((), [.header])
            }
        } catch {
            rollBack(step)
            throw error
        }
        finish(step, previouslyRemembered: previouslyRemembered)
    }

    /// Locks the profile in memory and forgets the remembered login (the stored key and `rememberedUsername`), for
    /// every instance. `rememberMe` and `lastUsername` stay. The local logout always happens; the error, if any, is
    /// only that it could not be written to disk (`.lockUnavailable`, `.writeFailed`). Unsaved progress is written
    /// first, on a best-effort basis. Talks to the secret store: call it off the main actor.
    public func logout() throws(ProfileError) {
        _ = try? persistDirty(minInterval: 0)

        // 1. Lock and forget in memory, under the in-process lock only.
        lock.lock()
        syncLocked()
        guard state.session != nil || state.rememberedUsername != nil else {
            lock.unlock()
            return
        }
        let forgotten = [state.session?.username, state.rememberedUsername].compactMap { $0 }
        state.endSession()
        state.rememberedUsername = nil
        lock.unlock()
        hub.yield(.local)

        // 2. Remove the keys, outside every lock.
        for name in forgotten { secretStore.remove(Self.rememberAccount(for: name)) }

        // 3. Write the forgotten state. The file lock is waited for without the in-process lock.
        let fileLock = try ProfileFileLock.acquire(at: lockURL, timeout: lockTimeout)
        defer { fileLock.release() }
        var rememberedMeanwhile: String?
        do throws(ProfileError) {
            lock.lock()
            defer { lock.unlock() }
            syncLocked()
            rememberedMeanwhile = state.rememberedUsername
            state.endSession()
            state.rememberedUsername = nil
            var working = state
            try saveLocked(&working, vaultChanged: false)
            state = working
        } catch {
            // A login another instance remembered meanwhile was cleared in memory but not on disk; its key goes anyway.
            if let name = rememberedMeanwhile { secretStore.remove(Self.rememberAccount(for: name)) }
            throw error
        }
        if let name = rememberedMeanwhile { secretStore.remove(Self.rememberAccount(for: name)) }
    }

    /// Deletes a profile and everything in its vault after checking its password; ends the session if it is the
    /// active one. Runs one PBKDF2 and talks to the secret store: call it off the main actor.
    public func deleteProfile(username: String, password: String) throws(ProfileError) {
        let name = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !password.isEmpty else { throw .missingCredentials }

        let (candidate, problem) = read { ($0.profiles[name.lowercased()], $0.fileProblem) }
        if candidate == nil, problem == .unreadable { throw .fileUnreadable }
        _ = try verifyPassword(password, against: candidate)

        let key = name.lowercased()
        try transact(requiresSession: false, fallback: ()) { (state: inout ProfileState) throws(ProfileError) -> ((), ProfileEffect) in
            guard let current = state.profiles[key], current.saltBase64 == candidate?.saltBase64,
                current.passwordHashBase64 == candidate?.passwordHashBase64
            else { throw .invalidCredentials }
            state.profiles[key] = nil
            if state.session?.usernameKey == key { state.endSession() }
            if state.rememberedUsername?.lowercased() == key { state.rememberedUsername = nil }
            return ((), [.header])
        }
        thumbnailCache.removeAll(profile: key)
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

    // MARK: - Remember-me and the secret store

    /// What the secret store held for a session's account before this login touched it, and whether the key is stored now.
    /// The secret store is read and written before the transaction (outside the file lock), and
    /// `rollBack` / `finish` settle it afterwards.
    struct RememberStep {
        let account: String
        let before: Data?
        /// Remember-me is on and the key is in the secret store.
        let stored: Bool
    }

    /// Stores the vault key for a remembered login (or, with remember-me off, only notes what was there). Call it
    /// outside every lock.
    func storeRememberKey(_ rememberMe: Bool, username: String, key: SymmetricKey) -> RememberStep {
        let account = Self.rememberAccount(for: username)
        let before = secretStore.data(for: account)
        let stored = rememberMe && secretStore.set(key.rawData, for: account)
        return RememberStep(account: account, before: before, stored: stored)
    }

    /// The in-memory half of remember-me, run inside the transaction: with the key stored, `rememberedUsername`
    /// becomes this user; otherwise it is cleared.
    func applyRememberChoice(_ rememberMe: Bool, step: RememberStep, state: inout ProfileState, session: inout ProfileSession) {
        state.rememberMe = rememberMe
        if step.stored {
            state.rememberedUsername = session.username
            session.remembered = true
        } else {
            state.rememberedUsername = nil
            session.remembered = false
        }
    }

    /// Puts the secret store back as it was when the transaction failed.
    func rollBack(_ step: RememberStep) {
        if let before = step.before { secretStore.set(before, for: step.account) } else { secretStore.remove(step.account) }
    }

    /// After a successful transaction: drop a key that did not become the remembered one and the one of the user who
    /// was remembered before.
    func finish(_ step: RememberStep, previouslyRemembered: String?) {
        if !step.stored { secretStore.remove(step.account) }
        if let previous = previouslyRemembered {
            let previousAccount = Self.rememberAccount(for: previous)
            if previousAccount != step.account { secretStore.remove(previousAccount) }
        }
    }
}
