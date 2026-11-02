import EngineKit
import Foundation
import LibraryKit

/// What the add / edit sheet of a translation provider holds while it is open. Nothing reaches the store until the
/// user saves; the key is part of the draft only so it can be tested and saved together with the rest.
struct ProviderDraft: Equatable {
    /// The id the provider is saved under; every save from this draft goes to it.
    var id: UUID
    /// The provider is not in the store yet (the first save adds it, later ones update it).
    var isNew: Bool
    private(set) var presetID: String
    var name: String
    /// As typed. A different address drops `discoveredModels`: they were listed by the old one.
    var baseURL: String {
        didSet {
            if baseURL.trimmingCharacters(in: .whitespacesAndNewlines) != oldValue.trimmingCharacters(in: .whitespacesAndNewlines) {
                discoveredModels = []
            }
        }
    }
    var apiKey: String
    var model: String
    var asyncTranslation: Bool
    /// Models the last successful test of this address listed.
    var discoveredModels: [String]

    /// A new provider filled from `preset`: its name, address and first suggested model.
    init(preset: TranslationProviderPreset = TranslationProviderPreset.all[0]) {
        id = UUID()
        isNew = true
        presetID = preset.id
        name = preset.name
        baseURL = preset.baseURL
        apiKey = ""
        model = preset.defaultModel
        asyncTranslation = true
        discoveredModels = []
    }

    /// An existing provider with its stored key.
    init(editing profile: TranslationProviderProfile, key: String) {
        id = profile.id
        isNew = false
        presetID = profile.presetID
        name = profile.name
        baseURL = profile.baseURL
        apiKey = key
        model = profile.model
        asyncTranslation = profile.asyncTranslation
        discoveredModels = profile.discoveredModels
    }

    var preset: TranslationProviderPreset { .preset(id: presetID) }

    /// Picking another provider type refills the name, address and model with its defaults (and forgets the models of
    /// the old address); the key and the async choice stay.
    mutating func apply(preset: TranslationProviderPreset) {
        presetID = preset.id
        name = preset.name
        baseURL = preset.baseURL
        model = preset.defaultModel
        discoveredModels = []
    }

    /// The profile as the store keeps it, key left out.
    var profile: TranslationProviderProfile {
        TranslationProviderProfile(
            id: id, name: name, presetID: presetID, baseURL: baseURL, model: model,
            asyncTranslation: asyncTranslation, discoveredModels: discoveredModels
        )
    }

    /// What the model menu offers.
    var availableModels: [String] { profile.availableModels }

    /// The address can be used (the sheet won't save without one).
    var hasUsableAddress: Bool { ExternalLLMServer.normalizeBaseURL(baseURL) != nil }
}
