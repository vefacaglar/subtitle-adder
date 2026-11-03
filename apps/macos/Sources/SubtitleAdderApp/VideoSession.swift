import EngineKit
import Foundation
import Observation

/// Everything the app knows about one video: its lines and translations, languages, status and progress, the job that
/// works on it, the subtitle file it came from and how it is displayed and exported. The view model owns one session
/// per video that was opened or worked on during this app run and shows exactly one of them (the foreground session);
/// its per-video properties forward to that one. A job holds on to its own session, so it keeps writing there whichever
/// video is on screen.
///
/// A session knows nothing about the toolbar, the engine or other sessions: it is state plus the transformations that
/// only touch that state. Everything that needs settings, the engine or the queue lives in the view model.
@MainActor
@Observable
final class VideoSession {
    typealias Status = SubtitleViewModel.Status
    typealias TextDraft = SubtitleViewModel.TextDraft

    /// The video, as it was first opened. `nil` for lines that were opened without one (a subtitle file on its own);
    /// such a session is not kept when another file is opened.
    let video: URL?

    // MARK: - Lines

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
    /// `uid` of the active line: the one the player follows and the single-line edits work on. Setting it selects just
    /// that line; `setSelectedLines` keeps a multi-selection.
    var selection: UUID? {
        didSet {
            guard !isSettingSelection else { return }
            let single: Set<UUID> = selection.map { [$0] } ?? []
            if selectedUIDs != single { selectedUIDs = single }
        }
    }
    /// All selected lines; `selection` is always one of them, or `nil` when the set is empty.
    private(set) var selectedUIDs: Set<UUID> = []
    @ObservationIgnored private var isSettingSelection = false
    /// Uncommitted text of the line being edited (used while focused).
    var draft: TextDraft?
    var detectedLanguage: String?
    /// What the lines are spoken in when the toolbar language doesn't say so: a batch run's item had its own source
    /// language, and a job keeps the language it started with after the toolbar moved on. Cleared when the user picks a
    /// language while this session is on screen.
    var linesSourceLanguage: String?

    // MARK: - Display and export

    /// Whether the subtitle over the video shows the original or the translation. With translations present, picking
    /// Original or Translation also switches the export language to match.
    var displayMode: SubtitleDisplay {
        didSet {
            guard displayMode != oldValue else { return }
            onDisplayModeChange?(self)
            guard hasTranslations else { return }
            switch displayMode {
            case .translation: exportLanguage = .translated
            case .original: exportLanguage = .original
            case .both: break
            }
        }
    }
    /// Whether the exported subtitle carries the original or the translated text. Falls back to the original when the
    /// translations go away; with translations present it moves the display mode along.
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

    // MARK: - Files

    /// The subtitle file the lines came from (import or sidecar); where "Altyazıyı kaydet" writes when there is no video.
    var loadedSubtitle: URL?
    /// File name of the last save, shown at the end of the finished status. A save (the editor's, auto-save, the
    /// playlist's) is what sets it, and what it wrote is no longer unsaved work.
    var savedSubtitleName: String? {
        didSet { if savedSubtitleName != nil { hasUnsavedEdits = false } }
    }
    /// The lines or their translations were edited, or a translation was applied, since they were opened or last saved.
    var hasUnsavedEdits = false

    // MARK: - Status

    var status: Status = .idle {
        didSet {
            // A translation job lives exactly as long as the status it set.
            if case .working = status {} else { isTranslationJobRunning = false }
            // The offline retry only makes sense for the failure that registered it.
            if case .failed = status {} else { offlineRetry = nil }
            onStatusChange?(self)
        }
    }
    /// A translation job ("Çevir…", "Komple çevir" after transcription) owns the status.
    var isTranslationJobRunning = false
    /// Background translation (external LLM, async mode) is under way while the video is still being transcribed, or
    /// its last batch is being awaited. An explicit flag: the status label isn't parsed to find out.
    var isAsyncTranslating = false {
        didSet {
            if !isAsyncTranslating { asyncTranslationFraction = nil }
        }
    }
    /// Share of the lines translated in the background (0...0.99); `nil` while unknown or when nothing runs.
    var asyncTranslationFraction: Double?
    /// The batch playlist is translating this video's lines (stage 3) while the status may show something else.
    var isPlaylistTranslating = false

