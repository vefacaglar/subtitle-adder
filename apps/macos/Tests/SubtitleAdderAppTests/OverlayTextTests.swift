import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

@Suite struct OverlayTextTests {
    private let list = [
        Sentence(id: 1, start: 0, end: 1, text: "Hello.", speaker: 0),
        Sentence(id: 2, start: 1, end: 2, text: "More.", speaker: 0),
        Sentence(id: 3, start: 2, end: 3, text: "- Hi.", speaker: 1),
    ]

    private func translations() -> [UUID: TranslationEntry] {
        Dictionary(uniqueKeysWithValues: list.map {
            ($0.uid, TranslationEntry(text: "T:" + $0.text, sourceText: $0.text, language: "es"))
        })
    }

    private func text(_ index: Int, _ display: SubtitleDisplay) -> String? {
        OverlayText.compose(sentences: list, translations: translations(), display: display, uid: list[index].uid)
    }

    @Test func dashFollowsTurnRuleOnOriginal() {
        #expect(text(0, .original) == "- Hello.")
        #expect(text(1, .original) == "More.")
    }

    @Test func alreadyDashedShownTextGetsNoSecondDash() {
        #expect(text(2, .original) == "- Hi.")
        // The translation text has no dash of its own, so it does get one.
        #expect(text(2, .translation) == "- T:- Hi.")
    }

    @Test func bothModeDashesEachLine() {
        #expect(text(0, .both) == "- Hello.\n- T:Hello.")
        #expect(text(1, .both) == "More.\nT:More.")
    }

    @Test func translationFallsBackToOriginalWithoutEntry() {
        let out = OverlayText.compose(sentences: list, translations: [:], display: .translation, uid: list[0].uid)
        #expect(out == "- Hello.")
    }

    @Test func noDashWithoutSpeakers() {
        let plain = [Sentence(id: 1, start: 0, end: 1, text: "A")]
        #expect(OverlayText.compose(sentences: plain, translations: [:], display: .original, uid: plain[0].uid) == "A")
    }
}
