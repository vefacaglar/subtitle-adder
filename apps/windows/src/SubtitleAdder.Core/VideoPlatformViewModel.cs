using System.Collections.Concurrent;
using System.Globalization;
using CommunityToolkit.Mvvm.ComponentModel;
using SubtitleAdder.EngineKit;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

/// <summary>
/// Presentation model for a single video card inside the YouTube-style local video platform feed.
/// </summary>
public sealed class VideoCardViewModel
{
    private static readonly ConcurrentDictionary<string, string?> SidecarCache =
        new(StringComparer.OrdinalIgnoreCase);

    public static void InvalidateSidecarCache(string? videoPath = null)
    {
        if (string.IsNullOrWhiteSpace(videoPath))
        {
            SidecarCache.Clear();
            return;
        }
        SidecarCache.TryRemove(videoPath, out _);
    }

    public VideoCardViewModel(VideoUserStats stats, IReadOnlyList<string>? customListNames = null)
    {
        Stats = stats;
        VideoPath = stats.VideoPath;
        FileName = stats.FileName;
        var stem = Path.GetFileNameWithoutExtension(stats.FileName);
        Title = string.IsNullOrWhiteSpace(stem) ? stats.FileName : stem;
        Extension = Path.GetExtension(stats.FileName).TrimStart('.').ToUpperInvariant();
        DirectoryPath = stats.DirectoryPath;
        FolderDisplay = FormatFolderDisplay(stats.DirectoryPath);
        ViewCount = stats.ViewCount;
        IsLiked = stats.IsLiked;
        Rating = Math.Clamp(stats.Rating, 0, 5);
        LastPositionSeconds = stats.LastPositionSeconds;
        DurationSeconds = stats.DurationSeconds;
        ProgressRatio = stats.ProgressRatio;
        LastWatchedAtUtc = stats.LastWatchedAtUtc;
        UpdatedAtUtc = stats.UpdatedAtUtc;
        SidecarSubtitlePath = FindVideoSubtitleFile(stats.VideoPath);
        HasSubtitle = SidecarSubtitlePath is not null;
        HasSidecarSubtitle = HasSubtitle;
        SubtitleBadgeText = SidecarSubtitlePath is { } subPath
            ? Path.GetExtension(subPath).TrimStart('.').ToUpperInvariant()
            : null;
        CustomListNames = customListNames ?? [];
        ThumbnailBytes = stats.ThumbnailBytes;
    }

    public VideoUserStats Stats { get; }
    public string VideoPath { get; }
    public byte[]? ThumbnailBytes { get; }
    public string FileName { get; }
    public string Title { get; }
    public string Extension { get; }
    public string DirectoryPath { get; }
    public string FolderDisplay { get; }
    public int ViewCount { get; }
    public bool IsLiked { get; }
    public int Rating { get; }
    public double LastPositionSeconds { get; }
    public double DurationSeconds { get; }
    public double ProgressRatio { get; }
    public DateTimeOffset? LastWatchedAtUtc { get; }
    public DateTimeOffset UpdatedAtUtc { get; }
    public string? SidecarSubtitlePath { get; }
    public bool HasSubtitle { get; }
    public bool HasSidecarSubtitle { get; }
    public string? SubtitleBadgeText { get; }
    public IReadOnlyList<string> CustomListNames { get; }

    public bool HasCustomLists => CustomListNames.Count > 0;
    public bool HasProgress => ProgressRatio > 0.01;

    public string SubtitleStatusText => HasSubtitle
        ? (string.IsNullOrEmpty(SubtitleBadgeText) ? L("Altyazılı") : $"{L("Altyazılı")} ({SubtitleBadgeText})")
        : L("Altyazı yok");

    public string ViewsText => ViewCount > 0
        ? L("%lld görüntüleme", (long)ViewCount)
        : L("Henüz izlenmedi");

    public string LikeButtonText => IsLiked ? L("Favorilerde") : L("Favorilere ekle");

    public string RatingBadgeText => Rating > 0 ? $"★ {Rating}/5" : "";

    public string DurationText => DurationSeconds > 0
        ? PlaybackTimeline.FormatTime(DurationSeconds, DurationSeconds >= 3600)
        : "";

