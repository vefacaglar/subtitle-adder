import EngineKit
import Foundation
import Observation

/// A saved translation provider (a cloud API or a server on the local network). The API key is deliberately not a
/// field: it lives in the `SecretStore` under `TranslationProviderStore.secretAccount(for:)`.
///
/// The JSON shape is the Windows app's (`translation-providers.json`) minus the key: camelCase, `presetId` and
/// `baseUrl` spelled that way on disk.
public struct TranslationProviderProfile: Sendable, Equatable, Identifiable, Codable {
    public var id: UUID
    /// Name shown in lists; falls back to the preset's name when saved blank.
    public var name: String
    /// `TranslationProviderPreset.id`; `"custom"` for an address the user typed.
    public var presetID: String
    public var baseURL: String
    /// Model sent to the provider; empty lets the engine take the first one the server lists.
    public var model: String
    /// Translate in the background while transcription is still running.
    public var asyncTranslation: Bool
    /// Models the provider reported on its last successful connection test.
    public var discoveredModels: [String]

    public init(
        id: UUID = UUID(), name: String = "", presetID: String = TranslationProviderPreset.customID,
        baseURL: String = "", model: String = "", asyncTranslation: Bool = true, discoveredModels: [String] = []
    ) {
        self.id = id
        self.name = name
        self.presetID = presetID
        self.baseURL = baseURL
        self.model = model
        self.asyncTranslation = asyncTranslation
        self.discoveredModels = discoveredModels
    }

    /// A new profile filled from a preset: its name, address and first suggested model. Background translation is on
    /// for a server on the local network only (`TranslationProviderPreset.translatesInBackgroundByDefault`).
    public init(preset: TranslationProviderPreset) {
        self.init(
            name: preset.name, presetID: preset.id, baseURL: preset.baseURL, model: preset.defaultModel,
            asyncTranslation: preset.translatesInBackgroundByDefault
        )
    }

    public var preset: TranslationProviderPreset { .preset(id: presetID) }

    /// What the model picker offers: the chosen model first, then the ones the provider reported, then the preset's
    /// suggestions (not for a LAN server that reported its own, whose suggestions are likely not installed), without
    /// duplicates (case-insensitive).
    public var availableModels: [String] {
        var result: [String] = []
        var seen = Set<String>()
        func add(_ model: String) {
            let trimmed = model.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty, seen.insert(trimmed.lowercased()).inserted { result.append(trimmed) }
        }
        add(model)
        discoveredModels.forEach(add)
        let preset = self.preset
        if discoveredModels.isEmpty || preset.id != "lan" { preset.suggestedModels.forEach(add) }
        return result
    }

    private enum CodingKeys: String, CodingKey {
        case id, name, model, asyncTranslation, discoveredModels
        case presetID = "presetId"
        case baseURL = "baseUrl"
    }

    /// Lenient like the Windows reader: every field but the id has a default, unknown keys (the Windows file also
    /// carries a plain-text `apiKey`) are ignored and never written back.
    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(UUID.self, forKey: .id) ?? UUID()
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        presetID = try container.decodeIfPresent(String.self, forKey: .presetID) ?? TranslationProviderPreset.customID
        baseURL = try container.decodeIfPresent(String.self, forKey: .baseURL) ?? ""
        model = try container.decodeIfPresent(String.self, forKey: .model) ?? ""
        asyncTranslation = try container.decodeIfPresent(Bool.self, forKey: .asyncTranslation) ?? true
        discoveredModels = try container.decodeIfPresent([String].self, forKey: .discoveredModels) ?? []
    }
}

/// What reading a provider's key found.
public enum ProviderKeyRead: Sendable, Equatable {
    /// The key as stored (not trimmed).
    case found(String)
    /// Nothing is stored for the provider.
    case absent
    /// The secret store could not be read; the key may well exist.
    case unavailable
}

