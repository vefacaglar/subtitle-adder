import EngineKit
import Foundation
import LibraryKit
import Observation
import Synchronization

/// The open video's views, favorite flag, stars and lists, for the bar on the player. Kept apart from the feed, so a
/// change of one never redraws the other, and playback ticks touch neither.
@MainActor
@Observable
final class PlayerEngagement {
    /// The video in the player.
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
/// `ProfileStore.changes()` (another window or instance, a login restored from the secret store) and changes in the media
/// index. Playback progress and thumbnails change nothing the lists show, and raise no refresh.
@MainActor
@Observable
final class VideoPlatformModel {
    // MARK: Profile

    private(set) var username: String?
    private(set) var registeredUsernames: [String] = []
    /// The user who last logged in, to pre-fill the login box.
    private(set) var lastUsername: String?
    @ObservationIgnored private let allowsRememberedLogin: Bool
    /// Whether the next login or registration should be remembered when the store supports it.
    var rememberMe: Bool
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
            dropStaleCards()
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
    /// How many videos of each list (by list id) are visible; the videos on an unplugged disk or deleted from their
    /// folder are not counted.
    private(set) var listCounts: [String: Int] = [:]
    /// Why the last list creation or rename failed; cleared by the next attempt.
    private(set) var listFailure: ProfileError?
    /// Where `listFailure` belongs, so it is shown in that place only.
    private(set) var listFailureContext: ListFailureContext?

    enum ListFailureContext: Equatable {
        case create
        case rename(String)
    }
    /// "Giriş yap / Profil oluştur" was pressed while on "Keşfet": show the sign-in card there.
    private(set) var requestedSignIn = false
    /// The hub is on screen. Off screen, a refresh keeps the header and the player bar current and skips the cards.
    var isHubActive = false {
        didSet {
            if isHubActive, !oldValue {
                dropStaleCards()
                scheduleRefresh()
            }
        }
    }

    let engagement = PlayerEngagement()

    var listError: String? { listFailure?.message }
    /// The failure of a list creation, for the place where the name is typed.
    var createListError: String? { listFailureContext == .create ? listFailure?.message : nil }

    /// The failure of renaming this list, for its rename form.
    func renameListError(for id: String) -> String? { listFailureContext == .rename(id) ? listFailure?.message : nil }

    /// The number the list pill and the list menu show.
    func visibleCount(of list: UserVideoList) -> Int { listCounts[list.id] ?? list.videoPaths.count }

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

    /// The video in the player, as far as the platform knows.
    @ObservationIgnored private(set) var playerVideo: URL?
    /// Where the player is now; set by the window, read by the progress timer and when the app quits.
    @ObservationIgnored var playbackSampler: (@MainActor () -> PlaybackPosition?)?
    /// The window whose player the platform follows (the last one that appeared or became key).
    @ObservationIgnored private weak var playbackOwner: PlayerWindowToken?
    /// A video opened before `start()` has restored the remembered login; counted once it is known whether there is one.
    @ObservationIgnored private var pendingOpen: URL?
    @ObservationIgnored private var hasRestoredLogin = false
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
    @ObservationIgnored private var appliedSessionGeneration: Int?
    /// Counts the requests for a snapshot; a build that sees it move on stops, its result is not wanted.
    @ObservationIgnored private let refreshRequests = RefreshCounter()
    /// Edits made on screen whose profile write is not over: they stay on top of snapshots meanwhile.
    @ObservationIgnored private var pendingEdits: [String: PendingEdit] = [:]
    @ObservationIgnored private var isCreatingList = false
    /// Folders listed for subtitle badges by the last feed build (tests).
    @ObservationIgnored private(set) var lastBadgeListings = 0

