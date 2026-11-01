import Foundation

/// Events the engine writes line by line to stdout in `--json` mode (see engine/src/subtitle_engine/cli.py).
public enum EngineEvent: Sendable, Equatable {
    case stage(Stage, fraction: Double?)
    case result(EngineResult)
    /// `translate` emits one of these per batch (a growing prefix of the sentences) and a final complete one.
    case translation(language: String, sentences: [Sentence])
    /// Live transcription snapshot: every sentence recognized so far (not a delta), throttled by the engine.
    /// `language` is `"?"` while it is not yet known.
    case partial(language: String, sentences: [Sentence])
    case error(String)

    public enum Stage: String, Sendable {
        case extractingAudio = "extracting_audio"
        case downloadingModel = "downloading_model"
        case loadingModel = "loading_model"
        case transcribing
        case diarizing
        case segmenting
        case translating
        case unknown
    }

    /// Decodes one stdout line. Returns `nil` for an event type this client doesn't know, so newer engines
    /// can add events without breaking older apps; malformed lines and known events with bad payloads throw.
    public static func decode(_ line: String) throws -> EngineEvent? {
        let data = Data(line.utf8)
        let head = try decoder.decode(Head.self, from: data)
        switch head.event {
        case "stage":
            let stage = try decoder.decode(StagePayload.self, from: data)
            return .stage(Stage(rawValue: stage.stage) ?? .unknown, fraction: stage.fraction)
        case "result":
            return .result(try decoder.decode(EngineResult.self, from: data))
        case "translation":
            let payload = try decoder.decode(SentencesPayload.self, from: data)
            return .translation(language: payload.language, sentences: payload.sentences)
        case "partial":
            let payload = try decoder.decode(SentencesPayload.self, from: data)
            return .partial(language: payload.language, sentences: payload.sentences)
        case "error":
            return .error(try decoder.decode(ErrorPayload.self, from: data).message)
        default:
            return nil
        }
    }

    private struct Head: Decodable { let event: String }
    private struct StagePayload: Decodable { let stage: String; let fraction: Double? }
    private struct SentencesPayload: Decodable { let language: String; let sentences: [Sentence] }
    private struct ErrorPayload: Decodable { let message: String }
    private static let decoder = JSONDecoder()
}

public struct EngineResult: Sendable, Equatable, Decodable {
    public var language: String
    public var duration: Double
    public var audio: String
    public var outputs: [String]
    public var sentences: [Sentence]
}
