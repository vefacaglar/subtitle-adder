import EngineKit
import Foundation
import LibraryKit

/// What the connection test says, with the styling it needs (success or error).
struct ExternalConnectionNotice: Equatable, Sendable {
    var message: LocalizedMessage
    var isError: Bool
}

/// A provider as it was just saved, and whether its API key made it into the keychain.
struct ProviderSaveResult {
    var profile: TranslationProviderProfile
    /// `false` when a non-blank key could not be stored: the provider is saved, but without that key.
    var keyStored: Bool
}

/// What a connection test of an unsaved provider (the sheet's draft) found. Nothing is stored for it.
struct ProviderDraftTest: Sendable {
    /// `nil` when the test was cancelled (the sheet went away): the server wasn't heard from.
    var notice: ExternalConnectionNotice?
    /// Models the server listed; empty unless it answered.
    var models: [String]
}

/// Translation providers: which one is active, editing the list, connection tests and what the toolbar menu does.
/// The `TranslationProviderStore` holds the data; every change goes through here so the engine launcher is told.
extension SubtitleViewModel {
    // MARK: - Active provider

    /// The active provider when the engine can use it: one is active and its address is usable. `nil` translates with a
    /// local model.
    var activeExternalProvider: TranslationProviderProfile? {
        guard let provider = providerStore.activeProvider, ExternalLLMServer.normalizeBaseURL(provider.baseURL) != nil else {
            return nil
        }
        return provider
    }

    /// Translation goes to the active provider in the background while transcription is still running.
    var isExternalAsyncActive: Bool { activeExternalProvider?.asyncTranslation ?? false }

    /// Whether the text to translate leaves the local network for `provider`'s server (a cloud service). Audio never
    /// leaves in any case.
    func sendsTextOverInternet(_ provider: TranslationProviderProfile) -> Bool {
        !ExternalLLM(url: provider.baseURL).isOnLocalNetwork
    }

    /// Name of the active provider when it is a cloud service; the privacy texts say that the text is sent to it.
    var activeCloudProviderName: String? {
        guard let provider = activeExternalProvider, sendsTextOverInternet(provider) else { return nil }
        return provider.name
    }

    /// What the toolbar's translation-model menu shows: the local model's name, "<provider>: <model>" for the active
    /// provider, or "Harici: <model>" for a LAN server.
    var translationModelTitle: String {
        guard let provider = activeExternalProvider else { return ModelLabel.name(for: translationModel) }
        let model = provider.model.trimmingCharacters(in: .whitespacesAndNewlines)
        if provider.presetID == "lan" {
            let shown = model.isEmpty ? L("Harici LLM") : model
            return provider.asyncTranslation ? L("Harici: %@ (Asenkron)", shown) : L("Harici: %@", shown)
        }
        return "\(provider.name): \(model.isEmpty ? L("Otomatik") : model)"
    }

    /// Changes whenever the toolbar's remote model list may have to be fetched again: another provider or address.
    var providerPrefetchKey: String {
        "\(providerStore.activeProviderID?.uuidString ?? "-")|\(activeExternalProvider?.baseURL ?? "")"
    }

    // MARK: - Editing

    /// Adds a provider with its key and, unless told otherwise, makes it the active one.
    @discardableResult
    func addProvider(_ profile: TranslationProviderProfile, key: String, activate: Bool = true) -> ProviderSaveResult {
        let stored = providerStore.add(profile, key: key, setActive: activate)
        publishLaunchSettings()
        return ProviderSaveResult(profile: stored, keyStored: keyWasStored(key, for: stored.id))
    }

    /// Replaces a provider's settings. `key` `nil` keeps the stored key, a string replaces it (blank removes it).
    /// Activation is left as it is. `nil` for an unknown provider.
    @discardableResult
    func updateProvider(_ profile: TranslationProviderProfile, key: String? = nil) -> ProviderSaveResult? {
        guard providerStore.update(profile, key: key) else { return nil }
        // The last test, and one still running, were about the old settings.
        forgetTests(of: profile.id)
        publishLaunchSettings()
        let stored = providerStore.providers.first { $0.id == profile.id } ?? profile
        return ProviderSaveResult(profile: stored, keyStored: key.map { keyWasStored($0, for: profile.id) } ?? true)
    }

    /// Removes a provider and its key; when it was the active one, translation goes back to the local models.
    func removeProvider(_ id: UUID) {
        providerStore.remove(id)
        forgetTests(of: id)
        publishLaunchSettings()
    }

    /// Makes a provider the active one, or `nil` the local models.
    func activateProvider(_ id: UUID?) {
        providerStore.setActive(id)
        publishLaunchSettings()
    }

    /// Settings: the model typed or picked in a provider's row. Does not change which provider is active.
    func setProviderModel(_ model: String, for id: UUID) {
        providerStore.updateModel(model, for: id)
        publishLaunchSettings()
    }

