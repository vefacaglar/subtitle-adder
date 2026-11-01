import Foundation
import Testing
@testable import EngineKit

@Suite struct SpeakerTests {
    private func dialogue() -> [Sentence] {
        [
            Sentence(id: 1, start: 0, end: 1, text: "Hello.", speaker: 0),
            Sentence(id: 2, start: 1, end: 2, text: "Hi there.", speaker: 1),
            Sentence(id: 3, start: 2, end: 3, text: "How are you?", speaker: 1),
            Sentence(id: 4, start: 3, end: 4, text: "Fine.", speaker: 0),
        ]
    }

    @Test func decodesSentenceWithSpeaker() throws {
        let json = #"{"id": 1, "start": 0.0, "end": 1.0, "text": "Hi.", "words": [], "speaker": 1}"#
        let sentence = try JSONDecoder().decode(Sentence.self, from: Data(json.utf8))
        #expect(sentence.speaker == 1)
    }

    @Test func decodesSentenceWithoutSpeakerOrNull() throws {
        let plain = #"{"id": 1, "start": 0.0, "end": 1.0, "text": "Hi.", "words": []}"#
        let null = #"{"id": 1, "start": 0.0, "end": 1.0, "text": "Hi.", "words": [], "speaker": null}"#
        #expect(try JSONDecoder().decode(Sentence.self, from: Data(plain.utf8)).speaker == nil)
        #expect(try JSONDecoder().decode(Sentence.self, from: Data(null.utf8)).speaker == nil)
    }

