import EngineKit
import Foundation
import SwiftUI
import Testing
@testable import SubtitleAdderApp

@Suite struct LocalizationAndAppearanceTests {
    @Test func appCatalogHasEnglishAndTurkish() {
        #expect(UILanguage.en.string("Altyazı üret", in: .module) == "Generate Subtitles")
        #expect(UILanguage.tr.string("Altyazı üret", in: .module) == "Altyazı üret")
        #expect(UILanguage.en.string("%lld satır · dil: %@", in: .module, [Int(3), "tr"]) == "Lines: 3 · language: tr")
        #expect(UILanguage.en.string("%@ için %@ seçildi.", in: .module, ["Turkish", "Fast"]) == "Selected Fast for Turkish.")
        #expect(UILanguage.en.string("Çevriliyor… %lld%%", in: .module, [Int(42)]) == "Translating… 42%")
        #expect(UILanguage.tr.string("Çevriliyor… %lld%%", in: .module, [Int(42)]) == "Çevriliyor… %42")
        // A key missing from the catalog falls back to the Turkish source text.
        #expect(UILanguage.en.string("Katalogda yok", in: .module) == "Katalogda yok")
    }

    @Test func languageChoiceResolution() {
        #expect(LanguageChoice.turkish.resolved(preferred: ["en-US"]) == .tr)
        #expect(LanguageChoice.english.resolved(preferred: ["tr-TR"]) == .en)
        #expect(LanguageChoice.system.resolved(preferred: ["tr-TR"]) == .tr)
        #expect(LanguageChoice.system.resolved(preferred: ["fr-FR"]) == .en)
    }

    @Test func themeMapping() {
        #expect(ThemeChoice.system.appearanceName == nil)
        #expect(ThemeChoice.light.appearanceName == .aqua)
        #expect(ThemeChoice.dark.appearanceName == .darkAqua)
    }

    @Test func overlayStyleClampsStoredValues() {
        let style = OverlayStyle(storedFamily: "Menlo", size: 5, textHex: "zzz", background: "nope", opacity: -1)
        #expect(style.sizeFraction == OverlayStyle.sizeRange.upperBound)
        #expect(style.textHex == OverlayStyle.defaultTextHex)
        #expect(style.background == .box)
        #expect(style.backgroundOpacity == 0)
        #expect(OverlayStyle(storedFamily: "", size: .nan, textHex: "#000000", background: "shadow", opacity: 7).sizeFraction
            == OverlayStyle.defaultSize)
        #expect(OverlayStyle(storedFamily: "", size: 0, textHex: "#000000", background: "shadow", opacity: 7).backgroundOpacity == 1)
    }

    @Test func outlineStyleRoundTripsAndUnknownValuesFallBackToBox() {
        #expect(OverlayBackground(rawValue: "outline") == .outline)
        #expect(OverlayBackground.allCases.map(\.rawValue) == ["box", "shadow", "outline"])
        let outline = OverlayStyle(storedFamily: "", size: 0.05, textHex: "#FFFFFF", background: "outline", opacity: 0.6)
        #expect(outline.background == .outline)
        #expect(OverlayStyle(storedFamily: "", size: 0.05, textHex: "#FFFFFF", background: "Outline", opacity: 0.6).background == .box)
        #expect(OverlayStyle(storedFamily: "", size: 0.05, textHex: "#FFFFFF", background: "", opacity: 0.6).background == .box)
        #expect(OverlayBackground.outline.title == "Kontur" || OverlayBackground.outline.title == "Outline")
    }

    @Test func outlineRadiusIsClampedBetweenOneAndTwoAndAHalfPoints() {
        func radius(fraction: Double, height: CGFloat) -> CGFloat {
            OverlayStyle(storedFamily: "", size: fraction, textHex: "#FFFFFF", background: "outline", opacity: 1)
                .outlineRadius(videoHeight: height)
        }
        // Minimum font size is 12 pt: 12 * 0.045 = 0.54, raised to 1.
        #expect(radius(fraction: 0.05, height: 100) == 1)
        // 50 pt font: 2.25, inside the range.
        #expect(abs(radius(fraction: 0.05, height: 1000) - 2.25) < 0.0001)
        // 0.09 * 2000 = 180 pt font: 8.1, capped at 2.5.
        #expect(radius(fraction: 0.09, height: 2000) == 2.5)
    }

    @Test func outlineOffsetsSitOnOneCircleOfTheGivenRadius() {
        let offsets = OverlayStyle.outlineOffsets(radius: 2)
        #expect(offsets.count == 8)
        for offset in offsets {
            #expect(abs((offset.width * offset.width + offset.height * offset.height).squareRoot() - 2) < 0.0001)
        }
        #expect(Set(offsets.map { "\($0.width),\($0.height)" }).count == 8)
        #expect(offsets.contains(CGSize(width: -2, height: 0)) && offsets.contains(CGSize(width: 0, height: 2)))
    }

    private func isolatedDefaults() throws -> UserDefaults {
        let suite = "SubtitleAdderTests.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defaults.removePersistentDomain(forName: suite)
        return defaults
    }

    @Test func resettingTheOverlayLeavesTheInterfaceFontAndThemeAlone() throws {
        let defaults = try isolatedDefaults()
        defaults.set("Menlo", forKey: AppearanceKeys.interfaceFont)
        defaults.set("dark", forKey: AppearanceKeys.theme)
        defaults.set("Georgia", forKey: AppearanceKeys.overlayFont)
        defaults.set("outline", forKey: AppearanceKeys.overlayBackground)
        defaults.set(0.09, forKey: AppearanceKeys.overlaySize)

        OverlayStyle.resetStored(in: defaults)

        #expect(defaults.string(forKey: AppearanceKeys.interfaceFont) == "Menlo")
        #expect(defaults.string(forKey: AppearanceKeys.theme) == "dark")
        #expect(defaults.string(forKey: AppearanceKeys.overlayFont) == "")
        #expect(defaults.string(forKey: AppearanceKeys.overlayBackground) == "box")
        #expect(defaults.double(forKey: AppearanceKeys.overlaySize) == OverlayStyle.defaultSize)
        // The two fonts have separate keys.
        #expect(AppearanceKeys.interfaceFont == "appFontFamily")
        #expect(!AppearanceKeys.overlayKeys.contains(AppearanceKeys.interfaceFont))
    }

    @Test func interfaceFontOnlyAppliesToInstalledFamilies() {
        #expect(InterfaceFont.font(family: "") == nil)
        #expect(InterfaceFont.font(family: "  ") == nil)
        #expect(InterfaceFont.font(family: "No Such Family", installed: ["Menlo"]) == nil)
        #expect(InterfaceFont.font(family: "Menlo", installed: ["Menlo"]) != nil)
    }

    @Test func overlayFontSize() {
        let style = OverlayStyle.standard
        #expect(style.fontSize(videoHeight: 1000) == 50)
        #expect(style.fontSize(videoHeight: 100) == 12)  // minimum readable size
    }

    @Test func hexColorRoundTrip() throws {
        let parsed = try #require(HexColor.parse("#FF8000"))
        #expect(parsed.r == 1)
        #expect(abs(parsed.g - 128.0 / 255) < 0.0001)
        #expect(HexColor.format(r: parsed.r, g: parsed.g, b: parsed.b) == "#FF8000")
        #expect(HexColor.parse("12345") == nil)
        #expect(HexColor.parse("#GGGGGG") == nil)
        #expect(HexColor.parse("00ff00")?.g == 1)
    }
}
