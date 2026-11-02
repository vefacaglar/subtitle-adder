import EngineKit
import Foundation
import Testing
@testable import LibraryKit

/// A providers file in a temporary folder with an in-memory key store; `store()` opens it again, as a relaunch would.
@MainActor
private final class Fixture {
    let folder = FileManager.default.temporaryDirectory.appending(path: "providers-\(UUID().uuidString)")
    let secrets = InMemorySecretStore()
    var fileURL: URL { folder.appending(path: TranslationProviderStore.fileName) }

    func store() -> TranslationProviderStore {
        TranslationProviderStore(fileURL: fileURL, secrets: secrets)
    }

    func fileText() throws -> String {
        try String(contentsOf: fileURL, encoding: .utf8)
    }

    func fileJSON() throws -> [String: Any] {
        let object = try JSONSerialization.jsonObject(with: Data(contentsOf: fileURL))
        return try #require(object as? [String: Any])
    }

    deinit { try? FileManager.default.removeItem(at: folder) }
}

private func profile(_ presetID: String, model: String? = nil, url: String? = nil, asyncTranslation: Bool = true) -> TranslationProviderProfile {
    var profile = TranslationProviderProfile(preset: .preset(id: presetID))
    if let model { profile.model = model }
    if let url { profile.baseURL = url }
    profile.asyncTranslation = asyncTranslation
    return profile
}

@MainActor
@Suite struct TranslationProviderStoreTests {
    // MARK: Persistence

    @Test func startsEmptyWithoutAFile() {
        let fixture = Fixture()
        let store = fixture.store()
        #expect(store.providers.isEmpty)
        #expect(store.activeProviderID == nil)
        #expect(store.activeProvider == nil)
        #expect(store.activeExternalLLM == nil)
        #expect(!FileManager.default.fileExists(atPath: fixture.fileURL.path))
    }

    @Test func roundTripsThroughTheFile() {
        let fixture = Fixture()
        let store = fixture.store()
        var openAI = profile("openai", model: "gpt-4.1-mini", asyncTranslation: false)
        openAI.discoveredModels = ["gpt-4.1-mini", "o3"]
        let lan = profile("lan", url: "http://192.168.1.50:11434")
        store.add(openAI, key: "sk-1234567890", setActive: false)
        store.add(lan, key: "")
        #expect(store.activeProviderID == lan.id)

        let reopened = fixture.store()
        #expect(reopened.providers == [openAI, lan])
        #expect(reopened.activeProviderID == lan.id)
        #expect(reopened.activeProvider?.presetID == "lan")
        #expect(reopened.apiKey(for: openAI.id) == "sk-1234567890")
        #expect(reopened.providers[0].asyncTranslation == false)
        #expect(reopened.providers[0].discoveredModels == ["gpt-4.1-mini", "o3"])
    }

    @Test func theFileHasTheWindowsShapeWithoutTheKey() throws {
        let fixture = Fixture()
        let store = fixture.store()
        let stored = store.add(profile("groq", model: "gemma2-9b-it"), key: "gsk-super-secret-9999", setActive: false)

        var json = try fixture.fileJSON()
        #expect(Set(json.keys) == ["activeProviderId", "providers"])
        #expect(json["activeProviderId"] is NSNull)
        let entry = try #require((json["providers"] as? [[String: Any]])?.first)
        #expect(Set(entry.keys) == ["id", "name", "presetId", "baseUrl", "model", "asyncTranslation", "discoveredModels"])
        #expect(entry["id"] as? String == stored.id.uuidString)
        #expect(entry["presetId"] as? String == "groq")
        #expect(entry["baseUrl"] as? String == "https://api.groq.com/openai/v1")
        #expect(entry["asyncTranslation"] as? Bool == true)

        store.setActive(stored.id)
        json = try fixture.fileJSON()
        #expect(json["activeProviderId"] as? String == stored.id.uuidString)
        // Slashes stay readable and the key is nowhere in the file.
        let text = try fixture.fileText()
        #expect(text.contains("https://api.groq.com/openai/v1"))
        #expect(!text.contains("gsk-super-secret"))
        #expect(!text.contains("apiKey"))
    }

