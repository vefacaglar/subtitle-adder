import Foundation
import Observation
import SubtitleCore
import WhisperTranscriber

@MainActor
@Observable
final class SubtitleViewModel {
    enum Status: Equatable {
        case idle
        case working(String)
        case finished
        case failed(String)
    }

    var video: URL?
    var language = ""            // boş = otomatik algıla
    var model = "small"
    var status: Status = .idle
    var detectedLanguage: String?
    var sentences: [Sentence] = []

    static let models = ["tiny", "base", "small", "medium", "large-v3-v20240930_turbo"]

    var isWorking: Bool {
        if case .working = status { true } else { false }
    }

    func generate() async {
        guard let video else { return }
        sentences = []
        detectedLanguage = nil
        let workDir = FileManager.default.temporaryDirectory.appending(path: "SubtitleAdder")
        let pipeline = SubtitlePipeline(transcriber: WhisperKitTranscriber(model: model))
        do {
            let result = try await pipeline.run(
                video: video,
                workDir: workDir,
                language: language.isEmpty ? nil : language
            ) { stage in
                Task { @MainActor in self.status = .working(Self.label(for: stage)) }
            }
            sentences = result.sentences
            detectedLanguage = result.language
            status = .finished
        } catch {
            status = .failed(error.localizedDescription)
        }
    }

    func export(to url: URL, as format: SubtitleFormat) {
        do {
            try SubtitleWriter.write(sentences, to: url, as: format)
        } catch {
            status = .failed(error.localizedDescription)
        }
    }

    private static func label(for stage: SubtitlePipeline.Stage) -> String {
        switch stage {
        case .extractingAudio: "Ses ayrılıyor…"
        case .transcribing: "Konuşma metne çevriliyor… (ilk seferde model indirilir)"
        case .segmenting: "Cümleler çıkarılıyor…"
        case .done: "Bitti"
        }
    }
}
