import EngineKit
import Foundation

enum TimingEdge {
    case start, end
}

/// Line editing operations. All of them work on `sentences` and can be undone.
extension SubtitleViewModel {
    var selectedSentence: Sentence? {
        guard let uid = selection else { return nil }
        return sentences.first { $0.uid == uid }
    }

    var canMergeSelected: Bool {
        guard hasSingleSelection, let uid = selection, let index = sentences.index(ofUID: uid) else { return false }
        return index + 1 < sentences.count
    }

    // MARK: - Text draft

    /// The text to display for a line: for the focused line, the draft that hasn't been committed yet.
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

    /// Writes the draft to the line. Called before structural operations and before undo/redo.
    /// While undoing, it writes without creating a record so the stack isn't corrupted.
    func commitDraft() {
        guard let draft else { return }
        self.draft = nil
        guard let index = sentences.index(ofUID: draft.uid) else { return }
        let text = draft.text.replacingOccurrences(of: "\n", with: " ")
        if undoManager?.isUndoing == true || undoManager?.isRedoing == true {
            sentences[index].text = text
        } else {
            commit(L("Metni Değiştir")) { $0.setText(text, at: index) }
        }
    }

    // MARK: - Operations

    func setText(_ text: String, for uid: UUID) {
        guard let index = sentences.index(ofUID: uid) else { return }
        commit(L("Metni Değiştir")) { $0.setText(text, at: index) }
    }

    func nudge(_ edge: TimingEdge, by delta: Double) {
        commitDraft()
        guard hasSingleSelection, let uid = selection, let index = sentences.index(ofUID: uid) else { return }
        commit(L("Zamanlamayı Değiştir")) { list in
            edge == .start ? list.nudgeStart(at: index, by: delta) : list.nudgeEnd(at: index, by: delta)
        }
    }

    func setSelected(_ edge: TimingEdge, to time: Double) {
        commitDraft()
        guard hasSingleSelection, let uid = selection, let index = sentences.index(ofUID: uid) else { return }
        commit(L("Zamanlamayı Değiştir")) { list in
            edge == .start ? list.setStart(time, at: index) : list.setEnd(time, at: index)
        }
    }

    func mergeSelectedWithNext() {
        commitDraft()
        guard hasSingleSelection, let uid = selection, let index = sentences.index(ofUID: uid),
              index + 1 < sentences.count
        else { return }
        let affected: Set<UUID> = [sentences[index].uid, sentences[index + 1].uid]
        commit(L("Satırları Birleştir"), clearingTranslations: affected) { $0.mergeWithNext(at: index) }
    }

    /// Sets a line's speaker (zero-based index); `nil` clears it.
    func setSpeaker(_ speaker: Int?, for uid: UUID) {
        commitDraft()
        guard let index = sentences.index(ofUID: uid) else { return }
        commit(L("Konuşmacıyı Değiştir")) { $0.setSpeaker(speaker, at: index) }
    }

    /// Assigns an unused speaker index to the line ("Yeni konuşmacı").
    func assignNewSpeaker(for uid: UUID) {
        setSpeaker(sentences.nextNewSpeaker, for: uid)
    }

    /// Deletes every selected line in one undo step; the line after the removed block (or the last one) becomes selected.
    func deleteSelected() {
        commitDraft()
        let indices = sentences.indices.filter { selectedUIDs.contains(sentences[$0].uid) }
        guard let first = indices.first else { return }
        let name = indices.count > 1 ? L("Satırları Sil") : L("Satırı Sil")
        guard commit(name, { list in
            var changed = false
            // From the back so the earlier indices stay valid.
            for index in indices.reversed() where list.deleteSentence(at: index) { changed = true }
            return changed
        }) else { return }
        selection = sentences.isEmpty ? nil : sentences[min(first, sentences.count - 1)].uid
    }

    func canSplitSelected(at time: Double) -> Bool {
        guard hasSingleSelection, let uid = selection, let index = sentences.index(ofUID: uid) else { return false }
        return sentences.canSplit(at: index, at: time)
    }

    func splitSelected(at time: Double) {
        commitDraft()
        guard hasSingleSelection, let uid = selection, let index = sentences.index(ofUID: uid) else { return }
        // The first part keeps its identity but its text changes; the translation of both parts is invalid.
        commit(L("Satırı Böl"), clearingTranslations: [uid]) { $0.splitSentence(at: index, at: time) }
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

    /// Changes `sentences` and records the old value on the undo stack (during undo,
    /// this call also creates the redo record). Translations of deleted/merged lines are cleared;
    /// if `translations` is given (undo/redo), that dictionary is restored as is.
    private func replaceSentences(
        _ updated: [Sentence],
        actionName: String,
        translations restoredTranslations: [UUID: TranslationEntry]? = nil,
        clearingTranslations clearedUIDs: Set<UUID> = []
    ) {
        let previous = sentences
        let previousTranslations = translations
        // A change from outside (undo/redo) can invalidate the focused draft.
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
        guard let undoManager else { return }
        undoManager.registerUndo(withTarget: self) { model in
            MainActor.assumeIsolated {
                model.replaceSentences(previous, actionName: actionName, translations: previousTranslations)
            }
        }
        undoManager.setActionName(actionName)
    }
}
