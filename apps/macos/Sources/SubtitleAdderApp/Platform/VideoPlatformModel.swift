import EngineKit
import Foundation
import LibraryKit
import Observation

/// The open video's views, favorite flag, stars and lists, for the bar on the player. Kept apart from the feed, so a
/// change of one never redraws the other, and playback ticks touch neither.
@MainActor
@Observable
final class PlayerEngagement {
    /// The video in the player (nothing while a playlist run owns it).
    private(set) var url: URL?
    /// The numbers below belong to `url`; false until the first refresh after the video changed.
    private(set) var isLoaded = false
    private(set) var views = 0
    private(set) var isLiked = false
    private(set) var rating = 0
    private(set) var listIDs: Set<String> = []

    func reset(url: URL?) {
        self.url = url
        isLoaded = false
        views = 0
        isLiked = false
        rating = 0
        listIDs = []
    }

    func apply(_ snapshot: EngagementSnapshot?) {
        guard let snapshot, snapshot.url == url else { return }
        if views != snapshot.views { views = snapshot.views }
        if isLiked != snapshot.isLiked { isLiked = snapshot.isLiked }
        if rating != snapshot.rating { rating = snapshot.rating }
        if listIDs != snapshot.listIDs { listIDs = snapshot.listIDs }
        if !isLoaded { isLoaded = true }
    }

    func setLiked(_ liked: Bool) { if isLoaded, isLiked != liked { isLiked = liked } }
    func setRating(_ stars: Int) { if isLoaded, rating != stars { rating = stars } }
}

/// A moment of playback, reported by the player.
struct PlaybackPosition: Equatable, Sendable {
    var seconds: Double
    var duration: Double
}

/// What the player tells the platform: playback started, or stopped at a position, or jumped, or is about to be
/// replaced by another video (with where the old one was).
enum PlaybackEvent: Equatable, Sendable {
    case started
    case paused(PlaybackPosition)
    case seeked(PlaybackPosition)
    case leaving(PlaybackPosition)
}

/// The local video platform: the profile (login, lists, favorites, stars, history), the feed over the indexed videos,
/// and the engagement of the video in the player. The state the views read is a snapshot, built off the main actor by
/// `PlatformFeedBuilder` and applied here; every call into `ProfileStore` goes through a `StoreQueue`, because the
/// store can wait (its locks, PBKDF2, a slow disk) and must never stall the interface. A card click, for one, opens
/// the video at once and never waits for the profile.
///
/// Three kinds of change reach the snapshot: the user's own actions (which end with a refresh), changes in
/// `ProfileStore.changes()` (another window or instance, a login restored from the keychain) and changes in the media
/// index. Playback progress and thumbnails change nothing the lists show, and raise no refresh.
@MainActor
@Observable
final class VideoPlatformModel {
    // MARK: Profile

    private(set) var username: String?
    private(set) var registeredUsernames: [String] = []
    /// The user who last logged in, to pre-fill the login box.
    private(set) var lastUsername: String?
    /// "Girişi hatırla"; applied by the next login or registration.
    var rememberMe = true
    private(set) var counts = ProfileCounts()
    private(set) var isAuthenticating = false
    /// Why the last login, registration or profile deletion failed; cleared by the next attempt.
    private(set) var authFailure: ProfileError?
    /// A failed action that has no place of its own on screen (a like, a star, a list change); shown in an alert.
    var actionFailure: ProfileError?

    var isLoggedIn: Bool { username != nil }
    var authError: String? { authFailure?.message }

    /// "3 izlenen · 2 beğenilen".
    var summaryText: String {
        isLoggedIn ? L("%lld izlenen · %lld beğenilen", counts.watched, counts.liked) : L("İzleme geçmişi için giriş yapın")
    }

    /// The toolbar button: the user's name, or the invitation to log in.
    var profileTitle: String { username ?? L("Profil / Giriş") }

    // MARK: Hub

