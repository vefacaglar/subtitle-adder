import EngineKit
import Foundation
import LibraryKit
import Testing
@testable import SubtitleAdderApp

/// The toolbar's translation-model popover: list building, search, switching provider, the cancellable refresh and what
/// choosing a row changes. Every store here is a temporary file with in-memory keys.
@MainActor
@Suite struct TranslationModelPickerTests {
    private struct Down: Error, LocalizedError { var errorDescription: String? { "timed out" } }

    private final class Env {
        let folder = FileManager.default.temporaryDirectory.appending(path: "picker-tests-\(UUID().uuidString)")
        let secrets = InMemorySecretStore()
        let defaults = UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        var fileURL: URL { folder.appending(path: TranslationProviderStore.fileName) }
        deinit { try? FileManager.default.removeItem(at: folder) }
    }

    private nonisolated static func modelsBody(_ names: [String]) -> Data {
        let items = names.map { #"{"id": "\#($0)"}"# }.joined(separator: ",")
        return Data(#"{"data": [\#(items)]}"#.utf8)
    }

    private func makeVM(_ env: Env, fetch: ExternalLLMServer.Fetch? = nil) -> SubtitleViewModel {
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: nil), defaults: env.defaults, launchState: EngineLaunchState(), externalFetch: fetch,
            providerStore: TranslationProviderStore(fileURL: env.fileURL, secrets: env.secrets)
        )
        vm.offlineMode = false
        return vm
    }

    private func lan(_ name: String = "Mac mini", model: String = "", discovered: [String] = []) -> TranslationProviderProfile {
        TranslationProviderProfile(
            name: name, presetID: "lan", baseURL: "10.0.0.2:11434", model: model, asyncTranslation: false,
            discoveredModels: discovered
        )
    }

    private func openAI(model: String = "gpt-4o-mini", discovered: [String] = []) -> TranslationProviderProfile {
        var profile = TranslationProviderProfile(preset: .preset(id: "openai"))
        profile.model = model
        profile.discoveredModels = discovered
        return profile
    }

    private func waitUntilTesting(_ vm: SubtitleViewModel, _ id: UUID) async throws {
        for _ in 0..<300 where !vm.testingProviders.contains(id) { try await Task.sleep(for: .milliseconds(10)) }
        #expect(vm.testingProviders.contains(id))
        // Past the key read, into the request.
        try await Task.sleep(for: .milliseconds(50))
    }

    // MARK: - List

    @Test func aProvidersListIsTrimmedDistinctSortedAndHoldsTheCurrentModel() {
        let env = Env()
        let vm = makeVM(env)
        let provider = vm.addProvider(
            lan(model: "Gamma", discovered: ["zeta", "Alpha", " alpha ", "beta", "BETA", "Gamma"]), key: ""
        ).profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(provider.id)
        #expect(model.rows.map(\.title) == ["Alpha", "beta", "Gamma", "zeta"])
        #expect(model.rows.filter(\.isCurrent).map(\.title) == ["Gamma"])
        #expect(model.highlightedID == "Gamma")
    }

    @Test func aModelNotInTheReportedListIsStillListed() {
        let env = Env()
        let vm = makeVM(env)
        let provider = vm.addProvider(lan(model: "custom:7b", discovered: ["a", "b"]), key: "").profile
        #expect(TranslationModelPickerModel.modelNames(for: provider) == ["a", "b", "custom:7b"])
    }

    @Test func withNothingReportedThePresetsSuggestionsAreListedWithTheCurrentModel() {
        let env = Env()
        let vm = makeVM(env)
        let provider = vm.addProvider(openAI(model: "My-Own-Model"), key: "sk-1").profile
        let names = TranslationModelPickerModel.modelNames(for: provider)
        #expect(names.contains("My-Own-Model"))
        #expect(Set(names.map { $0.lowercased() }).count == names.count)
        #expect(names == names.sorted { $0.caseInsensitiveCompare($1) == .orderedAscending })
        #expect(names.count == provider.availableModels.count)
    }

    @Test func aReportedListReplacesTheSuggestions() {
        let env = Env()
        let vm = makeVM(env)
        let provider = vm.addProvider(openAI(model: "gpt-4o-mini", discovered: ["x-1", "x-2"]), key: "sk-1").profile
        #expect(TranslationModelPickerModel.modelNames(for: provider) == ["gpt-4o-mini", "x-1", "x-2"])
    }

