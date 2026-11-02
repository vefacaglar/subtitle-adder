import CryptoKit
import Foundation

/// Local profiles with passwords and an encrypted vault per profile (watch history, favorites, ratings, custom lists,
/// thumbnails), stored in `profiles.json` (file format version 2 only; a file written by a newer or different version of
/// the app, such as the Windows app's version 3 with its SQLite `profiles.db`, is refused and never modified).
///
/// **Concurrency.** A `final class` that is `Sendable` by construction: all state lives in one value guarded by an
/// `NSLock`, and every public method is synchronous. A class rather than an actor because the view model needs cheap
/// synchronous reads (`isLoggedIn`, `totals()`). The in-process lock is only ever held for in-memory work, one `stat`
/// and (when another instance changed the file) a re-read, or one vault write. It is never held while waiting for the
/// cross-process file lock, while the keychain is asked, during PBKDF2 or while a file is fingerprinted, and a change is
/// delivered after it is released. The file lock is always taken first, so two stores in one process (or in two
/// processes) cannot deadlock.
///
/// **What touches the disk, so what must not run on the main actor.**
/// - Memory and at most one `stat` of `profiles.json` (it is read and decrypted again only when another instance
///   changed it; the folder watcher usually does that first): `isLoggedIn`, `currentUsername`, `registeredUsernames`,
///   `rememberMe`, `lastUsername`, `rememberedUsername`, `totals()`, `lists()`, `hasUnsavedChanges`, `changes()`. Safe on
///   the main actor, and never blocked by another instance's write or by the file lock. One exception: the first call
///   after a remembered login appears (at launch, or made by another instance) asks the keychain for the key, which
///   can show an access prompt on an ad-hoc signed build. Call `refresh()` from a background task at launch to take
///   that hit off the main actor.
/// - Reading files to fingerprint them (up to 192 KB each the first time, one `stat` afterwards, see
///   `VideoFingerprint`): `stats(for:)`, `thumbnail(for:)`, `lists(containing:)`, `isVideo(_:inList:)`,
///   `feed(_:search:discovered:)` and every method that takes a video URL (`recordWatch`, `updatePlaybackProgress`,
///   `toggleLike`, `setLiked`, `setRating`, `setThumbnail`, `addVideo`, `removeVideo`, `toggleVideo`). Call these from a
///   background task. The read-only ones take `fingerprints:` to stay on the main actor: `.pathOnly` touches no file
///   at all, `.cachedOnly` costs one `stat` per file.
/// - Writing (a file lock that may be waited for up to `lockTimeout`, encryption, `fsync`): everything that throws.
///   `updatePlaybackProgress` writes at most every `progressSaveInterval` seconds; the rest write on every call.
/// - Key derivation (PBKDF2, tens of milliseconds): `register`, `login`, `deleteProfile`.
/// - The keychain: `register`, `login`, `logout`, `deleteProfile` and the first call after a remembered login appears.
///
/// **Reads never write.** Following a moved file while reading (`stats(for:)`, `feed`, `lists(containing:)`) updates the
/// profile in memory, so the result already reflects the move, and the new path is saved with the next write of the
/// vault or by `persistPendingMigrations()`. Playback progress is kept in memory the same way and saved at most every
/// 15 seconds; the app calls `flushPlaybackProgress()` on pause, seek, close and quit. A change that is still
/// unsaved when the process ends is lost (a moved file is found again on the next lookup; some progress is not).
///
/// **Multiple instances.** Every read-modify-write holds an exclusive `flock` on `profiles.json.lock` and first
/// reloads the file if its content changed since this store last looked, so concurrent instances merge instead of
/// overwriting each other (unsaved progress is kept on top of what the other instance wrote). A watcher on the folder
/// (debounced 300 ms) notices changes made while nothing is calling the store and raises `ProfileChange.external`.
/// Writes are atomic. A damaged file is set aside, never overwritten; one written by a newer format or one that cannot
/// be read right now is left alone and writes fail (`.unsupportedFileVersion`, `.fileUnreadable`) until it can be read.
///
/// **Remember me.** The vault key of a remembered login lives in the `SecretStore` (the keychain), never in
/// `profiles.json`, which keeps only `rememberMe` and `rememberedUsername`. That pair is shared by all instances: one
/// that finds a remembered login while it has no user logs in with it (only if the profile has a vault the key
/// opens), and one that logged in through it is logged out when another instance logs out.
public final class ProfileStore: @unchecked Sendable {
    public let fileURL: URL
    let lockURL: URL
    let secretStore: any SecretStore
    let clock: @Sendable () -> Date
    let lockTimeout: TimeInterval

    /// Playback progress is written at most this often (seconds); `flushPlaybackProgress()` writes it at once.
    public static let progressSaveInterval: TimeInterval = 15