    public string LastWatchedText => LastWatchedAtUtc is { } watched
        ? L("Son izleme: %@", watched.ToLocalTime().ToString("g", CultureInfo.CurrentUICulture))
        : L("Henüz izlenmedi");

    private static string? FindVideoSubtitleFile(string? videoPath)
    {
        if (string.IsNullOrWhiteSpace(videoPath)) return null;
        if (SidecarCache.TryGetValue(videoPath, out var cached))
        {
            return cached;
        }

        var found = FindVideoSubtitleFileUncached(videoPath);
        SidecarCache[videoPath] = found;
        return found;
    }

    private static string? FindVideoSubtitleFileUncached(string videoPath)
    {
        if (SubtitleParser.FindSidecarSubtitle(videoPath) is { } exact)
            return exact;

        try
        {
            var dir = Path.GetDirectoryName(videoPath);
            var stem = Path.GetFileNameWithoutExtension(videoPath);
            if (string.IsNullOrEmpty(dir) || string.IsNullOrEmpty(stem) || !Directory.Exists(dir))
                return null;

            foreach (var ext in SubtitleParser.Extensions)
            {
                var matches = Directory.GetFiles(dir, $"{stem}.*{ext}");
                if (matches.Length > 0) return matches[0];
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException)
        {
        }
        return null;
    }

    private static string FormatFolderDisplay(string directoryPath)
    {
        if (string.IsNullOrWhiteSpace(directoryPath)) return "";
        var trimmed = directoryPath.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        var folderName = Path.GetFileName(trimmed);
        return string.IsNullOrWhiteSpace(folderName) ? trimmed : $"{folderName} ({trimmed})";
    }
}

/// <summary>
/// View model connecting <see cref="LocalProfileManager"/> to the toolbar profile status button,
/// player engagement bar, and YouTube-style local video platform hub.
/// </summary>
public sealed partial class VideoPlatformViewModel : ObservableObject, IDisposable
{
    public static VideoPlatformViewModel Shared { get; } = new();

    private readonly LocalProfileManager _profileManager;
    private readonly LanguageState _languageState;
    private readonly IUiDispatcher? _ui;
    private readonly List<string> _discoveredPaths = [];

    private string? _authError;
    private VideoFeedFilter _activeFilter = VideoFeedFilter.All;
    private string? _activeCustomListId;
    private string _searchQuery = "";
    private IReadOnlyList<VideoCardViewModel> _feedItems = [];
    private IReadOnlyList<UserVideoList> _customLists = [];
    private string? _activeVideoPath;
    // Session the active video was opened under; the video is forgotten as soon as the session changes, so progress
    // ticks of a video opened by one profile (or while logged out) never land in another profile's history.
    private long _activeVideoSessionToken;
    // False for a video that was already open when the session began (adopted at login): likes, ratings and lists
    // work for it, but its playback is not recorded as this profile's watch or resume position.
    private bool _activeVideoRecordsProgress;
    private int _activeVideoViewCount;
    private bool _activeVideoIsLiked;
    private int _activeVideoRating;
    private bool _rememberMe;

    public VideoPlatformViewModel(LocalProfileManager? profileManager = null, LanguageState? languageState = null, IUiDispatcher? ui = null)
    {
        _profileManager = profileManager ?? new LocalProfileManager();
        _languageState = languageState ?? LanguageState.Shared;
        _ui = ui;
        _rememberMe = _profileManager.RememberMe;

        _profileManager.StateChanged += OnProfileManagerStateChanged;
        _languageState.LanguageChanged += OnLanguageChanged;

        RefreshFeed();
    }

    /// <summary>Underlying profile and engagement store.</summary>
    public LocalProfileManager ProfileManager => _profileManager;

    /// <summary>Raised when the UI language changes and all localized strings are refreshed.</summary>
    public event EventHandler? LocalizationChanged;

    /// <summary>Whether a user profile is currently logged in.</summary>
    public bool IsLoggedIn => _profileManager.IsLoggedIn;

    /// <summary>
    /// Current profile session (see <see cref="LocalProfileManager.SessionToken"/>). Deferred work captures it and passes
    /// it back so it is dropped if the profile logged out or switched meanwhile.
    /// </summary>
    public long SessionToken => _profileManager.SessionToken;