    @Test func theKeyLivesOnlyInTheSecretStore() {
        let fixture = Fixture()
        let store = fixture.store()
        let stored = store.add(profile("openai"), key: "  sk-abc  ")
        #expect(fixture.secrets.accounts == ["translation-provider.\(stored.id.uuidString)"])
        #expect(fixture.secrets.string(for: TranslationProviderStore.secretAccount(for: stored.id)) == "sk-abc")
        #expect(store.apiKey(for: stored.id) == "sk-abc")
        // No key, no item.
        store.add(profile("lan"), key: "   ")
        #expect(fixture.secrets.accounts.count == 1)
    }

    @Test func readsAWindowsFileAndNeverWritesItsPlainTextKeyBack() throws {
        let fixture = Fixture()
        try FileManager.default.createDirectory(at: fixture.folder, withIntermediateDirectories: true)
        let id = "e621e1f8-c36c-495a-93fc-0c247a3e6e5f"
        let windows = """
        {"activeProviderId": "\(id)", "providers": [
          {"id": "\(id)", "name": "OpenAI", "apiKey": "sk-leaked", "baseUrl": "https://api.openai.com/v1"},
          {"id": "\(UUID().uuidString.lowercased())", "name": "x", "presetId": "lan", "baseUrl": "h", "model": "m",
           "asyncTranslation": false, "discoveredModels": ["a"]}
        ]}
        """
        try windows.write(to: fixture.fileURL, atomically: true, encoding: .utf8)
        let store = fixture.store()
        #expect(store.providers.count == 2)
        #expect(store.activeProviderID == UUID(uuidString: id))
        let first = store.providers[0]
        #expect(first.presetID == "custom")
        #expect(first.model.isEmpty)
        #expect(first.asyncTranslation)
        #expect(first.discoveredModels.isEmpty)
        #expect(store.providers[1].asyncTranslation == false)
        #expect(store.apiKey(for: first.id).isEmpty)

        store.updateModel("gpt-4o-mini", for: first.id)
        let rewritten = try fixture.fileText()
        #expect(!rewritten.contains("sk-leaked"))
    }

    @Test func anUnknownActiveIdOrABrokenFileStartsClean() throws {
        let fixture = Fixture()
        try FileManager.default.createDirectory(at: fixture.folder, withIntermediateDirectories: true)
        try #"{"activeProviderId": "e621e1f8-c36c-495a-93fc-0c247a3e6e5f", "providers": []}"#
            .write(to: fixture.fileURL, atomically: true, encoding: .utf8)
        #expect(fixture.store().activeProviderID == nil)
        try "not json".write(to: fixture.fileURL, atomically: true, encoding: .utf8)
        let broken = fixture.store()
        #expect(broken.providers.isEmpty && broken.activeProviderID == nil)
    }

    // MARK: Editing

    @Test func addTrimsAndNamesAfterThePresetWhenBlank() {
        let fixture = Fixture()
        let store = fixture.store()
        let stored = store.add(
            TranslationProviderProfile(name: "  ", presetID: "deepseek", baseURL: " https://api.deepseek.com/v1 ", model: " deepseek-chat "),
            key: "k", setActive: false
        )
        #expect(stored.name == "DeepSeek")
        #expect(stored.baseURL == "https://api.deepseek.com/v1")
        #expect(stored.model == "deepseek-chat")
        #expect(store.providers == [stored])
        #expect(store.activeProviderID == nil)
        // An id that is already there is replaced, not listed twice.
        var again = stored
        again.name = "Mine"
        store.add(again, key: "k2")
        #expect(store.providers.map(\.name) == ["Mine"])
        #expect(store.apiKey(for: stored.id) == "k2")
        #expect(store.activeProviderID == stored.id)
    }

    @Test func updateKeepsReplacesOrRemovesTheKey() {
        let fixture = Fixture()
        let store = fixture.store()
        var stored = store.add(profile("openai"), key: "sk-old")
        stored.model = "gpt-4.1-nano"
        stored.asyncTranslation = false
        #expect(store.update(stored))
        #expect(store.apiKey(for: stored.id) == "sk-old")
        #expect(store.providers[0].model == "gpt-4.1-nano" && !store.providers[0].asyncTranslation)

        #expect(store.update(stored, key: "sk-new"))
        #expect(store.apiKey(for: stored.id) == "sk-new")
        #expect(store.update(stored, key: "  "))
        #expect(store.apiKey(for: stored.id).isEmpty)
        #expect(fixture.secrets.accounts.isEmpty)
        #expect(fixture.store().providers[0].model == "gpt-4.1-nano")

        #expect(!store.update(profile("groq"), key: "x"))
        #expect(store.providers.count == 1 && fixture.secrets.accounts.isEmpty)
    }

