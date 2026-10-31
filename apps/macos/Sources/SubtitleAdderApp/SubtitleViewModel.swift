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

    var language = ""            // boş = otomatik algıla
    var model: String {
        didSet { UserDefaults.standard.set(model, forKey: Self.modelDefaultsKey) }
    }
    var status: Status = .idle
    var detectedLanguage: String?
    var sentences: [Sentence] = []
    var selection: UUID?         // seçili satırın `uid` değeri
    /// Düzenlenen satırın henüz commit edilmemiş metni (odaklıyken kullanılır).
    var draft: TextDraft?
    /// Pencerenin geri alma yöneticisi (görünüm tarafından verilir).
    @ObservationIgnored weak var undoManager: UndoManager?

    /// Motordan gelen model kataloğu; yüklenene kadar yedek liste gösterilir.
    var models: [EngineModel] = SubtitleViewModel.fallbackModels

    private(set) var video: URL?

    struct TextDraft: Equatable {
        var uid: UUID
        var text: String
    }

    static let defaultModel = "large-v3"

    /// Motor yoksa ya da katalog alınamazsa kullanılan liste (boyutlar katalogla aynı).
    static let fallbackModels: [EngineModel] = [
        EngineModel(id: "base", sizeMB: 145, speed: 5, accuracy: 2, downloaded: false),
        EngineModel(id: "small", sizeMB: 465, speed: 4, accuracy: 3, downloaded: false),
        EngineModel(id: "medium", sizeMB: 1500, speed: 3, accuracy: 4, downloaded: false),
        EngineModel(id: "large-v3-turbo", sizeMB: 1600, speed: 2, accuracy: 4, downloaded: false),
        EngineModel(id: "large-v3", sizeMB: 3000, speed: 1, accuracy: 5, downloaded: false),
    ]

    private static let modelDefaultsKey = "selectedModel"

    init() {
        model = UserDefaults.standard.string(forKey: Self.modelDefaultsKey) ?? Self.defaultModel
    }

    private let engine = EngineCommand.locate().map(EngineClient.init)
    private var job: Task<Void, Never>?
    private var jobID = UUID()  // yalnız güncel iş durumu değiştirebilir

    var isWorking: Bool {
        if case .working = status { true } else { false }
    }

    /// Yeni video seçildiğinde eski videoya ait işi, altyazıları ve geri alma yığınını temizler.
    func openVideo(_ url: URL?) {
        guard url != video else { return }
        job?.cancel()
        job = nil
        jobID = UUID()
        sentences = []
        selection = nil
        detectedLanguage = nil
        draft = nil
        status = .idle
        undoManager?.removeAllActions()
        video = url
    }

    /// Model kataloğunu motordan yeniler; motor yoksa ya da hata olursa yedek liste kalır.
    func loadModels() async {
        guard let engine, let list = try? await engine.models(), !list.isEmpty else { return }
        models = list
    }

    func generate() {
        guard let video else { return }
        guard let engine else { status = .failed(EngineError.notFound.localizedDescription); return }
        sentences = []
        selection = nil
        draft = nil
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
                        status = .working(Self.label(for: stage, fraction: fraction), fraction: fraction)
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
                // İndirilen model menüde "hazır" görünsün.
                await loadModels()
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

    private static func label(for stage: EngineEvent.Stage, fraction: Double?) -> String {
        switch stage {
        case .extractingAudio: "Ses ayrılıyor…"
        case .downloadingModel:
            if let fraction {
                "Model indiriliyor… %\(Int((min(max(fraction, 0), 1) * 100).rounded()))"
            } else {
                "Model indiriliyor…"
            }
        case .loadingModel: "Model yükleniyor…"
        case .transcribing: "Konuşma metne çevriliyor…"
        case .segmenting: "Cümleler çıkarılıyor…"
        case .unknown: "Çalışıyor…"
        }
    }
}