    /// <summary>
    /// Returns the video currently open in the player (the window's own state, not kept here). When a profile logs
    /// in while a video is open, that video becomes the active one so the engagement bar works for it; its earlier
    /// (logged-out or other-profile) playback is still not recorded.
    /// </summary>
    public Func<string?>? OpenVideoProvider { get; set; }

    /// <summary>Currently logged-in username, or <c>null</c> when logged out.</summary>
    public string? CurrentUsername => _profileManager.CurrentUsername;

    /// <summary>Most recently used username on this machine for convenience pre-filling.</summary>
    public string? LastUsername => _profileManager.LastUsername;

    /// <summary>Whether "Girişi hatırla" (remember login across restarts and instances) is checked. Opt-in: <c>false</c> unless the user ticked it for the current remembered session.</summary>
    public bool RememberMe
    {
        get => _rememberMe;
        set => SetProperty(ref _rememberMe, value);
    }

    /// <summary>All registered profile usernames on this machine.</summary>
    public IReadOnlyList<string> RegisteredUsernames => _profileManager.RegisteredUsernames;

    /// <summary>Localized authentication error message from the last login/register attempt, if any.</summary>
    public string? AuthError
    {
        get => _authError;
        private set => SetProperty(ref _authError, value);
    }

    /// <summary>Selected feed filter tab in the video platform hub.</summary>
    public VideoFeedFilter ActiveFilter
    {
        get => _activeFilter;
        set
        {
            if (!SetProperty(ref _activeFilter, value)) return;
            OnPropertyChanged(nameof(RequiresAuthForCurrentTab));
            OnPropertyChanged(nameof(ActiveCustomListName));
            RefreshFeed();
        }
    }

    /// <summary>ID of the selected user-created list when <see cref="ActiveFilter"/> is <see cref="VideoFeedFilter.CustomList"/>.</summary>
    public string? ActiveCustomListId
    {
        get => _activeCustomListId;
        set
        {
            if (!SetProperty(ref _activeCustomListId, value)) return;
            OnPropertyChanged(nameof(ActiveCustomListName));
            RefreshFeed();
        }
    }

    /// <summary>Name of the currently active custom list, if any.</summary>
    public string? ActiveCustomListName =>
        _activeCustomListId is { } id
            ? _customLists.FirstOrDefault(l => string.Equals(l.Id, id, StringComparison.OrdinalIgnoreCase))?.Name
            : null;

    /// <summary>Custom video lists created by the currently logged-in user.</summary>
    public IReadOnlyList<UserVideoList> CustomLists
    {
        get => _customLists;
        private set => SetProperty(ref _customLists, value);
    }

    /// <summary>
    /// True whenever no profile is logged in, on every tab including "Keşfet" (<see cref="VideoFeedFilter.All"/>) and
    /// search: the SSD media index lists the files of folders browsed in earlier sessions (newest first), which would
    /// reveal what was watched, so nothing from it is shown without a profile.
    /// </summary>
    public bool RequiresAuthForCurrentTab => !IsLoggedIn;

    /// <summary>Search query filtering the video platform feed by file name or folder path.</summary>
    public string SearchQuery
    {
        get => _searchQuery;
        set
        {
            var next = value ?? "";
            if (!SetProperty(ref _searchQuery, next)) return;
            OnPropertyChanged(nameof(RequiresAuthForCurrentTab));
            RefreshFeed();
        }
    }

    /// <summary>Filtered and ordered video cards currently shown in the platform hub.</summary>
    public IReadOnlyList<VideoCardViewModel> FeedItems
    {
        get => _feedItems;
        private set => SetProperty(ref _feedItems, value);
    }

    /// <summary>Path of the currently open video in the player, or <c>null</c>.</summary>
    public string? ActiveVideoPath
    {
        get => _activeVideoPath;
        private set
        {
            if (!SetProperty(ref _activeVideoPath, value)) return;
            OnPropertyChanged(nameof(HasActiveVideo));
        }
    }

    /// <summary>True when a profile is logged in and a video is currently open in the player.</summary>
    public bool HasActiveVideo => IsLoggedIn && !string.IsNullOrWhiteSpace(_activeVideoPath);

