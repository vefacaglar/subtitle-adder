import CryptoKit
import EngineKit
import Foundation

/// The two views of the window.
enum Workspace: Hashable, Sendable {
    case editor, playlist
}

/// One video in the batch playlist. In memory only; the source language belongs to the item, the target does not.
struct PlaylistItem: Identifiable, Equatable, Sendable {
    enum State: Equatable, Sendable {
        case pending, working, completed, failed
    }

    let id: UUID
    let videoURL: URL
    /// Spoken language of this video; empty means automatic detection.
    var sourceLanguage: String
    var state: State
    var statusMessage: LocalizedMessage
    var fraction: Double?

    var fileName: String { videoURL.lastPathComponent }
    var folderPath: String { videoURL.deletingLastPathComponent().path }
    /// The language can't change and the item can't go away while it is being worked on.
    var isEditable: Bool { state != .working }
    /// Its audio was extracted ahead of time ("Ses hazır · Sırada") and it waits for its turn; only that row is
    /// pending with a progress value.
    var isAudioReady: Bool { state == .pending && fraction != nil }
}

/// What one playlist run keeps track of while its three stages overlap.
@MainActor
final class PlaylistPipeline {
    /// Items whose speech recognition has begun (or was skipped because a subtitle file sits next to the video); the
    /// run never picks them again, so a failed item is retried by the next run, not by this one.
    var stage2Started: Set<UUID> = []
    /// Stage 1 tasks by item: the audio extraction started for it (running or done).
    var extractions: [UUID: Task<Void, Never>] = [:]
    /// The newest extraction; the next one queues behind it, so two never run at once.
    var lastExtraction: Task<Void, Never>?
    /// Items whose WAV is waiting in their folder for the transcription.
    var audioReady: Set<UUID> = []
    /// Local translation models that stage 3 is using right now, with the number of items using each. They stay out of
    /// reach of the model deletion in Settings, which the job-based protection (cleared whenever the video changes)
    /// doesn't cover while the next video is already transcribing.
    var translationModelsInUse: [String: Int] = [:]
}

/// Batch playlist: several videos are transcribed, translated into the one target language chosen at the start and
/// saved next to their videos. The run is a three-stage pipeline over the list:
///
/// 1. Audio extraction (CPU): one extraction at a time, at most one item ahead of the one being transcribed.
/// 2. Speech recognition (the editor's own flow: `openVideo`, `generate`): strictly one item after the other.
/// 3. Translation and save: handed off without waiting, so item N translates while item N + 1 transcribes. One
///    translation at a time.
///
/// With an external LLM in async mode the finished lines of the video being transcribed are also translated in the
/// background during stage 2; stage 3 then only sends what is left.
extension SubtitleViewModel {
    // MARK: - Items

    /// Adds videos with the toolbar language as their source language. A video that is already waiting or running
    /// isn't added twice; a finished or failed one can be added again.
    func addToPlaylist(_ urls: [URL], sourceLanguage: String? = nil) {
        let language = Self.playlistLanguage(sourceLanguage ?? self.language)
        var updated = playlist
        for url in urls {
            let key = url.standardizedFileURL.path
            let duplicate = updated.contains {
                $0.videoURL.standardizedFileURL.path == key && ($0.state == .pending || $0.state == .working)
            }
            if duplicate { continue }
            updated.append(PlaylistItem(
                id: UUID(), videoURL: url, sourceLanguage: language, state: .pending,
                statusMessage: .key("Sırada"), fraction: nil
            ))
        }
        if updated.count != playlist.count { playlist = updated }
    }

    func setPlaylistItemLanguage(_ id: UUID, to code: String) {
        let language = Self.playlistLanguage(code)
        guard let index = playlist.firstIndex(where: { $0.id == id }),
              playlist[index].isEditable, playlist[index].sourceLanguage != language
        else { return }
        playlist[index].sourceLanguage = language
    }

    /// Sets the source language of every item that isn't being worked on; an empty code makes them all automatic.
    func setAllPlaylistLanguages(_ code: String) {
        let language = Self.playlistLanguage(code)
        var updated = playlist
        for index in updated.indices where updated[index].isEditable {
            updated[index].sourceLanguage = language
        }
        if updated != playlist { playlist = updated }
    }

