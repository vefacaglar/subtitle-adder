import EngineKit
import Foundation
import Observation

@MainActor
@Observable
final class SubtitleViewModel {
    enum Status: Equatable {
        case idle
        case working(LocalizedMessage, fraction: Double?)
        case finished
        case failed(LocalizedMessage)
    }

    /// An empty code means automatic detection; the choice is persisted in UserDefaults.
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
    /// Target language used in the translation request; the choice is persistent.
    var translationTarget: String {
        didSet {
            guard translationTarget != oldValue else { return }
            UserDefaults.standard.set(translationTarget, forKey: Self.translationTargetDefaultsKey)
        }
    }
    /// Translation model (chosen in Settings, persistent).
    var translationModel: String {
        didSet {
            guard translationModel != oldValue else { return }
            UserDefaults.standard.set(translationModel, forKey: Self.translationModelDefaultsKey)
        }
    }
    /// Whether the subtitle over the video shows the original or the translation; persistent.
    var displayMode: SubtitleDisplay {
        didSet {
            guard displayMode != oldValue else { return }
            UserDefaults.standard.set(displayMode.rawValue, forKey: Self.displayModeDefaultsKey)
        }
    }
    /// Separate speakers while transcribing (`--diarize`); persistent, on by default.
    var diarize: Bool {
        didSet {
            guard diarize != oldValue else { return }
            UserDefaults.standard.set(diarize, forKey: Self.diarizeDefaultsKey)
        }
    }
    /// Known speaker count; 0 means automatic. Persistent.
    var speakerCount: Int {
        didSet {
            guard speakerCount != oldValue else { return }
            UserDefaults.standard.set(speakerCount, forKey: Self.speakerCountDefaultsKey)
        }
    }
    var status: Status = .idle
    var detectedLanguage: String?
    var sentences: [Sentence] = []
    /// Translations keyed by the line's stable `uid`; marked "eski" (stale) when the text is edited.
    var translations: [UUID: TranslationEntry] = [:]
    /// Target language of the last successful translation (for the export label).
    var translationLanguage: String?
    /// Uncommitted text of the translation being edited.
    var translationDraft: TextDraft?
    var selection: UUID?         // `uid` of the selected line
    /// Uncommitted text of the line being edited (used while focused).
    var draft: TextDraft?
    /// Short notice when the selected model was changed automatically because it didn't match the language filter.
    private(set) var modelNotice: LocalizedMessage?
    /// The window's undo manager (provided by the view).
    @ObservationIgnored weak var undoManager: UndoManager?

    /// Shared model catalog; the toolbar and Settings use the same list.
    let catalog: ModelCatalog

    var job: Task<Void, Never>?
    var jobID = UUID()  // only the current job may change the state

    private(set) var video: URL?

    struct TextDraft: Equatable {
        var uid: UUID
        var text: String
    }

    static let diarizationModelID = "speaker-diarization"
    static let defaultModel = "large-v3"
    static let defaultTranslationModel = "madlad-3b"
    static let defaultTranslationTarget = SubtitleLanguage.english.rawValue

    private static let modelDefaultsKey = "selectedModel"
    private static let languageDefaultsKey = "selectedLanguage"
    private static let translationModelDefaultsKey = "translationModel"
    private static let translationTargetDefaultsKey = "translationTarget"
    private static let displayModeDefaultsKey = "subtitleDisplayMode"
    private static let diarizeDefaultsKey = "diarizeSpeakers"
    private static let speakerCountDefaultsKey = "speakerCount"

    init(catalog: ModelCatalog) {
        self.catalog = catalog
        let savedLanguage = UserDefaults.standard.string(forKey: Self.languageDefaultsKey) ?? ""
        // The old version accepted free text; a code not in the list falls back to automatic.
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
        diarize = UserDefaults.standard.object(forKey: Self.diarizeDefaultsKey) as? Bool ?? true
        let savedCount = UserDefaults.standard.integer(forKey: Self.speakerCountDefaultsKey)
        speakerCount = savedCount >= 2 ? savedCount : 0
        reconcileModelSelection()
        reconcileTranslationTarget()
    }

    /// Catalog filtered by language; the toolbar picker shows this.
    var availableModels: [EngineModel] { catalog.models(for: language) }

    var isWorking: Bool {
        if case .working = status { true } else { false }
    }

    /// Source language that changes with the language code (automatic if empty); passed to the translate command as `--from`.
    /// With automatic selection, the language detected by the engine is used.
    var translationSource: String? {
        if !language.isEmpty { return language }
        return detectedLanguage
    }

    /// If the selected model isn't in the filtered list, switches to the most accurate (or, on a tie, the faster) model.
    func reconcileModelSelection() {
        let available = availableModels
        guard !available.isEmpty, !available.contains(where: { $0.id == model }) else { return }
        guard let best = available.max(by: { ($0.accuracy, $0.speed) < ($1.accuracy, $1.speed) }) else { return }
        model = best.id
        if let language = SubtitleLanguage(rawValue: language) {
            let languageCode = language.rawValue
            let modelID = best.id
            modelNotice = LocalizedMessage("notice:\(languageCode):\(modelID)") {
                L("%@ için %@ seçildi.", SubtitleLanguage(rawValue: languageCode)?.shortName ?? languageCode, ModelLabel.name(for: modelID))
            }
        }
    }

    /// If the translation model in Settings isn't in the catalog (an old saved value), falls back to the most accurate model.
    func reconcileTranslationModelSelection() {
        let available = catalog.translationModels
        guard !available.isEmpty, !available.contains(where: { $0.id == translationModel }) else { return }
        translationModel = (available.max(by: { ($0.accuracy, $0.speed) < ($1.accuracy, $1.speed) }) ?? available[0]).id
    }

