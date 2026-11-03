import AppKit
import SwiftUI

/// UserDefaults / @AppStorage keys for the appearance settings.
enum AppearanceKeys {
    static let theme = "appTheme"
    static let overlayFont = "overlayFontFamily"
    static let overlaySize = "overlayFontSize"
    static let overlayTextColor = "overlayTextColor"
    static let overlayBackground = "overlayBackgroundStyle"
    static let overlayOpacity = "overlayBackgroundOpacity"
    /// Font family of the app's own interface ("" = system); independent of the subtitle overlay font above.
    static let interfaceFont = "appFontFamily"

    /// The keys the overlay "reset" button restores; the theme and the interface font are not among them.
    static let overlayKeys = [overlayFont, overlaySize, overlayTextColor, overlayBackground, overlayOpacity]
}

/// App theme choice. "system" follows macOS.
enum ThemeChoice: String, CaseIterable, Identifiable, Sendable {
    case system, light, dark

    var id: String { rawValue }

    var title: String {
        switch self {
        case .system: L("Sistem")
        case .light: L("Açık")
        case .dark: L("Koyu")
        }
    }

    /// Value for `NSApp.appearance`; `nil` follows the system. Applies to every window, Settings included.
    var appearanceName: NSAppearance.Name? {
        switch self {
        case .system: nil
        case .light: .aqua
        case .dark: .darkAqua
        }
    }

    @MainActor func apply() {
        NSApp?.appearance = appearanceName.flatMap(NSAppearance.init(named:))
    }
}

enum OverlayBackground: String, CaseIterable, Identifiable, Sendable {
    /// Rounded box behind the text.
    case box
    /// No box; a soft shadow keeps the text readable.
    case shadow
    /// No box; a thin black contour around the glyphs.
    case outline

    var id: String { rawValue }

    var title: String {
        switch self {
        case .box: L("Kutu")
        case .shadow: L("Gölge")
        case .outline: L("Kontur")
        }
    }
}

/// How the subtitle over the video looks. Only affects the in-app preview, never exported files.
struct OverlayStyle: Equatable, Sendable {
    static let sizeRange: ClosedRange<Double> = 0.03...0.09
    static let defaultSize = 0.05
    static let defaultTextHex = "#FFFFFF"
    static let defaultOpacity = 0.6

    /// Empty means the system font.
    var fontFamily: String
    /// Font size as a fraction of the video height.
    var sizeFraction: Double
    var textHex: String
    var background: OverlayBackground
    var backgroundOpacity: Double

    static let standard = OverlayStyle(
        fontFamily: "", sizeFraction: defaultSize, textHex: defaultTextHex,
        background: .box, backgroundOpacity: defaultOpacity
    )

    /// Builds a style from raw stored values; out-of-range or malformed values fall back to safe ones.
    init(fontFamily: String, sizeFraction: Double, textHex: String, background: OverlayBackground, backgroundOpacity: Double) {
        self.fontFamily = fontFamily
        self.sizeFraction = sizeFraction.isFinite
            ? min(max(sizeFraction, Self.sizeRange.lowerBound), Self.sizeRange.upperBound) : Self.defaultSize
        self.textHex = HexColor.parse(textHex) == nil ? Self.defaultTextHex : textHex
        self.background = background
        self.backgroundOpacity = backgroundOpacity.isFinite ? min(max(backgroundOpacity, 0), 1) : Self.defaultOpacity
    }

    init(storedFamily: String, size: Double, textHex: String, background: String, opacity: Double) {
        self.init(
            fontFamily: storedFamily, sizeFraction: size, textHex: textHex,
            background: OverlayBackground(rawValue: background) ?? .box, backgroundOpacity: opacity
        )
    }

    func fontSize(videoHeight: CGFloat) -> CGFloat { max(12, videoHeight * sizeFraction) }

    /// Radius of the contour in the outline style, in points: grows with the text but stays between 1 and 2.5 so it
    /// neither vanishes on small video nor turns heavy at full screen.
    func outlineRadius(videoHeight: CGFloat) -> CGFloat {
        min(max(fontSize(videoHeight: videoHeight) * 0.045, 1.0), 2.5)
    }

    /// The eight offsets of the contour copies: up, down, left and right by `radius`, the diagonals by `radius / √2`
    /// so all eight lie on one circle.
    static func outlineOffsets(radius: CGFloat) -> [CGSize] {
        let d = radius / 2.0.squareRoot()
        return [
            CGSize(width: -radius, height: 0), CGSize(width: radius, height: 0),
            CGSize(width: 0, height: -radius), CGSize(width: 0, height: radius),
            CGSize(width: -d, height: -d), CGSize(width: d, height: -d),
            CGSize(width: -d, height: d), CGSize(width: d, height: d),
        ]
    }

    /// Restores the overlay settings in `defaults`. Leaves the theme and the interface font alone.
    static func resetStored(in defaults: UserDefaults = .standard) {
        defaults.set("", forKey: AppearanceKeys.overlayFont)
        defaults.set(defaultSize, forKey: AppearanceKeys.overlaySize)
        defaults.set(defaultTextHex, forKey: AppearanceKeys.overlayTextColor)
        defaults.set(OverlayBackground.box.rawValue, forKey: AppearanceKeys.overlayBackground)
        defaults.set(defaultOpacity, forKey: AppearanceKeys.overlayOpacity)
    }

