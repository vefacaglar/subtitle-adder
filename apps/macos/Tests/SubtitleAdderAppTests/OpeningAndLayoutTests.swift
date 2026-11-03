import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// How files reach the editor from Finder and from the pickers: routing, companion videos and batching.
@MainActor
@Suite struct OpeningAndLayoutTests {
    private func makeModel(defaults: UserDefaults? = nil) -> (SubtitleViewModel, UserDefaults) {
        let defaults = defaults ?? UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        let vm = SubtitleViewModel(catalog: ModelCatalog(engine: nil), defaults: defaults, launchState: EngineLaunchState())
        return (vm, defaults)
    }

    private func makeFolder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "open-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    /// The same file, whichever way the URL was built.
    private func sameFile(_ a: URL?, _ b: URL?) -> Bool {
        a?.standardizedFileURL.path == b?.standardizedFileURL.path
    }

    @discardableResult
    private func touch(_ name: String, in folder: URL, text: String = "") throws -> URL {
        let url = folder.appending(path: name)
        try Data(text.utf8).write(to: url)
        return url
    }

    private static let srt = """
    1
    00:00:00,000 --> 00:00:01,000
    Merhaba.

    2
    00:00:01,000 --> 00:00:02,000
    Dünya.
    """

    // MARK: - Opening from Finder

    @Test func aSingleVideoFromFinderSwitchesToTheEditorAndOpens() throws {
        let folder = try makeFolder()
        let (vm, _) = makeModel()
        let video = try touch("clip.mp4", in: folder)
        vm.workspace = .playlist
        vm.openExternalFiles([video])
        #expect(vm.workspace == .editor)
        #expect(vm.video == video)
        #expect(vm.playlist.isEmpty)
    }

    @Test func aDropOnThePlaylistTabStillGoesToThePlaylistButFinderOpensTheEditor() throws {
        let folder = try makeFolder()
        let (vm, _) = makeModel()
        let video = try touch("clip.mp4", in: folder)
        vm.workspace = .playlist
        vm.openFiles([video])
        #expect(vm.workspace == .playlist)
        #expect(vm.playlist.map(\.fileName) == ["clip.mp4"])
        #expect(vm.video == nil)
    }

    @Test func severalVideosFromFinderBuildAPlaylist() throws {
        let folder = try makeFolder()
        let (vm, _) = makeModel()
        let first = try touch("a.mp4", in: folder)
        let second = try touch("b.mkv", in: folder)
        vm.openExternalFiles([first, second])
        #expect(vm.workspace == .playlist)
        #expect(vm.playlist.map(\.fileName) == ["a.mp4", "b.mkv"])
        #expect(vm.video == nil)
    }

    @Test func aPlaylistRunDoesNotOwnTheEditorSoAFinderVideoOpensInIt() throws {
        let folder = try makeFolder()
        let (vm, _) = makeModel()
        let current = try touch("current.mp4", in: folder)
        vm.openVideo(current)
        vm.isPlaylistRunning = true
        defer { vm.isPlaylistRunning = false }
        let extra = try touch("extra.mp4", in: folder)
        vm.openExternalFiles([extra])
        #expect(vm.video == extra && vm.playlist.isEmpty && vm.workspace == .editor)
        // Two at once still build a list.
        vm.openExternalFiles([current, extra])
        #expect(vm.playlist.count == 2 && vm.workspace == .playlist)
    }

    @Test func missingFilesAndFoldersAreSkipped() throws {
        let folder = try makeFolder()
        let (vm, _) = makeModel()
        vm.openExternalFiles([folder.appending(path: "gone.mp4"), folder])
        #expect(vm.video == nil && vm.playlist.isEmpty && vm.workspace == .editor)
        vm.openExternalFiles([URL(string: "https://example.com/clip.mp4")!])
        #expect(vm.video == nil)

        // One real file among missing ones is a single video, not a playlist.
        let real = try touch("real.mp4", in: folder)
        vm.openExternalFiles([folder.appending(path: "gone.mp4"), real])
        #expect(vm.video == real && vm.playlist.isEmpty)
    }

    @Test func aVideoWithItsSubtitleOpensBothAndTheSubtitleWins() throws {
        let folder = try makeFolder()
        let (vm, _) = makeModel()
        let video = try touch("clip.mp4", in: folder)
        try touch("clip.vtt", in: folder, text: "WEBVTT\n\n00:00.000 --> 00:01.000\nSidecar.\n")
        let subtitle = try touch("other.srt", in: folder, text: Self.srt)
        vm.openExternalFiles([video, subtitle])
        #expect(vm.video == video)
        #expect(vm.loadedSubtitle == subtitle)
        #expect(vm.sentences.map(\.text) == ["Merhaba.", "Dünya."])
    }

    // MARK: - Companion video

    @Test func aSubtitleOpenedAloneBringsItsCompanionVideo() throws {
        let folder = try makeFolder()
        let video = try touch("Film.mkv", in: folder)
        let subtitle = try touch("Film.srt", in: folder, text: Self.srt)

        // Dropped on the window with no video open.
        let (dropped, _) = makeModel()
        dropped.openFiles([subtitle])
        #expect(sameFile(dropped.video, video))
        #expect(dropped.loadedSubtitle == subtitle)
        #expect(dropped.sentences.count == 2)
        #expect(dropped.status == .finished)

        // Picked with the toolbar button.
        let (picked, _) = makeModel()
        #expect(picked.openSubtitle(subtitle))
        #expect(sameFile(picked.video, video) && picked.sentences.count == 2)

        // From Finder, on any tab.
        let (external, _) = makeModel()
        external.workspace = .playlist
        external.openExternalFiles([subtitle])
        #expect(external.workspace == .editor)
        #expect(sameFile(external.video, video) && external.sentences.count == 2)
    }

