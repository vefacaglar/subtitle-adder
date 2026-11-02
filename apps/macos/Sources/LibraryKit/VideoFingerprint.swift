import CryptoKit
import Foundation

/// Identifies a video by content rather than by where it lives, so moving or renaming a file keeps its history.
///
/// The id is `fp:<length>:<24 hex>`: SHA-256 over the file length (8 bytes, little endian) and up to three 64 KB
/// slices (at offset 0, around the middle and at the end), cut to 24 hex digits. The algorithm is byte-for-byte the
/// one in the Windows app, so both apps compute the same id for the same file. Only an existing, non-empty,
/// readable regular file has one; anything else is identified by its normalized path.
public enum VideoFingerprint {
    static let sliceBytes = 65_536

    /// A trimmed, absolute, standardized path without a trailing slash (`""` for blank input). Relative paths resolve
    /// against the working directory. Compare results case-insensitively.
    public static func normalizedPath(_ path: String) -> String {
        let trimmed = path.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: "\""))
        guard !trimmed.isEmpty else { return "" }
        return URL(fileURLWithPath: trimmed).standardizedFileURL.path
    }

    /// Like `normalizedPath(_:)` for a file URL (`""` for any other kind of URL).
    public static func normalizedPath(_ url: URL) -> String {
        guard url.isFileURL else { return "" }
        let path = url.standardizedFileURL.path
        return path.isEmpty ? "" : path
    }

    /// The fingerprint if the file has one, otherwise the normalized path (`""` for a blank or non-file URL).
    public static func videoID(for url: URL) -> String {
        let path = normalizedPath(url)
        guard !path.isEmpty else { return "" }
        return fingerprint(ofNormalizedPath: path) ?? path
    }

    /// `fp:<length>:<24 hex>` for an existing, non-empty, readable file, else `nil`. Cached by path, size and
    /// modification time, so asking again for an unchanged file costs one `stat`.
    public static func fingerprint(ofNormalizedPath path: String) -> String? {
        guard !path.isEmpty else { return nil }
        var info = stat()
        guard stat(path, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG, info.st_size > 0 else { return nil }
        let length = Int64(info.st_size)
        let signature = CachedFingerprint.Signature(
            length: length, seconds: Int64(info.st_mtimespec.tv_sec), nanoseconds: Int64(info.st_mtimespec.tv_nsec))
        if let cached = cache.value(for: path, matching: signature) { return cached }
        guard let computed = compute(path: path, length: length) else { return nil }
        cache.store(computed, for: path, signature: signature)
        return computed
    }

    private static func compute(path: String, length: Int64) -> String? {
        let fd = open(path, O_RDONLY | O_CLOEXEC)
        guard fd >= 0 else { return nil }
        defer { close(fd) }

        var hasher = SHA256()
        withUnsafeBytes(of: length.littleEndian) { hasher.update(bufferPointer: $0) }

        let slice = Int64(sliceBytes)
        let offsets: [Int64] = [0, max(0, length / 2 - slice / 2), max(0, length - slice)]
        var buffer = [UInt8](repeating: 0, count: sliceBytes)
        for offset in offsets where offset < length {
            let wanted = Int(min(slice, length - offset))
            var total = 0
            while total < wanted {
                let read = buffer.withUnsafeMutableBytes { raw in
                    pread(fd, raw.baseAddress! + total, wanted - total, off_t(offset) + off_t(total))
                }
                if read < 0 {
                    if errno == EINTR { continue }
                    return nil
                }
                if read == 0 { break }
                total += read
            }
            if total > 0 {
                buffer.withUnsafeBytes { hasher.update(bufferPointer: UnsafeRawBufferPointer(rebasing: $0[0..<total])) }
            }
        }

        let hex = hasher.finalize().prefix(12).map { String(format: "%02x", $0) }.joined()
        return "fp:\(length):\(hex)"
    }

    private static let cache = FingerprintCache()
}

/// Process-wide memo of fingerprints, shared by every store so a library of thousands of files is read once.
private final class FingerprintCache: @unchecked Sendable {
    private let lock = NSLock()
    private var entries: [String: CachedFingerprint] = [:]

    func value(for path: String, matching signature: CachedFingerprint.Signature) -> String? {
        lock.withLock {
            guard let entry = entries[path.lowercased()], entry.signature == signature else { return nil }
            return entry.fingerprint
        }
    }

    func store(_ fingerprint: String, for path: String, signature: CachedFingerprint.Signature) {
        lock.withLock { entries[path.lowercased()] = CachedFingerprint(signature: signature, fingerprint: fingerprint) }
    }
}

private struct CachedFingerprint {
    struct Signature: Equatable {
        let length: Int64
        let seconds: Int64
        let nanoseconds: Int64
    }

    let signature: Signature
    let fingerprint: String
}
