import Foundation
import Testing
@testable import EngineKit

@Suite struct DiagnosticLogTests {
    private func tempLog(maxBytes: Int = DiagnosticLog.defaultMaxBytes, secrets: [String] = []) -> DiagnosticLog {
        let dir = FileManager.default.temporaryDirectory.appending(path: "diag-\(UUID().uuidString)", directoryHint: .isDirectory)
        return DiagnosticLog(fileURL: dir.appending(path: "diagnostics.log"), maxBytes: maxBytes, secrets: { secrets })
    }

    private func contents(_ url: URL) -> String {
        (try? String(contentsOf: url, encoding: .utf8)) ?? ""
    }

    @Test func writesTimestampedLinesAndCreatesTheFolder() {
        let log = tempLog()
        log.write("translation", "batch failed")
        log.write("translation", "second\nline  with\r\nbreaks")
        log.flush()
        let lines = contents(log.fileURL).split(separator: "\n").map(String.init)
        #expect(lines.count == 2)
        #expect(lines[0].hasSuffix("[translation] batch failed"))
        #expect(lines[1].hasSuffix("[translation] second line  with breaks"))
        // ISO 8601 timestamp first.
        #expect(lines[0].prefix(4).allSatisfy(\.isNumber) && lines[0].contains("T"))
    }

    @Test func rotatesToOldPastTheLimitAndKeepsOnlyOneOldFile() {
        let log = tempLog(maxBytes: 300)
        for index in 0..<12 {
            log.write("t", "line \(index) " + String(repeating: "x", count: 40))
            log.flush()
        }
        let current = contents(log.fileURL)
        let old = contents(log.rotatedURL)
        #expect(!old.isEmpty)
        #expect(log.rotatedURL.lastPathComponent == "diagnostics.log.old")
        // The current file stays near the limit instead of growing without bound.
        #expect(current.utf8.count < 300 + 120)
        #expect(current.contains("line 11 "))
        #expect(old.contains("line 0 ") == false || old.utf8.count < 300 + 120)
        // No file grew past limit plus one line, and nothing but the two files exists.
        let names = (try? FileManager.default.contentsOfDirectory(atPath: log.fileURL.deletingLastPathComponent().path)) ?? []
        #expect(Set(names) == ["diagnostics.log", "diagnostics.log.old"])
    }

    @Test func aConfiguredKeyIsNeverWrittenVerbatim() {
        let log = tempLog(secrets: ["sk-configured-key-123"])
        log.write("translation", "HTTP 401: invalid key sk-configured-key-123 for the account")
        log.write("translation", "failed", redacting: ["launch-key-456"])
        log.write("translation", "echo launch-key-456 and Authorization: Bearer abcdef.ghi_jkl", redacting: ["launch-key-456"])
        log.writeEngineLines([#"[translation] {"event": "http", "note": "sk-configured-key-123"}"#])
        log.flush()
        let text = contents(log.fileURL)
        #expect(!text.contains("sk-configured-key-123"))
        #expect(!text.contains("launch-key-456"))
        #expect(!text.contains("abcdef.ghi_jkl"))
        #expect(text.contains("HTTP 401: invalid key [redacted] for the account"))
        #expect(text.contains("Bearer [redacted]"))
        #expect(text.contains(#"[translation] {"event": "http", "note": "[redacted]"}"#))
    }

    @Test func blankSecretsDoNotEraseEverything() {
        let log = tempLog(secrets: ["", "   "])
        log.write("t", "plain message")
        log.flush()
        #expect(contents(log.fileURL).hasSuffix("[t] plain message\n"))
    }

    @Test func aVeryLongMessageIsCut() {
        let log = tempLog()
        log.write("t", String(repeating: "a", count: 10_000))
        log.flush()
        #expect(contents(log.fileURL).count < DiagnosticLog.maxMessageLength + 100)
    }

    @Test func extractsOnlyTranslationLinesFromEngineStderr() throws {
        let url = FileManager.default.temporaryDirectory.appending(path: "stderr-\(UUID().uuidString).log")
        defer { try? FileManager.default.removeItem(at: url) }
        let stderr = """
        Downloading model: 40%\r[translation] {"event": "request", "n": 1}
        some warning
        [translation] {"event": "retry", "n": 2}
         [translation] not at the start
        """
        try stderr.write(to: url, atomically: true, encoding: .utf8)
        #expect(DiagnosticLog.translationLines(inLogAt: url) == [
            #"[translation] {"event": "request", "n": 1}"#, #"[translation] {"event": "retry", "n": 2}"#,
        ])
        #expect(DiagnosticLog.translationLines(inLogAt: url.appendingPathExtension("missing")).isEmpty)
    }

    @Test func aTranslationRunCopiesItsTraceLinesBeforeTheStderrFileIsDeleted() async throws {
        let marker = "trace-\(UUID().uuidString)"
        let engine = try fakeEngine("""
        if [ "$1" = translate ]; then
          cat > /dev/null
          echo 'progress noise' >&2
          echo '[translation] {"event": "request", "id": "\(marker)"}' >&2
          echo '{"event": "translation", "language": "en", "sentences": []}'
        fi
        """)
        for try await _ in engine.translate([Sentence(id: 1, start: 0, end: 1, text: "Merhaba.")], to: "en") {}
        // The stream finishes just before the run is cleaned up, so wait for the copy to arrive.
        var text = ""
        for _ in 0..<100 {
            DiagnosticLog.shared.flush()
            text = contents(DiagnosticLog.shared.fileURL)
            if text.contains(marker) { break }
            try await Task.sleep(for: .milliseconds(50))
        }
        #expect(text.contains(#"[engine] [translation] {"event": "request", "id": "\#(marker)"}"#))
        #expect(!text.contains("progress noise"))
        // Subtitle text is not part of the log.
        #expect(!text.contains("Merhaba."))
        // Under the test runner the shared log is not the developer's real one.
        #expect(DiagnosticLog.shared.fileURL != DiagnosticLog.defaultFileURL)
    }

    @Test func otherCommandsDoNotCopyStderr() async throws {
        let marker = "noise-\(UUID().uuidString)"
        let engine = try fakeEngine("""
        echo '[translation] {"id": "\(marker)"}' >&2
        echo '{"event": "result", "language": "tr", "duration": 1, "audio": "a", "outputs": [], "sentences": []}'
        """)
        for try await _ in engine.transcribe(video: URL(filePath: "/v.mp4"), outDir: URL(filePath: "/tmp")) {}
        try await Task.sleep(for: .milliseconds(300))
        DiagnosticLog.shared.flush()
        #expect(!contents(DiagnosticLog.shared.fileURL).contains(marker))
    }

    @Test func theDefaultLocationIsTheAppSupportFolder() {
        let path = DiagnosticLog.defaultFileURL.path
        #expect(path.hasSuffix("/Library/Application Support/SubtitleAdder/diagnostics.log"))
    }
}