    var filter: VideoFeedFilter = .all {
        didSet {
            guard filter != oldValue else { return }
            requestedSignIn = false
            scheduleRefresh()
        }
    }
    /// The text in the search field. Searching starts on Enter; emptying the field shows the whole list again.
    var searchText = "" {
        didSet {
            if searchText.isEmpty, !searchQuery.isEmpty { clearSearch() }
        }
    }
    /// The search that is applied to the feed.
    private(set) var searchQuery = ""
    private(set) var cards: [VideoCard] = []
    private(set) var lists: [UserVideoList] = []
    /// The first feed has arrived (before it, the hub shows a spinner, not an empty state).
    private(set) var hasLoadedFeed = false
    private(set) var isRefreshing = false
    /// Why the last list creation or rename failed; cleared by the next attempt.
    private(set) var listFailure: ProfileError?
    /// "Giriş yap / Profil oluştur" was pressed while on "Keşfet": show the sign-in card there.
    private(set) var requestedSignIn = false
    /// The hub is on screen. Off screen, a refresh keeps the header and the player bar current and skips the cards.
    var isHubActive = false {
        didSet {
            if isHubActive, !oldValue { scheduleRefresh() }
        }
    }

    let engagement = PlayerEngagement()

    var listError: String? { listFailure?.message }

    /// The sign-in card replaces the cards: on a personal tab while logged out (unless searching), or on request.
    var showsAuthCard: Bool {
        !isLoggedIn && (requestedSignIn || (filter != .all && searchQuery.isEmpty))
    }

    var selectedList: UserVideoList? {
        guard case .customList(let id) = filter else { return nil }
        return lists.first { $0.id == id }
    }

    /// The history tab has something to clear.
    var canClearHistory: Bool { isLoggedIn && filter == .history && !cards.isEmpty }

    // MARK: Dependencies

    @ObservationIgnored let store: ProfileStore
    @ObservationIgnored let index: MediaLibraryIndex
    @ObservationIgnored let services: PlatformServices
    @ObservationIgnored let thumbnails: ThumbnailService
    @ObservationIgnored private let queues = StoreQueues()

    /// The video in the player, as far as the platform knows (`nil` while a playlist run owns the player).
    @ObservationIgnored private(set) var playerVideo: URL?
    /// Where the player is now; set by the window, read by the progress timer and when the app quits.
    @ObservationIgnored var playbackSampler: (@MainActor () -> PlaybackPosition?)?
    @ObservationIgnored private var progressTask: Task<Void, Never>?
    @ObservationIgnored private var seekFlushTask: Task<Void, Never>?

    @ObservationIgnored private var refreshTask: Task<Void, Never>?
    @ObservationIgnored private var refreshPending = false
    @ObservationIgnored private var actionTail: Task<Void, Never>?
    @ObservationIgnored private var pendingActions = 0
    @ObservationIgnored private var changesTask: Task<Void, Never>?
    @ObservationIgnored private var indexTask: Task<Void, Never>?
    @ObservationIgnored private var hasAppliedFirstSnapshot = false
    @ObservationIgnored private var appliedFeedKey: FeedKey?
    /// Folders listed for subtitle badges by the last feed build (tests).
    @ObservationIgnored private(set) var lastBadgeListings = 0

    init(
        store: ProfileStore, index: MediaLibraryIndex, services: PlatformServices = .system,
        thumbnailGenerator: @escaping ThumbnailService.Generator = ThumbnailGenerator.jpegData(for:)
    ) {
        self.store = store
        self.index = index
        self.services = services
        let queues = queues
        thumbnails = ThumbnailService(
            read: { [store] url in await queues.thumbnails.run { store.thumbnail(for: url) } },
            write: { [store] data, url in await queues.thumbnails.run { _ = try? store.setThumbnail(data, for: url) } },
            generate: thumbnailGenerator)
    }

    isolated deinit {
        changesTask?.cancel()
        indexTask?.cancel()
        progressTask?.cancel()
        seekFlushTask?.cancel()
    }

    // MARK: Lifecycle

    /// Follows the profile store and the media index and builds the first snapshot. Runs once; returns when that
    /// snapshot is in.
    func start() async {
        if changesTask == nil {
            // The streams exist before the first snapshot, so nothing that happens meanwhile is lost.
            let storeChanges = store.changes()
            changesTask = Task { [weak self] in
                for await _ in storeChanges {
                    guard let self else { return }
                    scheduleRefresh()
                }
            }
            let indexChanges = index.changes
            indexTask = Task { [weak self] in
                for await _ in indexChanges {
                    guard let self else { return }
                    indexChanged()
                }
            }
        }
        // A remembered login is restored from the keychain, which can ask the user for permission: never on the main
        // thread. The store does it here on a queue, and the snapshot that follows shows the result.
        let store = store
        await queues.actions.run { store.refresh() }
        await refresh()
    }