    @Test func aLanguageTaggedSubtitleFindsTheSameVideo() throws {
        let folder = try makeFolder()
        let video = try touch("Episode01.mp4", in: folder)
        let subtitle = try touch("Episode01.tr.srt", in: folder, text: Self.srt)
        let (vm, _) = makeModel()
        vm.openFiles([subtitle])
        #expect(sameFile(vm.video, video))
    }

    @Test func aSubtitleWithoutACompanionLoadsAlone() throws {
        let folder = try makeFolder()
        let subtitle = try touch("Lonely.srt", in: folder, text: Self.srt)
        let (vm, _) = makeModel()
        vm.openFiles([subtitle])
        #expect(vm.video == nil)
        #expect(vm.sentences.count == 2 && vm.loadedSubtitle == subtitle)
    }

    @Test func theVideoBeingWorkedOnStaysWhenASubtitleIsImportedButFinderMayReplaceIt() throws {
        let folder = try makeFolder()
        let current = try touch("current.mp4", in: folder)
        try touch("Film.mkv", in: folder)
        let subtitle = try touch("Film.srt", in: folder, text: Self.srt)
        let (vm, _) = makeModel()
        vm.openVideo(current)

        // Picker and drop: the open video keeps the imported lines.
        vm.openFiles([subtitle])
        #expect(vm.video == current)
        #expect(vm.sentences.count == 2)
        #expect(vm.openSubtitle(subtitle))
        #expect(vm.video == current)

        // Opening the subtitle from Finder is a request for that subtitle with its video.
        vm.openExternalFiles([subtitle])
        #expect(vm.video?.lastPathComponent == "Film.mkv")
        #expect(vm.loadedSubtitle == subtitle)
    }

    @Test func aSubtitleThatCannotBeReadStillReportsFailure() throws {
        let folder = try makeFolder()
        try touch("Film.mp4", in: folder)
        let subtitle = try touch("Film.srt", in: folder, text: "not a subtitle")
        let (vm, _) = makeModel()
        #expect(!vm.openSubtitle(subtitle))
        if case .failed = vm.status {} else { Issue.record("expected a failed status, got \(vm.status)") }
    }

    @Test func openingTheAlreadyOpenVideoAgainChangesNothing() throws {
        let folder = try makeFolder()
        let video = try touch("clip.mp4", in: folder)
        let (vm, _) = makeModel()
        vm.openVideo(video)
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Elle düzeltildi.")]
        vm.openVideo(video)
        vm.openExternalFiles([video])
        // `ContentView` reloads the player only when `video` changes, so nothing restarts either.
        #expect(vm.video == video)
        #expect(vm.sentences.map(\.text) == ["Elle düzeltildi."])
    }

    // MARK: - Coalescing

    /// Polls instead of sleeping a fixed time: other suites keep the main actor busy, which delays the timer.
    private func waitUntil(timeout: Double = 5, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline { try await Task.sleep(for: .milliseconds(10)) }
    }

    @Test func severalURLsInQuickSuccessionArriveAsOneBatch() async throws {
        var batches: [[URL]] = []
        let coalescer = ExternalOpenCoalescer(quietPeriod: .milliseconds(40), maxWait: .seconds(1)) { batches.append($0) }
        let a = URL(filePath: "/tmp/a.mp4"), b = URL(filePath: "/tmp/b.mp4"), c = URL(filePath: "/tmp/c.srt")
        coalescer.receive(a)
        coalescer.receive(b)
        coalescer.receive(a)
        coalescer.receive(c)
        coalescer.receive(URL(string: "https://example.com/x.mp4")!)
        #expect(batches.isEmpty)
        try await waitUntil { !batches.isEmpty }
        #expect(batches == [[a, b, c]])

        // A later open is a batch of its own.
        coalescer.receive(b)
        try await waitUntil { batches.count > 1 }
        #expect(batches == [[a, b, c], [b]])
    }

    @Test func theWaitForAnotherURLShrinksSoTheBatchIsOutByTheMaximumWait() {
        let quiet = Duration.milliseconds(250), maxWait = Duration.seconds(1)
        func delay(_ sinceFirst: Duration) -> Duration {
            ExternalOpenCoalescer.delay(quietPeriod: quiet, maxWait: maxWait, sinceFirst: sinceFirst)
        }
        #expect(delay(.zero) == quiet)
        #expect(delay(.milliseconds(700)) == quiet)
        #expect(delay(.milliseconds(900)) == .milliseconds(100))
        #expect(delay(.seconds(1)) == .zero)
        #expect(delay(.seconds(5)) == .zero)
    }

    @Test func flushDeliversImmediatelyAndOnlyOnce() async throws {
        var batches: [[URL]] = []
        let coalescer = ExternalOpenCoalescer(quietPeriod: .milliseconds(30), maxWait: .seconds(1)) { batches.append($0) }
        coalescer.flush()
        #expect(batches.isEmpty)
        let a = URL(filePath: "/tmp/a.mp4")
        coalescer.receive(a)
        coalescer.flush()
        #expect(batches == [[a]])
        try await Task.sleep(for: .milliseconds(150))
        #expect(batches == [[a]])
    }
}
