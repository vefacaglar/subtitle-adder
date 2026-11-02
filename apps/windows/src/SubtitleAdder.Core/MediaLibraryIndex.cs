using System.Globalization;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace SubtitleAdder.Core;

/// <summary>
/// Metadata for a media file indexed on the local SSD/disk, including whether a sidecar subtitle
/// (<c>.srt</c>, <c>.vtt</c>, <c>.ass</c>, <c>.ssa</c>, <c>.json</c>) already exists next to it.
/// </summary>
public sealed record IndexedMediaFile(
    string FullPath,
    string FileName,
    string DirectoryPath,
    long SizeBytes,
    DateTimeOffset LastWriteTimeUtc,
    bool HasSidecarSubtitle,
    string? SidecarExtension,
    DateTimeOffset IndexedAtUtc)
{
    /// <summary>Human-readable file size (e.g. "142 MB", "1.4 GB").</summary>
    [JsonIgnore]
    public string FormattedSize => MediaLibraryIndex.FormatFileSize(SizeBytes);

    /// <summary>Badge label when a sidecar subtitle exists (e.g. "SRT", "VTT", "ASS", or "CC").</summary>
    [JsonIgnore]
    public string? SidecarBadge => !HasSidecarSubtitle
        ? null
        : string.IsNullOrWhiteSpace(SidecarExtension)
            ? "CC"
            : SidecarExtension.TrimStart('.').ToUpperInvariant();
}

/// <summary>
/// Result of scanning a single directory on disk: its immediate child folders and media files.
/// </summary>
public sealed record DirectorySnapshot(
    string DirectoryPath,
    IReadOnlyList<string> Subdirectories,
    IReadOnlyList<IndexedMediaFile> MediaFiles,
    DateTimeOffset ScannedAtUtc);

/// <summary>
/// Persistent SSD-backed media index stored by default at <c>%LOCALAPPDATA%\SubtitleAdder\media-index.json</c>.
/// Caches directory snapshots and media file metadata in memory with thread-safe atomic JSON persistence on SSD,
/// enabling instant folder browsing, sidecar subtitle detection, and cross-folder search.
/// </summary>
public sealed class MediaLibraryIndex
{
    public static readonly IReadOnlyList<string> SupportedExtensions =
    [
        ".mp4", ".mov", ".m4v", ".mkv", ".avi", ".webm", ".wmv", ".mpg", ".mpeg", ".ts", ".mts", ".m2ts", ".3gp", ".flv",
        ".mp3", ".wav", ".m4a", ".aac", ".flac", ".ogg", ".opus", ".wma",
    ];

    private static readonly HashSet<string> SupportedExtensionSet = new(SupportedExtensions, StringComparer.OrdinalIgnoreCase);

