import CryptoKit
import Foundation
import Testing
@testable import LibraryKit

/// `profiles.json` and the vault inside it must stay readable by the Windows app, and the remembered login must
/// never end up in the file.
@Suite final class ProfileFileFormatTests {
    let folder = TestFolder()

    @Test func profilesJsonUsesTheSharedShape() throws {
        let store = folder.makeStore()
        try store.register(username: "Zed", password: "pw-zed", rememberMe: false)
        try store.logout()
        try store.register(username: "alice", password: "pw-alice", rememberMe: false)

        let root = try folder.profilesObject()
        #expect(root["version"] as? Int == 2)
        #expect(root["rememberMe"] as? Bool == false)
        #expect(root["lastUsername"] as? String == "alice")
        #expect(root["rememberedUsername"] == nil)
        #expect(!(try folder.rawProfiles()).contains("null"))

        let profiles = try #require(root["profiles"] as? [[String: Any]])
        #expect(profiles.compactMap { $0["username"] as? String } == ["alice", "Zed"])
        let alice = profiles[0]
        #expect(Data(base64Encoded: try #require(alice["saltBase64"] as? String))?.count == 16)
        #expect(Data(base64Encoded: try #require(alice["passwordHashBase64"] as? String))?.count == 32)
        #expect(alice["iterations"] as? Int == 100_000)
        #expect(ProfileDateCoding.date(from: try #require(alice["createdAtUtc"] as? String)) != nil)
        #expect(alice["encryptedVaultBase64"] as? String != nil)
        #expect(!(try folder.rawProfiles()).contains("\\/"), "slashes must not be escaped")
    }

    @Test func vaultIsAesGcmWithNonceTagCiphertextAndTheDocumentedPlaintext() throws {
        let video = folder.video("a.mp4")
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        try store.recordWatch(video, position: 10, duration: 100)
        let list = try #require(try store.createList(named: "Liste"))
        try store.addVideo(video, toList: list.id)
        try store.setThumbnail(Data("IMG".utf8), for: video)

        // Open the vault the way another implementation would: derive, split nonce | tag | ciphertext, decrypt.
        let profile = try #require((try folder.profilesObject()["profiles"] as? [[String: Any]])?.first)
        let salt = try #require(Data(base64Encoded: profile["saltBase64"] as! String))
        let derived = try ProfileCrypto.derive(password: "pw", salt: salt, iterations: 100_000)
        #expect(derived.passwordHash == Data(base64Encoded: profile["passwordHashBase64"] as! String))
        let blob = try #require(Data(base64Encoded: profile["encryptedVaultBase64"] as! String))
        let box = try AES.GCM.SealedBox(
            nonce: AES.GCM.Nonce(data: blob.prefix(12)), ciphertext: blob.dropFirst(28), tag: blob.dropFirst(12).prefix(16))
        let plaintext = try AES.GCM.open(box, using: derived.vaultKey)

        let vault = try #require(JSONSerialization.jsonObject(with: plaintext) as? [String: Any])
        let videos = try #require(vault["videos"] as? [[String: Any]])
        let record = try #require(videos.first)
        for key in ["videoPath", "fileName", "directoryPath", "viewCount", "isLiked", "rating", "lastPositionSeconds",
                    "durationSeconds", "lastWatchedAtUtc", "updatedAtUtc"] {
            #expect(record[key] != nil, "missing \(key)")
        }
        #expect(record["contentFingerprint"] == nil, "no fingerprint for a file that does not exist")
        #expect(record["thumbnail"] == nil)
        #expect(record["viewCount"] as? Int == 1)

        let lists = try #require(vault["customLists"] as? [[String: Any]])
        let id = try #require(lists.first?["id"] as? String)
        #expect(id == list.id)
        #expect(id.count == 32 && id.allSatisfy { "0123456789abcdef".contains($0) })
        #expect(lists.first?["videoPaths"] as? [String] == [normalized(video)])

        // Pictures live in the thumbnail cache, not in the vault.
        let thumbnails = try #require(vault["thumbnails"] as? [String: String])
        #expect(thumbnails.isEmpty)
    }

