import EngineKit
import Foundation
import LibraryKit
import Testing
@testable import SubtitleAdderApp

/// Edge cases of the sessions and the queue: what a playlist run refuses and defends, run-start failures, what a stopped
/// or re-requested job goes back to, the lane, memory and the external server translating in a video that is not on screen.
extension SessionTests {
    private static let srt = "1\n00:00:00,000 --> 00:00:01,000\nBaşka satır.\n"

    // MARK: - The playlist run keeps its videos

    @Test func aSubtitleIsRefusedForAVideoTheRunIsRecognizingAndTheRunGoesOn() async throws {
        let (vm, lab) = try makeLab()
        let x = lab.video("x")
        let other = lab.folder.appending(path: "other.srt")
        try Data(Self.srt.utf8).write(to: other)
        vm.translationTarget = "es"
        vm.addToPlaylist([x], sourceLanguage: "tr")
        vm.startPlaylist()
        #expect(await eventually { lab.started("x") })
        vm.openVideo(x)

        // Every way of loading it is refused with a message, and nothing is asked.
        #expect(!vm.loadSubtitleFile(other))
        #expect(vm.openRefusal?.text == SubtitleViewModel.batchBusyMessage.text)
        vm.openRefusal = nil
        vm.openFiles([other])
        #expect(vm.openRefusal != nil && vm.pendingOpen == nil)
        vm.openRefusal = nil
        vm.requestOpenSubtitle(other)
        #expect(vm.openRefusal != nil && vm.pendingOpen == nil)
        #expect(vm.loadedSubtitle == nil)

        lab.release("x")
        await vm.playlistTask?.value
        #expect(vm.playlist[0].state == .completed)
        #expect(vm.sentences.map(\.text) == ["Metin x"] && vm.savedSubtitleName == "x.srt")
        // Once the run is through with the video, a subtitle can be opened again.
        vm.openRefusal = nil
        #expect(vm.loadSubtitleFile(other) && vm.openRefusal == nil)
    }

    @Test func aSubtitleIsRefusedForAVideoTheRunIsTranslatingToo() async throws {
        let (vm, lab) = try makeLab(gatedTranslate: true)
        let x = lab.video("x")
        let other = lab.folder.appending(path: "other.srt")
        try Data(Self.srt.utf8).write(to: other)
        vm.translationTarget = "es"
        vm.addToPlaylist([x], sourceLanguage: "tr")
        vm.startPlaylist()
        lab.release("x")
        #expect(await eventually { vm.existingSession(for: x)?.isPlaylistTranslating == true })
        vm.openVideo(x)
        vm.openFiles([other])
        #expect(vm.openRefusal != nil && vm.pendingOpen == nil && vm.sentences.map(\.text) == ["Metin x"])
        vm.openRefusal = nil

        lab.release("translate")
        await vm.playlistTask?.value
        #expect(vm.playlist[0].state == .completed && vm.translations.values.map(\.text) == ["Çeviri"])
        #expect(vm.savedSubtitleName == "x.srt")
    }

    @Test func aStageThreeTranslationNeverWritesIntoLinesItWasNotStartedFor() async throws {
        let (vm, lab) = try makeLab(gatedTranslate: true)
        let x = lab.video("x")
        let other = lab.folder.appending(path: "other.srt")
        try Data(Self.srt.utf8).write(to: other)
        vm.translationTarget = "es"
        vm.addToPlaylist([x], sourceLanguage: "tr")
        vm.startPlaylist()
        lab.release("x")
        #expect(await eventually { vm.existingSession(for: x)?.isPlaylistTranslating == true })
        // Something replaces the lines behind the refusal's back.
        let session = try #require(vm.existingSession(for: x))
        #expect(vm.applySubtitle(other, to: session))
        lab.release("translate")
        await vm.playlistTask?.value

        #expect(session.sentences.map(\.text) == ["Başka satır."])
        #expect(session.translations.isEmpty && session.savedSubtitleName == nil)
    }

