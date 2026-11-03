import EngineKit
import Foundation

/// Single-video jobs: transcription ("Altyazı üret"), transcription plus translation ("Komple çevir"), translation
/// ("Çevir…") and retranslating selected lines. A job is requested for a session, waits in the `JobQueue` while another
/// video's job runs, and then runs on its own session with the toolbar settings captured when it was requested.
///
/// Whatever the job does, it does to the session it was started for: the lines, the status and the saved file belong to
/// that video even when another one is on screen by then.
extension SubtitleViewModel {
    // MARK: - Requesting

    /// The toolbar's choices as of now; a job keeps these whatever the toolbar does later.
    func currentJobSettings() -> JobSettings {
        JobSettings(
            language: language, model: model, diarize: diarize, speakerCount: speakerCount,
            translationTarget: translationTarget, translationModel: translationModel,
            autoSave: autoSaveSubtitle, exportFormat: exportFormat
        )
    }

    /// Transcribes the video on screen; with `thenTranslate` ("Komple çevir") the translation starts right after the
    /// result and, with auto-save on, the translated file is written when it finishes.
    func generate(thenTranslate: Bool = false) {
        request(.generate(thenTranslate: thenTranslate), on: foreground)
    }

    /// Translates all lines of the video on screen to the selected target language. While running, Stop cancels the same
    /// job. With `reuseExisting` only the lines that have no translation yet, whose translation is outdated or in
    /// another language are sent. With `only` (the selected lines, captured when the job is requested) just those lines
    /// are sent again, translated or not; their results merge into the other lines' translations, whatever language
    /// those are in.
    func translate(reuseExisting: Bool = false, only selectedLines: Set<UUID>? = nil) {
        request(.translate(reuseExisting: reuseExisting, only: selectedLines), on: foreground)
    }

    /// Whether "Seçilenleri yeniden çevir" can start: at least one line is selected and nothing else is running.
    var canTranslateSelected: Bool { !selectedUIDs.isEmpty && !isWorking }

    /// Sends the selected lines (all of them, translated or not) to the engine again and merges the results into the
    /// existing translations. Selection changes while the job runs don't change which lines it applies to.
    func translateSelected() {
        guard canTranslateSelected else { return }
        translate(only: selectedUIDs)
    }

    /// Asks for a job on `session`: it starts at once when no other single job runs, otherwise it waits in the queue
    /// ("Sırada") behind the ones requested earlier. A request for a video that already waits replaces that entry.
    func request(_ kind: JobKind, on session: VideoSession, settings: JobSettings? = nil) {
        // The playlist run owns the video while it works on it; stopping the run is the way to get it back.
        guard session.batchItemID == nil else { return }
        let settings = settings ?? currentJobSettings()
        guard preflight(kind, on: session, settings: settings) else { return }
        // A job that already runs for this video is replaced.
        if session.job != nil { stopJob(of: session) }
        noteRecentJob(session)
        let entry = JobQueue.Entry(session: session, kind: kind, settings: settings)
        if jobQueue.submit(entry) == .queued {
            session.status = .working(.key("Sırada"), fraction: nil)
        }
    }

    /// Checks what can be checked before the job takes its place in the queue, so a request that can't run says why at
    /// once instead of after a wait.
    private func preflight(_ kind: JobKind, on session: VideoSession, settings: JobSettings) -> Bool {
        switch kind {
        case .generate(let thenTranslate):
            guard session.video != nil else { return false }
            return checkGenerate(session, thenTranslate: thenTranslate, settings: settings)
        case .translate(let reuseExisting, let only):
            return prepareTranslation(session, reuseExisting: reuseExisting, only: only, settings: settings) != nil
        }
    }

