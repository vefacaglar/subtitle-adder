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
}

/// Batch playlist: several videos are transcribed, translated into the one target language chosen at the start and
/// saved next to their videos, one after the other. The editor's own single-video flow does the work for each item
/// (`openVideo`, `generate`, `translate`); with an external LLM in async mode the translation of an item runs in the
/// background while the next video transcribes.
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
        playlistTask = Task { await runPlaylist(engine: engine, target: translationTarget) }
    }

    /// Stops a playlist run: the item in progress goes back to waiting, the background translations stop.
    func cancelPlaylist() {
        playlistTask?.cancel()
        for task in playlistBackground { task.cancel() }
    }

    private enum ItemOutcome {
        case completed(LocalizedMessage)
        case failed(LocalizedMessage)
        case cancelled
        /// The translation went to a background task, which reports the end result itself.
        case handedOff
    }

    private func runPlaylist(engine: EngineClient, target: String) async {
        let savedLanguage = language
        let savedModel = model
        // A failed item is retried by the next run, not again by this one.
        var attempted: Set<UUID> = []
        playlistBackground = []
        playlistBackgroundPending = 0
        while !Task.isCancelled,
              let item = playlist.first(where: { ($0.state == .pending || $0.state == .failed) && !attempted.contains($0.id) }) {
            attempted.insert(item.id)
            activePlaylistItemID = item.id
            updatePlaylistItem(item.id, .working, .key("Başlıyor…"), nil)
            let outcome = await process(item, engine: engine, target: target)
            activePlaylistItemID = nil
            switch outcome {
            case .completed(let message):
                updatePlaylistItem(item.id, .completed, message, 1)
            case .failed(let message):
                updatePlaylistItem(item.id, .failed, message, nil)
            case .cancelled:
                updatePlaylistItem(item.id, .pending, .key("Sırada"), nil)
            case .handedOff:
                break
            }
            if case .cancelled = outcome { break }
        }
        // Items handed to the external server may still be translating.
        let background = playlistBackground
        if !background.isEmpty, !Task.isCancelled {
            status = .working(Self.label(for: .translating, fraction: nil, purpose: .translation), fraction: nil)
        }
        for task in background { await task.value }
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
        playlistTask = nil
        isPlaylistRunning = false
    }

    private func process(_ item: PlaylistItem, engine: EngineClient, target: String) async -> ItemOutcome {
        language = item.sourceLanguage
        useModelFitting(language: item.sourceLanguage)
        translationTarget = target
        // Reopening also makes a sidecar written by an earlier run count.
        openVideo(nil)
        openVideo(item.videoURL)
        // A sidecar next to the video is used as it is, no transcription (as on Windows). Once translations are saved as
        // `<video>.<fmt>` a sidecar may be an earlier translation; the list can't tell, so that run translates it again.
        if sentences.isEmpty {
            // With an external LLM in async mode `generate` translates the finished lines while it transcribes.
            generate()
            await finishJob()
            if Task.isCancelled { return .cancelled }
            if let message = failureMessage() { return .failed(message) }
            if sentences.isEmpty { return .failed(.key("Videoda konuşma bulunamadı.")) }
        }
        if Task.isCancelled { return .cancelled }

        if isExternalAsyncActive {
            handOffToBackground(item, engine: engine, target: target)
            return .handedOff
        }

        translate()
        await finishJob()
        if Task.isCancelled { return .cancelled }
        if let message = failureMessage() { return .failed(message) }
        if translations.isEmpty { return .failed(.key("Çeviri tamamlanamadı.")) }

        // Always the translation, whatever the auto-save toggle says.
        guard let url = await saveSubtitle(translated: true) else {
            // A stop during the render isn't a failure.
            if Task.isCancelled { return .cancelled }
            return .failed(failureMessage() ?? .key("Çeviri tamamlanamadı."))
        }
        return .completed(Self.savedMessage(url.lastPathComponent))
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

    // MARK: - Async mode (external LLM)

    /// Translates and saves the item in the background, one at a time, while the next video transcribes. Everything the
    /// task needs is captured now: the editor moves on to the next video. Lines the server already translated while the
    /// video was transcribing (`translations`) are not sent again.
    private func handOffToBackground(_ item: PlaylistItem, engine: EngineClient, target: String) {
        let lines = sentences
        let preTranslated = translations
        let source = translationSource
        let model = translationModel
        let format = exportFormat
        // The task may wait minutes for the earlier ones; it must still go to the server chosen now.
        let engine = pinningLaunchSettings(engine)
        if let initial = Self.translationProgress(done: preTranslated.count, total: lines.count) {
            updatePlaylistItem(
                item.id, .working, Self.label(for: .translating, fraction: initial, purpose: .translation), initial
            )
        } else {
            updatePlaylistItem(item.id, .working, .key("Çeviri hazırlanıyor…"), nil)
        }
        let previous = playlistBackground.last
        playlistBackgroundPending += 1
        playlistBackground.append(Task {
            defer { playlistBackgroundPending -= 1 }
            // One external translation at a time. (The live translation while a video transcribes is not held back
            // by this, as on Windows: the server queues the requests.)
            await previous?.value
            await translateAndSaveInBackground(
                item, lines: lines, preTranslated: preTranslated, engine: engine,
                source: source, target: target, model: model, format: format
            )
        })
    }

    /// Whether the editor currently shows the item's video, so what happens to the item is mirrored there.
    private func editorShows(_ item: PlaylistItem) -> Bool {
        video?.standardizedFileURL == item.videoURL.standardizedFileURL
    }

    private func translateAndSaveInBackground(
        _ item: PlaylistItem, lines: [Sentence], preTranslated: [UUID: TranslationEntry], engine: EngineClient,
        source: String?, target: String, model: String, format: SubtitleFormat
    ) async {
        guard !Task.isCancelled else {
            updatePlaylistItem(item.id, .pending, .key("Sırada"), nil)
            return
        }
        guard source != target else {
            updatePlaylistItem(item.id, .failed, .key("Kaynak ve hedef dil aynı olamaz."), nil)
            return
        }
        // What was translated during transcription stays; only the rest goes to the server, with its neighbours as context.
        var translated: [UUID: String] = [:]
        for line in lines {
            if let entry = preTranslated[line.uid], Self.isReusableTranslation(line, entry: entry, target: target) {
                translated[line.uid] = entry.text
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
                        for pair in pairs { translated[pair.original.uid] = pair.result.text }
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
        let finalLines = lines.map { line -> Sentence in
            var copy = line
            if let text = translated[line.uid] { copy.text = text }
            copy.words = []
            return copy
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
        if editorShows(item) { savedSubtitleName = name }
        updatePlaylistItem(item.id, .completed, Self.savedMessage(name), 1)
    }
}
