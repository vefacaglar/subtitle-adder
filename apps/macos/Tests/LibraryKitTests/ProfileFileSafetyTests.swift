import Foundation
import Testing
@testable import LibraryKit

/// Files this store cannot make sense of for reasons other than damage: written by a newer format, unreadable right now,
/// or a remembered login that nothing can vouch for.
@Suite final class ProfileFileSafetyTests {
    let folder = TestFolder()

    private func asideFiles() -> [String] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: folder.url.path)) ?? []
        return names.filter { $0.hasPrefix("profiles.corrupt-") }.sorted()
    }

    // MARK: - Newer format (M1)

    @Test(arguments: [
        #"{"version": 3, "accounts": {"alice": {"salt": 5}}, "profiles": "a different shape"}"#,
        #"{"version": 99, "profiles": {"alice": {"username": 5}}}"#,
        #"{"version": 4, "profiles": [{"username": ["not", "a", "string"]}], "rememberMe": "yes"}"#,
        #"{"version": 3}"#,
    ])
    func aNewerFileWhoseShapeDiffersIsLeftAloneNotTreatedAsCorrupt(json: String) throws {
        let future = Data(json.utf8)
        try future.write(to: folder.profilesFile)

        let store = folder.makeStore()
        #expect(store.registeredUsernames.isEmpty)
        #expect(throws: ProfileError.self) { try store.register(username: "bob", password: "pw") }
        #expect(try Data(contentsOf: folder.profilesFile) == future, "never replaced by a version 2 file")
        #expect(asideFiles().isEmpty, "never set aside either")

        store.refresh()
        #expect(throws: ProfileError.self) { try store.register(username: "bob", password: "pw") }
        #expect(try Data(contentsOf: folder.profilesFile) == future)
        #expect(!store.isLoggedIn)
    }

    @Test func theErrorNamesTheNewerVersion() throws {
        try Data(#"{"version": 7, "profiles": 12}"#.utf8).write(to: folder.profilesFile)
        let store = folder.makeStore()
        #expect(throws: ProfileError.unsupportedFileVersion(7)) { try store.register(username: "bob", password: "pw") }
    }

    @Test func aNewerFileLeftAloneByARunningStoreStaysUntouchedByItsWrites() throws {
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        let future = Data(#"{"version": 5, "vaults": []}"#.utf8)
        try future.write(to: folder.profilesFile)
        #expect(throws: ProfileError.unsupportedFileVersion(5)) { try store.createList(named: "x") }
        #expect(try Data(contentsOf: folder.profilesFile) == future)
        #expect(asideFiles().isEmpty)
    }

    @Test(arguments: [
        #"{"version": "two"}"#,
        #"{"version": 2, "profiles": "x"}"#,
        #"[1, 2, 3]"#,
        "",
    ])
    func aFileOfTheCurrentFormatThatCannotBeParsedIsStillDamageAndSetAside(json: String) throws {
        try Data(json.utf8).write(to: folder.profilesFile)
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        #expect(asideFiles().count == 1)
        #expect(try folder.profilesObject()["version"] as? Int == 2)
    }

    // MARK: - Unreadable file (M2)

    @Test(.enabled(if: getuid() != 0, "root ignores file permissions"))
    func anUnreadableFileIsNeverReplacedByTheEmptyViewOfAStoreThatCouldNotReadIt() throws {
        let first = folder.makeStore()
        try first.register(username: "alice", password: "pw-alice", rememberMe: false)
        try first.logout()
        let original = try Data(contentsOf: folder.profilesFile)

        chmod(folder.profilesFile.path, 0o000)
        defer { chmod(folder.profilesFile.path, 0o644) }
        let blind = folder.makeStore()
        #expect(blind.registeredUsernames.isEmpty, "it cannot see the profiles")
        #expect(throws: ProfileError.fileUnreadable) { try blind.register(username: "bob", password: "pw-bob") }
        #expect(throws: ProfileError.fileUnreadable) { try blind.login(username: "alice", password: "pw-alice") }
        #expect(throws: ProfileError.fileUnreadable) { try blind.deleteProfile(username: "alice", password: "pw-alice") }
        #expect(!blind.isLoggedIn)

        chmod(folder.profilesFile.path, 0o644)
        #expect(try Data(contentsOf: folder.profilesFile) == original, "nothing was written")
        #expect(asideFiles().isEmpty, "and nothing was set aside")

        // Readable again: the same store notices on its next call and keeps alice.
        #expect(blind.registeredUsernames == ["alice"])
        try blind.register(username: "bob", password: "pw-bob", rememberMe: false)
        #expect(blind.registeredUsernames == ["alice", "bob"])
        #expect(folder.makeStore().registeredUsernames == ["alice", "bob"])
        try blind.login(username: "alice", password: "pw-alice", rememberMe: false)
    }

    @Test(.enabled(if: getuid() != 0, "root ignores file permissions"))
    func aFileThatBecomesReadableIsReadAgainEvenThoughItsSignatureNeverChanged() throws {
        let first = folder.makeStore()
        try first.register(username: "alice", password: "pw", rememberMe: false)
        chmod(folder.profilesFile.path, 0o000)
        defer { chmod(folder.profilesFile.path, 0o644) }
        let blind = folder.makeStore()
        #expect(blind.registeredUsernames.isEmpty)
        #expect(blind.registeredUsernames.isEmpty, "still unreadable, still empty")
        chmod(folder.profilesFile.path, 0o644)
        // chmod changes neither the size, the modification time nor the inode.
        #expect(blind.registeredUsernames == ["alice"])
    }

    // MARK: - Remembered login without a vault (L6)

    @Test func aRememberedLoginNeverOpensAProfileThatHasNoVault() throws {
        let secrets = InMemorySecretStore()
        let setup = folder.makeStore(secrets: secrets)
        try setup.register(username: "ghost", password: "pw")
        try setup.logout()

        // The profile loses its vault (a file from another build), is marked as remembered, and an unrelated 32-byte
        // value sits in the keychain under its account.
        var root = try folder.profilesObject()
        var profiles = try #require(root["profiles"] as? [[String: Any]])
        profiles[0].removeValue(forKey: "encryptedVaultBase64")
        root["profiles"] = profiles
        root["rememberMe"] = true
        root["rememberedUsername"] = "ghost"
        try JSONSerialization.data(withJSONObject: root).write(to: folder.profilesFile)
        secrets.set(ProfileCrypto.randomBytes(ProfileCrypto.keyLength), for: "profile-remember.ghost")

        let planted = folder.makeStore(secrets: secrets)
        #expect(!planted.isLoggedIn, "nothing can confirm the key, so it must not unlock anything")
        #expect(planted.currentUsername == nil)
        #expect(planted.rememberedUsername == "ghost")
        planted.refresh()
        #expect(!planted.isLoggedIn)

        // The password still opens it.
        try planted.login(username: "ghost", password: "pw")
        #expect(planted.currentUsername == "ghost")
        try planted.setLiked(true, for: folder.video("a.mp4"))
        #expect(folder.makeStore(secrets: secrets).isLoggedIn, "with a vault written, remember-me works again")
    }

    @Test func aWrongKeyForAProfileWithAVaultStillDoesNotLogIn() throws {
        let secrets = InMemorySecretStore()
        let setup = folder.makeStore(secrets: secrets)
        try setup.register(username: "alice", password: "pw")
        secrets.set(ProfileCrypto.randomBytes(ProfileCrypto.keyLength), for: "profile-remember.alice")
        #expect(!folder.makeStore(secrets: secrets).isLoggedIn)
    }

    @Test func aMissingOrShortKeyDoesNotLogIn() throws {
        let secrets = InMemorySecretStore()
        let setup = folder.makeStore(secrets: secrets)
        try setup.register(username: "alice", password: "pw")
        secrets.set(Data(repeating: 1, count: 16), for: "profile-remember.alice")
        #expect(!folder.makeStore(secrets: secrets).isLoggedIn)
        secrets.remove("profile-remember.alice")
        #expect(!folder.makeStore(secrets: secrets).isLoggedIn)
    }
}
