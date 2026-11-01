import EngineKit
import Foundation
import Observation
import SwiftUI

/// Localized string from this target's catalog (Resources/Localizable.xcstrings). Keys are the Turkish source
/// strings. The language comes from `LanguageState`, so calling this inside a view body (or anything a body
/// calls) makes the view refresh when the user switches language in Settings.
func L(_ key: String, _ args: CVarArg...) -> String {
    LanguageState.shared.language.string(key, in: .module, args)
}

/// The language choice shown in Settings.
enum LanguageChoice: String, CaseIterable, Identifiable, Sendable {
    case system, turkish = "tr", english = "en"

    var id: String { rawValue }

    /// Picker label. Language names are shown in their own language except "Sistem".
    var title: String {
        switch self {
        case .system: L("Sistem")
        case .turkish: "Türkçe"
        case .english: "English"
        }
    }

    func resolved(preferred: [String] = UILanguage.systemPreferredLanguages()) -> UILanguage {
        switch self {
        case .system: UILanguage.system(preferred: preferred)
        case .turkish: .tr
        case .english: .en
        }
    }
}

/// Holds the persisted language choice and applies it: updates `LanguageState` (immediate, in-process:
/// strings, engine env) and `AppleLanguages` (so system menus/panels follow on the next launch).
@MainActor
@Observable
final class AppLanguage {
    static let defaultsKey = "uiLanguage"

    var choice: LanguageChoice {
        didSet {
            guard choice != oldValue else { return }
            defaults.set(choice.rawValue, forKey: Self.defaultsKey)
            apply()
        }
    }

    /// Language the process started with; used to tell whether system UI still needs a relaunch.
    @ObservationIgnored let launchLanguage: UILanguage
    /// System preferred languages, read once (and when the system locale changes), not on every body pass.
    @ObservationIgnored private var systemPreferred: [String]
    @ObservationIgnored private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        let saved = LanguageChoice(rawValue: defaults.string(forKey: Self.defaultsKey) ?? "") ?? .system
        let preferred = UILanguage.systemPreferredLanguages()
        systemPreferred = preferred
        choice = saved
        launchLanguage = saved.resolved(preferred: preferred)
        LanguageState.shared.language = launchLanguage
    }

    var effective: UILanguage { choice.resolved(preferred: systemPreferred) }

    /// System menus and standard panels only pick the new language on the next launch.
    var needsRelaunch: Bool { effective != launchLanguage }

    private func apply() {
        systemPreferred = UILanguage.systemPreferredLanguages()
        let language = effective
        LanguageState.shared.language = language
        if choice == .system {
            defaults.removeObject(forKey: "AppleLanguages")
        } else {
            defaults.set([language.rawValue], forKey: "AppleLanguages")
        }
    }
}

/// A message produced by the app that is localized when it is displayed, not when it is created,
/// so stored status/error text follows a language switch. Equality compares `id` only.
struct LocalizedMessage: Equatable, Sendable {
    let id: String
    private let build: @Sendable () -> String

    /// `id` must identify the content (key plus arguments) so equal ids mean equal messages.
    init(_ id: String, _ build: @escaping @Sendable () -> String) {
        self.id = id
        self.build = build
    }

    /// App-generated text with a catalog key and no arguments.
    static func key(_ key: String) -> LocalizedMessage {
        LocalizedMessage("key:\(key)") { L(key) }
    }

    /// Text that is already final (for example from the engine).
    static func verbatim(_ text: String) -> LocalizedMessage {
        LocalizedMessage("verbatim:\(text)") { text }
    }

    /// An error shown with its own description; `EngineError.notFound` is re-localized on each display.
    static func error(_ error: any Error) -> LocalizedMessage {
        LocalizedMessage("error:\(error)") { error.localizedDescription }
    }

    var text: String { build() }

    static func == (lhs: LocalizedMessage, rhs: LocalizedMessage) -> Bool { lhs.id == rhs.id }
}