    /// The index learned about other files: "Keşfet" and the search over it change, nothing else does.
    private func indexChanged() {
        guard isHubActive, filter == .all || !searchQuery.isEmpty else { return }
        scheduleRefresh()
    }

    // MARK: Refresh

    /// Asks for a new snapshot. Several requests in a row are served by one more build.
    func scheduleRefresh() {
        refreshPending = true
        if refreshTask == nil {
            isRefreshing = true
            refreshTask = Task { await runRefreshes() }
        }
    }

    /// Builds a new snapshot and returns when it is applied.
    func refresh() async {
        scheduleRefresh()
        await refreshTask?.value
    }

    /// Returns when every action started so far and every refresh they asked for are over.
    func settle() async {
        while pendingActions > 0 || refreshTask != nil {
            await actionTail?.value
            await refreshTask?.value
        }
    }

    /// What the cards on screen were built for; a different one means they are about another tab or query.
    private struct FeedKey: Equatable {
        let filter: VideoFeedFilter
        let search: String
        let loggedIn: Bool
    }

    private func runRefreshes() async {
        while refreshPending {
            refreshPending = false
            let wantsFeed = isHubActive
            let search = searchQuery
            let key = FeedKey(filter: filter, search: search, loggedIn: isLoggedIn)
            let discovered = await discoveredPaths(wantsFeed: wantsFeed, search: search, loggedIn: key.loggedIn)
            var request = FeedRequest(
                filter: filter, search: search, discoveredPaths: discovered, wantsFeed: wantsFeed, playerVideo: playerVideo)
            let badges = SubtitleBadges(listDirectory: services.listDirectory)

            // Following moved files means reading file slices the first time, which can take a while on a cold library.
            // A tab or query that has nothing on screen yet gets the quick build first (no file is opened), then the one
            // that follows the moves.
            if wantsFeed, !discovered.isEmpty, cards.isEmpty || appliedFeedKey != key {
                request.fingerprints = .pathOnly
                let quick = await build(request, badges: badges)
                if refreshPending { continue }
                apply(quick, wantsFeed: wantsFeed, key: key)
                request.fingerprints = .compute
            }
            let snapshot = await build(request, badges: badges)
            // Something asked again while this was being built: this one is already out of date.
            if refreshPending { continue }
            apply(snapshot, wantsFeed: wantsFeed, key: key)
        }
        isRefreshing = false
        refreshTask = nil
    }

    private func build(_ request: FeedRequest, badges: SubtitleBadges) async -> PlatformSnapshot {
        let store = store, services = services, thumbnails = thumbnails
        return await queues.feed.run {
            PlatformFeedBuilder.snapshot(
                store: store, request: request, services: services, badges: badges,
                seedThumbnail: { path, data in thumbnails.seed(data, for: path) })
        }
    }

    /// The indexed files "Keşfet" shows (and the search looks through): the newest ones, plus the best name matches
    /// for a query, since the newest few hundred need not contain what is being looked for.
    private func discoveredPaths(wantsFeed: Bool, search: String, loggedIn: Bool) async -> [String] {
        guard wantsFeed, filter == .all || (!loggedIn && !search.isEmpty) else { return [] }
        let limit = services.discoveredLimit
        var files = await index.allFiles(limit: limit)
        if !search.isEmpty { files += await index.search(search, limit: limit) }
        var seen = Set<String>()
        return files.map(\.fullPath).filter { seen.insert($0.lowercased()).inserted }
    }

    private func apply(_ snapshot: PlatformSnapshot, wantsFeed: Bool, key: FeedKey) {
        if username != snapshot.username { username = snapshot.username }
        if registeredUsernames != snapshot.registeredUsernames { registeredUsernames = snapshot.registeredUsernames }
        if lastUsername != snapshot.lastUsername { lastUsername = snapshot.lastUsername }
        if !hasAppliedFirstSnapshot {
            hasAppliedFirstSnapshot = true
            rememberMe = snapshot.rememberMe
        }
        if counts != snapshot.counts { counts = snapshot.counts }
        if lists != snapshot.lists { lists = snapshot.lists }
        engagement.apply(snapshot.engagement)
        // Off screen no cards were built; what is there stays until the hub opens and a refresh replaces it.
        if wantsFeed {
            appliedFeedKey = key
            let cards = snapshot.cards ?? []
            if self.cards != cards { self.cards = cards }
            if snapshot.cards != nil, !hasLoadedFeed { hasLoadedFeed = true }
        }
        lastBadgeListings = snapshot.listings
        // The list on show is gone (deleted elsewhere, or another profile logged in): back to "Keşfet".
        if case .customList(let id) = filter, !snapshot.lists.contains(where: { $0.id == id }) {
            filter = .all
        }
    }