    /// The queue gave the job the lane.
    func begin(_ entry: JobQueue.Entry) {
        let session = entry.session
        switch entry.kind {
        case .generate(let thenTranslate):
            startGenerate(session, thenTranslate: thenTranslate, settings: entry.settings, batchItem: nil)
        case .translate(let reuseExisting, let only):
            startTranslation(session, reuseExisting: reuseExisting, only: only, settings: entry.settings)
        }
        // Nothing was launched (a precondition failed after the wait): the lane is free again.
        if session.job == nil { jobQueue.jobEnded(session) }
    }

    // MARK: - Ending

    /// The job's task is over. A job that was replaced (hand-over from recognition to translation) or cancelled has a
    /// different id by now and leaves the session alone.
    func jobDidEnd(_ session: VideoSession, id: UUID) {
        releaseModels(of: id)
        guard session.jobID == id else { return }
        session.job = nil
        jobQueue.jobEnded(session)
    }

    /// Cancels the session's job whatever state it is in (running, or waiting in the queue) without touching its
    /// status. The queue moves on to the next video.
    func stopJob(of session: VideoSession) {
        jobQueue.remove(session)
        session.job?.cancel()
        session.job = nil
        releaseModels(of: session.jobID)
        session.jobID = UUID()
        session.cancelAsyncTranslation()
        jobQueue.jobEnded(session)
    }

    /// Stop. For the video on screen: a waiting job leaves the queue; a running job is cancelled and the queue goes on
    /// with the next video. While the list tab is showing a playlist run, or when the video on screen belongs to the
    /// run, it stops the run (all of its engine processes).
    func cancel() {
        if workspace == .playlist, isPlaylistRunning {
            cancelPlaylist()
            return
        }
        let session = foreground
        if session.batchItemID != nil || session.isPlaylistTranslating {
            // The run puts the item back to waiting, and with it this video's status.
            cancelPlaylist()
        } else if jobQueue.isQueued(session) || session.job != nil || session.isWorking {
            stopJob(of: session)
            forgetRecentJob(session)
            session.status = .idle
        } else if isPlaylistRunning {
            cancelPlaylist()
        }
    }

    /// Stop for a video of the list tab: cancels the job of that video (running or waiting) and takes its row away.
    func cancelJob(of video: URL) {
        guard let session = existingSession(for: video) else { return }
        if session.batchItemID != nil { cancelPlaylist(); return }
        stopJob(of: session)
        forgetRecentJob(session)
        session.status = .idle
    }

    /// Removes every waiting job from the queue; the running one is not touched.
    func clearQueue() {
        for session in jobQueue.clearWaiting() {
            session.status = .idle
            forgetRecentJob(session)
        }
    }

    /// Stop has something to stop: the job of the video on screen, or, in the list tab, the playlist run (the rows of
    /// single videos have a Stop of their own there).
    var canStop: Bool {
        workspace == .playlist ? isPlaylistRunning : isWorking
    }

    // MARK: - Transcription

    private func engineMissing(_ session: VideoSession) {
        session.status = .failed(.error(EngineError.notFound))
    }

    /// The preconditions of a transcription: an engine, and no model the offline mode keeps from being downloaded.
    private func checkGenerate(_ session: VideoSession, thenTranslate: Bool, settings: JobSettings) -> Bool {
        guard catalog.engine != nil else {
            engineMissing(session)
            return false
        }
        var needed = [settings.model]
        if settings.diarize { needed.append(Self.diarizationModelID) }
        if thenTranslate { needed.append(settings.translationModel) }
        let retry: @MainActor () -> Void = { [weak self] in
            self?.request(.generate(thenTranslate: thenTranslate), on: session, settings: settings)
        }
        return !blockedOffline(needed, session: session, retry: retry)
    }

    /// Where the engine works for `video`: the working folder for a single job, a folder of its own for a playlist item
    /// (so two videos with the same file name never share a WAV). Created if needed.
    func jobOutDirectory(for video: URL, batchItem: UUID?) -> URL {
        guard batchItem != nil else { return WorkingDirectory.ensureExists() }
        let folder = Self.playlistOutDirectory(for: video)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }

    /// Transcribes the session's video. `batchItem` is set for a playlist item: it uses the item's own folder, skips the
    /// queue (the run took the lane) and never chains into a translation (the run hands it off afterwards).
    func startGenerate(_ session: VideoSession, thenTranslate: Bool, settings: JobSettings, batchItem: UUID?) {
        guard let video = session.video else { return }
        guard let engine = catalog.engine else {
            engineMissing(session)
            return
        }
        if batchItem == nil, !checkGenerate(session, thenTranslate: thenTranslate, settings: settings) { return }
        session.cancelAsyncTranslation()
        session.isTranslationJobRunning = false
        // Komple çevir and playlist items with an external LLM in async mode: the server translates while the Mac is
        // still transcribing. (A playlist item doesn't chain into `translate`; the run hands it off afterwards.)
        let wantsAsyncTranslate = (thenTranslate || batchItem != nil) && isExternalAsyncActive
        session.resetForNewRun()
        if session === foreground {
            undoManager?.removeAllActions()
            modelNotice = nil
        }
        session.status = .working(.key("Başlıyor…"), fraction: nil)
        let outDir = jobOutDirectory(for: video, batchItem: batchItem)
        let options = EngineClient.TranscribeOptions(
            language: settings.language.isEmpty ? nil : settings.language,
            model: settings.model,
            diarize: settings.diarize,
            speakers: settings.speakerCount >= 2 ? settings.speakerCount : nil
        )
        let runningModel = settings.model
        let tracker = DownloadPhaseTracker(
            transcriptionDownloaded: catalog.models.first { $0.id == runningModel }?.downloaded ?? true,
            diarize: settings.diarize,
            diarizationDownloaded: catalog.diarizationModels.first?.downloaded ?? true
        )
        session.job?.cancel()
        let id = UUID()
        session.jobID = id
        var running: Set<String> = [runningModel]
        if settings.diarize { running.insert(Self.diarizationModelID) }
        claimModels(running, for: id)
        session.job = Task {
            // However it ends (result, error, cancel), the models become deletable in Settings and the lane is freed.
            defer { jobDidEnd(session, id: id) }
            var failed = false
            var jobSettings = settings
            var asyncTranslate = wantsAsyncTranslate
            var asyncEngine = engine
            if wantsAsyncTranslate {
                // Every batch of this run goes to the server it starts with, even if Settings changes meanwhile, and with
                // its real key: read again now, on a background thread. A key that can't be read leaves the translation
                // to the closing pass, which says why.
                let problem = await prepareExternalLaunch()
                guard session.jobID == id, !Task.isCancelled else { return }
                if problem == nil { asyncEngine = pinningLaunchSettings(engine) } else { asyncTranslate = false }
            }
            // With diarization, `downloading_model` can run twice back to back; the tracker tells which model it is.
            var downloadPhases = tracker
            do {
                for try await event in engine.transcribe(video: video, outDir: outDir, options: options) {
                    guard session.jobID == id, !Task.isCancelled else { return }
                    switch event {
                    case .stage(let stage, let fraction):
                        let download = downloadPhases.update(stage: stage, fraction: fraction)
                        let purpose: JobPurpose = download == .diarization ? .diarization : .transcription
                        session.status = .working(Self.label(for: stage, fraction: fraction, purpose: purpose), fraction: fraction)
                        if stage == .diarizing || stage == .segmenting {
                            // Recognition is done: even a short final batch can translate while speakers are analysed.
                            session.asyncTranscriptionComplete = true
                            if asyncTranslate { startAsyncTranslation(session, engine: asyncEngine, settings: jobSettings) }
                        }
                    case .partial(let language, let snapshot):
                        if session.applyPartial(language: language, sentences: snapshot) {
                            languageDetected(in: session, settings: &jobSettings)
                        }
                        if asyncTranslate { startAsyncTranslation(session, engine: asyncEngine, settings: jobSettings) }
                    case .result(let result):
                        // The last line is final now, but the closing pass takes it from here.
                        session.asyncDraining = true
                        session.sentences = session.sentences.mergedWithSnapshot(result.sentences)
                        session.detectedLanguage = result.language
                        // The lines are in the language the job was told they were in, whatever the toolbar says later.
                        if !jobSettings.language.isEmpty { session.linesSourceLanguage = jobSettings.language }
                        languageDetected(in: session, settings: &jobSettings)
                        session.updateAsyncFraction()
                        // The app doesn't use the extracted WAV, so it goes right away instead of waiting for the
                        // quit cleanup; the subtitle outputs and engine checkpoints stay in the working folder until then.
                        try? FileManager.default.removeItem(atPath: result.audio)
                    case .translation:
                        break
                    case .error(let message):
                        failed = true
                        session.cancelAsyncTranslation()
                        session.status = .failed(.verbatim(message))
                    }
                }
                // On cancel the stream ends without an error; in that case the status doesn't change.
                guard session.jobID == id, !Task.isCancelled else { return }
                if failed { session.cancelAsyncTranslation() }
                // Let the batch already at the server finish (the status shows its progress meanwhile); the lines it
                // didn't cover are translated below.
                if asyncTranslate, !failed {
                    await session.finishAsyncTranslation()
                    guard session.jobID == id, !Task.isCancelled else { return }
                }
                // Komple çevir: hand over to the translation job, which also auto-saves. Nothing to translate
                // (no speech found) just stays finished. After background translation only the missing, outdated
                // and wrong-language lines are left to do.
                if thenTranslate, !failed, !session.sentences.isEmpty {
                    startTranslation(session, reuseExisting: asyncTranslate, only: nil, settings: jobSettings)
                    return
                }
                if !failed { session.status = .finished }
                // Make the downloaded model show as "hazır" (ready) in the menu.
                await catalog.load()
            } catch is CancellationError {
                if session.jobID == id { session.status = .idle }
            } catch {
                if session.jobID == id {
                    session.cancelAsyncTranslation()
                    session.status = .failed(.error(error))
                }
            }
        }
    }