    @Test func setKeyReplacesTheKeyOfAnExistingProvider() {
        let fixture = Fixture()
        let store = fixture.store()
        let stored = store.add(profile("openai"), key: "")
        #expect(store.setKey("sk-late", for: stored.id))
        #expect(store.apiKey(for: stored.id) == "sk-late")
        #expect(!store.setKey("sk-orphan", for: UUID()))
        #expect(fixture.secrets.accounts.count == 1)
    }

    @Test func removingDeletesTheKeyAndClearsTheActiveProvider() {
        let fixture = Fixture()
        let store = fixture.store()
        let first = store.add(profile("openai"), key: "sk-1", setActive: false)
        let second = store.add(profile("deepseek"), key: "sk-2")
        #expect(store.activeProviderID == second.id)

        store.remove(first.id)
        #expect(store.providers == [second])
        #expect(store.activeProviderID == second.id)
        #expect(fixture.secrets.accounts == [TranslationProviderStore.secretAccount(for: second.id)])

        store.remove(second.id)
        #expect(store.providers.isEmpty)
        #expect(store.activeProviderID == nil)
        #expect(store.activeExternalLLM == nil)
        #expect(fixture.secrets.accounts.isEmpty)
        let reopened = fixture.store()
        #expect(reopened.providers.isEmpty && reopened.activeProviderID == nil)
    }

    @Test func settingTheActiveProvider() {
        let fixture = Fixture()
        let store = fixture.store()
        let first = store.add(profile("openai"), key: "", setActive: false)
        let second = store.add(profile("lan"), key: "", setActive: false)
        #expect(store.activeProviderID == nil)
        store.setActive(first.id)
        #expect(fixture.store().activeProviderID == first.id)
        store.setActive(second.id)
        #expect(store.activeProvider?.id == second.id)
        store.setActive(UUID())
        #expect(store.activeProviderID == nil)
        store.setActive(first.id)
        store.setActive(nil)
        #expect(fixture.store().activeProviderID == nil)
    }

    @Test func updatingTheModelOnly() {
        let fixture = Fixture()
        let store = fixture.store()
        let stored = store.add(profile("openai"), key: "")
        store.updateModel("  gpt-4.1-mini ", for: stored.id)
        #expect(fixture.store().providers[0].model == "gpt-4.1-mini")
        store.updateModel("x", for: UUID())
        #expect(store.providers[0].model == "gpt-4.1-mini")
    }

    @Test func discoveredModelsAreCleanedAndFillAnEmptyModel() {
        let fixture = Fixture()
        let store = fixture.store()
        let blank = store.add(profile("custom", model: "", url: "https://x.test/v1"), key: "")
        store.setDiscoveredModels([" a ", "b", "A", "", "c"], for: blank.id)
        #expect(store.providers[0].discoveredModels == ["a", "b", "c"])
        #expect(store.providers[0].model == "a")

        // A model the user chose stays.
        store.updateModel("mine", for: blank.id)
        store.setDiscoveredModels(["z"], for: blank.id)
        #expect(store.providers[0].model == "mine")
        #expect(fixture.store().providers[0].discoveredModels == ["z"])
        store.setDiscoveredModels(["q"], for: UUID())
        #expect(store.providers[0].discoveredModels == ["z"])
    }

    // MARK: Key mask

    @Test func maskRule() {
        let mask = TranslationProviderStore.mask
        #expect(mask("") == nil)
        #expect(mask("   ") == nil)
        #expect(mask("abcd") == "••••")
        #expect(mask("a") == "••••")
        #expect(mask("abcde") == "••••bcde")
        #expect(mask("12345678") == "••••5678")
        #expect(mask("123456789") == "123••••6789")
        #expect(mask("sk-1234567890") == "sk-••••7890")
        #expect(mask("  sk-1234567890\n") == "sk-••••7890")
    }

