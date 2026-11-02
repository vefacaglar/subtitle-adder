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
    public byte[]? ThumbnailBytes { get; init; }

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
public sealed class LocalProfileManager : IDisposable
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
    private readonly Dictionary<string, byte[]> _sessionVaultKeys = new(StringComparer.OrdinalIgnoreCase);
    private readonly string _instanceId = Guid.NewGuid().ToString("N");
    private readonly string _mutexName;
    private readonly StateSyncBroker _broker;
    private readonly IDisposable _brokerSubscription;
    private ProfileEntry? _activeProfile;
    private string? _activeUsername;
    private bool _rememberMe = true;
    private string? _lastUsername;
    private string? _rememberedUsername;
    private string? _rememberedProtectedKeyBase64;
    private string? _lastSyncedJson;

    public LocalProfileManager(string? filePath = null, StateSyncBroker? broker = null)
    {
        FilePath = string.IsNullOrWhiteSpace(filePath) ? DefaultPath : Path.GetFullPath(filePath);
        var pathHash = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(FilePath.ToUpperInvariant())))[..16];
        _mutexName = $@"Local\SubtitleAdder.Profiles.{pathHash}";
        _broker = broker ?? StateSyncBroker.Shared;
        LoadFromDisk();
        _brokerSubscription = _broker.Subscribe(StateSyncTopics.All, OnBrokerEvent);
    }

    /// <summary>Underlying Pub/Sub state synchronization broker used by this profile manager.</summary>
    public StateSyncBroker Broker => _broker;

    public void Dispose()
    {
        _brokerSubscription.Dispose();
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

    /// <summary>Whether the user opted to remember their login across app restarts.</summary>
    public bool RememberMe
    {
        get
        {
            lock (_sync) return _rememberMe;
        }
    }

    /// <summary>Most recently logged-in username (for pre-filling the login box).</summary>
    public string? LastUsername
    {
        get
        {
            lock (_sync) return _lastUsername;
        }
    }

    /// <summary>True when a user profile is currently authenticated.</summary>
    public bool IsLoggedIn
    {
        get
        {
            lock (_sync)
            {
                SyncFromDiskLocked();
                return _activeProfile is not null;
            }
        }
    }

    /// <summary>Username of the currently logged-in profile, or <c>null</c> when logged out.</summary>
    public string? CurrentUsername
    {
        get
        {
            lock (_sync)
            {
                SyncFromDiskLocked();
                return _activeProfile?.Username;
            }
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
                SyncFromDiskLocked();
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
                SyncFromDiskLocked();
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
                SyncFromDiskLocked();
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
                SyncFromDiskLocked();
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
                SyncFromDiskLocked();
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

    /// <summary>Registers a new local profile with PBKDF2-SHA256 password hashing and AES-256-GCM vault encryption.</summary>
    public bool Register(string username, string password, bool rememberMe = true) =>
        Register(username, password, out _, rememberMe);

    /// <summary>Registers a new local profile with PBKDF2-SHA256 password hashing and AES-256-GCM vault encryption.</summary>
    public bool Register(string username, string password, out string? error, bool rememberMe = true)
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
            if (_profiles.ContainsKey(cleanUser))
            {
                error = LastError = L("Bu kullanıcı adı zaten kayıtlı.");
                return false;
            }

            var salt = RandomNumberGenerator.GetBytes(SaltByteLength);
            var (authHash, vaultKey) = DeriveAuthHashAndVaultKey(password, salt, Pbkdf2Iterations);
            var entry = new ProfileEntry
            {
                Username = cleanUser,
                SaltBase64 = Convert.ToBase64String(salt),
                PasswordHashBase64 = Convert.ToBase64String(authHash),
                Iterations = Pbkdf2Iterations,
                CreatedAtUtc = DateTimeOffset.UtcNow,
            };

            _sessionVaultKeys[cleanUser] = vaultKey;
            _profiles[cleanUser] = entry;
            _activeProfile = entry;
            _activeUsername = entry.Username;
            _lastUsername = entry.Username;
            _rememberMe = rememberMe;
            if (rememberMe)
            {
                _rememberedUsername = entry.Username;
                _rememberedProtectedKeyBase64 = ProtectVaultKey(vaultKey, salt);
            }
            else
            {
                _rememberedUsername = null;
                _rememberedProtectedKeyBase64 = null;
            }
            LastError = error = null;
            SaveToDiskLocked();
        }

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ProfileLogin, username: cleanUser);
        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Authenticates an existing local profile using constant-time PBKDF2 hash comparison and unlocks its AES-256-GCM vault.</summary>
    public bool Login(string username, string password, bool rememberMe = true) =>
        Login(username, password, out _, rememberMe);

    /// <summary>Authenticates an existing local profile using constant-time PBKDF2 hash comparison and unlocks its AES-256-GCM vault.</summary>
    public bool Login(string username, string password, out string? error, bool rememberMe = true)
    {
        var cleanUser = (username ?? "").Trim();
        if (cleanUser.Length == 0 || string.IsNullOrEmpty(password))
        {
            error = LastError = L("Kullanıcı adı ve şifre gereklidir.");
            return false;
        }

        lock (_sync)
        {
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
            if (!_profiles.TryGetValue(cleanUser, out var entry) ||
                !TryVerifyAndUnlockProfileLocked(password, entry, out var vaultKey))
            {
                error = LastError = L("Kullanıcı adı veya şifre hatalı.");
                return false;
            }

            _sessionVaultKeys[entry.Username] = vaultKey;
            DecryptIntoProfileEntryLocked(entry, vaultKey);
            _activeProfile = entry;
            _activeUsername = entry.Username;
            _lastUsername = entry.Username;
            _rememberMe = rememberMe;
            var saltBytes = Convert.FromBase64String(entry.SaltBase64);
            if (rememberMe)
            {
                _rememberedUsername = entry.Username;
                _rememberedProtectedKeyBase64 = ProtectVaultKey(vaultKey, saltBytes);
            }
            else
            {
                _rememberedUsername = null;
                _rememberedProtectedKeyBase64 = null;
            }
            LastError = error = null;
            SaveToDiskLocked();
        }

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ProfileLogin, username: cleanUser);
        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Logs out of the active profile, clears remembered session keys, and locks all personal data in memory.</summary>
    public void Logout()
    {
        var changed = false;
        lock (_sync)
        {
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
            if (_activeProfile is not null || _activeUsername is not null || _rememberedUsername is not null)
            {
                if (_activeUsername is { } user)
                {
                    _sessionVaultKeys.Remove(user);
                    if (_profiles.TryGetValue(user, out var entry))
                    {
                        // Lock in-memory decrypted vault when logging out
                        entry.Videos.Clear();
                        entry.CustomLists.Clear();
                        entry.Thumbnails.Clear();
                    }
                }
                _activeProfile = null;
                _activeUsername = null;
                _rememberedUsername = null;
                _rememberedProtectedKeyBase64 = null;
                SaveToDiskLocked();
                changed = true;
            }
            LastError = null;
        }

        if (changed)
        {
            PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ProfileLogout);
            StateChanged?.Invoke(this, EventArgs.Empty);
        }
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
            if (!_profiles.TryGetValue(cleanUser, out var entry) ||
                !TryVerifyAndUnlockProfileLocked(password, entry, out _))
            {
                error = LastError = L("Kullanıcı adı veya şifre hatalı.");
                return false;
            }

            _profiles.Remove(cleanUser);
            _sessionVaultKeys.Remove(entry.Username);
            if (_activeProfile is not null &&
                string.Equals(_activeProfile.Username, entry.Username, StringComparison.OrdinalIgnoreCase))
            {
                _activeProfile = null;
                _activeUsername = null;
            }
            if (string.Equals(_rememberedUsername, entry.Username, StringComparison.OrdinalIgnoreCase))
            {
                _rememberedUsername = null;
                _rememberedProtectedKeyBase64 = null;
            }
            LastError = error = null;
            SaveToDiskLocked();
        }

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ProfileDeleted, username: cleanUser);
        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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

        PublishBrokerEvent(
            StateSyncTopics.Profiles,
            StateSyncEventKind.VideoWatched,
            videoPath: normalized,
            viewCount: updated.ViewCount,
            positionSeconds: updated.LastPositionSeconds,
            durationSeconds: updated.DurationSeconds);
        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
            if (_activeProfile is null) return null;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now);
            var cleanDuration = SanitizeSeconds(durationSeconds, existing.DurationSeconds);
            var cleanPosition = Math.Max(0, double.IsFinite(positionSeconds) ? positionSeconds : existing.LastPositionSeconds);
            if (cleanDuration > 0 && cleanPosition > cleanDuration) cleanPosition = cleanDuration;

            var changedSignificantly =
                Math.Abs(cleanPosition - existing.LastPositionSeconds) >= 0.5 ||
                Math.Abs(cleanDuration - existing.DurationSeconds) >= 0.5;

            updated = existing with
            {
                LastPositionSeconds = cleanPosition,
                DurationSeconds = cleanDuration,
                UpdatedAtUtc = changedSignificantly ? now : existing.UpdatedAtUtc,
            };
            _activeProfile.Videos[key] = updated;
            if (changedSignificantly)
            {
                SaveToDiskLocked();
            }
        }

        // Broadcast lightweight watch-progress event to other instances in memory; never invoke StateChanged (zero UI flicker).
        PublishBrokerEvent(
            StateSyncTopics.WatchProgress,
            StateSyncEventKind.ProgressUpdated,
            videoPath: normalized,
            positionSeconds: updated.LastPositionSeconds,
            durationSeconds: updated.DurationSeconds);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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

        PublishBrokerEvent(
            StateSyncTopics.Profiles,
            StateSyncEventKind.VideoWatched,
            videoPath: normalized,
            viewCount: updated.ViewCount,
            positionSeconds: updated.LastPositionSeconds,
            durationSeconds: updated.DurationSeconds);
        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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

        PublishBrokerEvent(
            StateSyncTopics.Profiles,
            StateSyncEventKind.VideoWatched,
            videoPath: normalized,
            viewCount: updated.ViewCount,
            positionSeconds: updated.LastPositionSeconds,
            durationSeconds: updated.DurationSeconds);
        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
            if (_activeProfile is null) return null;
            if (!ResolveAndMigrateVideoKeyLocked(_activeProfile, normalized, out var key, out result, out migrated) || result is null)
            {
                return null;
            }
            if (_activeProfile.Thumbnails.TryGetValue(key, out var b64) && TryDecodeBase64(b64) is { } thumbBytes)
            {
                result = result with { ThumbnailBytes = thumbBytes };
            }
            if (migrated)
            {
                SaveToDiskLocked();
            }
        }

        if (migrated) StateChanged?.Invoke(this, EventArgs.Empty);
        return result;
    }

    /// <summary>
    /// Stores an encrypted video thumbnail image (<paramref name="thumbnailBytes"/>) inside the active profile's
    /// AES-256-GCM vault on disk. Never writes a plaintext image file to disk.
    /// </summary>
    public bool SetThumbnail(string videoPath, byte[] thumbnailBytes)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0 || thumbnailBytes is null || thumbnailBytes.Length == 0)
            return false;

        lock (_sync)
        {
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
            if (_activeProfile is null) return false;
            var (key, _) = GetOrCreateStatsLocked(_activeProfile, normalized, DateTimeOffset.UtcNow);
            var encoded = Convert.ToBase64String(thumbnailBytes);
            if (_activeProfile.Thumbnails.TryGetValue(key, out var existingEncoded) &&
                string.Equals(existingEncoded, encoded, StringComparison.Ordinal))
            {
                return true;
            }
            _activeProfile.Thumbnails[key] = encoded;
            SaveToDiskLocked();
        }

        PublishBrokerEvent(
            StateSyncTopics.Thumbnail,
            StateSyncEventKind.ThumbnailCaptured,
            videoPath: normalized,
            payloadBase64: Convert.ToBase64String(thumbnailBytes));
        return true;
    }

    /// <summary>
    /// Retrieves and decrypts the thumbnail image bytes for <paramref name="videoPath"/> from the active profile's
    /// AES-256-GCM vault, or <c>null</c> when logged out or if no thumbnail has been stored yet.
    /// </summary>
    public byte[]? GetThumbnail(string? videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return null;

        lock (_sync)
        {
            SyncFromDiskLocked();
            if (_activeProfile is null) return null;
            return TryGetThumbnailBytesLocked(_activeProfile, normalized);
        }
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Deletes a custom list by <paramref name="listId"/> from the active profile.</summary>
    public bool DeleteCustomList(string listId)
    {
        if (string.IsNullOrWhiteSpace(listId)) return false;

        lock (_sync)
        {
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
            if (_activeProfile is null) return false;
            var removed = _activeProfile.CustomLists.RemoveAll(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            if (removed == 0) return false;
            SaveToDiskLocked();
        }

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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

        PublishBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged);
        StateChanged?.Invoke(this, EventArgs.Empty);
        return nowInList;
    }

    /// <summary>Returns all custom lists created by the active profile, or an empty list when logged out.</summary>
    public IReadOnlyList<UserVideoList> GetCustomLists()
    {
        lock (_sync)
        {
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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
            using var crossLock = AcquireCrossProcessMutex();
            SyncFromDiskLocked();
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
            foreach (var (key, video) in _activeProfile.Videos)
            {
                if (!string.IsNullOrEmpty(video.VideoPath))
                {
                    var withThumb = _activeProfile.Thumbnails.TryGetValue(key, out var b64) && TryDecodeBase64(b64) is { } tb
                        ? video with { ThumbnailBytes = tb }
                        : video;
                    mergedByPath[video.VideoPath] = withThumb;
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
                if (profile.Thumbnails.Remove(foundKey, out var migratedThumb))
                {
                    profile.Thumbnails[canonicalKey] = migratedThumb;
                }
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

    private static (byte[] AuthHash, byte[] VaultKey) DeriveAuthHashAndVaultKey(string password, byte[] salt, int iterations)
    {
        var derived = Rfc2898DeriveBytes.Pbkdf2(
            Encoding.UTF8.GetBytes(password),
            salt,
            iterations,
            HashAlgorithmName.SHA256,
            HashByteLength * 2);
        return (derived[..HashByteLength], derived[HashByteLength..]);
    }

    private static bool TryVerifyAndUnlockProfileLocked(string password, ProfileEntry entry, out byte[] vaultKey)
    {
        vaultKey = [];
        try
        {
            var salt = Convert.FromBase64String(entry.SaltBase64);
            var expectedHash = Convert.FromBase64String(entry.PasswordHashBase64);
            if (salt.Length == 0 || expectedHash.Length == 0) return false;
            var iterations = entry.Iterations > 0 ? entry.Iterations : Pbkdf2Iterations;
            var (actualHash, candidateVaultKey) = DeriveAuthHashAndVaultKey(password, salt, iterations);
            if (!CryptographicOperations.FixedTimeEquals(actualHash, expectedHash))
            {
                return false;
            }
            vaultKey = candidateVaultKey;
            return true;
        }
        catch (FormatException)
        {
            return false;
        }
    }

    private static string EncryptVault(ProfileSecretVault vault, byte[] vaultKey)
    {
        var plaintext = Encoding.UTF8.GetBytes(JsonSerializer.Serialize(vault, JsonOptions));
        var nonce = RandomNumberGenerator.GetBytes(12);
        var ciphertext = new byte[plaintext.Length];
        var tag = new byte[16];

        using var aes = new AesGcm(vaultKey, 16);
        aes.Encrypt(nonce, plaintext, ciphertext, tag);

        var combined = new byte[12 + 16 + ciphertext.Length];
        Buffer.BlockCopy(nonce, 0, combined, 0, 12);
        Buffer.BlockCopy(tag, 0, combined, 12, 16);
        Buffer.BlockCopy(ciphertext, 0, combined, 28, ciphertext.Length);
        return Convert.ToBase64String(combined);
    }

    private static ProfileSecretVault? DecryptVault(string? encryptedBase64, byte[] vaultKey)
    {
        if (string.IsNullOrWhiteSpace(encryptedBase64) || vaultKey.Length != 32) return null;
        try
        {
            var combined = Convert.FromBase64String(encryptedBase64);
            if (combined.Length < 28) return null;
            var nonce = combined.AsSpan(0, 12);
            var tag = combined.AsSpan(12, 16);
            var ciphertext = combined.AsSpan(28);
            var plaintext = new byte[ciphertext.Length];

            using var aes = new AesGcm(vaultKey, 16);
            aes.Decrypt(nonce, ciphertext, tag, plaintext);
            return JsonSerializer.Deserialize<ProfileSecretVault>(plaintext, JsonOptions);
        }
        catch
        {
            return null;
        }
    }

    private static string ProtectVaultKey(byte[] vaultKey, byte[] salt)
    {
        // Derive a current-OS-user bound wrapping key so the vault key is never stored in plaintext on disk.
        var userEntropy = Encoding.UTF8.GetBytes($"SubtitleAdder.RememberKey.{Environment.UserName}.{Environment.MachineName}");
        var wrapKey = Rfc2898DeriveBytes.Pbkdf2(userEntropy, salt, 10_000, HashAlgorithmName.SHA256, 32);
        var nonce = RandomNumberGenerator.GetBytes(12);
        var cipher = new byte[vaultKey.Length];
        var tag = new byte[16];
        using var aes = new AesGcm(wrapKey, 16);
        aes.Encrypt(nonce, vaultKey, cipher, tag);

        var combined = new byte[12 + 16 + cipher.Length];
        Buffer.BlockCopy(nonce, 0, combined, 0, 12);
        Buffer.BlockCopy(tag, 0, combined, 12, 16);
        Buffer.BlockCopy(cipher, 0, combined, 28, cipher.Length);
        return Convert.ToBase64String(combined);
    }

    private static byte[]? UnprotectVaultKey(string? protectedBase64, byte[] salt)
    {
        if (string.IsNullOrWhiteSpace(protectedBase64)) return null;
        try
        {
            var combined = Convert.FromBase64String(protectedBase64);
            if (combined.Length < 28 + 32) return null;
            var userEntropy = Encoding.UTF8.GetBytes($"SubtitleAdder.RememberKey.{Environment.UserName}.{Environment.MachineName}");
            var wrapKey = Rfc2898DeriveBytes.Pbkdf2(userEntropy, salt, 10_000, HashAlgorithmName.SHA256, 32);
            var nonce = combined.AsSpan(0, 12);
            var tag = combined.AsSpan(12, 16);
            var cipher = combined.AsSpan(28);
            var plain = new byte[cipher.Length];
            using var aes = new AesGcm(wrapKey, 16);
            aes.Decrypt(nonce, cipher, tag, plain);
            return plain.Length == 32 ? plain : null;
        }
        catch
        {
            return null;
        }
    }

    private static byte[]? TryDecodeBase64(string? b64)
    {
        if (string.IsNullOrWhiteSpace(b64)) return null;
        try { return Convert.FromBase64String(b64); }
        catch { return null; }
    }

    private static byte[]? TryGetThumbnailBytesLocked(ProfileEntry profile, string normalizedPath)
    {
        if (ResolveAndMigrateVideoKeyLocked(profile, normalizedPath, out var key, out _, out _) &&
            profile.Thumbnails.TryGetValue(key, out var b64))
        {
            return TryDecodeBase64(b64);
        }
        var fp = TryComputeFingerprintOnly(normalizedPath);
        if (fp is not null && profile.Thumbnails.TryGetValue(fp, out var b64Fp))
        {
            return TryDecodeBase64(b64Fp);
        }
        if (profile.Thumbnails.TryGetValue(normalizedPath, out var b64Path))
        {
            return TryDecodeBase64(b64Path);
        }
        return null;
    }

    private static void DecryptIntoProfileEntryLocked(ProfileEntry entry, byte[] vaultKey)
    {
        if (string.IsNullOrWhiteSpace(entry.EncryptedVaultBase64)) return;
        var vault = DecryptVault(entry.EncryptedVaultBase64, vaultKey);
        if (vault is null) return;

        entry.Videos.Clear();
        entry.CustomLists.Clear();
        entry.Thumbnails.Clear();

        if (vault.Videos is not null)
        {
            foreach (var video in vault.Videos)
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

        if (vault.CustomLists is not null)
        {
            foreach (var list in vault.CustomLists)
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

        if (vault.Thumbnails is not null)
        {
            foreach (var (k, v) in vault.Thumbnails)
            {
                if (!string.IsNullOrWhiteSpace(k) && !string.IsNullOrWhiteSpace(v))
                {
                    entry.Thumbnails[k] = v;
                }
            }
        }
    }

    private void LoadFromDisk()
    {
        lock (_sync)
        {
            SyncFromDiskLocked(force: true);
        }
    }

    private bool SyncFromDiskLocked(bool force = false)
    {
        try
        {
            if (AtomicFileStore.ReadAllTextShared(FilePath) is not { } json)
            {
                if (force)
                {
                    _profiles.Clear();
                    _lastSyncedJson = null;
                }
                return false;
            }

            if (!force && string.Equals(json, _lastSyncedJson, StringComparison.Ordinal))
            {
                return false;
            }

            var doc = JsonSerializer.Deserialize<ProfilesDocument>(json, JsonOptions);
            if (doc?.Profiles is null) return false;

            _profiles.Clear();
            _rememberMe = doc.RememberMe;
            if (!string.IsNullOrWhiteSpace(doc.LastUsername))
            {
                _lastUsername = doc.LastUsername.Trim();
            }
            _rememberedUsername = string.IsNullOrWhiteSpace(doc.RememberedUsername) ? null : doc.RememberedUsername.Trim();
            _rememberedProtectedKeyBase64 = string.IsNullOrWhiteSpace(doc.RememberedProtectedKeyBase64) ? null : doc.RememberedProtectedKeyBase64.Trim();

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
                    EncryptedVaultBase64 = dto.EncryptedVaultBase64,
                };

                // If this session doesn't have the vault key yet, check if the remembered key on disk unlocks this profile
                if (!_sessionVaultKeys.ContainsKey(entry.Username) &&
                    _rememberMe &&
                    string.Equals(_rememberedUsername, entry.Username, StringComparison.OrdinalIgnoreCase) &&
                    !string.IsNullOrWhiteSpace(_rememberedProtectedKeyBase64))
                {
                    try
                    {
                        var saltBytes = Convert.FromBase64String(entry.SaltBase64);
                        if (UnprotectVaultKey(_rememberedProtectedKeyBase64, saltBytes) is { } restoredKey)
                        {
                            _sessionVaultKeys[entry.Username] = restoredKey;
                            if (force && _activeUsername is null)
                            {
                                _activeUsername = entry.Username;
                            }
                        }
                    }
                    catch
                    {
                    }
                }

                if (_sessionVaultKeys.TryGetValue(entry.Username, out var vaultKey))
                {
                    DecryptIntoProfileEntryLocked(entry, vaultKey);
                }

                _profiles[entry.Username] = entry;
            }

            if (_activeUsername is not null)
            {
                _activeProfile = _profiles.TryGetValue(_activeUsername, out var active) ? active : null;
                if (_activeProfile is null) _activeUsername = null;
            }

            _lastSyncedJson = json;
            return true;
        }
        catch (Exception ex) when (ex is IOException or JsonException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    private void SaveToDiskLocked()
    {
        try
        {
            var dir = Path.GetDirectoryName(FilePath);
            if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);

            foreach (var entry in _profiles.Values)
            {
                if (_sessionVaultKeys.TryGetValue(entry.Username, out var vaultKey))
                {
                    var vault = new ProfileSecretVault
                    {
                        Videos = entry.Videos.Values
                            .OrderBy(v => v.VideoPath, StringComparer.OrdinalIgnoreCase)
                            .Select(v => v with { ThumbnailBytes = null })
                            .ToList(),
                        CustomLists = entry.CustomLists.ToList(),
                        Thumbnails = new Dictionary<string, string>(entry.Thumbnails, StringComparer.OrdinalIgnoreCase),
                    };
                    entry.EncryptedVaultBase64 = EncryptVault(vault, vaultKey);
                }
            }

            var doc = new ProfilesDocument
            {
                Version = 2,
                RememberMe = _rememberMe,
                LastUsername = _lastUsername,
                RememberedUsername = _rememberMe ? _rememberedUsername : null,
                RememberedProtectedKeyBase64 = _rememberMe ? _rememberedProtectedKeyBase64 : null,
                Profiles = _profiles.Values
                    .OrderBy(p => p.Username, StringComparer.OrdinalIgnoreCase)
                    .Select(p => new ProfileDto
                    {
                        Username = p.Username,
                        SaltBase64 = p.SaltBase64,
                        PasswordHashBase64 = p.PasswordHashBase64,
                        Iterations = p.Iterations,
                        CreatedAtUtc = p.CreatedAtUtc,
                        EncryptedVaultBase64 = p.EncryptedVaultBase64,
                    })
                    .ToList(),
            };

            var json = JsonSerializer.Serialize(doc, JsonOptions);
            if (AtomicFileStore.WriteAllTextAtomic(FilePath, json))
            {
                _lastSyncedJson = json;
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // Keep changes in memory if disk write is blocked.
        }
    }

    private void PublishBrokerEvent(
        string topic,
        StateSyncEventKind kind,
        string? username = null,
        string? videoPath = null,
        int? viewCount = null,
        double? positionSeconds = null,
        double? durationSeconds = null,
        string? payloadBase64 = null)
    {
        _broker.Publish(topic, new StateSyncEvent
        {
            Topic = topic,
            Kind = kind,
            SenderInstanceId = _instanceId,
            ScopePath = FilePath,
            Username = username ?? _activeUsername,
            VideoPath = videoPath,
            ViewCount = viewCount,
            PositionSeconds = positionSeconds,
            DurationSeconds = durationSeconds,
            PayloadBase64 = payloadBase64,
        });
    }

    private void OnBrokerEvent(StateSyncEvent evt)
    {
        if (string.Equals(evt.SenderInstanceId, _instanceId, StringComparison.Ordinal) ||
            !string.Equals(evt.ScopePath, FilePath, StringComparison.OrdinalIgnoreCase))
        {
            return;
        }

        // Silent in-memory updates for watch-progress and thumbnail topics (never rebuild UI cards!)
        if (string.Equals(evt.Topic, StateSyncTopics.WatchProgress, StringComparison.OrdinalIgnoreCase))
        {
            if (string.IsNullOrWhiteSpace(evt.VideoPath)) return;
            lock (_sync)
            {
                if (_activeProfile is null) return;
                var normalized = NormalizeVideoPath(evt.VideoPath);
                if (normalized.Length == 0) return;
                var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, DateTimeOffset.UtcNow);
                var cleanDuration = SanitizeSeconds(evt.DurationSeconds ?? 0, existing.DurationSeconds);
                var cleanPosition = Math.Max(0, evt.PositionSeconds ?? existing.LastPositionSeconds);
                if (cleanDuration > 0 && cleanPosition > cleanDuration) cleanPosition = cleanDuration;
                _activeProfile.Videos[key] = existing with
                {
                    LastPositionSeconds = cleanPosition,
                    DurationSeconds = cleanDuration,
                    UpdatedAtUtc = evt.TimestampUtc,
                };
            }
            return;
        }

        if (string.Equals(evt.Topic, StateSyncTopics.Thumbnail, StringComparison.OrdinalIgnoreCase))
        {
            if (string.IsNullOrWhiteSpace(evt.VideoPath) || string.IsNullOrWhiteSpace(evt.PayloadBase64)) return;
            lock (_sync)
            {
                if (_activeProfile is null) return;
                var normalized = NormalizeVideoPath(evt.VideoPath);
                if (normalized.Length == 0) return;
                var (key, _) = GetOrCreateStatsLocked(_activeProfile, normalized, DateTimeOffset.UtcNow);
                _activeProfile.Thumbnails[key] = evt.PayloadBase64;
            }
            return;
        }

        if (string.Equals(evt.Topic, StateSyncTopics.Profiles, StringComparison.OrdinalIgnoreCase))
        {
            var updated = false;
            lock (_sync)
            {
                updated = SyncFromDiskLocked(force: false);
            }
            if (updated)
            {
                StateChanged?.Invoke(this, EventArgs.Empty);
            }
        }
    }

    private CrossProcessMutexScope AcquireCrossProcessMutex() => new(_mutexName);

    private readonly struct CrossProcessMutexScope : IDisposable
    {
        private readonly Mutex? _mutex;
        private readonly bool _acquired;

        public CrossProcessMutexScope(string mutexName)
        {
            try
            {
                _mutex = new Mutex(initiallyOwned: false, mutexName);
                try
                {
                    _acquired = _mutex.WaitOne(2000);
                }
                catch (AbandonedMutexException)
                {
                    _acquired = true;
                }
            }
            catch
            {
                _mutex = null;
                _acquired = false;
            }
        }

        public void Dispose()
        {
            if (_mutex is null) return;
            if (_acquired)
            {
                try { _mutex.ReleaseMutex(); } catch { }
            }
            try { _mutex.Dispose(); } catch { }
        }
    }

    private sealed class ProfileEntry
    {
        public required string Username { get; init; }
        public required string SaltBase64 { get; init; }
        public required string PasswordHashBase64 { get; init; }
        public int Iterations { get; init; } = Pbkdf2Iterations;
        public DateTimeOffset CreatedAtUtc { get; init; }
        public string? EncryptedVaultBase64 { get; set; }
        public Dictionary<string, VideoUserStats> Videos { get; } = new(StringComparer.OrdinalIgnoreCase);
        public List<UserVideoList> CustomLists { get; } = [];
        public Dictionary<string, string> Thumbnails { get; } = new(StringComparer.OrdinalIgnoreCase);
    }

    private sealed class ProfileSecretVault
    {
        public List<VideoUserStats> Videos { get; set; } = [];
        public List<UserVideoList> CustomLists { get; set; } = [];
        public Dictionary<string, string> Thumbnails { get; set; } = new(StringComparer.OrdinalIgnoreCase);
    }

    private sealed class ProfilesDocument
    {
        public int Version { get; set; } = 2;
        public bool RememberMe { get; set; } = true;
        public string? LastUsername { get; set; }
        public string? RememberedUsername { get; set; }
        public string? RememberedProtectedKeyBase64 { get; set; }
        public List<ProfileDto> Profiles { get; set; } = [];
    }

    private sealed class ProfileDto
    {
        public string Username { get; set; } = "";
        public string SaltBase64 { get; set; } = "";
        public string PasswordHashBase64 { get; set; } = "";
        public int Iterations { get; set; } = Pbkdf2Iterations;
        public DateTimeOffset CreatedAtUtc { get; set; }
        public string? EncryptedVaultBase64 { get; set; }
    }
}