    private static readonly HashSet<string> AudioExtensionSet = new(
        [".mp3", ".wav", ".m4a", ".aac", ".flac", ".ogg", ".opus", ".wma"],
        StringComparer.OrdinalIgnoreCase);

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
    };

    private readonly string _instanceId = Guid.NewGuid().ToString("N");
    private readonly Lock _syncRoot = new();
    private readonly Lock _fileWriteLock = new();
    private readonly StateSyncBroker _broker;
    private readonly IDisposable _brokerSubscription;
    private readonly Dictionary<string, DirectoryEntry> _directories = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, IndexedMediaFile> _filesByPath = new(StringComparer.OrdinalIgnoreCase);

    public MediaLibraryIndex(string? filePath = null, StateSyncBroker? broker = null)
    {
        FilePath = string.IsNullOrWhiteSpace(filePath) ? DefaultPath : Path.GetFullPath(filePath);
        _broker = broker ?? StateSyncBroker.Shared;
        LoadFromDisk();
        _brokerSubscription = _broker.Subscribe(StateSyncTopics.MediaIndex, OnBrokerEvent);
    }

    public static string DefaultPath { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "SubtitleAdder",
        "media-index.json");

    public string FilePath { get; }

    /// <summary>Raised whenever any directory is scanned or updated in the index.</summary>
    public event EventHandler? IndexChanged;

    /// <summary>Total number of indexed media files across all scanned directories on SSD.</summary>
    public int TotalIndexedFiles
    {
        get
        {
            lock (_syncRoot) return _filesByPath.Count;
        }
    }

    /// <summary>All directory paths currently recorded in the index.</summary>
    public IReadOnlyList<string> IndexedDirectories
    {
        get
        {
            lock (_syncRoot)
            {
                return _directories.Keys
                    .OrderBy(k => k, StringComparer.OrdinalIgnoreCase)
                    .ToList();
            }
        }
    }

    public static bool IsMediaFile(string? filePath)
    {
        if (string.IsNullOrWhiteSpace(filePath)) return false;
        var ext = Path.GetExtension(filePath);
        return !string.IsNullOrEmpty(ext) && SupportedExtensionSet.Contains(ext);
    }

    public static bool IsAudioFile(string? filePath)
    {
        if (string.IsNullOrWhiteSpace(filePath)) return false;
        var ext = Path.GetExtension(filePath);
        return !string.IsNullOrEmpty(ext) && AudioExtensionSet.Contains(ext);
    }

    public static string FormatFileSize(long bytes)
    {
        if (bytes < 0) bytes = 0;
        if (bytes < 1024) return string.Create(CultureInfo.InvariantCulture, $"{bytes} B");
        var kb = bytes / 1024.0;
        if (kb < 1024) return string.Create(CultureInfo.InvariantCulture, $"{kb:0.#} KB");
        var mb = kb / 1024.0;
        if (mb < 1024) return string.Create(CultureInfo.InvariantCulture, $"{mb:0.#} MB");
        var gb = mb / 1024.0;
        return string.Create(CultureInfo.InvariantCulture, $"{gb:0.##} GB");
    }

    internal static string NormalizeDirectoryPath(string directoryPath)
    {
        if (string.IsNullOrWhiteSpace(directoryPath)) return "";
        try
        {
            var full = Path.GetFullPath(directoryPath.Trim());
            var root = Path.GetPathRoot(full);
            if (!string.IsNullOrEmpty(root) && string.Equals(full, root, StringComparison.OrdinalIgnoreCase))
            {
                return full;
            }
            return full.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        }
        catch (Exception)
        {
            return directoryPath.Trim();
        }
    }

    /// <summary>Immediately returns the cached media file list for <paramref name="directoryPath"/> if previously indexed.</summary>
    public IReadOnlyList<IndexedMediaFile> GetCachedDirectory(string directoryPath)
    {
        var normalized = NormalizeDirectoryPath(directoryPath);
        if (string.IsNullOrEmpty(normalized)) return [];
        lock (_syncRoot)
        {
            return _directories.TryGetValue(normalized, out var entry)
                ? entry.MediaFiles
                : [];
        }
    }

    /// <summary>Immediately returns the cached subdirectory paths for <paramref name="directoryPath"/> if previously indexed.</summary>
    public IReadOnlyList<string> GetCachedSubdirectories(string directoryPath)
    {
        var normalized = NormalizeDirectoryPath(directoryPath);
        if (string.IsNullOrEmpty(normalized)) return [];
        lock (_syncRoot)
        {
            return _directories.TryGetValue(normalized, out var entry)
                ? entry.Subdirectories
                : [];
        }
    }

    /// <summary>
    /// Enumerates subdirectories and supported media files in <paramref name="directoryPath"/> (skipping hidden,
    /// system, or inaccessible entries gracefully), detects sidecar subtitle files via
    /// <see cref="SubtitleParser.FindSidecarSubtitle"/>, updates the in-memory index (pruning deleted files),
    /// persists atomically to SSD, and returns the resulting <see cref="DirectorySnapshot"/>.
    /// </summary>
    public Task<DirectorySnapshot> ScanDirectoryAsync(string directoryPath, CancellationToken cancellationToken = default)
    {
        return Task.Run(() => ScanDirectoryCore(directoryPath, persist: true, cancellationToken), cancellationToken);
    }

    private DirectorySnapshot ScanDirectoryCore(string directoryPath, bool persist, CancellationToken cancellationToken)
    {
        var normalized = NormalizeDirectoryPath(directoryPath);
        var now = DateTimeOffset.UtcNow;
        if (string.IsNullOrEmpty(normalized) || !Directory.Exists(normalized))
        {
            RemoveDirectoryFromCache(normalized);
            if (persist) SaveToDisk();
            IndexChanged?.Invoke(this, EventArgs.Empty);
            return new DirectorySnapshot(normalized, [], [], now);
        }

        var enumOptions = new EnumerationOptions
        {
            IgnoreInaccessible = true,
            RecurseSubdirectories = false,
            AttributesToSkip = FileAttributes.Hidden | FileAttributes.System,
            ReturnSpecialDirectories = false,
        };

        var subdirs = new List<string>();
        try
        {
            foreach (var sub in Directory.EnumerateDirectories(normalized, "*", enumOptions))
            {
                cancellationToken.ThrowIfCancellationRequested();
                var name = Path.GetFileName(sub);
                if (string.IsNullOrEmpty(name) || name.StartsWith('.')) continue;
                subdirs.Add(NormalizeDirectoryPath(sub));
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or DirectoryNotFoundException)
        {
        }

        subdirs.Sort(StringComparer.OrdinalIgnoreCase);

        var mediaFiles = new List<IndexedMediaFile>();
        try
        {
            var dirInfo = new DirectoryInfo(normalized);
            foreach (var fileInfo in dirInfo.EnumerateFiles("*", enumOptions))
            {
                cancellationToken.ThrowIfCancellationRequested();
                if (!IsMediaFile(fileInfo.Name)) continue;
                try
                {
                    var fullPath = fileInfo.FullName;
                    var sidecarPath = SubtitleParser.FindSidecarSubtitle(fullPath);
                    var hasSidecar = sidecarPath is not null;
                    var sidecarExt = sidecarPath is not null ? Path.GetExtension(sidecarPath).ToLowerInvariant() : null;
                    mediaFiles.Add(new IndexedMediaFile(
                        FullPath: fullPath,
                        FileName: fileInfo.Name,
                        DirectoryPath: normalized,
                        SizeBytes: fileInfo.Length,
                        LastWriteTimeUtc: new DateTimeOffset(fileInfo.LastWriteTimeUtc, TimeSpan.Zero),
                        HasSidecarSubtitle: hasSidecar,
                        SidecarExtension: sidecarExt,
                        IndexedAtUtc: now));
                }
                catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
                {
                }
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or DirectoryNotFoundException)
        {
        }

        mediaFiles.Sort((a, b) => StringComparer.OrdinalIgnoreCase.Compare(a.FileName, b.FileName));

        var snapshot = new DirectorySnapshot(normalized, subdirs, mediaFiles, now);
        UpdateDirectoryInCache(snapshot);
        if (persist) SaveToDisk();
        IndexChanged?.Invoke(this, EventArgs.Empty);
        return snapshot;
    }

    /// <summary>
    /// Breadth-first background indexer that scans <paramref name="rootDirectory"/> and its child folders up to
    /// <paramref name="maxDepth"/> so browsing a parent folder automatically indexes nearby video folders onto the SSD.
    /// </summary>
    public Task CrawlSubtreeAsync(string rootDirectory, int maxDepth = 2, CancellationToken cancellationToken = default)
    {
        return Task.Run(() =>
        {
            var normalizedRoot = NormalizeDirectoryPath(rootDirectory);
            if (string.IsNullOrEmpty(normalizedRoot) || !Directory.Exists(normalizedRoot)) return;

            var clampedDepth = Math.Clamp(maxDepth, 0, 5);
            const int maxDirectoriesPerCrawl = 64;
            var visited = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            var queue = new Queue<(string Path, int Depth)>();
            queue.Enqueue((normalizedRoot, 0));
            visited.Add(normalizedRoot);

            var scannedAny = false;
            while (queue.Count > 0 && visited.Count <= maxDirectoriesPerCrawl)
            {
                cancellationToken.ThrowIfCancellationRequested();
                var (currentDir, depth) = queue.Dequeue();
                var snapshot = ScanDirectoryCore(currentDir, persist: false, cancellationToken);
                scannedAny = true;

                if (depth < clampedDepth)
                {
                    foreach (var sub in snapshot.Subdirectories)
                    {
                        if (visited.Count >= maxDirectoriesPerCrawl) break;
                        if (visited.Add(sub))
                        {
                            queue.Enqueue((sub, depth + 1));
                        }
                    }
                }
            }

            if (scannedAny)
            {
                SaveToDisk();
                IndexChanged?.Invoke(this, EventArgs.Empty);
            }
        }, cancellationToken);
    }

    /// <summary>
    /// Searches across all indexed media files on the SSD (case-insensitive substring match on
    /// <see cref="IndexedMediaFile.FileName"/> or <see cref="IndexedMediaFile.DirectoryPath"/>),
    /// ordered by relevance (filename match first, then recent <see cref="IndexedMediaFile.LastWriteTimeUtc"/>).
    /// </summary>
    public IReadOnlyList<IndexedMediaFile> Search(string query, int maxResults = 100)
    {
        if (string.IsNullOrWhiteSpace(query) || maxResults <= 0) return [];
        var trimmed = query.Trim();

        List<IndexedMediaFile> candidates;
        lock (_syncRoot)
        {
            candidates = _filesByPath.Values.ToList();
        }

        return candidates
            .Select(file => (File: file, Score: ComputeMatchScore(file, trimmed)))
            .Where(x => x.Score > 0)
            .OrderByDescending(x => x.Score)
            .ThenByDescending(x => x.File.LastWriteTimeUtc)
            .ThenBy(x => x.File.FileName, StringComparer.OrdinalIgnoreCase)
            .Take(maxResults)
            .Select(x => x.File)
            .ToList();
    }

    private static int ComputeMatchScore(IndexedMediaFile file, string query)
    {
        if (file.FileName.StartsWith(query, StringComparison.OrdinalIgnoreCase)) return 3;
        if (file.FileName.Contains(query, StringComparison.OrdinalIgnoreCase)) return 2;
        if (file.DirectoryPath.Contains(query, StringComparison.OrdinalIgnoreCase)) return 1;
        return 0;
    }

    private void RemoveDirectoryFromCache(string normalizedDir)
    {
        if (string.IsNullOrEmpty(normalizedDir)) return;
        lock (_syncRoot)
        {
            if (_directories.Remove(normalizedDir, out var existing))
            {
                foreach (var oldFile in existing.MediaFiles)
                {
                    _filesByPath.Remove(oldFile.FullPath);
                }
            }
        }
    }

    private void UpdateDirectoryInCache(DirectorySnapshot snapshot)
    {
        lock (_syncRoot)
        {
            if (_directories.TryGetValue(snapshot.DirectoryPath, out var existing))
            {
                foreach (var oldFile in existing.MediaFiles)
                {
                    _filesByPath.Remove(oldFile.FullPath);
                }
            }

            _directories[snapshot.DirectoryPath] = new DirectoryEntry
            {
                DirectoryPath = snapshot.DirectoryPath,
                Subdirectories = snapshot.Subdirectories.ToList(),
                MediaFiles = snapshot.MediaFiles.ToList(),
                ScannedAtUtc = snapshot.ScannedAtUtc,
            };

            foreach (var file in snapshot.MediaFiles)
            {
                _filesByPath[file.FullPath] = file;
            }
        }
    }

    private void LoadFromDisk()
    {
        try
        {
            if (AtomicFileStore.ReadAllTextShared(FilePath) is not { } json) return;
            var persisted = JsonSerializer.Deserialize<PersistedIndex>(json, JsonOptions);
            if (persisted?.Directories is null) return;

            lock (_syncRoot)
            {
                _directories.Clear();
                _filesByPath.Clear();
                foreach (var dir in persisted.Directories)
                {
                    if (string.IsNullOrWhiteSpace(dir.DirectoryPath)) continue;
                    var normalized = NormalizeDirectoryPath(dir.DirectoryPath);
                    var files = (dir.MediaFiles ?? [])
                        .Where(f => !string.IsNullOrWhiteSpace(f.FullPath))
                        .OrderBy(f => f.FileName, StringComparer.OrdinalIgnoreCase)
                        .ToList();
                    var subdirs = (dir.Subdirectories ?? [])
                        .Where(s => !string.IsNullOrWhiteSpace(s))
                        .Select(NormalizeDirectoryPath)
                        .OrderBy(s => s, StringComparer.OrdinalIgnoreCase)
                        .ToList();

                    _directories[normalized] = new DirectoryEntry
                    {
                        DirectoryPath = normalized,
                        Subdirectories = subdirs,
                        MediaFiles = files,
                        ScannedAtUtc = dir.ScannedAtUtc,
                    };

                    foreach (var file in files)
                    {
                        _filesByPath[file.FullPath] = file;
                    }
                }
            }
        }
        catch (Exception ex) when (ex is IOException or JsonException or UnauthorizedAccessException)
        {
            // Corrupt or inaccessible index file starts fresh.
        }
    }

    private void SaveToDisk()
    {
        lock (_fileWriteLock)
        {
            PersistedIndex payload;
            lock (_syncRoot)
            {
                payload = new PersistedIndex
                {
                    Version = 1,
                    UpdatedAtUtc = DateTimeOffset.UtcNow,
                    Directories = _directories.Values
                        .OrderBy(d => d.DirectoryPath, StringComparer.OrdinalIgnoreCase)
                        .ToList(),
                };
            }

            var json = JsonSerializer.Serialize(payload, JsonOptions);
            if (AtomicFileStore.WriteAllTextAtomic(FilePath, json))
            {
                _broker.Publish(StateSyncTopics.MediaIndex, new StateSyncEvent
                {
                    Topic = StateSyncTopics.MediaIndex,
                    Kind = StateSyncEventKind.MediaIndexed,
                    SenderInstanceId = _instanceId,
                    ScopePath = FilePath,
                });
            }
        }
    }

    private void OnBrokerEvent(StateSyncEvent evt)
    {
        if (string.Equals(evt.SenderInstanceId, _instanceId, StringComparison.Ordinal) ||
            !string.Equals(evt.ScopePath, FilePath, StringComparison.OrdinalIgnoreCase))
        {
            return;
        }
        LoadFromDisk();
        IndexChanged?.Invoke(this, EventArgs.Empty);
    }

    private sealed class PersistedIndex
    {
        public int Version { get; set; } = 1;
        public DateTimeOffset UpdatedAtUtc { get; set; }
        public List<DirectoryEntry> Directories { get; set; } = [];
    }

    private sealed class DirectoryEntry
    {
        public string DirectoryPath { get; set; } = "";
        public List<string> Subdirectories { get; set; } = [];
        public List<IndexedMediaFile> MediaFiles { get; set; } = [];
        public DateTimeOffset ScannedAtUtc { get; set; }
    }
}
