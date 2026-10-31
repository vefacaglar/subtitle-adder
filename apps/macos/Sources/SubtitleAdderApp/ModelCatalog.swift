import EngineKit
import Foundation
import Observation

/// Single source of truth for the model catalog: the toolbar picker, the Settings window and
/// subtitle generation see the same list; after a refresh/delete all of them update together.
@MainActor
@Observable
final class ModelCatalog {
    /// List shown when the engine isn't found or the catalog can't be fetched (same as the engine catalog).
    static let fallbackModels: [EngineModel] = [
        EngineModel(id: "base", sizeMB: 145, speed: 5, accuracy: 2, license: "MIT", downloaded: false),
        EngineModel(
            id: "gigaam-v3", backend: "onnx", languages: ["ru"],
            sizeMB: 216, speed: 5, accuracy: 4, license: "MIT", downloaded: false
        ),
        EngineModel(id: "small", sizeMB: 465, speed: 4, accuracy: 3, license: "MIT", downloaded: false),
        EngineModel(id: "parakeet-v3", backend: "onnx", languages: europeanLanguages,
                    sizeMB: 640, speed: 5, accuracy: 4, license: "CC-BY-4.0", downloaded: false),
        EngineModel(id: "canary-1b-v2", backend: "onnx", languages: europeanLanguages,
                    sizeMB: 983, speed: 2, accuracy: 4, license: "CC-BY-4.0", downloaded: false),
        EngineModel(id: "medium", sizeMB: 1500, speed: 3, accuracy: 4, license: "MIT", downloaded: false),
        EngineModel(id: "large-v3-turbo", sizeMB: 1600, speed: 2, accuracy: 4, license: "MIT", downloaded: false),
        EngineModel(id: "large-v3", sizeMB: 3000, speed: 1, accuracy: 5, license: "MIT", downloaded: false),
        EngineModel(
            id: "nllb-1.3b", kind: "translation", backend: "ctranslate2",
            languages: translationLanguages, sizeMB: 1336, speed: 3, accuracy: 4,
            license: "CC-BY-NC-4.0", downloaded: false
        ),
        EngineModel(
            id: "madlad-3b", kind: "translation", backend: "ctranslate2",
            languages: translationLanguages, sizeMB: 2839, speed: 2, accuracy: 5,
            license: "Apache-2.0", downloaded: false
        ),
    ]

    private static let translationLanguages = ["tr", "en", "pt", "es", "ru"]

    private static let europeanLanguages = [
        "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it",
        "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
    ]

    private(set) var models: [EngineModel] = fallbackModels
    private(set) var isLoading = false
    private(set) var loadError: String?
    private(set) var deleting: Set<String> = []
    private(set) var deleteErrors: [String: String] = [:]

    /// The model used by the running subtitle generation or translation; that model can't be deleted from Settings.
    var activeModel: String?

    @ObservationIgnored let engine: EngineClient?

    init(engine: EngineClient? = EngineCommand.locate().map(EngineClient.init)) {
        self.engine = engine
    }

    /// Total actual disk usage in the catalog (models not downloaded are not counted).
    var totalDiskMB: Int {
        models.filter(\.downloaded).reduce(0) { $0 + ($1.diskMB ?? $1.sizeMB) }
    }

    /// Subtitle models the toolbar picker sees (translation models excluded).
    func models(for language: String) -> [EngineModel] {
        let transcription = models.filter { !$0.isTranslation }
        if language.isEmpty { return transcription.filter(\.detectsLanguage) }
        return transcription.filter { $0.supports(language: language) }
    }

    /// The "Çeviri modelleri" section in Settings and the default translation model picker.
    var translationModels: [EngineModel] {
        models.filter(\.isTranslation)
    }

    /// Refreshes the catalog from the engine; on error the current list and the error text are kept.
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
            // The window closed; don't show an error, it will be retried on the next open.
        } catch {
            loadError = error.localizedDescription
        }
    }

    /// Deletes the model; on success sets the row to the "yüklü değil" state.
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
