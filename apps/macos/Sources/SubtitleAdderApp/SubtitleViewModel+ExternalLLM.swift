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

    /// The client with the launch settings as they are right now. Translation that starts later (a queued background
    /// task, the next batch) keeps the settings its decision was made under instead of whatever Settings says by then.
    func pinningLaunchSettings(_ engine: EngineClient) -> EngineClient {
        var pinned = engine
        pinned.launchSettings = launchState.settings
        return pinned
    }

    // MARK: - Connection test

    /// Asks the server for its models (`/api/tags`, then `/v1/models`), keeps the suitable ones and reports the
    /// result. An empty model field takes the first one found. Also what "Uzaktaki modelleri listele" runs.
    /// A cancelled probe changes nothing.
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
        case .cancelled:
            // The window that asked went away; the server wasn't heard from, so the earlier notice and list stand.
            return
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

    /// Fills the list of the server's models once an external server is in use and nothing is known about it yet; the
    /// toolbar menu shows what is there when opened, so this runs ahead of time (launch, switching the server on).
    func refreshExternalModelsIfNeeded() async {
        guard isExternalLLMActive, externalModels.isEmpty, !isTestingExternalConnection else { return }
        await testExternalConnection()
    }

    // MARK: - Background translation

    /// Status bar addition while the server translates in the background, e.g. "Çevriliyor… 40%". Nothing once the
    /// status itself shows the background translation.
    var asyncStatusDetail: String? {
        guard isAsyncTranslating, !asyncShownInStatus else { return nil }
        return asyncStatusText
    }

    /// "Çevriliyor… 40%", or "Çevriliyor…" while the share isn't known.
    var asyncStatusText: String {
        Self.label(for: .translating, fraction: asyncTranslationFraction, purpose: .translation).text
    }

    /// Drops the background translation: stops the server request and forgets which lines were handed over.
    func cancelAsyncTranslation() {
        asyncTask?.cancel()
        asyncTask = nil
        asyncRunID = UUID()
        asyncQueued = []
        asyncDraining = false
        asyncFailed = false
        asyncShownInStatus = false
        isAsyncTranslating = false
    }

    /// Called with every transcription snapshot: when enough lines are ready and no batch is on its way, sends them.
    /// The last two lines of a snapshot are still being transcribed, so they wait for the next one.
    func startAsyncTranslation(engine: EngineClient) {
        // The lines keep coming, so the share of translated ones moves even when no batch is sent.
        updateAsyncFraction()
        guard asyncTask == nil, !asyncDraining, !asyncFailed, !nextAsyncBatch().isEmpty else { return }
        let run = UUID()
        asyncRunID = run
        asyncTask = Task { await pumpAsyncTranslation(engine: engine, run: run) }
    }

    /// Transcription is over: no new batch starts, the one at the server finishes. Whatever is left is the final
    /// translation pass's job.
    func finishAsyncTranslation() async {
        asyncDraining = true
        if let task = asyncTask {
            asyncShownInStatus = true
            mirrorAsyncInStatus()
            await task.value
        }
        asyncShownInStatus = false
        isAsyncTranslating = false
    }

    /// Fewest new lines worth a request: smaller batches lose the context a model translates well with.
    nonisolated static var minAsyncBatchSentences: Int { 10 }

    /// Lines ready for the server: everything but the last two (the live end of the transcript still changes), not
    /// handed over before, with text. Empty until at least `minAsyncBatchSentences` of them have piled up.
    func nextAsyncBatch() -> [Sentence] {
        guard !asyncDraining, !asyncFailed, sentences.count > 2 else { return [] }
        let candidates = sentences.dropLast(2).filter {
            !asyncQueued.contains($0.uid) && !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
        return candidates.count >= Self.minAsyncBatchSentences ? Array(candidates) : []
    }

    /// One batch at a time until nothing is left to send. A failure stops it for the rest of the run (`asyncFailed`):
    /// the final pass translates the lines that are missing and reports the error if the server is really gone.
    private func pumpAsyncTranslation(engine: EngineClient, run: UUID) async {
        while !Task.isCancelled, asyncRunID == run {
            let batch = nextAsyncBatch()
            let target = translationTarget
            let source = translationSource
            // Same language in and out can't be translated; the toolbar target is reconciled when the language is known.
            guard !batch.isEmpty, source != target else { break }
            asyncQueued.formUnion(batch.map(\.uid))
            isAsyncTranslating = true
            updateAsyncFraction()
            guard await translateAsyncBatch(batch, engine: engine, target: target, source: source, run: run) else {
                if !Task.isCancelled, asyncRunID == run {
                    asyncFailed = true
                    asyncShownInStatus = false
                    isAsyncTranslating = false
                }
                break
            }
        }
        // Nothing suspends between the last check and here, so a snapshot can't slip in unnoticed.
        guard asyncRunID == run else { return }
        asyncTask = nil
    }

    /// Returns `false` when the batch failed or the run is over.
    private func translateAsyncBatch(
        _ batch: [Sentence], engine: EngineClient, target: String, source: String?, run: UUID
    ) async -> Bool {
        // The neighbours of the batch's lines go along as context; only the batch's own lines are applied.
        let request = Self.withTranslationContext(sentences, requested: batch)
        let alreadyTranslated = translations.count
        do {
            for try await event in engine.translate(request, to: target, from: source, model: translationModel) {
                guard !Task.isCancelled, asyncRunID == run else { return false }
                switch event {
                case .translation(let language, let translated):
                    applyBackgroundTranslation(translated, language: language, batch: batch)
                    updateAsyncFraction()
                case .stage(let stage, let part):
                    // The model's own progress through this batch, on top of what was translated before it.
                    guard stage == .translating, let part, isAsyncTranslating else { break }
                    setAsyncFraction(Self.translationProgress(
                        alreadyDone: alreadyTranslated, requested: batch.count, part: part,
                        total: max(sentences.count, alreadyTranslated + batch.count)
                    ))
                case .error:
                    return false
                case .result, .partial:
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

    /// Background progress from the translations at hand: translated lines over all lines, below 1.
    func updateAsyncFraction() {
        guard isAsyncTranslating else { return }
        setAsyncFraction(Self.translationProgress(done: translations.count, total: sentences.count))
    }

    private func setAsyncFraction(_ fraction: Double?) {
        asyncTranslationFraction = fraction
        mirrorAsyncInStatus()
    }

    /// After transcription the status is the background translation itself.
    private func mirrorAsyncInStatus() {
        guard asyncShownInStatus, case .working = status else { return }
        status = .working(
            Self.label(for: .translating, fraction: asyncTranslationFraction, purpose: .translation),
            fraction: asyncTranslationFraction
        )
    }
}
