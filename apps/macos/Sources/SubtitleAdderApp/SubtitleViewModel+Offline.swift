import EngineKit
import Foundation

/// Strict offline mode: a job that needs a model that isn't on disk fails up front instead of starting a download,
/// and the failure offers to turn offline mode off and run the same action again.
extension SubtitleViewModel {
    /// Whether the current failure is an offline block the user can resolve (shown as an action in the status bar).
    var canGoOnlineAndRetry: Bool {
        guard case .failed = status else { return false }
        return offlineRetry != nil
    }

    /// Whether the engine translates through an external LLM server (then no local translation model is needed).
    var isExternalLLMActive: Bool { externalLLM?.normalizedURL != nil }

    /// Call at the start of every job that may download models, with the ids of the models the job needs.
    /// In offline mode, a catalog model that isn't downloaded sets a failed status and remembers `retry` (the entry point
    /// to run again once offline mode is off); returns `true` and the job must not start. Ids the catalog doesn't
    /// know, or any id before the catalog has been loaded, are left to the engine, which reports a missing model itself.
    func blockedOffline(_ modelIDs: [String], retry: @escaping @MainActor () -> Void) -> Bool {
        offlineRetry = nil
        guard offlineMode, catalog.hasLoaded else { return false }
        let translationIDs = Set(catalog.translationModels.map(\.id))
        var missing: [String] = []
        for id in modelIDs where !missing.contains(id) {
            if isExternalLLMActive, translationIDs.contains(id) { continue }
            if let model = catalog.models.first(where: { $0.id == id }), !model.downloaded {
                missing.append(id)
            }
        }
        guard !missing.isEmpty else { return false }
        let names = missing
        status = .failed(LocalizedMessage("offline:" + names.joined(separator: ",")) {
            L("%@ indirilmemiş. Çevrimdışı mod açıkken model indirilemez.",
              names.map { ModelLabel.name(for: $0) }.joined(separator: ", "))
        })
        // After the status: any other status change clears it.
        offlineRetry = retry
        return true
    }

    /// Turns offline mode off (persisted) and runs the blocked job again, which downloads the missing model.
    func goOnlineAndRetry() {
        guard canGoOnlineAndRetry, let retry = offlineRetry else { return }
        offlineRetry = nil
        offlineMode = false
        retry()
    }
}