    @Test func theLocalListIsTheTranslationCatalogWithTheCurrentOneChecked() {
        let env = Env()
        let vm = makeVM(env)
        vm.chooseLocalTranslationModel("madlad-3b")
        let model = TranslationModelPickerModel(vm: vm)
        #expect(model.browsedProviderID == nil)
        #expect(model.rows.map(\.id) == vm.catalog.translationModels.map(\.id))
        #expect(model.rows.filter(\.isCurrent).map(\.id) == ["madlad-3b"])
        // The same titles the old menu showed, with the non-commercial mark.
        let nllb = model.rows.first { $0.id == "nllb-1.3b" }
        #expect(nllb?.title.hasSuffix("Ticari olmayan lisans") == true)
    }

    @Test func noLocalModelIsCheckedWhileAProviderIsActive() {
        let env = Env()
        let vm = makeVM(env)
        let provider = vm.addProvider(lan(model: "m"), key: "").profile
        let model = TranslationModelPickerModel(vm: vm)
        #expect(model.browsedProviderID == provider.id)
        model.selectProvider(nil)
        #expect(model.rows.allSatisfy { !$0.isCurrent })
    }

    @Test func providersAreListedOnDeviceFirstThenInStoredOrder() {
        let env = Env()
        let vm = makeVM(env)
        vm.addProvider(lan("Zed"), key: "")
        vm.addProvider(lan("Alpha"), key: "")
        let model = TranslationModelPickerModel(vm: vm)
        #expect(model.providerChoices.map(\.name) == ["Yerel modeller (Cihaz üzerinde)", "Zed", "Alpha"])
        #expect(model.providerChoices.first?.id == nil)
    }

    // MARK: - Search

    @Test func searchIsCaseInsensitiveContainsOnTheTitle() {
        let env = Env()
        let vm = makeVM(env)
        let provider = vm.addProvider(lan(model: "llama3", discovered: ["llama3", "Qwen3-14B", "gemma-4"]), key: "").profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(provider.id)
        model.searchText = "QWEN"
        #expect(model.rows.map(\.title) == ["Qwen3-14B"])
        model.searchText = "  a "
        #expect(model.rows.map(\.title) == ["gemma-4", "llama3"])
        model.searchText = "nothing"
        #expect(model.rows.isEmpty)
        #expect(model.emptyText == "Aramayla eşleşen model yok.")
    }

    @Test func anEmptyListSaysToRefresh() {
        let env = Env()
        let vm = makeVM(env)
        // A custom address has no suggestions to fall back on.
        let custom = TranslationProviderProfile(name: "Custom", presetID: "custom", baseURL: "10.0.0.2:8080")
        let provider = vm.addProvider(custom, key: "").profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(provider.id)
        #expect(model.rows.isEmpty)
        #expect(model.emptyText == "Model listesi boş. Yenile düğmesini kullanın.")
        #expect(model.canRefresh)
    }

    @Test func theHighlightFollowsTheSearchAndTheArrowsSkipNothingEnabled() {
        let env = Env()
        let vm = makeVM(env)
        let provider = vm.addProvider(lan(model: "b", discovered: ["a", "b", "c"]), key: "").profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(provider.id)
        #expect(model.highlightedID == "b")
        model.moveHighlight(by: 1)
        #expect(model.highlightedID == "c")
        model.moveHighlight(by: 1)
        #expect(model.highlightedID == "c")
        model.moveHighlight(by: -5)
        #expect(model.highlightedID == "a")
        model.searchText = "c"
        #expect(model.highlightedID == "c")
        model.searchText = "zzz"
        #expect(model.highlightedID == nil)
        #expect(!model.chooseHighlighted())
    }

    // MARK: - Choosing

    @Test func choosingAProvidersModelActivatesItWithThatModelAndRemembersItPerProvider() {
        let env = Env()
        let vm = makeVM(env)
        let one = vm.addProvider(lan("One", model: "m1", discovered: ["m1", "m2"]), key: "", activate: false).profile
        let two = vm.addProvider(lan("Two", model: "x1", discovered: ["x1", "x2"]), key: "", activate: false).profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(one.id)
        // Browsing does not switch anything.
        #expect(vm.providerStore.activeProviderID == nil)
        #expect(model.choose("m2"))
        #expect(vm.providerStore.activeProviderID == one.id)
        model.selectProvider(two.id)
        #expect(model.choose("x2"))
        #expect(vm.providerStore.activeProviderID == two.id)
        let stored = Dictionary(uniqueKeysWithValues: vm.providerStore.providers.map { ($0.name, $0.model) })
        #expect(stored == ["One": "m2", "Two": "x2"])
        // Not a listed model: refused.
        #expect(!model.choose("not-listed"))
        #expect(vm.providerStore.providers.first { $0.id == two.id }?.model == "x2")
    }