    /// The target language can't be the same as the source language; if it is, switches to another target.
    func reconcileTranslationTarget() {
        guard let source = translationSource, source == translationTarget else { return }
        guard let next = SubtitleLanguage.translationTargets.first(where: { $0.rawValue != source }) else { return }
        translationTarget = next.rawValue
    }

    /// When a new video is chosen, clears the old video's job, subtitles and undo stack.
    func openVideo(_ url: URL?) {
        guard url != video else { return }
        job?.cancel()
        job = nil
        jobID = UUID()
        catalog.activeModels = []
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
            status = .failed(.error(EngineError.notFound))
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
        status = .working(.key("Başlıyor…"), fraction: nil)
        let outDir = WorkingDirectory.url()
        let options = EngineClient.TranscribeOptions(
            language: language.isEmpty ? nil : language,
            model: model,
            diarize: diarize,
            speakers: speakerCount >= 2 ? speakerCount : nil
        )
        let runningModel = model
        let tracker = DownloadPhaseTracker(
            transcriptionDownloaded: catalog.models.first { $0.id == runningModel }?.downloaded ?? true,
            diarize: diarize,
            diarizationDownloaded: catalog.diarizationModels.first?.downloaded ?? true
        )
        job?.cancel()
        let id = UUID()
        jobID = id
        var running: Set<String> = [runningModel]
        if diarize { running.insert(Self.diarizationModelID) }
        catalog.activeModels = running
        job = Task {
            // However it ends (result, error, cancel), the model becomes deletable in Settings.
            defer { if jobID == id { catalog.activeModels = [] } }
            var failed = false
            // With diarization, `downloading_model` can run twice back to back; the tracker tells which model it is.
            var downloadPhases = tracker
            do {
                for try await event in engine.transcribe(video: video, outDir: outDir, options: options) {
                    guard jobID == id, !Task.isCancelled else { return }
                    switch event {
                    case .stage(let stage, let fraction):
                        let download = downloadPhases.update(stage: stage, fraction: fraction)
                        let purpose: JobPurpose = download == .diarization ? .diarization : .transcription
                        status = .working(Self.label(for: stage, fraction: fraction, purpose: purpose), fraction: fraction)
                    case .result(let result):
                        sentences = result.sentences
                        detectedLanguage = result.language
                        reconcileTranslationTarget()
                        // The app doesn't use the extracted WAV; delete the temporary file.
                        try? FileManager.default.removeItem(atPath: result.audio)
                    case .translation, .partial:
                        break
                    case .error(let message):
                        failed = true
                        status = .failed(.verbatim(message))
                    }
                }
                // On cancel the stream ends without an error; in that case the status doesn't change.
                guard jobID == id, !Task.isCancelled else { return }
                if !failed { status = .finished }
                // Make the downloaded model show as "hazır" (ready) in the menu.
                await catalog.load()
            } catch is CancellationError {
                if jobID == id { status = .idle }
            } catch {
                if jobID == id { status = .failed(.error(error)) }
            }
        }
    }

    func cancel() {
        job?.cancel()
        job = nil
        jobID = UUID()
        catalog.activeModels = []
        status = .idle
    }

    func render(as format: SubtitleFormat, translated: Bool = false) async -> String? {
        do {
            let list = translated ? exportSentences(translated: true) : sentences
            return try await catalog.engine?.render(list, as: format)
        } catch {
            status = .failed(.error(error))
            return nil
        }
    }

    enum JobPurpose {
        case transcription, translation, diarization
    }

    nonisolated static func label(for stage: EngineEvent.Stage, fraction: Double?, purpose: JobPurpose) -> LocalizedMessage {
        let fractionText = fraction.map { String($0) } ?? "-"
        let id = "stage:" + String(describing: stage) + ":" + fractionText + ":" + String(describing: purpose)
        return LocalizedMessage(id) { SubtitleViewModel.labelText(for: stage, fraction: fraction, purpose: purpose) }
    }

    nonisolated fileprivate static func labelText(for stage: EngineEvent.Stage, fraction: Double?, purpose: JobPurpose) -> String {
        switch stage {
        case .extractingAudio:
            return L("Ses ayrılıyor…")
        case .downloadingModel:
            let prefix = switch purpose {
            case .translation: L("Çeviri modeli indiriliyor…")
            case .diarization: L("Konuşmacı ayırma modeli indiriliyor…")
            case .transcription: L("Model indiriliyor…")
            }
            if let fraction {
                return "\(prefix) \(L("%lld%%", Int((min(max(fraction, 0), 1) * 100).rounded())))"
            }
            return prefix
        case .loadingModel:
            return purpose == .translation ? L("Çeviri modeli yükleniyor…") : L("Model yükleniyor…")
        case .transcribing:
            return L("Konuşma metne çevriliyor…")
        case .diarizing:
            if let fraction {
                return L("Konuşmacılar ayrılıyor… %lld%%", Int((min(max(fraction, 0), 1) * 100).rounded()))
            }
            return L("Konuşmacılar ayrılıyor…")
        case .segmenting:
            return L("Cümleler çıkarılıyor…")
        case .translating:
            if let fraction {
                return L("Çevriliyor… %lld%%", Int((min(max(fraction, 0), 1) * 100).rounded()))
            }
            return L("Çevriliyor…")
        case .unknown:
            return L("Çalışıyor…")
        }
    }
}