    // MARK: - Job bookkeeping (not shown anywhere)

    /// The running job (recognition, translation) of this video, if any.
    @ObservationIgnored var job: Task<Void, Never>?
    /// Only the current job may change the state.
    @ObservationIgnored var jobID = UUID()
    /// Re-runs the job that offline mode blocked; set only together with the matching failed status.
    @ObservationIgnored var offlineRetry: (@MainActor () -> Void)?
    /// The batch playlist item whose run owns this session right now (extraction, recognition, translation).
    @ObservationIgnored var batchItemID: UUID?
    /// Background translation of the lines transcribed so far: the running task, the lines already handed to it, and
    /// the run it belongs to. See `SubtitleViewModel+ExternalLLM.swift`.
    @ObservationIgnored var asyncTask: Task<Void, Never>?
    @ObservationIgnored var asyncQueued: Set<UUID> = []
    @ObservationIgnored var asyncRunID = UUID()
    /// Set once transcription is over: the running batch finishes, no new one starts.
    @ObservationIgnored var asyncDraining = false
    /// Speech recognition is finished: every line is final, so the batch holdback and the minimum batch size no longer
    /// apply and the tail can be translated while speaker analysis still runs.
    @ObservationIgnored var asyncTranscriptionComplete = false
    /// A batch failed (server unreachable, error event): no further batch starts in this run.
    @ObservationIgnored var asyncFailed = false
    /// Transcription is over and the status shows the background translation (label and fraction follow it).
    @ObservationIgnored var asyncShownInStatus = false
    /// Told after every status change; the view model keeps the list tab's batch rows in step with it.
    @ObservationIgnored var onStatusChange: (@MainActor (VideoSession) -> Void)?
    /// Told when the display mode changes, so the view model can remember the foreground session's choice.
    @ObservationIgnored var onDisplayModeChange: (@MainActor (VideoSession) -> Void)?

    init(video: URL?, displayMode: SubtitleDisplay = .original) {
        self.video = video
        self.displayMode = displayMode
    }

    // MARK: - Derived state

    var hasTranslations: Bool { !translations.isEmpty }

    /// The mode actually shown: without translations only the original exists, whatever was chosen last.
    var effectiveDisplayMode: SubtitleDisplay { hasTranslations ? displayMode : .original }

    /// Title of the translation choice in the display and export pickers, e.g. "Çeviri (es)".
    var translationTitle: String { SubtitleDisplay.translationTitle(language: translationLanguage) }

    /// A job is running here, or the external server is still translating lines in the background.
    var isWorking: Bool {
        if isAsyncTranslating { return true }
        if case .working = status { return true }
        return false
    }

    /// Exactly one line is selected: split, merge and the timing controls only act on a single line.
    var hasSingleSelection: Bool { selectedUIDs.count == 1 && selection != nil }

    /// What the lines are spoken in: the language they were pinned to, else the toolbar's `language` (automatic if
    /// empty), else the one the engine detected.
    func translationSource(language: String) -> String? {
        if let linesSourceLanguage { return linesSourceLanguage }
        if !language.isEmpty { return language }
        return detectedLanguage
    }

    /// Worth keeping when the user leaves the video: it has lines, a job or a queue entry still belongs to it, or it
    /// carries something that can't be read off the disk again. An empty idle session is dropped.
    var hasContent: Bool {
        !sentences.isEmpty || job != nil || isWorking
    }

    /// Reopening the video would give the very same session: its lines are the sidecar's as they are on disk, and
    /// nothing was added to them.
    var isReconstructible: Bool {
        guard job == nil, !isWorking, !hasUnsavedEdits, translations.isEmpty, loadedSubtitle != nil else { return false }
        if case .failed = status { return false }
        return true
    }

