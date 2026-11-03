import Foundation
import Testing
@testable import LibraryKit

/// Ports of the Windows `LocalProfileTests` (all but the named-pipe broker test).
@Suite final class ProfileStoreTests {
    let folder = TestFolder()

    @Test func registerHashesPasswordWithPbkdf2AndNeverStoresPlaintext() throws {
        let store = folder.makeStore()
        #expect(!store.isLoggedIn)
        #expect(store.currentUsername == nil)

        let secret = "MySuperSecretPassword!2026"
        try store.register(username: "emre", password: secret)

        #expect(store.isLoggedIn)
        #expect(store.currentUsername == "emre")
        #expect(store.registeredUsernames.contains("emre"))
        #expect(FileManager.default.fileExists(atPath: folder.profilesFile.path))

        let raw = try folder.rawProfiles()
        #expect(!raw.lowercased().contains(secret.lowercased()))
        #expect(raw.contains("saltBase64"))
        #expect(raw.contains("passwordHashBase64"))
        #expect(raw.contains("100000"))
    }

    @Test func loginAndLogoutEnforcePasswordAndSessionState() throws {
        let store = folder.makeStore()
        try store.register(username: "alice", password: "CorrectHorseBatteryStaple")
        try store.logout()
        #expect(!store.isLoggedIn)
        #expect(store.currentUsername == nil)

        #expect(throws: ProfileError.invalidCredentials) { try store.login(username: "alice", password: "WrongPassword") }
        #expect(!store.isLoggedIn)
        #expect(throws: ProfileError.invalidCredentials) {
            try store.login(username: "unknown_user", password: "CorrectHorseBatteryStaple")
        }
        #expect(!store.isLoggedIn)