    // MARK: Search

    /// Applies the text in the field (Enter, or the "Ara" button).
    func submitSearch() {
        requestedSignIn = false
        searchQuery = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        scheduleRefresh()
    }

    func clearSearch() {
        searchQuery = ""
        if !searchText.isEmpty { searchText = "" }
        scheduleRefresh()
    }

    func requestSignIn() {
        requestedSignIn = true
    }

    /// A pill was pressed. Unlike setting `filter`, this also puts an explicitly requested sign-in card away when the
    /// pill is the tab that is already open.
    func select(_ filter: VideoFeedFilter) {
        requestedSignIn = false
        self.filter = filter
    }

    // MARK: Actions

    /// Runs `work` after the actions before it. Not awaited by callers: the interface moves on at once.
    func enqueue(_ work: @escaping @MainActor () async -> Void) {
        pendingActions += 1
        let previous = actionTail
        actionTail = Task {
            await previous?.value
            await work()
            pendingActions -= 1
        }
    }

    /// Runs a store call on the actions queue.
    func runStore<T: Sendable>(
        _ body: @escaping @Sendable (ProfileStore) throws(ProfileError) -> T
    ) async -> Result<T, ProfileError> {
        let store = store
        return await queues.actions.run {
            Result<T, ProfileError>(catching: { () throws(ProfileError) -> T in try body(store) })
        }
    }

    /// Runs a store call as an action; a failure is shown in the alert, and the screen is brought back in step.
    func perform(_ body: @escaping @Sendable (ProfileStore) throws(ProfileError) -> Void) {
        enqueue { [self] in
            if case .failure(let error) = await runStore(body) { actionFailure = error }
            scheduleRefresh()
        }
    }

    // MARK: Accounts

    func login(username: String, password: String) async -> Bool {
        await authenticate { (store, remember) throws(ProfileError) in
            try store.login(username: username, password: password, rememberMe: remember)
        }
    }

    func register(username: String, password: String) async -> Bool {
        await authenticate { (store, remember) throws(ProfileError) in
            try store.register(username: username, password: password, rememberMe: remember)
        }
    }

    /// Deletes a profile after checking its password; ends the session if it is the one logged in.
    func deleteProfile(username: String, password: String) async -> Bool {
        await authenticate { (store, _) throws(ProfileError) in try store.deleteProfile(username: username, password: password) }
    }

    /// PBKDF2 runs in these (tens of milliseconds), so they run on a store queue while the forms show a spinner.
    private func authenticate(_ body: @escaping @Sendable (ProfileStore, Bool) throws(ProfileError) -> Void) async -> Bool {
        guard !isAuthenticating else { return false }
        isAuthenticating = true
        authFailure = nil
        let remember = rememberMe
        let result = await runStore { (store) throws(ProfileError) in try body(store, remember) }
        isAuthenticating = false
        if case .failure(let error) = result {
            authFailure = error
            return false
        }
        requestedSignIn = false
        if case .customList = filter { filter = .all }
        await refresh()
        return true
    }

    func clearAuthFailure() {
        authFailure = nil
    }

    /// Locks the profile and forgets the remembered login. The profile closes even if the file cannot be written.
    func logout() {
        requestedSignIn = false
        authFailure = nil
        if case .customList = filter { filter = .all }
        // Progress still held in memory goes to disk first: the session it belongs to ends here.
        perform { (store) throws(ProfileError) in
            _ = try? store.flushPlaybackProgress()
            try store.logout()
        }
    }

    // MARK: Lists

    /// Creates a list and shows it. A blank or taken name fails with an error under the field, not silently.
    func createList(named name: String) async -> Bool {
        listFailure = nil
        let result = await runStore { (store) throws(ProfileError) in try store.createList(named: name) }
        switch result {
        case .failure(let error):
            listFailure = error
            return false
        case .success(let list):
            await refresh()
            if let list { filter = .customList(list.id) }
            return list != nil
        }
    }

