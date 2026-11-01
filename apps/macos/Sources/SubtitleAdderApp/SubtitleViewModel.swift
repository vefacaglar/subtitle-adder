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
            guard language != oldValue, !isPlaylistRunning else { return }
            defaults.set(language, forKey: Self.languageDefaultsKey)
            // Picking a language is the user's word on what the lines are in.
            linesSourceLanguage = nil
            modelNotice = nil
            reconcileModelSelection()
            reconcileTranslationTarget()
        }
    }
    var model: String {
        didSet {
            guard model != oldValue, !isPlaylistRunning else { return }
            defaults.set(model, forKey: Self.modelDefaultsKey)
            modelNotice = nil
        }
    }
    /// Target language used in the translation request; the choice is persistent.
    var translationTarget: String {
        didSet {
            guard translationTarget != oldValue, !isPlaylistRunning else { return }
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
    /// Whether the subtitle over the video shows the original or the translation; persistent.
    /// With translations present, picking Original or Translation also switches the export language to match.
    var displayMode: SubtitleDisplay {
        didSet {
            guard displayMode != oldValue else { return }
            defaults.set(displayMode.rawValue, forKey: Self.displayModeDefaultsKey)
            guard hasTranslations else { return }
            switch displayMode {
            case .translation: exportLanguage = .translated
            case .original: exportLanguage = .original
            case .both: break
            }
        }
    }
    /// Whether the exported subtitle carries the original or the translated text. Falls back to the original
    /// when the translations go away; with translations present it moves the display mode along.
    var exportLanguage: ExportLanguage = .original {
        didSet {
            guard exportLanguage != oldValue, hasTranslations else { return }
            if exportLanguage == .translated, displayMode == .original {
                displayMode = .translation
            } else if exportLanguage == .original, displayMode == .translation {
                displayMode = .original
            }
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
    /// External LLM server on the local network (Settings > Harici LLM sunucusu): translation goes there instead of a
    /// local model. All four settings are persistent; the three that matter to the engine reach it through the launch
    /// settings.
    var externalLlmEnabled: Bool {
        didSet {
            guard externalLlmEnabled != oldValue else { return }
            defaults.set(externalLlmEnabled, forKey: Self.externalLlmEnabledDefaultsKey)
            publishLaunchSettings()
        }
    }
    /// Address of the server as typed (IP and port, with or without `http://`).
    var externalLlmUrl: String {
        didSet {
            guard externalLlmUrl != oldValue else { return }
            defaults.set(externalLlmUrl, forKey: Self.externalLlmUrlDefaultsKey)
            // Models and the test result belong to the old address.
            externalModels = []
            externalConnection = nil
            publishLaunchSettings()
        }
    }
    /// Model name on the server; empty lets the engine pick the first one the server lists.
    var externalLlmModel: String {
        didSet {
            guard externalLlmModel != oldValue else { return }
            defaults.set(externalLlmModel, forKey: Self.externalLlmModelDefaultsKey)
            publishLaunchSettings()
        }
    }
    /// With an external LLM, translate partial lines in the background while the video is still being transcribed.
    var externalLlmAsync: Bool {
        didSet {
            guard externalLlmAsync != oldValue else { return }
            defaults.set(externalLlmAsync, forKey: Self.externalLlmAsyncDefaultsKey)
        }
    }
    /// Models the server listed in the last connection test, without embedding-only ones.
    var externalModels: [String] = []
    /// Result of the last connection test, shown under the address field.
    var externalConnection: ExternalConnectionNotice?
    var isTestingExternalConnection = false
    /// Background translation (external LLM, async mode) is under way while the video is still being transcribed, or
    /// its last batch is being awaited. An explicit flag: the status label isn't parsed to find out.
    var isAsyncTranslating = false {
        didSet {
            if !isAsyncTranslating { asyncTranslationFraction = nil }
        }
    }
    /// Share of the lines translated in the background (0...0.99); `nil` while unknown or when nothing runs.
    var asyncTranslationFraction: Double?
    /// A translation job ("Çevir…", "Komple çevir" after transcription, a sequential playlist item) owns the status.
    var isTranslationJobRunning = false
    /// Playlist items handed to the external server (async mode) whose background translation hasn't finished yet.
    var playlistBackgroundPending = 0
    /// The playlist is translating in the background, whatever the status bar shows meanwhile (the next video may be
    /// transcribing).
    var isPlaylistBackgroundTranslating: Bool { playlistBackgroundPending > 0 }
    /// Show the subtitle editor to the right of the player instead of below it; persistent, off by default.
    var sideBySideLayout: Bool {
        didSet {
            guard sideBySideLayout != oldValue else { return }
            defaults.set(sideBySideLayout, forKey: Self.sideBySideLayoutDefaultsKey)
        }
    }
    /// Set by the ⌘O menu command; the main window opens its video picker and clears it.
    var videoPickerRequested = false
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
    var status: Status = .idle {
        didSet {
            if case .working(let label, let fraction) = status {
                syncActivePlaylistItem(label: label, fraction: fraction)
            }
            // A translation job lives exactly as long as the status it set.
            if case .working = status {} else { isTranslationJobRunning = false }
            // The offline retry only makes sense for the failure that registered it.
            if case .failed = status { return }
            offlineRetry = nil
        }
    }
    var detectedLanguage: String?
    /// What the lines in the editor are spoken in when a playlist run left them there: the run's item had its own
    /// source language, which the restored toolbar language doesn't know. Cleared as soon as other lines or another
    /// toolbar language come in.
    var linesSourceLanguage: String?
    var sentences: [Sentence] = [] {
        // Removed lines (and snapshots from a running job) can't stay selected.
        didSet { if !selectedUIDs.isEmpty || selection != nil { setSelectedLines(selectedUIDs, active: selection) } }
    }
    /// Translations keyed by the line's stable `uid`; marked "eski" (stale) when the text is edited.
    var translations: [UUID: TranslationEntry] = [:] {
        didSet {
            if translations.isEmpty { exportLanguage = .original }
        }
    }
    /// Target language of the last successful translation (for the export label).
    var translationLanguage: String?
    /// Uncommitted text of the translation being edited.
    var translationDraft: TextDraft?
    /// `uid` of the active line: the one the player follows and the single-line edits (split, merge, delete, timing,
    /// speaker) work on. Setting it selects just that line; `setSelectedLines` keeps a multi-selection.
    var selection: UUID? {
        didSet {
            guard !isSettingSelection else { return }
            let single: Set<UUID> = selection.map { [$0] } ?? []
            if selectedUIDs != single { selectedUIDs = single }
        }
    }
    /// All selected lines (⌘/⇧-click); `selection` is always one of them, or `nil` when the set is empty.
    private(set) var selectedUIDs: Set<UUID> = []
    @ObservationIgnored private var isSettingSelection = false
    /// Uncommitted text of the line being edited (used while focused).
    var draft: TextDraft?
    /// Short notice when the selected model was changed automatically because it didn't match the language filter.
    private(set) var modelNotice: LocalizedMessage?
    /// The window's undo manager (provided by the view).
    @ObservationIgnored weak var undoManager: UndoManager?

    /// Shared model catalog; the toolbar and Settings use the same list.
    let catalog: ModelCatalog

    /// Re-runs the job that offline mode blocked; set only together with the matching failed status.
    @ObservationIgnored var offlineRetry: (@MainActor () -> Void)?
    @ObservationIgnored let defaults: UserDefaults
    @ObservationIgnored let launchState: EngineLaunchState
    /// Fetches the server's model list for the connection test; tests replace it.
    @ObservationIgnored let externalFetch: ExternalLLMServer.Fetch
    /// Background translation of the lines transcribed so far (external LLM, async mode): the running task, the lines
    /// already handed to it, and the run it belongs to. See `SubtitleViewModel+ExternalLLM.swift`.
    @ObservationIgnored var asyncTask: Task<Void, Never>?
    @ObservationIgnored var asyncQueued: Set<UUID> = []
    @ObservationIgnored var asyncRunID = UUID()
    /// Set once transcription is over: the running batch finishes, no new one starts.
    @ObservationIgnored var asyncDraining = false
    /// A batch failed (server unreachable, error event): no further batch starts in this run, so a dead server
    /// isn't retried on every snapshot. The final translation pass picks up the lines and reports the error.
    @ObservationIgnored var asyncFailed = false
    /// Transcription is over and the status shows the background translation (label and fraction follow it).
    @ObservationIgnored var asyncShownInStatus = false
    @ObservationIgnored var connectionTestID = UUID()

    var job: Task<Void, Never>?
    var jobID = UUID()  // only the current job may change the state

    /// Which of the two views the window shows; dropping videos behaves differently on the playlist.
    var workspace: Workspace = .editor
    /// The batch playlist, in memory only. See `SubtitleViewModel+Playlist.swift`.
    var playlist: [PlaylistItem] = []
    /// A playlist run is under way: the source language, model and target are the run's, not the user's, and none
    /// of them is persisted until it ends.
    var isPlaylistRunning = false
    /// The item the foreground job (transcription, or translation in sequential mode) belongs to.
    @ObservationIgnored var activePlaylistItemID: UUID?
    @ObservationIgnored var playlistTask: Task<Void, Never>?
    /// Translate-and-save tasks of items handed to the external server, and the last one (the next waits for it).
    @ObservationIgnored var playlistBackground: [Task<Void, Never>] = []

    private(set) var video: URL?
    /// The subtitle file the lines came from (import or sidecar); where "Altyazıyı kaydet" writes when there is no video.
    var loadedSubtitle: URL?
    /// File name of the last save, shown at the end of the finished status.
    var savedSubtitleName: String?

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
    static let offlineModeDefaultsKey = "offlineMode"
    private static let autoSaveDefaultsKey = "autoSaveSubtitle"
    private static let externalLlmEnabledDefaultsKey = "externalLlmEnabled"
    private static let externalLlmUrlDefaultsKey = "externalLlmUrl"
    private static let externalLlmModelDefaultsKey = "externalLlmModel"
    private static let externalLlmAsyncDefaultsKey = "externalLlmAsync"
    private static let diarizeDefaultsKey = "diarizeSpeakers"
    private static let speakerCountDefaultsKey = "speakerCount"
    static let sideBySideLayoutDefaultsKey = "sideBySideLayout"

    init(
        catalog: ModelCatalog,
        defaults: UserDefaults = .standard,
        launchState: EngineLaunchState = .shared,
        externalFetch: ExternalLLMServer.Fetch? = nil
    ) {
        self.catalog = catalog
        self.defaults = defaults
        self.launchState = launchState
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
        let savedDisplay = defaults.string(forKey: Self.displayModeDefaultsKey) ?? ""
        displayMode = SubtitleDisplay(rawValue: savedDisplay) ?? .original
        diarize = defaults.object(forKey: Self.diarizeDefaultsKey) as? Bool ?? true
        let savedCount = defaults.integer(forKey: Self.speakerCountDefaultsKey)
        speakerCount = savedCount >= 2 ? savedCount : 0
        offlineMode = defaults.object(forKey: Self.offlineModeDefaultsKey) as? Bool ?? true
        autoSaveSubtitle = defaults.object(forKey: Self.autoSaveDefaultsKey) as? Bool ?? true
        externalLlmEnabled = defaults.bool(forKey: Self.externalLlmEnabledDefaultsKey)
        externalLlmUrl = defaults.string(forKey: Self.externalLlmUrlDefaultsKey) ?? ""
        externalLlmModel = defaults.string(forKey: Self.externalLlmModelDefaultsKey) ?? ""
        externalLlmAsync = defaults.bool(forKey: Self.externalLlmAsyncDefaultsKey)
        sideBySideLayout = defaults.bool(forKey: Self.sideBySideLayoutDefaultsKey)
        reconcileModelSelection()
        reconcileTranslationTarget()
        publishLaunchSettings()
    }

    /// Hands the offline and external LLM settings to the engine launcher; read at every engine launch.
    func publishLaunchSettings() {
        launchState.settings = EngineLaunchSettings(offline: offlineMode, externalLLM: externalLLM)
    }

    var hasTranslations: Bool { !translations.isEmpty }

    /// The mode actually shown: without translations only the original exists, whatever was chosen last.
    var effectiveDisplayMode: SubtitleDisplay { hasTranslations ? displayMode : .original }

    /// Exactly one line is selected: split, merge and the timing controls only act on a single line, so they stay
    /// unavailable for a multi-selection instead of silently picking one of its lines. Delete and retranslate take the
    /// whole selection.
    var hasSingleSelection: Bool { selectedUIDs.count == 1 && selection != nil }

    /// Selects `uids` (only those that exist). The active line is `active` if selected, else the previous active line
    /// if still selected, else the first selected line in document order; `nil` when nothing is selected.
    func setSelectedLines(_ uids: Set<UUID>, active: UUID?) {
        let existing = Set(sentences.map(\.uid))
        let valid = uids.intersection(existing)
        let newActive: UUID?
        if let active, valid.contains(active) {
            newActive = active
        } else if let selection, valid.contains(selection) {
            newActive = selection
        } else {
            newActive = sentences.first { valid.contains($0.uid) }?.uid
        }
        isSettingSelection = true
        defer { isSettingSelection = false }
        if selectedUIDs != valid { selectedUIDs = valid }
        if selection != newActive { selection = newActive }
    }

    /// The list's selection changed. The active line becomes the newly added one that lies farthest from the previous
    /// active line, which is the row the user clicked when a ⇧-click extended a range in either direction (a single
    /// ⌘-click adds just one). Without a previous active line the first added line in document order is used.
    func updateSelection(to uids: Set<UUID>) {
        let added = uids.subtracting(selectedUIDs)
        let candidates = sentences.enumerated().filter { added.contains($0.element.uid) }
        var active: UUID?
        if let selection, let anchor = sentences.index(ofUID: selection) {
            active = candidates.max { abs($0.offset - anchor) < abs($1.offset - anchor) }?.element.uid
        } else {
            active = candidates.first?.element.uid
        }
        setSelectedLines(uids, active: active)
    }

    /// A row was focused or opened its menu: inside the current multi-selection only the active line moves, anywhere
    /// else just this line is selected.
    func selectRow(_ uid: UUID) {
        if selectedUIDs.contains(uid) {
            setSelectedLines(selectedUIDs, active: uid)
        } else {
            setSelectedLines([uid], active: uid)
        }
    }

    /// Title of the translation choice in the display and export pickers, e.g. "Çeviri (es)".
    var translationTitle: String { SubtitleDisplay.translationTitle(language: translationLanguage) }

    /// Catalog filtered by language; the toolbar picker shows this.
    var availableModels: [EngineModel] { catalog.models(for: language) }

    /// A job is running, a playlist run is under way (including its background translations), or the external server
    /// is still translating lines in the background.
    var isWorking: Bool {
        if isPlaylistRunning || isAsyncTranslating { return true }
        if case .working = status { return true }
        return false
    }

    /// Translation is active, in the foreground or in the background; the editor header shows a badge for it.
    var isTranslationInProgress: Bool { isAsyncTranslating || isTranslationJobRunning || isPlaylistBackgroundTranslating }

    /// Text of the editor-header badge, e.g. "Çevriliyor… 45%"; `nil` when no translation runs.
    var translationBadgeText: String? {
        guard isTranslationInProgress else { return nil }
        if isTranslationJobRunning, case .working(let label, _) = status { return label.text }
        // Background playlist translations: show the stage while the status is a translation one, else the generic label.
        if isPlaylistBackgroundTranslating, case .working(let label, _) = status, label.id.hasSuffix(":translation") {
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
    var translationSource: String? {
        if let linesSourceLanguage { return linesSourceLanguage }
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
        // A playlist run keeps the target it started with.
        guard !isPlaylistRunning, let source = translationSource, source == translationTarget else { return }
        guard let next = SubtitleLanguage.translationTargets.first(where: { $0.rawValue != source }) else { return }
        translationTarget = next.rawValue
    }

    /// When a new video is chosen, clears the old video's job, subtitles and undo stack, then loads a subtitle file
    /// with the video's name from the same folder if there is one (no transcription needed then).
    func openVideo(_ url: URL?) {
        guard url != video else { return }
        job?.cancel()
        job = nil
        jobID = UUID()
        cancelAsyncTranslation()
        catalog.activeModels = []
        sentences = []
        translations = [:]
        translationDraft = nil
        translationLanguage = nil
        selection = nil
        detectedLanguage = nil
        linesSourceLanguage = nil
        draft = nil
        status = .idle
        undoManager?.removeAllActions()
        loadedSubtitle = nil
        savedSubtitleName = nil
        video = url
        if let url, let sidecar = SubtitleParser.findSidecar(for: url) {
            loadSubtitleFile(sidecar)
        }
    }

    /// Transcribes the video; with `thenTranslate` ("Komple çevir") the translation starts right after the result
    /// and, with auto-save on, the translated file is written when it finishes.
    func generate(thenTranslate: Bool = false) {
        guard let video else { return }
        guard let engine = catalog.engine else {
            status = .failed(.error(EngineError.notFound))
            return
        }
        var needed = [model]
        if diarize { needed.append(Self.diarizationModelID) }
        if thenTranslate { needed.append(translationModel) }
        let retry: @MainActor () -> Void = { [weak self] in self?.generate(thenTranslate: thenTranslate) }
        if blockedOffline(needed, retry: retry) { return }
        cancelAsyncTranslation()
        isTranslationJobRunning = false
        // Komple çevir and playlist items with an external LLM in async mode: the server translates while the Mac is
        // still transcribing. (A playlist item doesn't chain into `translate`; the run hands it off afterwards.)
        let asyncTranslate = (thenTranslate || isPlaylistRunning) && isExternalAsyncActive
        // Every batch of this run goes to the server it started with, even if Settings changes meanwhile.
        let asyncEngine = pinningLaunchSettings(engine)
        sentences = []
        translations = [:]
        translationDraft = nil
        translationLanguage = nil
        selection = nil
        draft = nil
        savedSubtitleName = nil
        undoManager?.removeAllActions()
        detectedLanguage = nil
        linesSourceLanguage = nil
        modelNotice = nil
        status = .working(.key("Başlıyor…"), fraction: nil)
        let outDir = WorkingDirectory.ensureExists()
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
                    case .partial(let language, let snapshot):
                        applyPartial(language: language, sentences: snapshot)
                        if asyncTranslate { startAsyncTranslation(engine: asyncEngine) }
                    case .result(let result):
                        // The last line is final now, but the closing pass takes it from here.
                        asyncDraining = true
                        sentences = sentences.mergedWithSnapshot(result.sentences)
                        detectedLanguage = result.language
                        reconcileTranslationTarget()
                        updateAsyncFraction()
                        // The app doesn't use the extracted WAV, so it goes right away instead of waiting for the
                        // quit cleanup; the subtitle outputs and engine checkpoints stay in the working folder until then.
                        try? FileManager.default.removeItem(atPath: result.audio)
                    case .translation:
                        break
                    case .error(let message):
                        failed = true
                        cancelAsyncTranslation()
                        status = .failed(.verbatim(message))
                    }
                }
                // On cancel the stream ends without an error; in that case the status doesn't change.
                guard jobID == id, !Task.isCancelled else { return }
                if failed { cancelAsyncTranslation() }
                // Let the batch already at the server finish (the status shows its progress meanwhile); the lines it
                // didn't cover are translated below.
                if asyncTranslate, !failed {
                    await finishAsyncTranslation()
                    guard jobID == id, !Task.isCancelled else { return }
                }
                // Komple çevir: hand over to the translation job, which also auto-saves. Nothing to translate
                // (no speech found) just stays finished. After background translation only the missing, outdated
                // and wrong-language lines are left to do.
                if thenTranslate, !failed, !sentences.isEmpty {
                    translate(reuseExisting: asyncTranslate)
                    return
                }
                if !failed { status = .finished }
                // Make the downloaded model show as "hazır" (ready) in the menu.
                await catalog.load()
            } catch is CancellationError {
                if jobID == id { status = .idle }
            } catch {
                if jobID == id {
                    cancelAsyncTranslation()
                    status = .failed(.error(error))
                }
            }
        }
    }

    /// Live transcription snapshot: replaces the lines, keeping the identity of the ones that are still there.
    /// The language is `"?"` until the engine has detected it.
    func applyPartial(language: String, sentences snapshot: [Sentence]) {
        sentences = sentences.mergedWithSnapshot(snapshot)
        guard !language.isEmpty, language != "?" else { return }
        detectedLanguage = language
        reconcileTranslationTarget()
    }

    func cancel() {
        cancelPlaylist()
        job?.cancel()
        job = nil
        jobID = UUID()
        cancelAsyncTranslation()
        catalog.activeModels = []
        status = .idle
    }

    func render(as format: SubtitleFormat, translated: Bool = false) async -> String? {
        guard let engine = catalog.engine else {
            status = .failed(.error(EngineError.notFound))
            return nil
        }
        // Jobs (auto-save, the playlist's save) render too, and a cancel ends the render with a CancellationError that
        // is not a failure; a render that outlives its job must not touch the status of whatever came next.
        let id = jobID
        do {
            let list = translated ? exportSentences(translated: true) : sentences
            return try await engine.render(list, as: format)
        } catch is CancellationError {
            return nil
        } catch {
            if jobID == id, !Task.isCancelled { status = .failed(.error(error)) }
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
