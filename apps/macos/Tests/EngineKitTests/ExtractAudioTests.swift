import Foundation
import Testing
@testable import EngineKit

/// `extract-audio --json`: the audio of a video without transcribing it.
@Suite struct ExtractAudioTests {
    @Test func argumentsNameTheVideoAndTheOutputFolder() {
        let args = EngineClient.extractAudioArguments(video: URL(filePath: "/tmp/a b.mp4"), outDir: URL(filePath: "/tmp/out"))
        #expect(args == ["extract-audio", "/tmp/a b.mp4", "--out-dir", "/tmp/out", "--json"])
    }

    @Test func decodesTheResultEventWithoutSentences() throws {
        let line = #"{"event": "result", "language": "?", "duration": 0.0, "audio": "/tmp/out/demo.wav", "outputs": [], "sentences": []}"#
        guard case .result(let result) = try EngineEvent.decode(line) else { Issue.record("expected a result"); return }
        #expect(result.audio == "/tmp/out/demo.wav")
        #expect(result.language == "?")
        #expect(result.duration == 0)
        #expect(result.outputs.isEmpty)
        #expect(result.sentences.isEmpty)
    }

    @Test func streamsStagesThenTheAudioPath() async throws {
        let argsFile = FileManager.default.temporaryDirectory.appending(path: "args-\(UUID().uuidString)")
        let engine = try fakeEngine("""
        echo "$@" > '\(argsFile.path)'
        echo '{"event": "stage", "stage": "extracting_audio", "fraction": null}'
        echo '{"event": "stage", "stage": "extracting_audio", "fraction": 1.0}'
        echo '{"event": "result", "language": "?", "duration": 0.0, "audio": "/tmp/out/v.wav", "outputs": [], "sentences": []}'
        """)
        var events: [EngineEvent] = []
        for try await event in engine.extractAudio(video: URL(filePath: "/v.mp4"), outDir: URL(filePath: "/tmp/out")) {
            events.append(event)
        }
        #expect(events.count == 3)
        #expect(events.first == .stage(.extractingAudio, fraction: nil))
        #expect(events[1] == .stage(.extractingAudio, fraction: 1.0))
        guard case .result(let result) = events[2] else { Issue.record("expected a result"); return }
        #expect(result.audio == "/tmp/out/v.wav")
        let arguments = try String(contentsOf: argsFile, encoding: .utf8)
        #expect(arguments.contains("extract-audio /v.mp4 --out-dir /tmp/out --json"))
    }

    @Test func surfacesTheEnginesError() async throws {
        let engine = try fakeEngine("""
        echo '{"event": "error", "message": "Video bulunamadı"}'
        exit 1
        """)
        await #expect(throws: EngineError.failed("Video bulunamadı")) {
            for try await _ in engine.extractAudio(video: URL(filePath: "/missing.mp4"), outDir: URL(filePath: "/tmp/out")) {}
        }
    }
}
