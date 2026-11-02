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

/// Opening another video or subtitle replaces what the editor holds. Every way of opening one (the sidebar, a card of the
/// platform, the picker, a drop, Finder's "Open With") goes through `requestOpen`, which asks first when that would stop a
/// running job or throw away edited lines, and otherwise just does it.
extension SubtitleViewModel {
    /// Something would be lost by replacing the editor's contents now. A playlist run owns the editor and refuses opens by
    /// itself, so it is not asked about.
    var openingNeedsConfirmation: Bool {
        !isPlaylistRunning && (isWorking || hasUnsavedEdits)
    }

    /// Whether `url` is the video the editor has open (the same file, however the URL was built).
    func isCurrentVideo(_ url: URL) -> Bool {
        video.map { $0.standardizedFileURL == url.standardizedFileURL } ?? false
    }

    /// Runs `open` now, or once the user has confirmed (`pendingOpen` shows the question; `confirmPendingOpen` and
    /// `cancelPendingOpen` answer it). `replacesWork` is `false` for an open that changes nothing (the video that is
    /// already open), which never asks. A newer request replaces one still waiting.
    func requestOpen(replacesWork: Bool = true, _ open: @escaping @MainActor () -> Void) {
        guard replacesWork, openingNeedsConfirmation else {
            pendingOpen = nil
            open()
            return
        }
        pendingOpen = PendingOpen(stopsJob: isWorking, losesEdits: hasUnsavedEdits, perform: open)
    }

    /// Opens a video (the picker, the sidebar), then runs `followUp`, e.g. starting the run the picker was asked for.
    func requestOpenVideo(_ url: URL, followUp: (@MainActor () -> Void)? = nil) {
        requestOpen(replacesWork: followUp != nil || !isCurrentVideo(url)) { [self] in
            openVideo(url)
            followUp?()
        }
    }

    /// Opens a subtitle file as the editor's lines (the picker). It replaces the lines, so it asks like a video does.
    func requestOpenSubtitle(_ url: URL) {
        requestOpen { [self] in openSubtitle(url) }
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
