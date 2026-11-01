import Foundation
import Observation

/// The two UI languages the app ships. Raw values are also what `SUBTITLE_ENGINE_LANG` expects.
public enum UILanguage: String, Sendable, CaseIterable, Equatable {
    case tr
    case en

    public var locale: Locale { Locale(identifier: rawValue) }

    /// Picks the language for "Sistem": Turkish when the first system language is Turkish, English otherwise.
    /// `preferred` is the macOS preferred-language list (not the app's own override).
    public static func system(preferred: [String] = systemPreferredLanguages()) -> UILanguage {
        // Same matching AppKit uses to pick a bundle localization; English when nothing matches.
        let match = Bundle.preferredLocalizations(from: ["tr", "en"], forPreferences: preferred).first
        return match.flatMap(UILanguage.init(rawValue:)) ?? .en
    }

    /// The system-wide preferred languages, read from the global domain so that the app's own
    /// `AppleLanguages` override (written for the next launch) doesn't hide the real system setting.
    public static func systemPreferredLanguages() -> [String] {
        let global = UserDefaults.standard.persistentDomain(forName: UserDefaults.globalDomain)
        return (global?["AppleLanguages"] as? [String]) ?? Locale.preferredLanguages
    }

    /// Looks `key` up in the `<lang>.lproj` sub-bundle of `bundle` (a SwiftPM resource bundle) and formats it.
    /// Keys are the Turkish source strings, so a missing translation falls back to Turkish text.
    public func string(_ key: String, in bundle: Bundle, _ args: [CVarArg] = []) -> String {
        let lookup: Bundle
        if let path = bundle.path(forResource: rawValue, ofType: "lproj"), let found = Bundle(path: path) {
            lookup = found
        } else {
            lookup = bundle
        }
        let format = lookup.localizedString(forKey: key, value: key, table: nil)
        guard !args.isEmpty else { return format }
        return String(format: format, locale: locale, arguments: args)
    }
}

/// Process-wide current UI language. Reading `language` inside a SwiftUI body registers an Observation
/// dependency, so every view that builds text with it refreshes when the language changes.
public final class LanguageState: Observable, @unchecked Sendable {
    public static let shared = LanguageState()

    private let registrar = ObservationRegistrar()
    private let storage = NSLock()
    private var value: UILanguage = .tr

    public var language: UILanguage {
        get {
            registrar.access(self, keyPath: \.language)
            storage.lock()
            defer { storage.unlock() }
            return value
        }
        set {
            registrar.withMutation(of: self, keyPath: \.language) {
                storage.lock()
                value = newValue
                storage.unlock()
            }
        }
    }
}

/// EngineKit's own user-facing strings (Resources/Localizable.xcstrings).
func engineString(_ key: String, language: UILanguage? = nil, _ args: CVarArg...) -> String {
    (language ?? LanguageState.shared.language).string(key, in: .module, args)
}
