import EngineKit
import Foundation

/// What the connection test says, with the styling it needs (success or error).
struct ExternalConnectionNotice: Equatable, Sendable {
    var message: LocalizedMessage
    var isError: Bool
}

/// External LLM server on the local network: the settings, the connection test and the background ("async")
/// translation of the lines transcribed so far.
extension SubtitleViewModel {
    // MARK: - Settings

    /// The server the engine translates with, or `nil` for a local model: enabled and the address usable.
    var externalLLM: ExternalLLM? {
        guard externalLlmEnabled else { return nil }
        let server = ExternalLLM(url: externalLlmUrl, model: externalLlmModel)
        return server.normalizedURL == nil ? nil : server
    }

    /// Translation goes to the external server in the background while transcription is still running.
    var isExternalAsyncActive: Bool { isExternalLLMActive && externalLlmAsync }

    /// What the toolbar's translation-model menu shows: the local model's name, or the external server's model.
    var translationModelTitle: String {
        guard isExternalLLMActive else { return ModelLabel.name(for: translationModel) }
        let name = externalLlmModel.trimmingCharacters(in: .whitespacesAndNewlines)
        let shown = name.isEmpty ? L("Harici LLM") : name
        return isExternalAsyncActive ? L("Harici: %@ (Asenkron)", shown) : L("Harici: %@", shown)
    }

    // MARK: - Connection test

    /// Asks the server for its models (`/api/tags`, then `/v1/models`), keeps the suitable ones and reports the
    /// result. An empty model field takes the first one found. Also what "Uzaktaki modelleri listele" runs.
    func testExternalConnection() async {
        let address = externalLlmUrl
        let testID = UUID()
        connectionTestID = testID
        isTestingExternalConnection = true
        let result = await ExternalLLMServer.testConnection(rawURL: address, fetch: externalFetch)
        // A newer test, or an address changed meanwhile, makes this answer obsolete.
        guard connectionTestID == testID else { return }
        isTestingExternalConnection = false
        guard address == externalLlmUrl else { return }
        switch result {
        case .invalidAddress:
            externalModels = []
            externalConnection = ExternalConnectionNotice(
                message: .key("Lütfen geçerli bir sunucu adresi girin."), isError: true
            )
        case .unreachable(let reason):
            externalModels = []
            externalConnection = ExternalConnectionNotice(
                message: LocalizedMessage("external-unreachable:\(reason)") { L("Sunucuya ulaşılamadı: %@", reason) },
                isError: true
            )
        case .reachable(let host, let latency, let models):
            externalModels = models
            if externalLlmModel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, let first = models.first {
                externalLlmModel = first
            }
            externalConnection = ExternalConnectionNotice(
                message: LocalizedMessage("external-ok:\(host):\(latency):\(models.count)") {
                    L("Bağlantı başarılı (%@ · %lld ms · %lld uygun model bulundu)", host, latency, models.count)
                },
                isError: false
            )
        }
    }

    // MARK: - Background translation

    /// Status bar addition while the server translates in the background, e.g. "Çevriliyor… 40%".
    var asyncStatusDetail: String? {
        asyncTranslationFraction.map { Self.label(for: .translating, fraction: $0, purpose: .translation).text }
    }

    /// Drops the background translation: stops the server request and forgets which lines were handed over.
    func cancelAsyncTranslation() {
        asyncTask?.cancel()
        asyncTask = nil
        asyncRunID = UUID()
        asyncQueued = []
        asyncDraining = false
        asyncTranslationFraction = nil
    }

    /// Called with every transcription snapshot: when lines are ready and no batch is on its way, sends them. The
    /// last line of a snapshot is still being transcribed, so it waits for the next one.
    func startAsyncTranslation(engine: EngineClient) {
        guard asyncTask == nil, !asyncDraining, !nextAsyncBatch().isEmpty else { return }
        let run = UUID()
        asyncRunID = run
        asyncTask = Task { await pumpAsyncTranslation(engine: engine, run: run) }
    }

    /// Transcription is over: no new batch starts, the one at the server finishes. Whatever is left is the final
    /// translation pass's job.
    func finishAsyncTranslation() async {
        asyncDraining = true
        if let task = asyncTask {
            if let fraction = asyncTranslationFraction {
                status = .working(Self.label(for: .translating, fraction: fraction, purpose: .translation), fraction: fraction)
            }
            await task.value
        }
        asyncTranslationFraction = nil
    }

    /// Lines ready for the server: everything but the last line, not handed over before, with text.
    func nextAsyncBatch() -> [Sentence] {
        guard !asyncDraining, sentences.count > 1 else { return [] }
        return sentences.dropLast().filter {
            !asyncQueued.contains($0.uid) && !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    /// One batch at a time until nothing is left to send. A failure just stops: the final pass translates
    /// the lines that are missing and reports the error if the server is really gone.
    private func pumpAsyncTranslation(engine: EngineClient, run: UUID) async {
        while !Task.isCancelled, asyncRunID == run {
            let batch = nextAsyncBatch()
            let target = translationTarget
            let source = translationSource
            // Same language in and out can't be translated; the toolbar target is reconciled when the language is known.
            guard !batch.isEmpty, source != target else { break }
            asyncQueued.formUnion(batch.map(\.uid))
            updateAsyncFraction()
            guard await translateAsyncBatch(batch, engine: engine, target: target, source: source, run: run) else { break }
        }
        // Nothing suspends between the last check and here, so a snapshot can't slip in unnoticed.
        guard asyncRunID == run else { return }
        asyncTask = nil
    }

    /// Returns `false` when the batch failed or the run is over.
    private func translateAsyncBatch(
        _ batch: [Sentence], engine: EngineClient, target: String, source: String?, run: UUID
    ) async -> Bool {
        do {
            for try await event in engine.translate(batch, to: target, from: source, model: translationModel) {
                guard !Task.isCancelled, asyncRunID == run else { return false }
                switch event {
                case .translation(let language, let translated):
                    applyBackgroundTranslation(translated, language: language, batch: batch)
                    updateAsyncFraction()
                case .error:
                    return false
                case .stage, .result, .partial:
                    break
                }
            }
        } catch {
            return false
        }
        return !Task.isCancelled && asyncRunID == run
    }

    /// Adds the translated lines of a batch, but only to lines that still read as they did when the batch was sent;
    /// a line whose text changed since is released so a later batch (or the final pass) translates it again.
    func applyBackgroundTranslation(_ translated: [Sentence], language: String, batch: [Sentence]) {
        var current: [UUID: Sentence] = [:]
        for sentence in sentences { current[sentence.uid] = sentence }
        var accepted: [(result: Sentence, original: Sentence)] = []
        for pair in Self.pairs(translated, with: batch) {
            if let now = current[pair.original.uid], now.text == pair.original.text {
                accepted.append(pair)
            } else {
                asyncQueued.remove(pair.original.uid)
            }
        }
        guard !accepted.isEmpty else { return }
        storeTranslations(accepted, language: language, merge: true)
    }

    private func updateAsyncFraction() {
        let total = max(1, sentences.count)
        asyncTranslationFraction = min(Double(translations.count) / Double(total), 0.99)
    }
}
