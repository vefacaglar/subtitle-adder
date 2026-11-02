using CommunityToolkit.Mvvm.ComponentModel;
using SubtitleAdder.EngineKit;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

/// <summary>Subfolder entry displayed in the File Explorer sidebar.</summary>
public sealed record ExplorerFolderItem(string FullPath, string Name);

/// <summary>Quick-access location (Videos, Downloads, Desktop, or mounted drive) in the File Explorer header.</summary>
public sealed record ExplorerRootLocation(string FullPath, string Name, string IconGlyph = "\uE8B7");

/// <summary>
/// View model for the left File Explorer sidebar. Browses local folders, coordinates SSD indexing via
/// <see cref="MediaLibraryIndex"/>, and supports instant filtering across both the current folder and the entire
/// SSD media index.
/// </summary>
public sealed partial class FileExplorerViewModel : ObservableObject
{
    public const string LastFolderDefaultsKey = "explorerLastFolder";

    private readonly MediaLibraryIndex _index;
    private readonly ISettingsStore _settings;
    private readonly IUiDispatcher _ui;
    private readonly Stack<string> _backStack = new();

    private string? _currentFolder;
    private bool _isScanning;
    private string _searchQuery = "";
    private IReadOnlyList<ExplorerFolderItem> _currentFolderSubdirectories = [];
    private IReadOnlyList<IndexedMediaFile> _currentFolderFiles = [];
    private IReadOnlyList<ExplorerFolderItem> _folders = [];
    private IReadOnlyList<IndexedMediaFile> _files = [];
    private IReadOnlyList<ExplorerRootLocation> _quickLocations = [];
    private CancellationTokenSource? _scanCts;

    public FileExplorerViewModel(MediaLibraryIndex index, ISettingsStore settings, IUiDispatcher? dispatcher = null)
    {
        _index = index;
        _settings = settings;
        _ui = dispatcher ?? ImmediateDispatcher.Instance;

        _quickLocations = BuildQuickLocations();

        var savedFolder = _settings.GetString(LastFolderDefaultsKey);
        if (!string.IsNullOrWhiteSpace(savedFolder) && Directory.Exists(savedFolder))
        {
            var normalized = MediaLibraryIndex.NormalizeDirectoryPath(savedFolder);
            _currentFolder = normalized;
            _currentFolderSubdirectories = _index.GetCachedSubdirectories(normalized)
                .Select(CreateFolderItem)
                .ToList();
            _currentFolderFiles = _index.GetCachedDirectory(normalized);
            ApplyFilterCore();
        }

        _index.IndexChanged += OnIndexChanged;
        LanguageState.Shared.LanguageChanged += OnLanguageChanged;
    }

    public MediaLibraryIndex Index => _index;

    public string? CurrentFolder
    {
        get => _currentFolder;
        private set
        {
            if (!SetProperty(ref _currentFolder, value)) return;
            OnPropertyChanged(nameof(CurrentFolderName));
            OnPropertyChanged(nameof(CanGoUp));
        }
    }

    /// <summary>Display name of the current folder or drive root.</summary>
    public string CurrentFolderName
    {
        get
        {
            if (string.IsNullOrWhiteSpace(_currentFolder)) return L("Dosya Gezgini");
            var trimmed = _currentFolder.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
            var name = Path.GetFileName(trimmed);
            return string.IsNullOrEmpty(name) ? _currentFolder : name;
        }
    }

    public bool CanGoUp
    {
        get
        {
            if (string.IsNullOrWhiteSpace(_currentFolder)) return false;
            try
            {
                return Directory.GetParent(_currentFolder) is not null;
            }
            catch (Exception)
            {
                return false;
            }
        }
    }

    public bool CanGoBack => _backStack.Count > 0;

    public bool IsScanning
    {
        get => _isScanning;
        private set => SetProperty(ref _isScanning, value);
    }