    @Test func aReplacedRecognitionJobLeavesTheItemWaitingAndTheRunGoesOn() async throws {
        let (vm, lab) = try makeLab()
        let x = lab.video("x"), y = lab.video("y")
        vm.translationTarget = "es"
        vm.addToPlaylist([x, y], sourceLanguage: "tr")
        vm.startPlaylist()
        #expect(await eventually { lab.started("x") })
        let session = try #require(vm.existingSession(for: x))
        // The job is taken away from under the run.
        vm.stopJob(of: session)
        #expect(await eventually { lab.started("y") })
        lab.release("y")
        await vm.playlistTask?.value
        #expect(vm.playlist.map(\.state) == [.pending, .completed])
        #expect(!vm.jobQueue.isBusy)
    }

    @Test func aSavedItemsSessionIsDroppedWhenItLeavesTheScreen() async throws {
        let (vm, lab) = try makeLab()
        let x = lab.video("x"), other = lab.video("other")
        vm.translationTarget = "es"
        vm.addToPlaylist([x], sourceLanguage: "tr")
        vm.openVideo(x)
        vm.startPlaylist()
        lab.release("x")
        await vm.playlistTask?.value
        // It is on screen, so it stays; leaving it drops it, since the file on disk is the result.
        #expect(vm.existingSession(for: x) != nil && vm.savedSubtitleName == "x.srt")
        vm.openVideo(other)
        #expect(vm.existingSession(for: x) == nil)
        // The list row still works: the video opens as usual.
        vm.openListRow(vm.listRows[0])
        #expect(vm.video == x && vm.workspace == .editor)
    }

    @Test func theRunDoesNotMoveTheToolbarsTargetEvenForTheItemOnScreen() async throws {
        let (vm, lab) = try makeLab()
        let x = lab.video("x")
        vm.translationTarget = "tr"
        vm.addToPlaylist([x], sourceLanguage: "")
        vm.openVideo(x)
        vm.startPlaylist()
        #expect(await eventually { lab.started("x") })
        lab.release("x")
        await vm.playlistTask?.value
        // The video turned out to be Turkish, the run's target is Turkish: the run says so, the toolbar stays.
        #expect(vm.translationTarget == "tr")
        #expect(vm.playlist[0].state == .failed)
    }

    // MARK: - Run-start failures belong to the list

    @Test func aPlaylistThatCannotStartDoesNotTouchTheStatusOfTheRunningVideo() async throws {
        let models = """
        echo '{"event": "models", "models": [\
        {"id": "large-v3", "size_mb": 3000, "speed": 1, "accuracy": 5, "downloaded": true},\
        {"id": "madlad-3b", "kind": "translation", "backend": "ctranslate2", "size_mb": 2839, "speed": 2, "accuracy": 5, "downloaded": false}\
        ]}'
        """
        let (vm, lab) = try makeLab(models: models)
        await vm.catalog.load()
        vm.model = "large-v3"
        vm.translationModel = "madlad-3b"
        vm.offlineMode = true
        let a = lab.video("a")
        vm.openVideo(a)
        vm.generate()
        #expect(await eventually { lab.started("a") })
        let session = vm.foreground
        vm.workspace = .playlist
        vm.addToPlaylist([lab.video("x")])
        vm.startPlaylist()

        #expect(session.isWorking && session.job != nil && vm.canStop == false)
        guard case .working = session.status else { Issue.record("the job's status was replaced"); return }
        let notice = try #require(vm.playlistNotice)
        #expect(notice.message.text.hasPrefix("MADLAD-400 3B indirilmemiş.") && notice.retry != nil)
        #expect(!vm.isPlaylistRunning)
        lab.release("a")
        #expect(await eventually { session.status == .finished })
    }

    // MARK: - Sessions that stay