    @Test func decodesSpeakerInResultAndTranslationEvents() throws {
        let result = try EngineEvent.decode(#"{"event": "result", "language": "en", "duration": 2.0, "audio": "a.wav", "outputs": [], "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Hi.", "words": [], "speaker": 0}]}"#)
        guard case .result(let value) = result else { Issue.record("not a result"); return }
        #expect(value.sentences[0].speaker == 0)
        let translation = try EngineEvent.decode(#"{"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Hola.", "words": [], "speaker": 1}]}"#)
        guard case .translation(_, let sentences) = translation else { Issue.record("not a translation"); return }
        #expect(sentences[0].speaker == 1)
    }

    @Test func decodesDiarizingStage() throws {
        #expect(try EngineEvent.decode(#"{"event": "stage", "stage": "diarizing", "fraction": null}"#)
            == .stage(.diarizing, fraction: nil))
    }

    @Test func encodesSpeakerOnlyWhenPresent() throws {
        let with = try JSONEncoder().encode(Sentence(id: 1, start: 0, end: 1, text: "A", speaker: 2))
        let without = try JSONEncoder().encode(Sentence(id: 1, start: 0, end: 1, text: "A"))
        #expect(String(decoding: with, as: UTF8.self).contains("\"speaker\":2"))
        #expect(!String(decoding: without, as: UTF8.self).contains("speaker"))
    }

    @Test func renderSendsSpeakerOnStdin() async throws {
        let engine = try fakeEngine("cat")
        let text = try await engine.render(dialogue(), as: .srt)
        #expect(text.contains("\"speaker\":1"))
    }

    @Test func translateSendsSpeakerOnStdin() async throws {
        let inputFile = FileManager.default.temporaryDirectory.appending(path: "input-\(UUID().uuidString)")
        let engine = try fakeEngine("""
        cat > '\(inputFile.path)'
        echo '{"event": "translation", "language": "es", "sentences": []}'
        """)
        for try await _ in engine.translate(dialogue(), to: "es") {}
        let input = try String(contentsOf: inputFile, encoding: .utf8)
        #expect(input.contains("\"speaker\":0"))
        #expect(input.contains("\"speaker\":1"))
    }

    @Test func transcribeArgumentsIncludeDiarizeOptions() {
        let video = URL(filePath: "/tmp/v.mp4"), out = URL(filePath: "/tmp/out")
        let off = EngineClient.transcribeArguments(video: video, outDir: out, options: .init(speakers: 3))
        #expect(!off.contains("--diarize"))
        #expect(!off.contains("--speakers"))
        let auto = EngineClient.transcribeArguments(video: video, outDir: out, options: .init(diarize: true))
        #expect(auto.contains("--diarize"))
        #expect(!auto.contains("--speakers"))
        let two = EngineClient.transcribeArguments(video: video, outDir: out, options: .init(diarize: true, speakers: 2))
        #expect(Array(two.suffix(3)) == ["--diarize", "--speakers", "2"])
    }

    @Test func decodesDiarizationModelKind() throws {
        let json = #"{"id": "speaker-diarization", "kind": "diarization", "backend": "onnx", "languages": null, "size_mb": 40, "disk_mb": null, "speed": 5, "accuracy": 3, "license": "MIT", "downloaded": false}"#
        let model = try JSONDecoder().decode(EngineModel.self, from: Data(json.utf8))
        #expect(model.isDiarization)
        #expect(!model.isTranscription)
        #expect(!model.isTranslation)
    }

    // MARK: - Dash rule

    @Test func dashAppliesAtSpeakerChanges() {
        let s = dialogue()
        #expect((0..<s.count).map { s.startsDialogueTurn(at: $0) } == [true, true, false, true])
        #expect(s.displayText(at: 0) == "- Hello.")
        #expect(s.displayText(at: 2) == "How are you?")
    }

    @Test func dashNeverAppliesWithOneSpeakerOrNone() {
        let one = [Sentence(id: 1, start: 0, end: 1, text: "A", speaker: 0),
                   Sentence(id: 2, start: 1, end: 2, text: "B", speaker: 0)]
        let none = [Sentence(id: 1, start: 0, end: 1, text: "A"), Sentence(id: 2, start: 1, end: 2, text: "B")]
        #expect(!one.startsDialogueTurn(at: 0))
        #expect(!none.startsDialogueTurn(at: 1))
        #expect(none.displayText(at: 1) == "B")
        #expect(!one.startsDialogueTurn(at: 99))
    }

    @Test func dashSkipsSentencesWithoutSpeaker() {
        let s = [Sentence(id: 1, start: 0, end: 1, text: "A", speaker: 0),
                 Sentence(id: 2, start: 1, end: 2, text: "B"),
                 Sentence(id: 3, start: 2, end: 3, text: "C", speaker: 1)]
        #expect(!s.startsDialogueTurn(at: 1))
        #expect(s.startsDialogueTurn(at: 2))
    }

    // MARK: - Editing

    @Test func setSpeakerChangesAndReportsNoOp() {
        var s = dialogue()
        let changed = s.setSpeaker(1, at: 0)
        #expect(changed)
        #expect(s[0].speaker == 1)
        let repeated = s.setSpeaker(1, at: 0)
        let outOfRange = s.setSpeaker(1, at: 9)
        #expect(!repeated)
        #expect(!outOfRange)
        #expect(s.nextNewSpeaker == 2)
    }

    @Test func mergeKeepsFirstSpeaker() {
        var s = dialogue()
        let merged = s.mergeWithNext(at: 0)
        #expect(merged)
        #expect(s[0].speaker == 0)
        #expect(s.count == 3)
    }

    @Test func splitKeepsSpeakerOnBothHalves() {
        var s = [Sentence(id: 1, start: 0, end: 4, text: "one two three four", speaker: 1)]
        let split = s.splitSentence(at: 0, at: 2)
        #expect(split)
        #expect(s.map(\.speaker) == [1, 1])
    }
}

@Suite struct DialogueParityTests {
    private func dashes(_ speakers: [Int?]) -> [Bool] {
        speakers.enumerated()
            .map { Sentence(id: $0.offset + 1, start: Double($0.offset), end: Double($0.offset + 1), text: "S", speaker: $0.element) }
            .dialogueDashes()
    }

    /// Mirrors engine/tests/test_diarize.py (speaker_dashes cases).
    @Test func matchesEngineSpeakerDashes() {
        #expect(dashes([0, 0, 1, 0]) == [true, false, true, true])
        #expect(dashes([0, 0, 0]) == [false, false, false])
        #expect(dashes([nil, nil, nil]) == [false, false, false])
        #expect(dashes([0, nil, 0, 1]) == [true, false, false, true])
        #expect(dashes([0, nil, 0]) == [false, false, false])  // one distinct speaker: no dashes
        #expect(dashes([]) == [])
    }