    func renameList(id: String, to name: String) async -> Bool {
        listFailure = nil
        let result = await runStore { (store) throws(ProfileError) in try store.renameList(id: id, to: name) }
        if case .failure(let error) = result {
            listFailure = error
            return false
        }
        await refresh()
        return true
    }

    func deleteList(id: String) {
        if case .customList(let current) = filter, current == id { filter = .all }
        perform { (store) throws(ProfileError) in _ = try store.deleteList(id: id) }
    }

    func clearListFailure() {
        listFailure = nil
    }

    func clearHistory() {
        perform { (store) throws(ProfileError) in _ = try store.clearWatchHistory() }
    }

    // MARK: Engagement

    /// The same key `ProfileStore` compares paths by.
    static func pathKey(_ url: URL) -> String { VideoFingerprint.normalizedPath(url).lowercased() }

    func isLiked(_ url: URL) -> Bool {
        let key = Self.pathKey(url)
        if let card = cards.first(where: { $0.path.lowercased() == key }) { return card.isLiked }
        return engagement.url.map(Self.pathKey) == key && engagement.isLiked
    }

    func isInList(_ id: String, _ url: URL) -> Bool {
        let key = Self.pathKey(url)
        if let card = cards.first(where: { $0.path.lowercased() == key }) { return card.lists.contains { $0.id == id } }
        return engagement.url.map(Self.pathKey) == key && engagement.listIDs.contains(id)
    }

    func toggleLike(_ url: URL) {
        setLiked(!isLiked(url), for: url)
    }

    /// Sets the favorite flag. The card and the player bar change at once; the profile write follows.
    func setLiked(_ liked: Bool, for url: URL) {
        let key = Self.pathKey(url)
        patchCards(key) { $0.isLiked = liked }
        if engagement.url.map(Self.pathKey) == key { engagement.setLiked(liked) }
        perform { (store) throws(ProfileError) in _ = try store.setLiked(liked, for: url) }
    }

    /// Sets the stars (0 clears them).
    func setRating(_ stars: Int, for url: URL) {
        let stars = min(max(stars, 0), 5)
        let key = Self.pathKey(url)
        patchCards(key) { $0.rating = stars }
        if engagement.url.map(Self.pathKey) == key { engagement.setRating(stars) }
        perform { (store) throws(ProfileError) in _ = try store.setRating(stars, for: url) }
    }

    func toggleList(_ id: String, for url: URL) {
        perform { (store) throws(ProfileError) in _ = try store.toggleVideo(url, inList: id) }
    }

    /// "+ Yeni liste oluştur ve ekle": a list with a name that is free ("Daha Sonra İzle" for the first one, then
    /// "Listem 2", "Listem 3", ... skipping the ones that exist), with the video in it.
    func createListAndAdd(_ url: URL) {
        let preferred = lists.isEmpty ? L("Daha Sonra İzle") : nil
        let base = L("Listem")
        let existing = lists.map(\.name)
        enqueue { [self] in
            let result = await runStore { (store) throws(ProfileError) -> Void in
                var names = Self.candidateNames(preferred: preferred, base: base, existing: existing)
                while let name = names.next() {
                    do throws(ProfileError) {
                        guard let list = try store.createList(named: name) else { return }
                        _ = try store.addVideo(url, toList: list.id)
                        return
                    } catch {
                        // Taken since the lists were read (another window): the next name.
                        if error != .duplicateListName { throw error }
                    }
                }
            }
            if case .failure(let error) = result { actionFailure = error }
            scheduleRefresh()
        }
    }

    /// The names to try for a quick-created list, in order: the preferred one, then "Listem N" for N from the number
    /// of lists plus one, skipping names already taken. Ignores case and diacritics, like the store.
    nonisolated static func candidateNames(preferred: String?, base: String, existing: [String]) -> NameSequence {
        NameSequence(preferred: preferred, base: base, existing: existing)
    }

    struct NameSequence: Sendable {
        private var queue: [String] = []
        private var counter: Int
        private let base: String
        private let taken: Set<String>

        init(preferred: String?, base: String, existing: [String]) {
            self.base = base
            taken = Set(existing.map(Self.fold))
            counter = existing.count + 1
            if let preferred, !taken.contains(Self.fold(preferred)) { queue = [preferred] }
        }

