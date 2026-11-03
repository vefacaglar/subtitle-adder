import CryptoKit
import Foundation

/// How much picture data one profile may keep on disk.
public struct ThumbnailCacheLimits: Sendable, Equatable {
    public var maxFiles: Int
    public var maxBytes: Int

    public init(maxFiles: Int = 1500, maxBytes: Int = 150 * 1024 * 1024) {
        self.maxFiles = max(1, maxFiles)
        self.maxBytes = max(1, maxBytes)
    }
}

/// The card pictures of the profiles, kept next to `profiles.json` instead of inside the vault: one encrypted file per
/// picture in a folder per profile, so storing a picture never rewrites (or re-encrypts) the vault, and browsing videos
/// never creates records.
///
/// Layout: `<root>/<hash of the username>/<hash of the picture key>.bin`. Neither the user's name nor a video's path
/// appears in a file name. Each file is AES-256-GCM under the profile's vault key, in the vault's own
/// `nonce | tag | ciphertext` layout, and is written atomically. A file that does not open (another profile's key, a
/// damaged file) is removed and counts as missing.
///
/// The folder of a profile is capped (`ThumbnailCacheLimits`). A write that goes past a cap evicts the files that were
/// used least recently, judged by their modification date, down to 90 % of the cap; reading a file that has not been
/// touched for an hour renews its date. Safe to call from any thread; other processes may share the folder (counts are
/// re-read from the disk when an eviction runs).
final class ThumbnailCache: @unchecked Sendable {
    let root: URL
    let limits: ThumbnailCacheLimits
    private let lock = NSLock()
    private var usage: [String: Usage] = [:]
    private let renewAfter: TimeInterval = 3600

    private struct Usage {
        var files: Int
        var bytes: Int
    }

    init(root: URL, limits: ThumbnailCacheLimits = ThumbnailCacheLimits()) {
        self.root = root
        self.limits = limits
    }

    /// The folder name of a profile: a hash, so the user's name is not on the disk in the clear.
    static func folderName(forProfile usernameKey: String) -> String {
        hex(SHA256.hash(data: Data(usernameKey.lowercased().utf8)).prefix(16))
    }

    static func fileName(forKey key: String) -> String {
        hex(SHA256.hash(data: Data(key.lowercased().utf8)).prefix(16)) + ".bin"
    }

    private static func hex<S: Sequence>(_ bytes: S) -> String where S.Element == UInt8 {
        bytes.map { String(format: "%02x", $0) }.joined()
    }

    private func folder(forProfile usernameKey: String) -> URL {
        root.appending(path: Self.folderName(forProfile: usernameKey), directoryHint: .isDirectory)
    }

    // MARK: Reading

    /// The picture stored under the first of `keys` that has one.
    func data(profile: String, keys: [String], vaultKey: SymmetricKey) -> Data? {
        let folder = folder(forProfile: profile)
        for key in keys {
            let file = folder.appending(path: Self.fileName(forKey: key), directoryHint: .notDirectory)
            guard let blob = try? Data(contentsOf: file) else { continue }
            guard let plain = ProfileCrypto.open(blob, key: vaultKey) else {
                lock.withLock { discard(file, folderName: folder.lastPathComponent) }
                continue
            }
            renewIfOld(file)
            return plain
        }
        return nil
    }