    /// The language of the session's lines is known: a target equal to it moves on to another, for the job and, when the
    /// session is on screen, for the toolbar too.
    private func languageDetected(in session: VideoSession, settings: inout JobSettings) {
        let source = session.translationSource(language: settings.language)
        settings.translationTarget = Self.reconciledTarget(settings.translationTarget, source: source)
        if session === foreground { reconcileTranslationTarget() }
    }

    /// Live transcription snapshot of the video on screen: replaces the lines, keeping the identity of the ones that are
    /// still there. The language is `"?"` until the engine has detected it.
    func applyPartial(language: String, sentences snapshot: [Sentence]) {
        if foreground.applyPartial(language: language, sentences: snapshot) {
            reconcileTranslationTarget()
        }
    }

    // MARK: - Translation

    /// Everything a translation job needs, worked out (and checked) from the session's lines and the job's settings.
    struct TranslationPlan {
        let engine: EngineClient
        let source: String?
        let target: String
        let model: String
        /// The lines of a selected retry; `nil` for the other kinds.
        let selectedRequest: [Sentence]?
        /// The lines to translate.
        let request: [Sentence]
        /// `request` with up to two neighbours per line, so the model still sees the dialogue.
        let contextualRequest: [Sentence]
        /// Lines whose translation stays count as done in the progress.
        let alreadyDone: Int
        let total: Int
        /// What the lines had before the job: a line the engine fails on keeps this.
        let baseline: [UUID: TranslationEntry]
        let reuseExisting: Bool
    }

