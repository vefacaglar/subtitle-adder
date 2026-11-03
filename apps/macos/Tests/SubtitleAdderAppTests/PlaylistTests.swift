import EngineKit
import Foundation
import LibraryKit
import Testing
@testable import SubtitleAdderApp

/// The batch playlist: items, the run (recognition one video at a time, translation handed off behind it), cancel,
/// retry, offline block, drops and the async mode with an external LLM. The three-stage pipeline (audio extraction
/// ahead of time, recognition, translation) is at the end of the file.
@MainActor
@Suite struct PlaylistTests {
    private func makeFolder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "playlist-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func makeModel(engine: EngineClient? = nil) -> (SubtitleViewModel, UserDefaults) {
        let defaults = UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine), defaults: defaults, launchState: EngineLaunchState()
        )
        vm.offlineMode = false
        // The translation of one item overlaps the recognition of the next (the low-memory tests do it the other way).
        vm.physicalMemory = SubtitleViewModel.localTranslationOverlapMemory
        return (vm, defaults)
    }

    private func makeModelWithLaunchState(engine: EngineClient) -> (SubtitleViewModel, EngineLaunchState) {
        let state = EngineLaunchState()
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine),
            defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: state
        )
        vm.offlineMode = false
        vm.physicalMemory = SubtitleViewModel.localTranslationOverlapMemory
        return (vm, state)
    }

    private func sentenceJSON(_ id: Int, _ text: String) -> String {
        #"{"id": \#(id), "start": \#(id), "end": \#(id + 1), "text": "\#(text)", "words": []}"#
    }

    /// `count` engine-format lines "Satır 1.", "Satır 2.", ….
    private func sentenceList(_ count: Int) -> String {
        (1...count).map { sentenceJSON($0, "Satır \($0).") }.joined(separator: ", ")
    }

    private func lineTexts(_ range: ClosedRange<Int>) -> [String] {
        range.map { "Satır \($0)." }
    }

    private func finished(_ text: String) -> String {
        """
        echo '{"event": "result", "language": "xx", "duration": 1, "audio": "/nonexistent.wav", "outputs": [], "sentences": [{"id": 1, "start": 0, "end": 1, "text": "\(text)", "words": []}]}'
        """
    }

    private func translated(_ text: String) -> String {
        """
        echo '{"event": "translation", "language": "tr", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "\(text)", "words": []}]}'
        """
    }

    /// Polls a condition on the main actor.
    private func eventually(_ condition: () -> Bool) async -> Bool {
        for _ in 0..<400 {
            if condition() { return true }
            try? await Task.sleep(for: .milliseconds(25))
        }
        return condition()
    }

    private func logLines(_ log: URL) -> [String] {
        ((try? String(contentsOf: log, encoding: .utf8)) ?? "").split(separator: "\n").map(String.init)
    }

    private func run(_ vm: SubtitleViewModel) async {
        vm.startPlaylist()
        await vm.playlistTask?.value
    }

    /// Playlist results are always `<stem>.<fmt>`, with no language suffix.
    private func savedName(_ stem: String, _ format: SubtitleFormat = .srt) -> String {
        SubtitleViewModel.subtitleFileName(stem: stem, format: format)
    }

    // MARK: - The Windows flow

    @Test func processesVideosWithPerFileLanguagesAndSavesToEachFolder() async throws {
        let root = try makeFolder()
        let dirA = root.appending(path: "folderA")
        let dirB = root.appending(path: "folderB")
        for dir in [dirA, dirB] { try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true) }
        let log = root.appending(path: "calls.log")
        let engine = try FakeEngine.make(
            transcribe: """
            echo "transcribe $*" >> "\(log.path)"
            case "$2" in
            *ClipA*) \(finished("Caralho!"));;
            *) \(finished("こんにちは"));;
            esac
            """,
            translate: """
            echo "translate $*" >> "\(log.path)"
            case "$*" in
            *"--from pt"*) \(translated("Hassiktir!"));;
            *) \(translated("Merhaba"));;
            esac
            """
        )
        let (vm, defaults) = makeModel(engine: engine)
        // Auto-save is off: the playlist saves regardless.
        vm.autoSaveSubtitle = false
        vm.translationTarget = "tr"
        let videoA = dirA.appending(path: "ClipA.mp4")
        let videoB = dirB.appending(path: "ClipB.mkv")

        vm.addToPlaylist([videoA, videoB])
        #expect(vm.playlist.count == 2)
        vm.setAllPlaylistLanguages("")
        #expect(vm.playlist.allSatisfy { $0.sourceLanguage.isEmpty })
        vm.setPlaylistItemLanguage(vm.playlist[0].id, to: "pt")
        vm.setPlaylistItemLanguage(vm.playlist[1].id, to: "ja")
        #expect(vm.playlist.map(\.sourceLanguage) == ["pt", "ja"])

        vm.startPlaylist()
        #expect(vm.isPlaylistRunning && vm.isWorking)
        await vm.playlistTask?.value

        #expect(!vm.isPlaylistRunning && !vm.isWorking)
        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        #expect(vm.playlistSummary == "2 / 2 tamamlandı")
        // Pipelined: the videos are transcribed one after the other, and a translation starts after its own video was
        // transcribed (it may overlap the next video's transcription); the translations keep the list's order.
        let lines = logLines(log)
        let transcriptions = lines.filter { $0.hasPrefix("transcribe") }
        let translations = lines.filter { $0.hasPrefix("translate") }
        #expect(transcriptions.count == 2 && translations.count == 2)
        #expect(transcriptions[0].contains(videoA.path) && transcriptions[0].contains("--language pt"))
        #expect(transcriptions[1].contains(videoB.path) && transcriptions[1].contains("--language ja"))
        #expect(translations[0].contains("--to tr") && translations[0].contains("--from pt"))
        #expect(translations[1].contains("--to tr") && translations[1].contains("--from ja"))
        let firstTranslation = try #require(lines.firstIndex { $0.hasPrefix("translate") })
        let firstTranscription = try #require(lines.firstIndex { $0.hasPrefix("transcribe") })
        #expect(firstTranscription < firstTranslation)

        let savedA = try String(contentsOf: dirA.appending(path: savedName("ClipA")), encoding: .utf8)
        let savedB = try String(contentsOf: dirB.appending(path: savedName("ClipB")), encoding: .utf8)
        #expect(savedA.contains("Hassiktir!") && !savedA.contains("Caralho"))
        #expect(savedB.contains("Merhaba"))
        #expect(vm.playlist[0].statusMessage.text == "Kaydedildi: \(savedName("ClipA"))")

        // The per-item languages were the run's, not the user's.
        #expect(vm.language.isEmpty && vm.translationTarget == "tr")
        #expect((defaults.string(forKey: "selectedLanguage") ?? "").isEmpty)
        #expect(defaults.string(forKey: "translationTarget") == "tr")
    }

    @Test func aSidecarSkipsTranscriptionAndAutoSaveDoesNotSaveTwice() async throws {
        let folder = try makeFolder()
        let log = folder.appending(path: "calls.log")
        try Data("1\n00:00:00,000 --> 00:00:01,000\nHola.\n".utf8).write(to: folder.appending(path: "clip.srt"))
        let engine = try FakeEngine.make(
            transcribe: "echo transcribe >> \"\(log.path)\"\n\(finished("Nope"))",
            translate: "echo translate >> \"\(log.path)\"\n\(translated("Merhaba"))"
        )
        let (vm, _) = makeModel(engine: engine)
        vm.autoSaveSubtitle = true
        vm.translationTarget = "tr"
        vm.addToPlaylist([folder.appending(path: "clip.mp4")], sourceLanguage: "es")
        await run(vm)

        #expect(vm.playlist[0].state == .completed)
        #expect(logLines(log) == ["translate"])
        let names = try FileManager.default.contentsOfDirectory(atPath: folder.path).sorted()
        // The translation replaced the sidecar: one file, `<stem>.srt`, like on Windows.
        #expect(names == ["calls.log", "clip.srt"])
        #expect(try String(contentsOf: folder.appending(path: "clip.srt"), encoding: .utf8).contains("Merhaba"))
    }

    // MARK: - Failure, retry, cancel

    @Test func aFailedItemIsRetriedByTheNextRunNotAgainByThisOne() async throws {
        let folder = try makeFolder()
        let marker = folder.appending(path: "fixed")
        let engine = try FakeEngine.make(
            transcribe: """
            case "$2" in
            *bad*) if [ ! -f "\(marker.path)" ]; then echo '{"event": "error", "message": "boom"}'; exit 1; fi;;
            esac
            \(finished("Merhaba"))
            """,
            translate: translated("Hello")
        )
        let (vm, _) = makeModel(engine: engine)
        vm.translationTarget = "tr"
        vm.addToPlaylist([folder.appending(path: "bad.mp4"), folder.appending(path: "good.mp4")])
        await run(vm)

        #expect(vm.playlist.map(\.state) == [.failed, .completed])
        #expect(vm.playlist[0].statusMessage.text == "boom")
        #expect(vm.playlistSummary == "1 / 2 tamamlandı")
        #expect(vm.canStartPlaylist)

        try Data().write(to: marker)
        await run(vm)
        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        #expect(!vm.canStartPlaylist)
    }

    @Test func aMissingFolderFailsTheItem() async throws {
        let root = try makeFolder()
        let engine = try FakeEngine.make(transcribe: finished("Merhaba"), translate: translated("Hello"))
        let (vm, _) = makeModel(engine: engine)
        vm.translationTarget = "tr"
        vm.addToPlaylist([root.appending(path: "gone/clip.mp4")])
        await run(vm)
        #expect(vm.playlist[0].state == .failed)
        #expect(vm.playlist[0].statusMessage.text.hasPrefix("Dosya kaydedilemedi:"))
    }

    @Test func cancelPutsTheCurrentItemBackToQueuedAndRestoresTheToolbar() async throws {
        let folder = try makeFolder()
        let started = folder.appending(path: "started")
        let engine = try FakeEngine.make(transcribe: "touch \"\(started.path)\"\nexec sleep 20")
        let (vm, defaults) = makeModel(engine: engine)
        vm.translationTarget = "tr"
        vm.addToPlaylist([folder.appending(path: "a.mp4"), folder.appending(path: "b.mp4")], sourceLanguage: "ja")
        vm.startPlaylist()
        let task = vm.playlistTask
        let working = await eventually { vm.playlist[0].state == .working && FileManager.default.fileExists(atPath: started.path) }
        #expect(working)
        #expect(vm.language == "ja")

        vm.cancel()
        await task?.value

        #expect(!vm.isPlaylistRunning && !vm.isWorking)
        #expect(vm.playlist.map(\.state) == [.pending, .pending])
        #expect(vm.playlist[0].statusMessage.text == "Sırada")
        #expect(vm.status == .idle)
        #expect(vm.language.isEmpty)
        #expect((defaults.string(forKey: "selectedLanguage") ?? "").isEmpty)
        #expect(vm.canStartPlaylist)
    }

    // MARK: - Offline

    @Test func aMissingModelBlocksTheRunWhileOffline() async throws {
        let engine = try FakeEngine.make(
            models: """
            echo '{"event": "models", "models": [\
            {"id": "large-v3", "size_mb": 3000, "speed": 1, "accuracy": 5, "downloaded": false},\
            {"id": "speaker-diarization", "kind": "diarization", "backend": "sherpa", "size_mb": 29, "speed": 4, "accuracy": 3, "downloaded": true},\
            {"id": "madlad-3b", "kind": "translation", "backend": "ctranslate2", "size_mb": 2839, "speed": 2, "accuracy": 5, "downloaded": true}\
            ]}'
            """,
            transcribe: finished("Merhaba"), translate: translated("Hello")
        )
        let (vm, _) = makeModel(engine: engine)
        await vm.catalog.load()
        vm.offlineMode = true
        vm.diarize = false
        vm.model = "large-v3"
        let folder = try makeFolder()
        vm.addToPlaylist([folder.appending(path: "a.mp4")])
        vm.startPlaylist()

        #expect(!vm.isPlaylistRunning && vm.playlistTask == nil)
        #expect(vm.canGoOnlineAndRetry)
        #expect(vm.playlist[0].state == .pending)
        guard case .failed(let message) = vm.status else { Issue.record("expected a failure"); return }
        #expect(message.text == "En doğru indirilmemiş. Çevrimdışı mod açıkken model indirilemez.")

        vm.goOnlineAndRetry()
        #expect(!vm.offlineMode && vm.isPlaylistRunning)
        await vm.playlistTask?.value
        #expect(vm.playlist[0].state == .completed)
    }

    // MARK: - Items

    @Test func addingSkipsWaitingDuplicatesButAllowsFinishedOnes() {
        let (vm, _) = makeModel()
        vm.language = "pt"
        let a = URL(filePath: "/tmp/a.mp4")
        vm.addToPlaylist([a, URL(filePath: "/tmp/b.mp4"), a])
        #expect(vm.playlist.map(\.fileName) == ["a.mp4", "b.mp4"])
        // The toolbar language is the default for new items; an unknown code means automatic.
        #expect(vm.playlist.map(\.sourceLanguage) == ["pt", "pt"])
        vm.addToPlaylist([URL(filePath: "/tmp/c.mp4")], sourceLanguage: "klingon")
        #expect(vm.playlist[2].sourceLanguage.isEmpty)

        vm.addToPlaylist([a])
        #expect(vm.playlist.count == 3)
        vm.updatePlaylistItem(vm.playlist[0].id, .completed, .key("Sırada"), 1)
        vm.addToPlaylist([a])
        #expect(vm.playlist.count == 4)
        #expect(vm.playlistSummary == "1 / 4 tamamlandı")
        vm.clearPlaylist()
        #expect(vm.playlist.isEmpty && vm.playlistSummary == "Listede henüz video yok.")
    }

    @Test func aWorkingItemKeepsItsLanguageAndStaysInTheListWhileRunning() {
        let (vm, _) = makeModel()
        vm.addToPlaylist([URL(filePath: "/tmp/a.mp4"), URL(filePath: "/tmp/b.mp4")], sourceLanguage: "es")
        let working = vm.playlist[0].id
        let waiting = vm.playlist[1].id
        vm.updatePlaylistItem(working, .working, .key("Başlıyor…"), 0.3)
        vm.isPlaylistRunning = true
        defer { vm.isPlaylistRunning = false }

        vm.setPlaylistItemLanguage(working, to: "en")
        vm.setAllPlaylistLanguages("ru")
        #expect(vm.playlist.map(\.sourceLanguage) == ["es", "ru"])
        #expect(!vm.canStartPlaylist)
        vm.removeFromPlaylist(working)
        #expect(vm.playlist.count == 2)
        vm.removeFromPlaylist(waiting)
        #expect(vm.playlist.map(\.id) == [working])
        vm.addToPlaylist([URL(filePath: "/tmp/c.mp4")])
        vm.clearPlaylist()
        #expect(vm.playlist.map(\.id) == [working])
    }

    @Test func playlistValuesAreNotPersistedWhileRunning() {
        let (vm, defaults) = makeModel()
        vm.language = "tr"
        vm.translationTarget = "es"
        vm.isPlaylistRunning = true
        vm.language = "ja"
        vm.translationTarget = "de"
        vm.model = "base"
        vm.isPlaylistRunning = false
        #expect(defaults.string(forKey: "selectedLanguage") == "tr")
        #expect(defaults.string(forKey: "translationTarget") == "es")
        #expect(defaults.string(forKey: "selectedModel") != "base")
    }

    // MARK: - Dropping videos

    @Test func severalVideosOrAnyVideoOnThePlaylistGoToThePlaylist() {
        let (vm, _) = makeModel()
        let a = URL(filePath: "/tmp/a.mp4")
        let b = URL(filePath: "/tmp/b.mp4")
        vm.openFiles([a])
        #expect(vm.video == a && vm.playlist.isEmpty && vm.workspace == .editor)

        vm.openFiles([a, b])
        #expect(vm.playlist.count == 2 && vm.workspace == .playlist)

        vm.workspace = .editor
        vm.playlist = []
        vm.workspace = .playlist
        vm.openFiles([b])
        #expect(vm.playlist.map(\.fileName) == ["b.mp4"])

        // While a run owns the editor, a video dropped on the editor tab goes to the list too.
        vm.workspace = .editor
        vm.isPlaylistRunning = true
        defer { vm.isPlaylistRunning = false }
        vm.openFiles([URL(filePath: "/tmp/c.mp4")])
        #expect(vm.video == a && vm.workspace == .playlist && vm.playlist.count == 2)
    }

    // MARK: - Async mode (external LLM)

    @Test func asyncModeTranslatesInTheBackgroundOneAtATimeWhileTheNextVideoTranscribes() async throws {
        let folder = try makeFolder()
        let log = folder.appending(path: "calls.log")
        let engine = try FakeEngine.make(
            transcribe: "echo \"transcribe $2\" >> \"\(log.path)\"\n\(finished("Merhaba"))",
            translate: """
            echo "translate-start" >> "\(log.path)"
            sleep 1
            echo "translate-end" >> "\(log.path)"
            \(translated("Hello"))
            """
        )
        let (vm, _) = makeModel(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmAsync = true
        vm.translationTarget = "en"
        let a = folder.appending(path: "a.mp4")
        let b = folder.appending(path: "b.mp4")
        vm.addToPlaylist([a, b], sourceLanguage: "tr")
        await run(vm)

        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        let lines = logLines(log)
        #expect(lines.filter { $0.hasPrefix("translate") } == ["translate-start", "translate-end", "translate-start", "translate-end"])
        // The second video was transcribed before the first translation was done.
        let secondTranscription = try #require(lines.firstIndex { $0 == "transcribe \(b.path)" })
        let firstEnd = try #require(lines.firstIndex(of: "translate-end"))
        #expect(secondTranscription < firstEnd)
        for stem in ["a", "b"] {
            let text = try String(contentsOf: folder.appending(path: savedName(stem)), encoding: .utf8)
            #expect(text.contains("Hello"))
        }
        #expect(vm.status == .finished)
    }

    @Test func asyncModeKeepsTheReviewMarkerInTheSavedJSON() async throws {
        let folder = try makeFolder()
        let engine = try FakeEngine.make(
            transcribe: finished("Merhaba"),
            translate: """
            echo '{"event": "translation", "language": "en", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Merhaba", "words": [], "needs_review": true}]}'
            """
        )
        let (vm, _) = makeModel(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmAsync = true
        vm.translationTarget = "en"
        vm.exportFormat = .json
        vm.addToPlaylist([folder.appending(path: "a.mp4")], sourceLanguage: "tr")
        await run(vm)
        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        // The fake renderer echoes the lines it was sent as JSON.
        let saved = try String(contentsOf: folder.appending(path: savedName("a", .json)), encoding: .utf8)
        #expect(saved.contains("\"needs_review\":true"))
    }

    @Test func theEditorBadgeShowsWhilePlaylistTranslationsRunInTheBackground() async throws {
        let folder = try makeFolder()
        let engine = try FakeEngine.make(
            transcribe: finished("Merhaba"), translate: "sleep 1\n\(translated("Hello"))"
        )
        let (vm, _) = makeModel(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmAsync = true
        vm.translationTarget = "en"
        vm.addToPlaylist([folder.appending(path: "a.mp4"), folder.appending(path: "b.mp4")], sourceLanguage: "tr")
        #expect(!vm.isTranslationInProgress && vm.translationBadgeText == nil)
        vm.startPlaylist()
        let task = vm.playlistTask
        // A hand-off sets the flag even though neither the foreground job flag nor the live translation is on.
        #expect(await eventually { vm.isPlaylistBackgroundTranslating && !vm.isAsyncTranslating && !vm.isTranslationJobRunning })
        #expect(vm.isTranslationInProgress)
        #expect(vm.translationBadgeText != nil)
        await task?.value
        #expect(!vm.isPlaylistBackgroundTranslating)
        #expect(!vm.isTranslationInProgress)
        #expect(vm.translationBadgeText == nil)
    }

    @Test func cancelClearsThePlaylistBackgroundTranslationFlag() async throws {
        let folder = try makeFolder()
        let started = folder.appending(path: "translating")
        let engine = try FakeEngine.make(
            transcribe: finished("Merhaba"), translate: "touch \"\(started.path)\"\nexec sleep 20"
        )
        let (vm, _) = makeModel(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmAsync = true
        vm.translationTarget = "en"
        vm.addToPlaylist([folder.appending(path: "a.mp4")], sourceLanguage: "tr")
        vm.startPlaylist()
        let task = vm.playlistTask
        #expect(await eventually { FileManager.default.fileExists(atPath: started.path) })
        #expect(vm.isPlaylistBackgroundTranslating)
        vm.cancel()
        await task?.value
        #expect(!vm.isPlaylistBackgroundTranslating && !vm.isTranslationInProgress)
    }

    @Test func asyncModeFailsAnItemWhoseFolderIsMissing() async throws {
        let root = try makeFolder()
        let engine = try FakeEngine.make(transcribe: finished("Merhaba"), translate: translated("Hello"))
        let (vm, _) = makeModel(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmAsync = true
        vm.translationTarget = "en"
        vm.addToPlaylist([root.appending(path: "gone/clip.mp4"), root.appending(path: "ok.mp4")], sourceLanguage: "tr")
        await run(vm)
        #expect(vm.playlist.map(\.state) == [.failed, .completed])
        #expect(vm.playlist[0].statusMessage.text.hasPrefix("Dosya kaydedilemedi:"))
    }

    @Test func cancelStopsBackgroundTranslations() async throws {
        let folder = try makeFolder()
        let started = folder.appending(path: "translating")
        let engine = try FakeEngine.make(
            transcribe: finished("Merhaba"), translate: "touch \"\(started.path)\"\nexec sleep 20"
        )
        let (vm, _) = makeModel(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmAsync = true
        vm.translationTarget = "en"
        vm.addToPlaylist([folder.appending(path: "a.mp4")], sourceLanguage: "tr")
        vm.startPlaylist()
        let task = vm.playlistTask
        #expect(await eventually { FileManager.default.fileExists(atPath: started.path) })
        vm.cancel()
        await task?.value
        #expect(vm.playlist[0].state == .pending)
        #expect(!vm.isWorking)
        #expect(!FileManager.default.fileExists(atPath: folder.appending(path: savedName("a")).path))
    }

    @Test func cancelDuringTheSaveRenderPutsTheItemBackToQueued() async throws {
        let folder = try makeFolder()
        let started = folder.appending(path: "rendering")
        let engine = try FakeEngine.make(
            transcribe: finished("Merhaba"), translate: translated("Hello"),
            render: "touch \"\(started.path)\"\nexec sleep 20"
        )
        let (vm, _) = makeModel(engine: engine)
        vm.autoSaveSubtitle = false
        vm.translationTarget = "en"
        vm.addToPlaylist([folder.appending(path: "a.mp4")], sourceLanguage: "tr")
        vm.startPlaylist()
        let task = vm.playlistTask
        #expect(await eventually { FileManager.default.fileExists(atPath: started.path) })

        vm.cancel()
        await task?.value

        #expect(vm.playlist[0].state == .pending)
        #expect(vm.playlist[0].statusMessage.text == "Sırada")
        #expect(vm.status == .idle)
        #expect(!vm.isWorking)
    }

    @Test func theEditorKeepsTheLastItemsLanguageAfterTheRun() async throws {
        let folder = try makeFolder()
        let engine = try FakeEngine.make(transcribe: finished("Hello"), translate: translated("Hola"))
        let (vm, _) = makeModel(engine: engine)
        vm.language = "tr"
        vm.translationTarget = "es"
        vm.addToPlaylist([folder.appending(path: "a.mp4")], sourceLanguage: "en")
        await run(vm)

        // The toolbar is the user's again, but the lines left in the editor are English.
        #expect(vm.language == "tr" && vm.translationTarget == "es")
        #expect(!vm.sentences.isEmpty)
        #expect(vm.translationSource == "en")
        // Other lines, or a language picked by hand, are the toolbar's business again.
        vm.openVideo(folder.appending(path: "other.mp4"))
        #expect(vm.translationSource == "tr")
    }

    // MARK: - Pinned settings

    @Test func backgroundTranslationsLaunchWithTheServerChosenAtHandOff() async throws {
        let folder = try makeFolder()
        let log = folder.appending(path: "urls.log")
        let engine = try FakeEngine.make(
            transcribe: finished("Merhaba"),
            translate: """
            echo "url ${SUBTITLE_ENGINE_EXTERNAL_LLM_URL:-none}" >> "\(log.path)"
            \(translated("Hello"))
            """
        )
        // The view model's own launch state is not the process-wide one the client reads when nothing is pinned, so
        // "none" would show up here if the hand-off didn't carry the settings along.
        let (vm, _) = makeModelWithLaunchState(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmAsync = true
        vm.translationTarget = "en"
        vm.addToPlaylist([folder.appending(path: "a.mp4"), folder.appending(path: "b.mp4")], sourceLanguage: "tr")
        await run(vm)

        #expect(logLines(log) == ["url http://10.0.0.2:1234", "url http://10.0.0.2:1234"])
        #expect(vm.playlist.allSatisfy { $0.state == .completed })
    }

    @Test func aPinnedClientIgnoresLaterSettingsChanges() throws {
        let engine = try FakeEngine.make()
        let (vm, state) = makeModelWithLaunchState(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmModel = "gemma4:e4b"
        let pinned = vm.pinningLaunchSettings(engine)

        vm.externalLlmEnabled = false
        vm.offlineMode = true
        let now = state.settings
        #expect(now.externalLLM == nil && now.offline)
        #expect(pinned.launchSettings == EngineLaunchSettings(
            offline: false, externalLLM: ExternalLLM(url: "10.0.0.2:1234", model: "gemma4:e4b")
        ))
        // An unpinned client follows the settings.
        #expect(engine.launchSettings == nil)
    }

    // MARK: - Pre-translation and the editor in async mode

    @Test func asyncModeTranslatesWhileTranscribingAndOnlySendsTheRestAtHandOff() async throws {
        let folder = try makeFolder()
        let log = try makeFolder()
        let dir = log.path
        let engine = try FakeEngine.make(
            transcribe: """
            echo '{"event": "partial", "language": "tr", "sentences": [\(sentenceList(12))]}'
            i=0; while [ ! -e '\(dir)/req-0' ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done
            echo '{"event": "result", "language": "tr", "duration": 14, "audio": "/nonexistent.wav", "outputs": [], "sentences": [\(sentenceList(13))]}'
            """,
            // Each answer is tagged with the number of the request, so it shows which request a line came from.
            translate: #"printf '{"event": "translation", "language": "en", "sentences": %s}\n' "$(sed "s/\"text\":\"\([^\"]*\)\"/\"text\":\"T$n-\1\"/g" "\#(dir)/req-$n")""#,
            translateInput: "n=$(ls '\(dir)' | wc -l | tr -d ' '); cat > '\(dir)/req-'$n"
        )
        let (vm, _) = makeModel(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmAsync = true
        vm.diarize = false
        vm.translationTarget = "en"
        let video = folder.appending(path: "clip.mp4")
        vm.addToPlaylist([video], sourceLanguage: "tr")
        await run(vm)

        // Ten lines went out while transcribing (the held-back two ride along as context); at hand-off only the other
        // three were requested, with two lines of context before them.
        let requests = try FileManager.default.contentsOfDirectory(atPath: dir).sorted().map { name -> [String] in
            try JSONDecoder().decode([Sentence].self, from: Data(contentsOf: log.appending(path: name))).map(\.text)
        }
        #expect(requests == [lineTexts(1...12), lineTexts(9...13)])
        #expect(vm.playlist[0].state == .completed)
        let saved = try String(contentsOf: folder.appending(path: savedName("clip")), encoding: .utf8)
        #expect(lineTexts(1...10).allSatisfy { saved.contains("T0-" + $0) })
        #expect(lineTexts(11...13).allSatisfy { saved.contains("T1-" + $0) })
        #expect(!saved.contains("T1-Satır 9.") && !saved.contains("T1-Satır 10."))

        // The editor still shows this video, now with its translation and the saved name.
        #expect(vm.video == video)
        let expected = lineTexts(1...10).map { "T0-" + $0 } + lineTexts(11...13).map { "T1-" + $0 }
        #expect(vm.sentences.compactMap { vm.translation(for: $0)?.text } == expected)
        #expect(vm.effectiveDisplayMode == .translation)
        #expect(vm.savedSubtitleName == savedName("clip"))
        #expect(vm.status == .finished)
    }
}

// MARK: - The three-stage pipeline

/// The stages overlap across videos: audio extraction (one at a time, one video ahead), recognition (one video at a
/// time) and translation plus save (one at a time, behind recognition). The fake engine's three subcommands log
/// `<phase>-start <stem> …` and `<phase>-end <stem>` lines to `calls.log` and, for the phases in `gated`, wait there
/// until the test creates a file `<phase>-<stem>` in the control folder, so a test decides when each step finishes.
/// Every process also writes its pid to `pid-<phase>-<stem>`. Videos are named `<stem>.mp4`; the recognized text is
/// "Metin <stem>" and the translation "Çeviri <stem>".
extension PlaylistTests {
    private struct Control {
        let folder: URL
        var log: URL { folder.appending(path: "calls.log") }

        func release(_ names: String...) {
            for name in names { FileManager.default.createFile(atPath: folder.appending(path: name).path, contents: Data()) }
        }

        func pid(_ name: String) -> Int32? {
            (try? String(contentsOf: folder.appending(path: "pid-" + name), encoding: .utf8))
                .flatMap { Int32($0.trimmingCharacters(in: .whitespacesAndNewlines)) }
        }
    }

    private func makeControl() throws -> Control {
        let folder = try makeFolder().appending(path: "control")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return Control(folder: folder)
    }

    private func gatedEngine(
        _ control: Control, gated: Set<String> = ["extract", "transcribe", "translate"],
        extractFails: Bool = false, transcribeFails: Bool = false
    ) throws -> EngineClient {
        func start(_ phase: String, _ detail: String) -> String {
            """
            echo $$ > '\(control.folder.path)/pid-\(phase)-'$stem
            echo "\(phase)-start $stem \(detail)" >> '\(control.log.path)'
            """
        }
        func hold(_ phase: String) -> String {
            let wait = gated.contains(phase)
                ? "i=0; while [ ! -e '\(control.folder.path)/\(phase)-'$stem ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done"
                : ":"
            return wait + "\necho \"\(phase)-end $stem\" >> '\(control.log.path)'"
        }
        let extract = extractFails
            ? """
            stem=$(basename "$2" .mp4)
            \(start("extract", "$4 $2"))
            echo '{"event": "error", "message": "boom-extract"}'
            exit 1
            """
            : #"""
            stem=$(basename "$2" .mp4)
            \#(start("extract", "$4 $2"))
            echo '{"event": "stage", "stage": "extracting_audio", "fraction": 0.5}'
            \#(hold("extract"))
            echo '{"event": "stage", "stage": "extracting_audio", "fraction": 1.0}'
            echo "{\"event\": \"result\", \"language\": \"?\", \"duration\": 0, \"audio\": \"$4/$stem.wav\", \"outputs\": [], \"sentences\": []}"
            """#
        let transcribe = transcribeFails
            ? """
            stem=$(basename "$2" .mp4)
            \(start("transcribe", "$4 $2"))
            echo '{"event": "error", "message": "boom-transcribe"}'
            exit 1
            """
            : #"""
            stem=$(basename "$2" .mp4)
            \#(start("transcribe", "$4 $2"))
            \#(hold("transcribe"))
            echo "{\"event\": \"result\", \"language\": \"en\", \"duration\": 1, \"audio\": \"/nonexistent.wav\", \"outputs\": [], \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Metin $stem\", \"words\": []}]}"
            """#
        let translate = #"""
        \#(start("translate", ""))
        \#(hold("translate"))
        echo "{\"event\": \"translation\", \"language\": \"tr\", \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Çeviri $stem\", \"words\": []}]}"
        """#
        return try FakeEngine.make(
            extractAudio: extract, transcribe: transcribe, translate: translate,
            translateInput: #"input=$(cat); stem=$(printf '%s' "$input" | sed -n 's/.*Metin \([a-z0-9]*\).*/\1/p' | head -1)"#
        )
    }

    private func has(_ control: Control, _ prefix: String) -> Bool {
        logLines(control.log).contains { $0.hasPrefix(prefix) }
    }

    private func count(_ control: Control, _ prefix: String) -> Int {
        logLines(control.log).filter { $0.hasPrefix(prefix) }.count
    }

    private func position(_ control: Control, _ line: String) -> Int? {
        logLines(control.log).firstIndex { $0.hasPrefix(line) }
    }

    /// The most `<phase>-start` lines that were open at once (a `-start` without its `-end` yet).
    private func peakOverlap(_ control: Control, _ phase: String) -> Int {
        var open = 0
        var peak = 0
        for line in logLines(control.log) {
            if line.hasPrefix("\(phase)-start") { open += 1; peak = max(peak, open) }
            if line.hasPrefix("\(phase)-end") { open -= 1 }
        }
        return peak
    }

    /// The folder a phase logged for a video: `<phase>-start <stem> <out-dir> <video>`.
    private func outDirectory(_ control: Control, _ phase: String, video: URL) -> String? {
        logLines(control.log)
            .map { $0.split(separator: " ").map(String.init) }
            .first { $0.count == 4 && $0[0] == "\(phase)-start" && $0[3] == video.path }?[2]
    }

    private func isAlive(_ pid: Int32) -> Bool { kill(pid, 0) == 0 }

    @Test func extractionRecognitionAndTranslationOverlapAcrossVideos() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control))
        vm.translationTarget = "tr"
        let videos = (1...4).map { root.appending(path: "v\($0).mp4") }
        vm.addToPlaylist(videos, sourceLanguage: "en")
        vm.startPlaylist()
        let task = vm.playlistTask

        // Stage 1 starts with the first video only: the second one waits until the first one's audio is out.
        #expect(await eventually { has(control, "extract-start v1") })
        let extracting = SubtitleViewModel.label(for: .extractingAudio, fraction: 0.5, purpose: .transcription).text
        #expect(await eventually { vm.playlist[0].statusMessage.text == extracting })
        try await Task.sleep(for: .milliseconds(250))
        #expect(count(control, "extract-start") == 1 && !has(control, "transcribe-start"))

        // Video 1 goes to recognition, and only video 2's audio is extracted ahead (one video ahead, one at a time).
        control.release("extract-v1")
        #expect(await eventually { has(control, "transcribe-start v1") && has(control, "extract-start v2") })
        #expect(vm.playlist[0].state == .working)

        // The second video's audio is ready, the third one isn't touched while video 1 is still being recognized.
        control.release("extract-v2")
        #expect(await eventually { vm.playlist[1].statusMessage.text == "Ses hazır · Sırada" })
        #expect(vm.playlist[1].state == .pending && vm.playlist[1].fraction == 1 && vm.playlist[1].isAudioReady)
        try await Task.sleep(for: .milliseconds(250))
        #expect(!has(control, "extract-start v3") && vm.playlist[2].statusMessage.text == "Sırada")

        // Video 1 is recognized: it translates (a local model) while video 2 is recognized and video 3's audio is
        // extracted, all at the same time.
        control.release("transcribe-v1")
        #expect(await eventually {
            has(control, "translate-start v1") && has(control, "transcribe-start v2") && has(control, "extract-start v3")
        })
        #expect(!has(control, "translate-end v1"))
        #expect(vm.playlist.map(\.state) == [.working, .working, .working, .pending])
        #expect(vm.playlist[3].statusMessage.text == "Sırada" && !vm.playlist[3].isAudioReady)

        // Video 1's translation finishes while video 2 is still being recognized, and is saved next to the video.
        control.release("translate-v1")
        #expect(await eventually { vm.playlist[0].state == .completed })
        #expect(vm.playlist[0].statusMessage.text == "Kaydedildi: \(savedName("v1"))")
        #expect(try String(contentsOf: root.appending(path: savedName("v1")), encoding: .utf8).contains("Çeviri v1"))
        #expect(!has(control, "transcribe-end v2"))

        // Video 2 is recognized before video 3's audio is out: it translates, video 3 waits for its own audio, and
        // video 4's audio does not start yet.
        control.release("transcribe-v2")
        #expect(await eventually { has(control, "translate-start v2") })
        try await Task.sleep(for: .milliseconds(250))
        #expect(!has(control, "transcribe-start v3") && !has(control, "extract-start v4"))

        control.release("extract-v3")
        #expect(await eventually { has(control, "transcribe-start v3") && has(control, "extract-start v4") })
        #expect(!has(control, "transcribe-start v4"))

        // Let the rest through; the stage-3 work of the earlier videos is still being waited for at the end.
        control.release("translate-v2", "extract-v4", "transcribe-v3", "transcribe-v4", "translate-v3", "translate-v4")
        await task?.value

        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        #expect(vm.playlistSummary == "4 / 4 tamamlandı")
        for stem in ["v1", "v2", "v3", "v4"] {
            #expect(try String(contentsOf: root.appending(path: savedName(stem)), encoding: .utf8).contains("Çeviri \(stem)"))
        }
        // One of each kind at a time, even though all three kinds ran together.
        #expect(peakOverlap(control, "extract") == 1)
        #expect(peakOverlap(control, "transcribe") == 1)
        #expect(peakOverlap(control, "translate") == 1)
        let order = logLines(control.log).map { $0.split(separator: " ").prefix(2).joined(separator: " ") }
        #expect(order.filter { $0.hasPrefix("extract-start") } == (1...4).map { "extract-start v\($0)" })
        #expect(order.filter { $0.hasPrefix("transcribe-start") } == (1...4).map { "transcribe-start v\($0)" })
        #expect(order.filter { $0.hasPrefix("translate-start") } == (1...4).map { "translate-start v\($0)" })
        // The recognition of video 2 and the audio of video 3 began before video 1's translation was over.
        let firstTranslationEnd = try #require(position(control, "translate-end v1"))
        #expect(try #require(position(control, "transcribe-start v2")) < firstTranslationEnd)
        #expect(try #require(position(control, "extract-start v3")) < firstTranslationEnd)
        // Each video was recognized in the folder its audio was extracted to, and those folders are the video's own.
        for video in videos {
            let extracted = try #require(outDirectory(control, "extract", video: video))
            #expect(outDirectory(control, "transcribe", video: video) == extracted)
            #expect(extracted == SubtitleViewModel.playlistOutDirectory(for: video).path)
        }
        #expect(!vm.isPlaylistRunning && !vm.isWorking && vm.status == .finished)
    }

    @Test func aLocalModelTranslatesVideoOneWhileVideoTwoIsRecognized() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control, gated: ["translate"]))
        vm.externalLlmEnabled = false
        vm.translationTarget = "tr"
        vm.addToPlaylist([root.appending(path: "v1.mp4"), root.appending(path: "v2.mp4")], sourceLanguage: "en")
        vm.startPlaylist()
        let task = vm.playlistTask

        #expect(await eventually { has(control, "translate-start v1") && has(control, "transcribe-end v2") })
        #expect(!has(control, "translate-end v1"))
        #expect(vm.isPlaylistBackgroundTranslating && vm.isTranslationInProgress)

        control.release("translate-v1", "translate-v2")
        await task?.value
        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        #expect(vm.playlistSummary == "2 / 2 tamamlandı")
        #expect(!vm.isPlaylistBackgroundTranslating)
    }

    @Test func translationsFinishAfterRecognitionIsOverAndTheListStillCompletes() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control, gated: ["translate"]))
        vm.translationTarget = "tr"
        vm.addToPlaylist((1...3).map { root.appending(path: "v\($0).mp4") }, sourceLanguage: "en")
        vm.startPlaylist()
        let task = vm.playlistTask

        // Video 2 is recognized while video 1 still translates, then waits for it before its own hand-off (the backlog
        // is one item at most), so video 3 is not recognized yet.
        #expect(await eventually { has(control, "transcribe-end v2") && vm.playlist[1].statusMessage.text == "Çeviri hazırlanıyor…" })
        try await Task.sleep(for: .milliseconds(250))
        #expect(!has(control, "transcribe-start v3") && vm.playlistBackgroundPending == 1)
        #expect(count(control, "translate-start") == 1)

        // Video 1 is over: video 2 translates and video 3 is recognized, then waits for video 2 the same way.
        control.release("translate-v1")
        #expect(await eventually { has(control, "translate-start v2") && has(control, "transcribe-end v3") })
        #expect(await eventually { vm.playlist[2].statusMessage.text == "Çeviri hazırlanıyor…" })
        #expect(vm.isPlaylistRunning && vm.playlistBackgroundPending == 1 && count(control, "translate-start") == 2)

        // Video 3 is handed off last; the list is only done once its translation is.
        control.release("translate-v2")
        #expect(await eventually { has(control, "translate-start v3") })
        #expect(await eventually { if case .working = vm.status { true } else { false } })

        control.release("translate-v3")
        await task?.value

        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        #expect(vm.playlistSummary == "3 / 3 tamamlandı")
        #expect(vm.status == .finished)
        #expect(peakOverlap(control, "translate") == 1)
        let started = logLines(control.log).filter { $0.hasPrefix("translate-start") }.map { String($0.split(separator: " ")[1]) }
        #expect(started == ["v1", "v2", "v3"])
    }

    @Test func eachItemHasItsOwnFolderEvenWithTheSameFileName() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let dirA = root.appending(path: "a")
        let dirB = root.appending(path: "b")
        for dir in [dirA, dirB] { try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true) }
        let (vm, _) = makeModel(engine: try gatedEngine(control, gated: []))
        vm.translationTarget = "tr"
        let first = dirA.appending(path: "clip.mp4")
        let second = dirB.appending(path: "clip.mp4")
        vm.addToPlaylist([first, second], sourceLanguage: "en")
        await run(vm)
        #expect(vm.playlist.allSatisfy { $0.state == .completed })

        let firstFolder = try #require(outDirectory(control, "transcribe", video: first))
        let secondFolder = try #require(outDirectory(control, "transcribe", video: second))
        #expect(firstFolder != secondFolder)
        // The audio extracted ahead of time went to the folder the recognition reads from.
        #expect(outDirectory(control, "extract", video: first) == firstFolder)
        #expect(outDirectory(control, "extract", video: second) == secondFolder)
        let playlistFolder = WorkingDirectory.url().appending(path: "playlist").standardizedFileURL.path
        for folder in [firstFolder, secondFolder] {
            #expect(URL(filePath: folder).deletingLastPathComponent().standardizedFileURL.path == playlistFolder)
        }
        // The folder only depends on the video's path.
        #expect(SubtitleViewModel.playlistOutDirectory(for: first) == SubtitleViewModel.playlistOutDirectory(for: dirA.appending(path: "../a/clip.mp4")))
        #expect(SubtitleViewModel.playlistOutDirectory(for: first) != SubtitleViewModel.playlistOutDirectory(for: second))
        let custom = URL(filePath: "/base")
        #expect(SubtitleViewModel.playlistOutDirectory(for: first, base: custom).deletingLastPathComponent().path == "/base/playlist")
    }

    @Test func theEditorsOwnRecognitionKeepsUsingTheWorkingFolder() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control, gated: []))
        vm.openVideo(root.appending(path: "solo.mp4"))
        vm.generate()
        await vm.job?.value
        let folder = try #require(outDirectory(control, "transcribe", video: root.appending(path: "solo.mp4")))
        #expect(URL(filePath: folder).standardizedFileURL.path == WorkingDirectory.url().standardizedFileURL.path)
    }

    @Test func aSubtitleFileNextToTheVideoNeedsNoAudioExtraction() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        try Data("1\n00:00:00,000 --> 00:00:01,000\nMetin sidecar\n\n".utf8).write(to: root.appending(path: "sidecar.srt"))
        let (vm, _) = makeModel(engine: try gatedEngine(control, gated: []))
        vm.translationTarget = "tr"
        vm.addToPlaylist([root.appending(path: "sidecar.mp4"), root.appending(path: "plain.mp4")], sourceLanguage: "en")
        await run(vm)

        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        #expect(!has(control, "extract-start sidecar") && !has(control, "transcribe-start sidecar"))
        #expect(has(control, "extract-start plain") && has(control, "transcribe-start plain"))
        // The sidecar's lines were translated and saved over it.
        #expect(try String(contentsOf: root.appending(path: savedName("sidecar")), encoding: .utf8).contains("Çeviri sidecar"))
    }

    @Test func aFailedAudioExtractionLeavesTheErrorToRecognition() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control, gated: [], extractFails: true, transcribeFails: true))
        vm.translationTarget = "tr"
        vm.addToPlaylist([root.appending(path: "v1.mp4"), root.appending(path: "v2.mp4")], sourceLanguage: "en")
        await run(vm)

        // The extraction's own error is never shown: each row ends with recognition's.
        #expect(vm.playlist.map(\.state) == [.failed, .failed])
        #expect(vm.playlist.allSatisfy { $0.statusMessage.text == "boom-transcribe" })
        #expect(has(control, "extract-start v1") && has(control, "extract-start v2"))
        #expect(vm.canStartPlaylist)
    }

    @Test func aFailedAudioExtractionStillLetsRecognitionRun() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control, gated: [], extractFails: true))
        vm.translationTarget = "tr"
        vm.addToPlaylist([root.appending(path: "v1.mp4"), root.appending(path: "v2.mp4")], sourceLanguage: "en")
        await run(vm)

        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        #expect(vm.playlist.allSatisfy { !$0.isAudioReady })
    }

    @Test func cancelStopsExtractionRecognitionAndTranslationTogether() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control))
        vm.translationTarget = "tr"
        vm.addToPlaylist((1...4).map { root.appending(path: "v\($0).mp4") }, sourceLanguage: "en")
        vm.startPlaylist()
        let task = vm.playlistTask

        // Video 1 translates, video 2 is recognized and video 3's audio is extracted, all stuck.
        #expect(await eventually { has(control, "extract-start v1") })
        control.release("extract-v1")
        #expect(await eventually { has(control, "transcribe-start v1") && has(control, "extract-start v2") })
        control.release("extract-v2")
        #expect(await eventually { vm.playlist[1].isAudioReady })
        control.release("transcribe-v1")
        #expect(await eventually {
            has(control, "translate-start v1") && has(control, "transcribe-start v2") && has(control, "extract-start v3")
        })
        let pids = try ["translate-v1", "transcribe-v2", "extract-v3"].map { name -> Int32 in
            try #require(control.pid(name))
        }
        #expect(pids.allSatisfy(isAlive))

        vm.cancel()
        await task?.value

        // Nothing is left running, and every row that did not finish is waiting again, audio-ready or not.
        #expect(await eventually { pids.allSatisfy { !isAlive($0) } })
        #expect(vm.playlist.allSatisfy { $0.state == .pending && $0.statusMessage.text == "Sırada" && $0.fraction == nil })
        #expect(!vm.isPlaylistRunning && !vm.isWorking && vm.status == .idle)
        #expect(vm.playlistTask == nil && vm.playlistPipeline == nil)
        #expect(vm.playlistBackground.isEmpty && vm.playlistBackgroundPending == 0 && !vm.isPlaylistBackgroundTranslating)
        #expect(!FileManager.default.fileExists(atPath: root.appending(path: savedName("v1")).path))
        #expect(vm.canStartPlaylist)
        // Video 4 was never touched.
        #expect(!has(control, "extract-start v4") && !has(control, "transcribe-start v3"))
    }

    // MARK: - Memory: a local translation model next to the recognition model

    @Test func theOverlapThresholdIsA24GiBMacAndExternalProvidersAlwaysOverlap() throws {
        let (vm, _) = makeModel()
        #expect(SubtitleViewModel.localTranslationOverlapMemory == 24 * 1024 * 1024 * 1024)
        vm.physicalMemory = SubtitleViewModel.localTranslationOverlapMemory - 1
        #expect(!vm.translationMayOverlapRecognition(external: false))
        #expect(vm.translationMayOverlapRecognition(external: true))
        vm.physicalMemory = SubtitleViewModel.localTranslationOverlapMemory
        #expect(vm.translationMayOverlapRecognition(external: false))
        vm.physicalMemory = 8 * 1024 * 1024 * 1024
        #expect(!vm.translationMayOverlapRecognition(external: false) && vm.translationMayOverlapRecognition(external: true))
    }

    @Test func aLocalModelOnASmallMacTranslatesVideoOneBeforeVideoTwoIsRecognized() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control, gated: ["translate"]))
        vm.physicalMemory = 16 * 1024 * 1024 * 1024
        vm.translationTarget = "tr"
        vm.addToPlaylist([root.appending(path: "v1.mp4"), root.appending(path: "v2.mp4")], sourceLanguage: "en")
        vm.startPlaylist()
        let task = vm.playlistTask

        #expect(await eventually { has(control, "translate-start v1") })
        try await Task.sleep(for: .milliseconds(300))
        // Video 2's recognition waits for video 1's translation, though its audio is ready.
        #expect(!has(control, "transcribe-start v2") && !has(control, "translate-end v1"))
        #expect(vm.playlist[0].state == .working && vm.isPlaylistBackgroundTranslating)

        control.release("translate-v1", "translate-v2")
        await task?.value
        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        let translationEnd = try #require(position(control, "translate-end v1"))
        #expect(try #require(position(control, "transcribe-start v2")) > translationEnd)
        #expect(peakOverlap(control, "translate") == 1 && peakOverlap(control, "transcribe") == 1)
        #expect(!vm.isPlaylistBackgroundTranslating && vm.status == .finished)
    }

    @Test func aStoppedRunDoesNotHangOnTheTranslationItWaitsFor() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control, gated: ["translate"]))
        vm.physicalMemory = 8 * 1024 * 1024 * 1024
        vm.translationTarget = "tr"
        vm.addToPlaylist([root.appending(path: "v1.mp4"), root.appending(path: "v2.mp4")], sourceLanguage: "en")
        vm.startPlaylist()
        let task = vm.playlistTask
        #expect(await eventually { has(control, "translate-start v1") })
        vm.cancel()
        await task?.value
        #expect(!vm.isPlaylistRunning && !has(control, "transcribe-start v2"))
        #expect(vm.playlist.allSatisfy { $0.state == .pending })
    }

    @Test func anExternalProviderOverlapsEvenOnASmallMac() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control, gated: ["translate"]))
        vm.physicalMemory = 8 * 1024 * 1024 * 1024
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.translationTarget = "tr"
        vm.addToPlaylist([root.appending(path: "v1.mp4"), root.appending(path: "v2.mp4")], sourceLanguage: "en")
        vm.startPlaylist()
        let task = vm.playlistTask

        #expect(await eventually { has(control, "translate-start v1") && has(control, "transcribe-end v2") })
        #expect(!has(control, "translate-end v1"))
        control.release("translate-v1", "translate-v2")
        await task?.value
        #expect(vm.playlist.allSatisfy { $0.state == .completed })
    }

    // MARK: - Settings are looked at again when stage 3 really starts

    /// A view model with one cloud provider (key "sk-old") whose keys live in `secrets`, and a gate nobody else holds.
    private func makeCloudModel(
        engine: EngineClient, secrets: InMemorySecretStore
    ) -> (SubtitleViewModel, TranslationProviderProfile) {
        let store = TranslationProviderStore(
            fileURL: FileManager.default.temporaryDirectory.appending(path: "providers-\(UUID().uuidString)/providers.json"),
            secrets: secrets
        )
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine), defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: EngineLaunchState(), providerStore: store
        )
        vm.offlineMode = false
        vm.physicalMemory = SubtitleViewModel.localTranslationOverlapMemory
        let provider = vm.addProvider(TranslationProviderProfile(preset: .preset(id: "openai")), key: "sk-old").profile
        return (vm, provider)
    }

    @Test func offlineModeSwitchedOnAfterTheHandOffStopsTheCloudTranslation() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeCloudModel(engine: try gatedEngine(control, gated: []), secrets: InMemorySecretStore())
        vm.translationTarget = "tr"
        vm.addToPlaylist([root.appending(path: "v1.mp4")], sourceLanguage: "en")
        // Something else (the editor's job) holds the gate, so the hand-off waits for it, as it can for minutes.
        #expect(vm.translationGate.tryAcquire())
        vm.startPlaylist()
        let task = vm.playlistTask
        #expect(await eventually { vm.playlistBackgroundPending == 1 })

        vm.offlineMode = true
        vm.translationGate.release()
        await task?.value

        #expect(vm.playlist[0].state == .failed)
        #expect(vm.playlist[0].statusMessage.text.contains("OpenAI") && vm.playlist[0].statusMessage.text.contains("Çevrimdışı"))
        #expect(!has(control, "translate-start"), "the text never went to the provider")
    }

    @Test func offlineModeSwitchedOnAfterTheHandOffIsPassedToALocalTranslation() async throws {
        let log = try makeFolder().appending(path: "offline.log")
        let engine = try FakeEngine.make(
            transcribe: finished("Merhaba"),
            translate: """
            echo "offline $SUBTITLE_ENGINE_OFFLINE" >> '\(log.path)'
            \(translated("Hello"))
            """
        )
        let (vm, _) = makeModelWithLaunchState(engine: engine)
        vm.translationTarget = "en"
        let video = log.deletingLastPathComponent().appending(path: "a.mp4")
        vm.addToPlaylist([video], sourceLanguage: "tr")
        #expect(vm.translationGate.tryAcquire())
        vm.startPlaylist()
        let task = vm.playlistTask
        #expect(await eventually { vm.playlistBackgroundPending == 1 })
        vm.offlineMode = true
        vm.translationGate.release()
        await task?.value
        // The hand-off pinned "offline 0"; the translation that really started says "offline 1".
        #expect(logLines(log) == ["offline 1"])
    }

    @Test func theKeyIsReadAgainWhenTheHandedOffTranslationStarts() async throws {
        let log = try makeFolder().appending(path: "keys.log")
        let engine = try FakeEngine.make(
            transcribe: finished("Merhaba"),
            translate: """
            echo "key ${SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY:-none}" >> '\(log.path)'
            \(translated("Hello"))
            """
        )
        let secrets = InMemorySecretStore()
        let (vm, provider) = makeCloudModel(engine: engine, secrets: secrets)
        vm.translationTarget = "en"
        vm.addToPlaylist([log.deletingLastPathComponent().appending(path: "a.mp4")], sourceLanguage: "tr")
        #expect(vm.translationGate.tryAcquire())
        vm.startPlaylist()
        let task = vm.playlistTask
        #expect(await eventually { vm.playlistBackgroundPending == 1 })
        // The key was changed while the translation waited; the server is still the one chosen at hand-off.
        secrets.set("sk-new", for: TranslationProviderStore.secretAccount(for: provider.id))
        vm.translationGate.release()
        await task?.value
        #expect(logLines(log) == ["key sk-new"])
        #expect(vm.playlist[0].state == .completed)
    }

    @Test func aKeyThatCannotBeReadWhenTheTranslationStartsFailsTheItemWithoutSendingAnything() async throws {
        struct Locked: SecretStore {
            func data(for account: String) -> Data? { nil }
            func read(_ account: String) -> SecretRead { .unavailable }
            func set(_ data: Data, for account: String) -> Bool { true }
            func remove(_ account: String) {}
        }
        let log = try makeFolder().appending(path: "calls.log")
        let engine = try FakeEngine.make(
            transcribe: finished("Merhaba"),
            translate: """
            echo "translate" >> '\(log.path)'
            \(translated("Hello"))
            """
        )
        let store = TranslationProviderStore(
            fileURL: log.deletingLastPathComponent().appending(path: "providers.json"), secrets: Locked()
        )
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine), defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: EngineLaunchState(), providerStore: store
        )
        vm.offlineMode = false
        vm.addProvider(TranslationProviderProfile(preset: .preset(id: "openai")), key: "")
        vm.translationTarget = "en"
        vm.addToPlaylist([log.deletingLastPathComponent().appending(path: "a.mp4")], sourceLanguage: "tr")
        await run(vm)
        #expect(vm.playlist[0].state == .failed)
        #expect(vm.playlist[0].statusMessage.text.contains("Anahtar Zinciri"))
        #expect(logLines(log).isEmpty)
    }

    @Test func cancelWhileOnlyTheFirstAudioIsBeingExtractedStopsIt() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let (vm, _) = makeModel(engine: try gatedEngine(control))
        vm.translationTarget = "tr"
        vm.addToPlaylist([root.appending(path: "v1.mp4"), root.appending(path: "v2.mp4")], sourceLanguage: "en")
        vm.startPlaylist()
        let task = vm.playlistTask
        #expect(await eventually { has(control, "extract-start v1") && control.pid("extract-v1") != nil })
        let pid = try #require(control.pid("extract-v1"))

        vm.cancel()
        await task?.value

        #expect(await eventually { !isAlive(pid) })
        #expect(vm.playlist.allSatisfy { $0.state == .pending && $0.statusMessage.text == "Sırada" && $0.fraction == nil })
        #expect(!has(control, "transcribe-start") && !has(control, "extract-start v2"))
        #expect(!vm.isPlaylistRunning && vm.playlistPipeline == nil && vm.canStartPlaylist)
    }
}