    @Test func maskedKeyReadsTheStoredKey() {
        let fixture = Fixture()
        let store = fixture.store()
        let withKey = store.add(profile("openai"), key: "sk-1234567890", setActive: false)
        let without = store.add(profile("lan"), key: "")
        #expect(store.maskedKey(for: withKey.id) == "sk-••••7890")
        #expect(store.maskedKey(for: without.id) == nil)
        #expect(store.maskedKey(for: UUID()) == nil)
    }

    // MARK: Models

    @Test func availableModelsListTheChosenOneThenDiscoveredThenSuggested() {
        var openAI = profile("openai", model: "gpt-4.1-nano")
        openAI.discoveredModels = ["gpt-4o-mini", "o3"]
        #expect(openAI.availableModels == ["gpt-4.1-nano", "gpt-4o-mini", "o3", "gpt-4.1-mini"])

        var shouting = profile("openai", model: "GPT-4O-MINI")
        shouting.discoveredModels = ["gpt-4o-mini"]
        #expect(shouting.availableModels == ["GPT-4O-MINI", "gpt-4.1-mini", "gpt-4.1-nano"])

        #expect(profile("openai", model: "").availableModels == ["gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano"])
        #expect(profile("custom", model: "").availableModels.isEmpty)
        #expect(profile("custom", model: " mine ").availableModels == ["mine"])
    }

    @Test func aLanServerThatReportedItsModelsDropsTheSuggestions() {
        #expect(profile("lan", model: "").availableModels == ["gemma4:e4b", "qwen3.5:4b"])
        var lan = profile("lan", model: "")
        lan.discoveredModels = ["qwen3:14b", "llama3.2"]
        #expect(lan.availableModels == ["qwen3:14b", "llama3.2"])
        lan.model = "llama3.2"
        #expect(lan.availableModels == ["llama3.2", "qwen3:14b"])
    }

    @Test func availableModelsByIdAndAnUnknownPresetIsCustom() {
        let fixture = Fixture()
        let store = fixture.store()
        let stored = store.add(profile("gemini"), key: "")
        #expect(store.availableModels(for: stored.id) == ["gemini-2.5-flash", "gemini-2.5-pro"])
        #expect(store.availableModels(for: UUID()).isEmpty)
        let odd = TranslationProviderProfile(presetID: "no-such-preset", model: "m")
        #expect(odd.preset == .custom)
        #expect(odd.availableModels == ["m"])
    }

    // MARK: Engine mapping

