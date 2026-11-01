import Foundation
import Testing
@testable import EngineKit

/// Offline mode and the external LLM reach the engine only through its environment, set on every launch.
@Suite struct LaunchSettingsTests {
    private let engine = EngineClient(command: EngineCommand(executable: URL(filePath: "/x")), language: .tr)

    private func environment(_ settings: EngineLaunchSettings, base: [String: String] = [:]) -> [String: String] {
        engine.processEnvironment(language: .tr, settings: settings, base: base)
    }

    @Test func offlineFlagIsAlwaysSet() {
        #expect(environment(.init(offline: true))["SUBTITLE_ENGINE_OFFLINE"] == "1")
        #expect(environment(.init(offline: false))["SUBTITLE_ENGINE_OFFLINE"] == "0")
        #expect(environment(.init(offline: true))["SUBTITLE_ENGINE_LANG"] == "tr")
    }

    @Test func externalLLMIsNormalizedAndModelTrimmed() {
        let env = environment(.init(externalLLM: ExternalLLM(url: " 192.168.1.50:1234/v1/chat/completions/ ", model: "  gemma4:e4b ")))
        #expect(env["SUBTITLE_ENGINE_EXTERNAL_LLM_URL"] == "http://192.168.1.50:1234")
        #expect(env["SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL"] == "gemma4:e4b")
    }

    @Test func emptyModelIsStillSet() {
        let env = environment(.init(externalLLM: ExternalLLM(url: "http://mac.local:11434")))
        #expect(env["SUBTITLE_ENGINE_EXTERNAL_LLM_URL"] == "http://mac.local:11434")
        #expect(env["SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL"] == "")
    }

    @Test func inheritedExternalVariablesAreRemovedWhenInactive() {
        let inherited = [
            "SUBTITLE_ENGINE_EXTERNAL_LLM_URL": "http://old:1", "SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL": "old", "PATH": "/bin",
        ]
        for settings in [EngineLaunchSettings(), EngineLaunchSettings(externalLLM: ExternalLLM(url: "   "))] {
            let env = environment(settings, base: inherited)
            #expect(env["SUBTITLE_ENGINE_EXTERNAL_LLM_URL"] == nil)
            #expect(env["SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL"] == nil)
            #expect(env["PATH"] == "/bin")
        }
    }

    @Test func explicitSettingsOverrideSharedState() {
        let explicit = EngineLaunchSettings(offline: true)
        var client = engine
        #expect(client.resolvedLaunchSettings(current: .init(offline: false)) == .init(offline: false))
        client.launchSettings = explicit
        #expect(client.resolvedLaunchSettings(current: .init(offline: false)) == explicit)
    }

    @Test func launchedProcessReceivesTheVariables() async throws {
        var client = try fakeEngine("""
        cat > /dev/null
        echo "{\\"event\\": \\"error\\", \\"message\\": \\"$SUBTITLE_ENGINE_OFFLINE|$SUBTITLE_ENGINE_EXTERNAL_LLM_URL|$SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL\\"}"
        exit 1
        """)
        client.language = .en
        client.launchSettings = .init(offline: true, externalLLM: ExternalLLM(url: "10.0.0.2:11434", model: "m"))
        await #expect(throws: EngineError.failed("1|http://10.0.0.2:11434|m")) {
            for try await _ in client.translate([], to: "en") {}
        }
    }
}