    /// Toolbar: translate with this model of this provider from now on.
    func chooseProviderModel(_ model: String, for id: UUID) {
        providerStore.updateModel(model, for: id)
        providerStore.setActive(id)
        publishLaunchSettings()
    }

    /// Toolbar: translate with one of the local models (the provider, if any, is switched off).
    func chooseLocalTranslationModel(_ modelID: String) {
        providerStore.setActive(nil)
        translationModel = modelID
        publishLaunchSettings()
    }

    /// The keychain accepted `key` for the provider (a blank key is nothing to store).
    private func keyWasStored(_ key: String, for id: UUID) -> Bool {
        let wanted = key.trimmingCharacters(in: .whitespacesAndNewlines)
        return wanted.isEmpty || providerStore.apiKey(for: id) == wanted
    }

    // MARK: - Connection tests

    /// Tests a saved provider with its stored key and shows the result under its row. Only a successful answer stores
    /// the models it listed (a failure says nothing about them); a provider without a chosen model takes the first.
    /// Also what "Uzaktaki modelleri listele" runs. A cancelled test changes nothing.
    func testConnection(of id: UUID) async {
        guard let provider = providerStore.providers.first(where: { $0.id == id }) else { return }
        let address = provider.baseURL
        let key = providerStore.apiKey(for: id)
        let testID = UUID()
        providerTestIDs[id] = testID
        testingProviders.insert(id)
        let result = await ExternalLLMServer.testConnection(
            rawURL: address, apiKey: key, model: provider.model, presetID: provider.presetID, fetch: externalFetch
        )
        // A newer test, an edit or a removal made this answer obsolete.
        guard providerTestIDs[id] == testID else { return }
        providerTestIDs[id] = nil
        testingProviders.remove(id)
        guard let now = providerStore.providers.first(where: { $0.id == id }), now.baseURL == address,
              providerStore.apiKey(for: id) == key
        else { return }
        // A cancelled test (the window that asked went away) didn't hear from the server: the earlier notice and list stand.
        guard let notice = Self.notice(for: result) else { return }
        providerNotices[id] = notice
        if case .reachable(_, _, let models) = result, !models.isEmpty {
            providerStore.setDiscoveredModels(models, for: id)
            publishLaunchSettings()
        }
    }

    /// Tests the settings in the sheet before they are saved.
    func testConnection(of draft: ProviderDraft) async -> ProviderDraftTest {
        let result = await ExternalLLMServer.testConnection(
            rawURL: draft.baseURL, apiKey: draft.apiKey, model: draft.model, presetID: draft.presetID, fetch: externalFetch
        )
        return ProviderDraftTest(notice: Self.notice(for: result), models: result.models)
    }

    /// Fills the list of the active provider's models when nothing is known about it yet; the toolbar menu shows what
    /// is there when opened and can't fetch by itself, so this runs ahead of time (launch, switching provider).
    func refreshActiveProviderModelsIfNeeded() async {
        guard let provider = activeExternalProvider, provider.discoveredModels.isEmpty,
              !testingProviders.contains(provider.id)
        else { return }
        await testConnection(of: provider.id)
    }

    /// Drops the result of the last test of a provider and invalidates one still running.
    private func forgetTests(of id: UUID) {
        providerTestIDs[id] = nil
        testingProviders.remove(id)
        providerNotices[id] = nil
    }

    /// `nil` for a cancelled test, which says nothing.
    private static func notice(for result: ExternalLLMServer.ConnectionResult) -> ExternalConnectionNotice? {
        switch result {
        case .cancelled:
            nil
        case .invalidAddress:
            ExternalConnectionNotice(message: .key("Lütfen geçerli bir sunucu adresi girin."), isError: true)
        case .unauthorized(let status):
            ExternalConnectionNotice(
                message: LocalizedMessage("external-unauthorized:\(status)") {
                    ExternalLLMServer.unauthorizedMessage(status: status)
                },
                isError: true
            )
        case .unreachable(let reason):
            ExternalConnectionNotice(
                message: LocalizedMessage("external-unreachable:\(reason)") { L("Sunucuya ulaşılamadı: %@", reason) },
                isError: true
            )
        case .reachable(let host, let latency, let models):
            ExternalConnectionNotice(
                message: LocalizedMessage("external-ok:\(host):\(latency):\(models.count)") {
                    L("Bağlantı başarılı (%@ · %lld ms · %lld uygun model bulundu)", host, latency, models.count)
                },
                isError: false
            )
        }
    }

    // MARK: - Store

    /// A providers store that lives only as long as the model: a temporary file and keys in memory.
    static func makeEphemeralProviderStore() -> TranslationProviderStore {
        TranslationProviderStore(
            fileURL: FileManager.default.temporaryDirectory.appending(path: "providers-\(UUID().uuidString)/\(TranslationProviderStore.fileName)"),
            secrets: InMemorySecretStore()
        )
    }
}
