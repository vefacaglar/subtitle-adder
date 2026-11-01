import EngineKit
import Foundation

/// What to show in the subtitle over the video; the choice is persistent.
enum SubtitleDisplay: String, CaseIterable, Identifiable, Sendable {
    case original
    case translation
    case both

    var id: String { rawValue }

    var title: String {
        switch self {
        case .original: L("Orijinal")
        case .translation: L("Çeviri")
        case .both: L("İkisi")
        }
    }
}

/// A line's translation. `sourceText` is the original text at the time it was translated; if the original
/// changes later, the translation counts as "eski" (stale) and is marked in the UI.
struct TranslationEntry: Equatable, Sendable {
    var text: String
    var sourceText: String
    var language: String

    func isStale(for sentence: Sentence) -> Bool {
        sourceText != sentence.text
    }
}
