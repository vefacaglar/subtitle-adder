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
    var selection: Int?          // seçili satırın kimliği
    /// Pencerenin geri alma yöneticisi (görünüm tarafından verilir).
    @ObservationIgnored weak var undoManager: UndoManager?

    static let models = ["tiny", "base", "small", "medium", "large-v3", "turbo"]

    private let engine = EngineCommand.locate().map(EngineClient.init)
    private var job: Task<Void, Never>?
    private var jobID = UUID()  // yalnız güncel iş durumu değiştirebilir

    var isWorking: Bool {
        if case .working = status { true } else { false }
    }

    func generate() {
        guard let video else { return }
        guard let engine else { status = .failed(EngineError.notFound.localizedDescription); return }
        sentences = []
        selection = nil
        undoManager?.removeAllActions()
        detectedLanguage = nil
        status = .working("Başlıyor…", fraction: nil)
        let outDir = FileManager.default.temporaryDirectory.appending(path: "SubtitleAdder")
        let options = EngineClient.TranscribeOptions(language: language.isEmpty ? nil : language, model: model)
        job?.cancel()
        let id = UUID()
        jobID = id
        job = Task {
            var failed = false
            do {
                for try await event in engine.transcribe(video: video, outDir: outDir, options: options) {
                    guard jobID == id, !Task.isCancelled else { return }
                    switch event {
                    case .stage(let stage, let fraction):
                        status = .working(Self.label(for: stage), fraction: fraction)
                    case .result(let result):
                        sentences = result.sentences
                        detectedLanguage = result.language
                        // Uygulama çıkarılan WAV'ı kullanmıyor; geçici dosyayı sil.
                        try? FileManager.default.removeItem(atPath: result.audio)
                    case .error(let message):
                        failed = true
                        status = .failed(message)
                    }
                }
                // İptalde akış hata vermeden biter; o durumda durum değişmez.
                guard jobID == id, !Task.isCancelled else { return }
                if !failed { status = .finished }
            } catch is CancellationError {
                if jobID == id { status = .idle }
            } catch {
                if jobID == id { status = .failed(error.localizedDescription) }
            }
        }
    }

    func cancel() {
        job?.cancel()
        job = nil
        jobID = UUID()
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
