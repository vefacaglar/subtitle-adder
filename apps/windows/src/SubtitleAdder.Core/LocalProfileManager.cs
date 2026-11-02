using System.Buffers;
using System.Buffers.Binary;
using System.Collections.Concurrent;
using System.Globalization;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

/// <summary>Feed filter categories for the local YouTube-style video platform.</summary>
public enum VideoFeedFilter
{
    All,
    History,
    Liked,
    MostWatched,
    TopRated,
    CustomList,
}

/// <summary>A user-created custom playlist/collection of local videos inside a profile.</summary>
public sealed record UserVideoList
{
    public required string Id { get; init; }
    public required string Name { get; init; }
    public required IReadOnlyList<string> VideoPaths { get; init; }
    public DateTimeOffset CreatedAtUtc { get; init; }
    public DateTimeOffset UpdatedAtUtc { get; init; }
}

/// <summary>Per-profile engagement and watch statistics for a single local video file.</summary>
public sealed record VideoUserStats
{
    public string? ContentFingerprint { get; init; }
    public required string VideoPath { get; init; }
    public required string FileName { get; init; }
    public required string DirectoryPath { get; init; }
    public int ViewCount { get; init; }
    public bool IsLiked { get; init; }
    public int Rating { get; init; }
    public double LastPositionSeconds { get; init; }
    public double DurationSeconds { get; init; }
    public DateTimeOffset? LastWatchedAtUtc { get; init; }
    public DateTimeOffset UpdatedAtUtc { get; init; }

    [JsonIgnore]
    public bool HasWatched => ViewCount > 0 || LastWatchedAtUtc is not null;

    [JsonIgnore]
    public double ProgressRatio =>
        DurationSeconds > 0 && double.IsFinite(DurationSeconds) && double.IsFinite(LastPositionSeconds)
            ? Math.Clamp(LastPositionSeconds / DurationSeconds, 0.0, 1.0)
            : 0.0;

    public static VideoUserStats CreateDefault(string normalizedPath, DateTimeOffset? now = null, string? contentFingerprint = null)
    {
        var timestamp = now ?? DateTimeOffset.UtcNow;
        var computed = contentFingerprint ?? LocalProfileManager.TryComputeFingerprintOnly(normalizedPath);
        return new VideoUserStats
        {
            ContentFingerprint = computed,
            VideoPath = normalizedPath,
            FileName = Path.GetFileName(normalizedPath),
            DirectoryPath = Path.GetDirectoryName(normalizedPath) ?? "",
            ViewCount = 0,
            IsLiked = false,
            Rating = 0,
            LastPositionSeconds = 0,
            DurationSeconds = 0,
            LastWatchedAtUtc = null,
            UpdatedAtUtc = timestamp,
        };
    }
}

/// <summary>
/// Manages password-protected local user profiles and their personal video watch history, view counts,
/// likes, 1..5 star ratings, and custom video lists. Persisted atomically as JSON at <c>%LOCALAPPDATA%\SubtitleAdder\profiles.json</c>.
/// Videos are tracked by a hybrid fast content fingerprint (<c>fp:&lt;length&gt;:&lt;sha256_prefix&gt;</c>) with path fallback,
/// so moving or renaming a video file automatically migrates its history, likes, ratings, and custom list memberships.
/// Passwords are never stored in plaintext; they are salted and hashed with PBKDF2-HMAC-SHA256.
/// </summary>
public sealed class LocalProfileManager
{
    private const int SaltByteLength = 16;
    private const int HashByteLength = 32;
    private const int Pbkdf2Iterations = 100_000;
    private const int FingerprintSliceBytes = 65536;

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private static readonly ConcurrentDictionary<string, CachedFingerprint> FingerprintCache =
        new(StringComparer.OrdinalIgnoreCase);

    private readonly record struct CachedFingerprint(long Length, long LastWriteTicks, string Fingerprint);

    private readonly Lock _sync = new();
    private readonly Dictionary<string, ProfileEntry> _profiles = new(StringComparer.OrdinalIgnoreCase);
    private ProfileEntry? _activeProfile;

    public LocalProfileManager(string? filePath = null)
    {
        FilePath = string.IsNullOrWhiteSpace(filePath) ? DefaultPath : filePath;
        LoadFromDisk();
    }

    /// <summary>Default storage path: <c>%LOCALAPPDATA%\SubtitleAdder\profiles.json</c>.</summary>
    public static string DefaultPath { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "SubtitleAdder",
        "profiles.json");

    /// <summary>Path to the JSON file backing this manager.</summary>
    public string FilePath { get; }

    /// <summary>Raised whenever authentication state, active profile video stats, or custom lists change.</summary>
    public event EventHandler? StateChanged;

    /// <summary>True when a user profile is currently authenticated.</summary>
    public bool IsLoggedIn
    {
        get
        {
            lock (_sync) return _activeProfile is not null;
        }
    }

    /// <summary>Username of the currently logged-in profile, or <c>null</c> when logged out.</summary>
    public string? CurrentUsername
    {
        get
        {
            lock (_sync) return _activeProfile?.Username;
        }
    }

    /// <summary>Last localized authentication error message, or <c>null</c> if the last auth operation succeeded.</summary>
    public string? LastError { get; private set; }

    /// <summary>All registered profile usernames on this machine, sorted alphabetically.</summary>
    public IReadOnlyList<string> RegisteredUsernames
    {
        get
        {
            lock (_sync)
            {
                return _profiles.Values
                    .Select(p => p.Username)
                    .OrderBy(u => u, StringComparer.CurrentCultureIgnoreCase)
                    .ToList();
            }
        }
    }

