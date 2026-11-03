import EngineKit
import Foundation

/// Subtitle files: importing an existing one, dropping files, saving a session's lines next to its video, auto-save and
/// "Komple çevir".
extension SubtitleViewModel {
    // MARK: - Importing

    /// Loads an existing subtitle file (.srt, .vtt, .ass, .ssa or the engine's .json) instead of transcribing, as the lines
    /// of `session` (the video on screen when not given). The video stays; translations, undo history and edits are
    /// cleared and display and export go back to the original. A job of that video stops.
    /// Returns `false` with a failed status when the file can't be read or has no valid lines, and `false` with an
    /// `openRefusal` when the playlist run is working on the video.
    @discardableResult
    func loadSubtitleFile(_ url: URL, into session: VideoSession? = nil) -> Bool {
        let session = session ?? foreground
        // The run that works on the video would go on writing into the lines.
        guard session.batchItemID == nil else {
            openRefusal = Self.batchBusyMessage
            return false
        }
        return applySubtitle(url, to: session)
    }

    /// `loadSubtitleFile` without the playlist run's claim on the video: for the run itself and for a session that is
    /// just being made.
    @discardableResult
    func applySubtitle(_ url: URL, to session: VideoSession) -> Bool {
        let parsed: [Sentence]
        do {
            parsed = try SubtitleParser.parseFile(at: url)
        } catch {
            session.status = .failed(.error(error))
            return false
        }
        guard !parsed.isEmpty else {
            session.status = .failed(.key("Altyazı dosyasında geçerli satır bulunamadı."))
            return false
        }
        stopJob(of: session)
        if session === foreground { undoManager?.removeAllActions() }
        session.load(subtitle: url, lines: parsed)
        return true
    }

    /// Files dropped on the window: subtitle files by extension, everything else is a video. Several videos, or any
    /// video while the playlist tab is showing, go to the playlist. One video opens in the editor, and a
    /// subtitle dropped with it is loaded after it (replacing a sidecar). A subtitle dropped alone while no video is
    /// open brings its companion video (same name in the same folder) along.
    func openFiles(_ urls: [URL]) {
        routeFiles(urls, external: false)
    }