    @Test func aProvidersModelIsCheckedOnlyWhileThatProviderIsActive() {
        let env = Env()
        let vm = makeVM(env)
        let one = vm.addProvider(lan("One", model: "m1", discovered: ["m1", "m2"]), key: "", activate: false).profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(one.id)
        #expect(model.rows.allSatisfy { !$0.isCurrent })
        // The highlight still starts on what the provider remembers.
        #expect(model.highlightedID == "m1")
        vm.activateProvider(one.id)
        #expect(model.rows.filter(\.isCurrent).map(\.id) == ["m1"])
    }

    @Test func localAndProviderSelectionsArePersistedSeparately() {
        let env = Env()
        let vm = makeVM(env)
        let provider = vm.addProvider(lan(model: "m1", discovered: ["m1", "m2"]), key: "", activate: false).profile
        let model = TranslationModelPickerModel(vm: vm)

        model.selectProvider(nil)
        #expect(model.choose("qwen3.5-4b"))
        #expect(vm.translationModel == "qwen3.5-4b" && vm.providerStore.activeProviderID == nil)

        model.selectProvider(provider.id)
        #expect(model.choose("m2"))
        #expect(vm.providerStore.activeProviderID == provider.id)
        // The local choice is untouched by the provider's.
        #expect(vm.translationModel == "qwen3.5-4b")

        model.selectProvider(nil)
        #expect(model.choose("madlad-3b"))
        #expect(vm.providerStore.activeProviderID == nil && vm.translationModel == "madlad-3b")
        // The provider kept its model.
        #expect(vm.providerStore.providers[0].model == "m2")

        // All of it survives a relaunch.
        let reloaded = TranslationProviderStore(fileURL: env.fileURL, secrets: env.secrets)
        #expect(reloaded.activeProviderID == nil && reloaded.providers[0].model == "m2")
        #expect(makeVM(env).translationModel == "madlad-3b")
    }

    @Test func returnChoosesTheHighlightedRow() {
        let env = Env()
        let vm = makeVM(env)
        let provider = vm.addProvider(lan(model: "a", discovered: ["a", "b"]), key: "", activate: false).profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(provider.id)
        model.moveHighlight(by: 1)
        #expect(model.chooseHighlighted())
        #expect(vm.providerStore.providers[0].model == "b" && vm.providerStore.activeProviderID == provider.id)
    }

    // MARK: - Offline and unusable providers

    @Test func offlineModeBlocksChoosingACloudModelButNotALanServer() {
        let env = Env()
        let vm = makeVM(env)
        let cloud = vm.addProvider(openAI(model: "gpt-4o-mini", discovered: ["gpt-4o-mini", "gpt-4o"]), key: "sk-1", activate: false).profile
        let server = vm.addProvider(lan(model: "m1", discovered: ["m1", "m2"]), key: "", activate: false).profile
        vm.offlineMode = true
        let model = TranslationModelPickerModel(vm: vm)

        model.selectProvider(cloud.id)
        #expect(model.rows.count == 2 && model.rows.allSatisfy { !$0.isEnabled })
        #expect(model.statusText == "Çevrimdışı mod açık: internetteki sağlayıcılara bağlanılmaz.")
        #expect(!model.canRefresh)
        #expect(!model.choose("gpt-4o"))
        #expect(!model.chooseHighlighted())
        #expect(vm.providerStore.activeProviderID == nil)
        #expect(vm.providerStore.providers.first { $0.id == cloud.id }?.model == "gpt-4o-mini")

        model.selectProvider(server.id)
        #expect(model.rows.allSatisfy(\.isEnabled) && model.statusText == nil)
        #expect(model.choose("m2"))
        #expect(vm.providerStore.activeProviderID == server.id)
    }

    @Test func aProviderWithoutAUsableAddressCannotBeChosenFrom() {
        let env = Env()
        let vm = makeVM(env)
        let blank = vm.addProvider(
            TranslationProviderProfile(name: "Blank", presetID: "lan", baseURL: " ", model: "m", discoveredModels: ["m", "n"]),
            key: "", activate: false
        ).profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(blank.id)
        #expect(model.statusText == "Lütfen geçerli bir sunucu adresi girin.")
        #expect(!model.choose("n"))
        #expect(vm.providerStore.activeProviderID == nil)
    }

    // MARK: - Removal