    /// <summary>Number of watched videos (<c>ViewCount &gt; 0</c>) in the active profile, or 0 when logged out.</summary>
    public int TotalWatchedVideosCount
    {
        get
        {
            lock (_sync)
            {
                if (_activeProfile is null) return 0;
                return _activeProfile.Videos.Values.Count(v => v.ViewCount > 0);
            }
        }
    }

    /// <summary>Sum of all video view counts in the active profile, or 0 when logged out.</summary>
    public int TotalViewsCount
    {
        get
        {
            lock (_sync)
            {
                if (_activeProfile is null) return 0;
                return _activeProfile.Videos.Values.Sum(v => v.ViewCount);
            }
        }
    }

    /// <summary>Number of liked videos in the active profile, or 0 when logged out.</summary>
    public int TotalLikedVideosCount
    {
        get
        {
            lock (_sync)
            {
                if (_activeProfile is null) return 0;
                return _activeProfile.Videos.Values.Count(v => v.IsLiked);
            }
        }
    }

    /// <summary>Number of rated videos (<c>Rating &gt; 0</c>) in the active profile, or 0 when logged out.</summary>
    public int TotalRatedVideosCount
    {
        get
        {
            lock (_sync)
            {
                if (_activeProfile is null) return 0;
                return _activeProfile.Videos.Values.Count(v => v.Rating > 0);
            }
        }
    }

    /// <summary>
    /// Computes a fast hybrid video identifier: if the file exists on disk with <c>Length &gt; 0</c>, reads up to three
    /// 64 KB slices (at offset <c>0</c>, middle, and end) and hashes <c>(length + slices)</c> with SHA-256 to return
    /// <c>"fp:&lt;length&gt;:&lt;24-hex&gt;"</c>. Otherwise falls back to the normalized full path.
    /// </summary>
    public static string ComputeVideoId(string? videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return "";
        return TryComputeFingerprintOnly(normalized) ?? normalized;
    }

    internal static string? TryComputeFingerprintOnly(string normalizedPath)
    {
        if (string.IsNullOrEmpty(normalizedPath)) return null;
        try
        {
            var info = new FileInfo(normalizedPath);
            if (!info.Exists || info.Length <= 0)
            {
                return null;
            }

            var length = info.Length;
            var ticks = info.LastWriteTimeUtc.Ticks;
            if (FingerprintCache.TryGetValue(normalizedPath, out var cached) &&
                cached.Length == length &&
                cached.LastWriteTicks == ticks)
            {
                return cached.Fingerprint;
            }

            long[] offsets =
            [
                0L,
                Math.Max(0L, (length / 2) - (FingerprintSliceBytes / 2)),
                Math.Max(0L, length - FingerprintSliceBytes),
            ];

            using var stream = new FileStream(
                normalizedPath,
                FileMode.Open,
                FileAccess.Read,
                FileShare.ReadWrite | FileShare.Delete);

            using var sha = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
            Span<byte> lengthBytes = stackalloc byte[8];
            BinaryPrimitives.WriteInt64LittleEndian(lengthBytes, length);
            sha.AppendData(lengthBytes);

            var buffer = ArrayPool<byte>.Shared.Rent(FingerprintSliceBytes);
            try
            {
                foreach (var offset in offsets)
                {
                    if (offset >= length) continue;
                    stream.Seek(offset, SeekOrigin.Begin);
                    var toRead = (int)Math.Min(FingerprintSliceBytes, length - offset);
                    var totalRead = 0;
                    while (totalRead < toRead)
                    {
                        var read = stream.Read(buffer, totalRead, toRead - totalRead);
                        if (read <= 0) break;
                        totalRead += read;
                    }
                    if (totalRead > 0)
                    {
                        sha.AppendData(buffer.AsSpan(0, totalRead));
                    }
                }
            }
            finally
            {
                ArrayPool<byte>.Shared.Return(buffer);
            }

            Span<byte> hash = stackalloc byte[32];
            sha.GetHashAndReset(hash);
            var hex = Convert.ToHexStringLower(hash)[..24];
            var fingerprint = string.Create(CultureInfo.InvariantCulture, $"fp:{length}:{hex}");
            FingerprintCache[normalizedPath] = new CachedFingerprint(length, ticks, fingerprint);
            return fingerprint;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or NotSupportedException)
        {
            return null;
        }
    }

    /// <summary>Registers a new local profile with PBKDF2-SHA256 password hashing and logs it in immediately.</summary>
    public bool Register(string username, string password) => Register(username, password, out _);