    /// <summary>
    /// Search filter. When empty, shows the current folder's subfolders and media files.
    /// When non-empty, filters the current folder AND includes matching media files from the entire SSD index.
    /// </summary>
    public string SearchQuery
    {
        get => _searchQuery;
        set
        {
            var next = value ?? "";
            if (!SetProperty(ref _searchQuery, next)) return;
            ApplyFilterCore();
        }
    }

    public IReadOnlyList<ExplorerFolderItem> Folders
    {
        get => _folders;
        private set => SetProperty(ref _folders, value);
    }

    public IReadOnlyList<IndexedMediaFile> Files
    {
        get => _files;
        private set
        {
            if (!SetProperty(ref _files, value)) return;
            OnPropertyChanged(nameof(StatusSummary));
        }
    }

    public IReadOnlyList<ExplorerRootLocation> QuickLocations
    {
        get => _quickLocations;
        private set => SetProperty(ref _quickLocations, value);
    }

    public int TotalIndexedFiles => _index.TotalIndexedFiles;

    public string StatusSummary =>
        L("%lld video · SSD indeksinde %lld dosya", (long)_files.Count, (long)TotalIndexedFiles);

    /// <summary>Returns the saved folder or the system Videos / UserProfile directory as a starting point.</summary>
    public string GetInitialFolder()
    {
        if (!string.IsNullOrWhiteSpace(_currentFolder) && Directory.Exists(_currentFolder))
        {
            return _currentFolder;
        }

        var saved = _settings.GetString(LastFolderDefaultsKey);
        if (!string.IsNullOrWhiteSpace(saved) && Directory.Exists(saved))
        {
            return MediaLibraryIndex.NormalizeDirectoryPath(saved);
        }

        var videos = Environment.GetFolderPath(Environment.SpecialFolder.MyVideos);
        if (!string.IsNullOrWhiteSpace(videos) && Directory.Exists(videos))
        {
            return MediaLibraryIndex.NormalizeDirectoryPath(videos);
        }

        var profile = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        if (!string.IsNullOrWhiteSpace(profile) && Directory.Exists(profile))
        {
            return MediaLibraryIndex.NormalizeDirectoryPath(profile);
        }

        return AppContext.BaseDirectory;
    }

    /// <summary>Navigates to <paramref name="folderPath"/>, scans it, and optionally crawls nearby subfolders onto the SSD index.</summary>
    public Task NavigateToAsync(string folderPath, bool crawlSubfolders = true) =>
        NavigateCoreAsync(folderPath, pushHistory: true, crawlSubfolders: crawlSubfolders);

    /// <summary>Navigates to the parent directory of <see cref="CurrentFolder"/> if one exists.</summary>
    public async Task GoUpAsync()
    {
        if (!CanGoUp || string.IsNullOrWhiteSpace(_currentFolder)) return;
        string? parentPath = null;
        try
        {
            parentPath = Directory.GetParent(_currentFolder)?.FullName;
        }
        catch (Exception)
        {
        }

        if (!string.IsNullOrWhiteSpace(parentPath))
        {
            await NavigateCoreAsync(parentPath, pushHistory: true, crawlSubfolders: false);
        }
    }

    /// <summary>Navigates back to the previously visited folder.</summary>
    public async Task GoBackAsync()
    {
        if (_backStack.Count == 0) return;
        var previous = _backStack.Pop();
        OnPropertyChanged(nameof(CanGoBack));
        await NavigateCoreAsync(previous, pushHistory: false, crawlSubfolders: false);
    }

    /// <summary>Re-scans <see cref="CurrentFolder"/> (or opens the initial folder if none is active yet) and refreshes quick locations.</summary>
    public async Task RefreshAsync()
    {
        QuickLocations = BuildQuickLocations();
        var target = _currentFolder ?? GetInitialFolder();
        await NavigateCoreAsync(target, pushHistory: false, crawlSubfolders: false);
    }

