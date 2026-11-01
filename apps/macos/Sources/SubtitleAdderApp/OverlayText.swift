import EngineKit
import Foundation

/// Composes the text drawn over the video, including the dialogue dash.
enum OverlayText {
    /// The dash rule is evaluated on the lines actually shown (original, translation, or both, per line):
    /// it follows the speaker turn rule and is skipped when the shown text already starts with a dash.
    static func compose(
        sentences: [Sentence],
        translations: [UUID: TranslationEntry],
        display: SubtitleDisplay,
        uid: UUID
    ) -> String? {
        guard let index = sentences.index(ofUID: uid) else { return nil }
        let dash = sentences.startsDialogueTurn(at: index)
        let original = sentences[index].text.trimmingCharacters(in: .whitespacesAndNewlines)
        let translated = translations[uid]?.text.trimmingCharacters(in: .whitespacesAndNewlines)
        let lines: [String]
        switch display {
        case .original:
            lines = [original]
        case .translation:
            lines = [translated?.isEmpty == false ? translated ?? "" : original]
        case .both:
            lines = [original, translated ?? ""]
        }
        let shown = lines.filter { !$0.isEmpty }.map { DialogueDash.apply(dash, to: $0) }
        return shown.isEmpty ? nil : shown.joined(separator: "\n")
    }
}
