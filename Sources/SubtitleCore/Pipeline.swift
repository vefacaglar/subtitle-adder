import Foundation

/// Hattın tamamı: video -> ses -> transkript -> cümleler.
public struct SubtitlePipeline: Sendable {
    public enum Stage: Sendable {
        case extractingAudio, transcribing, segmenting, done
    }

    public struct Result: Sendable {
        public var audio: URL
        public var language: String
        public var sentences: [Sentence]
    }

    public var transcriber: any Transcriber
    public var segmenter: SentenceSegmenter

    public init(transcriber: any Transcriber, segmenter: SentenceSegmenter = .init()) {
        self.transcriber = transcriber
        self.segmenter = segmenter
    }

    public func run(
        video: URL,
        workDir: URL,
        language: String? = nil,
        progress: @Sendable (Stage) -> Void = { _ in }
    ) async throws -> Result {
        progress(.extractingAudio)
        let wav = workDir.appending(path: video.deletingPathExtension().lastPathComponent + ".wav")
        try await AudioExtractor.extract(from: video, to: wav)

        progress(.transcribing)
        let transcript = try await transcriber.transcribe(audio: wav, language: language)

        progress(.segmenting)
        let sentences = segmenter.segment(transcript.words)

        progress(.done)
        return Result(audio: wav, language: transcript.language, sentences: sentences)
    }
}