    /// <summary>View count of the currently open video for the active profile.</summary>
    public int ActiveVideoViewCount
    {
        get => _activeVideoViewCount;
        private set
        {
            if (!SetProperty(ref _activeVideoViewCount, value)) return;
            OnPropertyChanged(nameof(ActiveVideoViewsText));
        }
    }

    /// <summary>Whether the currently open video is liked by the active profile.</summary>
    public bool ActiveVideoIsLiked
    {
        get => _activeVideoIsLiked;
        private set
        {
            if (!SetProperty(ref _activeVideoIsLiked, value)) return;
            OnPropertyChanged(nameof(ActiveVideoLikeText));
        }
    }

    /// <summary>Star rating (<c>0..5</c>) of the currently open video for the active profile.</summary>
    public int ActiveVideoRating
    {
        get => _activeVideoRating;
        private set => SetProperty(ref _activeVideoRating, value);
    }

    /// <summary>Localized view count badge text for the active video (e.g. "3 görüntüleme").</summary>
    public string ActiveVideoViewsText => L("%lld görüntüleme", (long)_activeVideoViewCount);

    /// <summary>Localized like button text for the active video ("Beğen" / "Beğenildi").</summary>
    public string ActiveVideoLikeText => _activeVideoIsLiked ? L("Beğenildi") : L("Beğen");

    /// <summary>Toolbar button label: active username when logged in, or "Profil / Giriş" when logged out.</summary>
    public string ProfileStatusText => IsLoggedIn && CurrentUsername is { } user ? user : L("Profil / Giriş");

    /// <summary>Profile summary line shown in the profile flyout ("N izlenen · M beğenilen").</summary>
    public string ProfileSummaryText => IsLoggedIn
        ? L("%lld izlenen · %lld beğenilen", (long)_profileManager.TotalWatchedVideosCount, (long)_profileManager.TotalLikedVideosCount)
        : L("İzleme geçmişi için giriş yapın");

    public int TotalWatchedVideosCount => _profileManager.TotalWatchedVideosCount;
    public int TotalLikedVideosCount => _profileManager.TotalLikedVideosCount;
    public int TotalRatedVideosCount => _profileManager.TotalRatedVideosCount;
    public int TotalViewsCount => _profileManager.TotalViewsCount;

    /// <summary>Attempts to log in with an existing local profile username and password.</summary>
    public bool TryLogin(string username, string password, bool? rememberMe = null)
    {
        var remember = rememberMe ?? _rememberMe;
        RememberMe = remember;
        if (_profileManager.Login(username, password, out var error, remember))
        {
            AuthError = null;
            SyncFromProfileManager();
            return true;
        }
        AuthError = error;
        return false;
    }

    /// <summary>Attempts to register a new local profile and log it in immediately.</summary>
    public bool TryRegister(string username, string password, bool? rememberMe = null)
    {
        var remember = rememberMe ?? _rememberMe;
        RememberMe = remember;
        if (_profileManager.Register(username, password, out var error, remember))
        {
            AuthError = null;
            SyncFromProfileManager();
            return true;
        }
        AuthError = error;
        return false;
    }

    /// <summary>
    /// Stores an encrypted thumbnail (<c>byte[]</c>) for <paramref name="videoPath"/> inside the active user's AES-256-GCM vault.
    /// With <paramref name="sessionToken"/>, nothing is saved unless that session is still the active one.
    /// </summary>
    public bool SaveEncryptedThumbnail(string videoPath, byte[] thumbnailBytes, long? sessionToken = null)
    {
        if (!IsLoggedIn || string.IsNullOrWhiteSpace(videoPath) || thumbnailBytes is not { Length: > 0 })
            return false;
        return _profileManager.SetThumbnail(videoPath, thumbnailBytes, sessionToken);
    }

    /// <summary>Retrieves the decrypted thumbnail (<c>byte[]</c>) for <paramref name="videoPath"/> from the active user's vault.</summary>
    public byte[]? GetEncryptedThumbnail(string? videoPath) =>
        IsLoggedIn ? _profileManager.GetThumbnail(videoPath) : null;

