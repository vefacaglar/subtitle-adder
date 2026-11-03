import CryptoKit
import Foundation

/// One entry of `profiles.json`: the password check and, until it is opened, the vault ciphertext.
struct ProfileRecord: Sendable {
    var username: String
    var saltBase64: String
    var passwordHashBase64: String
    var iterations: Int
    var createdAt: Date
    var encryptedVaultBase64: String?
}

/// The one unlocked profile of a store: its vault key and decrypted contents.
struct ProfileSession: Sendable {
    /// Lowercased username, the key into `ProfileState.profiles`.
    var usernameKey: String
    var username: String
    var vaultKey: SymmetricKey
    var contents: VaultContents
    /// The ciphertext `contents` was loaded from or last saved as; a different one on disk means another instance wrote.
    var loadedCiphertext: String?
    /// The session stands for the shared "remember me" login (restored from it, or created with remember-me on), so
    /// logging out elsewhere ends it here too. A session started without remember-me never follows the file.
    var remembered: Bool

    /// `contents` holds changes the file does not: playback progress and path migrations found while reading. They are
    /// written with the next save of the vault (any save writes all of `contents`), or by an explicit flush. Reads never
    /// write, and an unflushed change is lost if the process ends first.
    var dirty = false
    /// Part of what `dirty` stands for is a path migration found while reading (not just playback progress), so the
    /// vault has something worth writing even if no video is playing.
    var migrated = false
    /// The latest unsaved progress per record key. `contents` already reflects it; this is what an external reload of
    /// the vault re-applies on top of what the other instance wrote.
    var pendingProgress: [String: PendingProgress] = [:]
}

/// Playback progress that is in memory but not yet in the file.
struct PendingProgress: Sendable {
    let probe: VideoProbe
    var position: Double
    var duration: Double
    var at: Date
}

/// Why `profiles.json` cannot simply be overwritten.
enum FileProblem: Equatable, Sendable {
    /// Not valid JSON of the expected shape; a copy is set aside before the next write.
    case corrupt
    /// Exists but could not be read (permissions, I/O error). It is never set aside or replaced: writes fail with
    /// `.fileUnreadable` and every sync reads the file again.
    case unreadable
    case unsupportedVersion(Int)
}

struct ProfileState: Sendable {
    var profiles: [String: ProfileRecord] = [:]
    var rememberMe = true
    var lastUsername: String?
    var rememberedUsername: String?
    var session: ProfileSession?
    /// Bumped whenever a session starts or ends (a login, a logout, a deleted profile, a restored login), so a reader
    /// can tell that what it read belongs to a session that is over.
    var sessionGeneration = 0

    var hasSynced = false
    var lastSnapshot: ProfileFileSnapshot?
    var lastSignature: FileSignature?
    var fileProblem: FileProblem?
    /// Bumped whenever a different state of the file was applied; a remembered login is tried once per generation.
    var syncGeneration = 0
    var restoreAttemptedGeneration = -1
    /// When a save of the vault last succeeded or, for a throttled progress save, last failed; throttles progress writes.
    var lastVaultSaveAttempt: Date?

    mutating func startSession(_ new: ProfileSession) {
        session = new
        sessionGeneration += 1
    }

    mutating func endSession() {
        guard session != nil else { return }
        session = nil
        sessionGeneration += 1
    }

    /// The remembered username as the apps treat it: only while remember-me is on.
    var effectiveRemembered: String? { rememberMe ? rememberedUsername : nil }

    func document() -> ProfilesDocument {
        ProfilesDocument(
            rememberMe: rememberMe, lastUsername: lastUsername, rememberedUsername: effectiveRemembered,
            profiles: profiles.values
                .sorted { $0.username.lowercased() < $1.username.lowercased() }
                .map {
                    ProfileDTO(
                        username: $0.username, saltBase64: $0.saltBase64, passwordHashBase64: $0.passwordHashBase64,
                        iterations: $0.iterations, createdAtUtc: $0.createdAt, encryptedVaultBase64: $0.encryptedVaultBase64)
                })
    }

    /// Replaces the profile table and the header fields with those of a parsed file (the session is reconciled separately).
    mutating func load(_ document: ProfilesDocument) {
        rememberMe = document.rememberMe
        if let last = document.lastUsername?.trimmingCharacters(in: .whitespacesAndNewlines), !last.isEmpty { lastUsername = last }
        let remembered = document.rememberedUsername?.trimmingCharacters(in: .whitespacesAndNewlines)
        rememberedUsername = (remembered?.isEmpty ?? true) ? nil : remembered

        var table: [String: ProfileRecord] = [:]
        for dto in document.profiles {
            let name = dto.username.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !name.isEmpty,
                !dto.saltBase64.trimmingCharacters(in: .whitespaces).isEmpty,
                !dto.passwordHashBase64.trimmingCharacters(in: .whitespaces).isEmpty
            else { continue }
            table[name.lowercased()] = ProfileRecord(
                username: name, saltBase64: dto.saltBase64, passwordHashBase64: dto.passwordHashBase64,
                iterations: dto.iterations > 0 ? dto.iterations : ProfileCrypto.defaultIterations,
                createdAt: dto.createdAtUtc, encryptedVaultBase64: dto.encryptedVaultBase64)
        }
        profiles = table
    }
}

/// What a mutation did, so the store knows what to write and whether to tell the UI.
struct ProfileEffect: OptionSet {
    let rawValue: Int
    /// The active vault changed: re-encrypt it and save.
    static let vault = ProfileEffect(rawValue: 1)
    /// Only `profiles.json` itself changed (profile list, remember-me fields): save without touching the vault.
    static let header = ProfileEffect(rawValue: 2)
    /// Do not raise `ProfileChange.local` (playback progress, thumbnails, migrations found while reading).
    static let silent = ProfileEffect(rawValue: 4)

    var needsSave: Bool { !isDisjoint(with: [.vault, .header]) }
}

/// Fan-out of `ProfileChange` to any number of `AsyncStream`s.
final class ProfileChangeHub: @unchecked Sendable {
    private let lock = NSLock()
    private var continuations: [UUID: AsyncStream<ProfileChange>.Continuation] = [:]

    func stream() -> AsyncStream<ProfileChange> {
        AsyncStream<ProfileChange>(bufferingPolicy: .unbounded) { continuation in
            let id = UUID()
            lock.withLock { continuations[id] = continuation }
            continuation.onTermination = { [weak self] _ in
                self?.lock.withLock { self?.continuations[id] = nil }
            }
        }
    }

    func yield(_ change: ProfileChange) {
        let targets = lock.withLock { Array(continuations.values) }
        for continuation in targets { continuation.yield(change) }
    }

    func finishAll() {
        let targets = lock.withLock {
            defer { continuations = [:] }
            return Array(continuations.values)
        }
        for continuation in targets { continuation.finish() }
    }
}
