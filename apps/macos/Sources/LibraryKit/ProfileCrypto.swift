import CommonCrypto
import CryptoKit
import Foundation

/// Password hashing and the vault cipher, in the exact formats the Windows app uses.
///
/// One PBKDF2-HMAC-SHA256 run turns the password into 64 bytes: the first 32 are the password check stored in
/// `profiles.json`, the last 32 are the vault key and are never stored (except in the `SecretStore` for "remember me").
/// The vault is AES-256-GCM with a random 12-byte nonce per save, no associated data, laid out as `nonce | tag | ciphertext`.
enum ProfileCrypto {
    static let saltLength = 16
    static let hashLength = 32
    static let keyLength = 32
    static let defaultIterations = 100_000
    /// A damaged or hostile file must not be able to make a login spin for minutes.
    static let maxIterations = 10_000_000
    static let nonceLength = 12
    static let tagLength = 16

    struct Derived {
        let passwordHash: Data
        let vaultKey: SymmetricKey
    }

    /// PBKDF2-HMAC-SHA256 over the UTF-8 password (no trimming). `iterations <= 0` falls back to 100 000.
    static func derive(password: String, salt: Data, iterations: Int) throws(ProfileError) -> Derived {
        let rounds = iterations > 0 ? iterations : defaultIterations
        guard rounds <= maxIterations, !salt.isEmpty else { throw .cryptoFailure }
        let passwordBytes = Array(password.utf8)
        let saltBytes = [UInt8](salt)
        var derived = [UInt8](repeating: 0, count: hashLength + keyLength)
        defer { derived.withUnsafeMutableBytes { _ = memset_s($0.baseAddress, $0.count, 0, $0.count) } }
        let status = passwordBytes.withUnsafeBufferPointer { password in
            password.withMemoryRebound(to: CChar.self) { password in
                CCKeyDerivationPBKDF(
                    CCPBKDFAlgorithm(kCCPBKDF2), password.baseAddress, password.count, saltBytes, saltBytes.count,
                    CCPseudoRandomAlgorithm(kCCPRFHmacAlgSHA256), UInt32(rounds), &derived, derived.count)
            }
        }
        guard status == Int32(kCCSuccess) else { throw .cryptoFailure }
        return Derived(passwordHash: Data(derived[0..<hashLength]), vaultKey: SymmetricKey(data: derived[hashLength...]))
    }

    static func randomBytes(_ count: Int) -> Data {
        var generator = SystemRandomNumberGenerator()
        return Data((0..<count).map { _ in UInt8.random(in: .min ... .max, using: &generator) })
    }

    /// Compares without returning early on the first difference.
    static func constantTimeEqual(_ a: Data, _ b: Data) -> Bool {
        guard a.count == b.count else { return false }
        var difference: UInt8 = 0
        for (x, y) in zip(a, b) { difference |= x ^ y }
        return difference == 0
    }

    /// `nonce | tag | ciphertext` under a fresh random nonce.
    static func seal(_ plaintext: Data, key: SymmetricKey) throws(ProfileError) -> Data {
        guard let box = try? AES.GCM.seal(plaintext, using: key, nonce: AES.GCM.Nonce()) else { throw .cryptoFailure }
        return Data(box.nonce) + box.tag + box.ciphertext
    }

    /// The inverse of `seal`; `nil` for a wrong key, a damaged blob or a blob that is too short.
    static func open(_ blob: Data, key: SymmetricKey) -> Data? {
        guard blob.count >= nonceLength + tagLength else { return nil }
        let bytes = [UInt8](blob)
        let nonce = bytes[0..<nonceLength]
        let tag = bytes[nonceLength..<(nonceLength + tagLength)]
        let ciphertext = bytes[(nonceLength + tagLength)...]
        guard let box = try? AES.GCM.SealedBox(nonce: AES.GCM.Nonce(data: nonce), ciphertext: ciphertext, tag: tag) else { return nil }
        return try? AES.GCM.open(box, using: key)
    }
}

extension SymmetricKey {
    var rawData: Data { withUnsafeBytes { Data($0) } }
}
