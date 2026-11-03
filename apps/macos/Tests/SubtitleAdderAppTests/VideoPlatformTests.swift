import EngineKit
import Foundation
import LibraryKit
import Synchronization
import Testing
@testable import SubtitleAdderApp

/// The video platform's model: accounts, the feed and its filters, lists, engagement, watching and live updates.
@MainActor
@Suite(.serialized) struct VideoPlatformTests {
    // MARK: - Accounts

    @Test func registerLogoutLoginAndDeleteFlow() async throws {
        let sandbox = try PlatformSandbox()
        let model = sandbox.model()
        await model.start()
        #expect(!model.isLoggedIn)
        #expect(model.profileTitle == L("Profil / Giriş"))

        #expect(await model.register(username: "  Ayşe ", password: "pw1"))
        #expect(model.username == "Ayşe")
        #expect(model.profileTitle == "Ayşe")
        #expect(model.registeredUsernames == ["Ayşe"])
        #expect(model.authError == nil)

        model.logout()
        await model.settle()
        #expect(!model.isLoggedIn)

        // A wrong password says so and does not log in; the name matches ignoring case and keeps its registered casing.
        #expect(await model.login(username: "ayşe", password: "nope") == false)
        #expect(model.authFailure == .invalidCredentials)
        #expect(model.authError == ProfileError.invalidCredentials.message)
        #expect(!model.isLoggedIn)
        #expect(await model.login(username: "AYŞE", password: "pw1"))
        #expect(model.username == "Ayşe")
        #expect(model.authFailure == nil)

        // Deleting needs the right password; then the profile is gone and the session with it.
        #expect(await model.deleteProfile(username: "Ayşe", password: "wrong") == false)
        #expect(model.authFailure == .invalidCredentials)
        #expect(model.isLoggedIn)
        #expect(await model.deleteProfile(username: "Ayşe", password: "pw1"))
        #expect(!model.isLoggedIn)
        #expect(model.registeredUsernames.isEmpty)
        #expect(await model.login(username: "Ayşe", password: "pw1") == false)
    }

    @Test func authErrorsAreTypedAndLocalized() async throws {
        let sandbox = try PlatformSandbox()
        let model = sandbox.model()
        await model.start()

        #expect(await model.register(username: "  ", password: "pw") == false)
        #expect(model.authFailure == .emptyUsername)
        #expect(await model.register(username: "Ali", password: "") == false)
        #expect(model.authFailure == .emptyPassword)
        #expect(await model.login(username: "", password: "") == false)
        #expect(model.authFailure == .missingCredentials)

        #expect(await model.register(username: "Ali", password: "pw"))
        model.logout()
        await model.settle()
        #expect(await model.register(username: "ali", password: "other") == false)
        #expect(model.authFailure == .usernameTaken)
        #expect(model.authError == ProfileError.usernameTaken.message)
        // The next attempt starts clean.
        #expect(await model.login(username: "Ali", password: "pw"))
        #expect(model.authError == nil)
    }

    @Test func everyProfileErrorHasItsOwnMessage() {
        let errors: [ProfileError] = [
            .emptyUsername, .emptyPassword, .missingCredentials, .usernameTaken, .invalidCredentials, .vaultUnreadable,
            .emptyListName, .duplicateListName, .lockUnavailable, .writeFailed(errno: 13), .corruptFileNotPreserved,
            .unsupportedFileVersion(9), .fileUnreadable, .cryptoFailure,
        ]
        let messages = errors.map(\.message)
        #expect(messages.allSatisfy { !$0.isEmpty })
        #expect(Set(messages).count == messages.count)
        // The numbers the user may need are in the text.
        #expect(ProfileError.writeFailed(errno: 13).message.contains("13"))
        #expect(ProfileError.unsupportedFileVersion(9).message.contains("9"))
        // The text says the file comes from another version of the app and cannot be opened here.
        let refusal = UILanguage.en.string(
            "Profil dosyası uygulamanın daha yeni veya başka bir sürümü tarafından yazılmış (sürüm %lld); burada açılamaz ve değiştirilmeyecek.",
            in: .module, [9]
        )
        #expect(refusal.contains("newer or different version") && refusal.contains("cannot be opened here"))
    }

