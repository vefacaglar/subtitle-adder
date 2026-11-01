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