    /// <summary>Logs out of the active profile and clears personal engagement state from the UI.</summary>
    public void Logout()
    {
        _profileManager.Logout();
        ClearActiveVideo();
        // The remembered session is gone; the next login must be ticked again by the user.
        RememberMe = false;
        AuthError = null;
        _activeCustomListId = null;
        if (_activeFilter == VideoFeedFilter.CustomList)
        {
            _activeFilter = VideoFeedFilter.All;
        }
        SyncFromProfileManager();
    }

    /// <summary>Deletes a profile if the username and password match.</summary>
    public bool TryDeleteProfile(string username, string password)
    {
        if (_profileManager.DeleteProfile(username, password, out var error))
        {
            AuthError = null;
            SyncFromProfileManager();
            return true;
        }
        AuthError = error;
        return false;
    }

    /// <summary>
    /// Called when a video is opened in the player. If a profile is logged in and <paramref name="videoPath"/>
    /// is non-null, records a watch and refreshes the active video engagement bar and feed.
    /// </summary>
    public void OnVideoOpened(string? videoPath)
    {
        SetActiveVideoPathForCurrentSession(videoPath);

        if (IsLoggedIn && ActiveVideoPath is { } activePath)
        {
            _profileManager.RecordWatch(activePath, expectedSessionToken: _activeVideoSessionToken);
            return;
        }

        RefreshActiveVideoStats();
        RefreshFeed();
    }

    /// <summary>
    /// Updates the active video path and engagement bar stats immediately without recording a watch yet.
    /// </summary>
    public void SetActiveVideoImmediate(string? videoPath)
    {
        SetActiveVideoPathForCurrentSession(videoPath);
        RefreshActiveVideoStats();
    }

    /// <summary>
    /// Records a watch for <paramref name="videoPath"/> if a profile is logged in (safe to invoke from a debounced background task).
    /// With <paramref name="sessionToken"/> (captured when the video was opened) nothing is recorded if the profile
    /// logged out or switched in between.
    /// </summary>
    public void RecordWatchOnly(string? videoPath, long? sessionToken = null)
    {
        var normalized = LocalProfileManager.NormalizeVideoPath(videoPath);
        if (IsLoggedIn && normalized.Length > 0)
        {
            _profileManager.RecordWatch(normalized, expectedSessionToken: sessionToken);
        }
    }

    /// <summary>
    /// Updates playback resume progress for the active video without incrementing view count. Only for the session the
    /// video was opened in: after a logout or profile switch the active video is forgotten and ticks are ignored.
    /// </summary>
    public void UpdateActiveVideoProgress(double positionSeconds, double durationSeconds)
    {
        if (!_activeVideoRecordsProgress || !IsLoggedIn || ActiveVideoPath is not { } activePath) return;
        _profileManager.UpdatePlaybackProgress(activePath, positionSeconds, durationSeconds, _activeVideoSessionToken);
    }

    // The active video exists only for a logged-in session: while logged out no path is kept at all.
    private void SetActiveVideoPathForCurrentSession(string? videoPath)
    {
        var normalized = LocalProfileManager.NormalizeVideoPath(videoPath);
        if (normalized.Length == 0 || !IsLoggedIn)
        {
            ClearActiveVideo();
            return;
        }
        _activeVideoSessionToken = _profileManager.SessionToken;
        _activeVideoRecordsProgress = true;
        ActiveVideoPath = normalized;
    }

    private void ClearActiveVideo()
    {
        ActiveVideoPath = null;
        _activeVideoSessionToken = 0;
        _activeVideoRecordsProgress = false;
    }

    // A profile logged in while a video was open: make it the active video for explicit actions only.
    private void AdoptOpenVideoForNewSession()
    {
        if (ActiveVideoPath is not null || !IsLoggedIn || OpenVideoProvider?.Invoke() is not { Length: > 0 } open) return;
        var normalized = LocalProfileManager.NormalizeVideoPath(open);
        if (normalized.Length == 0) return;
        _activeVideoSessionToken = _profileManager.SessionToken;
        _activeVideoRecordsProgress = false;
        ActiveVideoPath = normalized;
        RefreshActiveVideoStats();
    }

    /// <summary>Toggles the like status of the currently open video.</summary>
    public void ToggleActiveVideoLike()
    {
        if (!IsLoggedIn || ActiveVideoPath is not { } activePath) return;
        _profileManager.ToggleLike(activePath);
    }

