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

    /// Whether the engine translates through an external LLM server (then no local translation model is needed): a
    /// provider is active and its address is usable.
    var isExternalLLMActive: Bool { activeExternalProvider != nil }

    /// Call at the start of every job that may download models, with the ids of the models the job needs.
    /// In offline mode, a catalog model that isn't downloaded sets a failed status and remembers `retry` (the entry point
    /// to run again once offline mode is off); returns `true` and the job must not start. Ids the catalog doesn't
    /// know, or any id before the catalog has been loaded, are left to the engine, which reports a missing model itself.
    func blockedOffline(
        _ modelIDs: [String], session: VideoSession? = nil, retry: @escaping @MainActor () -> Void
    ) -> Bool {
        let session = session ?? foreground
        session.offlineRetry = nil
        guard offlineMode else { return false }
        // A job that translates (its list carries the translation model) would send the text to the internet.
        if modelIDs.contains(translationModel), let provider = activeCloudProviderName {
            session.status = .failed(Self.offlineCloudMessage(provider))
            session.offlineRetry = retry
            return true
        }
        guard catalog.hasLoaded else { return false }
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
        session.status = .failed(LocalizedMessage("offline:" + names.joined(separator: ",")) {
            L("%@ indirilmemiş. Çevrimdışı mod açıkken model indirilemez.",
              names.map { ModelLabel.name(for: $0) }.joined(separator: ", "))
        })
        // After the status: any other status change clears it.
        session.offlineRetry = retry
        return true
    }

    /// Title of the status-bar action that resolves the offline block: download the missing model, or send the text
    /// to the cloud provider.
    var offlineRetryTitle: String {
        if case .failed(let message) = status, message.id.hasPrefix(Self.offlineCloudPrefix) {
            return L("Çevrimdışı modu kapat ve çevir")
        }
        return L("Çevrimdışı modu kapat ve indir")
    }

    /// Whether offline mode keeps the app from contacting `url` (anything outside the local network).
    func offlineBlocksServer(_ url: String) -> Bool {
        offlineMode && !ExternalLLM(url: url).isOnLocalNetwork
    }

    private static let offlineCloudPrefix = "offline-cloud:"

    /// Offline mode keeps the text from going to the cloud provider named `provider`.
    static func offlineCloudMessage(_ provider: String) -> LocalizedMessage {
        LocalizedMessage("\(offlineCloudPrefix)\(provider)") {
            L("Çevrimdışı mod açık: çeviri internetteki bir sağlayıcıya (%@) gönderilemez. Çevrimdışı modu kapat ya da yerel bir model veya sunucu seç.", provider)
        }
    }

    /// Turns offline mode off (persisted) and runs the blocked job again, which downloads the missing model.
    func goOnlineAndRetry() {
        guard canGoOnlineAndRetry, let retry = offlineRetry else { return }
        // The failure was this attempt's; whatever the retry starts reports for itself (a playlist run doesn't touch the
        // status of the video on screen at all).
        status = .idle
        offlineMode = false
        retry()
    }
}
