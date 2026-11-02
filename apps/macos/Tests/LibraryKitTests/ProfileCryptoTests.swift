import CryptoKit
import Foundation
import Testing
@testable import LibraryKit

@Suite struct ProfileCryptoTests {
    private func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }

    @Test func pbkdf2MatchesPublishedHmacSha256Vectors() throws {
        // Both from the PBKDF2-HMAC-SHA256 test vectors (RFC 7914 style): 32-byte outputs; the first half of our
        // 64-byte derivation is exactly that output.
        let one = try ProfileCrypto.derive(password: "password", salt: Data("salt".utf8), iterations: 1)
        #expect(hex(one.passwordHash) == "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b")
        let many = try ProfileCrypto.derive(password: "password", salt: Data("salt".utf8), iterations: 4096)
        #expect(hex(many.passwordHash) == "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a")
        #expect(many.vaultKey.rawData.count == 32)
        #expect(many.vaultKey.rawData != many.passwordHash)
    }

    @Test func nonPositiveIterationsFallBackToTheDefault() throws {
        let salt = Data(repeating: 7, count: 16)
        let fallback = try ProfileCrypto.derive(password: "pw", salt: salt, iterations: 0)
        let explicit = try ProfileCrypto.derive(password: "pw", salt: salt, iterations: 100_000)
        #expect(fallback.passwordHash == explicit.passwordHash)
        #expect(fallback.vaultKey.rawData == explicit.vaultKey.rawData)
    }

    @Test func passwordIsUsedAsTypedWithoutTrimming() throws {
        let salt = Data(repeating: 1, count: 16)
        let plain = try ProfileCrypto.derive(password: "pw", salt: salt, iterations: 10)
        let padded = try ProfileCrypto.derive(password: " pw ", salt: salt, iterations: 10)
        #expect(plain.passwordHash != padded.passwordHash)
    }

    @Test func absurdIterationCountsAreRefused() {
        #expect(throws: ProfileError.cryptoFailure) {
            try ProfileCrypto.derive(password: "pw", salt: Data(repeating: 1, count: 16), iterations: Int.max)
        }
    }

    @Test func sealedVaultUsesTheWindowsLayoutNonceTagCiphertext() throws {
        let key = SymmetricKey(size: .bits256)
        let plaintext = Data("{\"videos\":[]}".utf8)
        let blob = try ProfileCrypto.seal(plaintext, key: key)

        #expect(blob.count == 12 + 16 + plaintext.count)
        // Parse it by hand the way the Windows app does: nonce | tag | ciphertext.
        let box = try AES.GCM.SealedBox(
            nonce: AES.GCM.Nonce(data: blob.prefix(12)), ciphertext: blob.dropFirst(28), tag: blob.dropFirst(12).prefix(16))
        #expect(try AES.GCM.open(box, using: key) == plaintext)
        #expect(ProfileCrypto.open(blob, key: key) == plaintext)
    }

    @Test func aVaultBuiltByHandInTheWindowsLayoutOpens() throws {
        let key = SymmetricKey(size: .bits256)
        let plaintext = Data("{\"videos\":[],\"customLists\":[],\"thumbnails\":{}}".utf8)
        let nonce = AES.GCM.Nonce()
        let box = try AES.GCM.seal(plaintext, using: key, nonce: nonce)
        let handBuilt = Data(nonce) + box.tag + box.ciphertext // not CryptoKit's `combined`, which is nonce | ciphertext | tag
        #expect(ProfileCrypto.open(handBuilt, key: key) == plaintext)
        #expect(ProfileCrypto.open(box.combined!, key: key) == nil)
    }

    @Test func eachSaveUsesAFreshNonce() throws {
        let key = SymmetricKey(size: .bits256)
        let a = try ProfileCrypto.seal(Data("x".utf8), key: key)
        let b = try ProfileCrypto.seal(Data("x".utf8), key: key)
        #expect(a.prefix(12) != b.prefix(12))
    }

    @Test func wrongKeyTamperingAndShortBlobsAreRejected() throws {
        let key = SymmetricKey(size: .bits256)
        var blob = try ProfileCrypto.seal(Data("secret".utf8), key: key)
        #expect(ProfileCrypto.open(blob, key: SymmetricKey(size: .bits256)) == nil)
        blob[blob.count - 1] ^= 0x01
        #expect(ProfileCrypto.open(blob, key: key) == nil)
        #expect(ProfileCrypto.open(Data(repeating: 0, count: 27), key: key) == nil)
        #expect(ProfileCrypto.open(Data(), key: key) == nil)
    }

    @Test func constantTimeEqualComparesContentAndLength() {
        #expect(ProfileCrypto.constantTimeEqual(Data([1, 2, 3]), Data([1, 2, 3])))
        #expect(!ProfileCrypto.constantTimeEqual(Data([1, 2, 3]), Data([1, 2, 4])))
        #expect(!ProfileCrypto.constantTimeEqual(Data([1, 2, 3]), Data([1, 2])))
    }
}
