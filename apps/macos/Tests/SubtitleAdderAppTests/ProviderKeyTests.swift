import EngineKit
import Foundation
import LibraryKit
import Testing
@testable import SubtitleAdderApp

/// The API keys of the translation providers: read off the main thread into a cache the views and the launch settings
/// use, read again when a translation starts, and never lost to an edit that didn't touch them.
@MainActor
@Suite struct ProviderKeyTests {
    /// A key store that records where it was read and written, can refuse to be read, and can be changed behind the
    /// app's back (the user edited the keychain item elsewhere).
    private final class SpySecrets: SecretStore, @unchecked Sendable {
        private let lock = NSLock()
        private var values: [String: Data] = [:]
        private var readThreads: [Bool] = []
        private var writes = 0
        private var removals = 0
        private var refusing = false

        var unreadable: Bool {
            get { lock.withLock { refusing } }
            set { lock.withLock { refusing = newValue } }
        }
        /// Whether each read ran on the main thread, in order.
        var readsOnMain: [Bool] { lock.withLock { readThreads } }
        var writeCount: Int { lock.withLock { writes } }
        var removalCount: Int { lock.withLock { removals } }
        func stored(_ account: String) -> String? { lock.withLock { values[account].map { String(decoding: $0, as: UTF8.self) } } }

        func read(_ account: String) -> SecretRead {
            lock.withLock {
                readThreads.append(Thread.isMainThread)
                if refusing { return .unavailable }
                return values[account].map(SecretRead.found) ?? .absent
            }
        }

        func data(for account: String) -> Data? {
            if case .found(let data) = read(account) { return data }
            return nil
        }

        func set(_ data: Data, for account: String) -> Bool {
            lock.withLock {
                writes += 1
                values[account] = data
            }
            return true
        }

        func remove(_ account: String) {
            lock.withLock {
                removals += 1
                values[account] = nil
            }
        }
    }

    @MainActor private struct Env {
        let folder = FileManager.default.temporaryDirectory.appending(path: "key-tests-\(UUID().uuidString)")
        let secrets = SpySecrets()
        let defaults = UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        var fileURL: URL { folder.appending(path: TranslationProviderStore.fileName) }
        func store() -> TranslationProviderStore { TranslationProviderStore(fileURL: fileURL, secrets: secrets) }
    }

