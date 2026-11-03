import EngineKit
import Foundation

/// An open request that waits for the user's yes, and what it would cost.
struct PendingOpen {
    /// A job (recognition, translation) is running and opening stops it.
    let stopsJob: Bool
    /// Edited lines, or a translation that was never saved, would be dropped.
    let losesEdits: Bool
    let perform: @MainActor () -> Void

    /// The confirmation's text, saying what is actually at stake.
    var message: String {
        switch (stopsJob, losesEdits) {
        case (true, true): L("Süren iş durdurulacak ve kaydedilmemiş değişiklikler kaybolacak. Devam edilsin mi?")
        case (true, false): L("Süren iş durdurulacak. Devam edilsin mi?")
        default: L("Kaydedilmemiş değişiklikler kaybolacak. Devam edilsin mi?")
        }
    }
}

/// Opening a video brings its session to the front and leaves the one it replaces exactly as it is, running job and
/// unsaved edits included, so switching between videos never asks. Only an open that really destroys something asks: a
/// subtitle file loaded onto lines that have a running job or unsaved edits, a job restarted over them, or lines that
/// belong to no video (a subtitle file on its own), which have nowhere to stay when another file is opened.
///
/// Every way of opening a file (the sidebar, a card of the platform, the picker, a drop, Finder's "Open With") goes
/// through `requestOpen`, which asks first when a session would lose something, and otherwise just does it.
extension SubtitleViewModel {
    /// Something would be lost by replacing the lines on screen now.
    var openingNeedsConfirmation: Bool {
        Self.atStake(in: [foreground]) != nil
    }

    /// Whether `url` is the video the editor has open (the same file, however the URL was built).
    func isCurrentVideo(_ url: URL) -> Bool {
        video.map { Self.sessionKey($0) == Self.sessionKey(url) } ?? false
    }

    /// What losing `sessions` would cost, or `nil` when it costs nothing.
    private static func atStake(in sessions: [VideoSession]) -> (stopsJob: Bool, losesEdits: Bool)? {
        let stopsJob = sessions.contains { $0.isWorking }
        let losesEdits = sessions.contains { $0.hasUnsavedEdits }
        return stopsJob || losesEdits ? (stopsJob, losesEdits) : nil
    }

    /// The sessions whose work an open would destroy:
    /// - the lines of a video that a subtitle file (or "Komple çevir" right after the pick, `restarting`) replaces;
    /// - the lines on screen when they belong to no video and the open takes their place.
    /// Showing another video's session replaces nothing.
    func sessionsLost(
        opening video: URL?, subtitle: URL? = nil, findingVideo: Bool? = nil, restarting: Bool = false
    ) -> [VideoSession] {
        var lost: [VideoSession] = []
        func add(_ session: VideoSession?) {
            if let session, !lost.contains(where: { $0 === session }) { lost.append(session) }
        }
        // The session whose lines the open replaces, if any.
        var replaced: VideoSession?
        if let video {
            let isCurrent = isCurrentVideo(video)
            // Lines that belong to no video have nowhere to stay once another video is shown.
            if !isCurrent, foreground.video == nil { add(foreground) }
            if subtitle != nil || restarting { replaced = isCurrent ? foreground : existingSession(for: video) }
        } else if let subtitle {
            if findingVideo ?? (self.video == nil) {
                if let companion = SubtitleParser.findCompanionVideo(for: subtitle) {
                    // The companion video takes the lines.
                    if !isCurrentVideo(companion), foreground.video == nil { add(foreground) }
                    replaced = isCurrentVideo(companion) ? foreground : existingSession(for: companion)
                } else if self.video == nil {
                    // Lines on their own are replaced by the next lines on their own. A video on screen is just closed
                    // (its session stays), and the subtitle starts a new video-less one.
                    replaced = foreground
                }
            } else {
                replaced = foreground
            }
        }
        add(replaced)
        return lost
    }

    /// Runs `open` now, or once the user has confirmed (`pendingOpen` shows the question; `confirmPendingOpen` and
    /// `cancelPendingOpen` answer it). `losing` are the sessions the open would destroy the work of; with none, or none
    /// that has anything at stake, it never asks. A newer request replaces one still waiting.
    func requestOpen(losing sessions: [VideoSession], _ open: @escaping @MainActor () -> Void) {
        guard let stake = Self.atStake(in: sessions) else {
            pendingOpen = nil
            open()
            return
        }
        pendingOpen = PendingOpen(stopsJob: stake.stopsJob, losesEdits: stake.losesEdits, perform: open)
    }

    /// An open that replaces the lines on screen (`replacesWork`), or one that changes nothing (`false`).
    func requestOpen(replacesWork: Bool = true, _ open: @escaping @MainActor () -> Void) {
        requestOpen(losing: replacesWork ? [foreground] : [], open)
    }

    /// Opens a video (the picker, the sidebar), then runs `followUp`, e.g. starting the run the picker was asked for.
    func requestOpenVideo(_ url: URL, followUp: (@MainActor () -> Void)? = nil) {
        requestOpen(losing: sessionsLost(opening: url, restarting: followUp != nil)) { [self] in
            openVideo(url)
            followUp?()
        }
    }

    /// Opens a subtitle file as the editor's lines (the picker). It replaces the lines, so it asks like a video does.
    func requestOpenSubtitle(_ url: URL) {
        requestOpen(losing: sessionsLost(opening: nil, subtitle: url)) { [self] in openSubtitle(url) }
    }

    /// The user said yes: the waiting open runs, stopping the job and dropping the edits it was about.
    func confirmPendingOpen() {
        guard let pending = pendingOpen else { return }
        pendingOpen = nil
        pending.perform()
    }

    /// The user said no: nothing changes.
    func cancelPendingOpen() {
        pendingOpen = nil
    }
}
