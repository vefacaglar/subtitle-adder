import EngineKit
import Foundation
import LibraryKit
import Testing
@testable import SubtitleAdderApp

/// Translation providers in the view model: what reaches the engine, the toolbar title and actions, the connection
/// tests and saving. Never touches the real keychain or Application Support: every store here is a temporary file with
/// in-memory keys.
@MainActor
@Suite struct ProviderTests {
    /// A providers file in a temporary folder and an in-memory key store; `store()` opens them again, as a relaunch would.
    @MainActor private final class Env {
        let folder = FileManager.default.temporaryDirectory.appending(path: "provider-tests-\(UUID().uuidString)")
        let secrets = InMemorySecretStore()
        let defaults = UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        var fileURL: URL { folder.appending(path: TranslationProviderStore.fileName) }

        func store(secrets: any SecretStore? = nil) -> TranslationProviderStore {
            TranslationProviderStore(fileURL: fileURL, secrets: secrets ?? self.secrets)
        }

        deinit { try? FileManager.default.removeItem(at: folder) }
    }

    /// Requests a fake `Fetch` saw.
    private final class Requests: @unchecked Sendable {
        private let lock = NSLock()
        private var items: [URLRequest] = []
        func add(_ request: URLRequest) { lock.withLock { items.append(request) } }
        var all: [URLRequest] { lock.withLock { items } }
    }

    private nonisolated static func modelsBody(_ names: [String]) -> Data {
        let items = names.map { #"{"id": "\#($0)"}"# }.joined(separator: ",")
        return Data(#"{"data": [\#(items)]}"#.utf8)
    }

    private func makeModel(
        _ env: Env, store: TranslationProviderStore? = nil, fetch: ExternalLLMServer.Fetch? = nil
    ) -> (SubtitleViewModel, EngineLaunchState) {
        let state = EngineLaunchState()
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: nil), defaults: env.defaults, launchState: state, externalFetch: fetch,
            providerStore: store ?? env.store()
        )
        vm.offlineMode = false
        return (vm, state)
    }

    private func openAI(model: String = "gpt-4o-mini", isAsync asyncTranslation: Bool = true) -> TranslationProviderProfile {
        var profile = TranslationProviderProfile(preset: .preset(id: "openai"))
        profile.model = model
        profile.asyncTranslation = asyncTranslation
        return profile
    }

    private func lan(url: String = "10.0.0.2:11434", model: String = "", isAsync asyncTranslation: Bool = false) -> TranslationProviderProfile {
        TranslationProviderProfile(
            name: "Mac mini", presetID: "lan", baseURL: url, model: model, asyncTranslation: asyncTranslation
        )
    }

    // MARK: - What reaches the engine