    private func makeModel(
        _ env: Env, engine: EngineClient? = nil, fetch: ExternalLLMServer.Fetch? = nil
    ) -> (SubtitleViewModel, EngineLaunchState) {
        let state = EngineLaunchState()
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine), defaults: env.defaults, launchState: state, externalFetch: fetch,
            providerStore: env.store()
        )
        vm.offlineMode = false
        return (vm, state)
    }

    private func openAI(model: String = "gpt-4o-mini") -> TranslationProviderProfile {
        var profile = TranslationProviderProfile(preset: .preset(id: "openai"))
        profile.model = model
        return profile
    }

    /// A providers file with an active OpenAI provider whose key is in `env.secrets`, as an earlier session left it.
    @discardableResult
    private func savedProvider(_ env: Env, key: String = "sk-saved") -> TranslationProviderProfile {
        let store = env.store()
        let stored = store.add(openAI(), key: key)
        #expect(env.secrets.stored(TranslationProviderStore.secretAccount(for: stored.id)) == key)
        return stored
    }

    private func settle(_ vm: SubtitleViewModel) async {
        while let job = vm.job {
            await job.value
            if vm.job == job { break }
        }
    }

    // MARK: - Launch: no keychain on the main thread

    @Test func launchNeverTouchesTheKeychainOnTheMainThread() async {
        let env = Env()
        let provider = savedProvider(env)
        let before = env.secrets.readsOnMain.count
        let (vm, state) = makeModel(env)
        // Nothing has suspended since the init: the launch settings are made without a key, the row says it is reading.
        #expect(state.settings.externalLLM == ExternalLLM(url: "https://api.openai.com/v1", model: "gpt-4o-mini"))
        #expect(vm.providerKey(for: provider.id) == .loading)
        #expect(vm.providerKeyCaption(for: provider.id) == "Anahtar okunuyor…")

        // The background read fills the cache and then publishes the launch settings, key included.
        await vm.waitForProviderKeys()
        #expect(vm.providerKey(for: provider.id) == .present("sk-saved"))
        #expect(state.settings.externalLLM?.apiKey == "sk-saved")
        #expect(vm.providerKeyCaption(for: provider.id) == "••••aved")
        let reads = Array(env.secrets.readsOnMain.dropFirst(before))
        #expect(!reads.isEmpty && reads.allSatisfy { !$0 }, "every launch read ran off the main thread")
    }

    @Test func drawingRowsAndTogglingOfflineReadsNothing() async {
        let env = Env()
        let provider = savedProvider(env)
        let (vm, state) = makeModel(env)
        await vm.waitForProviderKeys()
        let reads = env.secrets.readsOnMain.count
        for _ in 0..<50 {
            _ = vm.providerKeyCaption(for: provider.id)
            _ = vm.providerKey(for: provider.id)
        }
        vm.offlineMode = true
        vm.offlineMode = false
        vm.setProviderModel("gpt-4.1-mini", for: provider.id)
        vm.activateProvider(nil)
        vm.activateProvider(provider.id)
        #expect(env.secrets.readsOnMain.count == reads, "the cache answers; the keychain is not asked again")
        #expect(state.settings.externalLLM == ExternalLLM(url: "https://api.openai.com/v1", model: "gpt-4.1-mini", apiKey: "sk-saved"))
    }

    @Test func aRefusedReadIsNotTreatedAsNoKey() async {
        let env = Env()
        let provider = savedProvider(env)
        env.secrets.unreadable = true
        let (vm, state) = makeModel(env)
        await vm.waitForProviderKeys()
        #expect(vm.providerKey(for: provider.id) == .unreadable)
        #expect(vm.providerKeyCaption(for: provider.id) == "Anahtar okunamadı")
        #expect(vm.providerKeyCaption(for: provider.id) != "Anahtar yok")
        // The launch settings go on without a key; the keychain item itself is untouched.
        #expect(state.settings.externalLLM?.apiKey == "")
        #expect(env.secrets.removalCount == 0)

        // Access comes back (the user allowed it): the next read finds the key.
        env.secrets.unreadable = false
        #expect(await vm.providerKeyForUse(provider.id) == .present("sk-saved"))
        #expect(state.settings.externalLLM?.apiKey == "sk-saved")
    }

    @Test func aKnownKeySurvivesALaterFailedRead() async {
        let env = Env()
        let provider = savedProvider(env)
        let (vm, state) = makeModel(env)
        await vm.waitForProviderKeys()
        env.secrets.unreadable = true
        await vm.loadProviderKeys([provider.id])
        #expect(vm.providerKey(for: provider.id) == .present("sk-saved"))
        #expect(state.settings.externalLLM?.apiKey == "sk-saved")
    }

    // MARK: - A translation reads the key again

    @Test func aTranslateJobReadsTheKeyAgainOffTheMainThread() async throws {
        let env = Env()
        savedProvider(env, key: "sk-old")
        let log = env.folder.appending(path: "keys.log")
        try FileManager.default.createDirectory(at: env.folder, withIntermediateDirectories: true)
        let engine = try FakeEngine.make(translate: """
        echo "key ${SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY:-none}" >> '\(log.path)'
        echo '{"event": "translation", "language": "en", "sentences": [{"id": 1, "start": 1, "end": 2, "text": "Hello.", "words": []}]}'
        """)
        let (vm, _) = makeModel(env, engine: engine)
        await vm.waitForProviderKeys()
        // The item is changed outside the app, e.g. in Keychain Access.
        let id = try #require(vm.providerStore.providers.first?.id)
        env.secrets.set("sk-new", for: TranslationProviderStore.secretAccount(for: id))
        let reads = env.secrets.readsOnMain.count

        vm.language = "tr"
        vm.translationTarget = "en"
        vm.sentences = [Sentence(id: 1, start: 1, end: 2, text: "Merhaba.")]
        vm.translate()
        await settle(vm)

        #expect(vm.status == .finished)
        #expect((try String(contentsOf: log, encoding: .utf8)).trimmingCharacters(in: .whitespacesAndNewlines) == "key sk-new")
        #expect(env.secrets.readsOnMain.count > reads, "the key was read again when the job started")
        #expect(env.secrets.readsOnMain.dropFirst(reads).allSatisfy { !$0 })
    }

    @Test func aKeyAlreadyHeldSurvivesARefusedReadAtTranslationStart() async throws {
        let env = Env()
        savedProvider(env)
        let log = env.folder.appending(path: "keys.log")
        try FileManager.default.createDirectory(at: env.folder, withIntermediateDirectories: true)
        let engine = try FakeEngine.make(translate: """
        echo "key ${SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY:-none}" >> '\(log.path)'
        """)
        let (vm, _) = makeModel(env, engine: engine)
        await vm.waitForProviderKeys()
        env.secrets.unreadable = true
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.sentences = [Sentence(id: 1, start: 1, end: 2, text: "Merhaba.")]
        vm.translate()
        await settle(vm)
        // A transient refusal doesn't throw away the key the app already has.
        #expect(vm.status == .finished)
        #expect((try String(contentsOf: log, encoding: .utf8)).trimmingCharacters(in: .whitespacesAndNewlines) == "key sk-saved")
    }

    @Test func aKeyNeverReadStopsTheTranslationWhileTheKeychainRefuses() async throws {
        let env = Env()
        let provider = savedProvider(env)
        env.secrets.unreadable = true
        let marker = env.folder.appending(path: "called")
        try FileManager.default.createDirectory(at: env.folder, withIntermediateDirectories: true)
        let engine = try FakeEngine.make(translate: "touch '\(marker.path)'")
        let (vm, _) = makeModel(env, engine: engine)
        await vm.waitForProviderKeys()
        #expect(vm.providerKey(for: provider.id) == .unreadable)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.sentences = [Sentence(id: 1, start: 1, end: 2, text: "Merhaba.")]
        vm.translate()
        await settle(vm)
        guard case .failed(let message) = vm.status else { Issue.record("expected a failure"); return }
        #expect(message.text.contains("OpenAI"))
        #expect(!FileManager.default.fileExists(atPath: marker.path))
        // Once access is back the same action works.
        env.secrets.unreadable = false
        vm.translate()
        await settle(vm)
        #expect(vm.status == .finished)
        #expect(FileManager.default.fileExists(atPath: marker.path))
    }

    // MARK: - Editing a provider never deletes its key by itself

    @Test func editingWithoutTouchingTheKeyKeepsIt() async {
        let env = Env()
        let provider = savedProvider(env)
        let (vm, _) = makeModel(env)
        await vm.waitForProviderKeys()
        var draft = ProviderDraft(editing: provider, key: vm.providerKey(for: provider.id))
        #expect(draft.apiKey.isEmpty && draft.keyToSave == nil && draft.effectiveKey == "sk-saved")
        draft.model = "gpt-4.1"
        draft.name = "Work"
        #expect(vm.updateProvider(draft.profile, key: draft.keyToSave)?.keyStored == true)

        #expect(env.secrets.stored(TranslationProviderStore.secretAccount(for: provider.id)) == "sk-saved")
        #expect(env.secrets.removalCount == 0 && env.secrets.writeCount == 1, "only the first save wrote a key")
        #expect(vm.providerStore.providers[0].model == "gpt-4.1" && vm.providerStore.providers[0].name == "Work")
        #expect(vm.providerKey(for: provider.id) == .present("sk-saved"))
    }

    @Test func editingWithAnUnreadableKeyKeepsIt() async {
        let env = Env()
        let provider = savedProvider(env)
        let writes = env.secrets.writeCount
        env.secrets.unreadable = true
        let (vm, state) = makeModel(env)
        await vm.waitForProviderKeys()
        var draft = ProviderDraft(editing: provider, key: vm.providerKey(for: provider.id))
        #expect(draft.storedKey == .unreadable && draft.keyToSave == nil && draft.canRemoveStoredKey)
        draft.model = "gpt-4.1"
        #expect(vm.updateProvider(draft.profile, key: draft.keyToSave) != nil)

        env.secrets.unreadable = false
        #expect(env.secrets.stored(TranslationProviderStore.secretAccount(for: provider.id)) == "sk-saved")
        #expect(env.secrets.removalCount == 0 && env.secrets.writeCount == writes)
        #expect(await vm.providerKeyForUse(provider.id) == .present("sk-saved"))
        #expect(state.settings.externalLLM?.model == "gpt-4.1" && state.settings.externalLLM?.apiKey == "sk-saved")
    }

    @Test func aTypedKeyReplacesAndAnExplicitRemovalRemoves() async {
        let env = Env()
        let provider = savedProvider(env)
        let (vm, state) = makeModel(env)
        await vm.waitForProviderKeys()

        var draft = ProviderDraft(editing: provider, key: vm.providerKey(for: provider.id))
        draft.apiKey = "  sk-typed \n"
        #expect(draft.keyEdited && draft.keyToSave == "  sk-typed \n" && !draft.canRemoveStoredKey)
        vm.updateProvider(draft.profile, key: draft.keyToSave)
        #expect(env.secrets.stored(TranslationProviderStore.secretAccount(for: provider.id)) == "sk-typed")
        #expect(vm.providerKey(for: provider.id) == .present("sk-typed") && state.settings.externalLLM?.apiKey == "sk-typed")

        // "Kayıtlı anahtarı sil".
        var removing = ProviderDraft(editing: provider, key: vm.providerKey(for: provider.id))
        #expect(removing.canRemoveStoredKey && !removing.removesStoredKey)
        removing.removeStoredKey()
        #expect(removing.removesStoredKey && removing.keyToSave == "")
        vm.updateProvider(removing.profile, key: removing.keyToSave)
        #expect(env.secrets.stored(TranslationProviderStore.secretAccount(for: provider.id)) == nil)
        #expect(vm.providerKey(for: provider.id) == .absent && vm.providerKeyCaption(for: provider.id) == "Anahtar yok")
        #expect(state.settings.externalLLM?.apiKey == "")

        // Typing something and then clearing it is the same explicit decision.
        var cleared = ProviderDraft(editing: provider, key: .present("sk-x"))
        cleared.apiKey = "abc"
        cleared.apiKey = ""
        #expect(cleared.keyToSave == "" && cleared.removesStoredKey)
    }

    @Test func aNewProviderSavesWhatTheFieldHolds() {
        var draft = ProviderDraft(preset: .preset(id: "openai"))
        #expect(draft.keyToSave == "" && !draft.removesStoredKey && !draft.canRemoveStoredKey)
        draft.apiKey = "sk-1"
        #expect(draft.keyToSave == "sk-1" && draft.effectiveKey == "sk-1")
    }

    @Test func aTestOfAnUntouchedDraftSendsTheStoredKey() async {
        let env = Env()
        let provider = savedProvider(env)
        final class Seen: @unchecked Sendable {
            private let lock = NSLock()
            private var headers: [String?] = []
            func add(_ value: String?) { lock.withLock { headers.append(value) } }
            var all: [String?] { lock.withLock { headers } }
        }
        let seen = Seen()
        let (vm, _) = makeModel(env, fetch: { request in
            seen.add(request.value(forHTTPHeaderField: "Authorization"))
            return (Data(#"{"data": [{"id": "gpt-4o-mini"}]}"#.utf8), 200)
        })
        await vm.waitForProviderKeys()
        var draft = ProviderDraft(editing: provider, key: vm.providerKey(for: provider.id))
        _ = await vm.testConnection(of: draft)
        let first = seen.all
        #expect(!first.isEmpty && first.allSatisfy { $0 == "Bearer sk-saved" })
        draft.apiKey = "sk-other"
        _ = await vm.testConnection(of: draft)
        let second = seen.all.dropFirst(first.count)
        #expect(!second.isEmpty && second.allSatisfy { $0 == "Bearer sk-other" })
    }

    // MARK: - Connection tests and prefetching

    @Test func aConnectionTestWithAnUnreadableKeySaysSoInsteadOfTestingWithoutIt() async {
        let env = Env()
        let provider = savedProvider(env)
        env.secrets.unreadable = true
        final class Count: @unchecked Sendable {
            private let lock = NSLock()
            private var value = 0
            func bump() { lock.withLock { value += 1 } }
            var current: Int { lock.withLock { value } }
        }
        let requests = Count()
        let (vm, _) = makeModel(env, fetch: { _ in
            requests.bump()
            return (Data(), 200)
        })
        await vm.waitForProviderKeys()
        await vm.testConnection(of: provider.id)
        #expect(requests.current == 0)
        #expect(vm.providerNotices[provider.id]?.isError == true)
        #expect(vm.providerNotices[provider.id]?.message.text.contains("Anahtar Zinciri") == true)
        #expect(vm.testingProviders.isEmpty)
    }

    @Test func onlyAServerOnTheLocalNetworkIsContactedByItself() async {
        final class Count: @unchecked Sendable {
            private let lock = NSLock()
            private var value = 0
            func bump() { lock.withLock { value += 1 } }
            var current: Int { lock.withLock { value } }
        }
        let env = Env()
        let requests = Count()
        let (vm, _) = makeModel(env, fetch: { _ in
            requests.bump()
            return (Data(#"{"data": [{"id": "m1"}]}"#.utf8), 200)
        })
        // An active cloud provider with nothing known about it: the toolbar's prefetch leaves it alone.
        var cloud = openAI(model: "")
        cloud.discoveredModels = []
        vm.addProvider(cloud, key: "sk-1")
        await vm.refreshActiveProviderModelsIfNeeded()
        #expect(requests.current == 0 && vm.providerStore.providers[0].discoveredModels.isEmpty)
        // On request it is asked.
        await vm.testConnection(of: vm.providerStore.providers[0].id)
        #expect(requests.current == 1)

        // A LAN server is prefetched.
        vm.addProvider(TranslationProviderProfile(name: "Mac", presetID: "lan", baseURL: "10.0.0.2:11434"), key: "")
        await vm.refreshActiveProviderModelsIfNeeded()
        #expect(requests.current == 2)
    }

    // MARK: - Defaults of a new provider

    @Test func aNewDraftTranslatesInTheBackgroundOnlyOnTheLocalNetwork() {
        #expect(ProviderDraft(preset: .preset(id: "lan")).asyncTranslation)
        for id in ["openai", "opencode-go", "openrouter", "deepseek", "groq", "gemini", "custom"] {
            #expect(!ProviderDraft(preset: .preset(id: id)).asyncTranslation, "\(id)")
        }
        // Picking another type in the sheet follows, until the switch has been flipped by hand.
        var draft = ProviderDraft(preset: .preset(id: "openai"))
        draft.apply(preset: .preset(id: "lan"))
        #expect(draft.asyncTranslation)
        draft.apply(preset: .preset(id: "groq"))
        #expect(!draft.asyncTranslation)
        draft.asyncTranslation = true
        draft.apply(preset: .preset(id: "openai"))
        #expect(draft.asyncTranslation, "the user's own choice stays")
    }
}