    /// Files handed over by Finder ("Open With", a double click, a drop on the Dock icon). Like a drop, except that it
    /// is an explicit request to open: a single video (or a subtitle with its companion video) always shows in the
    /// editor, whichever tab was up, and a subtitle looks for its companion video even when another video is open.
    /// Missing files and folders are skipped. Finder may deliver several files one by one; see `ExternalOpenCoalescer`.
    func openExternalFiles(_ urls: [URL]) {
        let files = urls.filter { url in
            var isDirectory: ObjCBool = false
            return url.isFileURL && FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory)
                && !isDirectory.boolValue
        }
        guard !files.isEmpty else { return }
        routeFiles(files, external: true)
    }

    private func routeFiles(_ urls: [URL], external: Bool) {
        let subtitle = urls.first(where: SubtitleParser.isSubtitleFile)
        let videos = urls.filter { !SubtitleParser.isSubtitleFile($0) }
        if videos.count > 1 || (!videos.isEmpty && workspace == .playlist && !external) {
            openMultipleVideos(videos)
            return
        }
        // Opening a video only brings its session to the front, so it never asks; what a subtitle replaces, or a
        // video-less editor that goes away, might be work (see `requestOpen`).
        let first = videos.first
        let findingVideo = videos.isEmpty && (external || video == nil)
        guard !refusesSubtitle(opening: first, subtitle: subtitle, findingVideo: findingVideo) else { return }
        requestOpen(losing: sessionsLost(opening: first, subtitle: subtitle, findingVideo: findingVideo)) { [self] in
            // The platform has no player or lines of its own: what is dropped on it is opened where it can be seen.
            if external || workspace == .platform { workspace = .editor }
            if let first { openVideo(first) }
            if let subtitle {
                openSubtitle(subtitle, findingVideo: findingVideo)
            }
        }
    }

    /// Opens a subtitle file as the lines of the editor. With `findingVideo` its companion video (`Film.srt` and
    /// `Film.tr.srt` find `Film.mkv`) is opened first, and the subtitle then replaces the sidecar that opening loaded.
    /// Without a companion the open video is closed: the subtitle stands alone, and "Altyazıyı kaydet" writes its own
    /// file instead of the sidecar of a video it has nothing to do with. The picker and drops pass whether a video is
    /// open: the video someone is working on stays. Returns whether the subtitle loaded.
    @discardableResult
    func openSubtitle(_ url: URL, findingVideo: Bool? = nil) -> Bool {
        if findingVideo ?? (video == nil) {
            if let companion = SubtitleParser.findCompanionVideo(for: url) {
                openVideo(companion)
            } else if video != nil {
                openVideo(nil)
            }
        }
        return loadSubtitleFile(url)
    }

    // MARK: - Saving

    /// Whether a video can be picked (toolbar button and ⌘O): in the editor. A running job or playlist run doesn't matter,
    /// since opening a video leaves the others' work alone.
    var canPickVideo: Bool { workspace == .editor }

    /// Whether the lines can be exported: there are some and no job is rewriting them.
    var canExport: Bool { !sentences.isEmpty && !isWorking }

    /// Whether "Altyazıyı kaydet" is enabled: exportable lines and a place to write them (the video's or the loaded
    /// subtitle's folder).
    var canSaveSubtitle: Bool { canSave(foreground) }

    /// Whether the session's lines can be written: there are some, no job is rewriting them and there is a place for
    /// the file (the video's or the loaded subtitle's folder).
    func canSave(_ session: VideoSession) -> Bool {
        !session.sentences.isEmpty && !session.isWorking && (session.video != nil || session.loadedSubtitle != nil)
    }

    /// Whether the export carries the translation: chosen, and there is one.
    var exportsTranslation: Bool { exportLanguage == .translated && hasTranslations }

    /// `<name>.<fmt>` next to the video (or the loaded subtitle when there is no video), for the original and for a
    /// translation alike, exactly like the Windows app. Saving overwrites; there is no language suffix.
    func subtitleFileName() -> String? {
        subtitleFileName(for: foreground, format: exportFormat)
    }

    func subtitleFileName(for session: VideoSession, format: SubtitleFormat) -> String? {
        guard let base = session.video ?? session.loadedSubtitle else { return nil }
        return Self.subtitleFileName(stem: base.deletingPathExtension().lastPathComponent, format: format)
    }

    nonisolated static func subtitleFileName(stem: String, format: SubtitleFormat) -> String {
        "\(stem).\(format.rawValue)"
    }

    /// `<name>[.<lang>].<fmt>`, the export panel's suggestion: a translation carries its language so it doesn't
    /// replace the original by accident.
    nonisolated static func exportFileName(stem: String, language: String?, format: SubtitleFormat) -> String {
        let suffix = language.map { "." + $0 } ?? ""
        return "\(stem)\(suffix).\(format.rawValue)"
    }

    /// Where "Altyazıyı kaydet" writes: next to the video (or the loaded subtitle).
    func subtitleSaveURL() -> URL? {
        subtitleSaveURL(for: foreground, format: exportFormat)
    }

    func subtitleSaveURL(for session: VideoSession, format: SubtitleFormat) -> URL? {
        guard let base = session.video ?? session.loadedSubtitle,
              let name = subtitleFileName(for: session, format: format)
        else { return nil }
        return base.deletingLastPathComponent().appendingPathComponent(name, isDirectory: false)
    }

    /// Default name in the export panel.
    var exportFilename: String {
        let stem = (video ?? loadedSubtitle)?.deletingPathExtension().lastPathComponent ?? "subtitles"
        return Self.exportFileName(
            stem: stem,
            language: exportsTranslation ? translationLanguage ?? "translated" : nil,
            format: exportFormat
        )
    }

    /// Renders the lines of the video on screen with the engine and writes the file next to the video as UTF-8 without a
    /// BOM, replacing a file of the same name. `translated` overrides the export language choice. On failure the status
    /// says why and nothing is returned.
    @discardableResult
    func saveSubtitle(translated: Bool? = nil) async -> URL? {
        commitDraft()
        commitTranslationDraft()
        return await saveSubtitle(of: foreground, translated: translated, format: exportFormat)
    }

    /// The same for any session: the lines rendered, the file written and the name and status that follow are all that
    /// session's, whichever video is on screen while the render and the write are awaited.
    @discardableResult
    func saveSubtitle(of session: VideoSession, translated: Bool? = nil, format: SubtitleFormat) async -> URL? {
        let useTranslation = (translated ?? (session.exportLanguage == .translated)) && session.hasTranslations
        guard let url = subtitleSaveURL(for: session, format: format) else { return nil }
        // The render is awaited; a subtitle file may be loaded onto the video meanwhile. What follows belongs to the
        // lines this save started for.
        let savedLoadedSubtitle = session.loadedSubtitle
        guard let text = await render(session, as: format, translated: useTranslation) else { return nil }
        do {
            try await Self.write(Data(text.utf8), to: url)
        } catch {
            session.status = Self.saveFailure(error)
            return nil
        }
        // The file is on disk either way, so the sidebar badge always follows; the session's own state only changes
        // when it still holds the lines the save was for.
        onSubtitleSaved?(url)
        guard session.loadedSubtitle == savedLoadedSubtitle else { return url }
        session.savedSubtitleName = url.lastPathComponent
        if !session.isWorking, session.status != .finished { session.status = .finished }
        return url
    }

    /// Writes `data` to `url` atomically on a background thread: a slow, network or sleeping disk must not block the
    /// main actor.
    nonisolated static func write(_ data: Data, to url: URL) async throws {
        try await Task.detached(priority: .utility) { try data.write(to: url, options: .atomic) }.value
    }

    /// Shows "Dosya kaydedilemedi: …" in the status bar; used by every place that writes a subtitle file.
    func reportSaveFailure(_ error: any Error) {
        status = Self.saveFailure(error)
    }

    nonisolated static func saveFailure(_ error: any Error) -> Status {
        let reason = error.localizedDescription
        return .failed(LocalizedMessage("save-failed:\(reason)") { L("Dosya kaydedilemedi: %@", reason) })
    }

    /// Result of the export panel: a write error is reported, a cancelled panel is not an error.
    /// A successful export clears an earlier "Dosya kaydedilemedi" so it doesn't look like this one failed too.
    func exportFinished(_ result: Result<URL, any Error>) {
        switch result {
        case .success:
            // What the panel wrote is on disk, so leaving the lines loses nothing of it.
            hasUnsavedEdits = false
            if !isWorking, case .failed(let message) = status, message.id.hasPrefix("save-failed:") {
                status = .finished
            }
        case .failure(let error):
            guard !Self.isUserCancellation(error) else { return }
            reportSaveFailure(error)
        }
    }

    nonisolated static func isUserCancellation(_ error: any Error) -> Bool {
        let nsError = error as NSError
        return nsError.domain == NSCocoaErrorDomain && nsError.code == NSUserCancelledError
    }

    /// Writes the translation next to the video when auto-save is on and there is a translation and a folder to write to.
    /// Called when a translation job finishes; transcription alone and the export panel never save by themselves.
    /// `session` is the video the job was for and `settings` the job's own; the video on screen and the toolbar's choices
    /// when not given.
    func autoSaveTranslationIfEnabled(
        of session: VideoSession? = nil, settings: JobSettings? = nil, fileManager: FileManager = .default
    ) async {
        let session = session ?? foreground
        let autoSave = settings?.autoSave ?? autoSaveSubtitle
        let format = settings?.exportFormat ?? exportFormat
        guard autoSave, session.hasTranslations, canSave(session),
              let folder = subtitleSaveURL(for: session, format: format)?.deletingLastPathComponent()
        else { return }
        var isDirectory: ObjCBool = false
        guard fileManager.fileExists(atPath: folder.path, isDirectory: &isDirectory), isDirectory.boolValue else { return }
        await saveSubtitle(of: session, translated: true, format: format)
    }

    // MARK: - Komple çevir

    /// Whether "Komple çevir" can run; with neither a video nor lines it first asks for a video.
    var canGenerateAndTranslate: Bool { !isWorking }

    /// Whether there is anything to run on yet (otherwise the view asks for a video first).
    var hasSourceForGenerateAndTranslate: Bool { video != nil || !sentences.isEmpty }

    /// Generate, translate and auto-save in one step. With a video open the audio is always transcribed from scratch,
    /// replacing any sidecar or imported lines: what sits next to the video can be an earlier translation (saved as
    /// `<video>.srt`), which must not be translated again as if it were the source. Lines without a video (an
    /// imported subtitle file) are only translated.
    func generateAndTranslate() {
        commitDraft()
        if video == nil, !sentences.isEmpty {
            translate()
        } else {
            generate(thenTranslate: true)
        }
    }

    // MARK: - Status text

    /// The playback problem to show next to the status line, or `nil`. It never replaces the status (the ready hint and
    /// the finished summary stay visible); it is only hidden while a job's progress or a real error owns the bar, and
    /// outside the editor. Generation is never blocked.
    func playbackNotice(_ failure: LocalizedMessage?) -> LocalizedMessage? {
        guard let failure, workspace == .editor, !isAsyncTranslating else { return nil }
        switch status {
        case .idle, .finished: return failure
        case .working, .failed: return nil
        }
    }
}