        // The username match ignores case and keeps the registered casing.
        try store.login(username: "ALICE", password: "CorrectHorseBatteryStaple")
        #expect(store.isLoggedIn)
        #expect(store.currentUsername == "alice")
    }

    @Test func loggedOutStateDoesNotRecordWatchHistoryOrExposeStats() throws {
        let video = folder.video("lecture.mp4")
        let store = folder.makeStore()
        try store.register(username: "alice", password: "pw123")
        try store.recordWatch(video, position: 42, duration: 120)
        try store.setLiked(true, for: video)
        try store.setRating(5, for: video)
        try store.logout()

        #expect(store.stats(for: video) == nil)
        #expect(store.feed(.all).isEmpty)
        #expect(store.feed(.history).isEmpty)
        #expect(store.totals() == .zero)

        #expect(try store.recordWatch(video, position: 99, duration: 120) == nil)
        #expect(try store.toggleLike(video) == false)
        #expect(try store.setRating(1, for: video) == nil)
        #expect(try store.createList(named: "Test List") == nil)

        try store.login(username: "alice", password: "pw123")
        let stats = try #require(store.stats(for: video))
        #expect(stats.viewCount == 1)
        #expect(stats.isLiked)
        #expect(stats.rating == 5)
        #expect(stats.lastPositionSeconds == 42)
    }

    @Test func persistenceReloadsViewCountLikesRatingsAndLastWatchedFromDisk() throws {
        let video = folder.video("movie.mkv")
        let first = folder.makeStore()
        try first.register(username: "cinema_fan", password: "pass99", rememberMe: false)
        try first.recordWatch(video, position: 15, duration: 300)
        try first.recordWatch(video, position: 180, duration: 300)
        try first.updatePlaybackProgress(video, position: 210, duration: 300)
        try first.setLiked(true, for: video)
        try first.setRating(4, for: video)

        let second = folder.makeStore()
        #expect(!second.isLoggedIn)
        #expect(second.registeredUsernames.contains("cinema_fan"))

        try second.login(username: "cinema_fan", password: "pass99")
        let restored = try #require(second.stats(for: video))
        #expect(restored.viewCount == 2)
        #expect(restored.isLiked)
        #expect(restored.rating == 4)
        #expect(restored.lastPositionSeconds == 210)
        #expect(restored.durationSeconds == 300)
        #expect(restored.lastWatchedAt != nil)
    }

    @Test func rememberMeDefaultTrueRestoresTheSessionAcrossNewInstancesUntilLogout() throws {
        let video = folder.video("remembered_show.mp4")
        let secrets = InMemorySecretStore()

        let first = folder.makeStore(secrets: secrets)
        try first.register(username: "emre", password: "pass2026")
        #expect(first.rememberMe)
        try first.recordWatch(video, position: 50, duration: 200)
        try first.setLiked(true, for: video)

        // A second instance (or a restart) unlocks the profile without the password.
        let second = folder.makeStore(secrets: secrets)
        #expect(second.isLoggedIn)
        #expect(second.currentUsername == "emre")
        #expect(second.lastUsername == "emre")
        let stats = try #require(second.stats(for: video))
        #expect(stats.viewCount == 1)
        #expect(stats.isLiked)

        // Logging out forgets the remembered login, so a third instance starts logged out.
        try second.logout()
        let third = folder.makeStore(secrets: secrets)
        #expect(!third.isLoggedIn)
        #expect(third.lastUsername == "emre")
    }

    @Test func encryptedVaultAndThumbnailsNeverExposePlaintextOnDisk() throws {
        let secretVideo = folder.video("TopSecret_Project_Alpha_2026.mkv")
        let secretListName = "Gizli Özel Koleksiyonum"
        let thumbnail = Data("FAKE_PNG_THUMBNAIL_PAYLOAD_987654321".utf8)

        let store = folder.makeStore()
        try store.register(username: "vault_user", password: "UltraStrongVaultPass!99", rememberMe: false)
        try store.recordWatch(secretVideo, position: 33, duration: 100)
        try store.setLiked(true, for: secretVideo)
        try store.setRating(5, for: secretVideo)
        let list = try #require(try store.createList(named: secretListName))
        #expect(try store.addVideo(secretVideo, toList: list.id))
        #expect(try store.setThumbnail(thumbnail, for: secretVideo))

        let raw = try folder.rawProfiles()
        #expect(raw.contains("encryptedVaultBase64"))
        // The picture is in the cache folder as ciphertext, under names that say nothing.
        let cached = try folder.cacheFiles()
        #expect(cached.count == 1)
        for file in cached {
            let bytes = try Data(contentsOf: file)
            #expect(bytes.range(of: thumbnail) == nil)
            #expect(!file.path.lowercased().contains("topsecret"))
            #expect(!file.path.lowercased().contains("vault_user"))
        }
        #expect(!raw.lowercased().contains("topsecret_project_alpha_2026"))
        #expect(!raw.lowercased().contains(secretListName.lowercased()))
        #expect(!raw.contains(thumbnail.base64EncodedString()))

        let reloaded = folder.makeStore()
        #expect(!reloaded.isLoggedIn)
        #expect(reloaded.thumbnail(for: secretVideo) == nil)

        try reloaded.login(username: "vault_user", password: "UltraStrongVaultPass!99")
        let stats = try #require(reloaded.stats(for: secretVideo))
        #expect(stats.viewCount == 1)
        #expect(stats.isLiked)
        #expect(stats.rating == 5)
        #expect(reloaded.thumbnail(for: secretVideo) == thumbnail)
        // The feed does not carry pictures; they are read where they are drawn.
        #expect(stats.thumbnail == nil)
        let history = reloaded.feed(.history)
        #expect(history.count == 1)
        #expect(history.first?.thumbnail == nil)
    }

    @Test func twoInstancesLoggedIntoTheSameProfileMergeHistoryLikesRatingsAndLists() throws {
        let video1 = folder.video("Instance1_Episode01.mp4")
        let video2 = folder.video("Instance2_Episode02.mp4")
        let secrets = InMemorySecretStore()

        let instance1 = folder.makeStore(secrets: secrets)
        try instance1.register(username: "shared_user", password: "shared_pass_123")
        let instance2 = folder.makeStore(secrets: secrets)
        #expect(instance2.isLoggedIn)
        #expect(instance2.currentUsername == "shared_user")

        try instance1.recordWatch(video1, position: 30, duration: 120)
        try instance1.setLiked(true, for: video1)

        try instance2.recordWatch(video2, position: 90, duration: 180)
        try instance2.setRating(5, for: video2)
        let list = try #require(try instance2.createList(named: "İki Pencere Ortak Liste"))
        #expect(try instance2.addVideo(video2, toList: list.id))

        try instance1.recordWatch(video1, position: 60, duration: 120)
        #expect(try instance1.addVideo(video1, toList: list.id))

        #expect(instance1.feed(.history).count == 2)
        #expect(instance2.feed(.history).count == 2)

        let video1OnInstance2 = try #require(instance2.stats(for: video1))
        #expect(video1OnInstance2.viewCount == 2)
        #expect(video1OnInstance2.isLiked)
        let video2OnInstance1 = try #require(instance1.stats(for: video2))
        #expect(video2OnInstance1.viewCount == 1)
        #expect(video2OnInstance1.rating == 5)

        let listsOn1 = instance1.lists()
        let listsOn2 = instance2.lists()
        #expect(listsOn1.count == 1)
        #expect(listsOn2.count == 1)
        #expect(listsOn1.first?.videoPaths.count == 2)
        #expect(listsOn2.first?.videoPaths.count == 2)
    }

    @Test func progressAndThumbnailWritesNeverRaiseTheLocalChangeSignal() async throws {
        let video = folder.video("Playing_Movie.mp4")
        let store = folder.makeStore(watches: true)
        try store.register(username: "emre", password: "pass123")
        try store.recordWatch(video)

        let log = ChangeLog(store)
        defer { log.stop() }

        for position in [15.0, 30.0, 45.0] { try store.updatePlaybackProgress(video, position: position, duration: 300) }
        let thumbnail = Data("SAMPLE_THUMBNAIL_BYTES".utf8)
        #expect(try store.setThumbnail(thumbnail, for: video))
        #expect(try store.setThumbnail(thumbnail, for: video))
        // The folder watcher fires 300 ms after the writes above; our own writes must not look like outside changes.
        try await Task.sleep(for: .milliseconds(700))

        #expect(log.events.isEmpty)
        #expect(store.stats(for: video)?.lastPositionSeconds == 45)
        #expect(store.thumbnail(for: video) == thumbnail)

        // A real change does raise it.
        try store.setLiked(true, for: video)
        #expect(await eventually { log.events == [.local] })
    }

    @Test func multipleProfilesHaveIsolatedHistoryLikesRatingsAndLists() throws {
        let videoA = folder.video("ep1.mp4")
        let videoB = folder.video("ep2.mp4")
        let store = folder.makeStore()

        try store.register(username: "user_one", password: "pass1")
        for _ in 0..<3 { try store.recordWatch(videoA) }
        try store.setLiked(true, for: videoA)
        try store.setRating(5, for: videoA)
        let scienceFiction = try #require(try store.createList(named: "Bilim Kurgu"))
        #expect(try store.addVideo(videoA, toList: scienceFiction.id))
        try store.logout()

        try store.register(username: "user_two", password: "pass2")
        #expect(store.stats(for: videoA) == nil)
        #expect(store.lists().isEmpty)
        try store.recordWatch(videoA)
        try store.setRating(2, for: videoA)
        try store.recordWatch(videoB)
        try store.recordWatch(videoB)
        try store.setLiked(true, for: videoB)
        try store.setRating(4, for: videoB)
        let comedy = try #require(try store.createList(named: "Komedi"))
        #expect(try store.addVideo(videoB, toList: comedy.id))
        try store.logout()

        let reloaded = folder.makeStore()
        try reloaded.login(username: "user_one", password: "pass1")
        let oneA = try #require(reloaded.stats(for: videoA))
        #expect(oneA.viewCount == 3)
        #expect(oneA.isLiked)
        #expect(oneA.rating == 5)
        #expect(reloaded.stats(for: videoB) == nil)
        let oneLists = reloaded.lists()
        #expect(oneLists.count == 1)
        #expect(oneLists.first?.name == "Bilim Kurgu")
        #expect(oneLists.first?.videoPaths == [normalized(videoA)])

        try reloaded.login(username: "user_two", password: "pass2")
        let twoA = try #require(reloaded.stats(for: videoA))
        let twoB = try #require(reloaded.stats(for: videoB))
        #expect(twoA.viewCount == 1)
        #expect(!twoA.isLiked)
        #expect(twoA.rating == 2)
        #expect(twoB.viewCount == 2)
        #expect(twoB.isLiked)
        #expect(twoB.rating == 4)
        let twoLists = reloaded.lists()
        #expect(twoLists.count == 1)
        #expect(twoLists.first?.name == "Komedi")
        #expect(twoLists.first?.videoPaths == [normalized(videoB)])
    }

    @Test func customListsAndClearWatchHistoryPreserveFavoritesRatingsAndLists() throws {
        let v1 = folder.video("Sezon_1_Bolum_01.mkv")
        let v2 = folder.video("Sezon_2_Bolum_05.mkv")
        let v3 = folder.video("Belgesel_Uzay.mp4")
        let store = folder.makeStore()
        try store.register(username: "emre", password: "pw123")

        let watchLater = try #require(try store.createList(named: "Daha Sonra İzle"))
        let bestEpisodes = try #require(try store.createList(named: "En İyi Bölümler"))
        // The same name ignoring case and diacritics is a duplicate.
        #expect(throws: ProfileError.duplicateListName) { try store.createList(named: "daha sonra izle") }
        #expect(store.lists().count == 2)

        // Videos can join lists before they are ever watched.
        #expect(try store.toggleVideo(v2, inList: watchLater.id))
        #expect(try store.toggleVideo(v1, inList: bestEpisodes.id))
        #expect(try store.toggleVideo(v2, inList: bestEpisodes.id))

        try store.recordWatch(v1)
        try store.toggleLike(v1)
        try store.recordWatch(v2)
        try store.setRating(5, for: v2)
        try store.recordWatch(v3)
        let doomed = Data("DOOMED".utf8)
        let kept = Data("KEPT".utf8)
        try store.setThumbnail(doomed, for: v3)
        try store.setThumbnail(kept, for: v1)

        let inBest = store.feed(.customList(bestEpisodes.id))
        #expect(inBest.map(\.videoPath) == [normalized(v1), normalized(v2)])
        #expect(inBest.last?.rating == 5)
        let names = Set(store.lists(containing: v2).map(\.name))
        #expect(names == ["Daha Sonra İzle", "En İyi Bölümler"])

        // Clearing the history keeps favorites, ratings and list members and drops the rest, thumbnails included.
        #expect(try store.clearWatchHistory())
        #expect(store.totals() == ProfileTotals(watched: 0, views: 0, liked: 1, rated: 1))
        #expect(store.feed(.history).isEmpty)
        let liked = store.feed(.liked)
        #expect(liked.map(\.videoPath) == [normalized(v1)])
        #expect(liked.first?.viewCount == 0)
        #expect(store.thumbnail(for: v1) == kept)
        #expect(store.feed(.topRated).map(\.videoPath) == [normalized(v2)])
        #expect(store.feed(.customList(bestEpisodes.id)).count == 2)
        #expect(store.stats(for: v3) == nil)
        #expect(store.thumbnail(for: v3) == nil)
    }

    @Test func filtersMultiTokenSearchAndEngagementActionsWorkCorrectly() throws {
        let v1 = folder.video("Alpha_Tutorial.mp4")
        let v2 = folder.video("Beta_Documentary.mkv")
        let v3 = folder.video("Gamma_Clip.mov")
        let discovered = folder.video("Sezon_2_Bolum_05.mp4")
        let store = folder.makeStore()

        #expect(!store.isLoggedIn)
        #expect(store.feed(.all).isEmpty)

        try store.register(username: "emre", password: "secret123")
        let all = [v1, v2, v3, discovered]

        try store.recordWatch(v1)
        try store.toggleLike(v1)
        try store.setRating(3, for: v1)
        for _ in 0..<3 { try store.recordWatch(v2) }
        try store.setRating(5, for: v2)
        try store.recordWatch(v3)
        try store.recordWatch(v3)
        try store.toggleLike(v3)
        try store.setRating(4, for: v3)

        func paths(_ filter: VideoFeedFilter, search: String? = nil) -> [String] {
            store.feed(filter, search: search, discovered: all).map(\.videoPath)
        }
        #expect(paths(.mostWatched) == [normalized(v2), normalized(v3), normalized(v1)])
        #expect(paths(.topRated) == [normalized(v2), normalized(v3), normalized(v1)])
        #expect(Set(paths(.liked)) == [normalized(v1), normalized(v3)])
        // The never-watched discovered file is not history.
        #expect(Set(paths(.history)) == [normalized(v1), normalized(v2), normalized(v3)])
        #expect(paths(.all).count == 4)

        #expect(paths(.all, search: "sezon 2 bolum") == [normalized(discovered)])
        #expect(paths(.all, search: "").count == 4)

        // Logged out: personal tabs are locked, "all" still lists the discovered files without any personal data.
        try store.logout()
        #expect(!store.isLoggedIn)
        let anonymous = store.feed(.all, discovered: all)
        #expect(anonymous.count == 4)
        #expect(anonymous.allSatisfy { $0.viewCount == 0 && !$0.isLiked && $0.rating == 0 })
        #expect(anonymous.map(\.fileName) == ["Alpha_Tutorial.mp4", "Beta_Documentary.mkv", "Gamma_Clip.mov", "Sezon_2_Bolum_05.mp4"])
        #expect(store.feed(.history, discovered: all).isEmpty)
        #expect(store.feed(.all, search: "gamma", discovered: all).count == 1)
    }

    @Test func movingOrRenamingAVideoFilePreservesHistoryLikesRatingsAndLists() throws {
        let original = folder.video("video_v1.mp4")
        var bytes = [UInt8](repeating: 0, count: 150_000)
        for index in bytes.indices { bytes[index] = UInt8.random(in: 0...255) }
        try Data(bytes).write(to: original)

        let expectedFingerprint = VideoFingerprint.videoID(for: original)
        #expect(expectedFingerprint.hasPrefix("fp:150000:"))

        let store = folder.makeStore()
        try store.register(username: "emre", password: "pass123")
        try store.recordWatch(original, position: 65, duration: 200)
        try store.recordWatch(original, position: 120, duration: 200)
        try store.setLiked(true, for: original)
        try store.setRating(5, for: original)
        let archive = try #require(try store.createList(named: "Arşivim"))
        #expect(try store.addVideo(original, toList: archive.id))
        try store.setThumbnail(Data("THUMB".utf8), for: original)

        let subfolder = folder.url.appending(path: "subfolder")
        try FileManager.default.createDirectory(at: subfolder, withIntermediateDirectories: true)
        let moved = subfolder.appending(path: "renamed_video.mp4")
        try FileManager.default.moveItem(at: original, to: moved)
        #expect(VideoFingerprint.videoID(for: moved) == expectedFingerprint)

        let migrated = try #require(store.stats(for: moved))
        #expect(migrated.contentFingerprint == expectedFingerprint)
        #expect(migrated.videoPath == normalized(moved))
        #expect(migrated.fileName == "renamed_video.mp4")
        #expect(migrated.directoryPath == normalized(subfolder))
        #expect(migrated.viewCount == 2)
        #expect(migrated.isLiked)
        #expect(migrated.rating == 5)
        #expect(migrated.lastPositionSeconds == 120)
        // The picture follows the file: it is stored under the content fingerprint.
        #expect(store.thumbnail(for: moved) == Data("THUMB".utf8))

        #expect(store.isVideo(moved, inList: archive.id))
        #expect(!store.isVideo(original, inList: archive.id))
        let listFeed = store.feed(.customList(archive.id))
        #expect(listFeed.count == 1)
        #expect(listFeed.first?.videoPath == normalized(moved))
        #expect(listFeed.first?.viewCount == 2)
        #expect(listFeed.first?.isLiked == true)
        #expect(listFeed.first?.rating == 5)

        // The migration was persisted, not just held in memory.
        let other = folder.makeStore(secrets: InMemorySecretStore())
        try other.login(username: "emre", password: "pass123")
        #expect(other.stats(for: moved)?.viewCount == 2)
        #expect(other.lists().first?.videoPaths == [normalized(moved)])
    }
}