    @Test func theActiveProviderBecomesTheExternalLLM() {
        let fixture = Fixture()
        let store = fixture.store()
        let openAI = store.add(
            profile("openai", model: " gpt-4.1-mini ", asyncTranslation: false), key: " sk-live \n", setActive: false
        )
        let lan = store.add(profile("lan", model: "gemma4:e4b", url: "10.0.0.2:11434"), key: "", setActive: false)
        #expect(store.activeExternalLLM == nil)

        store.setActive(openAI.id)
        #expect(store.activeExternalLLM == ActiveExternalLLM(
            llm: ExternalLLM(url: "https://api.openai.com/v1", model: "gpt-4.1-mini", apiKey: "sk-live"), asyncTranslation: false
        ))

        store.setActive(lan.id)
        #expect(store.activeExternalLLM == ActiveExternalLLM(
            llm: ExternalLLM(url: "10.0.0.2:11434", model: "gemma4:e4b", apiKey: ""), asyncTranslation: true
        ))

        store.setActive(nil)
        #expect(store.activeExternalLLM == nil)
    }

    @Test func aBlankAddressIsNoExternalServer() {
        let fixture = Fixture()
        let store = fixture.store()
        store.add(profile("custom", url: "  "), key: "sk")
        #expect(store.activeProvider != nil)
        #expect(store.activeExternalLLM == nil)
        let stored = store.providers[0]
        var unusable = stored
        unusable.baseURL = "http://"
        store.update(unusable)
        #expect(store.activeExternalLLM == nil)
    }

    // MARK: Migration

    private func legacyDefaults(
        url: String?, model: String? = nil, enabled: Bool = false, asyncTranslation: Bool = false
    ) -> UserDefaults {
        let defaults = UserDefaults(suiteName: "providers-test-\(UUID().uuidString)")!
        if let url { defaults.set(url, forKey: "externalLlmUrl") }
        if let model { defaults.set(model, forKey: "externalLlmModel") }
        defaults.set(enabled, forKey: "externalLlmEnabled")
        defaults.set(asyncTranslation, forKey: "externalLlmAsync")
        return defaults
    }

    private let lanNames = [TranslationProviderPreset.preset(id: "lan").name(language: .tr), TranslationProviderPreset.preset(id: "lan").name(language: .en)]

    @Test func anEnabledLegacyServerBecomesTheActiveLanProvider() throws {
        let fixture = Fixture()
        let store = fixture.store()
        let defaults = legacyDefaults(url: " 192.168.1.50:11434 ", model: " gemma4:e4b ", enabled: true, asyncTranslation: true)
        #expect(store.migrateLegacyIfNeeded(defaults: defaults))

        let provider = try #require(store.providers.first)
        #expect(store.providers.count == 1)
        #expect(provider.presetID == "lan")
        #expect(lanNames.contains(provider.name))
        #expect(provider.baseURL == "192.168.1.50:11434")
        #expect(provider.model == "gemma4:e4b")
        #expect(provider.asyncTranslation)
        #expect(store.activeProviderID == provider.id)
        #expect(fixture.secrets.accounts.isEmpty)
        #expect(FileManager.default.fileExists(atPath: fixture.fileURL.path))
        #expect(defaults.bool(forKey: TranslationProviderStore.migrationDefaultsKey))
        // The old settings stay for the old Settings screen.
        #expect(defaults.string(forKey: "externalLlmUrl") == " 192.168.1.50:11434 ")
        #expect(fixture.store().activeProviderID == provider.id)
    }

    @Test func aDisabledLegacyServerIsKeptButNotActive() throws {
        let fixture = Fixture()
        let store = fixture.store()
        #expect(store.migrateLegacyIfNeeded(defaults: legacyDefaults(url: "mac.local:1234", enabled: false)))
        let provider = try #require(store.providers.first)
        #expect(provider.baseURL == "mac.local:1234")
        #expect(provider.model.isEmpty)
        #expect(!provider.asyncTranslation)
        #expect(store.activeProviderID == nil)
        #expect(store.activeExternalLLM == nil)
    }

    @Test func aBlankLegacyAddressMigratesNothingAndWritesNoFile() {
        let fixture = Fixture()
        for url in [nil, "", "   "] {
            let defaults = legacyDefaults(url: url, enabled: true)
            let store = fixture.store()
            #expect(!store.migrateLegacyIfNeeded(defaults: defaults))
            #expect(store.providers.isEmpty)
            #expect(defaults.bool(forKey: TranslationProviderStore.migrationDefaultsKey))
        }
        #expect(!FileManager.default.fileExists(atPath: fixture.fileURL.path))
    }

    @Test func anExistingProvidersFileIsNeverOverwrittenByTheMigration() {
        let fixture = Fixture()
        let existing = fixture.store().add(profile("openai"), key: "", setActive: false)
        let store = fixture.store()
        #expect(!store.migrateLegacyIfNeeded(defaults: legacyDefaults(url: "mac.local:1234", enabled: true)))
        #expect(store.providers == [existing])
        #expect(store.activeProviderID == nil)
    }

    @Test func theMigrationRunsOnlyOnce() {
        let fixture = Fixture()
        let defaults = legacyDefaults(url: "mac.local:1234", enabled: true)
        #expect(fixture.store().migrateLegacyIfNeeded(defaults: defaults))
        // Even with the file gone (the user cleared the app's data) the old settings are not imported again.
        try? FileManager.default.removeItem(at: fixture.fileURL)
        let again = fixture.store()
        #expect(!again.migrateLegacyIfNeeded(defaults: defaults))
        #expect(again.providers.isEmpty)

        let flagged = legacyDefaults(url: "mac.local:1234", enabled: true)
        flagged.set(true, forKey: TranslationProviderStore.migrationDefaultsKey)
        #expect(!fixture.store().migrateLegacyIfNeeded(defaults: flagged))
    }
}
