import CryptoKit
import Foundation

/// Local profiles with passwords and an encrypted vault per profile (watch history, favorites, ratings, custom lists,
/// thumbnails), stored in `profiles.json` in the format the Windows app uses.
///
/// **Concurrency.** A `final class` that is `Sendable` by construction: all state lives in one value guarded by an
/// `NSLock`, and every public method is synchronous. A class rather than an actor because the view model needs cheap
/// synchronous reads (`isLoggedIn`, `stats(for:)`) and because file locks, `stat` and PBKDF2 would otherwise block a
/// cooperative-pool thread; callers that must not wait (PBKDF2 at 100 000 rounds takes tens of milliseconds) call the
/// slow methods (`register`, `login`, `deleteProfile`) off the main actor, e.g. from a detached task. The in-process lock
/// is never held during PBKDF2 or while a change is delivered, and is always taken before the file lock, so two stores
/// in one process (or in two processes) cannot deadlock.
///
/// **Multiple instances.** Every read-modify-write holds an exclusive `flock` on `profiles.json.lock` and first
/// reloads the file if its content changed since this store last looked, so concurrent instances merge instead of
/// overwriting each other. A watcher on the folder (debounced 300 ms) notices changes made while nothing is calling the
/// store and raises `ProfileChange.external`. Writes are atomic, and a damaged file is set aside, never overwritten.
///
/// **Remember me.** The vault key of a remembered login lives in the `SecretStore` (the keychain), never in
/// `profiles.json`, which keeps only `rememberMe` and `rememberedUsername`. That pair is shared by all instances: one
/// that finds a remembered login while it has no user logs in with it, and one that logged in through it is logged
/// out when another instance logs out.
public final class ProfileStore: @unchecked Sendable {
    public let fileURL: URL
    let lockURL: URL
    let secretStore: any SecretStore
    let clock: @Sendable () -> Date
    let lockTimeout: TimeInterval

    /// Guards `state`. Never held while waiting on anything but the file lock.
    let lock = NSLock()
    var state = ProfileState()
    let hub = ProfileChangeHub()
    private var watcher: ProfileFileWatcher?

    /// - Parameters:
    ///   - fileURL: `profiles.json`; defaults to the one in the app data folder.
    ///   - secretStore: where remembered-login keys go; defaults to the keychain.
    ///   - watchesFile: watch the folder for changes made by other instances.
    ///   - lockTimeout: how long to wait for the cross-process lock before failing with `.lockUnavailable`.
    ///   - clock: the time source for timestamps (injectable for tests).
    public init(
        fileURL: URL? = nil, secretStore: any SecretStore = KeychainSecretStore(), watchesFile: Bool = true,
        lockTimeout: TimeInterval = 5, clock: @escaping @Sendable () -> Date = { Date() }
    ) {
        let url = (fileURL ?? AppDataDirectory.file("profiles.json")).standardizedFileURL
        self.fileURL = url
        lockURL = url.appendingPathExtension("lock")
        self.secretStore = secretStore
        self.clock = clock
        self.lockTimeout = lockTimeout

        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        lock.withLock { _ = syncLocked(force: true) }
        if watchesFile {
            watcher = ProfileFileWatcher(folder: url.deletingLastPathComponent()) { [weak self] in self?.refresh() }
        }
    }

    deinit {
        watcher?.stop()
        hub.finishAll()
    }

    /// A new stream of change signals, each starting empty (changes before this call are not replayed). Delivery is
    /// from an arbitrary thread; hop to the main actor in the consumer.
    public func changes() -> AsyncStream<ProfileChange> {
        hub.stream()
    }

    /// Looks at the file now (cheap if it did not change). The watcher calls this; call it too when the app becomes
    /// active again.
    public func refresh() {
        lock.withLock { _ = syncLocked() }
    }

    // MARK: - State

    /// Whether a profile is unlocked in this store.
    public var isLoggedIn: Bool { read { $0.session != nil } }

    /// The unlocked profile's username in its registered casing, or `nil`.
    public var currentUsername: String? { read { $0.session?.username } }

    /// All registered usernames, sorted alphabetically (ignoring case).
    public var registeredUsernames: [String] {
        read { state in
            state.profiles.values.map(\.username).sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
        }
    }

    /// Whether the user asked to stay logged in across launches.
    public var rememberMe: Bool { read { $0.rememberMe } }

    /// The username that last logged in or registered, to pre-fill the login box.
    public var lastUsername: String? { read { $0.lastUsername } }

    /// The username a new instance would log in as without a password, if any.
    public var rememberedUsername: String? { read { $0.effectiveRemembered } }