    @Test func credentialsWorkRunsOffTheMainThread() async throws {
        // Registering stores the vault key in the secret store, which happens inside the PBKDF2 call.
        let sandbox = try PlatformSandbox()
        let recorder = ThreadRecordingSecrets()
        let model = sandbox.model(store: sandbox.makeStore(secrets: recorder))
        await model.start()
        #expect(await model.register(username: "Ali", password: "pw"))
        #expect(!recorder.touched.isEmpty)
        #expect(recorder.touched.allSatisfy { !$0 })
        let thread = recorder.touched.count
        model.logout()
        await model.settle()
        #expect(await model.login(username: "Ali", password: "pw"))
        #expect(recorder.touched.count > thread)
        #expect(recorder.touched.allSatisfy { !$0 })
    }

    // MARK: - Feed

    @Test func filtersAndSearchWorkOnTheProfilesVideos() async throws {
        let sandbox = try PlatformSandbox()
        let a = try sandbox.file("alpha.mp4"), b = try sandbox.file("beta.mp4"), c = try sandbox.file("gamma.mp4")
        let model = await sandbox.loggedInModel()

        for url in [a, b, a, c] {
            model.playerVideoChanged(to: url, countsAsOpen: true)
            await model.settle()
        }
        model.setLiked(true, for: c)
        model.setRating(5, for: b)
        await model.settle()

        model.filter = .history
        await model.settle()
        // Most recently watched first.
        #expect(model.cards.map(\.title) == ["gamma", "alpha", "beta"])
        #expect(model.counts.watched == 3)
        #expect(model.counts.liked == 1)
        #expect(model.counts.rated == 1)

        model.filter = .liked
        await model.settle()
        #expect(model.cards.map(\.title) == ["gamma"])
        model.filter = .topRated
        await model.settle()
        #expect(model.cards.map(\.title) == ["beta"])
        model.filter = .mostWatched
        await model.settle()
        #expect(model.cards.first?.title == "alpha")
        #expect(model.cards.first?.viewCount == 2)

        // The search narrows the tab that is open, and emptying the field brings the whole tab back.
        model.filter = .history
        model.searchText = "et"
        model.submitSearch()
        await model.settle()
        #expect(model.cards.map(\.title) == ["beta"])
        model.searchText = ""
        await model.settle()
        #expect(model.searchQuery.isEmpty)
        #expect(model.cards.count == 3)

        // Nothing matches: an empty feed, with the query to say so.
        model.searchText = "zzz"
        model.submitSearch()
        await model.settle()
        #expect(model.cards.isEmpty)
        #expect(model.searchQuery == "zzz")
        model.clearSearch()
        await model.settle()
        #expect(model.searchText.isEmpty)
        #expect(model.cards.count == 3)
    }

