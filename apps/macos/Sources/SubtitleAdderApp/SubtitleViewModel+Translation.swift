import EngineKit
import Foundation

/// Translation flow: sends the lines to the engine and maps editable translation texts by `uid`.
extension SubtitleViewModel {
    /// Returns the line's translation if there is one.
    func translation(for sentence: Sentence) -> TranslationEntry? {
        translations[sentence.uid]
    }

    func isTranslationStale(for sentence: Sentence) -> Bool {
        translations[sentence.uid]?.isStale(for: sentence) ?? false
    }

    /// If the translation is chosen on export, the times are kept and the text is replaced by the translation.
    func exportSentences(translated: Bool) -> [Sentence] {
        guard translated else { return sentences }
        return sentences.map { sentence in
            var copy = sentence
            if let entry = translations[sentence.uid], !entry.text.isEmpty {
                copy.text = entry.text
            }
            copy.words = []
            return copy
        }
    }

    /// Translates all lines to the selected target language. While running, Stop cancels the same job.
    /// With `reuseExisting` (after background translation) only the lines that have no translation yet, whose
    /// translation is outdated or in another language are sent.
    func translate(reuseExisting: Bool = false) {
        guard !sentences.isEmpty else { return }
        guard let engine = catalog.engine else {
            status = .failed(.error(EngineError.notFound))
            return
        }
        commitDraft()
        commitTranslationDraft()
        let source = translationSource
        let target = translationTarget
        guard source != target else {
            status = .failed(.key("Kaynak ve hedef dil aynı olamaz."))
            return
        }
        // An external LLM takes any language pair, so NLLB's need for a source language doesn't apply.
        if translationModel == "nllb-1.3b", source == nil, !isExternalLLMActive {
            status = .failed(.key("NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç."))
            return
        }
        let model = translationModel
        if blockedOffline([model], retry: { [weak self] in self?.translate(reuseExisting: reuseExisting) }) { return }
        cancelAsyncTranslation()
        let request = reuseExisting ? linesNeedingTranslation(into: target) : sentences
        // Lines that already have a translation count as done in the progress.
        let alreadyDone = sentences.count - request.count
        let total = max(1, sentences.count)
        savedSubtitleName = nil
        job?.cancel()
        let id = UUID()
        jobID = id
        // An external server has no local model to protect from deletion.
        catalog.activeModels = isExternalLLMActive ? [] : [model]
        if request.isEmpty {
            // Everything was translated in the background: only the save is left.
            status = .working(.key("Çeviri hazırlanıyor…"), fraction: nil)
            job = Task {
                guard jobID == id, !Task.isCancelled else { return }
                status = .finished
                await autoSaveTranslationIfEnabled()
            }
            return
        }
        status = alreadyDone > 0
            ? .working(Self.label(for: .translating, fraction: Double(alreadyDone) / Double(total), purpose: .translation),
                       fraction: Double(alreadyDone) / Double(total))
            : .working(.key("Çeviri hazırlanıyor…"), fraction: nil)
        job = Task {
            // However it ends, the model becomes deletable in Settings.
            defer { if jobID == id { catalog.activeModels = [] } }
            var failed = false
            do {
                for try await event in engine.translate(request, to: target, from: source, model: model) {
                    guard jobID == id, !Task.isCancelled else { return }
                    switch event {
                    case .stage(let stage, var fraction):
                        if alreadyDone > 0, stage == .translating, let part = fraction {
                            fraction = (Double(alreadyDone) + Double(request.count) * min(max(part, 0), 1)) / Double(total)
                        }
                        status = .working(
                            Self.label(for: stage, fraction: fraction, purpose: .translation),
                            fraction: fraction
                        )
                    case .translation(let language, let translated):
                        // One event per batch (a growing prefix) plus a final one; each is applied as it arrives.
                        applyTranslation(translated, language: language, source: request, merge: reuseExisting)
                    case .result, .partial:
                        break
                    case .error(let message):
                        failed = true
                        status = .failed(.verbatim(message))
                    }
                }
                // On cancel the stream ends without an error; in that case the status doesn't change.
                guard jobID == id, !Task.isCancelled else { return }
                if !failed {
                    status = .finished
                    await autoSaveTranslationIfEnabled()
                }
                // Make the downloaded translation model show as "hazır" (ready) in Settings.
                await catalog.load()
            } catch is CancellationError {
                if jobID == id { status = .idle }
            } catch {
                if jobID == id { status = .failed(.error(error)) }
            }
        }
    }

    /// The lines that have no translation into `target`, or whose translation is outdated (the original was edited).
    func linesNeedingTranslation(into target: String) -> [Sentence] {
        sentences.filter { Self.needsTranslation($0, entry: translations[$0.uid], into: target) }
    }

