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
        let format: String
        if let path = bundle.path(forResource: rawValue, ofType: "lproj"), let found = Bundle(path: path) {
            format = found.localizedString(forKey: key, value: key, table: nil)
        } else {
            // SwiftPM's native build system (Xcode ≤ 26) copies the catalog uncompiled; read it directly.
            format = RawStringCatalog.shared.value(for: key, language: rawValue, in: bundle) ?? key
        }
        guard !args.isEmpty else { return format }
        return String(format: format, locale: locale, arguments: args)
    }
}

/// Reads an uncompiled `Localizable.xcstrings` (JSON) from a resource bundle, cached per bundle.
final class RawStringCatalog: @unchecked Sendable {
    static let shared = RawStringCatalog()

    private let lock = NSLock()
    /// bundle path → key → language → value
    private var catalogs: [String: [String: [String: String]]] = [:]

    func value(for key: String, language: String, in bundle: Bundle) -> String? {
        lock.lock()
        defer { lock.unlock() }
        if catalogs[bundle.bundlePath] == nil {
            catalogs[bundle.bundlePath] = Self.load(bundle)
        }
        return catalogs[bundle.bundlePath]?[key]?[language]
    }

    private static func load(_ bundle: Bundle) -> [String: [String: String]] {
        guard let url = bundle.url(forResource: "Localizable", withExtension: "xcstrings"),
              let data = try? Data(contentsOf: url),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let strings = root["strings"] as? [String: Any]
        else { return [:] }
        var table: [String: [String: String]] = [:]
        for (key, entry) in strings {
            guard let localizations = (entry as? [String: Any])?["localizations"] as? [String: Any] else { continue }
            for (language, localization) in localizations {
                if let unit = (localization as? [String: Any])?["stringUnit"] as? [String: Any],
                   let value = unit["value"] as? String {
                    table[key, default: [:]][language] = value
                }
            }
        }
        return table
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
