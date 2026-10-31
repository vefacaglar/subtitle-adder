import Foundation
import SubtitleCore
import WhisperKit

/// WhisperKit ile on-device (CoreML / Neural Engine) STT. Model ilk kullanımda indirilir.
public final class WhisperKitTranscriber: Transcriber {
    /// WhisperKit Sendable değil; tek seferlik yüklenip yalnız bu sınıf içinden kullanılır.
    private final class Loaded: @unchecked Sendable {
        let kit: WhisperKit
        init(_ kit: WhisperKit) { self.kit = kit }
    }

    private let loader: Task<Loaded, Error>

    /// `model`: "tiny", "base", "small", "large-v3-v20240930_turbo" vb. Yükleme ilk çağrıda beklenir.
    public init(model: String = "small") {
        loader = Task { Loaded(try await WhisperKit(WhisperKitConfig(model: model))) }
    }

    public func transcribe(audio: URL, language: String?) async throws -> Transcript {
        let pipe = try await loader.value.kit
        let options = DecodingOptions(
            language: language,
            detectLanguage: language == nil,
            wordTimestamps: true,
            chunkingStrategy: .vad
        )
        let results: [TranscriptionResult] = try await pipe.transcribe(audioPath: audio.path, decodeOptions: options)
        let words = results
            .flatMap(\.segments)
            .flatMap { $0.words ?? [] }
            .compactMap { timing -> Word? in
                let text = timing.word.trimmingCharacters(in: .whitespacesAndNewlines)
                return text.isEmpty ? nil : Word(start: Double(timing.start), end: Double(timing.end), text: text)
            }
        return Transcript(language: results.first?.language ?? language ?? "?", words: words)
    }

}
