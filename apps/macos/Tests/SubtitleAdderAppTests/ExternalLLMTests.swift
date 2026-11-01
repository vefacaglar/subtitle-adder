import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// External LLM settings, the connection test and the background ("async") translation in the view model.
@MainActor
@Suite struct ExternalLLMTests {
    private func makeModel(
        engine: EngineClient? = nil,
        defaults: UserDefaults? = nil,
        fetch: ExternalLLMServer.Fetch? = nil
    ) -> (SubtitleViewModel, EngineLaunchState, UserDefaults) {
        let defaults = defaults ?? UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        let state = EngineLaunchState()
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine), defaults: defaults, launchState: state, externalFetch: fetch
        )
        vm.offlineMode = false
        return (vm, state, defaults)
    }

    private func makeFolder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "external-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    /// Waits for the running job and the one it hands over to.
    private func settle(_ vm: SubtitleViewModel) async {
        while let job = vm.job {
            await job.value
            if vm.job == job { break }
        }
    }

    private func line(_ id: Int, _ text: String) -> Sentence {
        Sentence(id: id, start: Double(id), end: Double(id) + 1, text: text)
    }

    private nonisolated static func tagsBody(_ names: [String]) -> Data {
        let items = names.map { #"{"name": "\#($0)"}"# }.joined(separator: ",")
        return Data(#"{"models": [\#(items)]}"#.utf8)
    }

    // MARK: - Settings

    @Test func settingsPersistAndReachTheEngine() {
        let (vm, state, defaults) = makeModel()
        #expect(!vm.externalLlmEnabled && vm.externalLlmUrl.isEmpty && vm.externalLlmModel.isEmpty && !vm.externalLlmAsync)
        #expect(vm.externalLLM == nil && !vm.isExternalLLMActive)
        vm.externalLlmUrl = " 192.168.1.50:11434 "
        // The address alone doesn't switch it on.
        #expect(state.settings.externalLLM == nil)
        vm.externalLlmEnabled = true
        vm.externalLlmModel = "gemma4:e4b"
        vm.externalLlmAsync = true
        #expect(vm.isExternalLLMActive && vm.isExternalAsyncActive)
        #expect(state.settings.externalLLM == ExternalLLM(url: " 192.168.1.50:11434 ", model: "gemma4:e4b"))
        #expect(defaults.bool(forKey: "externalLlmEnabled") && defaults.bool(forKey: "externalLlmAsync"))
        #expect(defaults.string(forKey: "externalLlmUrl") == " 192.168.1.50:11434 ")
        #expect(defaults.string(forKey: "externalLlmModel") == "gemma4:e4b")

        let (again, againState, _) = makeModel(defaults: defaults)
        #expect(again.externalLlmEnabled && again.externalLlmAsync)
        #expect(again.externalLlmUrl == " 192.168.1.50:11434 " && again.externalLlmModel == "gemma4:e4b")
        #expect(againState.settings.externalLLM?.normalizedURL == "http://192.168.1.50:11434")

        vm.externalLlmEnabled = false
        #expect(state.settings.externalLLM == nil && !vm.isExternalAsyncActive)
    }

    @Test func aBlankOrUnusableAddressIsNotActive() {
        let (vm, state, _) = makeModel()
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "   "
        #expect(!vm.isExternalLLMActive && state.settings.externalLLM == nil)
        vm.externalLlmUrl = "http://"
        #expect(!vm.isExternalLLMActive)
    }

    @Test func toolbarTitleNamesTheExternalModel() {
        let (vm, _, _) = makeModel()
        vm.translationModel = "madlad-3b"
        #expect(vm.translationModelTitle == ModelLabel.name(for: "madlad-3b"))
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        #expect(vm.translationModelTitle == "Harici: Harici LLM")
        vm.externalLlmModel = " gemma4:e4b "
        #expect(vm.translationModelTitle == "Harici: gemma4:e4b")
        vm.externalLlmAsync = true
        #expect(vm.translationModelTitle == "Harici: gemma4:e4b (Asenkron)")
    }

    // MARK: - Connection test

    @Test func connectionTestListsModelsAndFillsAnEmptyModelField() async {
        let (vm, _, _) = makeModel(fetch: { _, _ in Self.tagsBody(["gemma4:e4b", "nomic-embed-text", "qwen3:8b"]) })
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:11434/v1"
        await vm.testExternalConnection()
        #expect(vm.externalModels == ["gemma4:e4b", "qwen3:8b"])
        #expect(vm.externalLlmModel == "gemma4:e4b")
        #expect(!vm.isTestingExternalConnection)
        let notice = vm.externalConnection
        #expect(notice?.isError == false)
        #expect(notice?.message.text.hasPrefix("Bağlantı başarılı (10.0.0.2 · ") == true)
        #expect(notice?.message.text.hasSuffix("ms · 2 uygun model bulundu)") == true)
    }

    @Test func connectionTestKeepsAModelTheUserTyped() async {
        let (vm, _, _) = makeModel(fetch: { _, _ in Self.tagsBody(["a", "b"]) })
        vm.externalLlmUrl = "10.0.0.2:11434"
        vm.externalLlmModel = "custom"
        await vm.testExternalConnection()
        #expect(vm.externalLlmModel == "custom" && vm.externalModels == ["a", "b"])
    }

    @Test func connectionTestReportsAnUnreachableServerAndABadAddress() async {
        struct Down: Error, LocalizedError { var errorDescription: String? { "timed out" } }
        let (vm, _, _) = makeModel(fetch: { _, _ in throw Down() })
        vm.externalLlmUrl = "10.0.0.9:11434"
        vm.externalModels = ["stale"]
        await vm.testExternalConnection()
        #expect(vm.externalConnection?.isError == true)
        #expect(vm.externalConnection?.message.text == "Sunucuya ulaşılamadı: timed out")
        #expect(vm.externalModels.isEmpty)

        vm.externalLlmUrl = "   "
        await vm.testExternalConnection()
        #expect(vm.externalConnection?.isError == true)
        #expect(vm.externalConnection?.message.text == "Lütfen geçerli bir sunucu adresi girin.")
    }

    @Test func editingTheAddressClearsTheOldResult() async {
        let (vm, _, _) = makeModel(fetch: { _, _ in Self.tagsBody(["a"]) })
        vm.externalLlmUrl = "10.0.0.2:11434"
        await vm.testExternalConnection()
        #expect(vm.externalConnection != nil && !vm.externalModels.isEmpty)
        vm.externalLlmUrl = "10.0.0.3:11434"
        #expect(vm.externalConnection == nil && vm.externalModels.isEmpty)
    }

    // MARK: - Translating through the external server

    @Test func nllbWithoutASourceLanguageIsFineThroughAnExternalServer() async throws {
        let (vm, _, _) = makeModel(engine: try FakeEngine.make())
        vm.translationModel = "nllb-1.3b"
        vm.sentences = [line(1, "Merhaba.")]
        vm.translate()
        guard case .failed = vm.status else { Issue.record("expected the NLLB failure"); return }
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:11434"
        vm.translate()
        #expect(vm.isWorking)
        vm.cancel()
    }

    @Test func reuseOnlyTranslatesMissingOutdatedAndWrongLanguageLines() {
        let (vm, _, _) = makeModel()
        vm.sentences = [line(1, "Bir."), line(2, "İki."), line(3, "Üç."), line(4, "Dört.")]
        let uids = vm.sentences.map(\.uid)
        vm.translations = [
            uids[0]: TranslationEntry(text: "One.", sourceText: "Bir.", language: "en"),
            uids[1]: TranslationEntry(text: "Two.", sourceText: "Eski.", language: "en"),
            uids[2]: TranslationEntry(text: "Drei.", sourceText: "Üç.", language: "de"),
        ]
        #expect(vm.linesNeedingTranslation(into: "en").map(\.text) == ["İki.", "Üç.", "Dört."])
    }

    @Test func reuseWithEverythingTranslatedOnlySaves() async throws {
        let folder = try makeFolder()
        let marker = folder.appending(path: "called")
        let engine = try FakeEngine.make(translate: "touch '\(marker.path)'")
        let (vm, _, _) = makeModel(engine: engine)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.openVideo(folder.appending(path: "Film.mp4"))
        vm.sentences = [line(1, "Merhaba."), line(2, "Dünya.")]
        vm.applyTranslation(
            [line(1, "Hello."), line(2, "World.")], language: "en", source: vm.sentences
        )
        vm.translate(reuseExisting: true)
        await settle(vm)
        #expect(vm.status == .finished)
        #expect(!FileManager.default.fileExists(atPath: marker.path))
        let saved = try FileManager.default.contentsOfDirectory(atPath: folder.path).filter { $0.hasSuffix(".srt") }
        #expect(saved.count == 1)
    }

    // MARK: - Background translation

    private func sentenceJSON(_ id: Int, _ text: String) -> String {
        #"{"id": \#(id), "start": \#(id), "end": \#(id + 1), "text": "\#(text)", "words": []}"#
    }

    /// A fake engine whose `translate` records every request in `log` and answers with the lines prefixed "T-".
    private func recordingEngine(log: URL, transcribe: String) throws -> EngineClient {
        let dir = log.path
        return try FakeEngine.make(
            transcribe: transcribe,
            translate: #"printf '{"event": "translation", "language": "en", "sentences": %s}\n' "$(sed 's/"text":"\([^"]*\)"/"text":"T-\1"/g' "\#(dir)/req-$n")""#,
            translateInput: "n=$(ls '\(dir)' | wc -l | tr -d ' '); cat > '\(dir)/req-'$n"
        )
    }

    private func requests(in log: URL) throws -> [[String]] {
        try FileManager.default.contentsOfDirectory(atPath: log.path).sorted().map { name in
            let data = try Data(contentsOf: log.appending(path: name))
            return try JSONDecoder().decode([Sentence].self, from: data).map(\.text)
        }
    }

    @Test func asyncModeTranslatesFinishedLinesWhileTranscribingThenOnlyTheRest() async throws {
        let folder = try makeFolder()
        let log = try makeFolder()
        let transcribe = """
        echo '{"event": "partial", "language": "tr", "sentences": [\(sentenceJSON(1, "Bir.")), \(sentenceJSON(2, "İki."))]}'
        i=0; while [ ! -e '\(log.path)/req-0' ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done
        echo '{"event": "result", "language": "tr", "duration": 4, "audio": "/nonexistent.wav", "outputs": [], "sentences": [\(sentenceJSON(1, "Bir.")), \(sentenceJSON(2, "İki.")), \(sentenceJSON(3, "Üç."))]}'
        """
        let (vm, _, _) = makeModel(engine: try recordingEngine(log: log, transcribe: transcribe))
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:11434"
        vm.externalLlmAsync = true
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.diarize = false
        vm.openVideo(folder.appending(path: "Film.mp4"))
        vm.generateAndTranslate()
        await settle(vm)

        // The first line went out during transcription (the last line of a snapshot is still changing); the final
        // pass sent the other two.
        #expect(try requests(in: log) == [["Bir."], ["İki.", "Üç."]])
        #expect(vm.sentences.map(\.text) == ["Bir.", "İki.", "Üç."])
        #expect(vm.sentences.compactMap { vm.translation(for: $0)?.text } == ["T-Bir.", "T-İki.", "T-Üç."])
        #expect(vm.translationLanguage == "en")
        #expect(vm.status == .finished)
        #expect(vm.asyncTranslationFraction == nil && vm.asyncTask == nil)
        // Auto-save ran after the final pass.
        let saved = try FileManager.default.contentsOfDirectory(atPath: folder.path).filter { $0.hasSuffix(".srt") }
        #expect(saved.count == 1)
    }

    @Test func withoutAsyncModeNothingIsTranslatedBeforeTheResult() async throws {
        let folder = try makeFolder()
        let log = try makeFolder()
        let transcribe = """
        echo '{"event": "partial", "language": "tr", "sentences": [\(sentenceJSON(1, "Bir.")), \(sentenceJSON(2, "İki."))]}'
        sleep 0.3
        echo '{"event": "result", "language": "tr", "duration": 3, "audio": "/nonexistent.wav", "outputs": [], "sentences": [\(sentenceJSON(1, "Bir.")), \(sentenceJSON(2, "İki."))]}'
        """
        let (vm, _, _) = makeModel(engine: try recordingEngine(log: log, transcribe: transcribe))
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:11434"
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.diarize = false
        vm.openVideo(folder.appending(path: "Film.mp4"))
        vm.generateAndTranslate()
        await settle(vm)
        #expect(try requests(in: log) == [["Bir.", "İki."]])
        #expect(vm.translations.count == 2)
    }

    @Test func backgroundTranslationNeverLandsOnALineEditedSinceTheBatchWasSent() {
        let (vm, _, _) = makeModel()
        vm.sentences = [line(1, "Bir."), line(2, "İki."), line(3, "Üç.")]
        let batch = Array(vm.sentences.prefix(2))
        vm.asyncQueued = Set(batch.map(\.uid))
        // The second line changed while the batch was at the server.
        vm.sentences[1].text = "İki, düzeltildi."
        vm.applyBackgroundTranslation(
            [line(1, "One."), line(2, "Two.")], language: "en", batch: batch
        )
        #expect(vm.translation(for: vm.sentences[0])?.text == "One.")
        #expect(vm.translation(for: vm.sentences[1]) == nil)
        // The edited line is released so it gets translated again.
        #expect(vm.asyncQueued == [batch[0].uid])
        #expect(vm.linesNeedingTranslation(into: "en").map(\.text) == ["İki, düzeltildi.", "Üç."])
    }

    @Test func backgroundTranslationKeepsEarlierTranslations() {
        let (vm, _, _) = makeModel()
        vm.sentences = [line(1, "Bir."), line(2, "İki.")]
        vm.applyBackgroundTranslation([line(1, "One.")], language: "en", batch: [vm.sentences[0]])
        vm.applyBackgroundTranslation([line(2, "Two.")], language: "en", batch: [vm.sentences[1]])
        #expect(vm.sentences.compactMap { vm.translation(for: $0)?.text } == ["One.", "Two."])
        #expect(vm.displayMode == .translation && vm.exportLanguage == .translated)
    }

    @Test func stoppingCancelsTheBackgroundTranslation() async throws {
        let transcribe = """
        echo '{"event": "partial", "language": "tr", "sentences": [\(sentenceJSON(1, "Bir.")), \(sentenceJSON(2, "İki."))]}'
        exec sleep 5
        """
        let engine = try FakeEngine.make(transcribe: transcribe, translate: "exec sleep 5")
        let (vm, _, _) = makeModel(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:11434"
        vm.externalLlmAsync = true
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.diarize = false
        vm.openVideo(URL(filePath: "/tmp/never-opened.mp4"))
        vm.generateAndTranslate()
        for _ in 0..<100 where vm.asyncTranslationFraction == nil { try await Task.sleep(for: .milliseconds(50)) }
        #expect(vm.asyncTask != nil)
        #expect(vm.asyncTranslationFraction != nil)
        vm.cancel()
        #expect(vm.asyncTask == nil && vm.asyncTranslationFraction == nil && vm.asyncQueued.isEmpty)
        #expect(!vm.isWorking)
        // Whatever the cancelled batch would have delivered is not applied.
        try await Task.sleep(for: .milliseconds(100))
        #expect(vm.translations.isEmpty)
    }
}
