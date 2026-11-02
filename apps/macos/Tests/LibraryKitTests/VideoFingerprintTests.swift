import CryptoKit
import Foundation
import Testing
@testable import LibraryKit

@Suite final class VideoFingerprintTests {
    let folder = TestFolder()

    private func write(_ name: String, count: Int, seed: UInt8 = 1) throws -> URL {
        let url = folder.video(name)
        var bytes = [UInt8](repeating: 0, count: count)
        var value = UInt32(seed)
        for index in bytes.indices {
            value = value &* 1_664_525 &+ 1_013_904_223
            bytes[index] = UInt8(truncatingIfNeeded: value >> 16)
        }
        try Data(bytes).write(to: url)
        return url
    }

    /// The algorithm written out independently of the implementation: SHA-256 of the little-endian length and the three slices.
    private func expectedFingerprint(of data: Data) -> String {
        let length = data.count
        var hasher = SHA256()
        withUnsafeBytes(of: Int64(length).littleEndian) { hasher.update(bufferPointer: $0) }
        for offset in [0, max(0, length / 2 - 32_768), max(0, length - 65_536)] where offset < length {
            hasher.update(data: data[offset..<min(offset + 65_536, length)])
        }
        return "fp:\(length):" + hasher.finalize().prefix(12).map { String(format: "%02x", $0) }.joined()
    }

    @Test func fingerprintOf150000BytesStartsWithItsLengthAndSurvivesAMove() throws {
        let original = try write("a.mp4", count: 150_000)
        let id = VideoFingerprint.videoID(for: original)
        #expect(id.hasPrefix("fp:150000:"))
        #expect(id.count == "fp:150000:".count + 24)

        let moved = folder.video("sub/b.mkv")
        try FileManager.default.createDirectory(at: moved.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.moveItem(at: original, to: moved)
        #expect(VideoFingerprint.videoID(for: moved) == id)
        // The old location no longer resolves to a fingerprint, only to its path.
        #expect(VideoFingerprint.videoID(for: original) == normalized(original))
    }

    @Test func matchesTheDocumentedAlgorithmForLargeAndSmallFiles() throws {
        for count in [1, 100, 65_535, 65_536, 65_537, 150_000, 300_001] {
            let url = try write("f\(count).bin", count: count, seed: UInt8(count % 200))
            let data = try Data(contentsOf: url)
            #expect(VideoFingerprint.videoID(for: url) == expectedFingerprint(of: data), "length \(count)")
        }
    }

    @Test func contentChangeInASliceChangesTheFingerprintAndTheCacheNoticesTheEdit() throws {
        let url = try write("edit.bin", count: 150_000)
        let before = VideoFingerprint.videoID(for: url)
        var data = try Data(contentsOf: url)
        data[10] ^= 0xFF
        try data.write(to: url)
        // Same size, newer modification time: the cached value must not be reused.
        let future = Date().addingTimeInterval(5)
        try FileManager.default.setAttributes([.modificationDate: future], ofItemAtPath: url.path)
        let after = VideoFingerprint.videoID(for: url)
        #expect(before != after)
        #expect(after.hasPrefix("fp:150000:"))
    }

    @Test func emptyMissingAndNonRegularFilesFallBackToTheNormalizedPath() throws {
        let empty = folder.video("empty.mp4")
        try Data().write(to: empty)
        #expect(VideoFingerprint.videoID(for: empty) == normalized(empty))
        let missing = folder.video("missing.mp4")
        #expect(VideoFingerprint.videoID(for: missing) == normalized(missing))
        #expect(VideoFingerprint.fingerprint(ofNormalizedPath: folder.url.path) == nil)
        #expect(VideoFingerprint.videoID(for: URL(string: "https://example.com/a.mp4")!) == "")
    }

    @Test func normalizedPathTrimsQuotesResolvesDotsAndDropsTrailingSlash() {
        #expect(VideoFingerprint.normalizedPath("  \"/tmp/a/../b/c.mp4\"  ") == "/tmp/b/c.mp4")
        #expect(VideoFingerprint.normalizedPath("/tmp/dir/") == "/tmp/dir")
        #expect(VideoFingerprint.normalizedPath("   ") == "")
        #expect(VideoFingerprint.normalizedPath("") == "")
    }
}