    /// <summary>Registers a new local profile with PBKDF2-SHA256 password hashing and logs it in immediately.</summary>
    public bool Register(string username, string password, out string? error)
    {
        var cleanUser = (username ?? "").Trim();
        if (cleanUser.Length == 0)
        {
            error = LastError = L("Lütfen bir kullanıcı adı girin.");
            return false;
        }
        if (string.IsNullOrEmpty(password))
        {
            error = LastError = L("Lütfen bir şifre belirleyin.");
            return false;
        }

        lock (_sync)
        {
            if (_profiles.ContainsKey(cleanUser))
            {
                error = LastError = L("Bu kullanıcı adı zaten kayıtlı.");
                return false;
            }

            var salt = RandomNumberGenerator.GetBytes(SaltByteLength);
            var hash = HashPassword(password, salt, Pbkdf2Iterations, HashByteLength);
            var entry = new ProfileEntry
            {
                Username = cleanUser,
                SaltBase64 = Convert.ToBase64String(salt),
                PasswordHashBase64 = Convert.ToBase64String(hash),
                Iterations = Pbkdf2Iterations,
                CreatedAtUtc = DateTimeOffset.UtcNow,
            };

            _profiles[cleanUser] = entry;
            _activeProfile = entry;
            LastError = error = null;
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Authenticates an existing local profile using constant-time PBKDF2 hash comparison.</summary>
    public bool Login(string username, string password) => Login(username, password, out _);

    /// <summary>Authenticates an existing local profile using constant-time PBKDF2 hash comparison.</summary>
    public bool Login(string username, string password, out string? error)
    {
        var cleanUser = (username ?? "").Trim();
        if (cleanUser.Length == 0 || string.IsNullOrEmpty(password))
        {
            error = LastError = L("Kullanıcı adı ve şifre gereklidir.");
            return false;
        }

        lock (_sync)
        {
            if (!_profiles.TryGetValue(cleanUser, out var entry) || !VerifyPassword(password, entry))
            {
                error = LastError = L("Kullanıcı adı veya şifre hatalı.");
                return false;
            }

            _activeProfile = entry;
            LastError = error = null;
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Logs out of the active profile so personal watch history, likes, ratings, and lists are locked.</summary>
    public void Logout()
    {
        var changed = false;
        lock (_sync)
        {
            if (_activeProfile is not null)
            {
                _activeProfile = null;
                changed = true;
            }
            LastError = null;
        }

        if (changed) StateChanged?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>Deletes a profile and all of its watch history/ratings/lists after verifying its password.</summary>
    public bool DeleteProfile(string username, string password) => DeleteProfile(username, password, out _);

    /// <summary>Deletes a profile and all of its watch history/ratings/lists after verifying its password.</summary>
    public bool DeleteProfile(string username, string password, out string? error)
    {
        var cleanUser = (username ?? "").Trim();
        if (cleanUser.Length == 0 || string.IsNullOrEmpty(password))
        {
            error = LastError = L("Kullanıcı adı ve şifre gereklidir.");
            return false;
        }

        lock (_sync)
        {
            if (!_profiles.TryGetValue(cleanUser, out var entry) || !VerifyPassword(password, entry))
            {
                error = LastError = L("Kullanıcı adı veya şifre hatalı.");
                return false;
            }

            _profiles.Remove(cleanUser);
            if (_activeProfile is not null &&
                string.Equals(_activeProfile.Username, entry.Username, StringComparison.OrdinalIgnoreCase))
            {
                _activeProfile = null;
            }
            LastError = error = null;
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>
    /// Records a video view for the currently logged-in profile: increments <see cref="VideoUserStats.ViewCount"/>,
    /// sets <see cref="VideoUserStats.LastWatchedAtUtc"/> to UTC now, updates position/duration, and saves atomically.
    /// Does nothing and returns <c>null</c> when logged out.
    /// </summary>
    public VideoUserStats? RecordWatch(string videoPath, double positionSeconds = 0, double durationSeconds = 0)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return null;

        VideoUserStats updated;
        lock (_sync)
        {
            if (_activeProfile is null) return null;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now);
            var cleanDuration = SanitizeSeconds(durationSeconds, existing.DurationSeconds);
            var cleanPosition = SanitizeSeconds(positionSeconds, existing.LastPositionSeconds);
            if (cleanDuration > 0 && cleanPosition > cleanDuration) cleanPosition = cleanDuration;

            updated = existing with
            {
                ViewCount = existing.ViewCount + 1,
                LastPositionSeconds = cleanPosition,
                DurationSeconds = cleanDuration,
                LastWatchedAtUtc = now,
                UpdatedAtUtc = now,
            };
            _activeProfile.Videos[key] = updated;
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return updated;
    }

    /// <summary>
    /// Updates playback resume position and duration for the active profile without incrementing <see cref="VideoUserStats.ViewCount"/>.
    /// Does nothing and returns <c>null</c> when logged out.
    /// </summary>
    public VideoUserStats? UpdatePlaybackProgress(string videoPath, double positionSeconds, double durationSeconds)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return null;

        VideoUserStats updated;
        lock (_sync)
        {
            if (_activeProfile is null) return null;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now);
            var cleanDuration = SanitizeSeconds(durationSeconds, existing.DurationSeconds);
            var cleanPosition = Math.Max(0, double.IsFinite(positionSeconds) ? positionSeconds : existing.LastPositionSeconds);
            if (cleanDuration > 0 && cleanPosition > cleanDuration) cleanPosition = cleanDuration;

            updated = existing with
            {
                LastPositionSeconds = cleanPosition,
                DurationSeconds = cleanDuration,
                UpdatedAtUtc = now,
            };
            _activeProfile.Videos[key] = updated;
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return updated;
    }

    /// <summary>
    /// Clears watch history (<c>ViewCount = 0</c>, <c>LastWatchedAtUtc = null</c>, <c>LastPositionSeconds = 0</c>)
    /// for the active profile while preserving likes (<c>IsLiked</c>), star ratings (<c>Rating</c>), and custom lists.
    /// </summary>
    public bool ClearWatchHistory()
    {
        lock (_sync)
        {
            if (_activeProfile is null) return false;
            var now = DateTimeOffset.UtcNow;
            var listedPaths = _activeProfile.CustomLists
                .SelectMany(l => l.VideoPaths)
                .ToHashSet(StringComparer.OrdinalIgnoreCase);

            var keys = _activeProfile.Videos.Keys.ToList();
            foreach (var key in keys)
            {
                var current = _activeProfile.Videos[key];
                if (!current.IsLiked && current.Rating <= 0 && !listedPaths.Contains(current.VideoPath))
                {
                    _activeProfile.Videos.Remove(key);
                }
                else
                {
                    _activeProfile.Videos[key] = current with
                    {
                        ViewCount = 0,
                        LastWatchedAtUtc = null,
                        LastPositionSeconds = 0,
                        UpdatedAtUtc = now,
                    };
                }
            }
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Toggles the like state for <paramref name="videoPath"/> in the active profile and returns the new state.</summary>
    public bool ToggleLike(string videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return false;

        bool newLiked;
        lock (_sync)
        {
            if (_activeProfile is null) return false;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now);
            newLiked = !existing.IsLiked;
            _activeProfile.Videos[key] = existing with
            {
                IsLiked = newLiked,
                UpdatedAtUtc = now,
            };
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return newLiked;
    }

    /// <summary>Explicitly sets whether <paramref name="videoPath"/> is liked by the active profile.</summary>
    public VideoUserStats? SetLiked(string videoPath, bool isLiked)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return null;

        VideoUserStats updated;
        lock (_sync)
        {
            if (_activeProfile is null) return null;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now);
            updated = existing with
            {
                IsLiked = isLiked,
                UpdatedAtUtc = now,
            };
            _activeProfile.Videos[key] = updated;
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return updated;
    }

    /// <summary>Sets the star rating (<c>0..5</c>, where 0 clears the rating) for <paramref name="videoPath"/> in the active profile.</summary>
    public VideoUserStats? SetRating(string videoPath, int rating)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return null;
        var clamped = Math.Clamp(rating, 0, 5);

        VideoUserStats updated;
        lock (_sync)
        {
            if (_activeProfile is null) return null;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now);
            updated = existing with
            {
                Rating = clamped,
                UpdatedAtUtc = now,
            };
            _activeProfile.Videos[key] = updated;
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return updated;
    }

    /// <summary>
    /// Returns the active profile's stats for <paramref name="videoPath"/> (automatically migrating the stored path
    /// if the file was moved or renamed on disk and matches by content fingerprint), or <c>null</c> when logged out or untracked.
    /// </summary>
    public VideoUserStats? GetStats(string? videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return null;

        VideoUserStats? result;
        var migrated = false;
        lock (_sync)
        {
            if (_activeProfile is null) return null;
            if (!ResolveAndMigrateVideoKeyLocked(_activeProfile, normalized, out _, out result, out migrated))
            {
                return null;
            }
            if (migrated)
            {
                SaveToDiskLocked();
            }
        }

        if (migrated) StateChanged?.Invoke(this, EventArgs.Empty);
        return result;
    }

    // MARK: - Custom User Lists ("Listelerim / Ayrı Liste Oluşturduklarım")

    /// <summary>Creates a new custom video list in the active profile. Returns <c>null</c> if logged out, empty, or duplicate name.</summary>
    public UserVideoList? CreateCustomList(string name)
    {
        var cleanName = (name ?? "").Trim();
        if (cleanName.Length == 0) return null;

        UserVideoList created;
        lock (_sync)
        {
            if (_activeProfile is null) return null;
            if (_activeProfile.CustomLists.Any(l => string.Equals(l.Name, cleanName, StringComparison.CurrentCultureIgnoreCase)))
            {
                return null;
            }

            var now = DateTimeOffset.UtcNow;
            created = new UserVideoList
            {
                Id = Guid.NewGuid().ToString("N"),
                Name = cleanName,
                VideoPaths = [],
                CreatedAtUtc = now,
                UpdatedAtUtc = now,
            };
            _activeProfile.CustomLists.Add(created);
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return created;
    }

    /// <summary>Renames an existing custom list in the active profile.</summary>
    public bool RenameCustomList(string listId, string newName)
    {
        var cleanName = (newName ?? "").Trim();
        if (string.IsNullOrWhiteSpace(listId) || cleanName.Length == 0) return false;

        lock (_sync)
        {
            if (_activeProfile is null) return false;
            var idx = _activeProfile.CustomLists.FindIndex(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            if (idx < 0) return false;

            if (_activeProfile.CustomLists.Where((_, i) => i != idx)
                .Any(l => string.Equals(l.Name, cleanName, StringComparison.CurrentCultureIgnoreCase)))
            {
                return false;
            }

            var existing = _activeProfile.CustomLists[idx];
            _activeProfile.CustomLists[idx] = existing with
            {
                Name = cleanName,
                UpdatedAtUtc = DateTimeOffset.UtcNow,
            };
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Deletes a custom list by <paramref name="listId"/> from the active profile.</summary>
    public bool DeleteCustomList(string listId)
    {
        if (string.IsNullOrWhiteSpace(listId)) return false;

        lock (_sync)
        {
            if (_activeProfile is null) return false;
            var removed = _activeProfile.CustomLists.RemoveAll(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            if (removed == 0) return false;
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Adds <paramref name="videoPath"/> to the custom list identified by <paramref name="listId"/>.</summary>
    public bool AddVideoToCustomList(string listId, string videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (string.IsNullOrWhiteSpace(listId) || normalized.Length == 0) return false;

        lock (_sync)
        {
            if (_activeProfile is null) return false;
            var idx = _activeProfile.CustomLists.FindIndex(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            if (idx < 0) return false;

            // Ensure video fingerprint is recorded so future file renames/moves migrate this custom list entry too.
            _ = GetOrCreateStatsLocked(_activeProfile, normalized, DateTimeOffset.UtcNow);

            var existing = _activeProfile.CustomLists[idx];
            if (existing.VideoPaths.Any(p => string.Equals(p, normalized, StringComparison.OrdinalIgnoreCase)))
            {
                return true;
            }

            var updatedPaths = new List<string>(existing.VideoPaths) { normalized };
            _activeProfile.CustomLists[idx] = existing with
            {
                VideoPaths = updatedPaths,
                UpdatedAtUtc = DateTimeOffset.UtcNow,
            };
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Removes <paramref name="videoPath"/> from the custom list identified by <paramref name="listId"/>.</summary>
    public bool RemoveVideoFromCustomList(string listId, string videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (string.IsNullOrWhiteSpace(listId) || normalized.Length == 0) return false;

        lock (_sync)
        {
            if (_activeProfile is null) return false;
            _ = ResolveAndMigrateVideoKeyLocked(_activeProfile, normalized, out _, out _, out _);

            var idx = _activeProfile.CustomLists.FindIndex(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            if (idx < 0) return false;

            var existing = _activeProfile.CustomLists[idx];
            var updatedPaths = existing.VideoPaths
                .Where(p => !string.Equals(p, normalized, StringComparison.OrdinalIgnoreCase))
                .ToList();
            if (updatedPaths.Count == existing.VideoPaths.Count) return false;

            _activeProfile.CustomLists[idx] = existing with
            {
                VideoPaths = updatedPaths,
                UpdatedAtUtc = DateTimeOffset.UtcNow,
            };
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Toggles membership of <paramref name="videoPath"/> in the custom list identified by <paramref name="listId"/>. Returns true if now in list.</summary>
    public bool ToggleVideoInCustomList(string listId, string videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (string.IsNullOrWhiteSpace(listId) || normalized.Length == 0) return false;

        bool nowInList;
        lock (_sync)
        {
            if (_activeProfile is null) return false;
            _ = GetOrCreateStatsLocked(_activeProfile, normalized, DateTimeOffset.UtcNow);

            var idx = _activeProfile.CustomLists.FindIndex(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            if (idx < 0) return false;

            var existing = _activeProfile.CustomLists[idx];
            var updatedPaths = new List<string>(existing.VideoPaths);
            var matchIdx = updatedPaths.FindIndex(p => string.Equals(p, normalized, StringComparison.OrdinalIgnoreCase));
            if (matchIdx >= 0)
            {
                updatedPaths.RemoveAt(matchIdx);
                nowInList = false;
            }
            else
            {
                updatedPaths.Add(normalized);
                nowInList = true;
            }

            _activeProfile.CustomLists[idx] = existing with
            {
                VideoPaths = updatedPaths,
                UpdatedAtUtc = DateTimeOffset.UtcNow,
            };
            SaveToDiskLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return nowInList;
    }

    /// <summary>Returns all custom lists created by the active profile, or an empty list when logged out.</summary>
    public IReadOnlyList<UserVideoList> GetCustomLists()
    {
        lock (_sync)
        {
            if (_activeProfile is null) return [];
            return _activeProfile.CustomLists.ToList();
        }
    }

    /// <summary>Returns all custom lists in the active profile that contain <paramref name="videoPath"/>.</summary>
    public IReadOnlyList<UserVideoList> GetCustomListsContaining(string? videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return [];

        lock (_sync)
        {
            if (_activeProfile is null) return [];
            if (ResolveAndMigrateVideoKeyLocked(_activeProfile, normalized, out _, out _, out var migrated) && migrated)
            {
                SaveToDiskLocked();
            }
            return _activeProfile.CustomLists
                .Where(l => l.VideoPaths.Any(p => string.Equals(p, normalized, StringComparison.OrdinalIgnoreCase)))
                .ToList();
        }
    }

    /// <summary>Checks whether <paramref name="videoPath"/> belongs to the custom list <paramref name="listId"/>.</summary>
    public bool IsVideoInCustomList(string listId, string? videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (string.IsNullOrWhiteSpace(listId) || normalized.Length == 0) return false;

        lock (_sync)
        {
            if (_activeProfile is null) return false;
            if (ResolveAndMigrateVideoKeyLocked(_activeProfile, normalized, out _, out _, out var migrated) && migrated)
            {
                SaveToDiskLocked();
            }
            var list = _activeProfile.CustomLists.FirstOrDefault(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            return list is not null && list.VideoPaths.Any(p => string.Equals(p, normalized, StringComparison.OrdinalIgnoreCase));
        }
    }

    /// <summary>
    /// Queries the active profile's video feed filtered by <paramref name="filter"/>, optional <paramref name="searchQuery"/>,
    /// optional <paramref name="additionalDiscoveredPaths"/>, and optional <paramref name="customListId"/>.
    /// Automatically migrates any moved/renamed video files discovered via <paramref name="additionalDiscoveredPaths"/>.
    /// </summary>
    public IReadOnlyList<VideoUserStats> GetFeed(
        VideoFeedFilter filter,
        string? searchQuery = null,
        IEnumerable<string>? additionalDiscoveredPaths = null,
        string? customListId = null)
    {
        lock (_sync)
        {
            var now = DateTimeOffset.UtcNow;

            if (_activeProfile is null)
            {
                if (filter != VideoFeedFilter.All || additionalDiscoveredPaths is null)
                {
                    return [];
                }

                var discoveredOnly = new Dictionary<string, VideoUserStats>(StringComparer.OrdinalIgnoreCase);
                foreach (var rawPath in additionalDiscoveredPaths)
                {
                    var norm = NormalizeVideoPath(rawPath);
                    if (norm.Length > 0 && !discoveredOnly.ContainsKey(norm))
                    {
                        discoveredOnly[norm] = VideoUserStats.CreateDefault(norm, now);
                    }
                }

                IEnumerable<VideoUserStats> anonQuery = discoveredOnly.Values
                    .OrderBy(v => v.FileName, StringComparer.CurrentCultureIgnoreCase);

                if (!string.IsNullOrWhiteSpace(searchQuery))
                {
                    anonQuery = anonQuery.Where(v => MatchesSearchQuery(v, searchQuery));
                }

                return anonQuery.ToList();
            }

            var anyMigrated = false;
            var discoveredNormalized = new List<string>();
            if (additionalDiscoveredPaths is not null)
            {
                foreach (var rawPath in additionalDiscoveredPaths)
                {
                    var norm = NormalizeVideoPath(rawPath);
                    if (norm.Length == 0) continue;
                    discoveredNormalized.Add(norm);
                    if (ResolveAndMigrateVideoKeyLocked(_activeProfile, norm, out _, out _, out var migrated) && migrated)
                    {
                        anyMigrated = true;
                    }
                }
            }

            if (anyMigrated)
            {
                SaveToDiskLocked();
            }

            // Build path-keyed map of all profile videos + discovered videos + custom list videos
            var mergedByPath = new Dictionary<string, VideoUserStats>(StringComparer.OrdinalIgnoreCase);
            foreach (var video in _activeProfile.Videos.Values)
            {
                if (!string.IsNullOrEmpty(video.VideoPath))
                {
                    mergedByPath[video.VideoPath] = video;
                }
            }

            foreach (var norm in discoveredNormalized)
            {
                if (!mergedByPath.ContainsKey(norm))
                {
                    mergedByPath[norm] = VideoUserStats.CreateDefault(norm, now);
                }
            }

            foreach (var list in _activeProfile.CustomLists)
            {
                foreach (var rawPath in list.VideoPaths)
                {
                    var norm = NormalizeVideoPath(rawPath);
                    if (norm.Length > 0 && !mergedByPath.ContainsKey(norm))
                    {
                        mergedByPath[norm] = VideoUserStats.CreateDefault(norm, now);
                    }
                }
            }

            IEnumerable<VideoUserStats> query;

            if (filter == VideoFeedFilter.CustomList)
            {
                var targetList = !string.IsNullOrWhiteSpace(customListId)
                    ? _activeProfile.CustomLists.FirstOrDefault(l => string.Equals(l.Id, customListId, StringComparison.OrdinalIgnoreCase))
                    : _activeProfile.CustomLists.FirstOrDefault();

                if (targetList is null) return [];

                var listItems = new List<VideoUserStats>();
                foreach (var rawPath in targetList.VideoPaths)
                {
                    var norm = NormalizeVideoPath(rawPath);
                    if (norm.Length == 0) continue;
                    listItems.Add(mergedByPath.TryGetValue(norm, out var stats)
                        ? stats
                        : VideoUserStats.CreateDefault(norm, now));
                }
                query = listItems;
            }
            else
            {
                query = mergedByPath.Values;
                query = filter switch
                {
                    VideoFeedFilter.History => query
                        .Where(v => v.ViewCount > 0 || v.LastWatchedAtUtc is not null)
                        .OrderByDescending(v => v.LastWatchedAtUtc ?? v.UpdatedAtUtc)
                        .ThenByDescending(v => v.ViewCount)
                        .ThenBy(v => v.FileName, StringComparer.CurrentCultureIgnoreCase),

                    VideoFeedFilter.Liked => query
                        .Where(v => v.IsLiked)
                        .OrderByDescending(v => v.UpdatedAtUtc)
                        .ThenByDescending(v => v.ViewCount)
                        .ThenBy(v => v.FileName, StringComparer.CurrentCultureIgnoreCase),

                    VideoFeedFilter.MostWatched => query
                        .Where(v => v.ViewCount > 0)
                        .OrderByDescending(v => v.ViewCount)
                        .ThenByDescending(v => v.LastWatchedAtUtc ?? v.UpdatedAtUtc)
                        .ThenBy(v => v.FileName, StringComparer.CurrentCultureIgnoreCase),

                    VideoFeedFilter.TopRated => query
                        .Where(v => v.Rating > 0)
                        .OrderByDescending(v => v.Rating)
                        .ThenByDescending(v => v.ViewCount)
                        .ThenByDescending(v => v.UpdatedAtUtc)
                        .ThenBy(v => v.FileName, StringComparer.CurrentCultureIgnoreCase),

                    _ => query
                        .OrderByDescending(v => v.LastWatchedAtUtc is not null || v.IsLiked || v.Rating > 0)
                        .ThenByDescending(v => v.LastWatchedAtUtc ?? DateTimeOffset.MinValue)
                        .ThenByDescending(v => v.ViewCount)
                        .ThenBy(v => v.FileName, StringComparer.CurrentCultureIgnoreCase),
                };
            }

            if (!string.IsNullOrWhiteSpace(searchQuery))
            {
                query = query.Where(v => MatchesSearchQuery(v, searchQuery));
            }

            return query.ToList();
        }
    }

    private static readonly char[] SearchTokenSeparators = [' ', '\t', '_', '-', '.'];

    /// <summary>
    /// Multi-token case-insensitive search across file name, folder path, and full video path so queries like
    /// <c>"sezon 2 bolum"</c> match <c>"Sezon_2_Bolum_05.mkv"</c> as well as parent folder names.
    /// </summary>
    public static bool MatchesSearchQuery(VideoUserStats video, string? searchQuery)
    {
        var trimmed = (searchQuery ?? "").Trim();
        if (trimmed.Length == 0) return true;

        if (video.FileName.Contains(trimmed, StringComparison.CurrentCultureIgnoreCase) ||
            video.DirectoryPath.Contains(trimmed, StringComparison.CurrentCultureIgnoreCase) ||
            video.VideoPath.Contains(trimmed, StringComparison.CurrentCultureIgnoreCase))
        {
            return true;
        }

        var tokens = trimmed.Split(SearchTokenSeparators, StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        if (tokens.Length == 0) return true;

        foreach (var token in tokens)
        {
            var matched =
                video.FileName.Contains(token, StringComparison.CurrentCultureIgnoreCase) ||
                video.DirectoryPath.Contains(token, StringComparison.CurrentCultureIgnoreCase) ||
                video.VideoPath.Contains(token, StringComparison.CurrentCultureIgnoreCase) ||
                video.FileName.Contains(token, StringComparison.OrdinalIgnoreCase) ||
                video.DirectoryPath.Contains(token, StringComparison.OrdinalIgnoreCase);
            if (!matched) return false;
        }

        return true;
    }

    /// <summary>Normalizes a file path for case-insensitive Windows dictionary lookups.</summary>
    public static string NormalizeVideoPath(string? videoPath)
    {
        var trimmed = (videoPath ?? "").Trim().Trim('"');
        if (trimmed.Length == 0) return "";
        try
        {
            var full = Path.GetFullPath(trimmed);
            return full.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        }
        catch (Exception)
        {
            return trimmed.Replace('/', '\\').TrimEnd('\\');
        }
    }

    /// <summary>
    /// Resolves an existing <see cref="VideoUserStats"/> entry by content fingerprint (<c>fp:...</c>) or normalized path.
    /// If the file was moved or renamed on disk, automatically migrates its <see cref="VideoUserStats.VideoPath"/>
    /// and updates any custom lists referencing the old path.
    /// </summary>
    private static bool ResolveAndMigrateVideoKeyLocked(
        ProfileEntry profile,
        string normalizedPath,
        out string resolvedKey,
        out VideoUserStats? resolvedStats,
        out bool migrated)
    {
        migrated = false;
        var fingerprint = TryComputeFingerprintOnly(normalizedPath);
        var candidateKey = fingerprint ?? normalizedPath;

        string? foundKey = null;
        VideoUserStats? foundStats = null;

        if (fingerprint is not null && profile.Videos.TryGetValue(fingerprint, out var byFpKey))
        {
            foundKey = fingerprint;
            foundStats = byFpKey;
        }
        else if (profile.Videos.TryGetValue(normalizedPath, out var byPathKey))
        {
            foundKey = normalizedPath;
            foundStats = byPathKey;
        }
        else
        {
            foreach (var (k, v) in profile.Videos)
            {
                if (fingerprint is not null &&
                    string.Equals(v.ContentFingerprint, fingerprint, StringComparison.OrdinalIgnoreCase))
                {
                    foundKey = k;
                    foundStats = v;
                    break;
                }
                if (string.Equals(v.VideoPath, normalizedPath, StringComparison.OrdinalIgnoreCase))
                {
                    foundKey = k;
                    foundStats = v;
                    break;
                }
            }
        }

        if (foundKey is null || foundStats is null)
        {
            resolvedKey = candidateKey;
            resolvedStats = null;
            return false;
        }

        var canonicalKey = fingerprint ?? foundStats.ContentFingerprint ?? foundKey;
        var pathChanged = !string.Equals(foundStats.VideoPath, normalizedPath, StringComparison.OrdinalIgnoreCase);
        var keyChanged = !string.Equals(foundKey, canonicalKey, StringComparison.OrdinalIgnoreCase);
        var fpAdded = fingerprint is not null && !string.Equals(foundStats.ContentFingerprint, fingerprint, StringComparison.OrdinalIgnoreCase);

        if (pathChanged || keyChanged || fpAdded)
        {
            var oldPath = foundStats.VideoPath;
            var updated = foundStats with
            {
                ContentFingerprint = fingerprint ?? foundStats.ContentFingerprint,
                VideoPath = normalizedPath,
                FileName = Path.GetFileName(normalizedPath),
                DirectoryPath = Path.GetDirectoryName(normalizedPath) ?? "",
            };

            if (keyChanged)
            {
                profile.Videos.Remove(foundKey);
            }
            profile.Videos[canonicalKey] = updated;

            if (pathChanged && !string.IsNullOrEmpty(oldPath))
            {
                MigrateCustomListPathsLocked(profile, oldPath, normalizedPath);
            }

            migrated = true;
            resolvedKey = canonicalKey;
            resolvedStats = updated;
            return true;
        }

        resolvedKey = foundKey;
        resolvedStats = foundStats;
        return true;
    }

    private static void MigrateCustomListPathsLocked(ProfileEntry profile, string oldPath, string newPath)
    {
        for (var i = 0; i < profile.CustomLists.Count; i++)
        {
            var list = profile.CustomLists[i];
            if (!list.VideoPaths.Any(p => string.Equals(p, oldPath, StringComparison.OrdinalIgnoreCase)))
            {
                continue;
            }

            var updatedPaths = new List<string>(list.VideoPaths.Count);
            var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (var p in list.VideoPaths)
            {
                var replacement = string.Equals(p, oldPath, StringComparison.OrdinalIgnoreCase) ? newPath : p;
                if (seen.Add(replacement))
                {
                    updatedPaths.Add(replacement);
                }
            }

            profile.CustomLists[i] = list with
            {
                VideoPaths = updatedPaths,
                UpdatedAtUtc = DateTimeOffset.UtcNow,
            };
        }
    }

    private static (string Key, VideoUserStats Stats) GetOrCreateStatsLocked(ProfileEntry profile, string normalizedPath, DateTimeOffset now)
    {
        if (ResolveAndMigrateVideoKeyLocked(profile, normalizedPath, out var key, out var existing, out _) && existing is not null)
        {
            return (key, existing);
        }

        var fingerprint = TryComputeFingerprintOnly(normalizedPath);
        var newKey = fingerprint ?? normalizedPath;
        var created = VideoUserStats.CreateDefault(normalizedPath, now, fingerprint);
        profile.Videos[newKey] = created;
        return (newKey, created);
    }

    private static double SanitizeSeconds(double candidate, double fallback)
    {
        if (double.IsFinite(candidate) && candidate > 0) return candidate;
        return double.IsFinite(fallback) && fallback > 0 ? fallback : 0;
    }

    private static byte[] HashPassword(string password, byte[] salt, int iterations, int hashByteLength) =>
        Rfc2898DeriveBytes.Pbkdf2(
            Encoding.UTF8.GetBytes(password),
            salt,
            iterations,
            HashAlgorithmName.SHA256,
            hashByteLength);

    private static bool VerifyPassword(string password, ProfileEntry entry)
    {
        try
        {
            var salt = Convert.FromBase64String(entry.SaltBase64);
            var expectedHash = Convert.FromBase64String(entry.PasswordHashBase64);
            if (salt.Length == 0 || expectedHash.Length == 0) return false;
            var iterations = entry.Iterations > 0 ? entry.Iterations : Pbkdf2Iterations;
            var actualHash = HashPassword(password, salt, iterations, expectedHash.Length);
            return CryptographicOperations.FixedTimeEquals(actualHash, expectedHash);
        }
        catch (FormatException)
        {
            return false;
        }
    }

    private void LoadFromDisk()
    {
        lock (_sync)
        {
            _profiles.Clear();
            try
            {
                if (!File.Exists(FilePath)) return;
                var json = File.ReadAllText(FilePath, Encoding.UTF8);
                var doc = JsonSerializer.Deserialize<ProfilesDocument>(json, JsonOptions);
                if (doc?.Profiles is null) return;
                foreach (var dto in doc.Profiles)
                {
                    if (string.IsNullOrWhiteSpace(dto.Username) ||
                        string.IsNullOrWhiteSpace(dto.SaltBase64) ||
                        string.IsNullOrWhiteSpace(dto.PasswordHashBase64))
                    {
                        continue;
                    }

                    var entry = new ProfileEntry
                    {
                        Username = dto.Username.Trim(),
                        SaltBase64 = dto.SaltBase64,
                        PasswordHashBase64 = dto.PasswordHashBase64,
                        Iterations = dto.Iterations > 0 ? dto.Iterations : Pbkdf2Iterations,
                        CreatedAtUtc = dto.CreatedAtUtc,
                    };

                    if (dto.Videos is not null)
                    {
                        foreach (var video in dto.Videos)
                        {
                            var norm = NormalizeVideoPath(video.VideoPath);
                            if (norm.Length == 0) continue;
                            var fp = !string.IsNullOrWhiteSpace(video.ContentFingerprint)
                                ? video.ContentFingerprint.Trim()
                                : TryComputeFingerprintOnly(norm);
                            var key = fp ?? norm;
                            entry.Videos[key] = video with
                            {
                                ContentFingerprint = fp,
                                VideoPath = norm,
                                FileName = string.IsNullOrWhiteSpace(video.FileName) ? Path.GetFileName(norm) : video.FileName,
                                DirectoryPath = video.DirectoryPath ?? (Path.GetDirectoryName(norm) ?? ""),
                                Rating = Math.Clamp(video.Rating, 0, 5),
                                ViewCount = Math.Max(0, video.ViewCount),
                            };
                        }
                    }

                    if (dto.CustomLists is not null)
                    {
                        foreach (var list in dto.CustomLists)
                        {
                            if (string.IsNullOrWhiteSpace(list.Id) || string.IsNullOrWhiteSpace(list.Name))
                                continue;
                            var normalizedPaths = (list.VideoPaths ?? [])
                                .Select(NormalizeVideoPath)
                                .Where(p => p.Length > 0)
                                .Distinct(StringComparer.OrdinalIgnoreCase)
                                .ToList();
                            entry.CustomLists.Add(list with
                            {
                                Name = list.Name.Trim(),
                                VideoPaths = normalizedPaths,
                            });
                        }
                    }

                    _profiles[entry.Username] = entry;
                }
            }
            catch (Exception ex) when (ex is IOException or JsonException or UnauthorizedAccessException)
            {
                // Corrupt or inaccessible profile store starts clean in memory.
            }
        }
    }

    private void SaveToDiskLocked()
    {
        try
        {
            var dir = Path.GetDirectoryName(FilePath);
            if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);

            var doc = new ProfilesDocument
            {
                Version = 1,
                Profiles = _profiles.Values
                    .OrderBy(p => p.Username, StringComparer.OrdinalIgnoreCase)
                    .Select(p => new ProfileDto
                    {
                        Username = p.Username,
                        SaltBase64 = p.SaltBase64,
                        PasswordHashBase64 = p.PasswordHashBase64,
                        Iterations = p.Iterations,
                        CreatedAtUtc = p.CreatedAtUtc,
                        Videos = p.Videos.Values
                            .OrderBy(v => v.VideoPath, StringComparer.OrdinalIgnoreCase)
                            .ToList(),
                        CustomLists = p.CustomLists.ToList(),
                    })
                    .ToList(),
            };

            var json = JsonSerializer.Serialize(doc, JsonOptions);
            var tempPath = $"{FilePath}.{Guid.NewGuid():N}.tmp";
            File.WriteAllText(tempPath, json, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
            File.Move(tempPath, FilePath, overwrite: true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // Keep changes in memory if disk write is blocked.
        }
    }

    private sealed class ProfileEntry
    {
        public required string Username { get; init; }
        public required string SaltBase64 { get; init; }
        public required string PasswordHashBase64 { get; init; }
        public int Iterations { get; init; } = Pbkdf2Iterations;
        public DateTimeOffset CreatedAtUtc { get; init; }
        public Dictionary<string, VideoUserStats> Videos { get; } = new(StringComparer.OrdinalIgnoreCase);
        public List<UserVideoList> CustomLists { get; } = [];
    }

    private sealed class ProfilesDocument
    {
        public int Version { get; set; } = 1;
        public List<ProfileDto> Profiles { get; set; } = [];
    }

    private sealed class ProfileDto
    {
        public string Username { get; set; } = "";
        public string SaltBase64 { get; set; } = "";
        public string PasswordHashBase64 { get; set; } = "";
        public int Iterations { get; set; } = Pbkdf2Iterations;
        public DateTimeOffset CreatedAtUtc { get; set; }
        public List<VideoUserStats> Videos { get; set; } = [];
        public List<UserVideoList> CustomLists { get; set; } = [];
    }
}