    @Test func loggedOutPersonalTabsAskToSignInUnlessSearching() async throws {
        let sandbox = try PlatformSandbox()
        try sandbox.file("alpha.mp4")
        try sandbox.file("beta.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = sandbox.model(index: index)
        model.isHubActive = true
        await model.start()

        #expect(model.filter == .all)
        #expect(!model.showsAuthCard)
        #expect(model.cards.map(\.title) == ["alpha", "beta"])

        model.filter = .history
        await model.settle()
        #expect(model.showsAuthCard)
        #expect(model.cards.isEmpty)

        // A search looks through the indexed videos, so the sign-in card gives way to its result.
        model.searchText = "bet"
        model.submitSearch()
        await model.settle()
        #expect(!model.showsAuthCard)
        #expect(model.cards.map(\.title) == ["beta"])

        // The header's button asks for the card on "Keşfet" too.
        model.clearSearch()
        model.filter = .all
        model.requestSignIn()
        #expect(model.showsAuthCard)
        model.filter = .all
        #expect(model.showsAuthCard)
        model.filter = .liked
        model.filter = .all
        #expect(!model.showsAuthCard)

        // Pressing the pill of the tab that is already open puts the card away too.
        model.requestSignIn()
        #expect(model.showsAuthCard)
        model.select(.all)
        #expect(!model.showsAuthCard)
    }

    @Test func discoverShowsIndexedVideosAndOrdersLoggedInOnesByEngagement() async throws {
        let sandbox = try PlatformSandbox()
        let beta = try sandbox.file("beta.mp4")
        try sandbox.file("alpha.mp4")
        try sandbox.file("sub/gamma.mkv")
        let index = sandbox.makeIndex()
        await index.crawl(from: sandbox.media, maxDepth: 2)

        // Logged out: every indexed video, by name.
        let model = sandbox.model(index: index)
        model.isHubActive = true
        await model.start()
        #expect(model.cards.map(\.title) == ["alpha", "beta", "gamma"])

        // Logged in: the watched one first, the rest in the store's order.
        #expect(await model.register(username: "Ali", password: "pw"))
        model.playerVideoChanged(to: beta, countsAsOpen: true)
        await model.settle()
        #expect(model.cards.map(\.title) == ["beta", "alpha", "gamma"])
        #expect(model.cards.first?.viewCount == 1)
    }

    @Test func discoverTakesAtMostTheConfiguredNumberOfIndexedFiles() async throws {
        let sandbox = try PlatformSandbox()
        for name in ["alpha", "bravo", "charlie", "delta"] { try sandbox.file("\(name).mp4") }
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = sandbox.model(index: index, services: sandbox.services(discoveredLimit: 2))
        model.isHubActive = true
        await model.start()
        #expect(model.cards.count == 2)

        // A search reaches past the newest few: the best name matches are asked for as well.
        model.searchText = "delt"
        model.submitSearch()
        await model.settle()
        #expect(model.cards.map(\.title) == ["delta"])
    }

    @Test func discoverFollowsTheIndexAndStaysQuietOffScreen() async throws {
        let sandbox = try PlatformSandbox()
        try sandbox.file("alpha.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = sandbox.model(index: index)
        model.isHubActive = true
        await model.start()
        #expect(model.cards.map(\.title) == ["alpha"])

        try sandbox.file("beta.mp4")
        await index.scan(directory: sandbox.media)
        #expect(await waitFor { model.cards.count == 2 })

        // Off screen no cards are built, and coming back brings them up to date.
        model.isHubActive = false
        await model.settle()
        try sandbox.file("gamma.mp4")
        await index.scan(directory: sandbox.media)
        try await Task.sleep(for: .milliseconds(150))
        #expect(model.cards.count == 2)
        model.isHubActive = true
        await model.settle()
        #expect(model.cards.count == 3)
    }

    @Test func videosOnAnUnpluggedVolumeAreHiddenUntilItIsBack() async throws {
        let sandbox = try PlatformSandbox()
        let here = try sandbox.file("here.mp4")
        let model = await sandbox.loggedInModel()

        // Records for a file on a disk and for one that is here, both watched.
        let away = URL(filePath: "/Volumes/Archive/Films/away.mp4")
        for url in [away, here] {
            model.playerVideoChanged(to: url, countsAsOpen: true)
            await model.settle()
        }
        model.filter = .history
        await model.settle()
        #expect(model.cards.map(\.title) == ["here"])
        #expect(model.counts.watched == 1)

        sandbox.mount("/Volumes/Archive")
        await model.refresh()
        // Most recently watched first.
        #expect(model.cards.map(\.title) == ["here", "away"])
        #expect(model.counts.watched == 2)

        sandbox.unmount("/Volumes/Archive")
        await model.refresh()
        #expect(model.cards.map(\.title) == ["here"])

        // A folder that is gone counts as unreachable too.
        let gone = sandbox.media.appending(path: "removed/clip.mp4")
        _ = try? model.store.recordWatch(gone)
        await model.refresh()
        #expect(model.cards.map(\.title) == ["here"])
    }

    @Test func subtitleBadgesComeFromOneListingPerFolderPerRefresh() async throws {
        let sandbox = try PlatformSandbox()
        try sandbox.file("Film.mp4")
        try sandbox.file("Film.tr.srt")
        try sandbox.file("Other.mp4")
        try sandbox.file("Clip.mov")
        try sandbox.file("Clip.vtt")
        try sandbox.file("deep/Third.mp4")
        let index = sandbox.makeIndex()
        await index.crawl(from: sandbox.media, maxDepth: 2)
        let model = sandbox.model(index: index)
        model.isHubActive = true
        await model.start()

        let badges = Dictionary(uniqueKeysWithValues: model.cards.map { ($0.title, $0.subtitleExtension) })
        #expect(badges["Film"] == "srt")
        #expect(badges["Clip"] == "vtt")
        #expect(badges["Other"] == .some(nil))
        #expect(badges["Third"] == .some(nil))
        // Four videos in two folders: two listings per refresh, not four.
        #expect(model.lastBadgeListings == 2)
        let folder = ExplorerPaths.parent(of: model.cards.first { $0.title == "Film" }!.path)!
        let listed = sandbox.listingCount(of: folder)
        #expect(listed >= 1)

        // The next refresh looks again (a subtitle may have been written meanwhile), once for the folder.
        try sandbox.file("Other.srt")
        await model.refresh()
        #expect(model.cards.first { $0.title == "Other" }?.subtitleExtension == "srt")
        #expect(model.lastBadgeListings == 2)
        #expect(sandbox.listingCount(of: folder) == listed + 1)
    }

    @Test func cardsTellWhatWasWatchedWithoutRepeatingThemselves() async throws {
        let sandbox = try PlatformSandbox()
        let seen = try sandbox.file("seen.mp4"), fresh = try sandbox.file("fresh.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = await sandbox.loggedInModel(index: index)
        model.playerVideoChanged(to: seen, countsAsOpen: true)
        await model.settle()

        let unwatched = try #require(model.cards.first { $0.url == fresh })
        #expect(unwatched.watchBadges == [L("Henüz izlenmedi")])
        let watched = try #require(model.cards.first { $0.url == seen })
        #expect(watched.watchBadges.count == 2)
        #expect(watched.watchBadges[0] == L("%lld görüntüleme", 1))
        #expect(!watched.watchBadges.contains(L("Henüz izlenmedi")))
        #expect(watched.title == "seen")
        #expect(watched.extensionLabel == "MP4")
        #expect(watched.folderLine == "Media (\(sandbox.media.path))")
    }

    // MARK: - Watching

    @Test func aWatchIsRecordedForOpenedVideosButNotForPlaylistOnes() async throws {
        let sandbox = try PlatformSandbox()
        let opened = try sandbox.file("opened.mp4"), queued = try sandbox.file("queued.mp4")
        let model = sandbox.model()
        model.isHubActive = true
        await model.start()

        // Logged out nothing is recorded; logging in later does not make up for it.
        model.playerVideoChanged(to: opened, countsAsOpen: true)
        await model.settle()
        #expect(await model.register(username: "Ali", password: "pw"))
        #expect(model.store.stats(for: opened) == nil)

        model.playerVideoChanged(to: nil, countsAsOpen: false)
        model.playerVideoChanged(to: opened, countsAsOpen: true)
        await model.settle()
        #expect(model.store.stats(for: opened)?.viewCount == 1)
        // The same video reported again is not a second view.
        model.playerVideoChanged(to: opened, countsAsOpen: true)
        await model.settle()
        #expect(model.store.stats(for: opened)?.viewCount == 1)

        // A playlist run opens videos without counting them, and gives the editor's video back without counting it.
        model.playerVideoChanged(to: nil, countsAsOpen: false)
        model.playerVideoChanged(to: queued, countsAsOpen: false)
        await model.settle()
        #expect(model.store.stats(for: queued) == nil)
        model.playerVideoChanged(to: opened, countsAsOpen: false)
        await model.settle()
        #expect(model.store.stats(for: opened)?.viewCount == 1)

        // Opening a video again after something else counts again.
        model.playerVideoChanged(to: queued, countsAsOpen: true)
        await model.settle()
        #expect(model.store.stats(for: queued)?.viewCount == 1)
    }

    @Test func openingAVideoDoesNotWaitForTheProfile() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        // The call returns at once, before the watch is recorded: it is only queued.
        model.playerVideoChanged(to: video, countsAsOpen: true)
        #expect(model.playerVideo == video)
        #expect(model.engagement.url == video)
        #expect(!model.engagement.isLoaded)
        await model.settle()
        #expect(model.engagement.isLoaded)
        #expect(model.engagement.views == 1)
    }

    @Test func playbackProgressIsReportedEveryIntervalAndOnPause() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()

        let position = Mutex(PlaybackPosition(seconds: 42, duration: 100))
        model.playbackSampler = { position.withLock { $0 } }
        model.handle(.started)
        #expect(await waitFor { model.store.stats(for: video)?.lastPositionSeconds == 42 })
        position.withLock { $0.seconds = 60 }
        #expect(await waitFor { model.store.stats(for: video)?.lastPositionSeconds == 60 })

        // Pausing reports at once and stops the clock. It is on disk: a second store reads the file.
        model.handle(.paused(PlaybackPosition(seconds: 70, duration: 100)))
        await model.settle()
        #expect(model.store.stats(for: video)?.lastPositionSeconds == 70)
        #expect(sandbox.makeStore().stats(for: video)?.lastPositionSeconds == 70)
        position.withLock { $0.seconds = 90 }
        try await Task.sleep(for: .milliseconds(150))
        #expect(model.store.stats(for: video)?.lastPositionSeconds == 70)

        // Leaving the video reports where it was, to that video, and neither of these counts a view.
        model.handle(.leaving(PlaybackPosition(seconds: 80, duration: 100)))
        await model.settle()
        #expect(model.store.stats(for: video)?.lastPositionSeconds == 80)
        #expect(sandbox.makeStore().stats(for: video)?.lastPositionSeconds == 80)
        #expect(model.store.stats(for: video)?.viewCount == 1)
        #expect(model.store.stats(for: video)?.durationSeconds == 100)

        // No known length, nothing to report.
        model.handle(.paused(PlaybackPosition(seconds: 5, duration: 0)))
        await model.settle()
        #expect(model.store.stats(for: video)?.lastPositionSeconds == 80)
    }

    @Test func progressOfAPlaylistVideoIsNotRecorded() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("queued.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: nil, countsAsOpen: false)
        model.handle(.paused(PlaybackPosition(seconds: 30, duration: 60)))
        await model.settle()
        #expect(model.store.stats(for: video) == nil)
    }

    @Test func progressAndThumbnailsDoNotRebuildTheFeed() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        model.filter = .history
        await model.settle()
        let before = model.cards

        // Each of these is a write the store raises no change for.
        model.handle(.paused(PlaybackPosition(seconds: 10, duration: 100)))
        _ = try model.store.setThumbnail(Data(repeating: 7, count: 100), for: video)
        await model.settle()
        try await Task.sleep(for: .milliseconds(100))
        await model.settle()
        // The card still says what it said: progress is picked up by the next refresh, not forced by the write.
        #expect(model.cards == before)
    }