    private func renewIfOld(_ file: URL) {
        guard let date = (try? file.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate,
            Date().timeIntervalSince(date) > renewAfter
        else { return }
        try? FileManager.default.setAttributes([.modificationDate: Date()], ofItemAtPath: file.path)
    }

    // MARK: Writing

    /// Stores a picture. Returns whether it is on disk now.
    @discardableResult
    func store(_ data: Data, profile: String, key: String, vaultKey: SymmetricKey) -> Bool {
        guard let blob = try? ProfileCrypto.seal(data, key: vaultKey) else { return false }
        let folder = folder(forProfile: profile)
        let name = folder.lastPathComponent
        let file = folder.appending(path: Self.fileName(forKey: key), directoryHint: .notDirectory)
        return lock.withLock {
            guard ensureFolder(folder) else { return false }
            ensureUsage(folder: folder, name: name)
            let previous = fileSize(file)
            do { try blob.write(to: file, options: .atomic) } catch { return false }
            var current = usage[name] ?? Usage(files: 0, bytes: 0)
            if previous == nil { current.files += 1 }
            current.bytes += blob.count - (previous ?? 0)
            usage[name] = current
            if current.files > limits.maxFiles || current.bytes > limits.maxBytes {
                evict(folder: folder, name: name, keeping: file.lastPathComponent)
            }
            return true
        }
    }

    // MARK: Removing

    func remove(profile: String, keys: [String]) {
        let folder = folder(forProfile: profile)
        lock.withLock {
            for key in keys {
                discard(folder.appending(path: Self.fileName(forKey: key), directoryHint: .notDirectory), folderName: folder.lastPathComponent)
            }
        }
    }

    /// Deletes the whole folder of a profile (the profile is gone).
    func removeAll(profile: String) {
        let folder = folder(forProfile: profile)
        lock.withLock {
            try? FileManager.default.removeItem(at: folder)
            usage[folder.lastPathComponent] = nil
        }
    }

    // MARK: Bookkeeping (call with `lock` held)

    private func ensureFolder(_ folder: URL) -> Bool {
        do {
            try FileManager.default.createDirectory(
                at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            return true
        } catch {
            return false
        }
    }

    private func fileSize(_ file: URL) -> Int? {
        (try? file.resourceValues(forKeys: [.fileSizeKey]))?.fileSize
    }

    private func discard(_ file: URL, folderName: String) {
        guard let size = fileSize(file) else { return }
        guard (try? FileManager.default.removeItem(at: file)) != nil else { return }
        if var current = usage[folderName] {
            current.files = max(0, current.files - 1)
            current.bytes = max(0, current.bytes - size)
            usage[folderName] = current
        }
    }

    private struct Entry {
        let url: URL
        let date: Date
        let size: Int
    }

    private func entries(in folder: URL) -> [Entry] {
        let keys: [URLResourceKey] = [.contentModificationDateKey, .fileSizeKey]
        let urls = (try? FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: keys)) ?? []
        var found: [Entry] = []
        for url in urls {
            let values = try? url.resourceValues(forKeys: Set(keys))
            let date = values?.contentModificationDate ?? .distantPast
            if url.pathExtension != "bin" {
                // A temporary file of a write that never finished.
                if Date().timeIntervalSince(date) > 3600 { try? FileManager.default.removeItem(at: url) }
                continue
            }
            found.append(Entry(url: url, date: date, size: values?.fileSize ?? 0))
        }
        return found
    }

    /// Counts the folder the first time this instance writes to it.
    private func ensureUsage(folder: URL, name: String) {
        guard usage[name] == nil else { return }
        let all = entries(in: folder)
        usage[name] = Usage(files: all.count, bytes: all.reduce(0) { $0 + $1.size })
    }

    /// Removes the least recently used files until the folder is at 90 % of its caps. `keeping` (the file just
    /// written) survives even when its date ties with older ones.
    private func evict(folder: URL, name: String, keeping: String) {
        let all = entries(in: folder).sorted { ($0.date, $0.url.lastPathComponent) < ($1.date, $1.url.lastPathComponent) }
        var files = all.count
        var bytes = all.reduce(0) { $0 + $1.size }
        let fileTarget = limits.maxFiles * 9 / 10
        let byteTarget = limits.maxBytes / 10 * 9
        for entry in all where files > fileTarget || bytes > byteTarget {
            if entry.url.lastPathComponent == keeping { continue }
            guard (try? FileManager.default.removeItem(at: entry.url)) != nil else { continue }
            files -= 1
            bytes -= entry.size
        }
        usage[name] = Usage(files: files, bytes: bytes)
    }

    /// How many pictures the folder of a profile holds on disk (tests).
    func fileCount(profile: String) -> Int {
        lock.withLock { entries(in: folder(forProfile: profile)).count }
    }
}
