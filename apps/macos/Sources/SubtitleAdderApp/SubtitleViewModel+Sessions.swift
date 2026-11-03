import EngineKit
import Foundation

/// One session per video: opening a video brings its session to the front, leaving it keeps the session (with a job that
/// may still be running in it) for as long as the app runs.
///
/// - A session is created the first time its video is opened or a job is requested for it, and is looked up by the
///   video's standardized file URL.
/// - The foreground session is the one on screen. Opening a video never cancels anything: the job of the video that is
///   left goes on and writes into its own session.
/// - A session that holds nothing worth keeping (no lines, no job) is dropped when the user leaves it, and so is one
///   whose lines are just the sidecar file as it is on disk; reopening the video would give the same thing back.
/// - Sessions are not persisted: they live for the app run only.
/// - There is one undo history, the foreground session's. It is cleared whenever another video comes to the front,
///   because the window's undo manager is shared and a stack kept per video would need a manager per video.
extension SubtitleViewModel {
    // MARK: - Looking sessions up

    nonisolated static func sessionKey(_ url: URL) -> URL { url.standardizedFileURL }

    /// The session of `url` if it has one right now.
    func existingSession(for url: URL) -> VideoSession? { sessions[Self.sessionKey(url)] }

    /// The session of `url`, created and registered if needed. A new session starts with the sidecar subtitle file next
    /// to the video, if there is one (no transcription needed then); one that can't be read shows as the session's failure.
    func session(for url: URL) -> VideoSession {
        if let existing = existingSession(for: url) { return existing }
        let session = makeSession(for: url)
        sessions[Self.sessionKey(url)] = session
        if let sidecar = SubtitleParser.findSidecar(for: url) { loadSubtitleFile(sidecar, into: session) }
        return session
    }

    func makeSession(for video: URL?) -> VideoSession {
        let session = VideoSession(video: video, displayMode: Self.storedDisplayMode(in: defaults))
        wire(session)
        return session
    }

    /// Connects a session to the view model: the foreground session's display choice is remembered for the videos opened
    /// next, and a batch item's row follows its recognition job.
    func wire(_ session: VideoSession) {
        session.onDisplayModeChange = { [weak self] session in
            guard let self, session === foreground else { return }
            defaults.set(session.displayMode.rawValue, forKey: Self.displayModeDefaultsKey)
        }
        session.onStatusChange = { [weak self] session in self?.sessionStatusChanged(session) }
    }

    // MARK: - Opening

    /// Brings the session of `url` to the front (or an empty, video-less one for `nil`). The video that is left keeps
    /// its lines, status and job; the editor's undo history starts over.
    func openVideo(_ url: URL?) {
        if let url, let current = video, Self.sessionKey(current) == Self.sessionKey(url) { return }
        if url == nil, video == nil { return }
        leaveForeground()
        let next = url.map { session(for: $0) } ?? makeSession(for: nil)
        foreground = next
    }

    /// What happens to the session on screen when another one takes its place.
    private func leaveForeground() {
        // Typed but uncommitted text belongs to the session it was typed in.
        commitDraft()
        commitTranslationDraft()
        undoManager?.removeAllActions()
        let old = foreground
        guard let video = old.video else {
            // Lines of a subtitle file on their own have no key to come back by: they and their job end here.
            stopJob(of: old)
            return
        }
        let key = Self.sessionKey(video)
        if sessions[key] === old, !isKept(old) { sessions[key] = nil }
    }

    /// Whether a session must stay registered: it has content or a job, the list tab shows it, or it is a batch item's.
    private func isKept(_ session: VideoSession) -> Bool {
        if recentJobs.contains(where: { $0 === session }) { return true }
        if session.batchItemID != nil || session.isPlaylistTranslating || jobQueue.isQueued(session) { return true }
        return session.hasContent && !session.isReconstructible
    }

    /// Drops registered sessions nobody needs; called when a batch run ends.
    func pruneSessions() {
        for (key, session) in sessions where session !== foreground && !isKept(session) {
            sessions[key] = nil
        }
    }

    /// Shows a video of the list tab: the editor with that video's own session.
    func showInEditor(_ video: URL) {
        workspace = .editor
        openVideo(video)
    }

    // MARK: - Keeping the toolbar and the model catalog straight

    /// Marks the local models a running job uses, so Settings doesn't offer to delete them. Released with the job.
    func claimModels(_ models: Set<String>, for job: UUID) {
        modelClaims[job] = models.isEmpty ? nil : models
        publishModelClaims()
    }

    func releaseModels(of job: UUID) {
        guard modelClaims.removeValue(forKey: job) != nil else { return }
        publishModelClaims()
    }

    private func publishModelClaims() {
        catalog.activeModels = modelClaims.values.reduce(into: Set<String>()) { $0.formUnion($1) }
    }

    // MARK: - The list tab's rows for single jobs

    /// Remembers that the list tab shows this video's job.
    func noteRecentJob(_ session: VideoSession) {
        guard session.video != nil, !recentJobs.contains(where: { $0 === session }) else { return }
        recentJobs.append(session)
        if let video = session.video { sessions[Self.sessionKey(video)] = session }
    }

    func forgetRecentJob(_ session: VideoSession) {
        recentJobs.removeAll { $0 === session }
    }
}
