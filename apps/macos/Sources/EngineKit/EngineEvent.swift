import Foundation

/// Events the engine writes line by line to stdout in `--json` mode (see engine/src/subtitle_engine/cli.py).
public enum EngineEvent: Sendable, Equatable {
    case stage(Stage, fraction: Double?)
    case result(EngineResult)
    case translation(language: String, sentences: [Sentence])
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

    public static func decode(_ line: String) throws -> EngineEvent {
        let data = Data(line.utf8)
        let head = try decoder.decode(Head.self, from: data)
        switch head.event {
        case "stage":
            let stage = try decoder.decode(StagePayload.self, from: data)
            return .stage(Stage(rawValue: stage.stage) ?? .unknown, fraction: stage.fraction)
        case "result":
            return .result(try decoder.decode(EngineResult.self, from: data))
        case "translation":
            let payload = try decoder.decode(TranslationPayload.self, from: data)
            return .translation(language: payload.language, sentences: payload.sentences)
        case "error":
            return .error(try decoder.decode(ErrorPayload.self, from: data).message)
        default:
            throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "bilinmeyen olay: \(head.event)"))
        }
    }

    private struct Head: Decodable { let event: String }
    private struct StagePayload: Decodable { let stage: String; let fraction: Double? }
    private struct TranslationPayload: Decodable { let language: String; let sentences: [Sentence] }
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