    @Test func aProfilesFileWrittenByTheWindowsAppOpens() throws {
        let salt = Data((0..<16).map { UInt8($0) })
        let derived = try ProfileCrypto.derive(password: "geçerli şifre", salt: salt, iterations: 100_000)
        let vaultJSON = """
        {"videos":[{"contentFingerprint":"fp:5:abcdef","videoPath":"/Movies/film.mkv","fileName":"film.mkv","directoryPath":"/Movies",\
        "viewCount":3,"isLiked":true,"rating":4,"lastPositionSeconds":12.5,"durationSeconds":99,\
        "lastWatchedAtUtc":"2026-05-01T10:20:30.1234567+00:00","updatedAtUtc":"2026-05-01T10:20:30.1234567+00:00"}],\
        "customLists":[{"id":"0123456789abcdef0123456789abcdef","name":"Liste","videoPaths":["/Movies/film.mkv"],\
        "createdAtUtc":"2026-05-01T10:00:00+03:00","updatedAtUtc":"2026-05-01T10:00:00Z"}],"thumbnails":{"fp:5:abcdef":"SU1H"}}
        """
        // Built by hand in the Windows layout, not through ProfileCrypto.seal.
        let nonce = AES.GCM.Nonce()
        let box = try AES.GCM.seal(Data(vaultJSON.utf8), using: derived.vaultKey, nonce: nonce)
        let vault = (Data(nonce) + box.tag + box.ciphertext).base64EncodedString()
        let file = """
        {
          "version": 2,
          "rememberMe": false,
          "lastUsername": "Ayşe",
          "rememberedProtectedKeyBase64": "AAAA",
          "profiles": [
            {
              "username": "Ayşe",
              "saltBase64": "\(salt.base64EncodedString())",
              "passwordHashBase64": "\(derived.passwordHash.base64EncodedString())",
              "iterations": 100000,
              "createdAtUtc": "2026-04-30T08:00:00.0000000+00:00",
              "encryptedVaultBase64": "\(vault)"
            }
          ]
        }
        """
        try Data(file.utf8).write(to: folder.profilesFile)

        let store = folder.makeStore()
        #expect(store.registeredUsernames == ["Ayşe"])
        #expect(store.lastUsername == "Ayşe")
        #expect(!store.isLoggedIn)
        try store.login(username: "ayşe", password: "geçerli şifre", rememberMe: false)

        let stats = try #require(store.stats(for: URL(fileURLWithPath: "/Movies/film.mkv")))
        #expect(stats.viewCount == 3)
        #expect(stats.isLiked)
        #expect(stats.rating == 4)
        #expect(stats.contentFingerprint == "fp:5:abcdef")
        // A picture an older vault carries is still found.
        #expect(stats.thumbnail == Data("IMG".utf8))
        #expect(store.thumbnail(for: URL(fileURLWithPath: "/Movies/film.mkv")) == Data("IMG".utf8))
        #expect(stats.lastWatchedAt == ProfileDateCoding.date(from: "2026-05-01T10:20:30.1234567+00:00"))
        let list = try #require(store.lists().first)
        #expect(list.id == "0123456789abcdef0123456789abcdef")
        // 10:00 at +03:00 is 07:00 UTC.
        #expect(list.createdAt == ProfileDateCoding.date(from: "2026-05-01T07:00:00Z"))
        #expect(list.updatedAt == ProfileDateCoding.date(from: "2026-05-01T10:00:00Z"))
    }

