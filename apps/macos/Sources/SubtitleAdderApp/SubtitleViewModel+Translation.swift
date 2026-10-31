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
    func translate() {
        guard !sentences.isEmpty else { return }
        guard let engine = catalog.engine else {
            status = .failed(EngineError.notFound.localizedDescription)
            return
        }
        commitDraft()
        commitTranslationDraft()
        let source = translationSource
        let target = translationTarget
        guard source != target else {
            status = .failed("Kaynak ve hedef dil aynı olamaz.")
            return
        }
        if translationModel == "nllb-1.3b", source == nil {
            status = .failed("NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç.")
            return
        }
        let request = sentences
        let model = translationModel
        job?.cancel()
        let id = UUID()
        jobID = id
        catalog.activeModel = model
        status = .working("Çeviri hazırlanıyor…", fraction: nil)
        job = Task {
            // However it ends, the model becomes deletable in Settings.
            defer { if jobID == id { catalog.activeModel = nil } }
            var failed = false
            do {
                for try await event in engine.translate(request, to: target, from: source, model: model) {
                    guard jobID == id, !Task.isCancelled else { return }
                    switch event {
                    case .stage(let stage, let fraction):
                        status = .working(
                            Self.label(for: stage, fraction: fraction, purpose: .translation),
                            fraction: fraction
                        )
                    case .translation(let language, let translated):
                        applyTranslation(translated, language: language, source: request)
                    case .result:
                        break
                    case .error(let message):
                        failed = true
                        status = .failed(message)
                    }
                }
                // On cancel the stream ends without an error; in that case the status doesn't change.
                guard jobID == id, !Task.isCancelled else { return }
                if !failed { status = .finished }
                // Make the downloaded translation model show as "hazır" (ready) in Settings.
                await catalog.load()
            } catch is CancellationError {
                if jobID == id { status = .idle }
            } catch {
                if jobID == id { status = .failed(error.localizedDescription) }
            }
        }
    }

    /// Maps translated lines by their stable `uid` values; the engine preserves order and ids.
    private func applyTranslation(_ translated: [Sentence], language: String, source: [Sentence]) {
        var entries: [UUID: TranslationEntry] = [:]
        for (index, result) in translated.enumerated() {
            let original = source.first(where: { $0.id == result.id })
                ?? (source.indices.contains(index) ? source[index] : nil)
            guard let original else { continue }
            entries[original.uid] = TranslationEntry(
                text: result.text,
                sourceText: original.text,
                language: language
            )
        }
        translations = entries
        translationLanguage = language
        translationTarget = language
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
        replaceTranslations(updated, actionName: "Çeviriyi Değiştir")
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
