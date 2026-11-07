import Foundation
import LibraryKit
import Synchronization
import Testing
@testable import SubtitleAdderApp

/// With no profile logged in, no watch history exists: nothing is kept in memory from the profile that left, and
/// nothing that is still playing is recorded for the profile that logs in next.
@MainActor
@Suite(.serialized) struct LoggedOutHistoryTests {
    @Test func logoutAtOnceDropsTheProfilesStateFromMemory() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        model.filter = .history
        await model.settle()
        #expect(!model.cards.isEmpty)
        #expect(model.engagement.views == 1)

        // Right after the call, before the store has answered.
        model.logout()
        #expect(!model.isLoggedIn)
        #expect(model.cards.isEmpty)
        #expect(model.counts == ProfileCounts())
        #expect(model.engagement.views == 0)
        #expect(!model.engagement.isLoaded)
        await model.settle()
        #expect(!model.isLoggedIn)
        #expect(model.cards.allSatisfy { $0.viewCount == 0 && !$0.hasProgress })
    }

    @Test func logoutOutsideTheHubLeavesNoCards() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        model.filter = .history
        await model.settle()
        model.isHubActive = false
        model.logout()
        await model.settle()
        #expect(model.cards.isEmpty)
    }

    @Test func playbackThatContinuesAfterLogoutIsNotRecordedForTheNextProfile() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel(username: "Ali")
        _ = await model.register(username: "Veli", password: "pw")
        model.logout()
        await model.settle()
        #expect(await model.login(username: "Ali", password: "pw"))
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()

        let position = Mutex(PlaybackPosition(seconds: 10, duration: 100))
        model.playbackSampler = { position.withLock { $0 } }
        model.handle(.started)
        model.logout()
        await model.settle()

        // Veli logs in while the video plays on.
        #expect(await model.login(username: "Veli", password: "pw"))
        position.withLock { $0.seconds = 50 }
        try await Task.sleep(for: .milliseconds(200))
        model.handle(.paused(PlaybackPosition(seconds: 60, duration: 100)))
        await model.settle()
        #expect(model.store.stats(for: video) == nil)
        #expect(model.prepareForQuit())
        #expect(model.store.stats(for: video) == nil)
    }

    @Test func aVideoOpenedLoggedOutIsNotRecordedForALaterLogin() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel(username: "Ali")
        model.logout()
        await model.settle()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        let position = Mutex(PlaybackPosition(seconds: 2400, duration: 3000))
        model.playbackSampler = { position.withLock { $0 } }
        #expect(await model.login(username: "Ali", password: "pw"))
        model.handle(.paused(PlaybackPosition(seconds: 2400, duration: 3000)))
        await model.settle()
        #expect(model.store.stats(for: video) == nil)
        #expect(model.prepareForQuit())
        #expect(model.store.stats(for: video) == nil)
    }

    @Test func aDeletedProfilesPlaybackIsNotRecordedForTheNextLogin() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel(username: "Ali")
        _ = await model.register(username: "Veli", password: "pw")
        model.logout()
        await model.settle()
        #expect(await model.login(username: "Ali", password: "pw"))
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        #expect(await model.deleteProfile(username: "Ali", password: "pw"))
        #expect(await model.login(username: "Veli", password: "pw"))
        model.handle(.paused(PlaybackPosition(seconds: 60, duration: 100)))
        await model.settle()
        #expect(model.store.stats(for: video) == nil)
    }

    @Test func reopeningTheSameVideoAfterANewLoginRecordsIt() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = await sandbox.loggedInModel(username: "Ali")
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        model.logout()
        await model.settle()
        #expect(await model.login(username: "Ali", password: "pw"))
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        model.handle(.paused(PlaybackPosition(seconds: 30, duration: 100)))
        await model.settle()
        #expect(model.store.stats(for: video)?.lastPositionSeconds == 30)
    }

    @Test func logoutForgetsTheSearch() async throws {
        let sandbox = try PlatformSandbox()
        let model = await sandbox.loggedInModel(username: "Ali")
        model.searchText = "gizli"
        model.submitSearch()
        await model.settle()
        model.logout()
        #expect(model.searchText.isEmpty)
        #expect(model.searchQuery.isEmpty)
        await model.settle()
    }

    @Test func nothingIsRecordedWhileLoggedOut() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let model = sandbox.model()
        await model.start()
        model.playerVideoChanged(to: video, countsAsOpen: true)
        model.handle(.paused(PlaybackPosition(seconds: 30, duration: 60)))
        await model.settle()
        #expect(model.store.stats(for: video) == nil)
        #expect(model.prepareForQuit())
        #expect(model.store.stats(for: video) == nil)
    }

    @Test func aLoggedOutHubDescribesNoHistory() {
        let out = EmptyFeedText(filter: .all, search: "", listName: nil, loggedIn: false)
        let signedIn = EmptyFeedText(filter: .all, search: "", listName: nil, loggedIn: true)
        #expect(out.hint != signedIn.hint)
        // Not the search-miss text, and not the signed-in one; whatever the UI language.
        #expect(out.hint != L("Farklı bir anahtar kelime deneyin veya klasör gezgininden yeni bir video klasörü açın."))
        #expect(out.hint == L("Klasör gezgininden bir video klasörü açın. Giriş yaptığınızda izleme geçmişiniz de burada görünür."))
    }

    @Test func aThumbnailFinishedAfterAResetIsNotWritten() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let writes = Mutex(0)
        let gate = Mutex(false)
        let service = ThumbnailService(
            read: { _ in nil },
            write: { _, _ in writes.withLock { $0 += 1 } },
            generate: { _ in
                while !gate.withLock({ $0 }) { try? await Task.sleep(for: .milliseconds(10)) }
                return Data([1, 2, 3])
            })
        let task = Task { await service.data(for: video) }
        try await Task.sleep(for: .milliseconds(100))
        service.reset()
        gate.withLock { $0 = true }
        _ = await task.value
        #expect(writes.withLock { $0 } == 0)
        #expect(service.cachedCount == 0)
    }
}
