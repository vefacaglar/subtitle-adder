import EngineKit
import Foundation

/// The language picker in the toolbar. An empty code means automatic detection.
enum SubtitleLanguage: String, CaseIterable, Identifiable, Sendable {
    case automatic = ""
    case turkish = "tr"
    case portuguese = "pt"
    case spanish = "es"
    case russian = "ru"
    case english = "en"

    var id: String { rawValue }

    /// Name shown in the menu.
    var title: String {
        switch self {
        case .automatic: L("Otomatik")
        case .turkish: L("Türkçe (tr)")
        case .portuguese: L("Portekizce – Brezilya (pt)")
        case .spanish: L("İspanyolca (es)")
        case .russian: L("Rusça (ru)")
        case .english: L("İngilizce (en)")
        }
    }

    /// Name for the short info in the status bar.
    var shortName: String {
        switch self {
        case .automatic: L("Otomatik algılama")
        case .turkish: L("Türkçe")
        case .portuguese: L("Portekizce")
        case .spanish: L("İspanyolca")
        case .russian: L("Rusça")
        case .english: L("İngilizce")
        }
    }

    /// Languages that can be a translation target; "Otomatik" can't be a target.
    static var translationTargets: [SubtitleLanguage] {
        allCases.filter { $0 != .automatic }
    }

    /// Name in the target language menu (without the code).
    var targetName: String {
        switch self {
        case .automatic: L("Otomatik")
        case .turkish: L("Türkçe")
        case .portuguese: L("Portekizce – Brezilya")
        case .spanish: L("İspanyolca")
        case .russian: L("Rusça")
        case .english: L("İngilizce")
        }
    }
}

/// Localized names of the model ids and the texts of the catalog rows.
/// Labels are kept only here; the toolbar picker and Settings use the same text.
enum ModelLabel {
    static func name(for id: String) -> String {
        switch id {
        case "base": L("Hızlı")
        case "small": L("Dengeli")
        case "medium": L("Doğru")
        case "large-v3-turbo": L("Çok doğru (turbo)")
        case "large-v3": L("En doğru")
        case "parakeet-v3": L("Parakeet v3 (hızlı, çok dilli)")
        case "canary-1b-v2": L("Canary (çok dilli)")
        case "gigaam-v3": L("GigaAM (Rusça)")
        case "nllb-1.3b": L("NLLB-200 1.3B")
        case "madlad-3b": L("MADLAD-400 3B")
        case "speaker-diarization": L("Konuşmacı ayırma")
        default: id
        }
    }

    static func menuTitle(for model: EngineModel) -> String {
        "\(name(for: model.id)) · \(detail(for: model))"
    }

    static func detail(for model: EngineModel) -> String {
        model.downloaded ? L("hazır") : L("ilk kullanımda ~%@ indirilir", sizeText(model.sizeMB))
    }

    /// Short language summary: "Tüm diller" or "bg, hr, cs…".
    static func languageSummary(for model: EngineModel) -> String {
        if model.isDiarization { return L("Dilden bağımsız") }
        guard let languages = model.languages else { return L("Tüm diller") }
        let shown = languages.prefix(3).joined(separator: ", ")
        return languages.count > 3 ? "\(shown)…" : shown
    }

    static func storageStatus(for model: EngineModel) -> String {
        if model.downloaded {
            L("Yüklü — diskte %@", sizeText(model.diskMB ?? model.sizeMB))
        } else {
            L("Yüklü değil — ~%@ indirilir", sizeText(model.sizeMB))
        }
    }

    /// License text in the Settings row.
    static func licenseText(for model: EngineModel) -> String {
        L("Lisans: %@", model.license ?? L("bilinmiyor"))
    }

    static func sizeText(_ sizeMB: Int) -> String {
        guard sizeMB >= 1000 else { return "\(sizeMB) MB" }
        // The decimal separator follows the UI language (Turkish comma, English point).
        let separator = LanguageState.shared.language == .tr ? "," : "."
        var text = String(format: "%.1f", Double(sizeMB) / 1000)
            .replacingOccurrences(of: ".", with: separator)
        if text.hasSuffix("\(separator)0") { text = String(text.dropLast(2)) }
        return "\(text) GB"
    }
}