    // MARK: - Lists

    @Test func listsCanBeCreatedRenamedAndDeleted() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()

        #expect(await model.createList(named: "  Filmler "))
        #expect(model.lists.map(\.name) == ["Filmler"])
        let films = try #require(model.lists.first)
        // The new list is on show.
        #expect(model.filter == .customList(films.id))
        #expect(model.selectedList?.id == films.id)

        // A name that is blank or already taken (ignoring case and diacritics) says why instead of failing silently.
        #expect(await model.createList(named: "   ") == false)
        #expect(model.listFailure == .emptyListName)
        #expect(await model.createList(named: "FİLMLER") == false)
        #expect(model.listFailure == .duplicateListName)
        #expect(model.listError == ProfileError.duplicateListName.message)
        model.clearListFailure()
        #expect(model.listError == nil)
        #expect(model.lists.count == 1)

        #expect(await model.createList(named: "Diziler"))
        #expect(model.lists.map(\.name) == ["Filmler", "Diziler"])
        #expect(await model.renameList(id: films.id, to: "Diziler") == false)
        #expect(model.listFailure == .duplicateListName)
        #expect(await model.renameList(id: films.id, to: "  ") == false)
        #expect(model.listFailure == .emptyListName)
        #expect(await model.renameList(id: films.id, to: "Sinema"))
        #expect(model.listFailure == nil)
        #expect(model.lists.map(\.name) == ["Sinema", "Diziler"])