    /// Semibold font of the chosen family. Resolved through NSFontManager so family names (not only
    /// PostScript names) work and the nearest available weight is used.
    func font(size: CGFloat) -> Font {
        guard !fontFamily.isEmpty,
              let nsFont = NSFontManager.shared.font(withFamily: fontFamily, traits: [], weight: 7, size: size)
        else { return .system(size: size, weight: .semibold) }
        return Font(nsFont as CTFont)
    }

    var textColor: Color { HexColor.color(textHex) ?? .white }
}

/// Applies the theme to `NSApp` independent of any window (including when only Settings, or no window, is open).
@MainActor
final class AppearanceController {
    static let shared = AppearanceController()
    private var observer: NSObjectProtocol?
    private var applied: String?

    func start() {
        apply()
        guard observer == nil else { return }
        observer = NotificationCenter.default.addObserver(
            forName: UserDefaults.didChangeNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.apply() }
        }
    }

    private func apply() {
        let value = UserDefaults.standard.string(forKey: AppearanceKeys.theme) ?? ThemeChoice.system.rawValue
        guard value != applied else { return }
        applied = value
        (ThemeChoice(rawValue: value) ?? .system).apply()
    }
}

/// "#RRGGBB" conversions for the stored overlay text color.
enum HexColor {
    static func parse(_ hex: String) -> (r: Double, g: Double, b: Double)? {
        var text = hex.trimmingCharacters(in: .whitespaces)
        if text.hasPrefix("#") { text.removeFirst() }
        guard text.count == 6, let value = UInt32(text, radix: 16) else { return nil }
        return (Double((value >> 16) & 0xFF) / 255, Double((value >> 8) & 0xFF) / 255, Double(value & 0xFF) / 255)
    }

    static func format(r: Double, g: Double, b: Double) -> String {
        func byte(_ v: Double) -> Int { Int((min(max(v, 0), 1) * 255).rounded()) }
        return String(format: "#%02X%02X%02X", byte(r), byte(g), byte(b))
    }

    static func color(_ hex: String) -> Color? {
        parse(hex).map { Color(.sRGB, red: $0.r, green: $0.g, blue: $0.b) }
    }

    static func hex(from color: Color) -> String {
        let ns = NSColor(color).usingColorSpace(.sRGB) ?? .white
        return format(r: Double(ns.redComponent), g: Double(ns.greenComponent), b: Double(ns.blueComponent))
    }
}

/// The subtitle text as drawn over the video (and in the Settings preview).
struct SubtitleOverlayLabel: View {
    let text: String
    let style: OverlayStyle
    let videoHeight: CGFloat

    var body: some View {
        let label = Text(text)
            .font(style.font(size: style.fontSize(videoHeight: videoHeight)))
            .foregroundStyle(style.textColor)
            .multilineTextAlignment(.center)
            .lineLimit(3)
        switch style.background {
        case .box:
            label
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(.black.opacity(style.backgroundOpacity), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        case .shadow:
            label
                .shadow(color: .black.opacity(max(style.backgroundOpacity, 0.5)), radius: 3, x: 0, y: 1)
                .shadow(color: .black.opacity(0.8), radius: 1, x: 0, y: 0)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
        case .outline:
            // Eight black copies around the coloured text; offsets do not change the layout size.
            let offsets = OverlayStyle.outlineOffsets(radius: style.outlineRadius(videoHeight: videoHeight))
            label
                .background {
                    ZStack {
                        ForEach(offsets.indices, id: \.self) { index in
                            label
                                .foregroundStyle(.black)
                                .offset(offsets[index])
                        }
                    }
                    .accessibilityHidden(true)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
        }
    }
}

/// Applies the chosen interface font to a window's content. Read live, so changing it in Settings takes effect at
/// once. The subtitle overlay sets its own font and is never affected.
struct InterfaceFontModifier: ViewModifier {
    @AppStorage(AppearanceKeys.interfaceFont) private var family = ""

    func body(content: Content) -> some View {
        content.environment(\.font, InterfaceFont.font(family: family))
    }
}

extension View {
    /// Uses the interface font from Settings for text that has no explicit font.
    func interfaceFont() -> some View { modifier(InterfaceFontModifier()) }
}

enum InterfaceFont {
    /// The font to apply for a stored family: `nil` (the system font) when it is empty or not installed, so a font
    /// that was removed since it was chosen does not break the layout. Sized like the system body text.
    static func font(family: String, installed: [String]? = nil) -> Font? {
        let name = family.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, (installed ?? NSFontManager.shared.availableFontFamilies).contains(name) else { return nil }
        return .custom(name, size: NSFont.systemFontSize, relativeTo: .body)
    }
}

/// Reads the stored overlay settings; a view that declares it refreshes when they change.
struct OverlayStyleStorage: DynamicProperty {
    @AppStorage(AppearanceKeys.overlayFont) var family = ""
    @AppStorage(AppearanceKeys.overlaySize) var size = OverlayStyle.defaultSize
    @AppStorage(AppearanceKeys.overlayTextColor) var textHex = OverlayStyle.defaultTextHex
    @AppStorage(AppearanceKeys.overlayBackground) var background = OverlayBackground.box.rawValue
    @AppStorage(AppearanceKeys.overlayOpacity) var opacity = OverlayStyle.defaultOpacity

    var style: OverlayStyle {
        OverlayStyle(storedFamily: family, size: size, textHex: textHex, background: background, opacity: opacity)
    }
}