    /// A line needs the server when it has no translation, or the translation is outdated (the original was edited) or
    /// in another language.
    nonisolated static func needsTranslation(_ sentence: Sentence, entry: TranslationEntry?, into target: String) -> Bool {
        guard let entry else { return true }
        return entry.isStale(for: sentence) || entry.language != target
    }

    /// The requested lines with up to two neighbours on each side, in order: a resumed or partial request keeps the
    /// dialogue around a line so the model translates it in context. Only the requested lines' results are used.
    nonisolated static func withTranslationContext(_ all: [Sentence], requested: [Sentence]) -> [Sentence] {
        let wanted = Set(requested.map(\.uid))
        var keep = Set<Int>()
        for (index, sentence) in all.enumerated() where wanted.contains(sentence.uid) {
            keep.formUnion(max(0, index - 2)...min(all.count - 1, index + 2))
        }
        return all.indices.filter(keep.contains).map { all[$0] }
    }

    /// Maps translated lines by their stable `uid` values; the engine preserves order and ids. With `merge` the
    /// translations of other lines (same language) are kept and these are added to them.
    func applyTranslation(_ translated: [Sentence], language: String, source: [Sentence], merge: Bool = false) {
        storeTranslations(Self.pairs(translated, with: source), language: language, merge: merge)
    }

    /// Each translated line with the line it translates: matched by id, else by position.
    static func pairs(_ translated: [Sentence], with source: [Sentence]) -> [(result: Sentence, original: Sentence)] {
        translated.enumerated().compactMap { index, result in
            let original = source.first(where: { $0.id == result.id })
                ?? (source.indices.contains(index) ? source[index] : nil)
            return original.map { (result, $0) }
        }
    }

    func storeTranslations(_ pairs: [(result: Sentence, original: Sentence)], language: String, merge: Bool) {
        let hadTranslations = hasTranslations
        var entries: [UUID: TranslationEntry] = merge && (translationLanguage == language || translations.isEmpty)
            ? translations
            : [:]
        for (result, original) in pairs {
            entries[original.uid] = TranslationEntry(
                text: result.text,
                sourceText: original.text,
                language: language
            )
        }
        translations = entries
        translationLanguage = language
        translationTarget = language
        // The first translation to arrive switches the video overlay and the export to the translated text.
        if !hadTranslations, hasTranslations {
            if displayMode == .original { displayMode = .translation }
            exportLanguage = .translated
        }
    }

    // MARK: - Editing the translation text

    func translationDraftText(for sentence: Sentence) -> String {
        guard let translationDraft, translationDraft.uid == sentence.uid else {
            return translation(for: sentence)?.text ?? ""
        }
        return translationDraft.text
    }

    func beginTranslationEditing(_ sentence: Sentence) {
        guard translationDraft?.uid != sentence.uid else { return }
        commitTranslationDraft()
        guard translations[sentence.uid] != nil else { return }
        translationDraft = TextDraft(uid: sentence.uid, text: translations[sentence.uid]?.text ?? "")
    }

    func updateTranslationDraft(_ text: String, for uid: UUID) {
        if let translationDraft, translationDraft.uid != uid {
            commitTranslationDraft()
        }
        if var translationDraft, translationDraft.uid == uid {
            guard translationDraft.text != text else { return }
            translationDraft.text = text
            self.translationDraft = translationDraft
        } else if translations[uid] != nil {
            translationDraft = TextDraft(uid: uid, text: text)
        }
    }

    /// Writes the translation draft to the line and creates an undo record.
    func commitTranslationDraft() {
        guard let translationDraft else { return }
        self.translationDraft = nil
        guard let entry = translations[translationDraft.uid] else { return }
        let text = translationDraft.text.replacingOccurrences(of: "\n", with: " ")
        guard entry.text != text else { return }
        setTranslationText(text, for: translationDraft.uid)
    }

    func setTranslationText(_ text: String, for uid: UUID) {
        guard var entry = translations[uid], entry.text != text else { return }
        entry.text = text
        var updated = translations
        updated[uid] = entry
        replaceTranslations(updated, actionName: L("Çeviriyi Değiştir"))
    }

    private func replaceTranslations(_ updated: [UUID: TranslationEntry], actionName: String) {
        let previous = translations
        translations = updated
        guard let undoManager else { return }
        undoManager.registerUndo(withTarget: self) { model in
            MainActor.assumeIsolated { model.replaceTranslations(previous, actionName: actionName) }
        }
        undoManager.setActionName(actionName)
    }
}