    /// "N satır · dil: xx" (the language is left out when unknown), plus the translation language and the saved file.
    var finishedSummary: String {
        Self.summary(lines: sentences.count, detected: detectedLanguage, translation: translationLanguage, saved: savedSubtitleName)
    }

    nonisolated static func summary(lines: Int, detected: String?, translation: String?, saved: String?) -> String {
        var summary = detected.map { L("%lld satır · dil: %@", lines, $0) } ?? L("%lld satır", lines)
        if let translation {
            summary += L(" · çeviri: %@", translation)
        }
        if let saved {
            summary += L(" · kaydedildi: %@", saved)
        }
        return summary
    }

    // MARK: - Resetting

    /// A run that transcribes from scratch: nothing of the earlier lines is kept.
    func resetForNewRun() {
        sentences = []
        translations = [:]
        translationDraft = nil
        translationLanguage = nil
        selection = nil
        draft = nil
        savedSubtitleName = nil
        hasUnsavedEdits = false
        detectedLanguage = nil
        linesSourceLanguage = nil
    }

    /// Lines from a subtitle file: translations, edits and the display choice go back to the original.
    func load(subtitle url: URL, lines: [Sentence]) {
        loadedSubtitle = url
        savedSubtitleName = nil
        hasUnsavedEdits = false
        translations = [:]
        translationDraft = nil
        translationLanguage = nil
        selection = nil
        draft = nil
        // A subtitle file doesn't say which language it is in.
        detectedLanguage = nil
        linesSourceLanguage = nil
        displayMode = .original
        exportLanguage = .original
        sentences = lines
        status = .finished
    }

    // MARK: - Selection

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

    // MARK: - Translations

    func translation(for sentence: Sentence) -> TranslationEntry? {
        translations[sentence.uid]
    }

    func isTranslationStale(for sentence: Sentence) -> Bool {
        translations[sentence.uid]?.isStale(for: sentence) ?? false
    }

    /// Whether the line shows the review marker: its translation's flag, or (without a translation) the flag the line
    /// itself carries from an imported JSON file.
    func needsReview(for sentence: Sentence) -> Bool {
        translations[sentence.uid]?.needsReview ?? sentence.needsReview
    }

    /// If the translation is chosen on export, the times are kept and the text is replaced by the translation.
    func exportSentences(translated: Bool) -> [Sentence] {
        guard translated else { return sentences }
        return sentences.map { sentence in
            let entry = translations[sentence.uid]
            return SubtitleViewModel.exportLine(sentence, text: entry?.text, needsReview: entry?.needsReview ?? false)
        }
    }

    /// Stores translated lines. A result the engine flagged keeps the line's earlier translation (only the marker is
    /// set) when that is in the same language or `preserveOtherLanguages` is on (a selected retry); otherwise it shows
    /// the source text the engine returned, flagged, so a full translation into a new language never mixes in the old
    /// language. The earlier translation is looked up in `reviewFallback` (the translations from before the job) when
    /// given: every batch event rebuilds the dictionary, so `translations` itself only holds what earlier batches stored.
    /// With `merge` the translations of other lines (same language) are kept and these are added to them.
    func storeTranslations(
        _ pairs: [(result: Sentence, original: Sentence)], language: String, merge: Bool,
        preserveOtherLanguages: Bool = false, reviewFallback: [UUID: TranslationEntry]? = nil
    ) {
        let hadTranslations = hasTranslations
        let previous = reviewFallback ?? translations
        var entries: [UUID: TranslationEntry] = merge
            && (preserveOtherLanguages || translationLanguage == language || translations.isEmpty)
            ? translations
            : [:]
        for (result, original) in pairs {
            if result.needsReview, var existing = previous[original.uid],
               preserveOtherLanguages || existing.language == language {
                // A failed retry keeps what the line had (even if stale); only the marker is set.
                existing.needsReview = true
                entries[original.uid] = existing
                continue
            }
            entries[original.uid] = TranslationEntry(
                text: result.text,
                sourceText: original.text,
                language: language,
                needsReview: result.needsReview
            )
        }
        translations = entries
        // A translation that arrived is work the file on disk doesn't have yet (until a save writes it).
        if !pairs.isEmpty { hasUnsavedEdits = true }
        translationLanguage = language
        // The first translation to arrive switches the video overlay and the export to the translated text.
        if !hadTranslations, hasTranslations {
            if displayMode == .original { displayMode = .translation }
            exportLanguage = .translated
        }
    }

