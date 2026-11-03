import CryptoKit
import Foundation
import Testing
@testable import LibraryKit

/// The card pictures live in an encrypted cache next to the vault: storing one never rewrites `profiles.json` and
/// never creates a record, a vault from an older version is migrated, the cache is capped and goes with its profile.
@Suite final class ThumbnailCacheTests {
    let folder = TestFolder()
    private let picture = Data((0..<2_000).map { UInt8($0 % 251) })

    private func size(of url: URL) -> Int {
        (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? Int) ?? -1
    }

    // MARK: Storing

    @Test func storingPicturesNeverRewritesTheVaultOrCreatesRecords() throws {
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        let before = try Data(contentsOf: folder.profilesFile)
        let signature = ProfileFileIO.signature(of: folder.profilesFile)

        var videos: [URL] = []
        for index in 0..<40 {
            let video = folder.video("v\(index).mp4")
            try Data("content \(index)".utf8).write(to: video)
            videos.append(video)
            #expect(try store.setThumbnail(picture, for: video))
        }

        // Not one byte of the profiles file moved, and no video became a record.
        #expect(try Data(contentsOf: folder.profilesFile) == before)
        #expect(ProfileFileIO.signature(of: folder.profilesFile) == signature)
        #expect(store.totals() == .zero)
        #expect(store.feed(.history).isEmpty)
        #expect(videos.allSatisfy { store.stats(for: $0) == nil })
        #expect(!store.hasUnsavedChanges)
        // Every picture is a file of its own, and reads back.
        #expect(try folder.cacheFiles().count == 40)
        #expect(videos.allSatisfy { store.thumbnail(for: $0) == picture })
    }

    @Test func picturesAreEncryptedUnderTheProfilesKeyWithOpaqueNames() throws {
        let store = folder.makeStore()
        try store.register(username: "Emre Gizli", password: "pw", rememberMe: false)
        let video = folder.video("Cok_Gizli_Film.mp4")
        #expect(try store.setThumbnail(picture, for: video))

        let file = try #require(folder.cacheFiles().first)
        let bytes = try Data(contentsOf: file)
        #expect(bytes.range(of: picture.prefix(64)) == nil)
        #expect(file.pathExtension == "bin")
        let names = (file.deletingLastPathComponent().lastPathComponent + file.lastPathComponent).lowercased()
        #expect(!names.contains("emre") && !names.contains("film") && !names.contains("mp4"))
        #expect(bytes.count == picture.count + ProfileCrypto.nonceLength + ProfileCrypto.tagLength)

        // Another key does not open it; the file is dropped instead of being served.
        let cache = store.thumbnailCache
        let wrongKey = SymmetricKey(size: .bits256)
        #expect(cache.data(profile: "emre gizli", keys: [VaultContents.key(for: normalized(video))], vaultKey: wrongKey) == nil)
        #expect(try folder.cacheFiles().isEmpty)
    }

    @Test func loggedOutNothingIsStoredOrRead() throws {
        let store = folder.makeStore()
        let video = folder.video("a.mp4")
        #expect(try store.setThumbnail(picture, for: video) == false)
        #expect(store.thumbnail(for: video) == nil)
        #expect(try folder.cacheFiles().isEmpty)
    }

    @Test func eachProfileHasItsOwnPictures() throws {
        let store = folder.makeStore()
        let video = folder.video("a.mp4")
        try store.register(username: "ali", password: "pw", rememberMe: false)
        #expect(try store.setThumbnail(picture, for: video))
        try store.logout()
        try store.register(username: "veli", password: "pw", rememberMe: false)
        #expect(store.thumbnail(for: video) == nil)
        try store.logout()
        try store.login(username: "ali", password: "pw", rememberMe: false)
        #expect(store.thumbnail(for: video) == picture)
    }

    @Test func aPictureFollowsTheFileWhenItMoves() throws {
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw", rememberMe: false)
        let original = folder.video("original.mp4")
        try Data(repeating: 9, count: 5_000).write(to: original)
        #expect(try store.setThumbnail(picture, for: original))
        let moved = folder.video("renamed.mp4")
        try FileManager.default.moveItem(at: original, to: moved)
        #expect(store.thumbnail(for: moved) == picture)
    }

