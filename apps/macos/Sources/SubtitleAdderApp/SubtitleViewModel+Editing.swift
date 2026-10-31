import EngineKit
import Foundation

enum TimingEdge {
    case start, end
}

/// Satır düzenleme işlemleri. Hepsi `sentences` üzerinde çalışır ve geri alınabilir.
extension SubtitleViewModel {
    var selectedSentence: Sentence? {
        guard let uid = selection else { return nil }
        return sentences.first { $0.uid == uid }
    }

    var canMergeSelected: Bool {
        guard let uid = selection, let index = sentences.index(ofUID: uid) else { return false }
        return index + 1 < sentences.count
    }

    // MARK: - Metin taslağı

    /// Satırın gösterilecek metni: odaklı satırda henüz commit edilmemiş taslak.
    func draftText(for sentence: Sentence) -> String {
        guard let draft, draft.uid == sentence.uid else { return sentence.text }
        return draft.text
    }

    func beginEditing(_ sentence: Sentence) {
        guard draft?.uid != sentence.uid else { return }
        commitDraft()
        guard let current = sentences.first(where: { $0.uid == sentence.uid }) else { return }
        draft = TextDraft(uid: current.uid, text: current.text)
    }

    func updateDraft(_ text: String, for uid: UUID) {
        if let draft, draft.uid != uid {
            commitDraft()
        }
        if var draft, draft.uid == uid {
            guard draft.text != text else { return }
            draft.text = text
            self.draft = draft
        } else if sentences.contains(where: { $0.uid == uid }) {
            draft = TextDraft(uid: uid, text: text)
        }
    }

    /// Taslağı satıra yazar. Yapısal işlemlerden ve geri al/yinele öncesinde çağrılır.
    /// Geri alma sürerken yığını bozmamak için kayıt oluşturmadan yazar.
    func commitDraft() {
        guard let draft else { return }
        self.draft = nil
        guard let index = sentences.index(ofUID: draft.uid) else { return }
        let text = draft.text.replacingOccurrences(of: "\n", with: " ")
        if undoManager?.isUndoing == true || undoManager?.isRedoing == true {
            sentences[index].text = text
        } else {
            commit("Metni Değiştir") { $0.setText(text, at: index) }
        }
    }

    // MARK: - İşlemler

    func setText(_ text: String, for uid: UUID) {
        guard let index = sentences.index(ofUID: uid) else { return }
        commit("Metni Değiştir") { $0.setText(text, at: index) }
    }

    func nudge(_ edge: TimingEdge, by delta: Double) {
        commitDraft()
        guard let uid = selection, let index = sentences.index(ofUID: uid) else { return }
        commit("Zamanlamayı Değiştir") { list in
            edge == .start ? list.nudgeStart(at: index, by: delta) : list.nudgeEnd(at: index, by: delta)
        }
    }

    func setSelected(_ edge: TimingEdge, to time: Double) {
        commitDraft()
        guard let uid = selection, let index = sentences.index(ofUID: uid) else { return }
        commit("Zamanlamayı Değiştir") { list in
            edge == .start ? list.setStart(time, at: index) : list.setEnd(time, at: index)
        }
    }

    func mergeSelectedWithNext() {
        commitDraft()
        guard let uid = selection, let index = sentences.index(ofUID: uid),
              index + 1 < sentences.count
        else { return }
        let affected: Set<UUID> = [sentences[index].uid, sentences[index + 1].uid]
        commit("Satırları Birleştir", clearingTranslations: affected) { $0.mergeWithNext(at: index) }
    }

    func deleteSelected() {
        commitDraft()
        guard let uid = selection, let index = sentences.index(ofUID: uid) else { return }
        guard commit("Satırı Sil", { $0.deleteSentence(at: index) }) else { return }
        selection = sentences.isEmpty ? nil : sentences[min(index, sentences.count - 1)].uid
    }

    func canSplitSelected(at time: Double) -> Bool {
        guard let uid = selection, let index = sentences.index(ofUID: uid) else { return false }
        return sentences.canSplit(at: index, at: time)
    }

    func splitSelected(at time: Double) {
        commitDraft()
        guard let uid = selection, let index = sentences.index(ofUID: uid) else { return }
        // İlk parça kimliğini korur ama metni değişir; iki parçanın çevirisi de geçersizdir.
        commit("Satırı Böl", clearingTranslations: [uid]) { $0.splitSentence(at: index, at: time) }
    }

    @discardableResult
    private func commit(
        _ name: String,
        clearingTranslations uids: Set<UUID> = [],
        _ change: (inout [Sentence]) -> Bool
    ) -> Bool {
        var updated = sentences
        guard change(&updated) else { return false }
        replaceSentences(updated, actionName: name, clearingTranslations: uids)
        return true
    }

    /// `sentences`'ı değiştirir ve eski değeri geri alma yığınına kaydeder (geri alma sırasında
    /// bu çağrı yineleme kaydını da oluşturur). Silinen/birleşen satırların çevirileri temizlenir;
    /// `translations` verilirse (geri alma/yinele) o sözlük olduğu gibi geri yüklenir.
    private func replaceSentences(
        _ updated: [Sentence],
        actionName: String,
        translations restoredTranslations: [UUID: TranslationEntry]? = nil,
        clearingTranslations clearedUIDs: Set<UUID> = []
    ) {
        let previous = sentences
        let previousTranslations = translations
        // Dışarıdan gelen değişiklik (geri alma/yinele) odaklı taslağı geçersiz kılabilir.
        if let draft, updated.first(where: { $0.uid == draft.uid })?.text != draft.text {
            self.draft = nil
        }
        sentences = updated
        let validUIDs = Set(updated.map(\.uid))
        translations = restoredTranslations
            ?? translations.filter { validUIDs.contains($0.key) && !clearedUIDs.contains($0.key) }
        if let translationDraft, !validUIDs.contains(translationDraft.uid)
            || clearedUIDs.contains(translationDraft.uid) {
            self.translationDraft = nil
        }
        if let uid = selection, !validUIDs.contains(uid) { selection = nil }
        guard let undoManager else { return }
        undoManager.registerUndo(withTarget: self) { model in
            MainActor.assumeIsolated {
                model.replaceSentences(previous, actionName: actionName, translations: previousTranslations)
            }
        }
        undoManager.setActionName(actionName)
    }
}