        mutating func next() -> String? {
            if !queue.isEmpty { return queue.removeFirst() }
            for _ in 0..<500 {
                let name = "\(base) \(counter)"
                counter += 1
                if !taken.contains(Self.fold(name)) { return name }
            }
            return nil
        }

        private static func fold(_ text: String) -> String {
            text.trimmingCharacters(in: .whitespacesAndNewlines)
                .folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)
        }
    }

    private func patchCards(_ key: String, _ change: (inout VideoCard) -> Void) {
        guard let position = cards.firstIndex(where: { $0.path.lowercased() == key }) else { return }
        var card = cards[position]
        change(&card)
        cards[position] = card
    }

    // MARK: Watching

    /// The video in the player changed. `countsAsOpen` is a video the user opened (the picker, a drop, the sidebar, a
    /// card): it counts as a view when a profile is logged in. A video a playlist run opens is not passed at all (`nil`),
    /// and neither is the one the editor gets back when a run ends: only `countsAsOpen` records a watch.
    func playerVideoChanged(to url: URL?, countsAsOpen: Bool) {
        let same = url.map(Self.pathKey) == playerVideo.map(Self.pathKey)
        guard !same else { return }
        playerVideo = url
        engagement.reset(url: url)
        guard let url else { return }
        guard countsAsOpen, isLoggedIn else {
            scheduleRefresh()
            return
        }
        enqueue { [self] in
            if case .failure(let error) = await runStore({ (store) throws(ProfileError) in _ = try store.recordWatch(url) }) {
                actionFailure = error
            }
            scheduleRefresh()
        }
    }

    /// The player's report. Playing starts the 5 s progress clock; a pause, a jump or leaving the video reports where it
    /// was, and what is reported is flushed to disk when playback stops.
    func handle(_ event: PlaybackEvent) {
        switch event {
        case .started:
            startProgressClock()
        case .paused(let position), .leaving(let position):
            stopProgressClock()
            report(position, flush: true)
        case .seeked(let position):
            report(position, flush: false)
            scheduleSeekFlush()
        }
    }

    private func startProgressClock() {
        progressTask?.cancel()
        let interval = services.progressInterval
        progressTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: interval)
                guard !Task.isCancelled, let self else { return }
                if let position = playbackSampler?() { report(position, flush: false) }
            }
        }
    }

    private func stopProgressClock() {
        progressTask?.cancel()
        progressTask = nil
    }

    /// A jump settles half a second after the last one (a held arrow key makes many), then the position is written out.
    private func scheduleSeekFlush() {
        seekFlushTask?.cancel()
        seekFlushTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(500))
            guard !Task.isCancelled, let self else { return }
            flushProgress()
        }
    }

    /// Stores where playback is, without counting a view, and with `flush` writes it to disk at once (the store keeps
    /// progress in memory and writes it at most every 15 s otherwise). Background bookkeeping: a failure is not shown,
    /// the next report tries again. Never raises a refresh.
    private func report(_ position: PlaybackPosition, flush: Bool) {
        guard isLoggedIn, let url = playerVideo, position.seconds.isFinite, position.duration.isFinite,
            position.duration > 0
        else { return }
        enqueue { [self] in
            _ = await runStore { (store) throws(ProfileError) in
                _ = try store.updatePlaybackProgress(url, position: position.seconds, duration: position.duration)
                if flush { _ = try store.flushPlaybackProgress() }
            }
        }
    }

    /// Writes the playback progress the store still holds in memory to disk.
    private func flushProgress() {
        guard isLoggedIn else { return }
        enqueue { [self] in
            _ = await runStore { (store) throws(ProfileError) in _ = try store.flushPlaybackProgress() }
        }
    }

    /// The app is quitting: report where the video is and make sure it reaches the disk before the process ends. Runs on
    /// the main actor and waits, which is fine at this point.
    func prepareForQuit() {
        stopProgressClock()
        seekFlushTask?.cancel()
        if let url = playerVideo, let position = playbackSampler?(), position.duration > 0, position.duration.isFinite,
            position.seconds.isFinite
        {
            _ = try? store.updatePlaybackProgress(url, position: position.seconds, duration: position.duration)
        }
        _ = try? store.flushPlaybackProgress()
    }
}
