import EngineKit
import Foundation
import LibraryKit
import Testing
@testable import SubtitleAdderApp

/// Per-video sessions and the job queue: a job keeps running when another video is opened and writes into its own
/// session, single jobs wait in a FIFO queue, Stop and "Sıradakileri temizle" act on the right entries, the list tab
/// shows single jobs next to the playlist's items, and a playlist run works in sessions of its own.
@MainActor
@Suite struct SessionTests {
    // MARK: - A gated fake engine

    /// The fake engine's `transcribe` logs `start <stem>` / `end <stem>` to `calls.log` and waits there until the test
    /// creates `release-<stem>`; it emits a live line "Canlı <stem>" first and the final line "Metin <stem>" at the end.
    /// `translate` logs `translate <args>` and answers "Çeviri" for line 1 at once.
    struct Lab {
        let folder: URL
        var log: URL { folder.appending(path: "calls.log") }

        func video(_ stem: String) -> URL { folder.appending(path: "\(stem).mp4") }

        func release(_ stems: String...) {
            for stem in stems {
                FileManager.default.createFile(atPath: folder.appending(path: "release-\(stem)").path, contents: Data())
            }
        }

        func lines() -> [String] {
            ((try? String(contentsOf: log, encoding: .utf8)) ?? "").split(separator: "\n").map(String.init)
        }

        func started(_ stem: String) -> Bool { lines().contains { $0.hasPrefix("start \(stem) ") } }

        func ended(_ stem: String) -> Bool { lines().contains("end \(stem)") }

        /// The most transcriptions that ran at the same time, from the log.
        var peakTranscriptions: Int {
            var running = 0, peak = 0
            for line in lines() {
                if line.hasPrefix("start ") { running += 1; peak = max(peak, running) }
                if line.hasPrefix("end ") { running -= 1 }
            }
            return peak
        }

        var starts: [String] {
            lines().filter { $0.hasPrefix("start ") }.map { String($0.split(separator: " ")[1]) }
        }
    }