    func removeFromPlaylist(_ id: UUID) {
        guard let item = playlist.first(where: { $0.id == id }) else { return }
        if isPlaylistRunning, item.state == .working { return }
        playlist.removeAll { $0.id == id }
    }

    /// Empties the list; while a run is under way the items being worked on stay.
    func clearPlaylist() {
        playlist = isPlaylistRunning ? playlist.filter { $0.state == .working } : []
    }

    var hasPlaylistItems: Bool { !playlist.isEmpty }

    /// Something is waiting, or failed earlier and gets another try.
    var canStartPlaylist: Bool {
        !isWorking && playlist.contains { $0.state == .pending || $0.state == .failed }
    }

    var playlistSummary: String {
        guard !playlist.isEmpty else { return L("Listede henüz video yok.") }
        return L("%lld / %lld tamamlandı", playlist.filter { $0.state == .completed }.count, playlist.count)
    }

    private static func playlistLanguage(_ code: String) -> String {
        SubtitleLanguage(rawValue: code) == nil ? "" : code
    }

    // MARK: - Dropping videos

    /// More than one video dropped at once belongs to the playlist, and so does any video while the playlist is
    /// showing or running (the editor's single video would be pulled out from under the run).
    func openMultipleVideos(_ videos: [URL]) {
        addToPlaylist(videos)
        workspace = .playlist
    }

    // MARK: - Running

    /// Runs every waiting or failed item in order. The target language is the toolbar's at this moment and stays
    /// fixed for the whole run, each item uses its own source language, and none of it is persisted: the toolbar
    /// values are put back when the run ends.
    func startPlaylist() {
        guard canStartPlaylist else { return }
        guard let engine = catalog.engine else {
            status = .failed(.error(EngineError.notFound))
            return
        }
        var needed = [model, translationModel]
        if diarize { needed.append(Self.diarizationModelID) }
        if blockedOffline(needed, retry: { [weak self] in self?.startPlaylist() }) { return }
        commitDraft()
        commitTranslationDraft()
        isPlaylistRunning = true
        let pipeline = PlaylistPipeline()
        playlistPipeline = pipeline
        playlistTask = Task { await runPlaylist(engine: engine, target: translationTarget, pipeline: pipeline) }
    }

    /// Stops a playlist run: every engine process of it (audio extraction, recognition, translation) is terminated and
    /// the items that were not finished go back to waiting.
    func cancelPlaylist() {
        playlistTask?.cancel()
        for task in playlistBackground { task.cancel() }
        if let pipeline = playlistPipeline {
            for task in pipeline.extractions.values { task.cancel() }
        }
    }

    private enum ItemOutcome {
        /// The lines are in the editor and ready for stage 3.
        case ready
        case failed(LocalizedMessage)
        case cancelled
    }

