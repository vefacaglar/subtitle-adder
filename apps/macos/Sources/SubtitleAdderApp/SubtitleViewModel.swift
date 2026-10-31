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
            reconcileTranslationTarget()
        }
    }
    var model: String {
        didSet {
            guard model != oldValue else { return }
            UserDefaults.standard.set(model, forKey: Self.modelDefaultsKey)
            modelNotice = nil
        }
    }
    /// Çeviri isteğinde kullanılan hedef dil; seçim kalıcıdır.
    var translationTarget: String {
        didSet {
            guard translationTarget != oldValue else { return }
            UserDefaults.standard.set(translationTarget, forKey: Self.translationTargetDefaultsKey)
        }
    }
    /// Çeviri modeli (Ayarlar'dan seçilir, kalıcıdır).
    var translationModel: String {
        didSet {
            guard translationModel != oldValue else { return }
            UserDefaults.standard.set(translationModel, forKey: Self.translationModelDefaultsKey)
        }
    }
    /// Video üstü altyazıda orijinalin mi, çevirinin mi gösterileceği; kalıcıdır.
    var displayMode: SubtitleDisplay {
        didSet {
            guard displayMode != oldValue else { return }
            UserDefaults.standard.set(displayMode.rawValue, forKey: Self.displayModeDefaultsKey)
        }
    }
    var status: Status = .idle
    var detectedLanguage: String?
    var sentences: [Sentence] = []
    /// Satırın kararlı `uid` değerine bağlı çeviriler; metin düzenlemelerinde "eski" işaretlenir.
    var translations: [UUID: TranslationEntry] = [:]
    /// Son başarılı çevirinin hedef dili (dışa aktarma etiketi için).
    var translationLanguage: String?
    /// Düzenlenen çevirinin henüz commit edilmemiş metni.
    var translationDraft: TextDraft?
    var selection: UUID?         // seçili satırın `uid` değeri
    /// Düzenlenen satırın henüz commit edilmemiş metni (odaklıyken kullanılır).
    var draft: TextDraft?
    /// Seçili model dil filtresine uymadığı için otomatik değiştiyse kısa bilgi.
    private(set) var modelNotice: String?
    /// Pencerenin geri alma yöneticisi (görünüm tarafından verilir).
    @ObservationIgnored weak var undoManager: UndoManager?

    /// Paylaşılan model kataloğu; araç çubuğu ve Ayarlar aynı listeyi kullanır.
    let catalog: ModelCatalog

    var job: Task<Void, Never>?
    var jobID = UUID()  // yalnız güncel iş durumu değiştirebilir

    private(set) var video: URL?

    struct TextDraft: Equatable {
        var uid: UUID
        var text: String
    }

    static let defaultModel = "large-v3"
    static let defaultTranslationModel = "madlad-3b"
    static let defaultTranslationTarget = SubtitleLanguage.english.rawValue

    private static let modelDefaultsKey = "selectedModel"
    private static let languageDefaultsKey = "selectedLanguage"
    private static let translationModelDefaultsKey = "translationModel"
    private static let translationTargetDefaultsKey = "translationTarget"
    private static let displayModeDefaultsKey = "subtitleDisplayMode"

    init(catalog: ModelCatalog) {
        self.catalog = catalog
        let savedLanguage = UserDefaults.standard.string(forKey: Self.languageDefaultsKey) ?? ""
        // Eski sürüm serbest metin kabul ediyordu; listede olmayan kod otomatiğe döner.
        language = SubtitleLanguage(rawValue: savedLanguage) == nil ? "" : savedLanguage
        model = UserDefaults.standard.string(forKey: Self.modelDefaultsKey) ?? Self.defaultModel
        let savedTarget = UserDefaults.standard.string(forKey: Self.translationTargetDefaultsKey) ?? ""
        translationTarget = SubtitleLanguage(rawValue: savedTarget).map(\.rawValue)
            .flatMap { $0 == SubtitleLanguage.automatic.rawValue ? nil : $0 }
            ?? Self.defaultTranslationTarget
        translationModel = UserDefaults.standard.string(forKey: Self.translationModelDefaultsKey)
            ?? Self.defaultTranslationModel
        let savedDisplay = UserDefaults.standard.string(forKey: Self.displayModeDefaultsKey) ?? ""
        displayMode = SubtitleDisplay(rawValue: savedDisplay) ?? .original
        reconcileModelSelection()
        reconcileTranslationTarget()
    }

    /// Dile göre filtrelenmiş katalog; araç çubuğundaki seçici bunu gösterir.
    var availableModels: [EngineModel] { catalog.models(for: language) }

    var isWorking: Bool {
        if case .working = status { true } else { false }
    }

    /// Dil koduna göre (boşsa otomatik) değişen kaynak dil; çeviri komutuna `--from` olarak gider.
    /// Otomatik seçimde motorun algıladığı dil kullanılır.
    var translationSource: String? {
        if !language.isEmpty { return language }
        return detectedLanguage
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

    /// Ayarlardaki çeviri modeli katalogda yoksa (eski kayıt) en doğru modele döner.
    func reconcileTranslationModelSelection() {
        let available = catalog.translationModels
        guard !available.isEmpty, !available.contains(where: { $0.id == translationModel }) else { return }
        translationModel = (available.max(by: { ($0.accuracy, $0.speed) < ($1.accuracy, $1.speed) }) ?? available[0]).id
    }

    /// Kaynak dille aynı hedef dil seçilemez; seçilmişse başka bir hedefe geçer.
    func reconcileTranslationTarget() {
        guard let source = translationSource, source == translationTarget else { return }
        guard let next = SubtitleLanguage.translationTargets.first(where: { $0.rawValue != source }) else { return }
        translationTarget = next.rawValue
    }

    /// Yeni video seçildiğinde eski videoya ait işi, altyazıları ve geri alma yığınını temizler.
    func openVideo(_ url: URL?) {
        guard url != video else { return }
        job?.cancel()
        job = nil
        jobID = UUID()
        catalog.activeModel = nil
        sentences = []
        translations = [:]
        translationDraft = nil
        translationLanguage = nil
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
        translations = [:]
        translationDraft = nil
        translationLanguage = nil
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
                        status = .working(Self.label(for: stage, fraction: fraction, purpose: .transcription), fraction: fraction)
                    case .result(let result):
                        sentences = result.sentences
                        detectedLanguage = result.language
                        reconcileTranslationTarget()
                        // Uygulama çıkarılan WAV'ı kullanmıyor; geçici dosyayı sil.
                        try? FileManager.default.removeItem(atPath: result.audio)
                    case .translation:
                        break
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

    func render(as format: SubtitleFormat, translated: Bool = false) async -> String? {
        do {
            let list = translated ? exportSentences(translated: true) : sentences
            return try await catalog.engine?.render(list, as: format)
        } catch {
            status = .failed(error.localizedDescription)
            return nil
        }
    }

    enum JobPurpose {
        case transcription, translation
    }

    static func label(for stage: EngineEvent.Stage, fraction: Double?, purpose: JobPurpose) -> String {
        switch stage {
        case .extractingAudio:
            return "Ses ayrılıyor…"
        case .downloadingModel:
            let prefix = purpose == .translation ? "Çeviri modeli indiriliyor…" : "Model indiriliyor…"
            if let fraction {
                return "\(prefix) %\(Int((min(max(fraction, 0), 1) * 100).rounded()))"
            }
            return prefix
        case .loadingModel:
            return purpose == .translation ? "Çeviri modeli yükleniyor…" : "Model yükleniyor…"
        case .transcribing:
            return "Konuşma metne çevriliyor…"
        case .segmenting:
            return "Cümleler çıkarılıyor…"
        case .translating:
            if let fraction {
                return "Çevriliyor… %\(Int((min(max(fraction, 0), 1) * 100).rounded()))"
            }
            return "Çevriliyor…"
        case .unknown:
            return "Çalışıyor…"
        }
    }
}
