import EngineKit
import Foundation

enum TimingEdge {
    case start, end
}

/// Satır düzenleme işlemleri. Hepsi `sentences` üzerinde çalışır ve geri alınabilir.
extension SubtitleViewModel {
    var selectedSentence: Sentence? {
        selection.flatMap { id in sentences.first { $0.id == id } }
    }

    var canMergeSelected: Bool {
        guard let id = selection, let index = sentences.index(ofID: id) else { return false }
        return index + 1 < sentences.count
    }

    func setText(_ text: String, for id: Int) {
        guard let index = sentences.index(ofID: id) else { return }
        commit("Metni Değiştir") { $0.setText(text, at: index) }
    }

    func nudge(_ edge: TimingEdge, by delta: Double) {
        guard let id = selection, let index = sentences.index(ofID: id) else { return }
        commit("Zamanlamayı Değiştir") { list in
            edge == .start ? list.nudgeStart(at: index, by: delta) : list.nudgeEnd(at: index, by: delta)
        }
    }

    func setSelected(_ edge: TimingEdge, to time: Double) {
        guard let id = selection, let index = sentences.index(ofID: id) else { return }
        commit("Zamanlamayı Değiştir") { list in
            edge == .start ? list.setStart(time, at: index) : list.setEnd(time, at: index)
        }
    }

    func mergeSelectedWithNext() {
        guard let id = selection, let index = sentences.index(ofID: id) else { return }
        commit("Satırları Birleştir") { $0.mergeWithNext(at: index) }
    }

    func deleteSelected() {
        guard let id = selection, let index = sentences.index(ofID: id) else { return }
        commit("Satırı Sil") { $0.deleteSentence(at: index) }
        selection = sentences.isEmpty ? nil : sentences[min(index, sentences.count - 1)].id
    }

    private func commit(_ name: String, _ change: (inout [Sentence]) -> Bool) {
        var updated = sentences
        guard change(&updated) else { return }
        replaceSentences(updated, actionName: name)
    }

    /// `sentences`'ı değiştirir ve eski değeri geri alma yığınına kaydeder (geri alma sırasında
    /// bu çağrı yineleme kaydını da oluşturur).
    private func replaceSentences(_ updated: [Sentence], actionName: String) {
        let previous = sentences
        sentences = updated
        if let id = selection, !updated.contains(where: { $0.id == id }) { selection = nil }
        guard let undoManager else { return }
        undoManager.registerUndo(withTarget: self) { model in
            MainActor.assumeIsolated { model.replaceSentences(previous, actionName: actionName) }
        }
        undoManager.setActionName(actionName)
    }
}