    /// <summary>Sets the star rating (<c>0..5</c>) of the currently open video.</summary>
    public void SetActiveVideoRating(int stars)
    {
        if (!IsLoggedIn || ActiveVideoPath is not { } activePath) return;
        _profileManager.SetRating(activePath, stars);
    }

    /// <summary>Toggles the like status of a specific video path from a feed card.</summary>
    public void ToggleItemLike(string videoPath)
    {
        if (!IsLoggedIn || string.IsNullOrWhiteSpace(videoPath)) return;
        _profileManager.ToggleLike(videoPath);
    }

    /// <summary>Sets the star rating (<c>0..5</c>) of a specific video path from a feed card.</summary>
    public void SetItemRating(string videoPath, int stars)
    {
        if (!IsLoggedIn || string.IsNullOrWhiteSpace(videoPath)) return;
        _profileManager.SetRating(videoPath, stars);
    }

    /// <summary>Clears watch history for the active profile while preserving favorites, ratings, and custom lists.</summary>
    public bool ClearWatchHistory()
    {
        if (!IsLoggedIn) return false;
        return _profileManager.ClearWatchHistory();
    }

    // MARK: - Custom User Lists ("Listelerim")

    /// <summary>Creates a new custom video list in the active profile.</summary>
    public UserVideoList? CreateCustomList(string name)
    {
        if (!IsLoggedIn) return null;
        return _profileManager.CreateCustomList(name);
    }

    /// <summary>Renames an existing custom video list in the active profile.</summary>
    public bool RenameCustomList(string listId, string newName)
    {
        if (!IsLoggedIn) return false;
        return _profileManager.RenameCustomList(listId, newName);
    }

    /// <summary>Deletes a custom video list from the active profile.</summary>
    public bool DeleteCustomList(string listId)
    {
        if (!IsLoggedIn) return false;
        if (string.Equals(_activeCustomListId, listId, StringComparison.OrdinalIgnoreCase))
        {
            _activeCustomListId = null;
            if (_activeFilter == VideoFeedFilter.CustomList)
            {
                _activeFilter = VideoFeedFilter.All;
                OnPropertyChanged(nameof(ActiveFilter));
            }
        }
        return _profileManager.DeleteCustomList(listId);
    }

    /// <summary>Activates a specific custom list filter tab in the video platform hub.</summary>
    public void SelectCustomList(string listId)
    {
        if (string.IsNullOrWhiteSpace(listId)) return;
        _activeCustomListId = listId;
        _activeFilter = VideoFeedFilter.CustomList;
        OnPropertyChanged(nameof(ActiveCustomListId));
        OnPropertyChanged(nameof(ActiveFilter));
        OnPropertyChanged(nameof(ActiveCustomListName));
        OnPropertyChanged(nameof(RequiresAuthForCurrentTab));
        RefreshFeed();
    }

    /// <summary>Toggles membership of <paramref name="videoPath"/> in custom list <paramref name="listId"/>.</summary>
    public bool ToggleVideoInCustomList(string listId, string videoPath)
    {
        if (!IsLoggedIn || string.IsNullOrWhiteSpace(listId) || string.IsNullOrWhiteSpace(videoPath))
            return false;
        return _profileManager.ToggleVideoInCustomList(listId, videoPath);
    }

    /// <summary>Toggles membership of the currently open player video in custom list <paramref name="listId"/>.</summary>
    public bool ToggleActiveVideoInCustomList(string listId)
    {
        if (!IsLoggedIn || ActiveVideoPath is not { } activePath) return false;
        return ToggleVideoInCustomList(listId, activePath);
    }

    /// <summary>Returns true if <paramref name="videoPath"/> belongs to custom list <paramref name="listId"/>.</summary>
    public bool IsVideoInCustomList(string listId, string? videoPath) =>
        IsLoggedIn && _profileManager.IsVideoInCustomList(listId, videoPath);

    /// <summary>Feeds SSD-discovered video paths into the platform so they appear under "Keşfet" and instant search.</summary>
    public void SetDiscoveredPaths(IEnumerable<string> paths)
    {
        var next = new List<string>();
        if (paths is not null)
        {
            var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (var raw in paths)
            {
                var norm = LocalProfileManager.NormalizeVideoPath(raw);
                if (norm.Length > 0 && seen.Add(norm))
                {
                    next.Add(norm);
                }
            }
        }

        if (next.Count == _discoveredPaths.Count &&
            next.SequenceEqual(_discoveredPaths, StringComparer.OrdinalIgnoreCase))
        {
            return;
        }

        _discoveredPaths.Clear();
        _discoveredPaths.AddRange(next);
        RefreshFeed();
    }

