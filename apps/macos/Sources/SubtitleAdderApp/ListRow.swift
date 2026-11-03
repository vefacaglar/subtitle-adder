import Foundation

/// One row of the "Çeviri listesi" tab: either an item of the batch playlist or a single video's job (running, waiting or
/// finished during this app run). The rows of both kinds are shown in one list and look alike.
struct ListRow: Identifiable, Equatable {
    enum Origin: Equatable {
        case playlist(UUID)
        case job
    }

    let videoURL: URL
    let origin: Origin
    var state: PlaylistItem.State
    var message: LocalizedMessage
    var fraction: Double?
    /// The item's spoken language (empty means automatic); a single job's language is the toolbar's at the time, not a
    /// property of the row.
    var sourceLanguage: String
    var isAudioReady: Bool
    /// A single job is running or waiting, so the row can stop it.
    var canStop: Bool

    var id: String {
        switch origin {
        case .playlist(let id): "playlist:\(id)"
        case .job: "job:\(SubtitleViewModel.sessionKey(videoURL).path)"
        }
    }

    var fileName: String { videoURL.lastPathComponent }
    var folderPath: String { videoURL.deletingLastPathComponent().path }

    /// The id of the playlist item behind the row, if it is one.
    var playlistItemID: UUID? {
        if case .playlist(let id) = origin { return id }
        return nil
    }

    init(item: PlaylistItem) {
        videoURL = item.videoURL
        origin = .playlist(item.id)
        state = item.state
        message = item.statusMessage
        fraction = item.fraction
        sourceLanguage = item.sourceLanguage
        isAudioReady = item.isAudioReady
        canStop = false
    }

    /// The row of a single video's job, or `nil` when the session shows nothing to report (it is idle: never run, or
    /// stopped).
    @MainActor
    init?(job session: VideoSession, isQueued: Bool) {
        guard let video = session.video else { return nil }
        videoURL = video
        origin = .job
        sourceLanguage = ""
        isAudioReady = false
        switch session.status {
        case .idle:
            return nil
        case .working(let label, let fraction):
            state = isQueued ? .pending : .working
            message = isQueued ? .key("Sırada") : label
            self.fraction = isQueued ? nil : fraction
            canStop = true
        case .finished:
            state = .completed
            let count = session.sentences.count
            let detected = session.detectedLanguage
            let translation = session.translationLanguage
            let saved = session.savedSubtitleName
            message = LocalizedMessage("done:\(count):\(detected ?? "-"):\(translation ?? "-"):\(saved ?? "-")") {
                VideoSession.summary(lines: count, detected: detected, translation: translation, saved: saved)
            }
            fraction = 1
            canStop = false
        case .failed(let failure):
            state = .failed
            message = failure
            fraction = nil
            canStop = false
        }
    }
}

extension SubtitleViewModel {
    /// What the list tab shows: the batch playlist's items, then the single videos' jobs of this run (a video that is
    /// also a playlist item shows once, as the item).
    var listRows: [ListRow] {
        var rows = playlist.map(ListRow.init(item:))
        let listed = Set(playlist.map { Self.sessionKey($0.videoURL) })
        for session in recentJobs {
            guard let video = session.video, !listed.contains(Self.sessionKey(video)),
                  let row = ListRow(job: session, isQueued: jobQueue.isQueued(session))
            else { continue }
            rows.append(row)
        }
        return rows
    }

    var hasListRows: Bool { !listRows.isEmpty }

    /// Single-video jobs waiting for their turn ("Sıradakileri temizle" empties them).
    var queuedJobCount: Int { jobQueue.waiting.count }

    /// A row was clicked: the editor shows that video, with its own session (lines, progress, status).
    func openListRow(_ row: ListRow) {
        showInEditor(row.videoURL)
    }

    /// Takes a finished or failed single-video row off the list; a running or waiting one is stopped with `cancelJob`.
    func removeListRow(_ row: ListRow) {
        switch row.origin {
        case .playlist(let id):
            removeFromPlaylist(id)
        case .job:
            if row.canStop {
                cancelJob(of: row.videoURL)
            } else if let session = existingSession(for: row.videoURL) {
                forgetRecentJob(session)
            }
        }
    }
}
