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
        guard isAsyncTranslating, !foreground.asyncShownInStatus else { return nil }
        return asyncStatusText
    }

    /// "Çevriliyor… 40%", or "Çevriliyor…" while the share isn't known.
    var asyncStatusText: String {
        Self.label(for: .translating, fraction: asyncTranslationFraction, purpose: .translation).text
    }

    /// Background translation of the video on screen, with the toolbar's settings as they are now.
    func startAsyncTranslation(engine: EngineClient) {
        startAsyncTranslation(foreground, engine: engine, settings: currentJobSettings())
    }

    /// Called with every transcription snapshot of `session`: when enough lines are ready and no batch is on its way,
    /// sends them. The last two lines of a snapshot are still being transcribed, so they wait for the next one.
    func startAsyncTranslation(_ session: VideoSession, engine: EngineClient, settings: JobSettings) {
        // The lines keep coming, so the share of translated ones moves even when no batch is sent.
        session.updateAsyncFraction()
        // Another translation holds the gate: nothing starts now, the next snapshot looks again.
        guard session.asyncTask == nil, !session.asyncDraining, !session.asyncFailed, !translationGate.isBusy,
              !session.nextAsyncBatch().isEmpty
        else { return }
        let run = UUID()
        session.asyncRunID = run
        session.asyncTask = Task { await pumpAsyncTranslation(session, engine: engine, settings: settings, run: run) }
    }

    /// Fewest new lines worth a request: smaller batches lose the context a model translates well with.
    nonisolated static var minAsyncBatchSentences: Int { VideoSession.minAsyncBatchSentences }

    /// One batch at a time until nothing is left to send. A failure stops it for the rest of the run (`asyncFailed`):
    /// the final pass translates the lines that are missing and reports the error if the server is really gone.
    ///
    /// Each batch holds the app-wide translation gate. The pump never queues for it: if another translation (the
    /// playlist's stage 3, the editor's job) has the gate, it stops instead of starting a batch, and the lines it
    /// skipped are picked up by a later snapshot or by the closing pass.
    private func pumpAsyncTranslation(
        _ session: VideoSession, engine: EngineClient, settings: JobSettings, run: UUID
    ) async {
        while !Task.isCancelled, session.asyncRunID == run {
            guard translationGate.tryAcquire() else { break }
            let batch = session.nextAsyncBatch()
            let target = settings.translationTarget
            let source = session.translationSource(language: settings.language)
            // Same language in and out can't be translated; the target is reconciled when the language is known.
            guard !batch.isEmpty, source != target else {
                translationGate.release()
                break
            }
            session.asyncQueued.formUnion(batch.map(\.uid))
            session.isAsyncTranslating = true
            session.updateAsyncFraction()
            let succeeded = await translateAsyncBatch(
                batch, in: session, engine: engine, target: target, source: source,
                model: settings.translationModel, run: run
            )
            translationGate.release()
            guard succeeded else {
                if !Task.isCancelled, session.asyncRunID == run {
                    session.asyncFailed = true
                    session.asyncShownInStatus = false
                    session.isAsyncTranslating = false
                }
                break
            }
        }
        // Nothing suspends between the last check and here, so a snapshot can't slip in unnoticed.
        guard session.asyncRunID == run else { return }
        session.asyncTask = nil
    }

    /// Returns `false` when the batch failed or the run is over.
    private func translateAsyncBatch(
        _ batch: [Sentence], in session: VideoSession, engine: EngineClient, target: String, source: String?,
        model: String, run: UUID
    ) async -> Bool {
        // The neighbours of the batch's lines go along as context; only the batch's own lines are applied.
        let request = Self.withTranslationContext(session.sentences, requested: batch)
        let alreadyTranslated = session.translations.count
        do {
            for try await event in engine.translate(request, to: target, from: source, model: model) {
                guard !Task.isCancelled, session.asyncRunID == run else { return false }
                switch event {
                case .translation(let language, let translated):
                    session.applyBackgroundTranslation(translated, language: language, batch: batch)
                    if session === foreground { translationTarget = language }
                    session.updateAsyncFraction()
                case .stage(let stage, let part):
                    // The model's own progress through this batch, on top of what was translated before it.
                    guard stage == .translating, let part, session.isAsyncTranslating else { break }
                    session.setAsyncFraction(Self.translationProgress(
                        alreadyDone: alreadyTranslated, requested: batch.count, part: part,
                        total: max(session.sentences.count, alreadyTranslated + batch.count)
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
            if !Task.isCancelled, session.asyncRunID == run {
                engine.logDiagnostic(
                    "translation", "background batch of \(batch.count) lines failed: \(error.localizedDescription)"
                )
            }
            return false
        }
        return !Task.isCancelled && session.asyncRunID == run
    }

    /// Adds the translated lines of a batch to the video on screen; see `VideoSession.applyBackgroundTranslation`.
    func applyBackgroundTranslation(_ translated: [Sentence], language: String, batch: [Sentence]) {
        foreground.applyBackgroundTranslation(translated, language: language, batch: batch)
        translationTarget = language
    }
}
