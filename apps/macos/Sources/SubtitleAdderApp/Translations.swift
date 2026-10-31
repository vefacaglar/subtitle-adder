import EngineKit
import Foundation

/// Video üstündeki altyazıda neyin gösterileceği; seçim kalıcıdır.
enum SubtitleDisplay: String, CaseIterable, Identifiable, Sendable {
    case original
    case translation
    case both

    var id: String { rawValue }

    var title: String {
        switch self {
        case .original: "Orijinal"
        case .translation: "Çeviri"
        case .both: "İkisi"
        }
    }
}

/// Bir satırın çevirisi. `sourceText`, çevrildiği andaki orijinal metindir; orijinal
/// sonradan değişirse çeviri "eski" sayılır ve arayüzde işaretlenir.
struct TranslationEntry: Equatable, Sendable {
    var text: String
    var sourceText: String
    var language: String

    func isStale(for sentence: Sentence) -> Bool {
        sourceText != sentence.text
    }
}