    /// Guards `state`. Never held while waiting on anything, and never during a keychain call.
    let lock = NSLock()
    var state = ProfileState()
    let hub = ProfileChangeHub()
    private var watcher: ProfileFileWatcher?

    /// Reads `profiles.json` (if it exists). Does not ask the keychain: a remembered login is restored by the first
    /// call that needs to know (or by `refresh()`).
    ///
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

    /// Looks at the file now (cheap if it did not change) and restores a remembered login if there is one. The watcher
    /// calls this; call it too when the app becomes active again, and once from a background task at launch (the
    /// keychain read of a remembered login happens here).
    public func refresh() {
        syncAndRestore()
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

    /// Whether playback progress or a followed move is held in memory and not yet in the file. A quit path can use it
    /// to decide whether `flushPlaybackProgress()` has anything to do.
    public var hasUnsavedChanges: Bool { read { $0.session?.dirty == true } }

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

    /// Brings the state up to date (see `syncAndRestore()`), then runs `body` on it under the lock. `body` must not do I/O.
    func read<T>(_ body: (ProfileState) -> T) -> T {
        syncAndRestore()
        lock.lock()
        defer { lock.unlock() }
        return body(state)
    }

    /// Reloads `profiles.json` if its content changed since the last look, then, if a remembered login is waiting, asks
    /// the keychain for its key (outside the lock) and logs in with it.
    func syncAndRestore() {
        let candidate: RestoreCandidate? = lock.withLock {
            syncLocked()
            return restoreCandidateLocked()
        }
        if let candidate { restoreRememberedSession(candidate) }
    }

    /// Reloads `profiles.json` if its content changed since the last look. Lock-free with respect to other processes:
    /// the file is only ever replaced atomically. A file that could not be read is read again every time, because
    /// restoring its permissions changes neither its size nor its modification time. Returns whether anything a view
    /// would show changed (and has then raised `.external`). Must be called with `lock` held.
    @discardableResult
    func syncLocked(force: Bool = false) -> Bool {
        let signature = ProfileFileIO.signature(of: fileURL)
        let retrying = state.lastSnapshot == .unreadable
        if !force, !retrying, state.hasSynced, signature == state.lastSignature { return false }
        let snapshot = ProfileFileIO.snapshot(of: fileURL)
        if !force, state.hasSynced, snapshot == state.lastSnapshot {
            state.lastSignature = signature
            return false
        }
        let visible = apply(snapshot, signature: signature)
        if visible { hub.yield(.external) }
        return visible
    }

    private enum ParsedFile {
        case document(ProfilesDocument)
        case unsupported(Int)
        case damaged
    }

    /// Just the field that tells whether the rest can be understood.
    private struct VersionHeader: Decodable {
        var version: Int?
    }

    /// The version is read on its own first: a file from a newer format may have a shape this build cannot decode, and
    /// that must never be mistaken for damage (a damaged file is set aside and replaced).
    private static func parse(_ data: Data) -> ParsedFile {
        guard let header = try? ProfileJSON.decode(VersionHeader.self, from: data) else { return .damaged }
        if let version = header.version, version > 2 { return .unsupported(version) }
        guard let document = try? ProfileJSON.decode(ProfilesDocument.self, from: data) else { return .damaged }
        return .document(document)
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
            // Keep what we have in memory and refuse to write until the file can be read again.
            state.fileProblem = .unreadable
        case .content(let data):
            switch Self.parse(data) {
            case .document(let document):
                state.fileProblem = nil
                state.load(document)
            case .unsupported(let version):
                state.fileProblem = .unsupportedVersion(version)
            case .damaged:
                // Keep what we have in memory: the next write sets the damaged file aside and re-saves it.
                state.fileProblem = .corrupt
            }
        }
        state.lastSnapshot = snapshot
        state.lastSignature = signature
        state.hasSynced = true
        state.syncGeneration += 1

        if Set(state.profiles.keys) != oldNames || state.rememberMe != oldRememberMe { visible = true }

        // The active profile may have been written, deleted or locked by someone else.
        if var session = state.session {
            if let record = state.profiles[session.usernameKey] {
                if record.encryptedVaultBase64 != session.loadedCiphertext {
                    switch Self.openVault(of: record, with: session.vaultKey) {
                    case .success(var contents):
                        if contents.differsVisibly(from: session.contents) { visible = true }
                        // Progress this store has not saved yet stays on top of what the other instance wrote; an
                        // unsaved move is dropped (the next lookup of the file finds it again).
                        Self.reapply(session.pendingProgress, to: &contents)
                        session.contents = contents
                        session.loadedCiphertext = record.encryptedVaultBase64
                        session.dirty = !session.pendingProgress.isEmpty
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
        return visible
    }

    /// Puts unsaved progress back on a vault just loaded from the file. The resume point of what is being played here
    /// wins over the other instance's record of the same video (which usually changed for another reason, a like or a
    /// rating); the rest of that record stays as the other instance wrote it.
    private static func reapply(_ pending: [String: PendingProgress], to contents: inout VaultContents) {
        for progress in pending.values {
            let (key, existing, _) = contents.resolveOrCreate(progress.probe, now: progress.at)
            var updated = existing
            updated.lastPositionSeconds = progress.position
            if progress.duration > 0 { updated.durationSeconds = progress.duration }
            updated.updatedAt = max(existing.updatedAt, progress.at)
            contents.setVideo(updated, forKey: key)
        }
    }

    // MARK: - Remembered login

    struct RestoreCandidate: Sendable {
        let name: String
        let record: ProfileRecord
    }

    /// The remembered login to try now: this store has no user, the file names one, and this state of the file has not
    /// been tried yet (so a missing key costs one keychain query per change of the file, not one per call). Marks it
    /// tried. A profile without a vault is never a candidate: nothing could confirm the stored key is the right one, so
    /// any 32 bytes in the keychain would "unlock" it. Must be called with `lock` held.
    private func restoreCandidateLocked() -> RestoreCandidate? {
        guard state.session == nil, state.restoreAttemptedGeneration != state.syncGeneration,
            let name = state.effectiveRemembered, let record = state.profiles[name.lowercased()]
        else { return nil }
        state.restoreAttemptedGeneration = state.syncGeneration
        guard Self.hasVault(record) else { return nil }
        return RestoreCandidate(name: name, record: record)
    }

    /// Logs in with the remembered login: reads the key from the keychain and opens the vault with it, both outside
    /// the lock, then takes the session if nothing relevant changed meanwhile.
    private func restoreRememberedSession(_ candidate: RestoreCandidate) {
        guard let keyData = secretStore.data(for: Self.rememberAccount(for: candidate.name)),
            keyData.count == ProfileCrypto.keyLength
        else { return }
        let key = SymmetricKey(data: keyData)
        guard case .success(let contents) = Self.openVault(of: candidate.record, with: key) else { return }

        let restored: Bool = lock.withLock {
            guard state.session == nil, state.effectiveRemembered?.lowercased() == candidate.name.lowercased(),
                let current = state.profiles[candidate.name.lowercased()],
                current.encryptedVaultBase64 == candidate.record.encryptedVaultBase64
            else { return false }
            state.session = ProfileSession(
                usernameKey: current.username.lowercased(), username: current.username, vaultKey: key, contents: contents,
                loadedCiphertext: current.encryptedVaultBase64, remembered: true)
            return true
        }
        if restored { hub.yield(.external) }
    }

    static func hasVault(_ record: ProfileRecord) -> Bool {
        guard let encoded = record.encryptedVaultBase64 else { return false }
        return !encoded.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    static func openVault(of record: ProfileRecord, with key: SymmetricKey) -> Result<VaultContents, ProfileError> {
        guard hasVault(record), let encoded = record.encryptedVaultBase64 else {
            return .success(VaultContents())
        }
        guard let blob = Data(base64Encoded: encoded), let plaintext = ProfileCrypto.open(blob, key: key),
            let payload = try? ProfileJSON.decode(VaultPayload.self, from: plaintext)
        else { return .failure(.vaultUnreadable) }
        return .success(VaultContents(payload: payload))
    }

    // MARK: - Writing the file

    /// Encrypts the active vault if it changed (or holds unsaved progress or moves) and writes `profiles.json`. Needs
    /// `lock` and the file lock. Throws before touching anything if the existing file cannot be preserved; a damaged
    /// one is first set aside, an unreadable or newer-format one is never written over.
    func saveLocked(_ working: inout ProfileState, vaultChanged: Bool) throws(ProfileError) {
        switch working.fileProblem {
        case .unsupportedVersion(let version): throw .unsupportedFileVersion(version)
        case .unreadable: throw .fileUnreadable
        case .corrupt, nil: break
        }

        var sealed = false
        if var session = working.session, vaultChanged || session.dirty {
            guard let plaintext = try? ProfileJSON.encode(session.contents.payload(), pretty: false) else {
                throw .writeFailed(errno: EINVAL)
            }
            let encoded = try ProfileCrypto.seal(plaintext, key: session.vaultKey).base64EncodedString()
            working.profiles[session.usernameKey]?.encryptedVaultBase64 = encoded
            session.loadedCiphertext = encoded
            session.dirty = false
            session.pendingProgress = [:]
            working.session = session
            sealed = true
        }
        guard let data = try? ProfileJSON.encode(working.document(), pretty: true) else { throw .writeFailed(errno: EINVAL) }

        if working.fileProblem == .corrupt {
            try ProfileFileIO.preserveDamagedFile(at: fileURL, now: clock())
        } else if working.lastSnapshot == .content(data) {
            return
        }
        try ProfileFileIO.writeAtomically(data, to: fileURL)
        working.fileProblem = nil
        working.lastSnapshot = .content(data)
        working.lastSignature = ProfileFileIO.signature(of: fileURL)
        working.hasSynced = true
        if sealed { working.lastVaultSaveAttempt = clock() }
    }

    /// Runs one read-modify-write: take the file lock (waiting for it without holding the in-process lock, so readers
    /// are never held up by it), then the in-process lock, reload if the file moved on, apply `body` to a copy of the
    /// state and write it. The copy is committed only if the write succeeded, so a failure leaves memory as it was.
    /// With `requiresSession`, a logged-out store returns `fallback` without writing or locking. `body` and the write
    /// run under the in-process lock, so `body` must be memory-only: no keychain, no file reads.
    func transact<T>(
        requiresSession: Bool = true, fallback: @autoclosure () -> T,
        _ body: (inout ProfileState) throws(ProfileError) -> (T, ProfileEffect)
    ) throws(ProfileError) -> T {
        if requiresSession {
            syncAndRestore()
            if lock.withLock({ state.session == nil }) { return fallback() }
        }

        let fileLock = try ProfileFileLock.acquire(at: lockURL, timeout: lockTimeout)
        defer { fileLock.release() }
        lock.lock()
        defer { lock.unlock() }
        syncLocked()
        if requiresSession, state.session == nil { return fallback() }

        var working = state
        let (value, effect) = try body(&working)
        if effect.needsSave { try saveLocked(&working, vaultChanged: effect.contains(.vault)) }
        state = working
        if effect.needsSave, !effect.contains(.silent) { hub.yield(.local) }
        return value
    }

    /// Writes what is held in memory only (progress, followed moves). `minInterval` > 0 makes it a throttled save: it
    /// does nothing until that long after the last vault save. Returns whether a write happened.
    @discardableResult
    func persistDirty(minInterval: TimeInterval) throws(ProfileError) -> Bool {
        let due: Bool = lock.withLock {
            guard let session = state.session, session.dirty else { return false }
            guard minInterval > 0, let last = state.lastVaultSaveAttempt else { return true }
            return clock().timeIntervalSince(last) >= minInterval
        }
        guard due else { return false }

        var wrote = false
        do throws(ProfileError) {
            try transact(fallback: ()) { (state: inout ProfileState) throws(ProfileError) -> ((), ProfileEffect) in
                guard state.session?.dirty == true else { return ((), []) }
                wrote = true
                return ((), [.vault, .silent])
            }
        } catch {
            // Back off for the interval instead of retrying on every tick of a player.
            lock.withLock { state.lastVaultSaveAttempt = clock() }
            throw error
        }
        return wrote
    }

    // MARK: - Probes

    /// The lookup key for a file, fingerprinted as `mode` says (disk I/O for `.compute`, outside every lock) and
    /// prepared for resolving against the current vault.
    func lookupProbe(for url: URL, mode: FingerprintMode = .compute) -> VideoProbe? {
        guard let probe = VideoProbe(url, mode: mode) else { return nil }
        return prepared([probe]).first
    }

    /// Fingerprints, outside every lock, the recorded paths of the records these probes would take over from another
    /// location, so that resolving them under the lock needs no file access.
    func prepared(_ probes: [VideoProbe]) -> [VideoProbe] {
        guard probes.contains(where: { $0.fingerprint != nil }) else { return probes }
        syncAndRestore()
        guard let snapshot = lock.withLock({ state.session?.contents }) else { return probes }
        return probes.map { probe in
            guard let recorded = snapshot.takeoverPath(for: probe) else { return probe }
            let fingerprint = probe.mode == .compute
                ? VideoFingerprint.fingerprint(ofNormalizedPath: recorded)
                : VideoFingerprint.cachedFingerprint(ofNormalizedPath: recorded)
            guard let fingerprint else { return probe }
            return probe.withRecordedFingerprint(fingerprint, forPath: recorded)
        }
    }

    /// The unlocked profile's vault after following any moved or renamed files among `probes`, or `nil` while logged
    /// out. Reads never write: a move found here is applied in memory (so the result reflects it) and saved with the
    /// next write or `persistPendingMigrations()`. The value is a snapshot; work on it outside the lock.
    func readMigrating(_ probes: [VideoProbe]) -> VaultContents? {
        syncAndRestore()
        lock.lock()
        defer { lock.unlock() }
        guard state.session != nil else { return nil }
        if !probes.isEmpty {
            let now = clock()
            var migrated = false
            for probe in probes where state.session?.contents.resolve(probe, now: now)?.migrated == true { migrated = true }
            if migrated { state.session?.dirty = true }
        }
        return state.session?.contents
    }
}