    /// <summary>
    /// Synchronizes the explorer with a loaded or updated video path:
    /// if <see cref="CurrentFolder"/> is not set yet, navigates to the video's parent directory;
    /// otherwise rescans the video's directory so sidecar subtitle badges stay up to date.
    /// </summary>
    public async Task SyncToVideoPathAsync(string? videoPath)
    {
        if (string.IsNullOrWhiteSpace(videoPath)) return;
        var dir = Directory.Exists(videoPath) ? videoPath : Path.GetDirectoryName(videoPath);
        if (string.IsNullOrWhiteSpace(dir) || !Directory.Exists(dir)) return;

        var normalizedDir = MediaLibraryIndex.NormalizeDirectoryPath(dir);
        if (string.IsNullOrWhiteSpace(_currentFolder))
        {
            await NavigateToAsync(normalizedDir, crawlSubfolders: true);
            return;
        }

        if (string.Equals(_currentFolder, normalizedDir, StringComparison.OrdinalIgnoreCase))
        {
            await RefreshAsync();
        }
        else
        {
            await _index.ScanDirectoryAsync(normalizedDir);
            await _ui.InvokeAsync(() =>
            {
                ApplyFilterCore();
                OnPropertyChanged(nameof(TotalIndexedFiles));
                OnPropertyChanged(nameof(StatusSummary));
            });
        }
    }

    private async Task NavigateCoreAsync(string folderPath, bool pushHistory, bool crawlSubfolders)
    {
        if (string.IsNullOrWhiteSpace(folderPath)) return;
        var normalized = MediaLibraryIndex.NormalizeDirectoryPath(folderPath);
        if (string.IsNullOrEmpty(normalized) || !Directory.Exists(normalized)) return;

        _scanCts?.Cancel();
        var cts = new CancellationTokenSource();
        _scanCts = cts;
        var token = cts.Token;

        if (pushHistory &&
            !string.IsNullOrWhiteSpace(_currentFolder) &&
            !string.Equals(_currentFolder, normalized, StringComparison.OrdinalIgnoreCase))
        {
            _backStack.Push(_currentFolder);
            OnPropertyChanged(nameof(CanGoBack));
        }

        CurrentFolder = normalized;
        _settings.SetString(LastFolderDefaultsKey, normalized);

        // Immediately populate from cache if available so folder switching feels instant.
        var cachedDirs = _index.GetCachedSubdirectories(normalized);
        var cachedFiles = _index.GetCachedDirectory(normalized);
        if (cachedDirs.Count > 0 || cachedFiles.Count > 0)
        {
            _currentFolderSubdirectories = cachedDirs.Select(CreateFolderItem).ToList();
            _currentFolderFiles = cachedFiles;
            ApplyFilterCore();
        }

        IsScanning = true;
        try
        {
            var snapshot = await _index.ScanDirectoryAsync(normalized, token);
            if (token.IsCancellationRequested) return;

            await _ui.InvokeAsync(() =>
            {
                if (token.IsCancellationRequested) return;
                _currentFolderSubdirectories = snapshot.Subdirectories.Select(CreateFolderItem).ToList();
                _currentFolderFiles = snapshot.MediaFiles;
                ApplyFilterCore();
                OnPropertyChanged(nameof(TotalIndexedFiles));
                OnPropertyChanged(nameof(StatusSummary));
            });

            if (crawlSubfolders && !token.IsCancellationRequested)
            {
                await _index.CrawlSubtreeAsync(normalized, maxDepth: 2, token);
                if (!token.IsCancellationRequested)
                {
                    await _ui.InvokeAsync(() =>
                    {
                        ApplyFilterCore();
                        OnPropertyChanged(nameof(TotalIndexedFiles));
                        OnPropertyChanged(nameof(StatusSummary));
                    });
                }
            }
        }
        catch (OperationCanceledException)
        {
        }
        finally
        {
            if (ReferenceEquals(_scanCts, cts))
            {
                await _ui.InvokeAsync(() => IsScanning = false);
            }
        }
    }

