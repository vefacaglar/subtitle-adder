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
    /// What the key field holds: what the user typed. An existing provider's field starts empty (the stored key is not
    /// copied into it), so `keyToSave` is `nil` until something is typed.
    var apiKey: String {
        didSet { if apiKey != oldValue { keyEdited = true } }
    }
    /// The key field was typed in or cleared. Until then saving leaves the stored key alone.
    private(set) var keyEdited = false
    /// What the secret store said about the provider's key when the sheet opened (or was last refreshed); `.absent` for a
    /// new provider. Only the app's key cache fills it in; the draft never reads the secret store.
    private(set) var storedKey: ProviderKey
    var model: String
    var asyncTranslation: Bool {
        didSet { if asyncTranslation != oldValue { asyncTranslationChosen = true } }
    }
    /// The user flipped the switch, so picking another provider type no longer resets it to that type's default.
    private var asyncTranslationChosen = false
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
        storedKey = .absent
        model = preset.defaultModel
        asyncTranslation = preset.translatesInBackgroundByDefault
        discoveredModels = []
    }

    /// An existing provider. `key` is what the key cache knows about its stored key: the field stays empty, and the
    /// stored key is replaced or removed only if the user types or clears something.
    init(editing profile: TranslationProviderProfile, key: ProviderKey) {
        id = profile.id
        isNew = false
        presetID = profile.presetID
        name = profile.name
        baseURL = profile.baseURL
        apiKey = ""
        storedKey = key
        model = profile.model
        asyncTranslation = profile.asyncTranslation
        discoveredModels = profile.discoveredModels
    }

    var preset: TranslationProviderPreset { .preset(id: presetID) }

    /// Updates what is known about the stored key (the cache finished reading it after the sheet opened).
    mutating func setStoredKey(_ key: ProviderKey) {
        storedKey = key
    }

    /// The provider has a stored key (or may have one: it could not be read) that the field has not replaced yet, so
    /// the sheet offers to remove it.
    var canRemoveStoredKey: Bool {
        guard !isNew, !keyEdited else { return false }
        switch storedKey {
        case .present, .unreadable: return true
        case .loading, .absent: return false
        }
    }

    /// Saving will remove the stored key: the user asked to (or cleared what they had typed).
    var removesStoredKey: Bool {
        !isNew && keyEdited && apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && storedKey != .absent
    }

    /// The user's explicit "delete the stored key"; the only way, next to typing a new one, to change it.
    mutating func removeStoredKey() {
        apiKey = ""
        keyEdited = true
    }

    /// What to pass to the store when saving: `nil` keeps the stored key (the field was never touched), a string
    /// replaces it, and a blank one removes it. A new provider always saves what the field holds.
    var keyToSave: String? {
        isNew || keyEdited ? apiKey : nil
    }

    /// The key a connection test sends: the typed one, else the stored one (empty when none could be read).
    var effectiveKey: String {
        if isNew || keyEdited { return apiKey }
        if case .present(let key) = storedKey { return key }
        return ""
    }

    /// Picking another provider type refills the name, address and model with its defaults (and forgets the models of
    /// the old address); the key stays. A background-translation switch the user did not touch follows the new type's
    /// default (on for the local network only).
    mutating func apply(preset: TranslationProviderPreset) {
        presetID = preset.id
        name = preset.name
        baseURL = preset.baseURL
        model = preset.defaultModel
        discoveredModels = []
        if isNew, !asyncTranslationChosen {
            asyncTranslation = preset.translatesInBackgroundByDefault
            asyncTranslationChosen = false
        }
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
