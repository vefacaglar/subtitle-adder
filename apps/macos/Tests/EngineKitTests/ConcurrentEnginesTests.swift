import Foundation
import Testing
@testable import EngineKit

/// Several engines run at once (audio extraction, recognition and translation of different videos): an engine that has
/// nothing to say must not hold back what the others print. The engines sleep far longer than the time bound, so a
/// loaded machine (parallel builds) cannot make a correct reader look late, while one that waits for exit still fails.
@Suite struct ConcurrentEnginesTests {
    private let resultLine = #"{"event": "result", "language": "tr", "duration": 1, "audio": "a", "outputs": [], "sentences": []}"#

    @Test func aQuietEngineDoesNotHoldUpAnother() async throws {
        let quiet = try fakeEngine("""
        echo '{"event": "stage", "stage": "loading_model", "fraction": null}'
        exec sleep 20
        """)
        let busy = try fakeEngine("echo '\(resultLine)'")
        let quietRun = Task {
            for try await _ in quiet.transcribe(video: URL(filePath: "/v.mp4"), outDir: URL(filePath: "/tmp")) {}
        }
        // The quiet engine is now waiting for its next line.
        try await Task.sleep(for: .milliseconds(500))

        let started = ContinuousClock.now
        var results = 0
        for try await event in busy.extractAudio(video: URL(filePath: "/w.mp4"), outDir: URL(filePath: "/tmp")) {
            if case .result = event { results += 1 }
        }
        let rendered = try await busy.render([], as: .srt)
        #expect(results == 1)
        #expect(rendered.contains("result"))
        #expect(ContinuousClock.now - started < .seconds(10))
        quietRun.cancel()
    }

    @Test func linesAreHandedOverAsSoonAsTheyAreWritten() async throws {
        let engine = try fakeEngine("""
        echo '{"event": "stage", "stage": "extracting_audio", "fraction": 0.5}'
        exec sleep 20
        """)
        var iterator = engine.extractAudio(video: URL(filePath: "/v.mp4"), outDir: URL(filePath: "/tmp")).makeAsyncIterator()
        let started = ContinuousClock.now
        let first = try await iterator.next()
        #expect(first == .stage(.extractingAudio, fraction: 0.5))
        #expect(ContinuousClock.now - started < .seconds(10))
    }

    @Test func aLastLineWithoutNewlineAndWindowsLineEndingsAreRead() async throws {
        let engine = try fakeEngine("""
        printf '{"event": "stage", "stage": "extracting_audio", "fraction": 1.0}\\r\\n'
        printf '\(resultLine)'
        """)
        var events: [EngineEvent] = []
        for try await event in engine.extractAudio(video: URL(filePath: "/v.mp4"), outDir: URL(filePath: "/tmp")) {
            events.append(event)
        }
        #expect(events.count == 2)
        #expect(events.first == .stage(.extractingAudio, fraction: 1.0))
    }
}