    private func runPlaylist(engine: EngineClient, target: String, pipeline: PlaylistPipeline) async {
        let savedLanguage = language
        let savedModel = model
        playlistBackground = []
        playlistBackgroundPending = 0
        while !Task.isCancelled, let item = nextWaitingItem(pipeline) {
            // Barrier: the item's own audio extraction has to be over before its recognition starts, and with it the
            // extraction of the next item can begin; two never run at once.
            if let extraction = ensureAudioExtraction(item, engine: engine, pipeline: pipeline) {
                await extraction.value
            }
            if Task.isCancelled {
                updatePlaylistItem(item.id, .pending, .key("Sırada"), nil)
                break
            }
            pipeline.stage2Started.insert(item.id)
            activePlaylistItemID = item.id
            scheduleAudioLookahead(engine: engine, pipeline: pipeline)
            updatePlaylistItem(item.id, .working, .key("Başlıyor…"), pipeline.audioReady.contains(item.id) ? 1 : nil)
            let outcome = await transcribe(item, target: target)
            activePlaylistItemID = nil
            switch outcome {
            case .ready:
                handOffToStage3(item, engine: engine, target: target, pipeline: pipeline)
            case .failed(let message):
                updatePlaylistItem(item.id, .failed, message, nil)
            case .cancelled:
                updatePlaylistItem(item.id, .pending, .key("Sırada"), nil)
            }
            if case .cancelled = outcome { break }
        }
        // Translations handed off earlier may still be working.
        if playlistBackgroundPending > 0, !Task.isCancelled {
            status = .working(Self.label(for: .translating, fraction: nil, purpose: .translation), fraction: nil)
        }
        for task in playlistBackground { await task.value }
        for task in Array(pipeline.extractions.values) { await task.value }
        if Task.isCancelled {
            // Everything that was not finished is waiting again, whichever stage it had reached.
            for item in playlist where item.state == .working || item.isAudioReady {
                updatePlaylistItem(item.id, .pending, .key("Sırada"), nil)
            }
        }
        if !Task.isCancelled, case .working = status { status = .finished }
        // A model that was missing for some item offers to go online and run the list again.
        if offlineRetry != nil { offlineRetry = { [weak self] in self?.startPlaylist() } }
        // The editor keeps the last item's lines, which are in that item's language, not the one the toolbar returns to.
        let linesLanguage = sentences.isEmpty ? nil : translationSource
        // Put the toolbar back while the run's flag still keeps these from being saved as the user's choice.
        language = savedLanguage
        model = savedModel
        translationTarget = target
        linesSourceLanguage = linesLanguage
        playlistBackground = []
        playlistBackgroundPending = 0
        playlistPipeline = nil
        playlistTask = nil
        isPlaylistRunning = false
    }

    /// The first item that hasn't reached recognition yet: waiting, failed earlier (retried once per run), or already
    /// being prepared by stage 1.
    private func nextWaitingItem(_ pipeline: PlaylistPipeline) -> PlaylistItem? {
        playlist.first { isWaitingForRecognition($0, pipeline) }
    }

    private func isWaitingForRecognition(_ item: PlaylistItem, _ pipeline: PlaylistPipeline) -> Bool {
        !pipeline.stage2Started.contains(item.id)
            && (item.state == .pending || item.state == .failed || pipeline.extractions[item.id] != nil)
    }

    // MARK: - Stage 2: recognition

    /// Opens the item's video in the editor and transcribes it (unless a subtitle file sits next to it).
    private func transcribe(_ item: PlaylistItem, target: String) async -> ItemOutcome {
        language = item.sourceLanguage
        useModelFitting(language: item.sourceLanguage)
        translationTarget = target
        // Reopening also makes a sidecar written by an earlier run count.
        openVideo(nil)
        openVideo(item.videoURL)
        protectPlaylistModels()
        // A sidecar next to the video is used as it is, no transcription (as on Windows). Once translations are saved as
        // `<video>.<fmt>` a sidecar may be an earlier translation; the list can't tell, so that run translates it again.
        if sentences.isEmpty {
            // With an external LLM in async mode `generate` translates the finished lines while it transcribes.
            generate()
            protectPlaylistModels()
            await finishJob()
            protectPlaylistModels()
            if Task.isCancelled { return .cancelled }
            if let message = failureMessage() { return .failed(message) }
            if sentences.isEmpty { return .failed(.key("Videoda konuşma bulunamadı.")) }
        }
        if Task.isCancelled { return .cancelled }
        return .ready
    }

    /// The job that was just started, including one it hands over to.
    private func finishJob() async {
        while let current = job {
            await current.value
            if job == current { break }
        }
    }

    private func failureMessage() -> LocalizedMessage? {
        if case .failed(let message) = status { return message }
        return nil
    }

    /// For a language the selected model doesn't support, takes the most accurate model that does. Not persisted.
    private func useModelFitting(language: String) {
        let available = catalog.models(for: language)
        guard !available.isEmpty, !available.contains(where: { $0.id == model }),
              let best = available.max(by: { ($0.accuracy, $0.speed) < ($1.accuracy, $1.speed) })
        else { return }
        model = best.id
    }

    private static func savedMessage(_ name: String) -> LocalizedMessage {
        LocalizedMessage("saved:\(name)") { L("Kaydedildi: %@", name) }
    }

    // MARK: - Progress

    /// Mirrors the foreground job's progress into the row of the item it belongs to.
    func syncActivePlaylistItem(label: LocalizedMessage, fraction: Double?) {
        guard isPlaylistRunning, let id = activePlaylistItemID else { return }
        updatePlaylistItem(id, .working, label, fraction)
    }

