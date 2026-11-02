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
        if (SubtitleParser.FindSidecarSubtitle(videoPath) is { } exact)
            return exact;

        if (string.IsNullOrWhiteSpace(videoPath)) return null;
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
    private readonly List<string> _discoveredPaths = [];

    private string? _authError;
    private VideoFeedFilter _activeFilter = VideoFeedFilter.All;
    private string? _activeCustomListId;
    private string _searchQuery = "";
    private IReadOnlyList<VideoCardViewModel> _feedItems = [];
    private IReadOnlyList<UserVideoList> _customLists = [];
    private string? _activeVideoPath;
    private int _activeVideoViewCount;
    private bool _activeVideoIsLiked;
    private int _activeVideoRating;
    private bool _rememberMe = true;

    public VideoPlatformViewModel(LocalProfileManager? profileManager = null, LanguageState? languageState = null)
    {
        _profileManager = profileManager ?? new LocalProfileManager();
        _languageState = languageState ?? LanguageState.Shared;
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

    /// <summary>Currently logged-in username, or <c>null</c> when logged out.</summary>
    public string? CurrentUsername => _profileManager.CurrentUsername;

    /// <summary>Most recently used username on this machine for convenience pre-filling.</summary>
    public string? LastUsername => _profileManager.LastUsername;

    /// <summary>Whether "Girişi hatırla" (remember login across restarts and instances) is checked (default <c>true</c>).</summary>
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
    /// True when the user is logged out and has selected a personal profile tab (History, Liked, TopRated, MostWatched, CustomList)
    /// without an active search query. When on "Keşfet" (<see cref="VideoFeedFilter.All"/>) or searching, SSD-indexed videos are shown even when logged out.
    /// </summary>
    public bool RequiresAuthForCurrentTab =>
        !IsLoggedIn && _activeFilter != VideoFeedFilter.All && string.IsNullOrWhiteSpace(_searchQuery);

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

    /// <summary>Stores an encrypted thumbnail (<c>byte[]</c>) for <paramref name="videoPath"/> inside the active user's AES-256-GCM vault.</summary>
    public bool SaveEncryptedThumbnail(string videoPath, byte[] thumbnailBytes)
    {
        if (!IsLoggedIn || string.IsNullOrWhiteSpace(videoPath) || thumbnailBytes is not { Length: > 0 })
            return false;
        return _profileManager.SetThumbnail(videoPath, thumbnailBytes);
    }

    /// <summary>Retrieves the decrypted thumbnail (<c>byte[]</c>) for <paramref name="videoPath"/> from the active user's vault.</summary>
    public byte[]? GetEncryptedThumbnail(string? videoPath) =>
        IsLoggedIn ? _profileManager.GetThumbnail(videoPath) : null;

    /// <summary>Logs out of the active profile and clears personal engagement state from the UI.</summary>
    public void Logout()
    {
        _profileManager.Logout();
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
        var normalized = LocalProfileManager.NormalizeVideoPath(videoPath);
        ActiveVideoPath = normalized.Length > 0 ? normalized : null;

        if (IsLoggedIn && ActiveVideoPath is { } activePath)
        {
            _profileManager.RecordWatch(activePath);
        }

        RefreshActiveVideoStats();
        RefreshFeed();
    }

    /// <summary>Updates playback resume progress for the active video without incrementing view count.</summary>
    public void UpdateActiveVideoProgress(double positionSeconds, double durationSeconds)
    {
        if (!IsLoggedIn || ActiveVideoPath is not { } activePath) return;
        _profileManager.UpdatePlaybackProgress(activePath, positionSeconds, durationSeconds);
    }

    /// <summary>Toggles the like status of the currently open video.</summary>
    public void ToggleActiveVideoLike()
    {
        if (!IsLoggedIn || ActiveVideoPath is not { } activePath) return;
        _profileManager.ToggleLike(activePath);
        RefreshActiveVideoStats();
        RefreshFeed();
    }

    /// <summary>Sets the star rating (<c>0..5</c>) of the currently open video.</summary>
    public void SetActiveVideoRating(int stars)
    {
        if (!IsLoggedIn || ActiveVideoPath is not { } activePath) return;
        _profileManager.SetRating(activePath, stars);
        RefreshActiveVideoStats();
        RefreshFeed();
    }

    /// <summary>Toggles the like status of a specific video path from a feed card.</summary>
    public void ToggleItemLike(string videoPath)
    {
        if (!IsLoggedIn || string.IsNullOrWhiteSpace(videoPath)) return;
        _profileManager.ToggleLike(videoPath);
        RefreshActiveVideoStats();
        RefreshFeed();
    }

    /// <summary>Sets the star rating (<c>0..5</c>) of a specific video path from a feed card.</summary>
    public void SetItemRating(string videoPath, int stars)
    {
        if (!IsLoggedIn || string.IsNullOrWhiteSpace(videoPath)) return;
        _profileManager.SetRating(videoPath, stars);
        RefreshActiveVideoStats();
        RefreshFeed();
    }

    /// <summary>Clears watch history for the active profile while preserving favorites, ratings, and custom lists.</summary>
    public bool ClearWatchHistory()
    {
        if (!IsLoggedIn) return false;
        var ok = _profileManager.ClearWatchHistory();
        if (ok)
        {
            RefreshActiveVideoStats();
            RefreshFeed();
        }
        return ok;
    }

    // MARK: - Custom User Lists ("Listelerim")

    /// <summary>Creates a new custom video list in the active profile.</summary>
    public UserVideoList? CreateCustomList(string name)
    {
        if (!IsLoggedIn) return null;
        var created = _profileManager.CreateCustomList(name);
        if (created is not null)
        {
            SyncFromProfileManager();
        }
        return created;
    }

    /// <summary>Renames an existing custom video list in the active profile.</summary>
    public bool RenameCustomList(string listId, string newName)
    {
        if (!IsLoggedIn) return false;
        var ok = _profileManager.RenameCustomList(listId, newName);
        if (ok) SyncFromProfileManager();
        return ok;
    }

    /// <summary>Deletes a custom video list from the active profile.</summary>
    public bool DeleteCustomList(string listId)
    {
        if (!IsLoggedIn) return false;
        var ok = _profileManager.DeleteCustomList(listId);
        if (ok)
        {
            if (string.Equals(_activeCustomListId, listId, StringComparison.OrdinalIgnoreCase))
            {
                _activeCustomListId = null;
                if (_activeFilter == VideoFeedFilter.CustomList)
                {
                    _activeFilter = VideoFeedFilter.All;
                    OnPropertyChanged(nameof(ActiveFilter));
                }
            }
            SyncFromProfileManager();
        }
        return ok;
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
        var nowInList = _profileManager.ToggleVideoInCustomList(listId, videoPath);
        SyncFromProfileManager();
        return nowInList;
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
            if (_activeFilter == VideoFeedFilter.All || !string.IsNullOrWhiteSpace(_searchQuery))
            {
                var anonStats = _profileManager.GetFeed(VideoFeedFilter.All, _searchQuery, _discoveredPaths);
                FeedItems = anonStats.Select(s => new VideoCardViewModel(s, [])).ToList();
            }
            else
            {
                FeedItems = [];
            }
            return;
        }

        var statsList = _profileManager.GetFeed(_activeFilter, _searchQuery, _discoveredPaths, _activeCustomListId);
        FeedItems = statsList
            .Select(s =>
            {
                var listNames = _profileManager
                    .GetCustomListsContaining(s.VideoPath)
                    .Select(l => l.Name)
                    .ToList();
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

        var stats = _profileManager.GetStats(path);
        ActiveVideoViewCount = stats?.ViewCount ?? 0;
        ActiveVideoIsLiked = stats?.IsLiked ?? false;
        ActiveVideoRating = stats?.Rating ?? 0;
    }

    private void OnProfileManagerStateChanged(object? sender, EventArgs e)
    {
        SyncFromProfileManager();
    }

    private void SyncFromProfileManager()
    {
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
        RefreshFeed();
        OnPropertyChanged(string.Empty);
        LocalizationChanged?.Invoke(this, EventArgs.Empty);
    }

    public void Dispose()
    {
        _profileManager.StateChanged -= OnProfileManagerStateChanged;
        _languageState.LanguageChanged -= OnLanguageChanged;
    }
}
