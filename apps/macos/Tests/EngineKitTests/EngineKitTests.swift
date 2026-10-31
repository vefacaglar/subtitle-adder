import Foundation
import Testing
@testable import EngineKit

@Test func decodesStageEvents() throws {
    #expect(try EngineEvent.decode(#"{"event": "stage", "stage": "transcribing", "fraction": 0.5}"#)
        == .stage(.transcribing, fraction: 0.5))
    #expect(try EngineEvent.decode(#"{"event": "stage", "stage": "yeni_asama", "fraction": null}"#)
        == .stage(.unknown, fraction: nil))
}

@Test func decodesDownloadingModelStage() throws {
    #expect(try EngineEvent.decode(#"{"event": "stage", "stage": "downloading_model", "fraction": 0.37}"#)
        == .stage(.downloadingModel, fraction: 0.37))
    #expect(try EngineEvent.decode(#"{"event": "stage", "stage": "translating", "fraction": 0.42}"#)
        == .stage(.translating, fraction: 0.42))
    #expect(EngineEvent.Stage.translating.rawValue == "translating")
}

@Test func listsModelsFromEngine() async throws {
    let engine = try fakeEngine("""
    echo '{"event": "models", "models": [{"id": "base", "backend": "whisper", "languages": null, "size_mb": 145, "disk_mb": 141, "speed": 5, "accuracy": 2, "downloaded": true}, {"id": "gigaam-v3", "backend": "onnx", "languages": ["ru"], "size_mb": 216, "disk_mb": null, "speed": 5, "accuracy": 4, "downloaded": false}]}'
    """)
    let models = try await engine.models()
    #expect(models == [
        EngineModel(id: "base", backend: "whisper", languages: nil, sizeMB: 145, diskMB: 141, speed: 5, accuracy: 2, downloaded: true),
        EngineModel(id: "gigaam-v3", backend: "onnx", languages: ["ru"], sizeMB: 216, diskMB: nil, speed: 5, accuracy: 4, downloaded: false),
    ])
    #expect(models[0].kind == "transcription")
    #expect(models[0].license == nil)
    #expect(!models[0].isTranslation)
    #expect(!models[0].hasNonCommercialLicense)
    #expect(models[0].detectsLanguage)
    #expect(models[0].supports(language: "tr"))
    #expect(!models[1].detectsLanguage)
    #expect(models[1].supports(language: "ru"))
    #expect(!models[1].supports(language: "tr"))
}

@Test func decodesModelsWithoutNewFields() async throws {
    let engine = try fakeEngine("""
    echo '{"event": "models", "models": [{"id": "small", "size_mb": 465, "speed": 4, "accuracy": 3, "downloaded": false}]}'
    """)
    let model = try #require(try await engine.models().first)
    #expect(model.kind == "transcription")
    #expect(model.backend == "whisper")
    #expect(model.languages == nil)
    #expect(model.diskMB == nil)
    #expect(model.license == nil)
}

@Test func decodesTranslationModelFields() async throws {
    let engine = try fakeEngine("""
    echo '{"event": "models", "models": [{"id": "nllb-1.3b", "kind": "translation", "backend": "ctranslate2", "languages": ["tr", "en", "pt", "es", "ru"], "size_mb": 1336, "disk_mb": 1336, "speed": 3, "accuracy": 4, "license": "CC-BY-NC-4.0", "downloaded": true}]}'
    """)
    let model = try #require(try await engine.models().first)
    #expect(model.kind == "translation")
    #expect(model.isTranslation)
    #expect(model.license == "CC-BY-NC-4.0")
    #expect(model.hasNonCommercialLicense)
    #expect(model.supports(language: "es"))
}

@Test func deleteModelReturnsFreedSpace() async throws {
    let argsFile = FileManager.default.temporaryDirectory.appending(path: "args-\(UUID().uuidString)")
    let engine = try fakeEngine("""
    echo "$@" > '\(argsFile.path)'
    echo '{"event": "deleted", "id": "small", "freed_mb": 464}'
    """)
    let freed = try await engine.deleteModel("small")
    #expect(freed == 464)
    let arguments = try String(contentsOf: argsFile, encoding: .utf8)
    #expect(arguments.contains("delete-model small --json"))
}

@Test func deleteModelSurfacesEngineError() async throws {
    let engine = try fakeEngine("""
    echo '{"event": "error", "message": "tiny modeli indirilmemiş; silinecek dosya yok."}'
    exit 1
    """)
    await #expect(throws: EngineError.failed("tiny modeli indirilmemiş; silinecek dosya yok.")) {
        _ = try await engine.deleteModel("tiny")
    }
}

@Test func decodesResultAndError() throws {
    let line = #"{"event": "result", "language": "tr", "duration": 2.0, "audio": "/a.wav", "outputs": [], "sentences": [{"id": 1, "start": 0.0, "end": 1.0, "text": "Merhaba.", "words": [{"start": 0.0, "end": 1.0, "text": "Merhaba."}]}]}"#
    guard case .result(let result) = try EngineEvent.decode(line) else { Issue.record("result bekleniyordu"); return }
    #expect(result.language == "tr")
    #expect(result.sentences.map(\.text) == ["Merhaba."])
    #expect(try EngineEvent.decode(#"{"event": "error", "message": "olmadı"}"#) == .error("olmadı"))
}