    /// Counts over the unlocked profile (all zero while logged out).
    public func totals() -> ProfileTotals {
        read { state in
            guard let contents = state.session?.contents else { return .zero }
            return contents.videos.values.reduce(into: ProfileTotals.zero) { totals, video in
                if video.viewCount > 0 { totals.watched += 1 }
                totals.views += video.viewCount
                if video.isLiked { totals.liked += 1 }
                if video.rating > 0 { totals.rated += 1 }
            }
        }
    }

    static func rememberAccount(for username: String) -> String {
        "profile-remember.\(username.lowercased())"
    }

    // MARK: - Reading the file

    func read<T>(_ body: (ProfileState) -> T) -> T {
        lock.lock()
        defer { lock.unlock() }
        syncLocked()
        return body(state)
    }

    /// Reloads `profiles.json` if its content changed since the last look. Lock-free with respect to other processes:
    /// the file is only ever replaced atomically. Returns whether anything a view would show changed (and has then
    /// raised `.external`). Must be called with `lock` held.
    @discardableResult
    func syncLocked(force: Bool = false) -> Bool {
        let signature = ProfileFileIO.signature(of: fileURL)
        if !force, state.hasSynced, signature == state.lastSignature { return false }
        let snapshot = ProfileFileIO.snapshot(of: fileURL)
        if !force, state.hasSynced, snapshot == state.lastSnapshot {
            state.lastSignature = signature
            return false
        }
        let visible = apply(snapshot, signature: signature)
        if visible { hub.yield(.external) }
        return visible
    }

    private func apply(_ snapshot: ProfileFileSnapshot, signature: FileSignature?) -> Bool {
        let oldRemembered = state.effectiveRemembered
        let oldRememberMe = state.rememberMe
        let oldNames = Set(state.profiles.keys)
        var visible = false

        switch snapshot {
        case .missing:
            state.profiles = [:]
            state.fileProblem = nil
        case .unreadable:
            state.fileProblem = .corrupt
        case .content(let data):
            if let document = try? ProfileJSON.decode(ProfilesDocument.self, from: data) {
                if document.version > 2 {
                    state.fileProblem = .unsupportedVersion(document.version)
                } else {
                    state.fileProblem = nil
                    state.load(document)
                }
            } else {
                // Keep what we have in memory: the next write sets the damaged file aside and re-saves it.
                state.fileProblem = .corrupt
            }
        }
        state.lastSnapshot = snapshot
        state.lastSignature = signature
        state.hasSynced = true

        if Set(state.profiles.keys) != oldNames || state.rememberMe != oldRememberMe { visible = true }

        // The active profile may have been written, deleted or locked by someone else.
        if var session = state.session {
            if let record = state.profiles[session.usernameKey] {
                if record.encryptedVaultBase64 != session.loadedCiphertext {
                    switch Self.openVault(of: record, with: session.vaultKey) {
                    case .success(let contents):
                        if contents.differsVisibly(from: session.contents) { visible = true }
                        session.contents = contents
                        session.loadedCiphertext = record.encryptedVaultBase64
                        state.session = session
                    case .failure:
                        state.session = nil
                        visible = true
                    }
                }
            } else {
                state.session = nil
                visible = true
            }
        }

        // Another instance logged out of the shared remembered login: leave it too, but only a session that stands
        // for that login. A session started without remember-me cannot be attributed to the change and stays.
        if let session = state.session, session.remembered, state.effectiveRemembered == nil,
            oldRemembered?.lowercased() == session.usernameKey
        {
            state.session = nil
            visible = true
        }

        // A remembered login made elsewhere (or restored at launch) logs in a store that has no user.
        if restoreRememberedSession() { visible = true }
        return visible
    }

    /// Logs in with the remembered login if there is one, a stored key and a vault that opens with it.
    private func restoreRememberedSession() -> Bool {
        guard state.session == nil, let name = state.effectiveRemembered, let record = state.profiles[name.lowercased()],
            let keyData = secretStore.data(for: Self.rememberAccount(for: name)), keyData.count == ProfileCrypto.keyLength
        else { return false }
        let key = SymmetricKey(data: keyData)
        guard case .success(let contents) = Self.openVault(of: record, with: key) else { return false }
        state.session = ProfileSession(
            usernameKey: record.username.lowercased(), username: record.username, vaultKey: key, contents: contents,
            loadedCiphertext: record.encryptedVaultBase64, remembered: true)
        return true
    }