    @Test func linesFromAFileThatIsNotTheSidecarSurviveASwitch() throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b")
        let other = lab.folder.appending(path: "elsewhere.en.srt")
        try Data("1\n00:00:00,000 --> 00:00:01,000\nHello.\n\n".utf8).write(to: other)
        vm.openVideo(a)
        vm.openSubtitle(other, findingVideo: false)
        #expect(vm.sentences.map(\.text) == ["Hello."] && vm.video == a)
        vm.openVideo(b)
        vm.openVideo(a)
        #expect(vm.sentences.map(\.text) == ["Hello."] && vm.loadedSubtitle == other)
    }

    @Test func anOfflineBlockedSessionDoesNotKeepItselfAliveThroughItsRetry() async throws {
        let models = """
        echo '{"event": "models", "models": [{"id": "large-v3", "size_mb": 3000, "speed": 1, "accuracy": 5, "downloaded": false}]}'
        """
        let (vm, lab) = try makeLab(models: models)
        await vm.catalog.load()
        vm.model = "large-v3"
        vm.offlineMode = true
        vm.openVideo(lab.video("a"))
        weak let weakSession = vm.foreground
        vm.generate()
        #expect(vm.offlineRetry != nil)
        vm.openVideo(lab.video("b"))
        #expect(vm.existingSession(for: lab.video("a")) == nil)
        #expect(weakSession == nil)
    }

    // MARK: - The queue

    @Test func theQueueShowsAPositionAndTheEditorOfAWaitingVideoSaysSo() async throws {
        let (vm, lab) = try makeLab()
        vm.openVideo(lab.video("a"))
        vm.generate()
        #expect(await eventually { lab.started("a") })
        #expect(!vm.isQueued && vm.queuePosition == nil)
        vm.openVideo(lab.video("b"))
        vm.generate()
        #expect(vm.isQueued && vm.queuePosition == 1 && vm.sentences.isEmpty)
        vm.openVideo(lab.video("c"))
        vm.generate()
        #expect(vm.queuePosition == 2)
        vm.cancel()
        #expect(!vm.isQueued && vm.queuePosition == nil)
        vm.openVideo(lab.video("b"))
        #expect(vm.queuePosition == 1)
        vm.clearQueue()
        vm.openVideo(lab.video("a"))
        vm.cancel()
    }

    @Test func requestingTheRunningVideoAgainReplacesItInPlaceAtTheHead() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b")
        vm.openVideo(a)
        vm.generate()
        #expect(await eventually { lab.started("a") })
        let sessionA = vm.foreground
        vm.openVideo(b)
        vm.generate()
        #expect(vm.jobQueue.waiting.compactMap(\.session.video) == [b])

        vm.language = "en"
        vm.request(.generate(thenTranslate: false), on: sessionA)
        // A starts over at once, with the settings of the new request; B still waits behind it.
        #expect(vm.jobQueue.isRunning(sessionA) && vm.jobQueue.waiting.compactMap(\.session.video) == [b])
        #expect(await eventually { lab.starts == ["a", "a"] })
        #expect(lab.lines().last { $0.hasPrefix("start a ") }?.contains("--language en") == true)
        try await Task.sleep(for: .milliseconds(150))
        #expect(!lab.started("b"))
        lab.release("a")
        #expect(await eventually { lab.started("b") })
        lab.release("b")
        let sessionB = try #require(vm.existingSession(for: b))
        #expect(await eventually { sessionB.status == .finished })
    }

    @Test func stoppingAWaitingJobPutsAFinishedVideoBackWithItsSummaryAndItsRow() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b")
        lab.release("a")
        vm.openVideo(a)
        vm.generate()
        let sessionA = vm.foreground
        #expect(await eventually { sessionA.status == .finished })
        vm.openVideo(b)
        vm.generate()
        #expect(await eventually { lab.started("b") })

        // A is asked for again, waits, and is stopped: it is finished again, not idle.
        vm.openVideo(a)
        vm.generate()
        #expect(vm.status == queued)
        vm.cancel()
        #expect(vm.status == .finished && sessionA.sentences.map(\.text) == ["Metin a"])
        #expect(vm.listRows.contains { $0.fileName == "a.mp4" && $0.state == .completed })

        // The same through "Sıradakileri temizle".
        vm.generate()
        #expect(vm.status == queued)
        vm.clearQueue()
        #expect(vm.status == .finished && vm.listRows.contains { $0.fileName == "a.mp4" })
        vm.openVideo(b)
        vm.cancel()
    }

    @Test func aCancelledBatchWaiterIsNotLeftHoldingTheLane() async {
        let queue = JobQueue()
        queue.start = { _ in }
        let session = VideoSession(video: URL(filePath: "/tmp/a.mp4"))
        let settings = JobSettings(
            language: "", model: "base", diarize: false, speakerCount: 0, translationTarget: "en",
            translationModel: "madlad-3b", autoSave: false, exportFormat: .srt
        )
        queue.submit(JobQueue.Entry(
            session: session, kind: .generate(thenTranslate: false), settings: settings, previousStatus: .idle
        ))
        let waiter = Task { await queue.acquireBatchSlot() }
        try? await Task.sleep(for: .milliseconds(50))
        // The cancel and the end of the running job arrive in the same turn: the lane is handed to a task that is going away.
        waiter.cancel()
        queue.jobEnded(session)
        #expect(await waiter.value == false)
        #expect(!queue.batchHoldsLane && !queue.isBusy)
    }

    // MARK: - Who counts as working, and the models

    @Test func quittingIsHeldOffByABackgroundOrWaitingJobEvenWhenTheVideoOnScreenIsIdle() async throws {
        let (vm, lab) = try makeLab()
        #expect(!vm.hasActiveWork)
        vm.openVideo(lab.video("a"))
        vm.generate()
        #expect(await eventually { lab.started("a") })
        vm.openVideo(lab.video("b"))
        #expect(!vm.isWorking && vm.hasActiveWork)
        vm.generate()
        vm.openVideo(lab.video("c"))
        #expect(!vm.isWorking && vm.hasActiveWork)
        lab.release("a", "b")
        #expect(await eventually { !vm.jobQueue.isBusy })
        #expect(!vm.hasActiveWork)
    }

    @Test func aModelStaysProtectedUntilItsJobEndsWhateverElseIsStopped() async throws {
        let (vm, lab) = try makeLab()
        vm.model = "base"
        vm.openVideo(lab.video("a"))
        vm.generate()
        #expect(await eventually { lab.started("a") })
        #expect(vm.catalog.activeModels == ["base"])
        vm.openVideo(lab.video("b"))
        vm.generate()
        vm.cancel()
        #expect(vm.catalog.activeModels == ["base"], "stopping a waiting job releases nothing of the running one")
        vm.openVideo(lab.video("a"))
        vm.cancel()
        #expect(vm.catalog.activeModels.isEmpty)
    }

    // MARK: - The external server translating in a video that is not on screen

    @Test func theExternalServersBackgroundPumpKeepsWorkingForAVideoThatLeftTheScreen() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "pump-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let lines = (1...12).map {
            #"{"id": \#($0), "start": \#($0), "end": \#($0 + 1), "text": "Satır \#($0).", "words": []}"#
        }.joined(separator: ", ")
        let engine = try FakeEngine.make(
            transcribe: #"""
            echo '{"event": "partial", "language": "tr", "sentences": [\#(lines)]}'
            while [ ! -e '\#(folder.path)/release' ]; do sleep 0.02; done
            echo '{"event": "result", "language": "tr", "duration": 14, "audio": "/nonexistent.wav", "outputs": [], "sentences": [\#(lines)]}'
            """#,
            translate: #"""
            printf '{"event": "translation", "language": "en", "sentences": %s}\n' "$(sed 's/"text":"\([^"]*\)"/"text":"T-\1"/g' '\#(folder.path)/req')"
            """#,
            translateInput: "cat > '\(folder.path)/req'"
        )
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine), defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: EngineLaunchState()
        )
        vm.offlineMode = false
        vm.diarize = false
        vm.autoSaveSubtitle = false
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmAsync = true
        vm.language = "tr"
        vm.translationTarget = "en"
        let a = folder.appending(path: "a.mp4"), b = folder.appending(path: "b.mp4")
        vm.openVideo(a)
        vm.generateAndTranslate()
        let sessionA = vm.foreground
        #expect(await eventually { sessionA.translations.count >= 10 })

        // Another video comes to the front while the server is still translating A's lines.
        vm.openVideo(b)
        vm.translationTarget = "de"
        #expect(vm.translations.isEmpty && !vm.isAsyncTranslating && !vm.isTranslationInProgress)
        FileManager.default.createFile(atPath: folder.appending(path: "release").path, contents: Data())
        #expect(await eventually { sessionA.status == .finished || sessionA.status == .idle })
        await sessionA.job?.value

        // The rest was translated in A, the toolbar kept what the user chose, B saw none of it.
        #expect(sessionA.translations.count == 12 && sessionA.translationLanguage == "en")
        #expect(vm.translationTarget == "de" && vm.translations.isEmpty && vm.sentences.isEmpty)
    }
}
