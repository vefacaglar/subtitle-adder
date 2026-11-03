import AVFoundation
import AppKit
import EngineKit
import Foundation
import LibraryKit
import Synchronization
import Testing
@testable import SubtitleAdderApp

/// Holds the feed's directory listings, so a build stays under way for as long as a test wants.
private final class ListingGate: Sendable {
    private struct State {
        var closed = false
        var entered = 0
    }

    private let state = Mutex(State())

    func close() { state.withLock { $0.closed = true } }
    func open() { state.withLock { $0.closed = false } }
    /// How many listings started while the gate was closed.
    var entered: Int { state.withLock { $0.entered } }

    /// Blocks the calling (queue) thread while the gate is closed.
    func pass() {
        let wait = state.withLock { state -> Bool in
            if state.closed { state.entered += 1 }
            return state.closed
        }
        guard wait else { return }
        while state.withLock({ $0.closed }) { usleep(2_000) }
    }
}

/// A one-shot latch that an action can wait on.
private actor Latch {
    private var released = false
    func wait() async {
        while !released { try? await Task.sleep(for: .milliseconds(5)) }
    }
    func release() { released = true }
}

/// The fixes of the platform review: edits that never flicker, a player bar that reads its own video, a video opened
/// before the login is restored, tabs that never show another tab's cards, windows that let go of their player, and
/// the list, storage and logout details.
@MainActor
@Suite(.serialized) struct PlatformReviewFixTests {
    private func gatedServices(_ sandbox: PlatformSandbox, gate: ListingGate) -> PlatformServices {
        var services = sandbox.services()
        let listing = services.listDirectory
        services.listDirectory = { path in
            gate.pass()
            return listing(path)
        }
        return services
    }

    // MARK: Edits

    @Test func rapidTapsNeverFlickerBackToAnOlderValue() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = await sandbox.loggedInModel(index: index)
        model.select(.all)
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        #expect(model.cards.first?.rating == 0)

        // Two taps; the second write is held back behind a latch, so the first has finished and its refresh has read the
        // profile (3 stars) while the second (5 stars) is still on its way.
        let latch = Latch()
        model.setRating(3, for: video)
        model.enqueue { await latch.wait() }
        model.setRating(5, for: video)
        #expect(await waitFor { model.store.stats(for: video)?.rating == 3 })
        await model.refresh()
        await model.refresh()
        #expect(model.cards.first?.rating == 5)
        #expect(model.engagement.rating == 5)