    @Test func dashNotDoubledOnAlreadyDashedText() {
        #expect(DialogueDash.apply(true, to: "- Selam.") == "- Selam.")
        #expect(DialogueDash.apply(true, to: "\u{2013} Merhaba.") == "\u{2013} Merhaba.")
        #expect(DialogueDash.apply(true, to: "-Kısa.") == "- -Kısa.")
        #expect(DialogueDash.apply(false, to: "Selam.") == "Selam.")
        #expect(DialogueDash.apply(true, to: "Selam.") == "- Selam.")
    }
}

@Suite struct DownloadPhaseTrackerTests {
    private typealias T = DownloadPhaseTracker

    /// Runs a stage sequence through a tracker and returns the model label of each event.
    private func run(
        transcriptionDownloaded: Bool, diarizationDownloaded: Bool,
        _ events: [(EngineEvent.Stage, Double?)]
    ) -> [T.Kind?] {
        var tracker = T(transcriptionDownloaded: transcriptionDownloaded, diarize: true, diarizationDownloaded: diarizationDownloaded)
        return events.map { tracker.update(stage: $0.0, fraction: $0.1) }
    }

    @Test func bothMissingBackToBack() {
        let kinds = run(transcriptionDownloaded: false, diarizationDownloaded: false, [
            (.extractingAudio, nil), (.downloadingModel, 0), (.downloadingModel, 0.6), (.downloadingModel, 1),
            (.downloadingModel, 0), (.downloadingModel, 0.5), (.downloadingModel, 1), (.loadingModel, nil),
        ])
        #expect(kinds == [nil, .transcription, .transcription, .transcription,
                          .diarization, .diarization, .diarization, nil])
    }

    @Test func onlySpeakerModelMissing() {
        let kinds = run(transcriptionDownloaded: true, diarizationDownloaded: false,
                        [(.downloadingModel, 0), (.downloadingModel, 1), (.loadingModel, nil)])
        #expect(kinds == [.diarization, .diarization, nil])
    }

    @Test func onlyTranscriptionModelMissing() {
        let kinds = run(transcriptionDownloaded: false, diarizationDownloaded: true,
                        [(.downloadingModel, 0), (.downloadingModel, 1), (.loadingModel, nil)])
        #expect(kinds == [.transcription, .transcription, nil])
    }

    @Test func nothingMissingStillLabelsUnexpectedDownload() {
        let kinds = run(transcriptionDownloaded: true, diarizationDownloaded: true, [(.downloadingModel, 0.2)])
        #expect(kinds == [.transcription])
    }

    @Test func separatedPhasesByAnotherStage() {
        let kinds = run(transcriptionDownloaded: false, diarizationDownloaded: false, [
            (.downloadingModel, 1), (.loadingModel, nil), (.downloadingModel, 0.1),
        ])
        #expect(kinds == [.transcription, nil, .diarization])
    }

    @Test func diarizeOffExpectsNoSpeakerPhase() {
        var tracker = T(transcriptionDownloaded: false, diarize: false, diarizationDownloaded: false)
        #expect(tracker.update(stage: .downloadingModel, fraction: 1) == .transcription)
    }
}

/// End to end with diarization: two speakers in samples/dialog-en.mp4 (needs the base model; the diarization model downloads on first use).
@Test(.enabled(if: ProcessInfo.processInfo.environment["ENGINE_E2E"] == "1"))
func realEngineSeparatesSpeakersInDialogue() async throws {
    let repo = URL(filePath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    let engine = try #require(EngineCommand.locate(environment: [:]).map(EngineClient.init))
    let outDir = FileManager.default.temporaryDirectory.appending(path: "engine-e2e-diarize")
    var result: EngineResult?
    var stages: [EngineEvent.Stage] = []
    for try await event in engine.transcribe(
        video: repo.appending(path: "samples/dialog-en.mp4"), outDir: outDir,
        options: .init(language: "en", model: "base", diarize: true, speakers: 2)
    ) {
        if case .stage(let stage, _) = event { stages.append(stage) }
        if case .result(let value) = event { result = value }
    }
    let sentences = try #require(result?.sentences)
    #expect(stages.contains(.diarizing))
    #expect(sentences.allSatisfy { $0.speaker != nil })
    #expect(sentences.speakers.count == 2)
    let srt = try await engine.render(sentences, as: .srt)
    #expect(srt.contains("\n- "))
}