    private void ApplyFilterCore()
    {
        var query = _searchQuery.Trim();
        if (string.IsNullOrEmpty(query))
        {
            Folders = _currentFolderSubdirectories;
            Files = _currentFolderFiles;
            return;
        }

        Folders = _currentFolderSubdirectories
            .Where(f => f.Name.Contains(query, StringComparison.OrdinalIgnoreCase))
            .ToList();

        var combinedFiles = new List<IndexedMediaFile>();
        var seenPaths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        foreach (var file in _currentFolderFiles)
        {
            if (file.FileName.Contains(query, StringComparison.OrdinalIgnoreCase) ||
                file.DirectoryPath.Contains(query, StringComparison.OrdinalIgnoreCase))
            {
                if (seenPaths.Add(file.FullPath))
                {
                    combinedFiles.Add(file);
                }
            }
        }

        foreach (var indexedMatch in _index.Search(query, maxResults: 100))
        {
            if (seenPaths.Add(indexedMatch.FullPath))
            {
                combinedFiles.Add(indexedMatch);
            }
        }

        Files = combinedFiles;
    }

    private void OnIndexChanged(object? sender, EventArgs e)
    {
        _ui.Post(() =>
        {
            OnPropertyChanged(nameof(TotalIndexedFiles));
            OnPropertyChanged(nameof(StatusSummary));
        });
    }

    private void OnLanguageChanged(object? sender, UILanguage e)
    {
        _ui.Post(() =>
        {
            QuickLocations = BuildQuickLocations();
            OnPropertyChanged(nameof(CurrentFolderName));
            OnPropertyChanged(nameof(StatusSummary));
        });
    }

    private static ExplorerFolderItem CreateFolderItem(string path)
    {
        var trimmed = path.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        var name = Path.GetFileName(trimmed);
        return new ExplorerFolderItem(path, string.IsNullOrEmpty(name) ? path : name);
    }

    private static IReadOnlyList<ExplorerRootLocation> BuildQuickLocations()
    {
        var list = new List<ExplorerRootLocation>();
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        void AddSpecial(string? path, string name, string icon)
        {
            if (string.IsNullOrWhiteSpace(path) || !Directory.Exists(path)) return;
            var normalized = MediaLibraryIndex.NormalizeDirectoryPath(path);
            if (seen.Add(normalized))
            {
                list.Add(new ExplorerRootLocation(normalized, name, icon));
            }
        }

        var videos = Environment.GetFolderPath(Environment.SpecialFolder.MyVideos);
        AddSpecial(videos, L("Videolar"), "\uE8B2");

        var userProfile = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        if (!string.IsNullOrWhiteSpace(userProfile))
        {
            AddSpecial(Path.Combine(userProfile, "Downloads"), L("İndirilenler"), "\uE896");
        }

        var desktop = Environment.GetFolderPath(Environment.SpecialFolder.DesktopDirectory);
        AddSpecial(desktop, L("Masaüstü"), "\uE8FC");

        try
        {
            foreach (var drive in DriveInfo.GetDrives())
            {
                if (!drive.IsReady) continue;
                if (drive.DriveType is not (DriveType.Fixed or DriveType.Removable or DriveType.Network)) continue;
                var rootPath = MediaLibraryIndex.NormalizeDirectoryPath(drive.RootDirectory.FullName);
                var label = "";
                try
                {
                    label = drive.VolumeLabel;
                }
                catch (Exception)
                {
                }

                var driveName = string.IsNullOrWhiteSpace(label)
                    ? rootPath.TrimEnd('\\')
                    : $"{label} ({rootPath.TrimEnd('\\')})";
                if (seen.Add(rootPath))
                {
                    list.Add(new ExplorerRootLocation(rootPath, driveName, "\uEDA2"));
                }
            }
        }
        catch (Exception)
        {
        }

        return list;
    }
}
