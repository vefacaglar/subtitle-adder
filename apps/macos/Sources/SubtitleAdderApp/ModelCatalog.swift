import EngineKit
import Foundation
import Observation

/// Model kataloğunun tek kaynağı: araç çubuğundaki seçici, Ayarlar penceresi ve
/// altyazı üretimi aynı listeyi görür; yenileme/silme sonrası hepsi birlikte güncellenir.
@MainActor
@Observable
final class ModelCatalog {
    /// Motor bulunamazsa ya da katalog alınamazsa gösterilen liste (motor kataloğuyla aynı).
    static let fallbackModels: [EngineModel] = [
        EngineModel(id: "base", sizeMB: 145, speed: 5, accuracy: 2, downloaded: false),
        EngineModel(
            id: "gigaam-v3", backend: "onnx", languages: ["ru"],
            sizeMB: 216, speed: 5, accuracy: 4, downloaded: false
        ),
        EngineModel(id: "small", sizeMB: 465, speed: 4, accuracy: 3, downloaded: false),
        EngineModel(id: "parakeet-v3", backend: "onnx", languages: europeanLanguages,
                    sizeMB: 640, speed: 5, accuracy: 4, downloaded: false),
        EngineModel(id: "canary-1b-v2", backend: "onnx", languages: europeanLanguages,
                    sizeMB: 983, speed: 2, accuracy: 4, downloaded: false),
        EngineModel(id: "medium", sizeMB: 1500, speed: 3, accuracy: 4, downloaded: false),
        EngineModel(id: "large-v3-turbo", sizeMB: 1600, speed: 2, accuracy: 4, downloaded: false),
        EngineModel(id: "large-v3", sizeMB: 3000, speed: 1, accuracy: 5, downloaded: false),
    ]

    private static let europeanLanguages = [
        "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it",
        "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
    ]

    private(set) var models: [EngineModel] = fallbackModels
    private(set) var isLoading = false
    private(set) var loadError: String?
    private(set) var deleting: Set<String> = []
    private(set) var deleteErrors: [String: String] = [:]

    /// Süren altyazı üretiminin kullandığı model; o model Ayarlar'dan silinemez.
    var activeModel: String?

    @ObservationIgnored let engine: EngineClient?

    init(engine: EngineClient? = EngineCommand.locate().map(EngineClient.init)) {
        self.engine = engine
    }

    /// Katalogdaki toplam gerçek disk kullanımı (indirilmemişler sayılmaz).
    var totalDiskMB: Int {
        models.filter(\.downloaded).reduce(0) { $0 + ($1.diskMB ?? $1.sizeMB) }
    }

    /// Dile uygun modeller: boş dilde yalnız Whisper (dil algılar), dolu dilde
    /// o dili destekleyenler. `languages == nil` olanlar her dili destekler.
    func models(for language: String) -> [EngineModel] {
        if language.isEmpty { return models.filter(\.detectsLanguage) }
        return models.filter { $0.supports(language: language) }
    }

    /// Katalogu motordan yeniler; hata olursa eldeki liste ve hata metni korunur.
    func load() async {
        guard let engine else {
            loadError = EngineError.notFound.localizedDescription
            return
        }
        guard !isLoading else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            let list = try await engine.models()
            if !list.isEmpty { models = list }
            loadError = nil
        } catch is CancellationError {
            // Pencere kapandı; hata gösterme, sonraki açılışta yeniden denenir.
        } catch {
            loadError = error.localizedDescription
        }
    }

    /// Modeli siler; başarıda satırı "yüklü değil" durumuna geçirir.
    func delete(_ model: EngineModel) async {
        guard model.downloaded, !deleting.contains(model.id), activeModel != model.id else { return }
        guard let engine else {
            deleteErrors[model.id] = EngineError.notFound.localizedDescription
            return
        }
        deleting.insert(model.id)
        deleteErrors[model.id] = nil
        defer { deleting.remove(model.id) }
        do {
            _ = try await engine.deleteModel(model.id)
            if let index = models.firstIndex(where: { $0.id == model.id }) {
                models[index].downloaded = false
                models[index].diskMB = nil
            }
        } catch {
            deleteErrors[model.id] = error.localizedDescription
        }
    }
}
