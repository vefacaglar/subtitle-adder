import EngineKit
import Foundation
import LibraryKit

/// What the app knows about one provider's API key.
enum ProviderKey: Equatable, Sendable {
    /// The secret store has not answered yet.
    case loading
    /// The secret store answered: no key is stored.
    case absent
    /// The key, trimmed.
    case present(String)
    /// The secret store could not be read. The key may well
    /// exist, so this is never treated as "no key".
    case unreadable

    /// The key as lists show it (`sk-••••7890`); `nil` unless a key is known.
    var masked: String? {
        if case .present(let key) = self { return TranslationProviderStore.mask(key) }
        return nil
    }

    /// The key itself; empty unless a key is known.
    var value: String {
        if case .present(let key) = self { return key }
        return ""
    }

    var isSettled: Bool {
        switch self {
        case .absent, .present: true
        case .loading, .unreadable: false
        }
    }

    init(_ read: ProviderKeyRead) {
        switch read {
        case .found(let key):
            let trimmed = key.trimmingCharacters(in: .whitespacesAndNewlines)
            self = trimmed.isEmpty ? .absent : .present(trimmed)
        case .absent: self = .absent
        case .unavailable: self = .unreadable
        }
    }
}

/// API keys of the translation providers, kept in memory so that nothing on the main thread waits for the secret store.
///
/// The views and the launch settings are made from this cache, and a translation that is about to
/// start reads the key again (`prepareExternalLaunch`). Writing a key (the Save button of the provider sheet) is
/// user-initiated and still happens on the calling thread.
extension SubtitleViewModel {
    // MARK: - Reading

    /// What is known about the provider's key right now; `.loading` until the first read ends.
    func providerKey(for id: UUID) -> ProviderKey {
        providerKeys[id] ?? .loading
    }

    /// The key as the provider list shows it: `sk-••••7890`, "Anahtar yok", or why it can't be shown.
    func providerKeyCaption(for id: UUID) -> String {
        switch providerKey(for: id) {
        case .loading: L("Anahtar okunuyor…")
        case .absent: L("Anahtar yok")
        case .present(let key): TranslationProviderStore.mask(key) ?? L("Anahtar yok")
        case .unreadable: L("Anahtar okunamadı")
        }
    }

    /// Reads the keys of `ids` from the secret store on a background thread and stores what was found. A provider that was
    /// saved or removed meanwhile keeps what that said. Publishes the launch settings when done.
    func loadProviderKeys(_ ids: [UUID]) async {
        let ids = ids.filter { id in providerStore.providers.contains { $0.id == id } }
        guard !ids.isEmpty else { return }
        for id in ids where providerKeys[id] == nil { providerKeys[id] = .loading }
        let epochs = ids.map { providerKeyEpochs[$0, default: 0] }
        let reader = providerStore.keyReader
        let reads = await Task.detached(priority: .userInitiated) { ids.map { reader.read($0) } }.value
        for (index, id) in ids.enumerated() {
            guard providerKeyEpochs[id, default: 0] == epochs[index],
                  providerStore.providers.contains(where: { $0.id == id })
            else { continue }
            let now = ProviderKey(reads[index])
            // A key we already hold is better than none: a read that fails now doesn't
            // throw it away.
            if now == .unreadable, case .present = providerKeys[id] { continue }
            providerKeys[id] = now
        }
        publishLaunchSettings()
    }

    /// Starts reading every saved provider's key; called once at launch.
    func startLoadingProviderKeys() {
        let ids = providerStore.providers.map(\.id)
        providerKeyLoad = Task { await loadProviderKeys(ids) }
    }

    /// Returns once the launch read of the keys is over (tests, and anything that needs the keys right away).
    func waitForProviderKeys() async {
        await providerKeyLoad?.value
    }

    /// The key to use for `id` right now: the cached one when the secret store has answered, else a fresh read on a
    /// background thread (an earlier refusal may have been fixed since).
    func providerKeyForUse(_ id: UUID) async -> ProviderKey {
        if providerKey(for: id).isSettled { return providerKey(for: id) }
        await loadProviderKeys([id])
        return providerKey(for: id)
    }

    // MARK: - Writing

    /// Records what a write to the secret store left behind. `stored` is `false` when the write was refused: the old key may
    /// still be there, so the cache reads it again instead of guessing.
    func recordKeyWritten(_ key: String, stored: Bool, for id: UUID) {
        providerKeyEpochs[id, default: 0] += 1
        let trimmed = key.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty {
            providerKeys[id] = .absent
        } else if stored {
            providerKeys[id] = .present(trimmed)
        } else {
            providerKeys[id] = .loading
            Task { await loadProviderKeys([id]) }
        }
    }

    func forgetKey(of id: UUID) {
        providerKeyEpochs[id, default: 0] += 1
        providerKeys[id] = nil
    }

    // MARK: - Launch settings

    /// The active provider for the engine, with the key from the cache. Never touches the secret store; a key that has not
    /// been read yet (or could not be) goes along empty, and the translation that is about to start reads it again.
    func cachedExternalLLM() -> ExternalLLM? {
        guard let provider = providerStore.activeProvider else { return nil }
        let llm = ExternalLLM(
            url: provider.baseURL.trimmingCharacters(in: .whitespacesAndNewlines),
            model: provider.model.trimmingCharacters(in: .whitespacesAndNewlines),
            apiKey: providerKey(for: provider.id).value
        )
        return llm.normalizedURL == nil ? nil : llm
    }

    /// Makes sure the launch settings of a translation that is about to start carry the active provider's real key: it
    /// is read from the secret store again, on a background thread, and the launch settings are published. Returns what to
    /// show instead of starting when the provider's key exists but can't be read (sending the text without it would only
    /// fail at the server with a misleading "unauthorized"). `nil` when there is nothing in the way, including when no
    /// provider is active.
    func prepareExternalLaunch() async -> LocalizedMessage? {
        guard let provider = activeExternalProvider else { return nil }
        await loadProviderKeys([provider.id])
        guard activeExternalProvider?.id == provider.id, providerKey(for: provider.id) == .unreadable else { return nil }
        return Self.keyUnreadableMessage(provider.name)
    }

    static func keyUnreadableMessage(_ providerName: String) -> LocalizedMessage {
        LocalizedMessage("key-unreadable:\(providerName)") {
            L("%@ için API anahtarı okunamadı. Anahtarı yeniden girip tekrar dene.", providerName)
        }
    }
}