    /// Checks that a translation can start and works out what it sends; on a problem the session's status says why and
    /// the result is `nil`.
    func prepareTranslation(
        _ session: VideoSession, reuseExisting: Bool, only selectedLines: Set<UUID>?, settings: JobSettings
    ) -> TranslationPlan? {
        guard !session.sentences.isEmpty else { return nil }
        let selectedRequest = selectedLines.map { wanted in session.sentences.filter { wanted.contains($0.uid) } }
        if let selectedRequest, selectedRequest.isEmpty { return nil }
        guard let engine = catalog.engine else {
            engineMissing(session)
            return nil
        }
        if session === foreground {
            commitDraft()
            commitTranslationDraft()
        }
        let source = session.translationSource(language: settings.language)
        let target = settings.translationTarget
        guard source != target else {
            session.status = .failed(.key("Kaynak ve hedef dil aynı olamaz."))
            return nil
        }
        // An external LLM takes any language pair, so NLLB's need for a source language doesn't apply.
        if settings.translationModel == "nllb-1.3b", source == nil, !isExternalLLMActive {
            session.status = .failed(.key("NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç."))
            return nil
        }
        let model = settings.translationModel
        let retry: @MainActor () -> Void = { [weak self] in
            self?.request(.translate(reuseExisting: reuseExisting, only: selectedLines), on: session, settings: settings)
        }
        if blockedOffline([model], session: session, retry: retry) { return nil }
        let lines = session.sentences
        let request = selectedRequest ?? (reuseExisting ? linesNeedingTranslation(of: session, into: target) : lines)
        // A partial request carries up to two neighbours per line so the model still sees the dialogue; only the
        // requested lines' results are applied.
        let contextualRequest = Self.withTranslationContext(lines, requested: request)
        let alreadyDone = reuseExisting && selectedRequest == nil
            ? lines.count { line in
                session.translations[line.uid].map { Self.isReusableTranslation(line, entry: $0, target: target) } ?? false
            }
            : 0
        // The progress of a selected retry is the share of the selection, not of the whole subtitle.
        let total = max(1, selectedRequest?.count ?? lines.count)
        return TranslationPlan(
            engine: engine, source: source, target: target, model: model, selectedRequest: selectedRequest,
            request: request, contextualRequest: contextualRequest, alreadyDone: alreadyDone, total: total,
            baseline: session.translations, reuseExisting: reuseExisting
        )
    }

