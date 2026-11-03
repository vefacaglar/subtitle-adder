import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// Switching to another video never asks (the video left keeps its job and its edits in its own session); an open asks,
/// whichever way the file arrives, only where it would really destroy something: a subtitle loaded over lines with a
/// running job or unsaved edits, or lines that belong to no video. And a subtitle that comes without a video does not
/// borrow the video that happens to be open.
@MainActor
@Suite struct OpenConfirmationTests {
    private func makeFolder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "confirm-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
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

    /// A model whose engine never answers `transcribe` (it just sleeps), so a job stays running until it is cancelled.
    private func makeModel() throws -> SubtitleViewModel {
        let engine = try FakeEngine.make(transcribe: "exec sleep 30")
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine), defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: EngineLaunchState()
        )
        vm.offlineMode = false
        vm.diarize = false
        return vm
    }

    private func line(_ id: Int, _ text: String) -> Sentence {
        Sentence(id: id, start: Double(id), end: Double(id) + 1, text: text)
    }

    // MARK: - Nothing at stake

    @Test func anIdleEditorOpensAtOnce() throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder), b = try touch("b.mp4", in: folder)
        let vm = try makeModel()
        vm.requestOpenVideo(a)
        #expect(vm.video == a && vm.pendingOpen == nil)
        // Transcribed lines nobody edited are not worth a question either.
        vm.sentences = [line(1, "Merhaba.")]
        vm.requestOpenVideo(b)
        #expect(vm.video == b && vm.pendingOpen == nil)
    }

    @Test func theVideoThatIsAlreadyOpenNeverAsks() throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder)
        let vm = try makeModel()
        vm.openVideo(a)
        vm.sentences = [line(1, "Merhaba.")]
        vm.setText("Elle düzeltildi.", for: vm.sentences[0].uid)
        #expect(vm.hasUnsavedEdits)
        vm.requestOpenVideo(a)
        vm.openExternalFiles([a])
        #expect(vm.pendingOpen == nil && vm.sentences.map(\.text) == ["Elle düzeltildi."])
    }

    // MARK: - A running job

    @Test func switchingVideosNeverAsksAndNeverStopsTheJobOfTheOneLeft() async throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder), b = try touch("b.mp4", in: folder)
        let vm = try makeModel()
        vm.openVideo(a)
        vm.generate()
        #expect(vm.isWorking)

        // The picker and the sidebar/card closure, a drop and Finder's "Open With" all just show the other video.
        vm.requestOpenVideo(b)
        #expect(vm.pendingOpen == nil && vm.video == b && !vm.isWorking)
        vm.openFiles([a])
        #expect(vm.pendingOpen == nil && vm.video == a && vm.isWorking && vm.job != nil)
        vm.openExternalFiles([b])
        #expect(vm.pendingOpen == nil && vm.video == b && vm.workspace == .editor)
        var opened = false
        vm.requestOpen(losing: []) { opened = true }
        #expect(vm.pendingOpen == nil && opened)

        // A's job went on the whole time and is still its own.
        let session = try #require(vm.existingSession(for: a))
        #expect(session.isWorking && session.job != nil)
        vm.openVideo(a)
        vm.cancel()
    }

    @Test func aRunningJobAsksBeforeASubtitleReplacesTheLinesItWritesTo() async throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder)
        let vm = try makeModel()
        vm.openVideo(a)
        vm.generate()
        #expect(vm.isWorking)

        // The picker, a drop and a request that replaces what is on screen. (Finder's "Open With" of a subtitle looks
        // for the companion video instead, which has no session yet here.)
        vm.requestOpenSubtitle(try touch("c.srt", in: folder, text: Self.srt))
        #expect(vm.pendingOpen != nil && vm.video == a && vm.sentences.isEmpty)
        vm.cancelPendingOpen()
        var opened = false
        vm.requestOpen(losing: [vm.foreground]) { opened = true }
        #expect(vm.pendingOpen != nil && !opened)
        vm.cancelPendingOpen()
        vm.openFiles([try touch("d.srt", in: folder, text: Self.srt)])
        #expect(vm.pendingOpen != nil && vm.video == a && vm.sentences.isEmpty)

        // "Vazgeç": the job goes on, untouched.
        vm.cancelPendingOpen()
        #expect(vm.pendingOpen == nil && vm.video == a && vm.isWorking && vm.job != nil)

        // "Devam": the job stops and the subtitle's lines take its place.
        vm.requestOpenSubtitle(try touch("e.srt", in: folder, text: Self.srt))
        let pending = try #require(vm.pendingOpen)
        #expect(pending.stopsJob && !pending.losesEdits)
        #expect(pending.message == "Süren iş durdurulacak. Devam edilsin mi?")
        vm.confirmPendingOpen()
        #expect(vm.pendingOpen == nil && vm.video == a && !vm.isWorking && vm.sentences.count == 2)
    }

    @Test func aNewerRequestReplacesOneStillWaiting() throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder)
        let vm = try makeModel()
        vm.openVideo(a)
        vm.generate()
        vm.requestOpenSubtitle(try touch("b.srt", in: folder, text: Self.srt))
        vm.requestOpenSubtitle(try touch("c.srt", in: folder, text: Self.srt))
        vm.confirmPendingOpen()
        #expect(vm.loadedSubtitle?.lastPathComponent == "c.srt")
    }

    @Test func severalVideosGoToThePlaylistWithoutAskingBecauseTheyReplaceNothing() throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder)
        let vm = try makeModel()
        vm.openVideo(a)
        vm.generate()
        vm.openFiles([try touch("x.mp4", in: folder), try touch("y.mp4", in: folder)])
        #expect(vm.pendingOpen == nil && vm.playlist.count == 2 && vm.workspace == .playlist && vm.isWorking)
        vm.cancel()
    }

    // MARK: - Unsaved edits

    @Test func editedLinesSurviveSwitchingVideosAndAskOnlyWhenASubtitleWouldReplaceThem() async throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder), b = try touch("b.mp4", in: folder)
        let vm = try makeModel()
        vm.openVideo(a)
        vm.sentences = [line(1, "Merhaba."), line(2, "Dünya.")]
        #expect(!vm.hasUnsavedEdits)
        vm.setText("Selam.", for: vm.sentences[0].uid)
        #expect(vm.hasUnsavedEdits)

        // Showing b asks nothing: a keeps its edited lines and its flag.
        vm.requestOpenVideo(b)
        #expect(vm.pendingOpen == nil && vm.video == b && vm.sentences.isEmpty && !vm.hasUnsavedEdits)
        vm.requestOpenVideo(a)
        #expect(vm.video == a && vm.sentences.map(\.text) == ["Selam.", "Dünya."] && vm.hasUnsavedEdits)

        // A subtitle loaded over them is what would lose the work.
        vm.requestOpenSubtitle(try touch("s.srt", in: folder, text: Self.srt))
        let pending = try #require(vm.pendingOpen)
        #expect(!pending.stopsJob && pending.losesEdits)
        #expect(pending.message == "Kaydedilmemiş değişiklikler kaybolacak. Devam edilsin mi?")
        #expect(vm.video == a && vm.sentences.map(\.text) == ["Selam.", "Dünya."])
        vm.cancelPendingOpen()

        // Saving writes them to disk, so nothing is left to lose.
        #expect(await vm.saveSubtitle() != nil)
        #expect(!vm.hasUnsavedEdits)
        vm.requestOpenSubtitle(try touch("s.srt", in: folder, text: Self.srt))
        #expect(vm.pendingOpen == nil && vm.sentences.count == 2 && !vm.hasUnsavedEdits)
    }

    @Test func aJobAndEditsTogetherSayBoth() throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder)
        let vm = try makeModel()
        vm.openVideo(a)
        vm.sentences = [line(1, "Merhaba.")]
        vm.setText("Selam.", for: vm.sentences[0].uid)
        vm.translationTarget = "en"
        vm.language = "tr"
        vm.translate()
        #expect(vm.isWorking)
        vm.requestOpenSubtitle(try touch("s.srt", in: folder, text: Self.srt))
        let pending = try #require(vm.pendingOpen)
        #expect(pending.stopsJob && pending.losesEdits)
        #expect(pending.message == "Süren iş durdurulacak ve kaydedilmemiş değişiklikler kaybolacak. Devam edilsin mi?")
        vm.cancel()
    }

    @Test func aTranslationThatWasNeverSavedCountsAsUnsavedWork() throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder), b = try touch("b.mp4", in: folder)
        let vm = try makeModel()
        vm.autoSaveSubtitle = false
        vm.openVideo(a)
        vm.sentences = [line(1, "Merhaba.")]
        vm.applyTranslation([line(1, "Hello.")], language: "en", source: vm.sentences)
        #expect(vm.hasUnsavedEdits)
        vm.requestOpenSubtitle(try touch("s.srt", in: folder, text: Self.srt))
        #expect(vm.pendingOpen?.losesEdits == true && vm.video == a)
        vm.cancelPendingOpen()
        // Switching to another video keeps the translation and its flag.
        vm.requestOpenVideo(b)
        #expect(vm.pendingOpen == nil && vm.video == b)
        vm.openVideo(a)
        #expect(vm.hasTranslations && vm.hasUnsavedEdits)
    }

    @Test func undoingAnEditStillCountsAsAnEditedDocument() throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder)
        let vm = try makeModel()
        let undo = UndoManager()
        vm.undoManager = undo
        vm.openVideo(a)
        vm.sentences = [line(1, "Merhaba.")]
        vm.setText("Selam.", for: vm.sentences[0].uid)
        undo.undo()
        #expect(vm.sentences.map(\.text) == ["Merhaba."] && vm.hasUnsavedEdits)
        vm.requestOpenSubtitle(try touch("s.srt", in: folder, text: Self.srt))
        #expect(vm.pendingOpen != nil)
    }

    @Test func theUndoHistoryStartsOverWhenAnotherVideoComesToTheFront() throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder), b = try touch("b.mp4", in: folder)
        let vm = try makeModel()
        let undo = UndoManager()
        vm.undoManager = undo
        vm.openVideo(a)
        vm.sentences = [line(1, "Merhaba.")]
        vm.setText("Selam.", for: vm.sentences[0].uid)
        #expect(undo.canUndo)
        vm.openVideo(b)
        #expect(!undo.canUndo)
        // The edit itself is a's and is still there.
        vm.openVideo(a)
        #expect(vm.sentences.map(\.text) == ["Selam."] && !undo.canUndo)
    }

    @Test func linesOfASubtitleOnTheirOwnAreAskedAboutBeforeAVideoTakesTheirPlace() throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder)
        let vm = try makeModel()
        // A subtitle with no companion video: its lines belong to no video, so they have nowhere to stay.
        vm.openSubtitle(try touch("loose.srt", in: folder, text: Self.srt))
        #expect(vm.video == nil && vm.sentences.count == 2)
        vm.setText("Elle.", for: vm.sentences[0].uid)
        vm.requestOpenVideo(a)
        #expect(vm.pendingOpen?.losesEdits == true && vm.video == nil)
        vm.cancelPendingOpen()
        vm.openExternalFiles([a])
        #expect(vm.pendingOpen?.losesEdits == true && vm.video == nil)
        vm.confirmPendingOpen()
        #expect(vm.video == a && vm.pendingOpen == nil)
    }

    @Test func importedLinesStartCleanAndOpeningResetsTheFlag() throws {
        let folder = try makeFolder()
        let a = try touch("a.mp4", in: folder)
        let subtitle = try touch("s.srt", in: folder, text: Self.srt)
        let vm = try makeModel()
        vm.openVideo(a)
        vm.sentences = [line(1, "Merhaba.")]
        vm.setText("Selam.", for: vm.sentences[0].uid)
        #expect(vm.hasUnsavedEdits)
        vm.openSubtitle(subtitle)
        #expect(!vm.hasUnsavedEdits && vm.sentences.count == 2)
        vm.setText("Selam.", for: vm.sentences[0].uid)
        vm.openVideo(nil)
        #expect(!vm.hasUnsavedEdits)
    }

    // MARK: - A subtitle without a video does not borrow the open one

    @Test func aSubtitleFromFinderWithoutACompanionClosesTheOpenVideoSoSavesGoToItsOwnFile() async throws {
        let folder = try makeFolder()
        let other = try makeFolder()
        let current = try touch("current.mp4", in: folder)
        let sidecar = try touch("current.srt", in: folder, text: Self.srt)
        let original = try String(contentsOf: sidecar, encoding: .utf8)
        let loose = try touch("loose.srt", in: other, text: Self.srt)
        let vm = try makeModel()
        vm.openVideo(current)
        #expect(vm.loadedSubtitle == sidecar)

        vm.openExternalFiles([loose])
        #expect(vm.video == nil)
        #expect(vm.loadedSubtitle == loose && vm.sentences.count == 2)
        #expect(vm.subtitleSaveURL() == other.appending(path: "loose.srt"))

        vm.setText("Değişti.", for: vm.sentences[0].uid)
        #expect(await vm.saveSubtitle() == other.appending(path: "loose.srt"))
        #expect(try String(contentsOf: sidecar, encoding: .utf8) == original, "the open video's sidecar was not touched")
        #expect(try String(contentsOf: other.appending(path: "loose.srt"), encoding: .utf8).contains("Değişti."))
    }

    @Test func aSubtitleWithACompanionStillOpensItsVideo() throws {
        let folder = try makeFolder()
        let current = try touch("current.mp4", in: folder)
        try touch("Film.mkv", in: folder)
        let subtitle = try touch("Film.srt", in: folder, text: Self.srt)
        let vm = try makeModel()
        vm.openVideo(current)
        vm.openExternalFiles([subtitle])
        #expect(vm.video?.lastPathComponent == "Film.mkv" && vm.loadedSubtitle == subtitle)
    }

    @Test func aPickedOrDroppedSubtitleKeepsTheVideoThatIsOpen() throws {
        let folder = try makeFolder()
        let current = try touch("current.mp4", in: folder)
        let subtitle = try touch("loose.srt", in: folder, text: Self.srt)
        let vm = try makeModel()
        vm.openVideo(current)
        vm.openFiles([subtitle])
        #expect(vm.video == current)
        vm.requestOpenSubtitle(subtitle)
        #expect(vm.video == current && vm.sentences.count == 2)
    }

    // MARK: - The window can't get narrower than the sidebar and the detail together

    @Test func theWindowIsNeverNarrowerThanTheSidebarAndTheDetailNeedTogether() {
        #expect(ContentView.minimumWindowWidth >= ContentView.sidebarMinWidth + ContentView.detailMinWidth)
        #expect(ContentView.minimumWindowWidth == 820)
    }
}