    init(
        store: ProfileStore, index: MediaLibraryIndex, services: PlatformServices = .system,
        allowsRememberedLogin: Bool = true,
        thumbnailGenerator: @escaping ThumbnailService.Generator = ThumbnailGenerator.jpegData(for:)
    ) {
        self.store = store
        self.allowsRememberedLogin = allowsRememberedLogin
        rememberMe = allowsRememberedLogin
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
                for await change in storeChanges {
                    guard let self else { return }
                    // What this model does itself ends with a refresh of its own; only another window, process or
                    // a restored login needs one here.
                    if change == .local { continue }
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
        // Read the profile file on the store queue before building the first snapshot.
        let store = store
        await queues.actions.run { store.refresh() }
        hasRestoredLogin = true
        await refresh()
        countPendingOpen()
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
        refreshRequests.bump()
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

    private var currentFeedKey: FeedKey { FeedKey(filter: filter, search: searchQuery, loggedIn: isLoggedIn) }

    /// Cards built for another tab or query must not stay on screen under the new one while its build is under way
    /// (it can be slow on a cold library): they go, and the hub shows its spinner until the new cards arrive.
    private func dropStaleCards() {
        if !cards.isEmpty, appliedFeedKey != currentFeedKey { cards = [] }
    }

    private func runRefreshes() async {
        while refreshPending {
            refreshPending = false
            let wantsFeed = isHubActive
            let search = searchQuery
            let key = currentFeedKey
            // A request that comes in while this is being built makes the build stop and its result fall away.
            let ticket = refreshRequests.current
            let isCurrent: @Sendable () -> Bool = { [refreshRequests] in refreshRequests.current == ticket }
            let discovered = await discoveredPaths(wantsFeed: wantsFeed, search: search, loggedIn: key.loggedIn)
            var request = FeedRequest(
                filter: filter, search: search, discoveredPaths: discovered, wantsFeed: wantsFeed, playerVideo: playerVideo)
            let badges = SubtitleBadges(listDirectory: services.listDirectory)

            // Following moved files means reading file slices the first time, which can take a while on a cold library.
            // A tab or query that has nothing on screen yet gets the quick build first (no file is opened), then the one
            // that follows the moves. Logged out nothing follows moves, so one quick build is all there is.
            if wantsFeed, !discovered.isEmpty, key.loggedIn, cards.isEmpty || appliedFeedKey != key {
                request.fingerprints = .pathOnly
                let quick = await build(request, badges: badges, isCurrent: isCurrent)
                if refreshPending { continue }
                guard apply(quick, wantsFeed: wantsFeed, key: key) else { continue }
                request.fingerprints = .compute
            }
            let snapshot = await build(request, badges: badges, isCurrent: isCurrent)
            // Something asked again while this was being built: this one is already out of date.
            if refreshPending { continue }
            apply(snapshot, wantsFeed: wantsFeed, key: key)
        }
        isRefreshing = false
        refreshTask = nil
    }

    private func build(_ request: FeedRequest, badges: SubtitleBadges, isCurrent: @escaping @Sendable () -> Bool) async
        -> PlatformSnapshot
    {
        let store = store, services = services, thumbnails = thumbnails
        return await queues.feed.run {
            PlatformFeedBuilder.snapshot(
                store: store, request: request, services: services, badges: badges,
                seedThumbnail: { path, data in thumbnails.seed(data, for: path) }, shouldContinue: isCurrent)
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

    /// Puts a snapshot on screen. Returns `false` when it was dropped: it was read under a login that is over by now,
    /// and another one is asked for.
    @discardableResult
    private func apply(_ built: PlatformSnapshot, wantsFeed: Bool, key: FeedKey) -> Bool {
        guard built.sessionGeneration == store.sessionGeneration else {
            refreshPending = true
            return false
        }
        let snapshot = withPendingEdits(built)
        if appliedSessionGeneration != snapshot.sessionGeneration {
            // Another profile's pictures must not show for this one, and a closed profile's must not stay in memory.
            if appliedSessionGeneration != nil {
                thumbnails.reset()
                CardImageCache.removeAll()
            }
            appliedSessionGeneration = snapshot.sessionGeneration
        }
        if username != snapshot.username { username = snapshot.username }
        if registeredUsernames != snapshot.registeredUsernames { registeredUsernames = snapshot.registeredUsernames }
        if lastUsername != snapshot.lastUsername { lastUsername = snapshot.lastUsername }
        if !hasAppliedFirstSnapshot {
            hasAppliedFirstSnapshot = true
            rememberMe = allowsRememberedLogin && snapshot.rememberMe
        }
        if counts != snapshot.counts { counts = snapshot.counts }
        if lists != snapshot.lists { lists = snapshot.lists }
        if listCounts != snapshot.listCounts { listCounts = snapshot.listCounts }
        engagement.apply(snapshot.engagement)
        // Off screen no cards were built; what is there stays until the hub opens and a refresh replaces it.
        if wantsFeed {
            appliedFeedKey = FeedKey(filter: key.filter, search: key.search, loggedIn: snapshot.username != nil)
            let cards = snapshot.cards ?? []
            if self.cards != cards { self.cards = cards }
            if snapshot.cards != nil, !hasLoadedFeed { hasLoadedFeed = true }
        }
        lastBadgeListings = snapshot.listings
        // The list on show is gone (deleted elsewhere, or another profile logged in): back to "Keşfet".
        if case .customList(let id) = filter, !snapshot.lists.contains(where: { $0.id == id }) {
            filter = .all
        }
        // The request assumed a login state the store did not have (a login restored, or ended elsewhere): ask again.
        if (snapshot.username != nil) != key.loggedIn { refreshPending = true }
        return true
    }

    /// Edits still being written, laid over a snapshot that was read before they were.
    private func withPendingEdits(_ snapshot: PlatformSnapshot) -> PlatformSnapshot {
        guard !pendingEdits.isEmpty else { return snapshot }
        var patched = snapshot
        if var cards = patched.cards {
            for position in cards.indices {
                guard let edit = pendingEdits[cards[position].path.lowercased()] else { continue }
                if let liked = edit.liked { cards[position].isLiked = liked }
                if let rating = edit.rating { cards[position].rating = rating }
            }
            patched.cards = cards
        }
        if var open = patched.engagement, let edit = pendingEdits[Self.pathKey(open.url)] {
            if let liked = edit.liked { open.isLiked = liked }
            if let rating = edit.rating { open.rating = rating }
            patched.engagement = open
        }
        return patched
    }

    // MARK: Search

    /// Applies the text in the field (Enter, or the "Ara" button).
    func submitSearch() {
        requestedSignIn = false
        searchQuery = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        dropStaleCards()
        scheduleRefresh()
    }

    func clearSearch() {
        searchQuery = ""
        if !searchText.isEmpty { searchText = "" }
        dropStaleCards()
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
        let remember = allowsRememberedLogin && rememberMe
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

    /// Creates a list and shows it. A blank or taken name fails with an error under the field, not silently. While one
    /// creation is under way another is refused (a double press must not make two lists).
    func createList(named name: String) async -> Bool {
        guard !isCreatingList else { return false }
        isCreatingList = true
        defer { isCreatingList = false }
        listFailure = nil
        listFailureContext = nil
        let result = await runStore { (store) throws(ProfileError) in try store.createList(named: name) }
        switch result {
        case .failure(let error):
            fail(error, in: .create)
            return false
        case .success(let list):
            await refresh()
            if let list { filter = .customList(list.id) }
            return list != nil
        }
    }

    /// Renames a list. A name that is blank or taken fails with the reason, and so does a list that is not there any
    /// more (deleted in another window): the lists are brought up to date then.
    func renameList(id: String, to name: String) async -> Bool {
        listFailure = nil
        listFailureContext = nil
        let result = await runStore { (store) throws(ProfileError) in try store.renameList(id: id, to: name) }
        switch result {
        case .failure(let error):
            fail(error, in: .rename(id))
            return false
        case .success(let renamed):
            await refresh()
            if !renamed { fail(.listNotFound, in: .rename(id)) }
            return renamed
        }
    }

    /// Deletes a list. The hub goes back to "Keşfet" only once the list is really gone; a failure leaves it on show.
    func deleteList(id: String) {
        enqueue { [self] in
            let result = await runStore { (store) throws(ProfileError) in _ = try store.deleteList(id: id) }
            switch result {
            case .failure(let error):
                actionFailure = error
            case .success:
                if case .customList(let current) = filter, current == id { filter = .all }
            }
            scheduleRefresh()
        }
    }

    func clearListFailure() {
        listFailure = nil
        listFailureContext = nil
    }

    private func fail(_ error: ProfileError, in context: ListFailureContext) {
        listFailure = error
        listFailureContext = context
    }

    func clearHistory() {
        perform { (store) throws(ProfileError) in _ = try store.clearWatchHistory() }
    }

    // MARK: Engagement

    /// The same key `ProfileStore` compares paths by.
    static func pathKey(_ url: URL) -> String { VideoFingerprint.normalizedPath(url).lowercased() }

    /// The video in the player, as the player bar shows it. The bar's numbers are refreshed even while the hub is off
    /// screen, so they are the truth for this video; the cards are not (they are built only while the hub is open).
    private func isOpenVideo(_ key: String) -> Bool {
        engagement.isLoaded && engagement.url.map(Self.pathKey) == key
    }

    func isLiked(_ url: URL) -> Bool {
        let key = Self.pathKey(url)
        if isOpenVideo(key) { return engagement.isLiked }
        return cards.first(where: { $0.path.lowercased() == key })?.isLiked ?? false
    }

    func isInList(_ id: String, _ url: URL) -> Bool {
        let key = Self.pathKey(url)
        if isOpenVideo(key) { return engagement.listIDs.contains(id) }
        return cards.first(where: { $0.path.lowercased() == key })?.lists.contains { $0.id == id } ?? false
    }

    func toggleLike(_ url: URL) {
        setLiked(!isLiked(url), for: url)
    }

    /// Sets the favorite flag. The card and the player bar change at once; the profile write follows.
    func setLiked(_ liked: Bool, for url: URL) {
        let key = Self.pathKey(url)
        patchCards(key) { $0.isLiked = liked }
        if engagement.url.map(Self.pathKey) == key { engagement.setLiked(liked) }
        pendingEdits[key, default: PendingEdit()].liked = liked
        performEdit(key) { (store) throws(ProfileError) in _ = try store.setLiked(liked, for: url) }
    }

    /// Sets the stars (0 clears them).
    func setRating(_ stars: Int, for url: URL) {
        let stars = min(max(stars, 0), 5)
        let key = Self.pathKey(url)
        patchCards(key) { $0.rating = stars }
        if engagement.url.map(Self.pathKey) == key { engagement.setRating(stars) }
        pendingEdits[key, default: PendingEdit()].rating = stars
        performEdit(key) { (store) throws(ProfileError) in _ = try store.setRating(stars, for: url) }
    }

    /// What was changed on screen and not yet written; until the last write of a video is over, snapshots read before it
    /// show this on top, so a quick second tap never flickers back to the first value.
    private struct PendingEdit {
        var liked: Bool?
        var rating: Int?
        var outstanding = 0
    }

    /// Runs the write of an on-screen edit as an action, and lets the edit go once its last write is over.
    private func performEdit(_ key: String, _ body: @escaping @Sendable (ProfileStore) throws(ProfileError) -> Void) {
        pendingEdits[key, default: PendingEdit()].outstanding += 1
        enqueue { [self] in
            if case .failure(let error) = await runStore(body) { actionFailure = error }
            if var edit = pendingEdits[key] {
                edit.outstanding -= 1
                pendingEdits[key] = edit.outstanding > 0 ? edit : nil
            }
            scheduleRefresh()
        }
    }

    /// Puts the video in a list or takes it out. The caller says what it wants, not "the other one", so a menu that
    /// shows an out-of-date checkmark cannot undo what the user meant.
    func setInList(_ isIn: Bool, id: String, for url: URL) {
        perform { (store) throws(ProfileError) in
            if isIn {
                _ = try store.addVideo(url, toList: id)
            } else {
                _ = try store.removeVideo(url, fromList: id)
            }
        }
    }

    func toggleList(_ id: String, for url: URL) {
        setInList(!isInList(id, url), id: id, for: url)
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
    /// card): it counts as a view when a profile is logged in. Only `countsAsOpen` records a watch. Jobs and playlist runs
    /// work in sessions of their own and never change the video in the player.
    func playerVideoChanged(to url: URL?, countsAsOpen: Bool) {
        let same = url.map(Self.pathKey) == playerVideo.map(Self.pathKey)
        guard !same else { return }
        playerVideo = url
        pendingOpen = nil
        engagement.reset(url: url)
        guard let url else { return }
        guard countsAsOpen else {
            scheduleRefresh()
            return
        }
        guard hasRestoredLogin else {
            // Whether a login is restored is not known yet (`start()` is still asking): decide when it is.
            pendingOpen = url
            scheduleRefresh()
            return
        }
        guard isLoggedIn else {
            scheduleRefresh()
            return
        }
        countOpen(url)
    }

    /// The login is restored (or there is none): a video opened before that counts if a profile is logged in now and
    /// the video is still the one open.
    private func countPendingOpen() {
        guard let url = pendingOpen else { return }
        pendingOpen = nil
        guard isLoggedIn, playerVideo.map(Self.pathKey) == Self.pathKey(url) else { return }
        countOpen(url)
    }

    /// Records a view of the video the user opened. A failure here is background bookkeeping like the progress: it is
    /// not worth an alert on every open (a read-only profile file would raise one each time).
    private func countOpen(_ url: URL) {
        enqueue { [self] in
            _ = await runStore { (store) throws(ProfileError) in _ = try store.recordWatch(url) }
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
    /// the main actor and waits, which is fine at this point, but not for long: the write can wait for the profile
    /// file's lock (another process holding it) or a slow disk, so it is given `timeout` and then the app quits anyway.
    /// Returns whether the write finished in time.
    @discardableResult
    func prepareForQuit(timeout: Duration = .seconds(2)) -> Bool {
        stopProgressClock()
        seekFlushTask?.cancel()
        var final: (url: URL, position: PlaybackPosition)?
        if let url = playerVideo, let position = playbackSampler?(), position.duration > 0, position.duration.isFinite,
            position.seconds.isFinite
        {
            final = (url, position)
        }
        let store = store
        let finished = DispatchSemaphore(value: 0)
        let work = final
        DispatchQueue.global(qos: .userInitiated).async {
            if let work {
                _ = try? store.updatePlaybackProgress(work.url, position: work.position.seconds, duration: work.position.duration)
            }
            _ = try? store.flushPlaybackProgress()
            finished.signal()
        }
        let seconds = Double(timeout.components.seconds) + Double(timeout.components.attoseconds) / 1e18
        return finished.wait(timeout: .now() + seconds) == .success
    }

    // MARK: Windows

    /// A window's player starts reporting to the platform: where it is (for the progress clock and for quitting) and
    /// when it plays, stops or jumps. The window that attached last, or became key last, is the one followed; the
    /// platform holds the player only weakly through the token, so a closed window's player is not kept alive.
    func attachPlayer(_ player: PlayerModel, owner: PlayerWindowToken) {
        owner.player = player
        claimPlayback(owner)
        player.onPlaybackEvent = { [weak self, weak owner] event in
            guard let self, let owner else { return }
            handle(event, from: owner)
        }
    }

    /// The window's player stops reporting and goes quiet: it is paused, where it was is reported and flushed (so the
    /// resume point is on disk), and the platform lets go of it unless another window has taken over meanwhile.
    func detachPlayer(_ player: PlayerModel, owner: PlayerWindowToken) {
        player.pause()
        handle(.leaving(player.position), from: owner)
        player.onPlaybackEvent = nil
        releasePlayback(owner)
        owner.player = nil
    }

    /// Makes this window the one whose player the platform follows (it became the key window).
    func claimPlayback(_ owner: PlayerWindowToken) {
        playbackOwner = owner
        playbackSampler = { [weak owner] in owner?.player?.position }
    }

    /// Lets go of the sampler, but only if it still belongs to this window.
    func releasePlayback(_ owner: PlayerWindowToken) {
        guard playbackOwner === owner else { return }
        playbackOwner = nil
        playbackSampler = nil
    }

    /// An event of a window's player. Only the window that is followed is heard; another window's player does not
    /// move the progress of the video the platform follows.
    func handle(_ event: PlaybackEvent, from owner: PlayerWindowToken) {
        guard playbackOwner === owner else { return }
        handle(event)
    }
}

/// Identifies one window's player to the platform. Owned by the window; the platform keeps it weakly, and it keeps
/// the player weakly, so nothing here outlives the window.
@MainActor
final class PlayerWindowToken {
    weak var player: PlayerModel?
}

/// Counts requests for a new snapshot, readable from the queue that builds it.
private final class RefreshCounter: Sendable {
    private let value = Atomic<Int>(0)

    var current: Int { value.load(ordering: .acquiring) }

    func bump() {
        value.add(1, ordering: .acquiringAndReleasing)
    }
}