    static func openVault(of record: ProfileRecord, with key: SymmetricKey) -> Result<VaultContents, ProfileError> {
        guard let encoded = record.encryptedVaultBase64, !encoded.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return .success(VaultContents())
        }
        guard let blob = Data(base64Encoded: encoded), let plaintext = ProfileCrypto.open(blob, key: key),
            let payload = try? ProfileJSON.decode(VaultPayload.self, from: plaintext)
        else { return .failure(.vaultUnreadable) }
        return .success(VaultContents(payload: payload))
    }

    // MARK: - Writing the file

    /// Encrypts the active vault if it changed and writes `profiles.json`. Needs `lock` and the file lock. Throws
    /// before touching anything if the existing file cannot be preserved; a damaged one is first set aside.
    func saveLocked(_ working: inout ProfileState, vaultChanged: Bool) throws(ProfileError) {
        if vaultChanged, var session = working.session {
            guard let plaintext = try? ProfileJSON.encode(session.contents.payload(), pretty: false) else {
                throw .writeFailed(errno: EINVAL)
            }
            let encoded = try ProfileCrypto.seal(plaintext, key: session.vaultKey).base64EncodedString()
            working.profiles[session.usernameKey]?.encryptedVaultBase64 = encoded
            session.loadedCiphertext = encoded
            working.session = session
        }
        guard let data = try? ProfileJSON.encode(working.document(), pretty: true) else { throw .writeFailed(errno: EINVAL) }

        switch working.fileProblem {
        case .unsupportedVersion(let version): throw .unsupportedFileVersion(version)
        case .corrupt: try ProfileFileIO.preserveDamagedFile(at: fileURL, now: clock())
        case nil: if working.lastSnapshot == .content(data) { return }
        }
        try ProfileFileIO.writeAtomically(data, to: fileURL)
        working.fileProblem = nil
        working.lastSnapshot = .content(data)
        working.lastSignature = ProfileFileIO.signature(of: fileURL)
        working.hasSynced = true
    }

    /// Runs one read-modify-write: take the in-process lock, then the file lock, reload if the file moved on, apply
    /// `body` to a copy of the state and write it. The copy is committed only if the write succeeded, so a failure
    /// leaves memory as it was. With `requiresSession`, a logged-out store returns `fallback` without writing or locking.
    func transact<T>(
        requiresSession: Bool = true, fallback: @autoclosure () -> T,
        _ body: (inout ProfileState) throws(ProfileError) -> (T, ProfileEffect)
    ) throws(ProfileError) -> T {
        lock.lock()
        defer { lock.unlock() }
        syncLocked()
        if requiresSession, state.session == nil { return fallback() }

        let fileLock = try ProfileFileLock.acquire(at: lockURL, timeout: lockTimeout)
        defer { fileLock.release() }
        syncLocked()
        if requiresSession, state.session == nil { return fallback() }

        var working = state
        let (value, effect) = try body(&working)
        if effect.needsSave { try saveLocked(&working, vaultChanged: effect.contains(.vault)) }
        state = working
        if effect.needsSave, !effect.contains(.silent) { hub.yield(.local) }
        return value
    }

    /// Reads from the unlocked profile after following any moved or renamed files among `probes`. The migration is
    /// persisted when the file lock can be had; if it cannot, the result still reflects it and it is saved with the
    /// next write. Returns `nil` while logged out.
    func readMigrating<T>(_ probes: [VideoProbe], _ body: (ProfileSession) -> T) -> T? {
        lock.lock()
        defer { lock.unlock() }
        syncLocked()
        guard state.session != nil else { return nil }
        if !probes.isEmpty, wouldMigrateLocked(probes) { migrateLocked(probes) }
        guard let session = state.session else { return nil }
        return body(session)
    }

    private func wouldMigrateLocked(_ probes: [VideoProbe]) -> Bool {
        guard var trial = state.session?.contents else { return false }
        let now = clock()
        return probes.contains { trial.resolve($0, now: now)?.migrated == true }
    }

    private func migrateLocked(_ probes: [VideoProbe]) {
        guard let fileLock = try? ProfileFileLock.acquire(at: lockURL, timeout: lockTimeout) else { return }
        defer { fileLock.release() }
        syncLocked()
        guard state.session != nil else { return }

        var working = state
        let now = clock()
        var migrated = false
        for probe in probes {
            if working.session?.contents.resolve(probe, now: now)?.migrated == true { migrated = true }
        }
        guard migrated else { return }

        var saved = working
        if (try? saveLocked(&saved, vaultChanged: true)) != nil {
            state = saved
        } else {
            state = working
        }
    }
}