    @Test func theActiveProvidersAddressModelAndKeyReachTheEngine() throws {
        let env = Env()
        let (vm, state) = makeModel(env)
        #expect(state.settings.externalLLM == nil && !vm.isExternalLLMActive)
        let added = vm.addProvider(openAI(), key: "  sk-test-1234567890 \n")
        #expect(added.keyStored)
        #expect(vm.isExternalLLMActive)
        #expect(state.settings == EngineLaunchSettings(
            offline: false,
            externalLLM: ExternalLLM(url: "https://api.openai.com/v1", model: "gpt-4o-mini", apiKey: "sk-test-1234567890")
        ))
        // The key lives in the key store only, never in the providers file.
        #expect(try !String(contentsOf: env.fileURL, encoding: .utf8).contains("sk-test"))
        #expect(env.secrets.accounts == [TranslationProviderStore.secretAccount(for: added.profile.id)])

        // The offline switch keeps working next to it.
        vm.offlineMode = true
        #expect(state.settings.offline && state.settings.externalLLM?.apiKey == "sk-test-1234567890")

        vm.activateProvider(nil)
        #expect(state.settings.externalLLM == nil && !vm.isExternalLLMActive)
        vm.activateProvider(added.profile.id)
        #expect(state.settings.externalLLM?.url == "https://api.openai.com/v1")
        vm.removeProvider(added.profile.id)
        #expect(state.settings.externalLLM == nil && !vm.isExternalLLMActive)
        #expect(env.secrets.accounts.isEmpty)
    }

    @Test func modelAndKeyEditsFollowIntoTheLaunchSettings() {
        let env = Env()
        let (vm, state) = makeModel(env)
        let added = vm.addProvider(openAI(), key: "sk-old").profile
        vm.setProviderModel("  gpt-4.1-mini ", for: added.id)
        #expect(state.settings.externalLLM == ExternalLLM(url: "https://api.openai.com/v1", model: "gpt-4.1-mini", apiKey: "sk-old"))
        var edited = added
        edited.model = "gpt-4.1-nano"
        #expect(vm.updateProvider(edited, key: "sk-new")?.keyStored == true)
        #expect(state.settings.externalLLM?.apiKey == "sk-new" && state.settings.externalLLM?.model == "gpt-4.1-nano")
        // Keeping the key (nil) and removing it (blank).
        vm.updateProvider(edited)
        #expect(state.settings.externalLLM?.apiKey == "sk-new")
        vm.updateProvider(edited, key: "  ")
        #expect(state.settings.externalLLM?.apiKey == "")
        #expect(vm.providerStore.maskedKey(for: added.id) == nil)
    }

    @Test func aProviderWithoutAUsableAddressIsNotActive() {
        let env = Env()
        let (vm, state) = makeModel(env)
        vm.addProvider(lan(url: "   "), key: "")
        #expect(vm.providerStore.activeProvider != nil)
        #expect(!vm.isExternalLLMActive && !vm.isExternalAsyncActive && state.settings.externalLLM == nil)
        #expect(vm.translationModelTitle == ModelLabel.name(for: vm.translationModel))
    }

    // MARK: - Toolbar title and async flag

    @Test func theToolbarTitleNamesTheProviderAndItsModel() {
        let env = Env()
        let (vm, _) = makeModel(env)
        vm.translationModel = "madlad-3b"
        #expect(vm.translationModelTitle == ModelLabel.name(for: "madlad-3b"))

        let cloud = vm.addProvider(openAI(), key: "sk-1").profile
        #expect(vm.translationModelTitle == "OpenAI: gpt-4o-mini")
        vm.setProviderModel("", for: cloud.id)
        #expect(vm.translationModelTitle == "OpenAI: Otomatik")

        // A LAN server keeps its earlier wording, with "(Asenkron)" when it translates in the background.
        let server = vm.addProvider(lan(), key: "").profile
        #expect(vm.translationModelTitle == "Harici: Harici LLM")
        vm.setProviderModel(" gemma4:e4b ", for: server.id)
        #expect(vm.translationModelTitle == "Harici: gemma4:e4b")
        var backgrounded = vm.providerStore.providers[1]
        backgrounded.asyncTranslation = true
        vm.updateProvider(backgrounded)
        #expect(vm.translationModelTitle == "Harici: gemma4:e4b (Asenkron)")

        // A renamed cloud provider shows its own name.
        var renamed = vm.providerStore.providers[0]
        renamed.name = "Ofis hesabı"
        vm.updateProvider(renamed)
        vm.activateProvider(cloud.id)
        #expect(vm.translationModelTitle == "Ofis hesabı: Otomatik")
    }

    @Test func theAsyncFlagComesFromTheActiveProvider() {
        let env = Env()
        let (vm, _) = makeModel(env)
        #expect(!vm.isExternalAsyncActive)
        let asyncOn = vm.addProvider(openAI(isAsync: true), key: "sk-1").profile
        #expect(vm.isExternalLLMActive && vm.isExternalAsyncActive)
        let asyncOff = vm.addProvider(lan(isAsync: false), key: "").profile
        // The new provider is the active one now, and it translates one step after the other.
        #expect(vm.isExternalLLMActive && !vm.isExternalAsyncActive)
        vm.activateProvider(asyncOn.id)
        #expect(vm.isExternalAsyncActive)
        vm.activateProvider(asyncOff.id)
        #expect(!vm.isExternalAsyncActive)
        vm.activateProvider(nil)
        #expect(!vm.isExternalLLMActive && !vm.isExternalAsyncActive)
    }

    // MARK: - Launch

    @Test func theLegacyLANServerBecomesAProviderAtLaunch() {
        let env = Env()
        env.defaults.set(true, forKey: "externalLlmEnabled")
        env.defaults.set(" 10.0.0.2:1234 ", forKey: "externalLlmUrl")
        env.defaults.set("gemma4:e4b", forKey: "externalLlmModel")
        env.defaults.set(true, forKey: "externalLlmAsync")
        let (vm, state) = makeModel(env)
        #expect(vm.providerStore.providers.count == 1)
        let provider = vm.providerStore.providers[0]
        #expect(provider.presetID == "lan" && provider.baseURL == "10.0.0.2:1234" && provider.asyncTranslation)
        #expect(vm.providerStore.activeProviderID == provider.id)
        // The launch settings are already right when the first engine process starts.
        #expect(state.settings.externalLLM == ExternalLLM(url: "10.0.0.2:1234", model: "gemma4:e4b"))
        #expect(vm.isExternalLLMActive && vm.isExternalAsyncActive)
        #expect(vm.translationModelTitle == "Harici: gemma4:e4b (Asenkron)")

        // The next launch finds the provider in the file and doesn't import again.
        let (again, againState) = makeModel(env)
        #expect(again.providerStore.providers == [provider])
        #expect(againState.settings.externalLLM == state.settings.externalLLM)
    }

    @Test func aSwitchedOffLegacyServerIsKeptButNotActive() {
        let env = Env()
        env.defaults.set(false, forKey: "externalLlmEnabled")
        env.defaults.set("10.0.0.2:1234", forKey: "externalLlmUrl")
        let (vm, state) = makeModel(env)
        #expect(vm.providerStore.providers.count == 1 && vm.providerStore.activeProviderID == nil)
        #expect(state.settings.externalLLM == nil && !vm.isExternalLLMActive)
    }

    @Test func providersSurviveARelaunch() {
        let env = Env()
        let (vm, _) = makeModel(env)
        let a = vm.addProvider(openAI(), key: "sk-1", activate: false).profile
        let b = vm.addProvider(lan(model: "qwen3:8b"), key: "").profile
        let (again, state) = makeModel(env)
        #expect(again.providerStore.providers.map(\.id) == [a.id, b.id])
        #expect(again.providerStore.activeProviderID == b.id)
        #expect(state.settings.externalLLM == ExternalLLM(url: "10.0.0.2:11434", model: "qwen3:8b"))
        #expect(again.providerStore.apiKey(for: a.id) == "sk-1")
    }

    // MARK: - Toolbar actions

    @Test func toolbarActionsActivateTheRightProviderAndModel() {
        let env = Env()
        let (vm, state) = makeModel(env)
        let cloud = vm.addProvider(openAI(), key: "sk-1", activate: false).profile
        let server = vm.addProvider(lan(model: "gemma4:e4b"), key: "", activate: false).profile
        #expect(vm.providerStore.activeProviderID == nil && state.settings.externalLLM == nil)

        // Picking a model of a provider activates it with that model.
        vm.chooseProviderModel("gpt-4.1-mini", for: cloud.id)
        #expect(vm.providerStore.activeProviderID == cloud.id)
        #expect(state.settings.externalLLM == ExternalLLM(url: "https://api.openai.com/v1", model: "gpt-4.1-mini", apiKey: "sk-1"))
        // Another model of the active one only changes the model.
        vm.chooseProviderModel("gpt-4.1-nano", for: cloud.id)
        #expect(state.settings.externalLLM?.model == "gpt-4.1-nano")
        // Another provider's model switches over and leaves the first one's model alone.
        vm.chooseProviderModel("qwen3:8b", for: server.id)
        #expect(vm.providerStore.activeProviderID == server.id)
        #expect(state.settings.externalLLM == ExternalLLM(url: "10.0.0.2:11434", model: "qwen3:8b"))
        #expect(vm.providerStore.providers.first { $0.id == cloud.id }?.model == "gpt-4.1-nano")
        // "Etkinleştir" keeps the provider's own model.
        vm.activateProvider(cloud.id)
        #expect(state.settings.externalLLM?.model == "gpt-4.1-nano" && state.settings.externalLLM?.apiKey == "sk-1")

        // A local model switches the provider off and becomes the default.
        vm.chooseLocalTranslationModel("gemma-4-e4b")
        #expect(vm.providerStore.activeProviderID == nil && state.settings.externalLLM == nil)
        #expect(vm.translationModel == "gemma-4-e4b" && !vm.isExternalLLMActive)
        #expect(vm.translationModelTitle == ModelLabel.name(for: "gemma-4-e4b"))
    }

    // MARK: - Privacy

    @Test func onlyAProviderOutsideTheLocalNetworkIsACloudService() {
        let env = Env()
        let (vm, _) = makeModel(env)
        #expect(vm.activeCloudProviderName == nil)
        let cloud = vm.addProvider(openAI(), key: "sk-1").profile
        #expect(vm.activeCloudProviderName == "OpenAI")
        #expect(vm.sendsTextOverInternet(cloud))
        vm.addProvider(lan(), key: "")
        #expect(vm.activeCloudProviderName == nil)
        // The address decides, whatever the provider type says: a custom provider on the LAN stays local, a LAN
        // provider typed with a public address does not.
        vm.addProvider(TranslationProviderProfile(name: "NAS", presetID: "custom", baseURL: "http://nas.local:1234/v1"), key: "")
        #expect(vm.activeCloudProviderName == nil)
        vm.addProvider(lan(url: "https://llm.example.com"), key: "")
        #expect(vm.activeCloudProviderName == "Mac mini")
        vm.activateProvider(nil)
        #expect(vm.activeCloudProviderName == nil)
    }

    @Test func offlineModeBlocksTranslatingWithACloudProviderButNotWithALANServer() {
        let env = Env()
        let (vm, _) = makeModel(env)
        vm.addProvider(openAI(), key: "sk-1")
        vm.offlineMode = true
        var retried = false
        #expect(vm.blockedOffline([vm.model, vm.translationModel]) { retried = true })
        guard case .failed(let message) = vm.status else { Issue.record("not blocked"); return }
        #expect(message.text.contains("OpenAI"))
        #expect(vm.offlineRetryTitle == "Çevrimdışı modu kapat ve çevir")
        vm.goOnlineAndRetry()
        #expect(retried)
        #expect(!vm.offlineMode)
        // A job that doesn't translate never reaches the provider.
        vm.offlineMode = true
        #expect(!vm.blockedOffline([vm.model]) {})
        vm.addProvider(lan(), key: "")
        #expect(!vm.blockedOffline([vm.model, vm.translationModel]) {})
    }

    @Test func offlineModeSkipsConnectionTestsOfCloudProviders() async {
        let env = Env()
        let requests = Requests()
        let (vm, _) = makeModel(env, fetch: { request in
            requests.add(request)
            return (Self.modelsBody(["gemma-3"]), 200)
        })
        let cloud = vm.addProvider(openAI(model: ""), key: "sk-live").profile
        vm.offlineMode = true
        await vm.testConnection(of: cloud.id)
        await vm.refreshActiveProviderModelsIfNeeded()
        let draft = await vm.testConnection(of: ProviderDraft(editing: cloud, key: "sk-live"))
        #expect(requests.all.isEmpty)
        #expect(vm.providerNotices[cloud.id]?.isError == true)
        #expect(draft.notice?.isError == true)
        // A server on the local network is still tested.
        let local = vm.addProvider(lan(), key: "").profile
        await vm.testConnection(of: local.id)
        #expect(requests.all.count == 1)
    }

    // MARK: - Saving

    @Test func aRefusedKeychainWriteIsReported() {
        struct Refusing: SecretStore {
            func data(for account: String) -> Data? { nil }
            func set(_ data: Data, for account: String) -> Bool { false }
            func remove(_ account: String) {}
        }
        let env = Env()
        let (vm, state) = makeModel(env, store: env.store(secrets: Refusing()))
        let added = vm.addProvider(openAI(), key: "sk-1")
        #expect(!added.keyStored)
        // The provider is saved, without the key.
        #expect(vm.providerStore.providers.count == 1 && vm.providerStore.maskedKey(for: added.profile.id) == nil)
        #expect(state.settings.externalLLM?.apiKey == "")
        // Updating with the same key is refused again, which the sheet shows; a blank key has nothing to refuse.
        #expect(vm.updateProvider(added.profile, key: "sk-1")?.keyStored == false)
        #expect(vm.updateProvider(added.profile, key: "")?.keyStored == true)
        #expect(vm.updateProvider(added.profile)?.keyStored == true)
        #expect(vm.addProvider(lan(), key: "").keyStored)
    }

    @Test func aKeyThatReplacesAnotherMustBeTheOneStored() {
        // The store refuses the second write but still hands back the first key: that is not "stored".
        final class OnlyFirst: SecretStore, @unchecked Sendable {
            private let lock = NSLock()
            private var value: Data?
            func data(for account: String) -> Data? { lock.withLock { value } }
            func set(_ data: Data, for account: String) -> Bool {
                lock.withLock {
                    guard value == nil else { return false }
                    value = data
                    return true
                }
            }
            func remove(_ account: String) { lock.withLock { value = nil } }
        }
        let env = Env()
        let (vm, _) = makeModel(env, store: env.store(secrets: OnlyFirst()))
        let added = vm.addProvider(openAI(), key: "sk-first")
        #expect(added.keyStored)
        #expect(vm.updateProvider(added.profile, key: "sk-second")?.keyStored == false)
    }

    @Test func aDraftFollowsItsPresetAndForgetsModelsOfAnOldAddress() {
        var draft = ProviderDraft()
        #expect(draft.isNew && draft.preset.id == "opencode-go" && draft.profile.presetID == "opencode-go")
        draft.apiKey = "sk-keep"
        draft.discoveredModels = ["a", "b"]
        draft.apply(preset: .preset(id: "groq"))
        // The provider type is part of what gets saved (the Windows app lost it).
        #expect(draft.profile.presetID == "groq" && draft.preset.id == "groq")
        #expect(draft.name == "Groq" && draft.baseURL == "https://api.groq.com/openai/v1")
        #expect(draft.model == "llama-3.3-70b-versatile" && draft.discoveredModels.isEmpty)
        #expect(draft.apiKey == "sk-keep")
        #expect(draft.availableModels == ["llama-3.3-70b-versatile", "gemma2-9b-it"])

        draft.discoveredModels = ["x"]
        draft.baseURL = " https://api.groq.com/openai/v1 "
        #expect(draft.discoveredModels == ["x"])
        draft.baseURL = "https://other.example.com/v1"
        #expect(draft.discoveredModels.isEmpty)

        draft.apply(preset: .preset(id: "custom"))
        #expect(draft.baseURL.isEmpty && draft.model.isEmpty && !draft.hasUsableAddress)
        draft.baseURL = "host:1234"
        #expect(draft.hasUsableAddress)
    }

    @Test func savingADraftKeepsItsProviderTypeAndModels() {
        let env = Env()
        let (vm, _) = makeModel(env)
        var draft = ProviderDraft()
        draft.apply(preset: .preset(id: "deepseek"))
        draft.apiKey = "sk-ds"
        draft.discoveredModels = ["deepseek-chat", "deepseek-reasoner"]
        let saved = vm.addProvider(draft.profile, key: draft.apiKey)
        #expect(saved.profile.id == draft.id && saved.profile.presetID == "deepseek")
        #expect(vm.providerStore.providers[0].discoveredModels == ["deepseek-chat", "deepseek-reasoner"])
        // Editing opens with the stored key and the same identity.
        let editing = ProviderDraft(editing: saved.profile, key: vm.providerStore.apiKey(for: saved.profile.id))
        #expect(!editing.isNew && editing.id == draft.id && editing.apiKey == "sk-ds" && editing.preset.id == "deepseek")
    }

    // MARK: - Connection tests

    @Test func aTestSendsTheSavedKeyStoresTheModelsAndFillsAnEmptyModel() async {
        let env = Env()
        let requests = Requests()
        let (vm, state) = makeModel(env, fetch: { request in
            requests.add(request)
            return (Self.modelsBody(["gemma-3", "text-embedding-3-small", "gpt-4o-mini"]), 200)
        })
        let provider = vm.addProvider(openAI(model: ""), key: "sk-live").profile
        await vm.testConnection(of: provider.id)
        let request = requests.all.first
        #expect(request?.url?.absoluteString == "https://api.openai.com/v1/models")
        #expect(request?.value(forHTTPHeaderField: "Authorization") == "Bearer sk-live")
        // The embedding model can't translate and is left out.
        #expect(vm.providerStore.providers[0].discoveredModels == ["gemma-3", "gpt-4o-mini"])
        #expect(vm.providerStore.providers[0].model == "gemma-3")
        #expect(state.settings.externalLLM?.model == "gemma-3")
        #expect(vm.testingProviders.isEmpty)
        let notice = vm.providerNotices[provider.id]
        #expect(notice?.isError == false)
        #expect(notice?.message.text.hasPrefix("Bağlantı başarılı (api.openai.com · ") == true)
        #expect(notice?.message.text.hasSuffix("ms · 2 uygun model bulundu)") == true)
    }

    @Test func aTestKeepsTheModelTheUserChose() async {
        let env = Env()
        let (vm, _) = makeModel(env, fetch: { _ in (Self.modelsBody(["a", "b"]), 200) })
        let provider = vm.addProvider(openAI(model: "custom"), key: "sk-1").profile
        await vm.testConnection(of: provider.id)
        #expect(vm.providerStore.providers[0].model == "custom")
        #expect(vm.providerStore.providers[0].discoveredModels == ["a", "b"])
    }

    /// A provider that already knows two models, tested against a server that fails.
    private func failedTest(
        fetch: @escaping ExternalLLMServer.Fetch
    ) async -> (stored: TranslationProviderProfile?, notice: ExternalConnectionNotice?, testing: Bool) {
        let env = Env()
        let (vm, _) = makeModel(env, fetch: fetch)
        var profile = openAI()
        profile.discoveredModels = ["kept-1", "kept-2"]
        let id = vm.addProvider(profile, key: "sk-bad").profile.id
        await vm.testConnection(of: id)
        return (vm.providerStore.providers.first { $0.id == id }, vm.providerNotices[id], !vm.testingProviders.isEmpty)
    }

    @Test func aRefusedKeyIsReportedAndKeepsTheStoredModels() async {
        let result = await failedTest(fetch: { _ in (Data(), 401) })
        #expect(result.stored?.discoveredModels == ["kept-1", "kept-2"])
        #expect(result.notice?.isError == true)
        #expect(result.notice?.message.text.contains("(HTTP 401)") == true)
        #expect(!result.testing)
    }

    @Test func anUnreachableServerIsReportedAndKeepsTheStoredModels() async {
        struct Down: Error, LocalizedError { var errorDescription: String? { "timed out" } }
        let result = await failedTest(fetch: { _ in throw Down() })
        #expect(result.stored?.discoveredModels == ["kept-1", "kept-2"])
        #expect(result.notice?.isError == true)
        #expect(result.notice?.message.text == "Sunucuya ulaşılamadı: timed out")
        #expect(!result.testing)
    }

    @Test func aBlankAddressIsReported() async {
        let env = Env()
        let (vm, _) = makeModel(env, fetch: { _ in (Data(), 200) })
        let id = vm.addProvider(lan(url: "  "), key: "").profile.id
        await vm.testConnection(of: id)
        #expect(vm.providerNotices[id]?.isError == true)
        #expect(vm.providerNotices[id]?.message.text == "Lütfen geçerli bir sunucu adresi girin.")
    }

    @Test func aCancelledTestChangesNothing() async throws {
        let env = Env()
        let (vm, _) = makeModel(env, fetch: { _ in
            try await Task.sleep(for: .seconds(30))
            return (Data(), 200)
        })
        let id = vm.addProvider(openAI(), key: "sk-1").profile.id
        let test = Task { await vm.testConnection(of: id) }
        for _ in 0..<100 where !vm.testingProviders.contains(id) { try await Task.sleep(for: .milliseconds(10)) }
        #expect(vm.testingProviders.contains(id))
        test.cancel()
        await test.value
        #expect(vm.testingProviders.isEmpty)
        #expect(vm.providerNotices[id] == nil && vm.providerStore.providers[0].discoveredModels.isEmpty)
    }

    @Test func editingOrRemovingAProviderDropsItsTestResult() async throws {
        let env = Env()
        let (vm, _) = makeModel(env, fetch: { _ in (Self.modelsBody(["a"]), 200) })
        let profile = vm.addProvider(openAI(), key: "sk-1").profile
        await vm.testConnection(of: profile.id)
        #expect(vm.providerNotices[profile.id] != nil)
        var edited = profile
        edited.baseURL = "https://api.other.example/v1"
        vm.updateProvider(edited)
        #expect(vm.providerNotices[profile.id] == nil)

        await vm.testConnection(of: profile.id)
        vm.removeProvider(profile.id)
        #expect(vm.providerNotices[profile.id] == nil && vm.testingProviders.isEmpty)
    }

    @Test func anAnswerForAnAddressEditedMeanwhileIsDropped() async throws {
        let env = Env()
        let gate = AsyncGate()
        let (vm, _) = makeModel(env, fetch: { _ in
            await gate.wait()
            return (Self.modelsBody(["old-server-model"]), 200)
        })
        let profile = vm.addProvider(openAI(), key: "sk-1").profile
        let test = Task { await vm.testConnection(of: profile.id) }
        for _ in 0..<200 where !vm.testingProviders.contains(profile.id) { try await Task.sleep(for: .milliseconds(10)) }
        var edited = profile
        edited.baseURL = "https://api.other.example/v1"
        vm.updateProvider(edited)
        await gate.open()
        await test.value
        #expect(vm.providerNotices[profile.id] == nil)
        #expect(vm.providerStore.providers[0].discoveredModels.isEmpty)
    }

    @Test func testingADraftStoresNothing() async {
        let env = Env()
        let requests = Requests()
        let (vm, _) = makeModel(env, fetch: { request in
            requests.add(request)
            return (Self.modelsBody(["m1", "m2"]), 200)
        })
        var draft = ProviderDraft()
        draft.apply(preset: .preset(id: "openai"))
        draft.apiKey = "sk-draft"
        let result = await vm.testConnection(of: draft)
        #expect(result.models == ["m1", "m2"] && result.notice?.isError == false)
        #expect(requests.all.first?.value(forHTTPHeaderField: "Authorization") == "Bearer sk-draft")
        #expect(vm.providerStore.providers.isEmpty && env.secrets.accounts.isEmpty)

        draft.baseURL = "  "
        let blank = await vm.testConnection(of: draft)
        #expect(blank.models.isEmpty && blank.notice?.isError == true)
    }

    @Test func theToolbarListIsFilledAheadOfTimeOnlyForTheActiveProviderWithNothingKnown() async {
        final class Counter: @unchecked Sendable {
            private let lock = NSLock()
            private var value = 0
            func hit() { lock.withLock { value += 1 } }
            var count: Int { lock.withLock { value } }
        }
        let env = Env()
        let counter = Counter()
        let (vm, _) = makeModel(env, fetch: { _ in
            counter.hit()
            return (Self.modelsBody(["gemma4:e4b"]), 200)
        })
        await vm.refreshActiveProviderModelsIfNeeded()
        #expect(counter.count == 0)

        // A provider that isn't the active one isn't asked.
        let server = vm.addProvider(lan(), key: "", activate: false).profile
        await vm.refreshActiveProviderModelsIfNeeded()
        #expect(counter.count == 0)

        vm.activateProvider(server.id)
        await vm.refreshActiveProviderModelsIfNeeded()
        #expect(counter.count == 1 && vm.providerStore.providers[0].discoveredModels == ["gemma4:e4b"])
        // Known now: opening the menu again doesn't ask the server again.
        await vm.refreshActiveProviderModelsIfNeeded()
        #expect(counter.count == 1)
    }

    @Test func theLaunchSettingsFollowTheDiscoveredModelsOnlyForTheActiveProvider() async {
        let env = Env()
        let (vm, state) = makeModel(env, fetch: { _ in (Self.modelsBody(["first", "second"]), 200) })
        let other = vm.addProvider(openAI(model: ""), key: "sk-o", activate: false).profile
        let active = vm.addProvider(lan(), key: "").profile
        await vm.testConnection(of: other.id)
        // The other provider got its model, the engine still translates with the active one.
        #expect(vm.providerStore.providers.first { $0.id == other.id }?.model == "first")
        #expect(state.settings.externalLLM == ExternalLLM(url: "10.0.0.2:11434"))
        await vm.testConnection(of: active.id)
        #expect(state.settings.externalLLM == ExternalLLM(url: "10.0.0.2:11434", model: "first"))
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
