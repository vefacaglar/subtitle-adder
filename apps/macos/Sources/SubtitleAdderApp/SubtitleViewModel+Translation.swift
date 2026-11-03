import EngineKit
import Foundation

/// Translation flow: sends the lines to the engine and maps editable translation texts by `uid`.
extension SubtitleViewModel {
    /// The line as exported with a translation: a non-empty `text` replaces the original (the times are kept) and
    /// carries the translation's `needsReview`, replacing a flag the line itself had; without text the line keeps its
    /// own text and flag. Word timestamps never go with a translated text.
    nonisolated static func exportLine(_ line: Sentence, text: String?, needsReview: Bool) -> Sentence {
        var copy = line
        if let text, !text.isEmpty {
            copy.text = text
            copy.needsReview = needsReview
        }
        copy.words = []
        return copy
    }

    /// Whether an existing translation can stay instead of being sent again: the original wasn't edited since, it is in
    /// `target`, it has text, and it is not just the source repeated (a model that gave up returns its input).
    /// Sources shorter than four characters (names, "OK") may legitimately stay the same.
    nonisolated static func isReusableTranslation(_ sentence: Sentence, entry: TranslationEntry, target: String) -> Bool {
        guard !entry.needsReview, !entry.isStale(for: sentence), entry.language == target else { return false }
        let translated = entry.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !translated.isEmpty else { return false }
        let source = sentence.text.trimmingCharacters(in: .whitespacesAndNewlines)
        return !(source.count >= 4 && translated.caseInsensitiveCompare(source) == .orderedSame)
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
    /// With `preserveOtherLanguages` the kept translations stay even when they are in another language.
    /// `reviewFallback` is what the lines had before the whole job (see `VideoSession.storeTranslations`). `session` is
    /// the video the translation is for (the one on screen when not given).
    func applyTranslation(
        _ translated: [Sentence], language: String, source: [Sentence], merge: Bool = false,
        preserveOtherLanguages: Bool = false, reviewFallback: [UUID: TranslationEntry]? = nil,
        to session: VideoSession? = nil
    ) {
        storeTranslations(
            Self.pairs(translated, with: source), language: language, merge: merge,
            preserveOtherLanguages: preserveOtherLanguages, reviewFallback: reviewFallback, in: session
        )
    }

    /// Each translated line with the line it translates, matched by id only. A result whose id isn't in `source` (a
    /// context neighbour, or something the engine invented) is dropped; it must never land on a line by position.
    static func pairs(_ translated: [Sentence], with source: [Sentence]) -> [(result: Sentence, original: Sentence)] {
        var byID: [Int: Sentence] = [:]
        for sentence in source where byID[sentence.id] == nil { byID[sentence.id] = sentence }
        return translated.compactMap { result in byID[result.id].map { (result, $0) } }
    }

    /// Stores translated lines in `session` (the one on screen when not given); see `VideoSession.storeTranslations`.
    /// The toolbar's target follows the language of what arrived, but only when the video is on screen: a translation
    /// that lands in a video in the background doesn't change what the user has chosen.
    func storeTranslations(
        _ pairs: [(result: Sentence, original: Sentence)], language: String, merge: Bool,
        preserveOtherLanguages: Bool = false, reviewFallback: [UUID: TranslationEntry]? = nil,
        in session: VideoSession? = nil
    ) {
        let session = session ?? foreground
        session.storeTranslations(
            pairs, language: language, merge: merge, preserveOtherLanguages: preserveOtherLanguages,
            reviewFallback: reviewFallback
        )
        if updatesToolbar(for: session) { translationTarget = language }
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
        // An edited line is the user's own text now.
        entry.needsReview = false
        var updated = translations
        updated[uid] = entry
        replaceTranslations(updated, actionName: L("Çeviriyi Değiştir"))
    }

    /// Puts an undo record on the stack that restores `baseline` without changing the translations now; undoing it
    /// records the redo, like any other translation change.
    func registerTranslationUndo(on session: VideoSession, restoring baseline: [UUID: TranslationEntry], actionName: String) {
        guard let undoManager else { return }
        undoManager.registerUndo(withTarget: session) { [weak self] session in
            MainActor.assumeIsolated { self?.replaceTranslations(baseline, in: session, actionName: actionName) }
        }
        undoManager.setActionName(actionName)
    }

    private func replaceTranslations(
        _ updated: [UUID: TranslationEntry], in session: VideoSession? = nil, actionName: String
    ) {
        let session = session ?? foreground
        let previous = session.translations
        session.hasUnsavedEdits = true
        session.translations = updated
        guard let undoManager else { return }
        undoManager.registerUndo(withTarget: session) { [weak self] session in
            MainActor.assumeIsolated { self?.replaceTranslations(previous, in: session, actionName: actionName) }
        }
        undoManager.setActionName(actionName)
    }
}
