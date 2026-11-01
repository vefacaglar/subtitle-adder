import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// The batch playlist: items, the sequential run, cancel, retry, offline block, drops and the async mode with an
/// external LLM.
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
        return (vm, defaults)
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

    private func savedName(_ stem: String, _ language: String, _ format: SubtitleFormat = .srt) -> String {
        SubtitleViewModel.subtitleFileName(stem: stem, language: language, format: format)
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
        // Sequential: each video is transcribed, then translated, before the next one starts.
        let calls = logLines(log).map { $0.split(separator: " ").first.map(String.init) ?? "" }
        #expect(calls == ["transcribe", "translate", "transcribe", "translate"])
        let lines = logLines(log)
        #expect(lines[0].contains(videoA.path) && lines[0].contains("--language pt"))
        #expect(lines[1].contains("--to tr") && lines[1].contains("--from pt"))
        #expect(lines[2].contains(videoB.path) && lines[2].contains("--language ja"))
        #expect(lines[3].contains("--to tr") && lines[3].contains("--from ja"))

        let savedA = try String(contentsOf: dirA.appending(path: savedName("ClipA", "tr")), encoding: .utf8)
        let savedB = try String(contentsOf: dirB.appending(path: savedName("ClipB", "tr")), encoding: .utf8)
        #expect(savedA.contains("Hassiktir!") && !savedA.contains("Caralho"))
        #expect(savedB.contains("Merhaba"))
        #expect(vm.playlist[0].statusMessage.text == "Kaydedildi: \(savedName("ClipA", "tr"))")

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
        #expect(names == ["calls.log", "clip.srt", savedName("clip", "tr")].sorted())
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
        let engine = try FakeEngine.make(transcribe: "touch \"\(started.path)\"\nsleep 20")
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
            let text = try String(contentsOf: folder.appending(path: savedName(stem, "en")), encoding: .utf8)
            #expect(text.contains("Hello"))
        }
        #expect(vm.status == .finished)
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
            transcribe: finished("Merhaba"), translate: "touch \"\(started.path)\"\nsleep 20"
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
        #expect(!FileManager.default.fileExists(atPath: folder.appending(path: savedName("a", "en")).path))
    }
}