    func updatePlaylistItem(_ id: UUID, _ state: PlaylistItem.State, _ message: LocalizedMessage, _ fraction: Double?) {
        guard let index = playlist.firstIndex(where: { $0.id == id }) else { return }
        playlist[index].state = state
        playlist[index].statusMessage = message
        playlist[index].fraction = fraction
    }

    // MARK: - Working folders

    /// The folder the engine works in for one playlist video: `<working folder>/playlist/<short hash of the path>`.
    /// Every item has its own, so two videos with the same file name in different folders never share a WAV while one
    /// of them is still being read. Audio extraction and recognition of the same video use the same one, which is how
    /// the recognition finds the WAV that was extracted ahead of time.
    nonisolated static func playlistOutDirectory(for video: URL, base: URL = WorkingDirectory.url()) -> URL {
        let digest = SHA256.hash(data: Data(video.standardizedFileURL.path.utf8))
        let name = digest.prefix(6).map { String(format: "%02x", $0) }.joined()
        return base.appending(path: "playlist").appending(path: name)
    }

    /// Where the engine works for `video`: its own folder while a playlist item is being transcribed, the working
    /// folder itself for the editor's single-video flow. Created if needed.
    func engineOutDirectory(for video: URL) -> URL {
        guard isPlaylistRunning, activePlaylistItemID != nil else { return WorkingDirectory.ensureExists() }
        let folder = Self.playlistOutDirectory(for: video)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }

    // MARK: - Stage 1: audio extraction

    /// How many items beyond the one being transcribed get their audio extracted ahead of time.
    private static let maxAudioLookahead = 1

    /// The item's audio extraction: the one already started, a new one, or `nil` when none is needed because a
    /// subtitle file sits next to the video (it is used as it is, nothing is transcribed).
    private func ensureAudioExtraction(
        _ item: PlaylistItem, engine: EngineClient, pipeline: PlaylistPipeline
    ) -> Task<Void, Never>? {
        if let existing = pipeline.extractions[item.id] { return existing }
        guard !Task.isCancelled, SubtitleParser.findSidecar(for: item.videoURL) == nil else { return nil }
        // Queued behind the newest one, so that two extractions never run at once.
        let previous = pipeline.lastExtraction
        let task = Task {
            await previous?.value
            await extractAudio(item, engine: engine, pipeline: pipeline)
        }
        pipeline.extractions[item.id] = task
        pipeline.lastExtraction = task
        return task
    }

    /// Starts the extraction for the next item that isn't being transcribed yet, at most `maxAudioLookahead` items
    /// ahead. An item that needs no extraction (it has a subtitle file) still takes up the slot.
    private func scheduleAudioLookahead(engine: EngineClient, pipeline: PlaylistPipeline) {
        guard !Task.isCancelled else { return }
        for candidate in playlist.filter({ isWaitingForRecognition($0, pipeline) }).prefix(Self.maxAudioLookahead) {
            _ = ensureAudioExtraction(candidate, engine: engine, pipeline: pipeline)
        }
    }

    /// Extracts the item's WAV into its folder while an earlier video is transcribed. The row shows the progress;
    /// when it is over the row reads "Ses hazır · Sırada". A failed or stopped extraction just puts the row back to
    /// "Sırada": the recognition extracts the audio itself and reports the real error when its turn comes.
    private func extractAudio(_ item: PlaylistItem, engine: EngineClient, pipeline: PlaylistPipeline) async {
        var finished = false
        if !Task.isCancelled {
            let folder = Self.playlistOutDirectory(for: item.videoURL)
            try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            do {
                for try await event in engine.extractAudio(video: item.videoURL, outDir: folder) {
                    guard !Task.isCancelled else { break }
                    switch event {
                    case .stage(let stage, let fraction):
                        guard !pipeline.stage2Started.contains(item.id) else { continue }
                        updatePlaylistItem(
                            item.id, .working, Self.label(for: stage, fraction: fraction, purpose: .transcription), fraction
                        )
                    case .result:
                        finished = true
                    case .partial, .translation, .error:
                        break
                    }
                }
            } catch {
                finished = false
            }
        }
        guard !pipeline.stage2Started.contains(item.id) else { return }
        if finished, !Task.isCancelled {
            pipeline.audioReady.insert(item.id)
            updatePlaylistItem(item.id, .pending, .key("Ses hazır · Sırada"), 1)
        } else {
            updatePlaylistItem(item.id, .pending, .key("Sırada"), nil)
        }
    }

