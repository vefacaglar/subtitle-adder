import EngineKit
import Foundation

/// Subtitle files: importing an existing one, dropping files, saving next to the video, auto-save and "Komple çevir".
extension SubtitleViewModel {
    // MARK: - Importing

    /// Loads an existing subtitle file (.srt, .vtt, .ass, .ssa or the engine's .json) instead of transcribing. The current
    /// video stays; translations, undo history and edits are cleared and display and export go back to the original.
    /// Returns `false` with a failed status when the file can't be read or has no valid lines.
    @discardableResult
    func loadSubtitleFile(_ url: URL) -> Bool {
        let parsed: [Sentence]
        do {
            parsed = try SubtitleParser.parseFile(at: url)
        } catch {
            status = .failed(.error(error))
            return false
        }
        guard !parsed.isEmpty else {
            status = .failed(.key("Altyazı dosyasında geçerli satır bulunamadı."))
            return false
        }
        job?.cancel()
        job = nil
        jobID = UUID()
        cancelAsyncTranslation()
        catalog.activeModels = []
        loadedSubtitle = url
        savedSubtitleName = nil
        translations = [:]
        translationDraft = nil
        translationLanguage = nil
        selection = nil
        draft = nil
        // A subtitle file doesn't say which language it is in; a previous video's detection would be wrong.
        detectedLanguage = nil
        linesSourceLanguage = nil
        undoManager?.removeAllActions()
        displayMode = .original
        exportLanguage = .original
        sentences = parsed
        status = .finished
        return true
    }

    /// Files dropped on the window or handed over by Finder: subtitle files by extension, everything else is a video.
    /// Several videos, or any video while the playlist is showing or running, go to the playlist. One video opens in
    /// the editor, and a subtitle dropped with it is loaded after it (replacing a sidecar).
    func openFiles(_ urls: [URL]) {
        let subtitle = urls.first(where: SubtitleParser.isSubtitleFile)
        let videos = urls.filter { !SubtitleParser.isSubtitleFile($0) }
        if videos.count > 1 || (!videos.isEmpty && (workspace == .playlist || isPlaylistRunning)) {
            openMultipleVideos(videos)
            return
        }
        // The run owns the editor's video and lines until it ends.
        guard !isPlaylistRunning else { return }
        if let video = videos.first { openVideo(video) }
        if let subtitle { loadSubtitleFile(subtitle) }
    }

    // MARK: - Saving

    /// Whether the lines can be exported: there are some and no job is rewriting them.
    var canExport: Bool { !sentences.isEmpty && !isWorking }

    /// Whether "Altyazıyı kaydet" is enabled: exportable lines and a place to write them (the video's or the loaded
    /// subtitle's folder).
    var canSaveSubtitle: Bool { canExport && (video != nil || loadedSubtitle != nil) }

    /// Whether the export carries the translation: chosen, and there is one.
    var exportsTranslation: Bool { exportLanguage == .translated && hasTranslations }

    /// `<name>.<fmt>` next to the video (or the loaded subtitle when there is no video), for the original and for a
    /// translation alike, exactly like the Windows app. Saving overwrites; there is no language suffix.
    func subtitleFileName() -> String? {
        guard let base = video ?? loadedSubtitle else { return nil }
        return Self.subtitleFileName(stem: base.deletingPathExtension().lastPathComponent, format: exportFormat)
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
        guard let base = video ?? loadedSubtitle, let name = subtitleFileName() else { return nil }
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

    /// Renders with the engine and writes the file next to the video as UTF-8 without a BOM, replacing a file of the
    /// same name. `translated` overrides the export language choice. On failure the status says why and nothing is returned.
    @discardableResult
    func saveSubtitle(translated: Bool? = nil) async -> URL? {
        commitDraft()
        commitTranslationDraft()
        let useTranslation = (translated ?? (exportLanguage == .translated)) && hasTranslations
        guard let url = subtitleSaveURL() else { return nil }
        guard let text = await render(as: exportFormat, translated: useTranslation) else { return nil }
        do {
            try Data(text.utf8).write(to: url, options: .atomic)
        } catch {
            let reason = error.localizedDescription
            status = .failed(LocalizedMessage("save-failed:\(reason)") { L("Dosya kaydedilemedi: %@", reason) })
            return nil
        }
        savedSubtitleName = url.lastPathComponent
        if !isWorking, status != .finished { status = .finished }
        return url
    }

    /// Writes the translation next to the video when auto-save is on and there is a translation and a folder to write to.
    /// Called when a translation job finishes; transcription alone and the export panel never save by themselves.
    func autoSaveTranslationIfEnabled(fileManager: FileManager = .default) async {
        guard autoSaveSubtitle, hasTranslations, canSaveSubtitle,
              let folder = subtitleSaveURL()?.deletingLastPathComponent()
        else { return }
        var isDirectory: ObjCBool = false
        guard fileManager.fileExists(atPath: folder.path, isDirectory: &isDirectory), isDirectory.boolValue else { return }
        await saveSubtitle(translated: true)
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

    /// "N satır · dil: xx" (the language is left out when unknown), plus the translation language and the saved file.
    var finishedSummary: String {
        var summary = detectedLanguage.map { L("%lld satır · dil: %@", sentences.count, $0) }
            ?? L("%lld satır", sentences.count)
        if let language = translationLanguage {
            summary += L(" · çeviri: %@", language)
        }
        if let saved = savedSubtitleName {
            summary += L(" · kaydedildi: %@", saved)
        }
        return summary
    }
}