/// Sahte motor betiğiyle süreç yönetimini (olay akışı, hata olayı, render stdin) uçtan uca dener.
private func fakeEngine(_ script: String) throws -> EngineClient {
    let url = FileManager.default.temporaryDirectory.appending(path: "fake-engine-\(UUID().uuidString).sh")
    try ("#!/bin/sh\n" + script).write(to: url, atomically: true, encoding: .utf8)
    try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
    return EngineClient(command: EngineCommand(executable: url))
}

@Test func streamsEventsUntilResult() async throws {
    let engine = try fakeEngine("""
    echo '{"event": "stage", "stage": "extracting_audio", "fraction": null}'
    echo 'motor gürültüsü' >&2
    echo '{"event": "result", "language": "tr", "duration": 1, "audio": "a", "outputs": [], "sentences": []}'
    """)
    var events: [EngineEvent] = []
    for try await event in engine.transcribe(video: URL(filePath: "/v.mp4"), outDir: URL(filePath: "/tmp")) {
        events.append(event)
    }
    #expect(events.count == 2)
    #expect(events.first == .stage(.extractingAudio, fraction: nil))
}

@Test func engineErrorEventThrows() async throws {
    let engine = try fakeEngine("""
    echo '{"event": "error", "message": "Video bulunamadı"}'
    exit 1
    """)
    await #expect(throws: EngineError.failed("Video bulunamadı")) {
        for try await _ in engine.transcribe(video: URL(filePath: "/v.mp4"), outDir: URL(filePath: "/tmp")) {}
    }
}

@Test func renderSendsSentencesOnStdin() async throws {
    let engine = try fakeEngine("cat")  // stdin'i aynen geri yazar
    let text = try await engine.render([Sentence(id: 1, start: 0, end: 1, text: "Selam.")], as: .srt)
    #expect(text.contains("\"Selam.\""))
}

@Test func decodesTranslationEvent() throws {
    let line = #"{"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0.0, "end": 1.0, "text": "Hola.", "words": []}]}"#
    let expected = EngineEvent.translation(
        language: "es",
        sentences: [Sentence(id: 1, start: 0, end: 1, text: "Hola.")]
    )
    #expect(try EngineEvent.decode(line) == expected)
}

@Test func translateStreamsEventsAndSendsRequest() async throws {
    let argsFile = FileManager.default.temporaryDirectory.appending(path: "args-\(UUID().uuidString)")
    let inputFile = FileManager.default.temporaryDirectory.appending(path: "input-\(UUID().uuidString)")
    let engine = try fakeEngine("""
    cat > '\(inputFile.path)'
    echo "$@" > '\(argsFile.path)'
    echo '{"event": "stage", "stage": "downloading_model", "fraction": 0.12}'
    echo '{"event": "stage", "stage": "translating", "fraction": 0.4}'
    echo '{"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0.0, "end": 1.0, "text": "Hola.", "words": []}]}'
    """)
    let source = Sentence(
        id: 1, start: 0, end: 1, text: "Merhaba.",
        words: [Word(start: 0, end: 1, text: "Merhaba.")]
    )
    var events: [EngineEvent] = []
    for try await event in engine.translate([source], to: "es", from: "tr", model: "madlad-3b") {
        events.append(event)
    }
    #expect(events == [
        .stage(.downloadingModel, fraction: 0.12),
        .stage(.translating, fraction: 0.4),
        .translation(language: "es", sentences: [Sentence(id: 1, start: 0, end: 1, text: "Hola.")]),
    ])
    let arguments = try String(contentsOf: argsFile, encoding: .utf8)
    #expect(arguments.contains("translate --to es --model madlad-3b --json --from tr"))
    let input = try String(contentsOf: inputFile, encoding: .utf8)
    #expect(input.contains("\"Merhaba.\""))
    #expect(input.contains("\"words\""))
}

@Test func translateOmitsSourceWhenNil() async throws {
    let argsFile = FileManager.default.temporaryDirectory.appending(path: "args-\(UUID().uuidString)")
    let engine = try fakeEngine("""
    echo "$@" > '\(argsFile.path)'
    echo '{"event": "translation", "language": "es", "sentences": []}'
    """)
    for try await _ in engine.translate([], to: "es", model: "madlad-3b") {}
    let arguments = try String(contentsOf: argsFile, encoding: .utf8)
    #expect(arguments.contains("translate --to es --model madlad-3b --json"))
    #expect(!arguments.contains("--from"))
}

@Test func translateSurfacesEngineError() async throws {
    let engine = try fakeEngine("""
    echo '{"event": "error", "message": "NLLB için kaynak dil gerekli."}'
    exit 1
    """)
    await #expect(throws: EngineError.failed("NLLB için kaynak dil gerekli.")) {
        for try await _ in engine.translate([], to: "es", from: nil, model: "nllb-1.3b") {}
    }
}

