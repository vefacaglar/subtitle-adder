import EngineKit
import Foundation
import LibraryKit
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

    // MARK: - Toolbar choices

    /// An empty code means automatic detection; the choice is persisted in UserDefaults.
    var language: String {
        didSet {
            guard language != oldValue else { return }
            defaults.set(language, forKey: Self.languageDefaultsKey)
            // Picking a language is the user's word on what the lines are in.
            foreground.linesSourceLanguage = nil
            modelNotice = nil
            reconcileModelSelection()
            reconcileTranslationTarget()
        }
    }
    var model: String {
        didSet {
            guard model != oldValue else { return }
            defaults.set(model, forKey: Self.modelDefaultsKey)
            modelNotice = nil
        }
    }
    /// Target language used in the translation request; the choice is persistent.
    var translationTarget: String {
        didSet {
            guard translationTarget != oldValue else { return }
            defaults.set(translationTarget, forKey: Self.translationTargetDefaultsKey)
        }
    }
    /// Translation model (chosen in Settings, persistent).
    var translationModel: String {
        didSet {
            guard translationModel != oldValue else { return }
            defaults.set(translationModel, forKey: Self.translationModelDefaultsKey)
        }
    }
    /// Strict offline mode (Settings > Gizlilik): the engine never touches the network, so missing models
    /// aren't downloaded. Persistent, on by default.
    var offlineMode: Bool {
        didSet {
            guard offlineMode != oldValue else { return }
            defaults.set(offlineMode, forKey: Self.offlineModeDefaultsKey)
            publishLaunchSettings()
        }
    }
    /// Translation providers (cloud APIs, servers on the local network) and which one is active; the source of truth
    /// for the external LLM. Change it only through the provider methods in `SubtitleViewModel+Providers.swift`, which
    /// also hand the result to the engine launcher.
    let providerStore: TranslationProviderStore
    /// What the app knows about each provider's API key, read off the main actor; the views and the launch settings are
    /// made from it. See `SubtitleViewModel+ProviderKeys.swift`.
    var providerKeys: [UUID: ProviderKey] = [:]
    /// Counts the writes of a provider's key, so a read that was under way when the key changed is dropped.
    @ObservationIgnored var providerKeyEpochs: [UUID: Int] = [:]
    /// The read of every saved provider's key started at launch.
    @ObservationIgnored var providerKeyLoad: Task<Void, Never>?
    /// Result of the last connection test per provider, shown under its row in Settings.
    var providerNotices: [UUID: ExternalConnectionNotice] = [:]
    /// Providers whose connection test is running.
    var testingProviders: Set<UUID> = []
    /// Show the subtitle editor to the right of the player instead of below it; persistent, off by default.
    var sideBySideLayout: Bool {
        didSet {
            guard sideBySideLayout != oldValue else { return }
            defaults.set(sideBySideLayout, forKey: Self.sideBySideLayoutDefaultsKey)
        }
    }
    /// Hides the subtitle panel so the player fills the area; persistent, off by default. Full screen hides it too,
    /// without touching this choice.
    var hideSubtitlePanel: Bool {
        didSet {
            guard hideSubtitlePanel != oldValue else { return }
            defaults.set(hideSubtitlePanel, forKey: Self.hideSubtitlePanelDefaultsKey)
        }
    }
    /// Starts playing a video as soon as it is opened; persistent, on by default.
    var autoPlayVideo: Bool {
        didSet {
            guard autoPlayVideo != oldValue else { return }
            defaults.set(autoPlayVideo, forKey: Self.autoPlayVideoDefaultsKey)
        }
    }
    /// Hides the window toolbar so the player and the lines get the room; not persisted.
    var isTheaterMode = false
    /// Separate speakers while transcribing (`--diarize`); persistent, on by default.
    var diarize: Bool {
        didSet {
            guard diarize != oldValue else { return }
            defaults.set(diarize, forKey: Self.diarizeDefaultsKey)
        }
    }
    /// Known speaker count; 0 means automatic. Persistent.
    var speakerCount: Int {
        didSet {
            guard speakerCount != oldValue else { return }
            defaults.set(speakerCount, forKey: Self.speakerCountDefaultsKey)
        }
    }
    /// Writes the translation next to the video as soon as a translation job finishes; persistent, on by default.
    var autoSaveSubtitle: Bool {
        didSet {
            guard autoSaveSubtitle != oldValue else { return }
            defaults.set(autoSaveSubtitle, forKey: Self.autoSaveDefaultsKey)
        }
    }
    /// Format of "Altyazıyı kaydet", auto-save and the export panel.
    var exportFormat: SubtitleFormat = .srt
    /// Short notice when the selected model was changed automatically because it didn't match the language filter.
    var modelNotice: LocalizedMessage?
    /// The window's undo manager (provided by the view). There is one undo history, the foreground session's: it is
    /// cleared whenever another video comes to the front.
    @ObservationIgnored weak var undoManager: UndoManager?

    /// Shared model catalog; the toolbar and Settings use the same list.
    let catalog: ModelCatalog

    @ObservationIgnored let defaults: UserDefaults
    @ObservationIgnored let launchState: EngineLaunchState
    /// Fetches the server's model list for the connection test; tests replace it.
    @ObservationIgnored let externalFetch: ExternalLLMServer.Fetch
    /// At most one translation runs at a time in the whole app (the editor's job, the closing pass, the background
    /// batches, the playlist's stage 3): every `engine.translate` goes through this gate. See `TranslationGate`.
    @ObservationIgnored let translationGate = TranslationGate()
    /// The running connection test of each provider; a newer test (or an edit) makes an older answer obsolete.
    @ObservationIgnored var providerTestIDs: [UUID: UUID] = [:]

    // MARK: - Sessions and jobs

    /// The session on screen: its lines, status and progress are what the editor, the player overlay and the status bar
    /// show, and the per-video properties of this view model (`sentences`, `status`, `translations`, …) are its.
    /// Switch it only through `openVideo`. See `SubtitleViewModel+Sessions.swift`.
    var foreground: VideoSession
    /// The sessions of the videos opened or worked on during this app run, by standardized file URL. The foreground
    /// session of a video is in here too; a session without a video (a subtitle file on its own) is not.
    @ObservationIgnored var sessions: [URL: VideoSession] = [:]
    /// Single-video jobs of this run, oldest first: running, waiting and finished ones. The list tab shows them next
    /// to the batch playlist's items.
    var recentJobs: [VideoSession] = []
    /// The lane for single-video jobs, shared with the batch playlist's recognition. See `JobQueue`.
    let jobQueue = JobQueue()
    /// Local models that running jobs use, by the job that claimed them; the union is what Settings can't delete.
    @ObservationIgnored var modelClaims: [UUID: Set<String>] = [:]

    /// Which of the views the window shows; dropping videos behaves differently on the playlist.
    var workspace: Workspace = .editor
    /// The batch playlist, in memory only. See `SubtitleViewModel+Playlist.swift`.
    var playlist: [PlaylistItem] = []
    /// Why the playlist run could not start (offline mode, no engine); shown in the list tab. The status of no video
    /// carries it.
    var playlistNotice: PlaylistNotice?
    /// A playlist run is under way. Each item works in its own session and the run uses the settings it started with,
    /// so the editor, the player and the toolbar stay the user's.
    var isPlaylistRunning = false
    /// Playlist items handed to translation (stage 3) whose translation hasn't finished yet.
    var playlistBackgroundPending = 0
    /// The playlist is translating in the background (the next video may be transcribing meanwhile).
    var isPlaylistBackgroundTranslating: Bool { playlistBackgroundPending > 0 }
    @ObservationIgnored var playlistTask: Task<Void, Never>?
    /// Translate-and-save tasks of the items handed off after recognition, in order. The run hands an item off only
    /// after the one before it is over, so at most one of them is unfinished.
    @ObservationIgnored var playlistBackground: [Task<Void, Never>] = []
    /// Audio extraction and scheduling state of the run under way.
    @ObservationIgnored var playlistPipeline: PlaylistPipeline?
    /// The list's row is being copied into the video's session; the session's own status hook ignores that.
    @ObservationIgnored var isMirroringPlaylist = false
    /// This Mac's memory in bytes; decides whether a local translation may overlap the next video's recognition (see
    /// `localTranslationOverlapMemory`). Tests set it.
    @ObservationIgnored var physicalMemory = ProcessInfo.processInfo.physicalMemory

    /// Why an open was refused (a subtitle onto a video the playlist run is working on); shown as an alert.
    var openRefusal: LocalizedMessage?
    /// An open request that waits for the user's answer because it would lose a running job or edited lines.
    var pendingOpen: PendingOpen?
    /// Called with the file after every subtitle written next to a video (editor save, auto-save, playlist), so the file
    /// sidebar can rescan that folder.
    @ObservationIgnored var onSubtitleSaved: (@MainActor (URL) -> Void)?

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
    static let displayModeDefaultsKey = "subtitleDisplayMode"
    static let offlineModeDefaultsKey = "offlineMode"
    private static let autoSaveDefaultsKey = "autoSaveSubtitle"
    private static let diarizeDefaultsKey = "diarizeSpeakers"
    private static let speakerCountDefaultsKey = "speakerCount"
    static let sideBySideLayoutDefaultsKey = "sideBySideLayout"
    static let hideSubtitlePanelDefaultsKey = "hideSubtitlePanel"
    static let autoPlayVideoDefaultsKey = "autoPlayVideo"

    init(
        catalog: ModelCatalog,
        defaults: UserDefaults = .standard,
        launchState: EngineLaunchState = .shared,
        externalFetch: ExternalLLMServer.Fetch? = nil,
        providerStore: TranslationProviderStore? = nil
    ) {
        self.catalog = catalog
        self.defaults = defaults
        self.launchState = launchState
        // The app passes the real store (file + keychain); anything else gets one that lives and dies with this model, so
        // tests never touch the user's providers or keychain.
        self.providerStore = providerStore ?? Self.makeEphemeralProviderStore()
        self.externalFetch = externalFetch ?? ExternalLLMServer.urlSessionFetch()
        let savedLanguage = defaults.string(forKey: Self.languageDefaultsKey) ?? ""
        // The old version accepted free text; a code not in the list falls back to automatic.
        language = SubtitleLanguage(rawValue: savedLanguage) == nil ? "" : savedLanguage
        model = defaults.string(forKey: Self.modelDefaultsKey) ?? Self.defaultModel
        let savedTarget = defaults.string(forKey: Self.translationTargetDefaultsKey) ?? ""
        translationTarget = SubtitleLanguage(rawValue: savedTarget).map(\.rawValue)
            .flatMap { $0 == SubtitleLanguage.automatic.rawValue ? nil : $0 }
            ?? Self.defaultTranslationTarget
        translationModel = defaults.string(forKey: Self.translationModelDefaultsKey)
            ?? Self.defaultTranslationModel
        diarize = defaults.object(forKey: Self.diarizeDefaultsKey) as? Bool ?? true
        let savedCount = defaults.integer(forKey: Self.speakerCountDefaultsKey)
        speakerCount = savedCount >= 2 ? savedCount : 0
        offlineMode = defaults.object(forKey: Self.offlineModeDefaultsKey) as? Bool ?? true
        autoSaveSubtitle = defaults.object(forKey: Self.autoSaveDefaultsKey) as? Bool ?? true
        sideBySideLayout = defaults.bool(forKey: Self.sideBySideLayoutDefaultsKey)
        hideSubtitlePanel = defaults.bool(forKey: Self.hideSubtitlePanelDefaultsKey)
        autoPlayVideo = defaults.object(forKey: Self.autoPlayVideoDefaultsKey) as? Bool ?? true
        foreground = VideoSession(video: nil, displayMode: Self.storedDisplayMode(in: defaults))
        wire(foreground)
        jobQueue.start = { [weak self] entry in self?.begin(entry) }
        // Once: the single LAN server of earlier versions becomes the first provider.
        self.providerStore.migrateLegacyIfNeeded(defaults: defaults)
        reconcileModelSelection()
        reconcileTranslationTarget()
        // The keychain is not touched here: the keys are read by a background task, which publishes the launch settings
        // again once it has them. Until then the active provider goes along without its key.
        publishLaunchSettings()
        startLoadingProviderKeys()
    }

    /// Hands the offline and external LLM settings to the engine launcher; read at every engine launch. Never touches
    /// the keychain: the active provider's key comes from the cache (`providerKeys`).
    func publishLaunchSettings() {
        launchState.settings = EngineLaunchSettings(offline: offlineMode, externalLLM: cachedExternalLLM())
    }

    /// Catalog filtered by language; the toolbar picker shows this.
    var availableModels: [EngineModel] { catalog.models(for: language) }

    /// The video on screen is being worked on (a job runs, waits for its turn, or the external server is still
    /// translating lines in the background). Other videos' jobs and the playlist run don't count: they don't keep this
    /// one from being edited.
    var isWorking: Bool { foreground.isWorking }

    /// Something is going on anywhere in the app: a job of any video, a queued one, or a playlist run. Decides whether
    /// quitting may clear the engine's scratch folder.
    var hasActiveWork: Bool {
        isPlaylistRunning || jobQueue.isBusy || foreground.isWorking || sessions.values.contains { $0.isWorking }
    }

    /// Translation is active for the video on screen, in the foreground or in the background; the editor header shows a
    /// badge for it.
    var isTranslationInProgress: Bool {
        foreground.isAsyncTranslating || foreground.isTranslationJobRunning || foreground.isPlaylistTranslating
    }

    /// Text of the editor-header badge, e.g. "Çevriliyor… 45%"; `nil` when no translation runs.
    var translationBadgeText: String? {
        guard isTranslationInProgress else { return nil }
        if foreground.isTranslationJobRunning, case .working(let label, _) = status { return label.text }
        // Background playlist translation: show the stage while the status is a translation one, else the generic label.
        if foreground.isPlaylistTranslating, case .working(let label, _) = status, label.id.hasSuffix(":translation") {
            return label.text
        }
        return Self.label(for: .translating, fraction: asyncTranslationFraction, purpose: .translation).text
    }

    /// Share of `total` lines that are done, held below 1 while the job runs (the last step is the save or the final
    /// batch, not the progress). Unknown (`nil`) when nothing is done yet.
    nonisolated static func translationProgress(done: Int, total: Int) -> Double? {
        guard done > 0, total > 0 else { return nil }
        return min(max(Double(done) / Double(total), 0), 0.99)
    }

    /// Progress of a translation that skips `alreadyDone` lines and sends `requested` more: `part` is the engine's own
    /// 0...1 fraction for the request. Capped like `translationProgress`.
    nonisolated static func translationProgress(alreadyDone: Int, requested: Int, part: Double, total: Int) -> Double {
        guard total > 0 else { return min(max(part, 0), 0.99) }
        let value = (Double(alreadyDone) + Double(requested) * min(max(part, 0), 1)) / Double(total)
        return min(max(value, 0), 0.99)
    }

    /// Source language that changes with the language code (automatic if empty); passed to the translate command as `--from`.
    /// With automatic selection, the language detected by the engine is used.
    var translationSource: String? { foreground.translationSource(language: language) }

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

    /// The target a job should use: the requested one, or another when it equals the source language.
    nonisolated static func reconciledTarget(_ target: String, source: String?) -> String {
        guard let source, source == target else { return target }
        return SubtitleLanguage.translationTargets.first(where: { $0.rawValue != source })?.rawValue ?? target
    }

    // MARK: - Labels

    enum JobPurpose {
        case transcription, translation, diarization
    }

    nonisolated static func label(for stage: EngineEvent.Stage, fraction: Double?, purpose: JobPurpose) -> LocalizedMessage {
        let fractionText = fraction.map { String($0) } ?? "-"
        let id = "stage:" + String(describing: stage) + ":" + fractionText + ":" + String(describing: purpose)
        return LocalizedMessage(id) { SubtitleViewModel.labelText(for: stage, fraction: fraction, purpose: purpose) }
    }

    /// Whole percent (0...100) of a 0...1 fraction, clamped and rounded.
    nonisolated static func percent(_ fraction: Double) -> Int {
        Int((min(max(fraction, 0), 1) * 100).rounded())
    }

    nonisolated fileprivate static func labelText(for stage: EngineEvent.Stage, fraction: Double?, purpose: JobPurpose) -> String {
        switch stage {
        case .extractingAudio:
            if let fraction {
                return L("Ses ayrılıyor… %lld%%", percent(fraction))
            }
            return L("Ses ayrılıyor…")
        case .downloadingModel:
            let prefix = switch purpose {
            case .translation: L("Çeviri modeli indiriliyor…")
            case .diarization: L("Konuşmacı ayırma modeli indiriliyor…")
            case .transcription: L("Model indiriliyor…")
            }
            if let fraction {
                return "\(prefix) \(L("%lld%%", percent(fraction)))"
            }
            return prefix
        case .loadingModel:
            return purpose == .translation ? L("Çeviri modeli yükleniyor…") : L("Model yükleniyor…")
        case .transcribing:
            if let fraction {
                return L("Konuşma metne çevriliyor… %lld%%", percent(fraction))
            }
            return L("Konuşma metne çevriliyor…")
        case .diarizing:
            if let fraction {
                return L("Konuşmacılar ayrılıyor… %lld%%", percent(fraction))
            }
            return L("Konuşmacılar ayrılıyor…")
        case .segmenting:
            return L("Cümleler çıkarılıyor…")
        case .translating:
            if let fraction {
                return L("Çevriliyor… %lld%%", percent(fraction))
            }
            return L("Çevriliyor…")
        case .unknown:
            return L("Çalışıyor…")
        }
    }
}
