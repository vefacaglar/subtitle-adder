import EngineKit
import Foundation

/// The per-video properties of the view model are the foreground session's: views, menu commands and most of the code
/// read and write them here and never know which session that is. Code that has to work on a session that may not be
/// on screen (a job, a save) takes the `VideoSession` itself instead.
extension SubtitleViewModel {
    // MARK: - Identity

    /// The video on screen; `nil` when only a subtitle file (or nothing) is open.
    var video: URL? { foreground.video }

    // MARK: - Lines

    var sentences: [Sentence] {
        get { foreground.sentences }
        set { foreground.sentences = newValue }
    }
    /// Translations keyed by the line's stable `uid`; marked "eski" (stale) when the text is edited.
    var translations: [UUID: TranslationEntry] {
        get { foreground.translations }
        set { foreground.translations = newValue }
    }
    /// Target language of the last successful translation (for the export label).
    var translationLanguage: String? {
        get { foreground.translationLanguage }
        set { foreground.translationLanguage = newValue }
    }
    /// Uncommitted text of the translation being edited.
    var translationDraft: TextDraft? {
        get { foreground.translationDraft }
        set { foreground.translationDraft = newValue }
    }
    /// `uid` of the active line: the one the player follows and the single-line edits (split, merge, delete, timing,
    /// speaker) work on. Setting it selects just that line; `setSelectedLines` keeps a multi-selection.
    var selection: UUID? {
        get { foreground.selection }
        set { foreground.selection = newValue }
    }
    /// All selected lines (⌘/⇧-click); `selection` is always one of them, or `nil` when the set is empty.
    var selectedUIDs: Set<UUID> { foreground.selectedUIDs }
    /// Uncommitted text of the line being edited (used while focused).
    var draft: TextDraft? {
        get { foreground.draft }
        set { foreground.draft = newValue }
    }
    var detectedLanguage: String? {
        get { foreground.detectedLanguage }
        set { foreground.detectedLanguage = newValue }
    }
    /// What the lines in the editor are spoken in when the toolbar language doesn't say so (see `VideoSession`).
    var linesSourceLanguage: String? {
        get { foreground.linesSourceLanguage }
        set { foreground.linesSourceLanguage = newValue }
    }

    var hasTranslations: Bool { foreground.hasTranslations }

    /// The mode actually shown: without translations only the original exists, whatever was chosen last.
    var effectiveDisplayMode: SubtitleDisplay { foreground.effectiveDisplayMode }

    /// Title of the translation choice in the display and export pickers, e.g. "Çeviri (es)".
    var translationTitle: String { foreground.translationTitle }

    /// Exactly one line is selected: split, merge and the timing controls only act on a single line, so they stay
    /// unavailable for a multi-selection instead of silently picking one of its lines. Delete and retranslate take the
    /// whole selection.
    var hasSingleSelection: Bool { foreground.hasSingleSelection }

    func setSelectedLines(_ uids: Set<UUID>, active: UUID?) { foreground.setSelectedLines(uids, active: active) }
    func updateSelection(to uids: Set<UUID>) { foreground.updateSelection(to: uids) }
    func selectRow(_ uid: UUID) { foreground.selectRow(uid) }

    func translation(for sentence: Sentence) -> TranslationEntry? { foreground.translation(for: sentence) }
    func isTranslationStale(for sentence: Sentence) -> Bool { foreground.isTranslationStale(for: sentence) }
    func needsReview(for sentence: Sentence) -> Bool { foreground.needsReview(for: sentence) }

    // MARK: - Display and export

    /// Whether the subtitle over the video shows the original or the translation; the last choice is remembered for the
    /// videos opened next. With translations present, picking Original or Translation also switches the export language.
    var displayMode: SubtitleDisplay {
        get { foreground.displayMode }
        set { foreground.displayMode = newValue }
    }
    /// Whether the exported subtitle carries the original or the translated text.
    var exportLanguage: ExportLanguage {
        get { foreground.exportLanguage }
        set { foreground.exportLanguage = newValue }
    }

    static func storedDisplayMode(in defaults: UserDefaults) -> SubtitleDisplay {
        SubtitleDisplay(rawValue: defaults.string(forKey: displayModeDefaultsKey) ?? "") ?? .original
    }

    // MARK: - Files

    /// The subtitle file the lines came from (import or sidecar); where "Altyazıyı kaydet" writes when there is no video.
    var loadedSubtitle: URL? {
        get { foreground.loadedSubtitle }
        set { foreground.loadedSubtitle = newValue }
    }
    /// File name of the last save, shown at the end of the finished status.
    var savedSubtitleName: String? {
        get { foreground.savedSubtitleName }
        set { foreground.savedSubtitleName = newValue }
    }
    /// The lines or their translations were edited, or a translation was applied, since they were opened or last saved.
    var hasUnsavedEdits: Bool {
        get { foreground.hasUnsavedEdits }
        set { foreground.hasUnsavedEdits = newValue }
    }

    // MARK: - Status

    var status: Status {
        get { foreground.status }
        set { foreground.status = newValue }
    }
    /// A translation job ("Çevir…", "Komple çevir" after transcription) owns the status.
    var isTranslationJobRunning: Bool {
        get { foreground.isTranslationJobRunning }
        set { foreground.isTranslationJobRunning = newValue }
    }
    /// Background translation (external LLM, async mode) is under way while the video is still being transcribed.
    var isAsyncTranslating: Bool {
        get { foreground.isAsyncTranslating }
        set { foreground.isAsyncTranslating = newValue }
    }
    /// Share of the lines translated in the background (0...0.99); `nil` while unknown or when nothing runs.
    var asyncTranslationFraction: Double? {
        get { foreground.asyncTranslationFraction }
        set { foreground.asyncTranslationFraction = newValue }
    }
    /// Re-runs the job that offline mode blocked; set only together with the matching failed status.
    var offlineRetry: (@MainActor () -> Void)? {
        get { foreground.offlineRetry }
        set { foreground.offlineRetry = newValue }
    }

    // MARK: - The foreground job's internals (tests look at them)

    var job: Task<Void, Never>? { foreground.job }
    var asyncTask: Task<Void, Never>? { foreground.asyncTask }
    var asyncQueued: Set<UUID> {
        get { foreground.asyncQueued }
        set { foreground.asyncQueued = newValue }
    }
    var asyncFailed: Bool {
        get { foreground.asyncFailed }
        set { foreground.asyncFailed = newValue }
    }
    var asyncTranscriptionComplete: Bool {
        get { foreground.asyncTranscriptionComplete }
        set { foreground.asyncTranscriptionComplete = newValue }
    }

    /// "N satır · dil: xx" (the language is left out when unknown), plus the translation language and the saved file.
    var finishedSummary: String { foreground.finishedSummary }
}
