import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// Languages, translation models, offline mode, display mode and live output in the view model.
@MainActor
@Suite struct AppFoundationTests {
    private func fakeEngine(models: String = "", transcribe: String = "", translate: String = "") throws -> EngineClient {
        try FakeEngine.make(models: models, transcribe: transcribe, translate: translate)
    }

    private static let modelsEvent = """
    echo '{"event": "models", "models": [\
    {"id": "base", "size_mb": 145, "speed": 5, "accuracy": 2, "downloaded": false},\
    {"id": "large-v3", "size_mb": 3000, "speed": 1, "accuracy": 5, "downloaded": false},\
    {"id": "speaker-diarization", "kind": "diarization", "backend": "sherpa", "size_mb": 29, "speed": 4, "accuracy": 3, "downloaded": true},\
    {"id": "madlad-3b", "kind": "translation", "backend": "ctranslate2", "size_mb": 2839, "speed": 2, "accuracy": 5, "downloaded": false},\
    {"id": "qwen3.5-4b", "kind": "translation", "backend": "llama", "size_mb": 2614, "speed": 3, "accuracy": 4, "downloaded": true}\
    ]}'
    """

    private func makeModel(
        engine: EngineClient? = nil,
        defaults: UserDefaults? = nil
    ) -> (SubtitleViewModel, EngineLaunchState, UserDefaults) {
        let defaults = defaults ?? UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        let state = EngineLaunchState()
        let vm = SubtitleViewModel(catalog: ModelCatalog(engine: engine), defaults: defaults, launchState: state)
        return (vm, state, defaults)
    }

    /// Evaluated outside `#expect`, whose closure can't capture the main-actor view model.
    private func isBlocked(_ vm: SubtitleViewModel, _ ids: [String]) -> Bool {
        vm.blockedOffline(ids, retry: {})
    }

    private func line(_ id: Int, _ start: Double, _ end: Double, _ text: String) -> Sentence {
        Sentence(id: id, start: start, end: end, text: text)
    }

    // MARK: - Languages

    @Test func languageMenuOrderAndTargets() {
        #expect(SubtitleLanguage.allCases.map(\.rawValue) == ["", "tr", "pt", "es", "ru", "en", "de", "fr", "ja", "zh"])
        #expect(SubtitleLanguage.translationTargets.map(\.rawValue) == ["tr", "pt", "es", "ru", "en", "de", "fr", "ja", "zh"])
        #expect(SubtitleLanguage.german.title == "Almanca (de)")
        #expect(SubtitleLanguage.chinese.targetName == "Çince")
    }

    @Test func modelsAreFilteredByTheNewLanguages() {
        let catalog = ModelCatalog(engine: nil)
        func ids(_ language: String) -> Set<String> { Set(catalog.models(for: language).map(\.id)) }
        #expect(ids("de").isSuperset(of: ["parakeet-v3", "canary-1b-v2", "large-v3"]))
        #expect(ids("fr").contains("parakeet-v3"))
        #expect(!ids("de").contains("gigaam-v3"))
        for language in ["ja", "zh"] {
            #expect(ids(language).contains("large-v3"))
            #expect(!ids(language).contains("parakeet-v3"))
            #expect(!ids(language).contains("canary-1b-v2"))
        }
    }

    @Test func targetIsMovedOffTheSourceLanguage() {
        let (vm, _, _) = makeModel()
        vm.translationTarget = "ja"
        vm.language = "ja"
        #expect(vm.translationTarget == "tr")
        vm.translationTarget = "zh"
        vm.language = "zh"
        #expect(vm.translationTarget == "tr")
        vm.detectedLanguage = nil
        #expect(SubtitleLanguage(rawValue: vm.translationTarget) != nil)
    }

    // MARK: - Translation models

    @Test func fallbackCatalogListsLlamaModelsBeforeTheClassicOnes() {
        let catalog = ModelCatalog(engine: nil)
        #expect(catalog.llamaTranslationModels.map(\.id) == ["qwen3.5-4b", "gemma-4-e4b", "qwen3.5-9b", "gemma-4-12b"])
        #expect(catalog.classicTranslationModels.map(\.id) == ["nllb-1.3b", "madlad-3b"])
        #expect(catalog.translationModels.map(\.id) == catalog.llamaTranslationModels.map(\.id) + ["nllb-1.3b", "madlad-3b"])
        let gemma = catalog.llamaTranslationModels[1]
        #expect(gemma.sizeMB == 4747 && gemma.accuracy == 5 && gemma.supports(language: "zh"))
        #expect(catalog.classicTranslationModels[0].hasNonCommercialLicense)
    }

    @Test func translationMenuTitles() {
        let catalog = ModelCatalog(engine: nil)
        #expect(ModelLabel.name(for: "gemma-4-12b") == "Gemma 4 12B")
        #expect(ModelLabel.translationMenuTitle(for: catalog.llamaTranslationModels[0]) == "Qwen 3.5 4B · ilk kullanımda ~2,6 GB indirilir")
        var ready = catalog.llamaTranslationModels[0]
        ready.downloaded = true
        #expect(ModelLabel.translationMenuTitle(for: ready) == "Qwen 3.5 4B · hazır")
        #expect(ModelLabel.translationMenuTitle(for: catalog.classicTranslationModels[0]).hasSuffix("Ticari olmayan lisans"))
    }