    // MARK: Migration

    @Test func aVaultWithPicturesIsMigratedOnItsFirstSave() throws {
        let password = "pw"
        let salt = Data((0..<16).map { UInt8($0) })
        let derived = try ProfileCrypto.derive(password: password, salt: salt, iterations: 100_000)
        func record(_ path: String, views: Int = 0, liked: Bool = false, position: Double = 0) -> String {
            let watched = views > 0 ? ",\"lastWatchedAtUtc\":\"2026-05-01T10:20:30Z\"" : ""
            return "{\"videoPath\":\"\(path)\",\"fileName\":\"\((path as NSString).lastPathComponent)\","
                + "\"directoryPath\":\"/Movies\",\"viewCount\":\(views),\"isLiked\":\(liked),\"rating\":0,"
                + "\"lastPositionSeconds\":\(position),\"durationSeconds\":0,\"updatedAtUtc\":\"2026-05-01T10:20:30Z\"\(watched)}"
        }
        let image = picture.base64EncodedString()
        let records = [
            record("/Movies/watched.mkv", views: 2), record("/Movies/browsed.mkv"), record("/Movies/listed.mkv"),
            record("/Movies/resumed.mkv", position: 30), record("/Movies/liked.mkv", liked: true),
        ].joined(separator: ",")
        let pictures = ["watched", "browsed", "listed", "resumed", "liked"].map { "\"/movies/\($0).mkv\":\"\(image)\"" }
            + ["\"fp:9:orphan\":\"\(image)\""]
        let vaultJSON = "{\"videos\":[\(records)],"
            + "\"customLists\":[{\"id\":\"0123456789abcdef0123456789abcdef\",\"name\":\"Liste\","
            + "\"videoPaths\":[\"/Movies/listed.mkv\"],\"createdAtUtc\":\"2026-05-01T07:00:00Z\","
            + "\"updatedAtUtc\":\"2026-05-01T07:00:00Z\"}],\"thumbnails\":{\(pictures.joined(separator: ","))}}"
        let box = try AES.GCM.seal(Data(vaultJSON.utf8), using: derived.vaultKey, nonce: AES.GCM.Nonce())
        let vault = (Data(box.nonce) + box.tag + box.ciphertext).base64EncodedString()
        let file = "{\"version\":2,\"rememberMe\":false,\"profiles\":[{\"username\":\"Ayşe\","
            + "\"saltBase64\":\"\(salt.base64EncodedString())\","
            + "\"passwordHashBase64\":\"\(derived.passwordHash.base64EncodedString())\",\"iterations\":100000,"
            + "\"createdAtUtc\":\"2026-04-30T08:00:00Z\",\"encryptedVaultBase64\":\"\(vault)\"}]}"
        try Data(file.utf8).write(to: folder.profilesFile)
        let sizeBefore = size(of: folder.profilesFile)

        let store = folder.makeStore()
        try store.login(username: "Ayşe", password: password, rememberMe: false)
        // Before any save the pictures are served from the vault.
        #expect(store.thumbnail(for: URL(fileURLWithPath: "/Movies/browsed.mkv")) == picture)
        #expect(try folder.cacheFiles().isEmpty)

        // The first write of the vault moves them.
        try store.setRating(3, for: folder.video("other.mp4"))
        #expect(try folder.cacheFiles().count == 6)
        for name in ["watched", "browsed", "listed", "resumed", "liked"] {
            #expect(store.thumbnail(for: URL(fileURLWithPath: "/Movies/\(name).mkv")) == picture, "\(name)")
        }
        #expect(size(of: folder.profilesFile) < sizeBefore)

        let profiles = try folder.profilesObject()["profiles"] as? [[String: Any]]
        let encoded = try #require(profiles?.first?["encryptedVaultBase64"] as? String)
        let blob = try #require(Data(base64Encoded: encoded))
        let plain = try #require(ProfileCrypto.open(blob, key: derived.vaultKey))
        let payload = try #require(JSONSerialization.jsonObject(with: plain) as? [String: Any])
        #expect((payload["thumbnails"] as? [String: Any])?.isEmpty == true)
        let kept = Set((payload["videos"] as? [[String: Any]] ?? []).compactMap { $0["videoPath"] as? String })
        // The record that only held a picture is gone; everything the user did stays.
        #expect(kept == [
            "/Movies/watched.mkv", "/Movies/listed.mkv", "/Movies/resumed.mkv", "/Movies/liked.mkv",
            normalized(folder.video("other.mp4")),
        ])

        // A new instance finds them in the cache.
        let again = folder.makeStore(secrets: InMemorySecretStore())
        try again.login(username: "Ayşe", password: password, rememberMe: false)
        #expect(again.thumbnail(for: URL(fileURLWithPath: "/Movies/browsed.mkv")) == picture)
    }

    // MARK: Limits

    @Test func theCacheEvictsTheLeastRecentlyUsedPictures() throws {
        let root = folder.url.appending(path: "cache")
        let cache = ThumbnailCache(root: root, limits: ThumbnailCacheLimits(maxFiles: 10, maxBytes: 10_000_000))
        let key = SymmetricKey(size: .bits256)
        let profile = "ali"
        let directory = root.appending(path: ThumbnailCache.folderName(forProfile: profile))

        for index in 0..<10 {
            #expect(cache.store(picture, profile: profile, key: "k\(index)", vaultKey: key))
            // k0 is the oldest, k9 the newest.
            let date = Date(timeIntervalSinceNow: Double(index - 100) * 60)
            let file = directory.appending(path: ThumbnailCache.fileName(forKey: "k\(index)"))
            try FileManager.default.setAttributes([.modificationDate: date], ofItemAtPath: file.path)
        }
        #expect(cache.fileCount(profile: profile) == 10)

        // The 11th goes past the cap: down to 90 %, oldest first, the new one kept.
        #expect(cache.store(picture, profile: profile, key: "k10", vaultKey: key))
        #expect(cache.fileCount(profile: profile) == 9)
        #expect(cache.data(profile: profile, keys: ["k0"], vaultKey: key) == nil)
        #expect(cache.data(profile: profile, keys: ["k1"], vaultKey: key) == nil)
        #expect(cache.data(profile: profile, keys: ["k2"], vaultKey: key) == picture)
        #expect(cache.data(profile: profile, keys: ["k10"], vaultKey: key) == picture)
    }

    @Test func theCacheIsCappedInBytesToo() throws {
        let each = picture.count + ProfileCrypto.nonceLength + ProfileCrypto.tagLength
        let cache = ThumbnailCache(
            root: folder.url.appending(path: "cache"),
            limits: ThumbnailCacheLimits(maxFiles: 1000, maxBytes: each * 5 + 10))
        let key = SymmetricKey(size: .bits256)
        for index in 0..<12 { #expect(cache.store(picture, profile: "ali", key: "k\(index)", vaultKey: key)) }
        let count = cache.fileCount(profile: "ali")
        #expect(count <= 5 && count >= 3)
        #expect(cache.data(profile: "ali", keys: ["k11"], vaultKey: key) == picture)
    }

    @Test func readingKeepsAPictureAliveAgainstEviction() throws {
        let root = folder.url.appending(path: "cache")
        let cache = ThumbnailCache(root: root, limits: ThumbnailCacheLimits(maxFiles: 5, maxBytes: 10_000_000))
        let key = SymmetricKey(size: .bits256)
        let directory = root.appending(path: ThumbnailCache.folderName(forProfile: "ali"))
        for index in 0..<5 {
            #expect(cache.store(picture, profile: "ali", key: "k\(index)", vaultKey: key))
            let file = directory.appending(path: ThumbnailCache.fileName(forKey: "k\(index)"))
            let date = Date(timeIntervalSinceNow: -Double(10 - index) * 7_200)
            try FileManager.default.setAttributes([.modificationDate: date], ofItemAtPath: file.path)
        }
        // k0 is the oldest, but it is read now: its date is renewed.
        #expect(cache.data(profile: "ali", keys: ["k0"], vaultKey: key) == picture)
        #expect(cache.store(picture, profile: "ali", key: "k5", vaultKey: key))
        #expect(cache.data(profile: "ali", keys: ["k0"], vaultKey: key) == picture)
        #expect(cache.data(profile: "ali", keys: ["k1"], vaultKey: key) == nil)
    }

    // MARK: Removing

    @Test func deletingAProfileDeletesItsPictures() throws {
        let store = folder.makeStore()
        try store.register(username: "ali", password: "pw", rememberMe: false)
        let video = folder.video("a.mp4")
        #expect(try store.setThumbnail(picture, for: video))
        try store.logout()
        try store.register(username: "veli", password: "pw", rememberMe: false)
        #expect(try store.setThumbnail(picture, for: video))
        #expect(try folder.cacheFiles().count == 2)

        try store.deleteProfile(username: "ali", password: "pw")
        #expect(try folder.cacheFiles().count == 1)
        #expect(store.thumbnail(for: video) == picture)
        try store.deleteProfile(username: "veli", password: "pw")
        #expect(try folder.cacheFiles().isEmpty)
    }

    @Test func clearingTheHistoryForgetsThePicturesOfTheVideosItDrops() throws {
        let store = folder.makeStore()
        try store.register(username: "ali", password: "pw", rememberMe: false)
        let dropped = folder.video("dropped.mp4"), kept = folder.video("kept.mp4"), browsed = folder.video("browsed.mp4")
        try store.recordWatch(dropped)
        try store.recordWatch(kept)
        try store.setLiked(true, for: kept)
        for video in [dropped, kept, browsed] { #expect(try store.setThumbnail(picture, for: video)) }

        #expect(try store.clearWatchHistory())
        #expect(store.thumbnail(for: dropped) == nil)
        #expect(store.thumbnail(for: kept) == picture)
        // A picture of a video that never had a record is not history; it stays.
        #expect(store.thumbnail(for: browsed) == picture)
    }

    // MARK: Sessions and migrations

    @Test func theSessionGenerationChangesWithEveryLoginAndLogout() throws {
        let store = folder.makeStore()
        let start = store.sessionGeneration
        try store.register(username: "ali", password: "pw", rememberMe: false)
        let registered = store.sessionGeneration
        #expect(registered != start)
        // Plain work inside the session leaves it alone.
        try store.setLiked(true, for: folder.video("a.mp4"))
        #expect(store.sessionGeneration == registered)
        try store.logout()
        let loggedOut = store.sessionGeneration
        #expect(loggedOut != registered)
        try store.logout()
        #expect(store.sessionGeneration == loggedOut)
        try store.login(username: "ali", password: "pw", rememberMe: false)
        #expect(store.sessionGeneration != loggedOut)
    }

    @Test func playbackProgressAloneIsNotAnUnsavedMigration() throws {
        let store = folder.makeStore()
        try store.register(username: "ali", password: "pw", rememberMe: false)
        let video = folder.video("a.mp4")
        try Data(repeating: 3, count: 4_000).write(to: video)
        try store.recordWatch(video)
        try store.updatePlaybackProgress(video, position: 20, duration: 100)
        #expect(store.hasUnsavedChanges)
        #expect(!store.hasUnsavedMigrations)

        // Following a move is one.
        let moved = folder.video("b.mp4")
        try FileManager.default.moveItem(at: video, to: moved)
        _ = store.stats(for: moved)
        #expect(store.hasUnsavedMigrations)
        try store.persistPendingMigrations()
        #expect(!store.hasUnsavedMigrations)
        #expect(!store.hasUnsavedChanges)
    }

    @Test func aStoreCanBeBuiltWithoutReadingTheFile() throws {
        let first = folder.makeStore()
        try first.register(username: "ali", password: "pw", rememberMe: false)
        let lazy = ProfileStore(
            fileURL: folder.profilesFile, secretStore: InMemorySecretStore(), watchesFile: false, readsFileImmediately: false)
        // Nothing is read until something asks.
        #expect(lazy.registeredUsernames == ["ali"])
        try lazy.login(username: "ali", password: "pw", rememberMe: false)
        #expect(lazy.isLoggedIn)
    }
}