    // MARK: - Stage 3: translation and save

    /// Keeps the local translation models of the running hand-offs out of Settings' deletion. The editor's own jobs
    /// reset the set whenever the video changes, so this is applied again after each of those.
    private func protectPlaylistModels() {
        guard let pipeline = playlistPipeline, !pipeline.translationModelsInUse.isEmpty else { return }
        catalog.activeModels.formUnion(pipeline.translationModelsInUse.keys)
    }

    /// Translates and saves the item in the background, one translation at a time, while the next video transcribes.
    /// Everything the task needs is captured now: the editor moves on to the next video. Lines the server already
    /// translated while the video was transcribing (`translations`) are not sent again.
    private func handOffToStage3(_ item: PlaylistItem, engine: EngineClient, target: String, pipeline: PlaylistPipeline) {
        let lines = sentences
        let preTranslated = translations
        let source = translationSource
        let model = translationModel
        let format = exportFormat
        let external = isExternalLLMActive
        // A model that wasn't on disk is downloaded by this translation; the catalog should show it as ready after.
        let refreshCatalog = !external && catalog.models.first(where: { $0.id == model })?.downloaded == false
        // The task may wait minutes for the earlier ones; it must still go to the server chosen now.
        let engine = pinningLaunchSettings(engine)
        if let initial = Self.translationProgress(done: preTranslated.count, total: lines.count) {
            updatePlaylistItem(
                item.id, .working, Self.label(for: .translating, fraction: initial, purpose: .translation), initial
            )
        } else {
            updatePlaylistItem(item.id, .working, .key("Çeviri hazırlanıyor…"), nil)
        }
        if !external {
            pipeline.translationModelsInUse[model, default: 0] += 1
            protectPlaylistModels()
        }
        let previous = playlistBackground.last
        playlistBackgroundPending += 1
        playlistBackground.append(Task {
            defer {
                playlistBackgroundPending -= 1
                if !external { releaseTranslationModel(model, in: pipeline) }
            }
            // One translation at a time, in the order the videos were transcribed. (The live translation while a video
            // transcribes is not held back by this, as on Windows: the server queues the requests.)
            await previous?.value
            await translateAndSaveInBackground(
                item, lines: lines, preTranslated: preTranslated, engine: engine, source: source, target: target,
                model: model, format: format, external: external, refreshCatalog: refreshCatalog
            )
        })
    }

    private func releaseTranslationModel(_ model: String, in pipeline: PlaylistPipeline) {
        let remaining = (pipeline.translationModelsInUse[model] ?? 1) - 1
        if remaining > 0 {
            pipeline.translationModelsInUse[model] = remaining
        } else {
            pipeline.translationModelsInUse[model] = nil
            catalog.activeModels.remove(model)
        }
    }

    /// Whether the editor currently shows the item's video, so what happens to the item is mirrored there.
    private func editorShows(_ item: PlaylistItem) -> Bool {
        video?.standardizedFileURL == item.videoURL.standardizedFileURL
    }