private func isAlive(_ pid: pid_t) -> Bool { kill(pid, 0) == 0 }

/// PID dosyası yazılana kadar bekler.
private func readPID(_ file: URL) async throws -> pid_t {
    for _ in 0..<100 {
        if let text = try? String(contentsOf: file, encoding: .utf8),
           let pid = pid_t(text.trimmingCharacters(in: .whitespacesAndNewlines)) { return pid }
        try await Task.sleep(for: .milliseconds(50))
    }
    throw EngineError.failed("PID okunamadı")
}

private func waitUntilDead(_ pid: pid_t) async throws -> Bool {
    for _ in 0..<40 {
        if !isAlive(pid) { return true }
        try await Task.sleep(for: .milliseconds(50))
    }
    return !isAlive(pid)
}

@Test func cancelKillsEngine() async throws {
    let pidFile = FileManager.default.temporaryDirectory.appending(path: "pid-\(UUID().uuidString)")
    let engine = try fakeEngine("""
    echo $$ > '\(pidFile.path)'
    echo '{"event": "stage", "stage": "transcribing", "fraction": null}'
    exec sleep 30
    """)
    let consumer = Task {
        for try await _ in engine.transcribe(video: URL(filePath: "/v.mp4"), outDir: URL(filePath: "/tmp")) {}
    }
    let pid = try await readPID(pidFile)
    try await Task.sleep(for: .milliseconds(300))  // ilk olay tüketildi, motor hâlâ uyuyor
    #expect(isAlive(pid))
    consumer.cancel()
    #expect(try await waitUntilDead(pid))
}

@Test func garbageStdoutLineKillsEngineAndThrows() async throws {
    let pidFile = FileManager.default.temporaryDirectory.appending(path: "pid-\(UUID().uuidString)")
    let engine = try fakeEngine("""
    echo $$ > '\(pidFile.path)'
    echo 'bu JSON değil'
    exec sleep 30
    """)
    await #expect(throws: (any Error).self) {
        for try await _ in engine.transcribe(video: URL(filePath: "/v.mp4"), outDir: URL(filePath: "/tmp")) {}
    }
    let pid = try await readPID(pidFile)
    #expect(try await waitUntilDead(pid))
}

@Test func locatesDevEngine() {
    let command = EngineCommand.locate(environment: [:])
    #expect(command?.arguments.contains("subtitle-engine") == true)
}

/// Gerçek motorla uçtan uca: `ENGINE_E2E=1 swift test` (samples/demo.mp4 ve base modeli gerekir).
@Test(.enabled(if: ProcessInfo.processInfo.environment["ENGINE_E2E"] == "1"))
func realEngineOnDemoVideo() async throws {
    let repo = URL(filePath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    let engine = try #require(EngineCommand.locate(environment: [:]).map(EngineClient.init))
    let outDir = FileManager.default.temporaryDirectory.appending(path: "engine-e2e")
    var result: EngineResult?
    for try await event in engine.transcribe(
        video: repo.appending(path: "samples/demo.mp4"), outDir: outDir,
        options: .init(language: "tr", model: "base")
    ) {
        if case .result(let r) = event { result = r }
    }
    #expect(result?.language == "tr")
    #expect(result?.sentences.count == 3)
    let srt = try await engine.render(try #require(result?.sentences), as: .srt)
    #expect(srt.hasPrefix("1\n00:00:00,000 --> "))
}

/// Gerçek motorla uçtan uca çeviri: demo videoyu base ile yazıya döküp madlad-3b ile İspanyolcaya çevirir.
@Test(.enabled(if: ProcessInfo.processInfo.environment["ENGINE_E2E"] == "1"))
func realEngineTranslatesDemoVideo() async throws {
    let repo = URL(filePath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    let engine = try #require(EngineCommand.locate(environment: [:]).map(EngineClient.init))
    let outDir = FileManager.default.temporaryDirectory.appending(path: "engine-e2e-translate")
    var result: EngineResult?
    for try await event in engine.transcribe(
        video: repo.appending(path: "samples/demo.mp4"), outDir: outDir,
        options: .init(language: "tr", model: "base")
    ) {
        if case .result(let value) = event { result = value }
    }
    let source = try #require(result?.sentences)
    var language: String?
    var translated: [Sentence] = []
    for try await event in engine.translate(source, to: "es", from: "tr", model: "madlad-3b") {
        if case .translation(let target, let sentences) = event {
            language = target
            translated = sentences
        }
    }
    let timesAndIDsEqual = zip(source, translated).allSatisfy {
        $0.id == $1.id && $0.start == $1.start && $0.end == $1.end
    }
    let textsNonEmpty = translated.allSatisfy {
        !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
    let textsDiffer = zip(source, translated).allSatisfy { $0.text != $1.text }
    #expect(language == "es")
    #expect(!source.isEmpty)
    #expect(translated.count == source.count)
    #expect(timesAndIDsEqual)
    #expect(textsNonEmpty)
    #expect(textsDiffer)
}