    @Test func aProviderRemovedMeanwhileRefusesTheChoiceAndFallsBackToTheLocalModels() {
        let env = Env()
        let vm = makeVM(env)
        let provider = vm.addProvider(lan(model: "m1", discovered: ["m1", "m2"]), key: "", activate: false).profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(provider.id)
        vm.removeProvider(provider.id)
        // The popover has not been told yet: choosing must still not touch anything.
        #expect(model.rows.isEmpty)
        #expect(!model.choose("m2"))
        #expect(vm.providerStore.activeProviderID == nil && vm.providerStore.providers.isEmpty)
        #expect(model.browsedProviderID == nil)
        #expect(model.statusText == "Bu sağlayıcı artık yok.")
        #expect(model.rows.map(\.id) == vm.catalog.translationModels.map(\.id))
    }

    @Test func removingTheBrowsedProviderCancelsItsRefresh() async throws {
        let env = Env()
        let gate = AsyncGate()
        let vm = makeVM(env, fetch: { _ in
            await gate.wait()
            return (Self.modelsBody(["late"]), 200)
        })
        let provider = vm.addProvider(lan(), key: "").profile
        let model = TranslationModelPickerModel(vm: vm)
        model.refresh()
        try await waitUntilTesting(vm, provider.id)
        vm.removeProvider(provider.id)
        model.providersChanged()
        await gate.open()
        await model.waitForRefresh()
        #expect(!model.isRefreshing && vm.providerStore.providers.isEmpty)
        #expect(vm.providerNotices[provider.id] == nil)
    }

    // MARK: - Refresh

    @Test func refreshFillsTheListAndSaysHowItWent() async {
        let env = Env()
        let vm = makeVM(env, fetch: { _ in (Self.modelsBody(["b-model", "a-model"]), 200) })
        let provider = vm.addProvider(lan(), key: "", activate: false).profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(provider.id)
        model.refresh()
        #expect(model.isRefreshing && model.statusText == "Modeller yenileniyor…")
        #expect(!model.canRefresh)
        await model.waitForRefresh()
        #expect(!model.isRefreshing && model.canRefresh)
        #expect(model.rows.map(\.title) == ["a-model", "b-model"])
        #expect(model.statusText?.hasPrefix("Bağlantı başarılı") == true)
        // Refreshing a provider that isn't active selects nothing.
        #expect(vm.providerStore.activeProviderID == nil)
    }

    @Test func aFailedRefreshKeepsTheListAndShowsTheReason() async {
        let env = Env()
        let vm = makeVM(env, fetch: { _ in throw Down() })
        let provider = vm.addProvider(lan(model: "m1", discovered: ["m1", "m2"]), key: "").profile
        let model = TranslationModelPickerModel(vm: vm)
        model.refresh()
        await model.waitForRefresh()
        #expect(model.rows.map(\.title) == ["m1", "m2"])
        #expect(model.statusText == "Modeller yenilenemedi: Sunucuya ulaşılamadı: timed out")
        #expect(vm.providerStore.providers.first { $0.id == provider.id }?.discoveredModels == ["m1", "m2"])
        #expect(model.canRefresh)
    }

    @Test func refreshIsOnlyForASavedProvider() {
        let env = Env()
        let vm = makeVM(env)
        let model = TranslationModelPickerModel(vm: vm)
        #expect(model.browsedProvider == nil && !model.canRefresh)
        model.refresh()
        #expect(!model.isRefreshing)
    }

    @Test func switchingProviderCancelsTheRefreshClearsTheSearchAndALateAnswerChangesNothing() async throws {
        let env = Env()
        let gate = AsyncGate()
        let vm = makeVM(env, fetch: { _ in
            await gate.wait()
            return (Self.modelsBody(["late-model"]), 200)
        })
        let one = vm.addProvider(lan("One", model: "m1", discovered: ["m1"]), key: "", activate: false).profile
        let two = vm.addProvider(lan("Two", model: "x1", discovered: ["x1"]), key: "", activate: false).profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(one.id)
        model.searchText = "m"
        model.refresh()
        try await waitUntilTesting(vm, one.id)

        model.selectProvider(two.id)
        #expect(model.searchText.isEmpty && !model.isRefreshing)
        #expect(model.statusText == nil)
        #expect(model.rows.map(\.title) == ["x1"])

        await gate.open()
        for _ in 0..<300 where vm.testingProviders.contains(one.id) { try await Task.sleep(for: .milliseconds(10)) }
        #expect(!vm.testingProviders.contains(one.id))
        // Nothing arrived: not in the list, the status or the store.
        #expect(model.statusText == nil && model.rows.map(\.title) == ["x1"])
        #expect(vm.providerStore.providers.first { $0.id == one.id }?.discoveredModels == ["m1"])
        #expect(vm.providerNotices[one.id] == nil)
    }