    // MARK: - Offline mode

    @Test func offlineModeDefaultsOnPersistsAndReachesTheEngine() {
        let (vm, state, defaults) = makeModel()
        #expect(vm.offlineMode)
        #expect(state.settings.offline)
        vm.offlineMode = false
        #expect(defaults.bool(forKey: "offlineMode") == false)
        #expect(!state.settings.offline)
        let (again, againState, _) = makeModel(defaults: defaults)
        #expect(!again.offlineMode)
        #expect(!againState.settings.offline)
        again.externalLLM = ExternalLLM(url: "192.168.1.5:1234", model: "m")
        #expect(againState.settings == EngineLaunchSettings(offline: false, externalLLM: ExternalLLM(url: "192.168.1.5:1234", model: "m")))
    }

    @Test func missingModelBlocksTheJobWhileOffline() async throws {
        let (vm, _, _) = makeModel(engine: try fakeEngine(models: Self.modelsEvent))
        await vm.catalog.load()
        vm.diarize = true
        vm.model = "large-v3"
        #expect(isBlocked(vm, ["large-v3", "speaker-diarization", "large-v3"]))
        guard case .failed(let message) = vm.status else { Issue.record("expected a failure"); return }
        #expect(message.text == "En doğru indirilmemiş. Çevrimdışı mod açıkken model indirilemez.")
        #expect(vm.canGoOnlineAndRetry)
    }

    @Test func nothingIsBlockedWhenOnlineDownloadedUnknownOrNotYetLoaded() async throws {
        let (vm, _, _) = makeModel(engine: try fakeEngine(models: Self.modelsEvent))
        // The catalog is still the fallback list, whose download flags mean nothing.
        #expect(!isBlocked(vm, ["large-v3"]))
        await vm.catalog.load()
        #expect(!isBlocked(vm, ["speaker-diarization", "qwen3.5-4b", "not-in-catalog"]))
        #expect(!vm.canGoOnlineAndRetry)
        vm.offlineMode = false
        #expect(!isBlocked(vm, ["large-v3"]))
    }

    @Test func externalLLMExcludesTranslationModelsFromTheCheck() async throws {
        let (vm, _, _) = makeModel(engine: try fakeEngine(models: Self.modelsEvent))
        await vm.catalog.load()
        #expect(isBlocked(vm, ["madlad-3b"]))
        vm.externalLLM = ExternalLLM(url: "192.168.1.5:11434")
        #expect(!isBlocked(vm, ["madlad-3b"]))
        #expect(isBlocked(vm, ["base", "madlad-3b"]))
        guard case .failed(let message) = vm.status else { Issue.record("expected a failure"); return }
        #expect(message.text.hasPrefix("Hızlı indirilmemiş."))
    }

    @Test func goingOnlineTurnsOfflineModeOffAndRunsTheBlockedJobAgain() async throws {
        let engine = try fakeEngine(
            models: Self.modelsEvent,
            transcribe: """
            echo '{"event": "partial", "language": "?", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Mer", "words": []}]}'
            echo '{"event": "result", "language": "tr", "duration": 2, "audio": "/nonexistent.wav", "outputs": [], "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Merhaba.", "words": []}]}'
            """
        )
        let (vm, state, defaults) = makeModel(engine: engine)
        await vm.catalog.load()
        vm.diarize = false
        vm.model = "base"
        vm.openVideo(URL(filePath: "/tmp/video.mp4"))
        vm.generate()
        #expect(vm.canGoOnlineAndRetry)
        #expect(vm.sentences.isEmpty)

        vm.goOnlineAndRetry()
        #expect(!vm.offlineMode)
        #expect(defaults.bool(forKey: "offlineMode") == false)
        #expect(!state.settings.offline)
        #expect(!vm.canGoOnlineAndRetry)
        await vm.job?.value
        #expect(vm.status == .finished)
        #expect(vm.sentences.map(\.text) == ["Merhaba."])
        #expect(vm.detectedLanguage == "tr")
    }

    @Test func translateIsBlockedWithTheTranslationModel() async throws {
        let (vm, _, _) = makeModel(engine: try fakeEngine(models: Self.modelsEvent))
        await vm.catalog.load()
        vm.sentences = [line(1, 0, 1, "Merhaba.")]
        vm.translationModel = "madlad-3b"
        vm.translate()
        guard case .failed(let message) = vm.status else { Issue.record("expected a failure"); return }
        #expect(message.text.hasPrefix("MADLAD-400 3B indirilmemiş."))
        #expect(vm.canGoOnlineAndRetry)
        #expect(vm.job == nil)
        // Any later status clears the retry offer.
        vm.status = .idle
        #expect(!vm.canGoOnlineAndRetry)
    }

    // MARK: - Display mode