    /// Live transcription snapshot: replaces the lines, keeping the identity of the ones that are still there. The
    /// language is `"?"` until the engine has detected it; returns whether it is known now.
    @discardableResult
    func applyPartial(language: String, sentences snapshot: [Sentence]) -> Bool {
        sentences = sentences.mergedWithSnapshot(snapshot)
        guard !language.isEmpty, language != "?" else { return false }
        detectedLanguage = language
        return true
    }

    // MARK: - Background translation

    /// Fewest new lines worth a request: smaller batches lose the context a model translates well with.
    nonisolated static var minAsyncBatchSentences: Int { 10 }

    /// Lines ready for the server: everything but the last two (the live end of the transcript still changes), not
    /// handed over before, with text. Empty until at least `minAsyncBatchSentences` of them have piled up. Once
    /// recognition is complete (`asyncTranscriptionComplete`) every line counts and a single one is enough.
    func nextAsyncBatch() -> [Sentence] {
        let complete = asyncTranscriptionComplete
        guard !asyncDraining, !asyncFailed, complete ? !sentences.isEmpty : sentences.count > 2 else { return [] }
        let stable = complete ? sentences[...] : sentences.dropLast(2)
        let candidates = stable.filter {
            !asyncQueued.contains($0.uid) && !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
        return candidates.count >= (complete ? 1 : Self.minAsyncBatchSentences) ? Array(candidates) : []
    }

    /// Drops the background translation: stops the server request and forgets which lines were handed over.
    func cancelAsyncTranslation() {
        asyncTask?.cancel()
        asyncTask = nil
        asyncRunID = UUID()
        asyncQueued = []
        asyncDraining = false
        asyncTranscriptionComplete = false
        asyncFailed = false
        asyncShownInStatus = false
        isAsyncTranslating = false
    }

    /// Transcription is over: no new batch starts, the one at the server finishes. Whatever is left is the final
    /// translation pass's job.
    func finishAsyncTranslation() async {
        asyncDraining = true
        if let task = asyncTask {
            asyncShownInStatus = true
            mirrorAsyncInStatus()
            await task.value
        }
        asyncShownInStatus = false
        isAsyncTranslating = false
    }

    /// Background progress from the translations at hand: translated lines over all lines, below 1.
    func updateAsyncFraction() {
        guard isAsyncTranslating else { return }
        setAsyncFraction(SubtitleViewModel.translationProgress(done: translations.count, total: sentences.count))
    }

    func setAsyncFraction(_ fraction: Double?) {
        asyncTranslationFraction = fraction
        mirrorAsyncInStatus()
    }

    /// After transcription the status is the background translation itself.
    private func mirrorAsyncInStatus() {
        guard asyncShownInStatus, case .working = status else { return }
        status = .working(
            SubtitleViewModel.label(for: .translating, fraction: asyncTranslationFraction, purpose: .translation),
            fraction: asyncTranslationFraction
        )
    }

    /// Adds the translated lines of a batch, but only to lines that still read as they did when the batch was sent;
    /// a line whose text changed since is released so a later batch (or the final pass) translates it again.
    func applyBackgroundTranslation(_ translated: [Sentence], language: String, batch: [Sentence]) {
        var current: [UUID: Sentence] = [:]
        for sentence in sentences { current[sentence.uid] = sentence }
        var accepted: [(result: Sentence, original: Sentence)] = []
        for pair in SubtitleViewModel.pairs(translated, with: batch) {
            if let now = current[pair.original.uid], now.text == pair.original.text {
                accepted.append(pair)
            } else {
                asyncQueued.remove(pair.original.uid)
            }
        }
        guard !accepted.isEmpty else { return }
        storeTranslations(accepted, language: language, merge: true)
    }
}
