import EngineKit
import Foundation
import Observation

@MainActor
@Observable
final class SubtitleViewModel {
    enum Status: Equatable {
        case idle
        case working(String, fraction: Double?)
        case finished
        case failed(String)
    }

    var video: URL?
    var language = ""            // boş = otomatik algıla
    var model = "small"
    var status: Status = .idle
    var detectedLanguage: String?
    var sentences: [Sentence] = []

    static let models = ["tiny", "base", "small", "medium", "large-v3", "turbo"]

    private let engine = EngineCommand.locate().map(EngineClient.init)
    private var job: Task<Void, Never>?

    var isWorking: Bool {
        if case .working = status { true } else { false }
    }

    func generate() {
        guard let video else { return }
        guard let engine else { status = .failed(EngineError.notFound.localizedDescription); return }
        sentences = []
        detectedLanguage = nil
        status = .working("Başlıyor…", fraction: nil)
        let outDir = FileManager.default.temporaryDirectory.appending(path: "SubtitleAdder")
        let options = EngineClient.TranscribeOptions(language: language.isEmpty ? nil : language, model: model)
        job = Task {
            do {
                for try await event in engine.transcribe(video: video, outDir: outDir, options: options) {
                    switch event {
                    case .stage(let stage, let fraction):
                        status = .working(Self.label(for: stage), fraction: fraction)
                    case .result(let result):
                        sentences = result.sentences
                        detectedLanguage = result.language
                    case .error(let message):
                        status = .failed(message)
                    }
                }
                status = .finished
            } catch is CancellationError {
                status = .idle
            } catch {
                status = .failed(error.localizedDescription)
            }
        }
    }

    func cancel() {
        job?.cancel()
        job = nil
        status = .idle
    }

    func render(as format: SubtitleFormat) async -> String? {
        do {
            return try await engine?.render(sentences, as: format)
        } catch {
            status = .failed(error.localizedDescription)
            return nil
        }
    }

    private static func label(for stage: EngineEvent.Stage) -> String {
        switch stage {
        case .extractingAudio: "Ses ayrılıyor…"
        case .loadingModel: "Model yükleniyor… (ilk seferde indirilir)"
        case .transcribing: "Konuşma metne çevriliyor…"
        case .segmenting: "Cümleler çıkarılıyor…"
        case .unknown: "Çalışıyor…"
        }
    }
}
