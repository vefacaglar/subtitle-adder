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

    /// Boş kod otomatik algılamadır; seçim UserDefaults'ta kalıcıdır.
    var language: String {
        didSet {
            guard language != oldValue else { return }
            UserDefaults.standard.set(language, forKey: Self.languageDefaultsKey)
            modelNotice = nil
            reconcileModelSelection()
        }
    }
    var model: String {
        didSet {
            guard model != oldValue else { return }
            UserDefaults.standard.set(model, forKey: Self.modelDefaultsKey)
            modelNotice = nil
        }
    }
    var status: Status = .idle
    var detectedLanguage: String?
    var sentences: [Sentence] = []
    var selection: UUID?         // seçili satırın `uid` değeri
    /// Düzenlenen satırın henüz commit edilmemiş metni (odaklıyken kullanılır).
    var draft: TextDraft?
    /// Seçili model dil filtresine uymadığı için otomatik değiştiyse kısa bilgi.
    private(set) var modelNotice: String?
    /// Pencerenin geri alma yöneticisi (görünüm tarafından verilir).
    @ObservationIgnored weak var undoManager: UndoManager?

    /// Paylaşılan model kataloğu; araç çubuğu ve Ayarlar aynı listeyi kullanır.
    let catalog: ModelCatalog

    private var job: Task<Void, Never>?
    private var jobID = UUID()  // yalnız güncel iş durumu değiştirebilir

    private(set) var video: URL?

    struct TextDraft: Equatable {
        var uid: UUID
        var text: String
    }

    static let defaultModel = "large-v3"

    private static let modelDefaultsKey = "selectedModel"
    private static let languageDefaultsKey = "selectedLanguage"

    init(catalog: ModelCatalog) {
        self.catalog = catalog
        let savedLanguage = UserDefaults.standard.string(forKey: Self.languageDefaultsKey) ?? ""
        // Eski sürüm serbest metin kabul ediyordu; listede olmayan kod otomatiğe döner.
        language = SubtitleLanguage(rawValue: savedLanguage) == nil ? "" : savedLanguage
        model = UserDefaults.standard.string(forKey: Self.modelDefaultsKey) ?? Self.defaultModel
        reconcileModelSelection()
    }

    /// Dile göre filtrelenmiş katalog; araç çubuğundaki seçici bunu gösterir.
    var availableModels: [EngineModel] { catalog.models(for: language) }

    var isWorking: Bool {
        if case .working = status { true } else { false }
    }

    /// Seçili model filtrelenen listede yoksa en doğru (eşitse daha hızlı) modele geçer.
    func reconcileModelSelection() {
        let available = availableModels
        guard !available.isEmpty, !available.contains(where: { $0.id == model }) else { return }
        guard let best = available.max(by: { ($0.accuracy, $0.speed) < ($1.accuracy, $1.speed) }) else { return }
        model = best.id
        if let language = SubtitleLanguage(rawValue: language) {
            modelNotice = "\(language.shortName) için \(ModelLabel.name(for: best.id)) seçildi."
        }
    }

    /// Yeni video seçildiğinde eski videoya ait işi, altyazıları ve geri alma yığınını temizler.
    func openVideo(_ url: URL?) {
        guard url != video else { return }
        job?.cancel()
        job = nil
        jobID = UUID()
        catalog.activeModel = nil
        sentences = []
        selection = nil
        detectedLanguage = nil
        draft = nil
        status = .idle
        undoManager?.removeAllActions()
        video = url
    }

    func generate() {
        guard let video else { return }
        guard let engine = catalog.engine else {
            status = .failed(EngineError.notFound.localizedDescription)
            return
        }
        sentences = []
        selection = nil
        draft = nil
        undoManager?.removeAllActions()
        detectedLanguage = nil
        modelNotice = nil
        status = .working("Başlıyor…", fraction: nil)
        let outDir = FileManager.default.temporaryDirectory.appending(path: "SubtitleAdder")
        let options = EngineClient.TranscribeOptions(language: language.isEmpty ? nil : language, model: model)
        let runningModel = model
        job?.cancel()
        let id = UUID()
        jobID = id
        catalog.activeModel = runningModel
        job = Task {
            // Hangi yolla biterse bitsin (sonuç, hata, iptal) model Ayarlar'da silinebilir olur.
            defer { if jobID == id { catalog.activeModel = nil } }
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
                await catalog.load()
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
        catalog.activeModel = nil
        status = .idle
    }

    func render(as format: SubtitleFormat) async -> String? {
        do {
            return try await catalog.engine?.render(sentences, as: format)
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