    private func makeLab() throws -> (SubtitleViewModel, Lab) {
        let folder = FileManager.default.temporaryDirectory.appending(path: "sessions-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let lab = Lab(folder: folder)
        let engine = try FakeEngine.make(
            transcribe: #"""
            stem=$(basename "$2" .mp4)
            echo "start $stem $*" >> '\#(lab.log.path)'
            echo '{"event": "stage", "stage": "transcribing", "fraction": 0.5}'
            echo "{\"event\": \"partial\", \"language\": \"tr\", \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Canlı $stem\", \"words\": []}]}"
            while [ ! -e "\#(folder.path)/release-$stem" ]; do sleep 0.02; done
            echo "end $stem" >> '\#(lab.log.path)'
            echo "{\"event\": \"result\", \"language\": \"tr\", \"duration\": 1, \"audio\": \"/nonexistent.wav\", \"outputs\": [], \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Metin $stem\", \"words\": []}]}"
            """#,
            translate: #"""
            echo "translate $*" >> '\#(lab.log.path)'
            echo '{"event": "translation", "language": "tr", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Çeviri", "words": []}]}'
            """#
        )
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine), defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: EngineLaunchState()
        )
        vm.offlineMode = false
        vm.diarize = false
        vm.autoSaveSubtitle = false
        vm.physicalMemory = SubtitleViewModel.localTranslationOverlapMemory
        return (vm, lab)
    }

    /// Polls a condition on the main actor.
    private func eventually(_ condition: () -> Bool) async -> Bool {
        for _ in 0..<400 {
            if condition() { return true }
            try? await Task.sleep(for: .milliseconds(25))
        }
        return condition()
    }

    private let queued = SubtitleViewModel.Status.working(.key("Sırada"), fraction: nil)

    // MARK: - Switching videos keeps the job

    @Test func switchingVideosKeepsTheJobRunningAndRestoresItsLiveStateOnReturn() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b")
        vm.openVideo(a)
        vm.generate()
        #expect(await eventually { vm.sentences.first?.text == "Canlı a" })

        // B opens fresh; A's job is still running in A's session.
        vm.openVideo(b)
        #expect(vm.video == b && vm.sentences.isEmpty && vm.status == .idle && !vm.isWorking)
        let sessionA = try #require(vm.existingSession(for: a))
        let job = try #require(sessionA.job)
        #expect(sessionA.isWorking)

        // Back on A: its live line, its progress, its status.
        vm.openVideo(a)
        #expect(vm.sentences.map(\.text) == ["Canlı a"] && vm.isWorking)
        guard case .working = vm.status else { Issue.record("expected a running status"); return }

        // The result arrives while B is on screen and lands in A only.
        vm.openVideo(b)
        lab.release("a")
        await job.value
        #expect(vm.sentences.isEmpty && vm.status == .idle && vm.detectedLanguage == nil)
        #expect(sessionA.sentences.map(\.text) == ["Metin a"] && sessionA.status == .finished)
        #expect(sessionA.detectedLanguage == "tr")

        vm.openVideo(a)
        #expect(vm.sentences.map(\.text) == ["Metin a"] && vm.status == .finished)
    }

    @Test func aJobFromOneVideoNeverTouchesTheToolbarOrTheOtherVideosDisplay() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b")
        vm.translationTarget = "es"
        vm.openVideo(a)
        vm.displayMode = .both
        vm.generate(thenTranslate: true)
        #expect(await eventually { lab.started("a") })
        vm.openVideo(b)
        // Display and export choices are per video; the new one starts from the remembered choice.
        #expect(vm.displayMode == .both)
        vm.displayMode = .original
        vm.translationTarget = "tr"
        lab.release("a")
        let sessionA = try #require(vm.existingSession(for: a))
        #expect(await eventually { sessionA.status == .finished })

        // A was translated into the target it was asked for, not the one the toolbar moved to meanwhile.
        #expect(lab.lines().contains { $0.hasPrefix("translate ") && $0.contains("--to es") })
        #expect(sessionA.translationLanguage == "tr" && sessionA.hasTranslations)
        #expect(vm.translationTarget == "tr" && vm.displayMode == .original && !vm.hasTranslations)
        vm.openVideo(a)
        #expect(vm.displayMode == .both && vm.exportLanguage == .translated)
    }

    // MARK: - The queue

    @Test func jobsWaitInFIFOOrderAndOneNewRequestPerVideoReplacesTheOldOne() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b"), c = lab.video("c")
        vm.openVideo(a)
        vm.generate()
        #expect(await eventually { lab.started("a") })

        vm.openVideo(b)
        vm.generate()
        #expect(vm.status == queued && vm.jobQueue.isQueued(vm.foreground) && vm.isWorking)
        vm.openVideo(c)
        vm.generate()
        #expect(vm.status == queued)
        #expect(vm.jobQueue.waiting.compactMap(\.session.video) == [b, c])

        // A second request for B replaces the first one and keeps its place; it carries the toolbar as of then.
        vm.openVideo(b)
        vm.language = "en"
        vm.generate()
        #expect(vm.jobQueue.waiting.compactMap(\.session.video) == [b, c])
        #expect(vm.jobQueue.waiting.first?.settings.language == "en")

        // One transcription at a time, in the order of the requests.
        try await Task.sleep(for: .milliseconds(200))
        #expect(lab.starts == ["a"])
        lab.release("a")
        #expect(await eventually { lab.started("b") })
        #expect(lab.lines().last { $0.hasPrefix("start b ") }?.contains("--language en") == true)
        try await Task.sleep(for: .milliseconds(150))
        #expect(!lab.started("c"))
        lab.release("b")
        #expect(await eventually { lab.started("c") })
        lab.release("c")
        let sessionC = try #require(vm.existingSession(for: c))
        #expect(await eventually { sessionC.status == .finished })
        #expect(lab.starts == ["a", "b", "c"] && lab.peakTranscriptions == 1)
        #expect(await eventually { !vm.jobQueue.isBusy })
    }

    @Test func stopOnAQueuedVideoRemovesOnlyItAndStopOnTheRunningOneLetsTheQueueGoOn() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b"), c = lab.video("c")
        vm.openVideo(a)
        vm.generate()
        #expect(await eventually { lab.started("a") })
        vm.openVideo(b)
        vm.generate()
        vm.openVideo(c)
        vm.generate()
        #expect(vm.jobQueue.waiting.compactMap(\.session.video) == [b, c])

        // Viewing the queued C: Stop takes C out and nothing else.
        vm.cancel()
        #expect(vm.status == .idle && !vm.isWorking)
        #expect(vm.jobQueue.waiting.compactMap(\.session.video) == [b])
        let sessionA = try #require(vm.existingSession(for: a))
        #expect(sessionA.isWorking && vm.jobQueue.isRunning(sessionA))

        // Viewing the running A: Stop ends A only, and B, which was waiting, takes the lane.
        vm.openVideo(a)
        vm.cancel()
        #expect(vm.status == .idle && !vm.isWorking && sessionA.job == nil)
        #expect(await eventually { lab.started("b") })
        let sessionB = try #require(vm.existingSession(for: b))
        #expect(sessionB.isWorking && vm.jobQueue.waiting.isEmpty)
        lab.release("b")
        #expect(await eventually { sessionB.status == .finished })
        #expect(lab.starts == ["a", "b"])
    }

    @Test func clearQueueDropsTheWaitingJobsAndLeavesTheRunningOne() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b"), c = lab.video("c")
        vm.openVideo(a)
        vm.generate()
        #expect(await eventually { lab.started("a") })
        vm.openVideo(b)
        vm.generate()
        vm.openVideo(c)
        vm.generate()
        #expect(vm.queuedJobCount == 2)

        vm.clearQueue()
        #expect(vm.queuedJobCount == 0 && vm.status == .idle)
        let sessionB = try #require(vm.existingSession(for: b))
        #expect(sessionB.status == .idle && !vm.isWorking)
        let sessionA = try #require(vm.existingSession(for: a))
        #expect(sessionA.isWorking && vm.jobQueue.isRunning(sessionA))
        lab.release("a")
        #expect(await eventually { sessionA.status == .finished })
        try await Task.sleep(for: .milliseconds(150))
        #expect(lab.starts == ["a"])
    }

    @Test func aQueuedTranslationAndARetranslateOfSelectedLinesWaitTooAndKeepTheirLines() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b")
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.openVideo(a)
        vm.generate()
        #expect(await eventually { lab.started("a") })

        vm.openVideo(b)
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Merhaba.")]
        vm.setSelectedLines([vm.sentences[0].uid], active: nil)
        vm.translateSelected()
        #expect(vm.status == queued)
        // The queued video can't be edited while it waits for its turn.
        #expect(vm.isWorking && !vm.canTranslateSelected)

        lab.release("a")
        let sessionB = try #require(vm.existingSession(for: b))
        #expect(await eventually { sessionB.status == .finished })
        #expect(sessionB.translations.values.map(\.text) == ["Çeviri"])
    }

    // MARK: - Saves

    @Test func theAutoSaveOfABackgroundJobWritesItsOwnVideosFileAndReportsItsURL() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b")
        vm.autoSaveSubtitle = true
        vm.translationTarget = "tr"
        var saved: [URL] = []
        vm.onSubtitleSaved = { saved.append($0) }
        vm.openVideo(a)
        vm.generateAndTranslate()
        #expect(await eventually { lab.started("a") })
        vm.openVideo(b)
        lab.release("a")
        let sessionA = try #require(vm.existingSession(for: a))
        #expect(await eventually { sessionA.savedSubtitleName == "a.srt" })

        #expect(saved == [lab.folder.appending(path: "a.srt")])
        #expect(try String(contentsOf: lab.folder.appending(path: "a.srt"), encoding: .utf8).contains("Çeviri"))
        #expect(!FileManager.default.fileExists(atPath: lab.folder.appending(path: "b.srt").path))
        #expect(sessionA.status == .finished && !sessionA.hasUnsavedEdits)
        // The video on screen has nothing to do with it.
        #expect(vm.video == b && vm.status == .idle && vm.savedSubtitleName == nil)
    }

    @Test func savingASessionThatIsNotOnScreenUsesThatSessionsLinesAndPath() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b")
        vm.openVideo(a)
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Birinci.")]
        let sessionA = vm.foreground
        vm.openVideo(b)
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "İkinci.")]

        let url = await vm.saveSubtitle(of: sessionA, format: .srt)
        #expect(url == lab.folder.appending(path: "a.srt"))
        let written = try String(contentsOf: lab.folder.appending(path: "a.srt"), encoding: .utf8)
        #expect(written.contains("Birinci.") && !written.contains("İkinci."))
        #expect(sessionA.savedSubtitleName == "a.srt" && vm.savedSubtitleName == nil)
    }

    // MARK: - The list tab

    @Test func singleJobsShowAsRowsWithTheirStateAndAClickOpensTheirSession() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b")
        #expect(vm.listRows.isEmpty && vm.playlistSummary == "Listede henüz video yok.")
        vm.openVideo(a)
        vm.generate()
        #expect(await eventually { lab.started("a") })
        vm.openVideo(b)
        vm.generate()
        vm.workspace = .playlist

        let rows = vm.listRows
        #expect(rows.map(\.fileName) == ["a.mp4", "b.mp4"])
        #expect(rows.map(\.state) == [.working, .pending])
        #expect(rows[1].message.text == "Sırada" && rows.allSatisfy { $0.canStop && $0.origin == .job })
        #expect(vm.playlistSummary == "0 / 2 tamamlandı")

        // A click on a row shows that video in the editor.
        vm.openListRow(rows[0])
        #expect(vm.workspace == .editor && vm.video == a && vm.isWorking)

        // Stop on a row cancels that video's job only.
        vm.workspace = .playlist
        vm.removeListRow(vm.listRows[1])
        #expect(vm.listRows.map(\.fileName) == ["a.mp4"] && vm.queuedJobCount == 0)

        lab.release("a")
        let sessionA = try #require(vm.existingSession(for: a))
        #expect(await eventually { sessionA.status == .finished })
        let done = try #require(vm.listRows.first)
        #expect(done.state == .completed && !done.canStop && done.message.text.hasPrefix("1 satır"))
        #expect(vm.playlistSummary == "1 / 1 tamamlandı")
        vm.clearPlaylist()
        #expect(vm.listRows.isEmpty)
    }

    @Test func aVideoThatIsAlsoAPlaylistItemShowsOnceAsTheItem() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a")
        vm.openVideo(a)
        vm.generate()
        #expect(await eventually { lab.started("a") })
        vm.addToPlaylist([a])
        #expect(vm.listRows.count == 1 && vm.listRows[0].playlistItemID != nil)
        vm.cancel()
    }

    // MARK: - Sessions that are kept and dropped

    @Test func sessionsWithContentAreKeptAndEmptyIdleOnesDropped() throws {
        let (vm, lab) = try makeLab()
        let empty = lab.video("empty"), lines = lab.video("lines"), sidecar = lab.video("sidecar")
        try Data("1\n00:00:00,000 --> 00:00:01,000\nMerhaba.\n".utf8).write(to: lab.folder.appending(path: "sidecar.srt"))

        vm.openVideo(empty)
        vm.openVideo(lines)
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Satır.")]
        vm.openVideo(sidecar)
        #expect(vm.sentences.count == 1 && vm.loadedSubtitle != nil)
        vm.openVideo(empty)

        // Nothing in it: gone. Typed-in lines: kept. A sidecar as it is on disk: it comes back the same, so it goes.
        #expect(vm.existingSession(for: lines) != nil)
        #expect(vm.existingSession(for: sidecar) == nil)
        vm.openVideo(lines)
        #expect(vm.existingSession(for: empty) == nil)
        #expect(vm.sentences.map(\.text) == ["Satır."])
        vm.openVideo(sidecar)
        #expect(vm.sentences.map(\.text) == ["Merhaba."])
    }

    @Test func aSessionWithARunningJobOrEditsStaysUntilItsWorkIsDone() async throws {
        let (vm, lab) = try makeLab()
        let a = lab.video("a"), b = lab.video("b")
        vm.openVideo(a)
        vm.generate()
        #expect(await eventually { lab.started("a") })
        vm.openVideo(b)
        // A has no lines worth keeping yet, but it has a job.
        #expect(vm.existingSession(for: a) != nil)
        lab.release("a")
        let sessionA = try #require(vm.existingSession(for: a))
        #expect(await eventually { sessionA.status == .finished })
        #expect(vm.existingSession(for: a) === sessionA)
    }

    @Test func linesOfASubtitleOnTheirOwnEndWithTheirJobWhenAnotherFileIsOpened() async throws {
        let (vm, lab) = try makeLab()
        let loose = lab.folder.appending(path: "loose.srt")
        try Data("1\n00:00:00,000 --> 00:00:01,000\nMerhaba.\n".utf8).write(to: loose)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.openSubtitle(loose)
        #expect(vm.video == nil && vm.sentences.count == 1)
        let orphan = vm.foreground
        vm.translate()
        #expect(vm.isWorking)
        vm.openVideo(lab.video("a"))
        #expect(orphan.job == nil && vm.video == lab.video("a"))
        #expect(await eventually { !vm.jobQueue.isBusy })
    }

    // MARK: - The playlist works in sessions of its own

    @Test func aPlaylistRunNeverTakesOverTheEditorTheToolbarOrThePlayersVideo() async throws {
        let (vm, lab) = try makeLab()
        let watched = lab.video("watched"), x = lab.video("x"), y = lab.video("y"), other = lab.video("other")
        vm.language = "tr"
        vm.translationTarget = "es"
        vm.openVideo(watched)
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "İzlenen.")]
        vm.addToPlaylist([x, y], sourceLanguage: "ja")
        vm.startPlaylist()
        #expect(await eventually { lab.started("x") })

        // The video on screen, its lines and the toolbar are the user's all along.
        #expect(vm.video == watched && vm.sentences.map(\.text) == ["İzlenen."] && !vm.isWorking)
        #expect(vm.language == "tr" && vm.translationTarget == "es" && vm.isPlayerShown)
        // The user opens another video and edits it while the run goes on.
        vm.openVideo(other)
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Başka.")]
        vm.setText("Başka, düzeltildi.", for: vm.sentences[0].uid)
        // Opening the item being recognized shows its live line and progress, in its own session.
        vm.openVideo(x)
        #expect(await eventually { vm.sentences.first?.text == "Canlı x" })
        #expect(vm.isWorking)

        lab.release("x", "y")
        await vm.playlistTask?.value
        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        #expect(vm.video == x && vm.sentences.map(\.text) == ["Metin x"] && vm.status == .finished)
        #expect(vm.savedSubtitleName == "x.srt")
        vm.openVideo(other)
        #expect(vm.sentences.map(\.text) == ["Başka, düzeltildi."] && vm.hasUnsavedEdits)
        vm.openVideo(watched)
        #expect(vm.sentences.map(\.text) == ["İzlenen."] && vm.status == .idle)
        #expect(vm.language == "tr" && vm.translationTarget == "es")
        #expect(lab.lines().filter { $0.hasPrefix("start ") }.allSatisfy { $0.contains("--language ja") })
    }

    @Test func aSingleJobStartedDuringARunWaitsForTheItemBeingRecognizedAndGoesBeforeTheNextOne() async throws {
        let (vm, lab) = try makeLab()
        let x = lab.video("x"), y = lab.video("y"), z = lab.video("z")
        vm.translationTarget = "es"
        vm.addToPlaylist([x, y], sourceLanguage: "tr")
        vm.startPlaylist()
        #expect(await eventually { lab.started("x") })

        vm.openVideo(z)
        vm.generate()
        #expect(vm.status == queued && vm.jobQueue.isQueued(vm.foreground))
        try await Task.sleep(for: .milliseconds(200))
        #expect(lab.starts == ["x"])

        // The run's recognition of x is over: the single job takes the engine before the run's next item.
        lab.release("x")
        #expect(await eventually { lab.started("z") })
        try await Task.sleep(for: .milliseconds(200))
        #expect(!lab.started("y"))
        lab.release("z")
        #expect(await eventually { lab.started("y") })
        lab.release("y")
        await vm.playlistTask?.value
        #expect(lab.starts == ["x", "z", "y"] && lab.peakTranscriptions == 1)
        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        #expect(vm.existingSession(for: z)?.status == .finished)
    }

    @Test func aSingleJobRunningWhenTheRunReachesItsRecognitionMakesTheRunWait() async throws {
        let (vm, lab) = try makeLab()
        let x = lab.video("x"), z = lab.video("z")
        vm.translationTarget = "es"
        vm.openVideo(z)
        vm.generate()
        #expect(await eventually { lab.started("z") })
        vm.addToPlaylist([x], sourceLanguage: "tr")
        vm.startPlaylist()
        try await Task.sleep(for: .milliseconds(250))
        #expect(lab.starts == ["z"])
        lab.release("z")
        #expect(await eventually { lab.started("x") })
        lab.release("x")
        await vm.playlistTask?.value
        #expect(lab.peakTranscriptions == 1 && vm.playlist[0].state == .completed)
    }

    @Test func stopOnAnItemOfTheRunStopsTheRunAndPutsItsItemsBack() async throws {
        let (vm, lab) = try makeLab()
        let x = lab.video("x"), y = lab.video("y")
        vm.translationTarget = "es"
        vm.addToPlaylist([x, y], sourceLanguage: "tr")
        vm.startPlaylist()
        #expect(await eventually { lab.started("x") })
        let task = vm.playlistTask
        vm.openVideo(x)
        #expect(vm.isWorking)
        vm.cancel()
        await task?.value
        #expect(!vm.isPlaylistRunning && !vm.isWorking && vm.status == .idle)
        #expect(vm.playlist.map(\.state) == [.pending, .pending])
        #expect(await eventually { !vm.jobQueue.isBusy })
    }

    @Test func aLineAlreadyInASessionIsUsedByTheRunAsItIs() async throws {
        let (vm, lab) = try makeLab()
        let x = lab.video("x")
        vm.translationTarget = "es"
        vm.openVideo(x)
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Elle yazılmış.")]
        vm.addToPlaylist([x], sourceLanguage: "tr")
        vm.startPlaylist()
        await vm.playlistTask?.value
        #expect(vm.playlist[0].state == .completed)
        #expect(lab.starts.isEmpty)
        #expect(vm.translations.values.map(\.text) == ["Çeviri"])
    }

    // MARK: - JobQueue on its own

    private func entry(_ stem: String, _ session: VideoSession? = nil) -> JobQueue.Entry {
        let settings = JobSettings(
            language: "", model: "base", diarize: false, speakerCount: 0, translationTarget: "en",
            translationModel: "madlad-3b", autoSave: false, exportFormat: .srt
        )
        return JobQueue.Entry(
            session: session ?? VideoSession(video: URL(filePath: "/tmp/\(stem).mp4")),
            kind: .generate(thenTranslate: false), settings: settings
        )
    }

    @Test func theQueueStartsTheFirstJobAndHandsTheLaneOnInOrder() {
        let queue = JobQueue()
        var started: [String] = []
        queue.start = { started.append($0.session.video?.lastPathComponent ?? "") }
        let a = entry("a"), b = entry("b"), c = entry("c")
        #expect(queue.submit(a) == .started && queue.submit(b) == .queued && queue.submit(c) == .queued)
        #expect(started == ["a.mp4"] && queue.isRunning(a.session) && queue.position(of: c.session) == 1)

        queue.jobEnded(b.session)
        #expect(started == ["a.mp4"], "only the running job can end")
        queue.jobEnded(a.session)
        #expect(started == ["a.mp4", "b.mp4"] && queue.waiting.count == 1)
        queue.jobEnded(b.session)
        queue.jobEnded(c.session)
        #expect(started == ["a.mp4", "b.mp4", "c.mp4"])
        queue.jobEnded(c.session)
        #expect(!queue.isBusy)
    }

    @Test func aNewRequestForAWaitingVideoReplacesItInPlaceAndNeverTheRunningOne() {
        let queue = JobQueue()
        queue.start = { _ in }
        let a = entry("a"), b = entry("b"), c = entry("c")
        queue.submit(a)
        queue.submit(b)
        queue.submit(c)
        let again = JobQueue.Entry(session: b.session, kind: .translate(reuseExisting: false, only: nil), settings: b.settings)
        #expect(queue.submit(again) == .queued)
        #expect(queue.waiting.map(\.id) == [again.id, c.id])
        #expect(queue.waiting.first?.kind == .translate(reuseExisting: false, only: nil))
    }

    @Test func removingAndClearingOnlyTouchTheWaitingEntries() {
        let queue = JobQueue()
        queue.start = { _ in }
        let a = entry("a"), b = entry("b"), c = entry("c")
        queue.submit(a)
        queue.submit(b)
        queue.submit(c)
        #expect(!queue.remove(a.session) && queue.isRunning(a.session))
        #expect(queue.remove(b.session) && !queue.remove(b.session))
        let cleared = queue.clearWaiting()
        #expect(cleared.count == 1 && cleared[0] === c.session)
        #expect(queue.waiting.isEmpty && queue.isRunning(a.session))
    }

    @Test func theBatchTakesTheLaneWhenItIsFreeAndWaitingJobsGoBeforeIt() async {
        let queue = JobQueue()
        var started: [String] = []
        queue.start = { started.append($0.session.video?.lastPathComponent ?? "") }
        #expect(await queue.acquireBatchSlot() && queue.batchHoldsLane)
        // Held by the batch: a job waits.
        let a = entry("a")
        #expect(queue.submit(a) == .queued && started.isEmpty)
        let waiter = Task { await queue.acquireBatchSlot() }
        try? await Task.sleep(for: .milliseconds(50))
        queue.releaseBatchSlot()
        // The job goes first; the batch's next item gets the lane once the job is over.
        #expect(started == ["a.mp4"] && !queue.batchHoldsLane)
        queue.jobEnded(a.session)
        #expect(await waiter.value && queue.batchHoldsLane)
        queue.releaseBatchSlot()
        #expect(!queue.isBusy)
    }

    @Test func aBatchThatWaitsForTheLaneLeavesWhenItsTaskIsCancelled() async {
        let queue = JobQueue()
        queue.start = { _ in }
        let a = entry("a")
        queue.submit(a)
        let waiter = Task { await queue.acquireBatchSlot() }
        try? await Task.sleep(for: .milliseconds(50))
        waiter.cancel()
        #expect(await waiter.value == false && !queue.batchHoldsLane)
        queue.jobEnded(a.session)
        #expect(!queue.isBusy)
    }
}
