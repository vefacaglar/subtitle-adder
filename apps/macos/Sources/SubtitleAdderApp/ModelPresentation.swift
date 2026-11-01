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
        case .automatic: "Otomatik"
        case .turkish: "Türkçe (tr)"
        case .portuguese: "Portekizce – Brezilya (pt)"
        case .spanish: "İspanyolca (es)"
        case .russian: "Rusça (ru)"
        case .english: "İngilizce (en)"
        }
    }

    /// Name for the short info in the status bar.
    var shortName: String {
        switch self {
        case .automatic: "Otomatik algılama"
        case .turkish: "Türkçe"
        case .portuguese: "Portekizce"
        case .spanish: "İspanyolca"
        case .russian: "Rusça"
        case .english: "İngilizce"
        }
    }

    /// Languages that can be a translation target; "Otomatik" can't be a target.
    static var translationTargets: [SubtitleLanguage] {
        allCases.filter { $0 != .automatic }
    }

    /// Name in the target language menu (without the code).
    var targetName: String {
        switch self {
        case .automatic: "Otomatik"
        case .turkish: "Türkçe"
        case .portuguese: "Portekizce – Brezilya"
        case .spanish: "İspanyolca"
        case .russian: "Rusça"
        case .english: "İngilizce"
        }
    }
}

/// Turkish names of the model ids and the texts of the catalog rows.
/// Labels are kept only here; the toolbar picker and Settings use the same text.
enum ModelLabel {
    static func name(for id: String) -> String {
        switch id {
        case "base": "Hızlı"
        case "small": "Dengeli"
        case "medium": "Doğru"
        case "large-v3-turbo": "Çok doğru (turbo)"
        case "large-v3": "En doğru"
        case "parakeet-v3": "Parakeet v3 (hızlı, çok dilli)"
        case "canary-1b-v2": "Canary (çok dilli)"
        case "gigaam-v3": "GigaAM (Rusça)"
        case "nllb-1.3b": "NLLB-200 1.3B"
        case "madlad-3b": "MADLAD-400 3B"
        case "speaker-diarization": "Konuşmacı ayırma"
        default: id
        }
    }

    static func menuTitle(for model: EngineModel) -> String {
        "\(name(for: model.id)) · \(detail(for: model))"
    }

    static func detail(for model: EngineModel) -> String {
        model.downloaded ? "hazır" : "ilk kullanımda ~\(sizeText(model.sizeMB)) indirilir"
    }

    /// Short language summary: "Tüm diller" or "bg, hr, cs…".
    static func languageSummary(for model: EngineModel) -> String {
        if model.isDiarization { return "Dilden bağımsız" }
        guard let languages = model.languages else { return "Tüm diller" }
        let shown = languages.prefix(3).joined(separator: ", ")
        return languages.count > 3 ? "\(shown)…" : shown
    }

    static func storageStatus(for model: EngineModel) -> String {
        if model.downloaded {
            "Yüklü — diskte \(sizeText(model.diskMB ?? model.sizeMB))"
        } else {
            "Yüklü değil — ~\(sizeText(model.sizeMB)) indirilir"
        }
    }

    /// License text in the Settings row.
    static func licenseText(for model: EngineModel) -> String {
        "Lisans: \(model.license ?? "bilinmiyor")"
    }

    static func sizeText(_ sizeMB: Int) -> String {
        guard sizeMB >= 1000 else { return "\(sizeMB) MB" }
        var text = String(format: "%.1f", Double(sizeMB) / 1000)
            .replacingOccurrences(of: ".", with: ",")
        if text.hasSuffix(",0") { text = String(text.dropLast(2)) }
        return "\(text) GB"
    }
}