    @Test func dateCodingRoundTripsAndReadsDotNetShapes() throws {
        let date = Date(timeIntervalSince1970: 1_800_000_123.4567)
        let text = ProfileDateCoding.string(from: date)
        #expect(text.hasSuffix("+00:00"))
        #expect(text.contains("."))
        let back = try #require(ProfileDateCoding.date(from: text))
        #expect(abs(back.timeIntervalSince(date)) < 1e-6)

        #expect(ProfileDateCoding.string(from: Date(timeIntervalSince1970: 0)) == "1970-01-01T00:00:00.0000000+00:00")
        #expect(ProfileDateCoding.date(from: "2026-11-02T18:30:15Z") == ProfileDateCoding.date(from: "2026-11-02T21:30:15+03:00"))
        #expect(ProfileDateCoding.date(from: "2026-11-02T18:30:15") == ProfileDateCoding.date(from: "2026-11-02T18:30:15Z"))
        #expect(ProfileDateCoding.date(from: "2026-11-02T18:30:15.5Z")?.timeIntervalSince1970 ?? 0
            == (ProfileDateCoding.date(from: "2026-11-02T18:30:15Z")?.timeIntervalSince1970 ?? 0) + 0.5)
        #expect(ProfileDateCoding.date(from: "not a date") == nil)
        #expect(ProfileDateCoding.date(from: "2026-13-02T18:30:15Z") == nil)
        #expect(ProfileDateCoding.compactTimestamp(Date(timeIntervalSince1970: 0)) == "19700101-000000")
    }

    // MARK: - Remember me

    @Test func theRememberedKeyLivesInTheSecretStoreAndNeverInTheFile() throws {
        let secrets = InMemorySecretStore()
        let store = folder.makeStore(secrets: secrets)
        try store.register(username: "Emre", password: "pw")

        #expect(secrets.accounts == ["profile-remember.emre"])
        let key = try #require(secrets.data(for: "profile-remember.emre"))
        #expect(key.count == 32)
        let raw = try folder.rawProfiles()
        #expect(!raw.contains(key.base64EncodedString()))
        #expect(!raw.lowercased().contains("protectedkey"))
        let root = try folder.profilesObject()
        #expect(root["rememberMe"] as? Bool == true)
        #expect(root["rememberedUsername"] as? String == "Emre")
        #expect(store.rememberedUsername == "Emre")

        // The stored key is the vault key, so it opens the vault without the password.
        let profile = try #require((root["profiles"] as? [[String: Any]])?.first)
        let blob = try #require(Data(base64Encoded: profile["encryptedVaultBase64"] as! String))
        #expect(ProfileCrypto.open(blob, key: SymmetricKey(data: key)) != nil)
    }

    @Test func logoutClearsTheStoredKeyAndRememberedUsernameButKeepsRememberMeAndLastUsername() throws {
        let secrets = InMemorySecretStore()
        let store = folder.makeStore(secrets: secrets)
        try store.register(username: "emre", password: "pw")
        try store.logout()

        #expect(secrets.accounts.isEmpty)
        let root = try folder.profilesObject()
        #expect(root["rememberedUsername"] == nil)
        #expect(root["rememberMe"] as? Bool == true)
        #expect(root["lastUsername"] as? String == "emre")
        #expect(store.rememberedUsername == nil)
        #expect(store.rememberMe)
        #expect(store.lastUsername == "emre")
    }

    @Test func loggingInWithRememberMeOffClearsTheRememberedLogin() throws {
        let secrets = InMemorySecretStore()
        let store = folder.makeStore(secrets: secrets)
        try store.register(username: "emre", password: "pw")
        #expect(!secrets.accounts.isEmpty)

        try store.login(username: "emre", password: "pw", rememberMe: false)
        #expect(secrets.accounts.isEmpty)
        #expect(!store.rememberMe)
        #expect(try folder.profilesObject()["rememberedUsername"] == nil)
        #expect(!folder.makeStore(secrets: secrets).isLoggedIn)

        try store.logout()
        try store.register(username: "second", password: "pw", rememberMe: false)
        #expect(secrets.accounts.isEmpty)
    }

    @Test func switchingTheRememberedUserForgetsThePreviousKey() throws {
        let secrets = InMemorySecretStore()
        let store = folder.makeStore(secrets: secrets)
        try store.register(username: "one", password: "pw1")
        try store.register(username: "two", password: "pw2")
        #expect(secrets.accounts == ["profile-remember.two"])
        #expect(store.rememberedUsername == "two")
    }

    @Test func deletingAProfileRemovesItsKeyVaultAndSession() throws {
        let secrets = InMemorySecretStore()
        let store = folder.makeStore(secrets: secrets)
        try store.register(username: "emre", password: "pw")
        try store.register(username: "other", password: "pw2", rememberMe: false)
        try store.login(username: "emre", password: "pw", rememberMe: true)
        #expect(secrets.accounts == ["profile-remember.emre"])

        #expect(throws: ProfileError.invalidCredentials) { try store.deleteProfile(username: "emre", password: "nope") }
        #expect(store.registeredUsernames == ["emre", "other"])

        try store.deleteProfile(username: "EMRE", password: "pw")
        #expect(secrets.accounts.isEmpty)
        #expect(store.registeredUsernames == ["other"])
        #expect(!store.isLoggedIn)
        #expect(!folder.makeStore(secrets: secrets).isLoggedIn)
        #expect(throws: ProfileError.invalidCredentials) { try store.login(username: "emre", password: "pw") }
    }

    @Test func registerRulesAreEnforced() throws {
        let store = folder.makeStore()
        #expect(throws: ProfileError.emptyUsername) { try store.register(username: "   ", password: "pw") }
        #expect(throws: ProfileError.emptyPassword) { try store.register(username: "emre", password: "") }
        try store.register(username: "  emre  ", password: " pw ")
        #expect(store.currentUsername == "emre")
        #expect(throws: ProfileError.usernameTaken) { try store.register(username: "EMRE", password: "x") }
        #expect(throws: ProfileError.missingCredentials) { try store.login(username: "", password: "x") }
        #expect(throws: ProfileError.missingCredentials) { try store.login(username: "emre", password: "") }

        // Passwords are not trimmed.
        try store.logout()
        #expect(throws: ProfileError.invalidCredentials) { try store.login(username: "emre", password: "pw") }
        try store.login(username: "emre", password: " pw ")
    }
}