    @Test func displayModeIsPersistedAndFallsBackToOriginalWithoutTranslations() {
        let (vm, _, defaults) = makeModel()
        vm.displayMode = .both
        #expect(defaults.string(forKey: "subtitleDisplayMode") == "both")
        #expect(vm.effectiveDisplayMode == .original)
        let (again, _, _) = makeModel(defaults: defaults)
        #expect(again.displayMode == .both)
        again.sentences = [line(1, 0, 1, "a")]
        again.applyTranslation([line(1, 0, 1, "b")], language: "en", source: again.sentences)
        #expect(again.effectiveDisplayMode == .both)
    }

    @Test func firstTranslationSwitchesDisplayAndExportToTheTranslation() {
        let (vm, _, _) = makeModel()
        vm.sentences = [line(1, 0, 1, "a"), line(2, 1, 2, "b")]
        #expect(vm.displayMode == .original && vm.exportLanguage == .original)
        #expect(vm.translationTitle == "Çeviri")
        vm.applyTranslation([line(1, 0, 1, "A")], language: "en", source: vm.sentences)
        #expect(vm.displayMode == .translation)
        #expect(vm.exportLanguage == .translated)
        #expect(vm.translationTitle == "Çeviri (en)")
        // The user goes back to the original; the next batch must not override that.
        vm.displayMode = .original
        vm.applyTranslation([line(1, 0, 1, "A"), line(2, 1, 2, "B")], language: "en", source: vm.sentences)
        #expect(vm.displayMode == .original && vm.exportLanguage == .original)
        #expect(vm.translations.count == 2)
    }

    @Test func displayModeAndExportLanguageFollowEachOther() {
        let (vm, _, _) = makeModel()
        vm.sentences = [line(1, 0, 1, "a")]
        vm.applyTranslation([line(1, 0, 1, "A")], language: "es", source: vm.sentences)
        vm.displayMode = .original
        #expect(vm.exportLanguage == .original)
        vm.displayMode = .translation
        #expect(vm.exportLanguage == .translated)
        vm.displayMode = .both
        #expect(vm.exportLanguage == .translated)
        vm.exportLanguage = .original
        #expect(vm.displayMode == .both)
        vm.displayMode = .translation
        vm.exportLanguage = .original
        #expect(vm.displayMode == .original)
        vm.exportLanguage = .translated
        #expect(vm.displayMode == .translation)
    }

    @Test func exportLanguageResetsWhenTranslationsGoAway() {
        let (vm, _, _) = makeModel()
        vm.sentences = [line(1, 0, 1, "a")]
        vm.applyTranslation([line(1, 0, 1, "A")], language: "en", source: vm.sentences)
        #expect(vm.exportLanguage == .translated)
        vm.translations = [:]
        #expect(vm.exportLanguage == .original)
        // With nothing translated, the display choice can't change the export.
        vm.displayMode = .translation
        #expect(vm.exportLanguage == .original)
    }

    @Test func translationEventsAreAppliedAsTheyArrive() async throws {
        let engine = try fakeEngine(translate: """
        echo '{"event": "translation", "language": "en", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Hello.", "words": []}]}'
        echo '{"event": "translation", "language": "en", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Hello.", "words": []}, {"id": 2, "start": 1, "end": 2, "text": "World.", "words": []}]}'
        """)
        let (vm, _, _) = makeModel(engine: engine)
        vm.offlineMode = false
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.sentences = [line(1, 0, 1, "Merhaba."), line(2, 1, 2, "Dünya.")]
        vm.translate()
        await vm.job?.value
        #expect(vm.status == .finished)
        #expect(vm.sentences.compactMap { vm.translation(for: $0)?.text } == ["Hello.", "World."])
        #expect(vm.translationLanguage == "en")
        #expect(vm.displayMode == .translation && vm.exportLanguage == .translated)
    }

    // MARK: - Live partial output

    @Test func partialSnapshotsKeepLineIdentitiesAndIgnoreUnknownLanguage() {
        let (vm, _, _) = makeModel()
        vm.applyPartial(language: "?", sentences: [line(1, 0, 1, "Mer")])
        #expect(vm.detectedLanguage == nil)
        let first = vm.sentences[0].uid
        vm.selection = first

        vm.applyPartial(language: "tr", sentences: [line(1, 0, 1.4, "Merhaba"), line(2, 1.4, 2, "dünya")])
        #expect(vm.detectedLanguage == "tr")
        #expect(vm.sentences.map(\.text) == ["Merhaba", "dünya"])
        #expect(vm.sentences[0].uid == first)
        #expect(vm.selection == first)
        let second = vm.sentences[1].uid

        vm.applyPartial(language: "tr", sentences: [line(1, 0, 1.4, "Merhaba"), line(2, 1.4, 2, "dünya"), line(3, 2, 3, "!")])
        #expect(vm.sentences.map(\.uid).prefix(2) == [first, second])
        #expect(vm.sentences.count == 3)
    }

    @Test func partialLanguageMovesTheTranslationTargetOffTheSource() {
        let (vm, _, _) = makeModel()
        vm.translationTarget = "en"
        vm.applyPartial(language: "en", sentences: [line(1, 0, 1, "Hello")])
        #expect(vm.translationTarget != "en")
    }
}
