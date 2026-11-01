import EngineKit
import Foundation

/// What to show in the subtitle over the video; the choice is persistent.
enum SubtitleDisplay: String, CaseIterable, Identifiable, Sendable {
    case original
    case translation
    case both

    var id: String { rawValue }

    var title: String { title(translationLanguage: nil) }

    /// With a known translation language the translation reads "Çeviri (es)".
    func title(translationLanguage: String?) -> String {
        switch self {
        case .original: L("Orijinal")
        case .translation: Self.translationTitle(language: translationLanguage)
        case .both: L("İkisi")
        }
    }

    /// "Çeviri (es)" once translated, otherwise "Çeviri".
    static func translationTitle(language: String?) -> String {
        language.map { L("Çeviri (%@)", $0) } ?? L("Çeviri")
    }
}

/// Which text the exported subtitle file carries.
enum ExportLanguage: String, CaseIterable, Identifiable, Sendable {
    case original, translated
    var id: String { rawValue }
}

/// A line's translation. `sourceText` is the original text at the time it was translated; if the original
/// changes later, the translation counts as "eski" (stale) and is marked in the UI.
struct TranslationEntry: Equatable, Sendable {
    var text: String
    var sourceText: String
    var language: String
    /// The engine could not translate the line (`text` is the source or an older translation); it is shown with a
    /// marker until the text is edited, and the line is sent again by any "reuse" path.
    var needsReview = false

    func isStale(for sentence: Sentence) -> Bool {
        sourceText != sentence.text
    }
}