    @Test func closingThePopoverCancelsTheRefresh() async throws {
        let env = Env()
        let gate = AsyncGate()
        let vm = makeVM(env, fetch: { _ in
            await gate.wait()
            return (Self.modelsBody(["late-model"]), 200)
        })
        let provider = vm.addProvider(lan(model: "m1", discovered: ["m1"]), key: "").profile
        let model = TranslationModelPickerModel(vm: vm)
        model.refresh()
        try await waitUntilTesting(vm, provider.id)
        model.close()
        #expect(!model.isRefreshing)
        await gate.open()
        await model.waitForRefresh()
        for _ in 0..<300 where vm.testingProviders.contains(provider.id) { try await Task.sleep(for: .milliseconds(10)) }
        #expect(vm.providerStore.providers.first { $0.id == provider.id }?.discoveredModels == ["m1"])
        #expect(model.statusText == nil)
    }

    @Test func anEditMeanwhileDropsTheAnswer() async throws {
        let env = Env()
        let gate = AsyncGate()
        let vm = makeVM(env, fetch: { _ in
            await gate.wait()
            return (Self.modelsBody(["old-server-model"]), 200)
        })
        let profile = vm.addProvider(lan(model: "m1", discovered: ["m1"]), key: "").profile
        let model = TranslationModelPickerModel(vm: vm)
        model.refresh()
        try await waitUntilTesting(vm, profile.id)
        var edited = profile
        edited.baseURL = "10.0.0.9:11434"
        vm.updateProvider(edited)
        await gate.open()
        await model.waitForRefresh()
        #expect(vm.providerStore.providers[0].discoveredModels == ["m1"])
        #expect(model.statusText == nil && !model.isRefreshing)
    }

    @Test func openingAgainStartsFromTheActiveProviderWithAnEmptySearch() {
        let env = Env()
        let vm = makeVM(env)
        let one = vm.addProvider(lan("One"), key: "", activate: false).profile
        let two = vm.addProvider(lan("Two"), key: "").profile
        let model = TranslationModelPickerModel(vm: vm)
        model.selectProvider(one.id)
        model.searchText = "abc"
        model.open()
        #expect(model.browsedProviderID == two.id && model.searchText.isEmpty)
        vm.activateProvider(nil)
        model.open()
        #expect(model.browsedProviderID == nil)
    }

    // MARK: - Texts

    @Test func theToolbarTitleAndTooltipNameTheModelAndWhereItRuns() {
        let env = Env()
        let vm = makeVM(env)
        let model = TranslationModelPickerModel(vm: vm)
        #expect(model.buttonSymbol == nil)
        #expect(model.helpText.hasPrefix(vm.translationModelTitle + "\n"))
        let cloud = vm.addProvider(openAI(), key: "sk-1").profile
        #expect(model.buttonSymbol == "cloud")
        #expect(model.helpText.hasPrefix("OpenAI: gpt-4o-mini\n") && model.helpText.contains(cloud.name))
        vm.addProvider(lan(), key: "")
        #expect(model.buttonSymbol == "network")
    }

    @Test func everyKeyOfThePickerIsInTheCatalogInBothLanguages() throws {
        let package = URL(filePath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let catalogURL = package.appending(path: "Sources/SubtitleAdderApp/Resources/Localizable.xcstrings")
        let root = try #require(JSONSerialization.jsonObject(with: Data(contentsOf: catalogURL)) as? [String: Any])
        let strings = try #require(root["strings"] as? [String: [String: Any]])
        let pattern = try NSRegularExpression(pattern: #"(?:L\(|\.key\()"((?:[^"\\]|\\.)*)""#)
        var checked = 0
        for name in ["TranslationModelPicker.swift", "TranslationModelPickerModel.swift"] {
            let text = try String(contentsOf: package.appending(path: "Sources/SubtitleAdderApp/\(name)"), encoding: .utf8)
            for match in pattern.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                let key = String(text[Range(match.range(at: 1), in: text)!])
                let localizations = strings[key]?["localizations"] as? [String: [String: [String: String]]]
                for language in ["tr", "en"] {
                    #expect(localizations?[language]?["stringUnit"]?["value"]?.isEmpty == false, "\(name): \(key) has no \(language) text")
                }
                checked += 1
            }
        }
        #expect(checked >= 15)
    }
}

/// Holds a fake server's answer back until the test lets it go.
private actor AsyncGate {
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if isOpen { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        isOpen = true
        waiters.forEach { $0.resume() }
        waiters = []
    }
}