        await latch.release()
        await model.settle()
        #expect(model.store.stats(for: video)?.rating == 5)
        #expect(model.cards.first?.rating == 5)
        #expect(model.engagement.rating == 5)
    }

    @Test func aLikeTapStaysOnScreenWhileAnEarlierWriteIsStillUnderWay() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = await sandbox.loggedInModel(index: index)
        await model.settle()

        let latch = Latch()
        model.setLiked(true, for: video)
        model.enqueue { await latch.wait() }
        model.setLiked(false, for: video)
        model.setLiked(true, for: video)
        #expect(await waitFor { model.store.stats(for: video)?.isLiked == true })
        await model.refresh()
        #expect(model.cards.first?.isLiked == true)
        await latch.release()
        await model.settle()
        #expect(model.cards.first?.isLiked == true)
        #expect(model.store.stats(for: video)?.isLiked == true)
    }

    @Test func anEditEndsWithItsLastWriteAndTheProfileHasTheFinalSay() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = await sandbox.loggedInModel(index: index)
        model.setRating(4, for: video)
        await model.settle()
        // Another window changes it: with no edit pending, the snapshot is the truth.
        _ = try model.store.setRating(2, for: video)
        await model.refresh()
        #expect(model.cards.first?.rating == 2)
    }

    // MARK: The player bar

    @Test func thePlayerBarAndItsListMenuReadTheirOwnVideoNotAStaleCard() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = await sandbox.loggedInModel(index: index)
        #expect(await model.createList(named: "Sonra"))
        model.select(.all)
        await model.settle()
        let list = try #require(model.lists.first)
        #expect(model.cards.contains { $0.url == video })

        // The user goes to the player: the hub is off screen, so its cards stop being rebuilt.
        model.isHubActive = false
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()

        model.setInList(true, id: list.id, for: video)
        await model.settle()
        #expect(model.engagement.listIDs == [list.id])
        #expect(model.isInList(list.id, video))
        // The card the hub kept still says "not in the list"; nothing reads it for the open video.
        #expect(model.cards.first { $0.url == video }?.lists.isEmpty == true)

        // Choosing the checked item sends "off", exactly once, however often the menu is rebuilt.
        model.setInList(false, id: list.id, for: video)
        model.setInList(false, id: list.id, for: video)
        await model.settle()
        #expect(!model.store.isVideo(video, inList: list.id))
        #expect(!model.isInList(list.id, video))
        model.toggleList(list.id, for: video)
        await model.settle()
        #expect(model.store.isVideo(video, inList: list.id))

        _ = try model.store.setLiked(true, for: video)
        await model.refresh()
        #expect(model.isLiked(video))
        #expect(model.cards.first { $0.url == video }?.isLiked == false)
    }

    // MARK: Opening before the login is known

    @Test func aVideoOpenedBeforeTheRestoredLoginIsKnownIsCounted() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4"), other = try sandbox.file("other.mp4")
        let playlist = try sandbox.file("queued.mp4")
        let first = sandbox.model()
        await first.start()
        #expect(await first.register(username: "Ali", password: "pw"))

        // A new launch with the login remembered; Finder hands a file over before the login is restored.
        let second = sandbox.model()
        second.playerVideoChanged(to: video, countsAsOpen: true)
        #expect(!second.isLoggedIn)
        await second.start()
        await second.settle()
        #expect(second.isLoggedIn)
        #expect(second.store.stats(for: video)?.viewCount == 1)

        // Replaced by another video before the answer came: only the one still open counts.
        let third = sandbox.model()
        third.playerVideoChanged(to: video, countsAsOpen: true)
        third.playerVideoChanged(to: other, countsAsOpen: true)
        await third.start()
        await third.settle()
        #expect(third.store.stats(for: other)?.viewCount == 1)
        // Replaced before the answer came: it was not counted a second time.
        #expect(third.store.stats(for: video)?.viewCount == 1)
        // A playlist run's video is never counted.
        let fourth = sandbox.model()
        fourth.playerVideoChanged(to: nil, countsAsOpen: false)
        fourth.playerVideoChanged(to: playlist, countsAsOpen: false)
        await fourth.start()
        await fourth.settle()
        #expect(fourth.store.stats(for: playlist) == nil)
    }

    @Test func aVideoOpenedBeforeStartIsNotCountedWithoutALogin() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let first = sandbox.model()
        await first.start()
        first.rememberMe = false
        #expect(await first.register(username: "Ali", password: "pw"))
        let second = sandbox.model()
        second.playerVideoChanged(to: video, countsAsOpen: true)
        await second.start()
        await second.settle()
        #expect(!second.isLoggedIn)
        #expect(await second.login(username: "Ali", password: "pw"))
        await second.settle()
        #expect(second.store.stats(for: video) == nil)
    }

    @Test func aFailingWatchRecordIsSilent() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        // The profiles file can no longer be written: its folder is read-only.
        chmod(sandbox.base.path, 0o555)
        defer { chmod(sandbox.base.path, 0o755) }
        guard getuid() != 0 else { return }
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        // Bookkeeping like the progress: no alert on every open.
        #expect(model.actionFailure == nil)
    }

    // MARK: Tabs

    @Test func switchingTabsAtOnceShowsTheLoadingStateNotTheOldTabsCards() async throws {
        let sandbox = try PlatformSandbox()
        let a = try sandbox.file("a.mp4"), b = try sandbox.file("b.mp4")
        let gate = ListingGate()
        let model = await sandbox.loggedInModel(services: gatedServices(sandbox, gate: gate))
        for url in [a, b] {
            model.playerVideoChanged(to: url, countsAsOpen: true)
            await model.settle()
        }
        model.setLiked(true, for: b)
        await model.settle()
        model.filter = .history
        await model.settle()
        #expect(model.cards.count == 2)

        gate.close()
        model.filter = .liked
        // At once, before any build has answered: no card of "İzleme geçmişi" under the "Favorilerim" pill.
        #expect(model.cards.isEmpty)
        #expect(model.isRefreshing)
        #expect(await waitFor { gate.entered > 0 })
        #expect(model.cards.isEmpty)
        #expect(model.isRefreshing)

        gate.open()
        await model.settle()
        #expect(model.cards.map(\.title) == ["b"])
        #expect(!model.isRefreshing)

        // A new query is the same: the old results go while it is looked for.
        gate.close()
        model.searchText = "zzz"
        model.submitSearch()
        #expect(model.cards.isEmpty)
        gate.open()
        await model.settle()
        model.clearSearch()
        #expect(model.cards.isEmpty || model.isRefreshing)
        await model.settle()
        #expect(model.cards.map(\.title) == ["b"])
    }

    @Test func aBuildNobodyWantsAnyMoreStopsBeforeOpeningTheLibrary() async throws {
        let sandbox = try PlatformSandbox()
        let store = sandbox.makeStore()
        try store.register(username: "Ali", password: "pw")
        var paths: [String] = []
        var urls: [URL] = []
        for index in 0..<30 {
            let url = try sandbox.file("f\(index).mp4", content: "content \(index) \(UUID().uuidString)")
            urls.append(url)
            paths.append(url.path)
        }
        let request = FeedRequest(filter: .all, search: "", discoveredPaths: paths, wantsFeed: true, playerVideo: nil)
        let wanted = Mutex(false)
        let snapshot = PlatformFeedBuilder.snapshot(
            store: store, request: request, services: sandbox.services(), shouldContinue: { wanted.withLock { $0 } })
        #expect(snapshot.cards == nil)
        // Not one file was read for its fingerprint.
        #expect(urls.allSatisfy { VideoFingerprint.cachedFingerprint(ofNormalizedPath: VideoFingerprint.normalizedPath($0)) == nil })

        wanted.withLock { $0 = true }
        let full = PlatformFeedBuilder.snapshot(
            store: store, request: request, services: sandbox.services(), shouldContinue: { wanted.withLock { $0 } })
        #expect(full.cards?.count == 30)
    }

    @Test func aSnapshotBuiltBeforeALogoutIsNeverApplied() async throws {
        let sandbox = try PlatformSandbox()
        try sandbox.file("a.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let gate = ListingGate()
        let model = await sandbox.loggedInModel(index: index, services: gatedServices(sandbox, gate: gate))
        await model.settle()
        #expect(model.isLoggedIn)

        // A build reads the profile while it is logged in and is then held up building the cards.
        gate.close()
        model.scheduleRefresh()
        #expect(await waitFor { gate.entered > 0 })
        // The login ends behind the model's back, and the held build is let go.
        try model.store.logout()
        gate.open()
        // What was read for the old login is not shown; the next build tells the truth.
        #expect(await waitFor { !model.isLoggedIn })
        await model.settle()
        #expect(model.username == nil)
    }

    @Test func loggedOutBuildsDoNotOpenFiles() async throws {
        let sandbox = try PlatformSandbox()
        let urls = try (0..<4).map { try sandbox.file("clip\($0).mp4", content: "unique \($0) \(UUID().uuidString)") }
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = sandbox.model(index: index)
        model.isHubActive = true
        await model.start()
        #expect(model.cards.count == 4)
        func fingerprinted(_ url: URL) -> Bool {
            VideoFingerprint.cachedFingerprint(ofNormalizedPath: VideoFingerprint.normalizedPath(url)) != nil
        }
        #expect(!urls.contains(where: fingerprinted))

        // Logged in, the same files are followed by content.
        #expect(await model.register(username: "Ali", password: "pw"))
        await model.refresh()
        #expect(urls.allSatisfy(fingerprinted))
    }

    // MARK: Own actions

    @Test func anOwnActionBuildsTheFeedOnce() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = await sandbox.loggedInModel(index: index)
        await model.settle()
        let firstCard = try #require(model.cards.first)
        let folder = try #require(ExplorerPaths.parent(of: firstCard.path))
        let before = sandbox.listingCount(of: folder)

        model.setLiked(true, for: video)
        await model.settle()
        // A second build, from the store's own change signal, would be under way by now.
        try await Task.sleep(for: .milliseconds(300))
        await model.settle()
        #expect(sandbox.listingCount(of: folder) == before + 1)
        #expect(model.cards.first?.isLiked == true)
    }

    @Test func aRefreshDoesNotRewriteTheVaultBecauseProgressIsPending() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let index = sandbox.makeIndex()
        await index.scan(directory: sandbox.media)
        let model = await sandbox.loggedInModel(index: index)
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()

        _ = try model.store.updatePlaybackProgress(video, position: 20, duration: 100)
        #expect(model.store.hasUnsavedChanges)
        let before = try Data(contentsOf: sandbox.profilesFile)
        await model.refresh()
        await model.refresh()
        #expect(try Data(contentsOf: sandbox.profilesFile) == before)
        #expect(model.store.hasUnsavedChanges)
    }

    // MARK: Lists

    @Test func renamingAListThatIsGoneFailsAndNamesTheReason() async throws {
        let sandbox = try PlatformSandbox()
        let model = await sandbox.loggedInModel()
        #expect(await model.createList(named: "Filmler"))
        #expect(await model.renameList(id: "no-such-list", to: "Başka") == false)
        #expect(model.listFailure == .listNotFound)
        #expect(model.listError == ProfileError.listNotFound.message)
        // It belongs to the rename form of that list, not to the box where lists are created.
        #expect(model.renameListError(for: "no-such-list") == model.listError)
        #expect(model.createListError == nil)
        #expect(model.lists.map(\.name) == ["Filmler"])
    }

    @Test func aListFailureIsShownInOnePlaceOnly() async throws {
        let sandbox = try PlatformSandbox()
        let model = await sandbox.loggedInModel()
        #expect(await model.createList(named: "Filmler"))
        let films = try #require(model.lists.first)

        #expect(await model.createList(named: "  ") == false)
        #expect(model.createListError == ProfileError.emptyListName.message)
        #expect(model.renameListError(for: films.id) == nil)

        #expect(await model.renameList(id: films.id, to: "  ") == false)
        #expect(model.createListError == nil)
        #expect(model.renameListError(for: films.id) == ProfileError.emptyListName.message)
        // Another list's form has nothing to say about it.
        #expect(model.renameListError(for: "other") == nil)
        model.clearListFailure()
        #expect(model.listError == nil)
    }

    @Test func twoCreationsAtOnceMakeOneList() async throws {
        let sandbox = try PlatformSandbox()
        let model = await sandbox.loggedInModel()
        async let first = model.createList(named: "Aynı")
        async let second = model.createList(named: "Aynı")
        let results = await [first, second]
        #expect(results.filter { $0 }.count == 1)
        #expect(model.lists.map(\.name) == ["Aynı"])
        #expect(model.listFailure == nil || model.listFailure == .duplicateListName)
        // The guard lets go afterwards.
        #expect(await model.createList(named: "Sonraki"))
    }

    @Test func aFailedDeleteLeavesTheListOnShow() async throws {
        let sandbox = try PlatformSandbox()
        let model = await sandbox.loggedInModel()
        #expect(await model.createList(named: "Filmler"))
        let films = try #require(model.lists.first)
        #expect(model.filter == .customList(films.id))

        chmod(sandbox.base.path, 0o555)
        defer { chmod(sandbox.base.path, 0o755) }
        guard getuid() != 0 else { return }
        model.deleteList(id: films.id)
        await model.settle()
        #expect(model.actionFailure != nil)
        #expect(model.filter == .customList(films.id))
        #expect(model.lists.count == 1)

        chmod(sandbox.base.path, 0o755)
        model.actionFailure = nil
        model.deleteList(id: films.id)
        await model.settle()
        #expect(model.actionFailure == nil)
        #expect(model.filter == .all)
        #expect(model.lists.isEmpty)
    }

    @Test func listCountsAndCardsLeaveOutVideosThatAreNotThere() async throws {
        let sandbox = try PlatformSandbox()
        let here = try sandbox.file("here.mp4"), gone = try sandbox.file("gone.mp4")
        let away = URL(filePath: "/Volumes/Archive/Films/away.mp4")
        let model = await sandbox.loggedInModel()
        #expect(await model.createList(named: "Hepsi"))
        let list = try #require(model.lists.first)
        for url in [here, gone, away] { model.setInList(true, id: list.id, for: url) }
        await model.settle()
        // The disk of "away" is unplugged: two of three are visible.
        #expect(model.lists.first?.videoPaths.count == 3)
        #expect(model.visibleCount(of: try #require(model.lists.first)) == 2)

        // A file deleted from a folder that is still there is hidden too.
        try FileManager.default.removeItem(at: gone)
        await model.refresh()
        #expect(model.visibleCount(of: try #require(model.lists.first)) == 1)
        #expect(model.cards.map(\.title) == ["here"])

        sandbox.mount("/Volumes/Archive")
        await model.refresh()
        #expect(model.visibleCount(of: try #require(model.lists.first)) == 2)
        #expect(model.cards.map(\.title) == ["here", "away"])
        // The list itself still holds all three.
        #expect(model.lists.first?.videoPaths.count == 3)
    }

    @Test func aWatchedVideoDeletedFromItsFolderLeavesTheHistory() async throws {
        let sandbox = try PlatformSandbox()
        let kept = try sandbox.file("kept.mp4"), doomed = try sandbox.file("doomed.mp4")
        let model = await sandbox.loggedInModel()
        for url in [kept, doomed] {
            model.playerVideoChanged(to: url, countsAsOpen: true)
            await model.settle()
        }
        model.filter = .history
        await model.settle()
        #expect(model.cards.count == 2)
        #expect(model.counts.watched == 2)

        try FileManager.default.removeItem(at: doomed)
        await model.refresh()
        #expect(model.cards.map(\.title) == ["kept"])
        #expect(model.counts.watched == 1)
    }

    @Test func subtitleBadgeListingsCanBeReadWhileTheyAreCounted() {
        let badges = SubtitleBadges(listDirectory: { _ in ["a.mp4", "a.srt"] })
        DispatchQueue.concurrentPerform(iterations: 200) { index in
            _ = badges.badge(forVideoAt: "/folder\(index % 20)/a.mp4")
            _ = badges.listings
        }
        #expect(badges.listings == 20)
    }

    @Test func theSubtitleChipUsesOneFormatKeyInBothLanguages() {
        #expect(UILanguage.en.string("Altyazılı (%@)", in: .module, ["SRT"]) == "Subtitled (SRT)")
        #expect(UILanguage.tr.string("Altyazılı (%@)", in: .module, ["SRT"]) == "Altyazılı (SRT)")
        #expect(UILanguage.en.string("Bu liste artık yok.", in: .module) == "This list no longer exists.")
    }

    // MARK: Windows

    @Test func aClosedWindowsPlayerIsNotKeptAliveByThePlatform() async throws {
        let sandbox = try PlatformSandbox()
        let model = sandbox.model()
        let token = PlayerWindowToken()
        weak var weakPlayer: PlayerModel?
        do {
            let player = PlayerModel()
            weakPlayer = player
            model.attachPlayer(player, owner: token)
            #expect(model.playbackSampler?() != nil)
        }
        // The window went away; nothing the platform holds points at its player any more.
        await Task.yield()
        #expect(weakPlayer == nil)
        #expect(model.playbackSampler?() == nil)
    }

    @Test func theFollowedWindowOwnsTheSamplerAndAClosingOneLeavesAnotherAlone() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()

        let first = PlayerWindowToken(), second = PlayerWindowToken()
        let firstPlayer = PlayerModel(), secondPlayer = PlayerModel()
        model.attachPlayer(firstPlayer, owner: first)
        model.attachPlayer(secondPlayer, owner: second)

        // The window that came last is followed: the first one's reports are not heard.
        model.handle(.paused(PlaybackPosition(seconds: 30, duration: 100)), from: first)
        await model.settle()
        #expect(model.store.stats(for: video)?.lastPositionSeconds == 0)
        model.handle(.paused(PlaybackPosition(seconds: 40, duration: 100)), from: second)
        await model.settle()
        #expect(model.store.stats(for: video)?.lastPositionSeconds == 40)

        // The first window closes: the second keeps the sampler.
        model.detachPlayer(firstPlayer, owner: first)
        #expect(model.playbackSampler != nil)
        #expect(firstPlayer.onPlaybackEvent == nil)
        #expect(secondPlayer.onPlaybackEvent != nil)

        // Becoming key again claims it; closing the one that has it clears it.
        model.claimPlayback(first)
        model.releasePlayback(second)
        #expect(model.playbackSampler != nil)
        model.detachPlayer(secondPlayer, owner: second)
        #expect(model.playbackSampler != nil)
        model.releasePlayback(first)
        #expect(model.playbackSampler == nil)
    }

    @Test func closingAWindowPausesItsPlayerAndSavesWhereItWas() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()

        let wav = try Self.silentWAV(seconds: 3)
        defer { try? FileManager.default.removeItem(at: wav) }
        let player = PlayerModel()
        let token = PlayerWindowToken()
        model.attachPlayer(player, owner: token)
        player.load(wav)
        #expect(await waitFor { player.position.duration > 0 })
        player.play()
        #expect(await waitFor { player.isPlaying })

        model.detachPlayer(player, owner: token)
        #expect(!player.isPlaying)
        await model.settle()
        // On disk, not only in memory: another instance of the store reads the file.
        let reader = sandbox.makeStore()
        #expect(reader.stats(for: video)?.durationSeconds == 3)
        #expect(model.playbackSampler == nil)
    }

    private static func silentWAV(seconds: Int) throws -> URL {
        let sampleRate = 8000
        let dataSize = seconds * sampleRate * 2
        var data = Data()
        func append32(_ value: Int) { withUnsafeBytes(of: UInt32(value).littleEndian) { data.append(contentsOf: $0) } }
        func append16(_ value: Int) { withUnsafeBytes(of: UInt16(value).littleEndian) { data.append(contentsOf: $0) } }
        data.append(contentsOf: Array("RIFF".utf8)); append32(36 + dataSize)
        data.append(contentsOf: Array("WAVEfmt ".utf8)); append32(16)
        append16(1); append16(1); append32(sampleRate); append32(sampleRate * 2); append16(2); append16(16)
        data.append(contentsOf: Array("data".utf8)); append32(dataSize)
        data.append(Data(count: dataSize))
        let url = FileManager.default.temporaryDirectory.appending(path: "silence-\(UUID().uuidString).wav")
        try data.write(to: url)
        return url
    }

    // MARK: Leaving and quitting

    @Test func pausingAndLeavingAreOnDiskForAnotherInstanceToRead() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        // The same login, restored by a second store from the shared keychain: it reads the file, not this store's memory.
        let reader = sandbox.makeStore()

        model.handle(.paused(PlaybackPosition(seconds: 70, duration: 100)))
        await model.settle()
        #expect(reader.stats(for: video)?.lastPositionSeconds == 70)

        model.handle(.leaving(PlaybackPosition(seconds: 80, duration: 100)))
        await model.settle()
        #expect(reader.stats(for: video)?.lastPositionSeconds == 80)

        // A jump is written half a second after the last one.
        model.handle(.seeked(PlaybackPosition(seconds: 90, duration: 100)))
        await model.settle()
        #expect(await waitFor { reader.stats(for: video)?.lastPositionSeconds == 90 })
    }

    @Test func loggingOutWritesTheProgressTheSessionStillHeld() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        // A jump is only in memory (the write is throttled).
        model.handle(.seeked(PlaybackPosition(seconds: 42, duration: 100)))
        await model.settle()
        #expect(model.store.hasUnsavedChanges)

        model.logout()
        await model.settle()
        #expect(!model.isLoggedIn)
        let reader = sandbox.makeStore()
        try reader.login(username: "Ayşe", password: "pw", rememberMe: false)
        #expect(reader.stats(for: video)?.lastPositionSeconds == 42)
    }

    @Test func quittingWritesWhereTheVideoWas() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        model.playbackSampler = { PlaybackPosition(seconds: 33, duration: 100) }

        #expect(model.prepareForQuit())
        let reader = sandbox.makeStore()
        #expect(reader.stats(for: video)?.lastPositionSeconds == 33)
        #expect(reader.stats(for: video)?.viewCount == 1)
    }

    @Test func quittingNeverWaitsLongForAProfileFileSomeoneElseHolds() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        model.playbackSampler = { PlaybackPosition(seconds: 33, duration: 100) }

        // Another process holds the cross-process lock of the profiles file.
        let descriptor = open(sandbox.profilesFile.path + ".lock", O_RDWR | O_CREAT, 0o600)
        #expect(descriptor >= 0)
        #expect(flock(descriptor, LOCK_EX) == 0)
        defer {
            flock(descriptor, LOCK_UN)
            close(descriptor)
        }
        let started = ContinuousClock.now
        let finished = model.prepareForQuit(timeout: .milliseconds(300))
        let elapsed = ContinuousClock.now - started
        #expect(!finished)
        #expect(elapsed < .seconds(2))
    }
}