/// Reads provider keys from the secret store. Not tied to the main actor: call it from a background task.
public struct ProviderKeyReader: Sendable {
    let secrets: any SecretStore

    public func read(_ id: UUID) -> ProviderKeyRead {
        switch secrets.read(TranslationProviderStore.secretAccount(for: id)) {
        case .found(let data): .found(String(decoding: data, as: UTF8.self))
        case .absent: .absent
        case .unavailable: .unavailable
        }
    }
}

/// The provider an engine launch should translate with, ready for `EngineLaunchSettings`.
public struct ActiveExternalLLM: Sendable, Equatable {
    public var llm: ExternalLLM
    /// Translate in the background while transcription is still running.
    public var asyncTranslation: Bool

    public init(llm: ExternalLLM, asyncTranslation: Bool) {
        self.llm = llm
        self.asyncTranslation = asyncTranslation
    }
}

/// The translation providers the user saved and which one is active (none means the local models), persisted in
/// `translation-providers.json` in the app data folder. API keys never touch that file: they are kept in the
/// `SecretStore` under `translation-provider.<uuid>` and removed with their provider.
@MainActor
@Observable
public final class TranslationProviderStore {
    public nonisolated static let fileName = "translation-providers.json"

    /// `<Application Support>/SubtitleAdder/translation-providers.json`.
    public nonisolated static var defaultFileURL: URL { AppDataDirectory.file(fileName) }

    /// UserDefaults flag set once the legacy single-server settings were looked at.
    public nonisolated static let migrationDefaultsKey = "translationProvidersMigrated"

    public private(set) var providers: [TranslationProviderProfile] = []
    /// The provider translation goes through; `nil` translates with a local model.
    public private(set) var activeProviderID: UUID?

    @ObservationIgnored private let fileURL: URL
    @ObservationIgnored private nonisolated let secrets: any SecretStore
    /// Why the file on disk may not simply be overwritten yet (see `LoadProblem`); `nil` when it may.
    @ObservationIgnored private var loadProblem: LoadProblem?

    /// Loads the file if it exists. A missing one starts empty; one that cannot be read, cannot be parsed or has
    /// entries that cannot be parsed starts with what could be read, and the file on disk is not replaced until a copy
    /// of it is safe (see `save()`). `secrets` and `fileURL` are injectable for tests.
    public init(fileURL: URL = TranslationProviderStore.defaultFileURL, secrets: any SecretStore = InMemorySecretStore()) {
        self.fileURL = fileURL
        self.secrets = secrets
        load()
    }

    public var activeProvider: TranslationProviderProfile? {
        providers.first { $0.id == activeProviderID }
    }

    // MARK: - Editing

    /// Saves a new provider without touching the secret store and, unless told otherwise, makes it the active one.
    /// Returns the profile as stored (trimmed, a blank name replaced by the preset's). The key is set separately, with
    /// `setKey(_:for:)`, so the caller decides when the secret store is touched.
    @discardableResult
    public func add(_ profile: TranslationProviderProfile, setActive: Bool = true) -> TranslationProviderProfile {
        let stored = normalized(profile)
        if let index = providers.firstIndex(where: { $0.id == stored.id }) {
            providers[index] = stored
        } else {
            providers.append(stored)
        }
        if setActive { activeProviderID = stored.id }
        save()
        return stored
    }

    /// Saves a new provider with its key (blank for none) and, unless told otherwise, makes it the active one.
    /// Returns the profile as stored. A secret store write that fails leaves the provider without a key, which
    /// `maskedKey(for:)` shows as `nil`. Writes the secret store on the calling thread.
    @discardableResult
    public func add(_ profile: TranslationProviderProfile, key: String, setActive: Bool = true) -> TranslationProviderProfile {
        let stored = add(profile, setActive: setActive)
        storeKey(key, for: stored.id)
        return stored
    }