    /// Translates the session's lines. See `translate(reuseExisting:only:)`.
    func startTranslation(
        _ session: VideoSession, reuseExisting: Bool, only selectedLines: Set<UUID>?, settings: JobSettings
    ) {
        guard let plan = prepareTranslation(session, reuseExisting: reuseExisting, only: selectedLines, settings: settings)
        else { return }
        session.cancelAsyncTranslation()
        session.savedSubtitleName = nil
        session.job?.cancel()
        let id = UUID()
        session.jobID = id
        // An external server has no local model to protect from deletion.
        claimModels(isExternalLLMActive ? [] : [plan.model], for: id)
        if plan.request.isEmpty {
            // Everything was translated in the background: only the save is left.
            session.status = .working(.key("Çeviri hazırlanıyor…"), fraction: nil)
            session.isTranslationJobRunning = true
            session.job = Task {
                defer { jobDidEnd(session, id: id) }
                guard session.jobID == id, !Task.isCancelled else { return }
                session.status = .finished
                await autoSaveTranslationIfEnabled(of: session, settings: settings)
            }
            return
        }
        if let initial = Self.translationProgress(done: plan.alreadyDone, total: plan.total) {
            session.status = .working(Self.label(for: .translating, fraction: initial, purpose: .translation), fraction: initial)
        } else {
            session.status = .working(.key("Çeviri hazırlanıyor…"), fraction: nil)
        }
        session.isTranslationJobRunning = true
        session.job = Task {
            var undoRegistered = false
            defer { jobDidEnd(session, id: id) }
            var failed = false
            do {
                // Only one translation runs at a time in the app: while another one holds the gate this job waits with
                // the status it already has ("Çeviri hazırlanıyor…"); Stop leaves the queue. Held for the engine
                // process only, not for the save below.
                do {
                    try await translationGate.acquire()
                    defer { translationGate.release() }
                    guard session.jobID == id, !Task.isCancelled else { return }
                    // The provider's key is read again now, on a background thread, and a key that exists but can't be
                    // read stops the job here instead of sending the text without it.
                    if let problem = await prepareExternalLaunch() {
                        guard session.jobID == id, !Task.isCancelled else { return }
                        session.status = .failed(problem)
                        return
                    }
                    guard session.jobID == id, !Task.isCancelled else { return }
                    // Launched with the settings just published, i.e. with the key that was just read.
                    let launching = pinningLaunchSettings(plan.engine)
                    for try await event in launching.translate(
                        plan.contextualRequest, to: plan.target, from: plan.source, model: plan.model
                    ) {
                        guard session.jobID == id, !Task.isCancelled else { return }
                        switch event {
                        case .stage(let stage, var fraction):
                            if stage == .translating, let part = fraction {
                                fraction = Self.translationProgress(
                                    alreadyDone: plan.alreadyDone, requested: plan.request.count, part: part, total: plan.total
                                )
                            }
                            session.status = .working(
                                Self.label(for: stage, fraction: fraction, purpose: .translation),
                                fraction: fraction
                            )
                        case .translation(let language, let translated):
                            // A selected retry is one undo step back to what the lines had before it, however many
                            // batches it applies and even if it is stopped; registered with the first batch, and only
                            // while the video is on screen (the undo history is the foreground's).
                            if plan.selectedRequest != nil, !undoRegistered {
                                undoRegistered = true
                                if session === foreground {
                                    registerTranslationUndo(
                                        on: session, restoring: plan.baseline, actionName: L("Seçilenleri Yeniden Çevir")
                                    )
                                }
                            }
                            // One event per batch (a growing prefix) plus a final one; each is applied as it arrives.
                            applyTranslation(
                                translated, language: language, source: plan.request,
                                merge: reuseExisting || plan.selectedRequest != nil,
                                preserveOtherLanguages: plan.selectedRequest != nil, reviewFallback: plan.baseline,
                                to: session
                            )
                        case .result, .partial:
                            break
                        case .error(let message):
                            failed = true
                            session.status = .failed(.verbatim(message))
                        }
                    }
                }
                // On cancel the stream ends without an error; in that case the status doesn't change.
                guard session.jobID == id, !Task.isCancelled else { return }
                if !failed {
                    session.status = .finished
                    await autoSaveTranslationIfEnabled(of: session, settings: settings)
                }
                // Make the downloaded translation model show as "hazır" (ready) in Settings.
                await catalog.load()
            } catch is CancellationError {
                if session.jobID == id { session.status = .idle }
            } catch {
                if session.jobID == id { session.status = .failed(.error(error)) }
            }
        }
    }

    /// The lines whose translation into `target` can't be kept: none yet, outdated, in another language, empty or a
    /// copy of the original.
    func linesNeedingTranslation(of session: VideoSession? = nil, into target: String) -> [Sentence] {
        let session = session ?? foreground
        return session.sentences.filter { sentence in
            guard let entry = session.translations[sentence.uid] else { return true }
            return !Self.isReusableTranslation(sentence, entry: entry, target: target)
        }
    }

    // MARK: - Rendering

    func render(as format: SubtitleFormat, translated: Bool = false) async -> String? {
        await render(foreground, as: format, translated: translated)
    }

    /// Renders the session's lines with the engine. Jobs (auto-save, the playlist's save) render too, and a cancel ends
    /// the render with a CancellationError that is not a failure; a render that outlives its job must not touch the
    /// status of whatever came next.
    func render(_ session: VideoSession, as format: SubtitleFormat, translated: Bool = false) async -> String? {
        guard let engine = catalog.engine else {
            engineMissing(session)
            return nil
        }
        let id = session.jobID
        do {
            let list = translated ? session.exportSentences(translated: true) : session.sentences
            return try await engine.render(list, as: format)
        } catch is CancellationError {
            return nil
        } catch {
            if session.jobID == id, !Task.isCancelled { session.status = .failed(.error(error)) }
            return nil
        }
    }
}