        model.toggleList(films.id, for: video)
        await model.settle()
        model.filter = .customList(films.id)
        await model.settle()
        #expect(model.cards.map(\.title) == ["clip"])
        #expect(model.cards.first?.lists.map(\.name) == ["Sinema"])
        #expect(model.lists.first?.videoPaths.count == 1)

        // Deleting the list on show goes back to "Keşfet".
        model.deleteList(id: films.id)
        await model.settle()
        #expect(model.filter == .all)
        #expect(model.lists.map(\.name) == ["Diziler"])
    }

    @Test func quickCreateMakesAListWithAFreeNameAndAddsTheVideo() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()

        model.createListAndAdd(video)
        await model.settle()
        #expect(model.lists.map(\.name) == [L("Daha Sonra İzle")])
        #expect(model.lists[0].videoPaths.count == 1)
        #expect(model.isInList(model.lists[0].id, video) == false || model.engagement.url == nil)

        model.createListAndAdd(video)
        await model.settle()
        #expect(model.lists.map(\.name) == [L("Daha Sonra İzle"), "\(L("Listem")) 2"])
        // "Listem 4" taken by hand (three lists now, so "Listem 4" is the next number): the next quick list moves on
        // instead of failing on the duplicate.
        #expect(await model.createList(named: "\(L("Listem")) 4"))
        model.createListAndAdd(video)
        await model.settle()
        #expect(model.lists.map(\.name).contains("\(L("Listem")) 5"))
        model.createListAndAdd(video)
        await model.settle()
        #expect(model.lists.map(\.name).contains("\(L("Listem")) 6"))
        #expect(Set(model.lists.map(\.name)).count == model.lists.count)
        #expect(model.actionFailure == nil)
    }

    @Test func quickCreateNamesSkipTheTakenOnes() {
        var names = VideoPlatformModel.candidateNames(preferred: nil, base: "Listem", existing: ["A", "listem 3"])
        #expect(names.next() == "Listem 4")
        #expect(names.next() == "Listem 5")
        var first = VideoPlatformModel.candidateNames(preferred: "Daha Sonra İzle", base: "Listem", existing: [])
        #expect(first.next() == "Daha Sonra İzle")
        #expect(first.next() == "Listem 1")
        // The preferred name is skipped when it exists, ignoring case and diacritics.
        var taken = VideoPlatformModel.candidateNames(preferred: "Daha Sonra İzle", base: "Listem", existing: ["daha sonra izle"])
        #expect(taken.next() == "Listem 2")
    }

    @Test func clearingTheHistoryKeepsFavoritesAndLists() async throws {
        let sandbox = try PlatformSandbox()
        let kept = try sandbox.file("kept.mp4"), gone = try sandbox.file("gone.mp4")
        let model = await sandbox.loggedInModel()
        for url in [kept, gone] {
            model.playerVideoChanged(to: url, countsAsOpen: true)
            await model.settle()
        }
        model.setLiked(true, for: kept)
        await model.settle()
        model.filter = .history
        await model.settle()
        #expect(model.canClearHistory)
        #expect(model.cards.count == 2)

        model.clearHistory()
        await model.settle()
        #expect(model.cards.isEmpty)
        #expect(!model.canClearHistory)
        #expect(model.counts.watched == 0)
        #expect(model.counts.liked == 1)
        model.filter = .liked
        await model.settle()
        #expect(model.cards.map(\.title) == ["kept"])
        #expect(model.cards.first?.viewCount == 0)
        // The button only exists on the history tab.
        #expect(!model.canClearHistory)
    }

    // MARK: - Engagement

    @Test func engagementActionsChangeTheCardAndThePlayerBar() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = await sandbox.loggedInModel(index: index)
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        #expect(model.engagement.isLoaded)
        #expect(model.engagement.views == 1)
        #expect(!model.engagement.isLiked)

        // The bar and the card change at once; the profile write follows.
        model.toggleLike(video)
        #expect(model.engagement.isLiked)
        #expect(model.cards.first?.isLiked == true)
        model.setRating(4, for: video)
        #expect(model.engagement.rating == 4)
        #expect(model.cards.first?.rating == 4)
        await model.settle()
        let stats = try #require(model.store.stats(for: video))
        #expect(stats.isLiked)
        #expect(stats.rating == 4)
        #expect(model.engagement.isLiked && model.engagement.rating == 4)

        // Clicking the current star again (the view passes 0) clears the stars; the heart toggles back.
        model.setRating(0, for: video)
        model.toggleLike(video)
        await model.settle()
        #expect(model.store.stats(for: video)?.rating == 0)
        #expect(model.store.stats(for: video)?.isLiked == false)
        #expect(model.engagement.rating == 0)
        #expect(!model.engagement.isLiked)

        // Lists: the bar's checkmarks follow the membership.
        #expect(await model.createList(named: "Sonra"))
        let list = try #require(model.lists.first)
        model.toggleList(list.id, for: video)
        await model.settle()
        #expect(model.engagement.listIDs == [list.id])
        #expect(model.isInList(list.id, video))
        model.toggleList(list.id, for: video)
        await model.settle()
        #expect(model.engagement.listIDs.isEmpty)
        #expect(!model.isInList(list.id, video))
    }

    @Test func theBarIsEmptyWhileLoggedOutOrWithoutAVideo() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = sandbox.model()
        await model.start()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        #expect(model.engagement.url == video)
        #expect(!model.engagement.isLoaded)

        #expect(await model.register(username: "Ali", password: "pw"))
        #expect(model.engagement.isLoaded)
        model.playerVideoChanged(to: nil, countsAsOpen: false)
        #expect(model.engagement.url == nil)
        #expect(!model.engagement.isLoaded)
    }

    @Test(.enabled(if: getuid() != 0, "root ignores the read-only folder"))
    func aFailedActionIsReportedAndTheScreenReturnsToTheTruth() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        // The profiles file can no longer be written: its folder is read-only.
        chmod(sandbox.base.path, 0o555)
        defer { chmod(sandbox.base.path, 0o755) }
        model.setLiked(true, for: video)
        #expect(model.engagement.isLiked)
        await model.settle()
        #expect(model.actionFailure != nil)
        #expect(!model.engagement.isLiked)
        #expect(model.cards.first { $0.url == video }?.isLiked != true)
    }

    // MARK: - Live updates

    @Test func anotherInstancesChangesReachTheFeedAndTheHeader() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = sandbox.model(store: sandbox.makeStore(watches: true))
        model.isHubActive = true
        await model.start()
        #expect(await model.register(username: "Ali", password: "pw"))
        model.filter = .liked
        await model.settle()
        #expect(model.cards.isEmpty)

        // A second store on the same file stands for another window or process.
        // (It shares the keychain: logging in there without "remember" would log this instance out, as designed.)
        let other = sandbox.makeStore(watches: false)
        let loggedIn = await Task.detached { (try? other.login(username: "Ali", password: "pw", rememberMe: true)) != nil }.value
        #expect(loggedIn)
        _ = try other.setLiked(true, for: video)
        _ = try other.register(username: "Zed", password: "pw2", rememberMe: true)

        #expect(await waitFor(timeout: 8) { model.cards.map(\.title) == ["clip"] })
        #expect(model.counts.liked == 1)
        #expect(await waitFor { model.registeredUsernames.contains("Zed") })
    }

    @Test func aLoginRestoredFromTheKeychainShowsUpWithoutAnyAction() async throws {
        let sandbox = try PlatformSandbox()
        let first = sandbox.model()
        await first.start()
        #expect(await first.register(username: "Ali", password: "pw", ))
        // A new launch with the same file and keychain is logged in from the start.
        let second = sandbox.model()
        await second.start()
        #expect(second.username == "Ali")
        #expect(second.isLoggedIn)
    }

    @Test func aProfileDeletedElsewhereEndsTheSessionAndLeavesTheListsTab() async throws {
        let sandbox = try PlatformSandbox()
        let model = sandbox.model(store: sandbox.makeStore(watches: true))
        model.isHubActive = true
        await model.start()
        #expect(await model.register(username: "Ali", password: "pw"))
        #expect(await model.createList(named: "Filmler"))
        #expect(model.selectedList != nil)

        let other = sandbox.makeStore(watches: false)
        try await Task.detached { try other.deleteProfile(username: "Ali", password: "pw") }.value
        #expect(await waitFor(timeout: 8) { !model.isLoggedIn })
        #expect(await waitFor { model.filter == .all })
        #expect(model.registeredUsernames.isEmpty)
    }

    @Test func rememberMeAndLastUsernameComeFromTheProfile() async throws {
        let sandbox = try PlatformSandbox()
        let model = sandbox.model()
        await model.start()
        #expect(model.rememberMe)
        model.rememberMe = false
        #expect(await model.register(username: "Ali", password: "pw"))
        #expect(model.lastUsername == "Ali")
        #expect(model.store.rememberMe == false)
        // Not remembered: a new launch is logged out.
        let next = sandbox.model()
        await next.start()
        #expect(!next.isLoggedIn)
        #expect(next.lastUsername == "Ali")
        #expect(next.registeredUsernames == ["Ali"])
    }

    // MARK: - Text

    @Test func emptyFeedsSayWhatIsMissing() {
        #expect(EmptyFeedText(filter: .history, search: "", listName: nil).title == L("Henüz izleme geçmişi yok"))
        #expect(EmptyFeedText(filter: .liked, search: "", listName: nil).title == L("Henüz favorilere eklenen video yok"))
        #expect(EmptyFeedText(filter: .topRated, search: "", listName: nil).title == L("Henüz puanlanan video yok"))
        #expect(EmptyFeedText(filter: .mostWatched, search: "", listName: nil).title == L("Henüz izlenen video yok"))
        #expect(EmptyFeedText(filter: .all, search: "", listName: nil).title == L("Gösterilecek video bulunamadı"))
        let list = EmptyFeedText(filter: .customList("x"), search: "", listName: "Filmler")
        #expect(list.title == L("\"%@\" listesinde henüz video yok", "Filmler"))
        // A search that found nothing names the query, whatever the tab.
        let search = EmptyFeedText(filter: .history, search: "xyz", listName: nil)
        #expect(search.title == L("\"%@\" için eşleşen video bulunamadı", "xyz"))
    }

    @Test func formattingHelpers() {
        #expect(PathText.durationText(0).isEmpty)
        #expect(PathText.durationText(.nan).isEmpty)
        #expect(PathText.durationText(65) == "1:05")
        #expect(PathText.durationText(3725) == "1:02:05")
        #expect(PathText.folderLine("/Users/ali/Movies/") == "Movies (/Users/ali/Movies)")
        #expect(PathText.folderLine("/") == "/")
        #expect(PathText.folderLine("").isEmpty)
    }
}

/// Records on which thread the store reaches into the secret store, which it does inside the PBKDF2 calls.
private final class ThreadRecordingSecrets: SecretStore, @unchecked Sendable {
    private let inner = InMemorySecretStore()
    private let lock = NSLock()
    private var calls: [Bool] = []

    /// For each call, whether it ran on the main thread.
    var touched: [Bool] { lock.withLock { calls } }

    private func note() { lock.withLock { calls.append(Thread.isMainThread) } }

    func data(for account: String) -> Data? {
        note()
        return inner.data(for: account)
    }

    @discardableResult func set(_ data: Data, for account: String) -> Bool {
        note()
        return inner.set(data, for: account)
    }

    func remove(_ account: String) {
        note()
        inner.remove(account)
    }
}