    /// Replaces the provider with the same id. `key` `nil` keeps the stored key, a string replaces it (blank removes
    /// it). Returns `false` for an unknown id.
    @discardableResult
    public func update(_ profile: TranslationProviderProfile, key: String? = nil) -> Bool {
        guard let index = providers.firstIndex(where: { $0.id == profile.id }) else { return false }
        providers[index] = normalized(profile)
        if let key { storeKey(key, for: profile.id) }
        save()
        return true
    }

    /// Removes the provider and its key; when it was the active one, translation goes back to the local models.
    public func remove(_ id: UUID) {
        let before = providers.count
        providers.removeAll { $0.id == id }
        secrets.remove(Self.secretAccount(for: id))
        guard providers.count != before else { return }
        if activeProviderID == id { activeProviderID = nil }
        save()
    }

    /// Makes a provider the active one, or `nil` the local models. An unknown id counts as `nil`.
    public func setActive(_ id: UUID?) {
        let resolved = id.flatMap { id in providers.contains { $0.id == id } ? id : nil }
        guard resolved != activeProviderID else { return }
        activeProviderID = resolved
        save()
    }

    /// Changes only the model of a provider (what the toolbar picker does).
    public func updateModel(_ model: String, for id: UUID) {
        let model = model.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let index = providers.firstIndex(where: { $0.id == id }), providers[index].model != model else { return }
        providers[index].model = model
        save()
    }

