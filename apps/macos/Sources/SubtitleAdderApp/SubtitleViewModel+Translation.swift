import EngineKit
import Foundation

/// Çeviri akışı: satırları motora gönderir, düzenlenebilir çeviri metinlerini `uid` ile eşler.
extension SubtitleViewModel {
    /// Satırın çevirisi varsa döner.
    func translation(for sentence: Sentence) -> TranslationEntry? {
        translations[sentence.uid]
    }

    func isTranslationStale(for sentence: Sentence) -> Bool {
        translations[sentence.uid]?.isStale(for: sentence) ?? false
    }

    /// Dışa aktarmada çeviri seçilirse zamanlar korunur, metin çeviriyle değişir.
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

    /// Tüm satırları seçili hedef dile çevirir. Sürerken Durdur aynı işi iptal eder.
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
            // Hangi yolla biterse bitsin model Ayarlar'da silinebilir olur.
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
                // İptalde akış hata vermeden biter; o durumda durum değişmez.
                guard jobID == id, !Task.isCancelled else { return }
                if !failed { status = .finished }
                // İndirilen çeviri modeli Ayarlar'da "hazır" görünsün.
                await catalog.load()
            } catch is CancellationError {
                if jobID == id { status = .idle }
            } catch {
                if jobID == id { status = .failed(error.localizedDescription) }
            }
        }
    }

    /// Çevrilen satırları kararlı `uid` değerleriyle eşler; motor sırayı ve kimlikleri korur.
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

    // MARK: - Çeviri metnini düzenleme

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

    /// Çeviri taslağını satıra yazar ve geri alma kaydı oluşturur.
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
