import EngineKit
import Foundation

/// The external server (a cloud provider or a LAN server, see `SubtitleViewModel+Providers.swift`) translating in the
/// background ("async") the lines transcribed so far.
extension SubtitleViewModel {
    /// The client with the launch settings as they are right now. Translation that starts later (a queued background
    /// task, the next batch) keeps the settings its decision was made under instead of whatever Settings says by then.
    func pinningLaunchSettings(_ engine: EngineClient) -> EngineClient {
        var pinned = engine
        pinned.launchSettings = launchState.settings
        return pinned
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
        asyncTranscriptionComplete = false
        asyncFailed = false
        asyncShownInStatus = false
        isAsyncTranslating = false
    }

    /// Called with every transcription snapshot: when enough lines are ready and no batch is on its way, sends them.
    /// The last two lines of a snapshot are still being transcribed, so they wait for the next one.
    func startAsyncTranslation(engine: EngineClient) {
        // The lines keep coming, so the share of translated ones moves even when no batch is sent.
        updateAsyncFraction()
        // Another translation holds the gate: nothing starts now, the next snapshot looks again.
        guard asyncTask == nil, !asyncDraining, !asyncFailed, !translationGate.isBusy, !nextAsyncBatch().isEmpty else { return }
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
    /// handed over before, with text. Empty until at least `minAsyncBatchSentences` of them have piled up. Once
    /// recognition is complete (`asyncTranscriptionComplete`) every line counts and a single one is enough.
    func nextAsyncBatch() -> [Sentence] {
        let complete = asyncTranscriptionComplete
        guard !asyncDraining, !asyncFailed, complete ? !sentences.isEmpty : sentences.count > 2 else { return [] }
        let stable = complete ? sentences[...] : sentences.dropLast(2)
        let candidates = stable.filter {
            !asyncQueued.contains($0.uid) && !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
        return candidates.count >= (complete ? 1 : Self.minAsyncBatchSentences) ? Array(candidates) : []
    }

    /// One batch at a time until nothing is left to send. A failure stops it for the rest of the run (`asyncFailed`):
    /// the final pass translates the lines that are missing and reports the error if the server is really gone.
    ///
    /// Each batch holds the app-wide translation gate. The pump never queues for it: if another translation (the
    /// playlist's stage 3, the editor's job) has the gate, it stops instead of starting a batch, and the lines it
    /// skipped are picked up by a later snapshot or by the closing pass.
    private func pumpAsyncTranslation(engine: EngineClient, run: UUID) async {
        while !Task.isCancelled, asyncRunID == run {
            guard translationGate.tryAcquire() else { break }
            let batch = nextAsyncBatch()
            let target = translationTarget
            let source = translationSource
            // Same language in and out can't be translated; the toolbar target is reconciled when the language is known.
            guard !batch.isEmpty, source != target else {
                translationGate.release()
                break
            }
            asyncQueued.formUnion(batch.map(\.uid))
            isAsyncTranslating = true
            updateAsyncFraction()
            let succeeded = await translateAsyncBatch(batch, engine: engine, target: target, source: source, run: run)
            translationGate.release()
            guard succeeded else {
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
                case .error(let message):
                    engine.logDiagnostic("translation", "background batch of \(batch.count) lines failed: \(message)")
                    return false
                case .result, .partial:
                    break
                }
            }
        } catch {
            // The engine's error text (never the lines themselves) so a failure that only shows as "some lines are
            // missing" can be traced afterwards.
            if !Task.isCancelled, asyncRunID == run {
                engine.logDiagnostic(
                    "translation", "background batch of \(batch.count) lines failed: \(error.localizedDescription)"
                )
            }
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