    /// Replaces the models a provider reported. Only call it with the result of a successful connection test: a
    /// failed one says nothing about the provider's models. A provider without a chosen model takes the first.
    public func setDiscoveredModels(_ models: [String], for id: UUID) {
        guard let index = providers.firstIndex(where: { $0.id == id }) else { return }
        var seen = Set<String>()
        let cleaned = models
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty && seen.insert($0.lowercased()).inserted }
        providers[index].discoveredModels = cleaned
        if providers[index].model.isEmpty, let first = cleaned.first { providers[index].model = first }
        save()
    }

    // MARK: - Keys

    /// Secret store account of a provider's API key.
    public nonisolated static func secretAccount(for id: UUID) -> String {
        "translation-provider.\(id.uuidString)"
    }

    /// A reader that can be used from a background task.
    public nonisolated var keyReader: ProviderKeyReader { ProviderKeyReader(secrets: secrets) }

    /// The provider's API key, empty when none is stored. Reads the secret store on the calling thread and cannot tell a
    /// missing key from an unreadable one; the app does not use it (see `keyReader`).
    public func apiKey(for id: UUID) -> String {
        secrets.string(for: Self.secretAccount(for: id)) ?? ""
    }

    /// Replaces (blank removes) the key of an existing provider. `false` when the secret store refused it.
    @discardableResult
    public func setKey(_ key: String, for id: UUID) -> Bool {
        guard providers.contains(where: { $0.id == id }) else { return false }
        return storeKey(key, for: id)
    }

    /// The key as shown in lists, `nil` when there is none (the app says "Anahtar yok"). Reads the secret store on the
    /// calling thread.
    public func maskedKey(for id: UUID) -> String? {
        Self.mask(apiKey(for: id))
    }

    /// Shortest keys show nothing, medium ones their last four characters, long ones also the first three:
    /// `sk-1234567890` becomes `sk-••••7890`. `nil` for an empty key.
    public nonisolated static func mask(_ key: String) -> String? {
        let key = key.trimmingCharacters(in: .whitespacesAndNewlines)
        if key.isEmpty { return nil }
        if key.count <= 4 { return "••••" }
        if key.count <= 8 { return "••••" + key.suffix(4) }
        return key.prefix(3) + "••••" + key.suffix(4)
    }

    // MARK: - Models and the engine

    /// `TranslationProviderProfile.availableModels` of the provider with this id; empty for an unknown id.
    public func availableModels(for id: UUID) -> [String] {
        providers.first { $0.id == id }?.availableModels ?? []
    }

    /// The active provider as the engine needs it, with its key from the secret store (read on the calling thread);
    /// `nil` for local models or when the provider's address is blank or unusable.
    public var activeExternalLLM: ActiveExternalLLM? {
        guard let provider = activeProvider else { return nil }
        let llm = ExternalLLM(
            url: provider.baseURL.trimmingCharacters(in: .whitespacesAndNewlines),
            model: provider.model.trimmingCharacters(in: .whitespacesAndNewlines),
            apiKey: apiKey(for: provider.id).trimmingCharacters(in: .whitespacesAndNewlines)
        )
        guard llm.normalizedURL != nil else { return nil }
        return ActiveExternalLLM(llm: llm, asyncTranslation: provider.asyncTranslation)
    }

    // MARK: - Migration

    /// One-time import of the single LAN server the app used to keep in `UserDefaults` (`externalLlmEnabled`,
    /// `externalLlmUrl`, `externalLlmModel`, `externalLlmAsync`): when there is no providers file yet and an address
    /// was saved, it becomes a `lan` provider without a key, active if the server was switched on. The legacy keys
    /// are left alone, and the flag `migrationDefaultsKey` makes sure this decides only once. Returns whether a
    /// provider was created.
    @discardableResult
    public func migrateLegacyIfNeeded(defaults: UserDefaults = .standard) -> Bool {
        guard !defaults.bool(forKey: Self.migrationDefaultsKey) else { return false }
        defer { defaults.set(true, forKey: Self.migrationDefaultsKey) }
        guard providers.isEmpty, !FileManager.default.fileExists(atPath: fileURL.path) else { return false }
        let url = (defaults.string(forKey: "externalLlmUrl") ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty else { return false }
        // The old version always spoke plain HTTP to a scheme-less address. Today an address without a scheme is judged
        // by its host (a public-looking one becomes https and counts as a cloud service, which offline mode blocks), so
        // the address is kept the way the server has been used until now.
        let lowercased = url.lowercased()
        let address = lowercased.hasPrefix("http://") || lowercased.hasPrefix("https://") ? url : "http://" + url
        let profile = TranslationProviderProfile(
            name: TranslationProviderPreset.preset(id: "lan").name,
            presetID: "lan",
            baseURL: address,
            model: (defaults.string(forKey: "externalLlmModel") ?? "").trimmingCharacters(in: .whitespacesAndNewlines),
            asyncTranslation: defaults.bool(forKey: "externalLlmAsync")
        )
        providers.append(profile)
        activeProviderID = defaults.bool(forKey: "externalLlmEnabled") ? profile.id : nil
        save()
        return true
    }

    // MARK: - Storage

    private func normalized(_ profile: TranslationProviderProfile) -> TranslationProviderProfile {
        var profile = profile
        profile.presetID = profile.presetID.trimmingCharacters(in: .whitespacesAndNewlines)
        if profile.presetID.isEmpty { profile.presetID = TranslationProviderPreset.customID }
        profile.name = profile.name.trimmingCharacters(in: .whitespacesAndNewlines)
        if profile.name.isEmpty { profile.name = profile.preset.name }
        profile.baseURL = profile.baseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        profile.model = profile.model.trimmingCharacters(in: .whitespacesAndNewlines)
        return profile
    }

    @discardableResult
    private func storeKey(_ key: String, for id: UUID) -> Bool {
        let key = key.trimmingCharacters(in: .whitespacesAndNewlines)
        let account = Self.secretAccount(for: id)
        guard !key.isEmpty else {
            secrets.remove(account)
            return true
        }
        return secrets.set(key, for: account)
    }

    /// Why the providers file cannot be overwritten right now.
    private enum LoadProblem {
        /// It exists but could not be read (permissions, I/O error). Nothing is saved until it can be, and then the
        /// entries on disk are merged with the edits made meanwhile.
        case unreadable
        /// It could not be parsed, or entries of it were skipped. A copy is set aside before the first save.
        case damaged
    }

    private enum FileRead {
        case missing
        case unreadable
        case damaged
        case loaded(PersistedState)
    }

    /// An element of `providers` that may be unreadable without taking the others down.
    private struct LossyProfile: Decodable {
        let value: TranslationProviderProfile?

        init(from decoder: any Decoder) {
            value = try? TranslationProviderProfile(from: decoder)
        }
    }

    /// What goes to disk: the Windows file's two top-level fields.
    private struct PersistedState: Codable {
        var activeProviderId: UUID?
        var providers: [TranslationProviderProfile]
        /// Entries (and a malformed active id) the file had that could not be read; never written.
        var skipped = 0

        init(activeProviderId: UUID?, providers: [TranslationProviderProfile]) {
            self.activeProviderId = activeProviderId
            self.providers = providers
        }

        private enum CodingKeys: String, CodingKey { case activeProviderId, providers }

        /// Reads each provider on its own: one with a bad id or field is skipped (and counted), the rest stay. Only a
        /// file that is not a JSON object of this shape at all fails as a whole.
        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            do {
                activeProviderId = try container.decodeIfPresent(UUID.self, forKey: .activeProviderId)
            } catch {
                activeProviderId = nil
                skipped += 1
            }
            let lossy = try container.decodeIfPresent([LossyProfile].self, forKey: .providers) ?? []
            providers = lossy.compactMap(\.value)
            skipped += lossy.count - providers.count
        }

        /// A missing active provider is written as `null`, as the Windows app does.
        func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(activeProviderId, forKey: .activeProviderId)
            try container.encode(providers, forKey: .providers)
        }
    }

    private static func readFile(_ url: URL) -> FileRead {
        let data: Data
        do {
            data = try Data(contentsOf: url)
        } catch {
            var info = stat()
            return stat(url.path, &info) != 0 && errno == ENOENT ? .missing : .unreadable
        }
        guard let state = try? JSONDecoder().decode(PersistedState.self, from: data) else { return .damaged }
        return .loaded(state)
    }

    private func load() {
        switch Self.readFile(fileURL) {
        case .missing: break
        case .unreadable: loadProblem = .unreadable
        case .damaged: loadProblem = .damaged
        case .loaded(let state):
            providers = state.providers
            activeProviderID = state.activeProviderId.flatMap { id in providers.contains { $0.id == id } ? id : nil }
            if state.skipped > 0 { loadProblem = .damaged }
        }
    }

    /// Whether the file may be replaced now. A file that is merely missing always may. One that could not be read is
    /// read again first (it may be readable by now: its entries are merged in, ours win on the same id). One that
    /// was damaged or only partly readable is copied to `translation-providers.corrupt-<timestamp>.json` first, and
    /// if the copy cannot be made nothing is saved, so the original is never lost.
    private func prepareFileForSave() -> Bool {
        if loadProblem == .unreadable {
            switch Self.readFile(fileURL) {
            case .unreadable: return false
            case .missing: loadProblem = nil
            case .damaged: loadProblem = .damaged
            case .loaded(let disk):
                let known = Set(providers.map(\.id))
                providers = disk.providers.filter { !known.contains($0.id) } + providers
                if activeProviderID == nil {
                    activeProviderID = disk.activeProviderId.flatMap { id in providers.contains { $0.id == id } ? id : nil }
                }
                loadProblem = disk.skipped > 0 ? .damaged : nil
            }
        }
        guard loadProblem == .damaged else { return true }
        do {
            try ProfileFileIO.preserveDamagedFile(at: fileURL, now: Date())
            loadProblem = nil
            return true
        } catch {
            return false
        }
    }

    /// Atomic write; if the disk refuses it the state stays in memory for this session. Skipped while the file on
    /// disk could not be loaded and not yet be kept safe (see `prepareFileForSave()`).
    private func save() {
        guard prepareFileForSave() else { return }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(PersistedState(activeProviderId: activeProviderID, providers: providers)) else { return }
        try? FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: fileURL, options: .atomic)
    }
}
