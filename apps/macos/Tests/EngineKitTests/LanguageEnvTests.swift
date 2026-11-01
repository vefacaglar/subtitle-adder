import Foundation
import Testing
@testable import EngineKit

/// Every engine launch must carry `SUBTITLE_ENGINE_LANG` so engine error messages follow the UI language.
@Suite struct LanguageEnvTests {
    /// The fake engine reports the env var it received as its error message (or as stdout for `render`).
    private func echoEngine(language: UILanguage?) throws -> EngineClient {
        var engine = try fakeEngine("""
        cat > /dev/null
        echo "{\\"event\\": \\"error\\", \\"message\\": \\"lang=$SUBTITLE_ENGINE_LANG\\"}"
        exit 1
        """)
        engine.language = language
        return engine
    }

    private let sample = [Sentence(id: 1, start: 0, end: 1, text: "A")]

    private func expected(_ language: UILanguage) -> EngineError { .failed("lang=\(language.rawValue)") }

    @Test(arguments: [UILanguage.tr, UILanguage.en])
    func transcribePassesLanguage(language: UILanguage) async throws {
        let engine = try echoEngine(language: language)
        await #expect(throws: expected(language)) {
            for try await _ in engine.transcribe(video: URL(filePath: "/v.mp4"), outDir: URL(filePath: "/tmp")) {}
        }
    }

    @Test(arguments: [UILanguage.tr, UILanguage.en])
    func translatePassesLanguage(language: UILanguage) async throws {
        let engine = try echoEngine(language: language)
        await #expect(throws: expected(language)) {
            for try await _ in engine.translate(sample, to: "en") {}
        }
    }

    @Test(arguments: [UILanguage.tr, UILanguage.en])
    func modelsPassesLanguage(language: UILanguage) async throws {
        let engine = try echoEngine(language: language)
        await #expect(throws: expected(language)) { _ = try await engine.models() }
    }

    @Test(arguments: [UILanguage.tr, UILanguage.en])
    func deleteModelPassesLanguage(language: UILanguage) async throws {
        let engine = try echoEngine(language: language)
        await #expect(throws: expected(language)) { _ = try await engine.deleteModel("small") }
    }

    @Test func renderPassesLanguage() async throws {
        var engine = try fakeEngine("cat > /dev/null; echo \"$SUBTITLE_ENGINE_LANG\"")
        engine.language = .en
        #expect(try await engine.render(sample, as: .srt).trimmingCharacters(in: .whitespacesAndNewlines) == "en")
    }

    @Test func environmentKeepsParentVariablesAndOverridesLanguage() throws {
        let engine = EngineClient(command: EngineCommand(executable: URL(filePath: "/x")), language: .en)
        let env = engine.processEnvironment(language: engine.resolvedLanguage(), base: ["PATH": "/bin", "SUBTITLE_ENGINE_LANG": "tr"])
        #expect(env["SUBTITLE_ENGINE_LANG"] == "en")
        #expect(env["PATH"] == "/bin")
    }

    @Test func defaultFollowsCurrentUILanguage() {
        let follow = EngineClient(command: EngineCommand(executable: URL(filePath: "/x")))
        #expect(follow.resolvedLanguage(current: .en) == .en)
        #expect(follow.resolvedLanguage(current: .tr) == .tr)
        let fixed = EngineClient(command: EngineCommand(executable: URL(filePath: "/x")), language: .en)
        #expect(fixed.resolvedLanguage(current: .tr) == .en)
    }

    @Test func systemLanguageFallsBackToEnglishUnlessTurkish() {
        #expect(UILanguage.system(preferred: ["tr-TR", "en-US"]) == .tr)
        #expect(UILanguage.system(preferred: ["tr"]) == .tr)
        // Same matching as AppKit: the first preferred language the app supports wins.
        #expect(UILanguage.system(preferred: ["de-DE", "tr-TR"]) == .tr)
        #expect(UILanguage.system(preferred: ["de-DE", "en-US", "tr-TR"]) == .en)
        #expect(UILanguage.system(preferred: ["pt-BR"]) == .en)
        #expect(UILanguage.system(preferred: []) == .en)
        #expect(UILanguage.system(preferred: ["tr_TR"]) == .tr)
    }

    @Test func engineStringsAreTranslated() {
        #expect(UILanguage.en.string("Motor model listesi yazmadı.", in: .module) == "The engine did not output a model list.")
        #expect(UILanguage.tr.string("Motor model listesi yazmadı.", in: .module) == "Motor model listesi yazmadı.")
        #expect(UILanguage.en.string("Motor hata koduyla çıktı (%lld). %@", in: .module, [Int(3), "x"])
            == "The engine exited with an error code (3). x")
    }
}