    private func translateAndSaveInBackground(
        _ item: PlaylistItem, lines: [Sentence], preTranslated: [UUID: TranslationEntry], engine: EngineClient,
        source: String?, target: String, model: String, format: SubtitleFormat, external: Bool, refreshCatalog: Bool
    ) async {
        guard !Task.isCancelled else {
            updatePlaylistItem(item.id, .pending, .key("Sırada"), nil)
            return
        }
        guard source != target else {
            updatePlaylistItem(item.id, .failed, .key("Kaynak ve hedef dil aynı olamaz."), nil)
            return
        }
        // An external LLM takes any language pair, so NLLB's need for a source language doesn't apply.
        if model == "nllb-1.3b", source == nil, !external {
            updatePlaylistItem(
                item.id, .failed, .key("NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç."), nil
            )
            return
        }
        // What was translated during transcription stays; only the rest goes to the server, with its neighbours as context.
        var translated: [UUID: (text: String, needsReview: Bool)] = [:]
        for line in lines {
            if let entry = preTranslated[line.uid], Self.isReusableTranslation(line, entry: entry, target: target) {
                translated[line.uid] = (entry.text, entry.needsReview)
            }
        }
        let preDone = translated.count
        let total = max(1, lines.count)
        let remaining = lines.filter { translated[$0.uid] == nil }
        // The row starts at the share the background translation already got through.
        if let initial = Self.translationProgress(done: preDone, total: total) {
            updatePlaylistItem(
                item.id, .working, Self.label(for: .translating, fraction: initial, purpose: .translation), initial
            )
        } else {
            updatePlaylistItem(item.id, .working, .key("Çeviri hazırlanıyor…"), nil)
        }
        let request = Self.withTranslationContext(lines, requested: remaining)
        var engineError: String?
        if !remaining.isEmpty {
            do {
                for try await event in engine.translate(request, to: target, from: source, model: model) {
                    guard !Task.isCancelled else { break }
                    switch event {
                    case .stage(let stage, var fraction):
                        if stage == .translating, let part = fraction {
                            fraction = Self.translationProgress(
                                alreadyDone: preDone, requested: remaining.count, part: part, total: total
                            )
                        }
                        let label = Self.label(for: stage, fraction: fraction, purpose: .translation)
                        updatePlaylistItem(item.id, .working, label, fraction)
                        if editorShows(item), case .working(let current, _) = status,
                           current.id.hasPrefix("stage:translating:") {
                            status = .working(label, fraction: fraction)
                        }
                    case .translation(let language, let batch):
                        // A growing prefix per batch plus a final complete one; context lines are not applied.
                        let pairs = Self.pairs(batch, with: remaining).filter { !$0.result.text.isEmpty }
                        for pair in pairs { translated[pair.original.uid] = (pair.result.text, pair.result.needsReview) }
                        if editorShows(item) { storeTranslations(pairs, language: language, merge: true) }
                    case .error(let message):
                        engineError = message
                    case .result, .partial:
                        break
                    }
                }
            } catch {
                if !Task.isCancelled { engineError = error.localizedDescription }
            }
        }
        if Task.isCancelled {
            updatePlaylistItem(item.id, .pending, .key("Sırada"), nil)
            return
        }
        if let engineError {
            updatePlaylistItem(item.id, .failed, .verbatim(engineError), nil)
            return
        }
        guard !translated.isEmpty else {
            updatePlaylistItem(item.id, .failed, .key("Çeviri tamamlanamadı."), nil)
            return
        }
        // The same lines the editor would export: a line the engine gave up on keeps its source text and its marker.
        let finalLines = lines.map { line in
            Self.exportLine(line, text: translated[line.uid]?.text, needsReview: translated[line.uid]?.needsReview ?? false)
        }
        let text: String
        do {
            text = try await engine.render(finalLines, as: format)
        } catch is CancellationError {
            updatePlaylistItem(item.id, .pending, .key("Sırada"), nil)
            return
        } catch {
            updatePlaylistItem(item.id, .failed, .error(error), nil)
            return
        }
        let name = Self.subtitleFileName(stem: item.videoURL.deletingPathExtension().lastPathComponent, format: format)
        let url = item.videoURL.deletingLastPathComponent().appendingPathComponent(name, isDirectory: false)
        do {
            // A folder that is gone makes the write throw: the item fails instead of reporting a save that didn't happen.
            try Data(text.utf8).write(to: url, options: .atomic)
        } catch {
            let reason = error.localizedDescription
            updatePlaylistItem(
                item.id, .failed, LocalizedMessage("save-failed:\(reason)") { L("Dosya kaydedilemedi: %@", reason) }, nil
            )
            return
        }
        onSubtitleSaved?(url)
        if editorShows(item) { savedSubtitleName = name }
        updatePlaylistItem(item.id, .completed, Self.savedMessage(name), 1)
        // Make the translation model that was just downloaded show as "hazır" (ready) in Settings.
        if refreshCatalog { await catalog.load() }
    }
}