    /// <summary>Rebuilds <see cref="FeedItems"/>, <see cref="CustomLists"/>, and active video stats from <see cref="ProfileManager"/>.</summary>
    public void RefreshFeed()
    {
        RefreshActiveVideoStats();
        CustomLists = IsLoggedIn ? _profileManager.GetCustomLists() : [];

        if (!IsLoggedIn)
        {
            // No feed at all without a profile: the indexed paths come from earlier sessions' browsing.
            FeedItems = [];
            return;
        }

        var listNamesByPath = new Dictionary<string, List<string>>(StringComparer.OrdinalIgnoreCase);
        foreach (var list in CustomLists)
        {
            foreach (var vp in list.VideoPaths)
            {
                var norm = LocalProfileManager.NormalizeVideoPath(vp);
                if (norm.Length == 0) continue;
                if (!listNamesByPath.TryGetValue(norm, out var names))
                {
                    names = [];
                    listNamesByPath[norm] = names;
                }
                names.Add(list.Name);
            }
        }

        var statsList = _profileManager.GetFeed(_activeFilter, _searchQuery, _discoveredPaths, _activeCustomListId);
        FeedItems = statsList
            .Select(s =>
            {
                var listNames = listNamesByPath.TryGetValue(s.VideoPath, out var names)
                    ? (IReadOnlyList<string>)names
                    : [];
                return new VideoCardViewModel(s, listNames);
            })
            .ToList();
    }

    private void RefreshActiveVideoStats()
    {
        if (!IsLoggedIn || ActiveVideoPath is not { } path)
        {
            ActiveVideoViewCount = 0;
            ActiveVideoIsLiked = false;
            ActiveVideoRating = 0;
            return;
        }

        var stats = _profileManager.GetStatsFastInMemory(path);
        ActiveVideoViewCount = stats?.ViewCount ?? 0;
        ActiveVideoIsLiked = stats?.IsLiked ?? false;
        ActiveVideoRating = stats?.Rating ?? 0;
    }

    private void OnProfileManagerStateChanged(object? sender, EventArgs e)
    {
        (_ui ?? LocalizedStrings.Shared.Dispatcher).Post(SyncFromProfileManager);
    }

    private void SyncFromProfileManager()
    {
        if (ActiveVideoPath is not null && (!IsLoggedIn || _profileManager.SessionToken != _activeVideoSessionToken))
        {
            ClearActiveVideo();
        }
        AdoptOpenVideoForNewSession();
        OnPropertyChanged(nameof(IsLoggedIn));
        OnPropertyChanged(nameof(CurrentUsername));
        OnPropertyChanged(nameof(LastUsername));
        OnPropertyChanged(nameof(RegisteredUsernames));
        OnPropertyChanged(nameof(HasActiveVideo));
        OnPropertyChanged(nameof(RequiresAuthForCurrentTab));
        OnPropertyChanged(nameof(ProfileStatusText));
        OnPropertyChanged(nameof(ProfileSummaryText));
        OnPropertyChanged(nameof(TotalWatchedVideosCount));
        OnPropertyChanged(nameof(TotalLikedVideosCount));
        OnPropertyChanged(nameof(TotalRatedVideosCount));
        OnPropertyChanged(nameof(TotalViewsCount));
        OnPropertyChanged(nameof(ActiveCustomListName));
        RefreshFeed();
    }

    private void OnLanguageChanged(object? sender, UILanguage language)
    {
        (_ui ?? LocalizedStrings.Shared.Dispatcher).Post(() =>
        {
            RefreshFeed();
            OnPropertyChanged(string.Empty);
            LocalizationChanged?.Invoke(this, EventArgs.Empty);
        });
    }

    public void Dispose()
    {
        _profileManager.StateChanged -= OnProfileManagerStateChanged;
        _languageState.LanguageChanged -= OnLanguageChanged;
    }
}
