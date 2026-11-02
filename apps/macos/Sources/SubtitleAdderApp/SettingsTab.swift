import Foundation

/// The tabs of the Settings window. The selection is kept in `UserDefaults` so other windows can open Settings on a
/// given tab (the toolbar's "API sağlayıcılarını yönet…" opens it on `.translation`). Every tab of the `TabView` needs
/// a `.tag` with its case here.
enum SettingsTab: String, CaseIterable {
    case general, translation, appearance, models

    static let storageKey = "settingsTab"

    /// The stored choice, `.general` when nothing (or something unknown) was stored.
    static func stored(_ raw: String) -> SettingsTab { SettingsTab(rawValue: raw) ?? .general }
}
