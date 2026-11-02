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

    public static VideoUserStats CreateDefault(
        string normalizedPath,
        DateTimeOffset? now = null,
        string? contentFingerprint = null,
        bool computeFingerprintFromDisk = true)
    {
        var timestamp = now ?? DateTimeOffset.UtcNow;
        var computed = contentFingerprint ?? (computeFingerprintFromDisk
            ? LocalProfileManager.TryComputeFingerprintOnly(normalizedPath)
            : LocalProfileManager.TryGetCachedFingerprint(normalizedPath));
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
/// likes, 1..5 star ratings, thumbnails, and custom video lists.
/// Backed by normalized AES-256-GCM encrypted SQLite WAL tables (<c>profiles.db</c>) with an asynchronous
/// <see cref="System.Threading.Channels.Channel{T}"/> write queue and a compact auth header (<c>profiles.json</c>).
/// Videos are tracked by a hybrid fast content fingerprint (<c>fp:&lt;length&gt;:&lt;sha256_prefix&gt;</c>) with path fallback,
/// so moving or renaming a video file automatically migrates its history, likes, ratings, thumbnails, and custom list memberships.
/// </summary>
public sealed class LocalProfileManager : IDisposable
{
    private const int SaltByteLength = 16;
    private const int HashByteLength = 32;
    private const int Pbkdf2Iterations = 100_000;
    private const int FingerprintSliceBytes = 65536;
    private const string SqliteVaultMarkerPlaintext = "{\"storage\":\"sqlite-v3\"}";

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private static readonly JsonSerializerOptions CompactJsonOptions = new()
    {
        WriteIndented = false,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private static readonly ConcurrentDictionary<string, CachedFingerprint> FingerprintCache =
        new(StringComparer.OrdinalIgnoreCase);

    private static readonly ConcurrentDictionary<string, long> GlobalFileWriteGeneration =
        new(StringComparer.OrdinalIgnoreCase);

    private readonly record struct CachedFingerprint(long Length, long LastWriteTicks, string Fingerprint);

    private readonly Lock _sync = new();
    private readonly Dictionary<string, ProfileEntry> _profiles = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, byte[]> _sessionVaultKeys = new(StringComparer.OrdinalIgnoreCase);
    private readonly HashSet<string> _processedRequestIds = new(StringComparer.Ordinal);
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
    private long _lastSyncedGeneration = -1;
    private volatile bool _dirtyFromExternalBroker;

    public LocalProfileManager(string? filePath = null, StateSyncBroker? broker = null)
    {
        FilePath = string.IsNullOrWhiteSpace(filePath) ? DefaultPath : Path.GetFullPath(filePath);
        SqliteDbPath = Path.ChangeExtension(FilePath, ".db");
        BackupFilePath = FilePath + ".bak";
        var pathHash = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(FilePath.ToUpperInvariant())))[..16];
        _mutexName = $@"Local\SubtitleAdder.Profiles.{pathHash}";
        _broker = broker ?? StateSyncBroker.Shared;
        _ = _broker.GetDataWorkerStore(SqliteDbPath);
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

    /// <summary>Path to the compact auth header JSON file backing this manager.</summary>
    public string FilePath { get; }

    /// <summary>Path to the SQLite WAL database (<c>profiles.db</c>) storing normalized encrypted tables.</summary>
    public string SqliteDbPath { get; }

    /// <summary>Path to the verified backup (<c>profiles.json.bak</c>) created before legacy vault migration.</summary>
    public string BackupFilePath { get; }

    internal SqliteProfileStore SqlStore => _broker.GetDataWorkerStore(SqliteDbPath);

    /// <summary>Number of SQLite write operations currently queued in the background channel.</summary>
    public int PendingSqliteWriteCount => SqlStore.PendingWriteCount;

    /// <summary>Total number of coalesced SQLite write batches committed by the background worker.</summary>
    public long TotalSqliteBatchesCommitted => SqlStore.TotalBatchesCommitted;

    /// <summary>Total number of SQLite write operations processed by the background worker.</summary>
    public long TotalSqliteOperationsCommitted => SqlStore.TotalOperationsCommitted;

    /// <summary>Total number of coalesced <c>watch_progress</c> table upserts committed to SQLite.</summary>
    public long TotalWatchProgressSqliteUpserts => SqlStore.TotalWatchProgressUpserts;

    /// <summary>Last storage write error observed by the SQLite background writer, or <c>null</c> when healthy.</summary>
    public string? LastStorageWriteError => SqlStore.LastWriteError;

    /// <summary>Flushes all pending asynchronous SQLite writes on the background channel.</summary>
    public Task<bool> FlushPendingWritesAsync() => _broker.FlushPendingWritesAsync(SqliteDbPath);

    /// <summary>Synchronously waits for all queued SQLite writes to commit.</summary>
    public bool FlushPendingWrites(TimeSpan? timeout = null) => _broker.FlushPendingWrites(SqliteDbPath, timeout);

    /// <summary>Returns row counts across the 5 normalized encrypted SQLite tables for the active user.</summary>
    public (long Videos, long WatchProgress, long CustomLists, long CustomListItems, long Thumbnails) GetSqliteTableRowCounts()
    {
        string? username;
        lock (_sync)
        {
            username = _activeProfile?.Username;
        }
        return string.IsNullOrEmpty(username) ? (0, 0, 0, 0, 0) : SqlStore.GetTableRowCounts(username);
    }

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
            SqlStore.TryDrainWhenHealthyNonBlocking();
            lock (_sync)
            {
                EnsureSyncedIfChangedLocked();
                return _lastUsername;
            }
        }
    }

    /// <summary>True when a user profile is currently authenticated.</summary>
    public bool IsLoggedIn
    {
        get
        {
            SqlStore.TryDrainWhenHealthyNonBlocking();
            lock (_sync)
            {
                EnsureSyncedIfChangedLocked();
                return _activeProfile is not null;
            }
        }
    }

    /// <summary>Username of the currently logged-in profile, or <c>null</c> when logged out.</summary>
    public string? CurrentUsername
    {
        get
        {
            SqlStore.TryDrainWhenHealthyNonBlocking();
            lock (_sync)
            {
                EnsureSyncedIfChangedLocked();
                return _activeProfile?.Username;
            }
        }
    }

    /// <summary>
    /// Returns the active authenticated profile's username and a defensive copy of its 256-bit AES-GCM vault key.
    /// Returns <c>false</c> when no profile is logged in or when the profile's vault key is not unlocked in this session.
    /// </summary>
    public bool TryGetActiveProfileVaultKey(out string username, out byte[] vaultKey)
    {
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is not null &&
                _sessionVaultKeys.TryGetValue(_activeProfile.Username, out var key) &&
                key.Length == 32)
            {
                username = _activeProfile.Username;
                vaultKey = (byte[])key.Clone();
                return true;
            }

            username = "";
            vaultKey = [];
            return false;
        }
    }

    /// <summary>Last localized authentication error message, or <c>null</c> if the last auth operation succeeded.</summary>
    public string? LastError { get; private set; }

    /// <summary>All registered profile usernames on this machine, sorted alphabetically.</summary>
    public IReadOnlyList<string> RegisteredUsernames
    {
        get
        {
            SqlStore.TryDrainWhenHealthyNonBlocking();
            lock (_sync)
            {
                EnsureSyncedIfChangedLocked();
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
            SqlStore.TryDrainWhenHealthyNonBlocking();
            lock (_sync)
            {
                EnsureSyncedIfChangedLocked();
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
            SqlStore.TryDrainWhenHealthyNonBlocking();
            lock (_sync)
            {
                EnsureSyncedIfChangedLocked();
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
            SqlStore.TryDrainWhenHealthyNonBlocking();
            lock (_sync)
            {
                EnsureSyncedIfChangedLocked();
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
            SqlStore.TryDrainWhenHealthyNonBlocking();
            lock (_sync)
            {
                EnsureSyncedIfChangedLocked();
                if (_activeProfile is null) return 0;
                return _activeProfile.Videos.Values.Count(v => v.Rating > 0);
            }
        }
    }

    /// <summary>
    /// Optional custom storage availability predicate for testing or custom volume filtering.
    /// When set, a video path is considered available only if both its drive root is ready and this predicate returns <c>true</c>.
    /// </summary>
    public Func<string, bool>? StorageAvailabilityFilter { get; set; }

    /// <summary>
    /// Checks whether the drive/volume hosting <paramref name="videoPath"/> is currently connected and ready,
    /// without spinning up or seeking individual files on mechanical HDDs.
    /// </summary>
    public bool IsVideoStorageAvailable(string? videoPath) =>
        IsVideoStorageAvailableCore(NormalizeVideoPath(videoPath), rootCache: null);

    private bool IsVideoStorageAvailableCore(string normalizedPath, Dictionary<string, bool>? rootCache)
    {
        if (string.IsNullOrWhiteSpace(normalizedPath)) return false;

        if (StorageAvailabilityFilter is { } customFilter && !customFilter(normalizedPath))
        {
            return false;
        }

        try
        {
            var root = Path.GetPathRoot(normalizedPath);
            if (string.IsNullOrWhiteSpace(root)) return true;

            if (rootCache is not null && rootCache.TryGetValue(root, out var cachedReady))
            {
                return cachedReady;
            }

            bool isReady;
            if (root.StartsWith(@"\\", StringComparison.Ordinal))
            {
                isReady = Directory.Exists(root);
            }
            else
            {
                var drive = new DriveInfo(root);
                isReady = drive.IsReady;
            }

            rootCache?.TryAdd(root, isReady);
            return isReady;
        }
        catch
        {
            return false;
        }
    }

    internal static string? TryGetCachedFingerprint(string normalizedPath) =>
        !string.IsNullOrEmpty(normalizedPath) && FingerprintCache.TryGetValue(normalizedPath, out var cached)
            ? cached.Fingerprint
            : null;

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

        SqlStore.TryDrainWhenHealthyNonBlocking();
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
            var createdAt = DateTimeOffset.UtcNow;
            var entry = new ProfileEntry
            {
                Username = cleanUser,
                SaltBase64 = Convert.ToBase64String(salt),
                PasswordHashBase64 = Convert.ToBase64String(authHash),
                Iterations = Pbkdf2Iterations,
                CreatedAtUtc = createdAt,
                EncryptedVaultBase64 = EncryptVaultMarker(vaultKey),
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
            SaveAuthHeaderToDiskLocked();
            _broker.SubmitWriteRequest(new StateSyncWriteRequest
            {
                RequestId = Guid.NewGuid().ToString("N"),
                SenderInstanceId = _instanceId,
                DbPath = SqliteDbPath,
                Kind = nameof(SqliteProfileStore.WriteKind.UpsertProfile),
                Username = entry.Username,
                SaltBase64 = entry.SaltBase64,
                PasswordHashBase64 = entry.PasswordHashBase64,
                Iterations = entry.Iterations,
                CreatedAtUtc = entry.CreatedAtUtc,
                PostCommitEvents =
                [
                    CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ProfileLogin, username: cleanUser),
                    CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged),
                ],
            });
        }

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

        SqlStore.TryDrainWhenHealthyNonBlocking();
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
            SaveAuthHeaderToDiskLocked();
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
        SqlStore.TryDrainWhenHealthyNonBlocking();
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
                        entry.Videos.Clear();
                        entry.CustomLists.Clear();
                        entry.DecodedThumbnails.Clear();
                        entry.MissingThumbnails.Clear();
                    }
                }
                _activeProfile = null;
                _activeUsername = null;
                _rememberedUsername = null;
                _rememberedProtectedKeyBase64 = null;
                SaveAuthHeaderToDiskLocked();
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

        SqlStore.TryDrainWhenHealthyNonBlocking();
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
            SaveAuthHeaderToDiskLocked();
            _broker.SubmitWriteRequest(new StateSyncWriteRequest
            {
                RequestId = Guid.NewGuid().ToString("N"),
                SenderInstanceId = _instanceId,
                DbPath = SqliteDbPath,
                Kind = nameof(SqliteProfileStore.WriteKind.DeleteProfileCascade),
                Username = entry.Username,
                PostCommitEvents =
                [
                    CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ProfileDeleted, username: cleanUser),
                    CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged),
                ],
            });
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>
    /// Records a video view for the currently logged-in profile: increments <see cref="VideoUserStats.ViewCount"/>,
    /// sets <see cref="VideoUserStats.LastWatchedAtUtc"/> to UTC now, updates position/duration, and enqueues
    /// an encrypted SQLite row update on the centralized background write worker with an idempotency <paramref name="requestId"/>.
    /// Does nothing and returns <c>null</c> when logged out.
    /// </summary>
    public VideoUserStats? RecordWatch(string videoPath, double positionSeconds = 0, double durationSeconds = 0, string? requestId = null)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return null;

        var reqId = string.IsNullOrWhiteSpace(requestId) ? Guid.NewGuid().ToString("N") : requestId.Trim();
        VideoUserStats updated;
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return null;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now);

            if (!string.IsNullOrWhiteSpace(requestId) &&
                (!_processedRequestIds.Add(reqId) || SqlStore.IsRequestProcessed(reqId)))
            {
                _processedRequestIds.Add(reqId);
                EnqueueVideoAndProgressUpsertLocked(
                    _activeProfile.Username,
                    key,
                    existing,
                    includeProgress: false,
                    requestId: reqId,
                    viewCountDelta: 1,
                    postCommitEvents: null);
                return existing;
            }

            _processedRequestIds.Add(reqId);
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
            EnqueueVideoAndProgressUpsertLocked(
                _activeProfile.Username,
                key,
                updated,
                includeProgress: cleanPosition > 0 || cleanDuration > 0,
                requestId: reqId,
                viewCountDelta: 1,
                postCommitEvents:
                [
                    CreateBrokerEvent(
                        StateSyncTopics.Profiles,
                        StateSyncEventKind.VideoWatched,
                        videoPath: normalized,
                        viewCount: updated.ViewCount,
                        positionSeconds: updated.LastPositionSeconds,
                        durationSeconds: updated.DurationSeconds),
                    CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged),
                ]);
            BumpInMemoryGenerationLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return updated;
    }

    /// <summary>
    /// Updates playback resume position and duration for the active profile without incrementing <see cref="VideoUserStats.ViewCount"/>.
    /// Enqueues a coalesced update to the dedicated <c>watch_progress</c> SQLite table without touching the <c>videos</c> table.
    /// Does nothing and returns <c>null</c> when logged out.
    /// </summary>
    public VideoUserStats? UpdatePlaybackProgress(string videoPath, double positionSeconds, double durationSeconds)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return null;

        VideoUserStats updated;
        var changedSignificantly = false;
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return null;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now, persistNewVideoRowIfCreated: true);
            var cleanDuration = SanitizeSeconds(durationSeconds, existing.DurationSeconds);
            var cleanPosition = Math.Max(0, double.IsFinite(positionSeconds) ? positionSeconds : existing.LastPositionSeconds);
            if (cleanDuration > 0 && cleanPosition > cleanDuration) cleanPosition = cleanDuration;

            changedSignificantly =
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
                EnqueueWatchProgressUpsertOnlyLocked(
                    _activeProfile.Username,
                    key,
                    updated,
                    postCommitEvents:
                    [
                        CreateBrokerEvent(
                            StateSyncTopics.WatchProgress,
                            StateSyncEventKind.ProgressUpdated,
                            videoPath: normalized,
                            positionSeconds: updated.LastPositionSeconds,
                            durationSeconds: updated.DurationSeconds),
                    ]);
                BumpInMemoryGenerationLocked();
            }
        }

        if (!changedSignificantly)
        {
            // Broadcast lightweight watch-progress event to other instances in memory; never invoke StateChanged (zero UI flicker).
            PublishBrokerEvent(
                StateSyncTopics.WatchProgress,
                StateSyncEventKind.ProgressUpdated,
                videoPath: normalized,
                positionSeconds: updated.LastPositionSeconds,
                durationSeconds: updated.DurationSeconds);
        }
        return updated;
    }

    /// <summary>
    /// Clears watch history (<c>ViewCount = 0</c>, <c>LastWatchedAtUtc = null</c>, <c>LastPositionSeconds = 0</c>)
    /// for the active profile while preserving likes (<c>IsLiked</c>), star ratings (<c>Rating</c>), and custom lists.
    /// </summary>
    public bool ClearWatchHistory()
    {
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
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

            if (_sessionVaultKeys.TryGetValue(_activeProfile.Username, out var vaultKey))
            {
                var remainingVideos = new List<StateSyncBatchBlobDto>(_activeProfile.Videos.Count);
                var remainingProgress = new List<StateSyncBatchBlobDto>();
                foreach (var (k, v) in _activeProfile.Videos)
                {
                    remainingVideos.Add(new StateSyncBatchBlobDto(
                        k,
                        EncryptVideoRowBlob(v, vaultKey),
                        v.UpdatedAtUtc.UtcTicks));
                    if (v.LastPositionSeconds > 0 || v.DurationSeconds > 0)
                    {
                        remainingProgress.Add(new StateSyncBatchBlobDto(
                            k,
                            EncryptWatchProgressRowBlob(v, vaultKey),
                            v.UpdatedAtUtc.UtcTicks));
                    }
                }
                _broker.SubmitWriteRequest(new StateSyncWriteRequest
                {
                    RequestId = Guid.NewGuid().ToString("N"),
                    SenderInstanceId = _instanceId,
                    DbPath = SqliteDbPath,
                    Kind = nameof(SqliteProfileStore.WriteKind.ReplaceAllVideosAndProgress),
                    Username = _activeProfile.Username,
                    BatchVideos = remainingVideos,
                    BatchProgress = remainingProgress,
                    PostCommitEvents = [CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged)],
                });
            }
            BumpInMemoryGenerationLocked();
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
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return false;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now);
            newLiked = !existing.IsLiked;
            var updated = existing with
            {
                IsLiked = newLiked,
                UpdatedAtUtc = now,
            };
            _activeProfile.Videos[key] = updated;
            EnqueueVideoAndProgressUpsertLocked(
                _activeProfile.Username,
                key,
                updated,
                includeProgress: false,
                postCommitEvents: [CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged)]);
            BumpInMemoryGenerationLocked();
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
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return null;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now);
            updated = existing with
            {
                IsLiked = isLiked,
                UpdatedAtUtc = now,
            };
            _activeProfile.Videos[key] = updated;
            EnqueueVideoAndProgressUpsertLocked(
                _activeProfile.Username,
                key,
                updated,
                includeProgress: false,
                postCommitEvents:
                [
                    CreateBrokerEvent(
                        StateSyncTopics.Profiles,
                        StateSyncEventKind.VideoWatched,
                        videoPath: normalized,
                        viewCount: updated.ViewCount,
                        positionSeconds: updated.LastPositionSeconds,
                        durationSeconds: updated.DurationSeconds),
                    CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged),
                ]);
            BumpInMemoryGenerationLocked();
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
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return null;
            var now = DateTimeOffset.UtcNow;
            var (key, existing) = GetOrCreateStatsLocked(_activeProfile, normalized, now);
            updated = existing with
            {
                Rating = clamped,
                UpdatedAtUtc = now,
            };
            _activeProfile.Videos[key] = updated;
            EnqueueVideoAndProgressUpsertLocked(
                _activeProfile.Username,
                key,
                updated,
                includeProgress: false,
                postCommitEvents:
                [
                    CreateBrokerEvent(
                        StateSyncTopics.Profiles,
                        StateSyncEventKind.VideoWatched,
                        videoPath: normalized,
                        viewCount: updated.ViewCount,
                        positionSeconds: updated.LastPositionSeconds,
                        durationSeconds: updated.DurationSeconds),
                    CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged),
                ]);
            BumpInMemoryGenerationLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return updated;
    }

    /// <summary>
    /// Performs a fast, purely in-memory stats lookup for <paramref name="videoPath"/> on the active profile
    /// without any disk I/O, <see cref="FileInfo"/> checks, thumbnail BLOB queries, or SQLite flushes.
    /// Designed for UI-thread calls such as <see cref="VideoPlatformViewModel.SetActiveVideoImmediate"/>.
    /// </summary>
    public VideoUserStats? GetStatsFastInMemory(string? videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return null;

        lock (_sync)
        {
            if (_activeProfile is null) return null;

            if (_activeProfile.Videos.TryGetValue(normalized, out var directByPath))
            {
                return directByPath;
            }

            if (TryGetCachedFingerprint(normalized) is { } cachedFp &&
                _activeProfile.Videos.TryGetValue(cachedFp, out var directByCachedFp) &&
                string.Equals(directByCachedFp.VideoPath, normalized, StringComparison.OrdinalIgnoreCase))
            {
                return directByCachedFp;
            }

            foreach (var v in _activeProfile.Videos.Values)
            {
                if (string.Equals(v.VideoPath, normalized, StringComparison.OrdinalIgnoreCase))
                {
                    return v;
                }
            }

            return null;
        }
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
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return null;
            if (!ResolveAndMigrateVideoKeyLocked(_activeProfile, normalized, out var key, out result, out migrated) || result is null)
            {
                return null;
            }
            if (TryGetThumbnailBytesByKeyLocked(_activeProfile, key, normalized) is { } thumbBytes)
            {
                result = result with { ThumbnailBytes = thumbBytes };
            }
            if (migrated)
            {
                BumpInMemoryGenerationLocked();
            }
        }

        if (migrated) StateChanged?.Invoke(this, EventArgs.Empty);
        return result;
    }

    /// <summary>
    /// Stores an encrypted video thumbnail image (<paramref name="thumbnailBytes"/>) as a binary AES-256-GCM <c>BLOB</c>
    /// inside the <c>thumbnails</c> SQLite table via the background write queue. Never writes plaintext or Base64 to disk.
    /// </summary>
    public bool SetThumbnail(string videoPath, byte[] thumbnailBytes)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0 || thumbnailBytes is null || thumbnailBytes.Length == 0)
            return false;

        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return false;
            var (key, _) = GetOrCreateStatsLocked(_activeProfile, normalized, DateTimeOffset.UtcNow, persistNewVideoRowIfCreated: true);
            if (_activeProfile.DecodedThumbnails.TryGetValue(key, out var existingBytes) &&
                existingBytes.AsSpan().SequenceEqual(thumbnailBytes))
            {
                return true;
            }

            var copy = (byte[])thumbnailBytes.Clone();
            _activeProfile.DecodedThumbnails[key] = copy;
            _activeProfile.MissingThumbnails.Remove(key);
            if (_sessionVaultKeys.TryGetValue(_activeProfile.Username, out var vaultKey))
            {
                var encryptedBlob = EncryptThumbnailBlob(copy, vaultKey);
                _broker.SubmitWriteRequest(new StateSyncWriteRequest
                {
                    RequestId = Guid.NewGuid().ToString("N"),
                    SenderInstanceId = _instanceId,
                    DbPath = SqliteDbPath,
                    Kind = nameof(SqliteProfileStore.WriteKind.UpsertThumbnail),
                    Username = _activeProfile.Username,
                    Key = key,
                    Blob = encryptedBlob,
                    UpdatedTicks = DateTimeOffset.UtcNow.UtcTicks,
                    PostCommitEvents =
                    [
                        CreateBrokerEvent(
                            StateSyncTopics.Thumbnail,
                            StateSyncEventKind.ThumbnailCaptured,
                            videoPath: normalized,
                            payloadBase64: Convert.ToBase64String(thumbnailBytes)),
                    ],
                });
            }
            BumpInMemoryGenerationLocked();
        }

        return true;
    }

    /// <summary>
    /// Retrieves and decrypts the thumbnail image bytes for <paramref name="videoPath"/> from the active profile's
    /// in-memory cache or encrypted SQLite <c>thumbnails</c> table, or <c>null</c> when logged out or if no thumbnail is stored.
    /// </summary>
    public byte[]? GetThumbnail(string? videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return null;

        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
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
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
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
            EnqueueCustomListHeaderUpsertLocked(
                _activeProfile.Username,
                created,
                postCommitEvents: [CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged)]);
            BumpInMemoryGenerationLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return created;
    }

    /// <summary>Renames an existing custom list in the active profile.</summary>
    public bool RenameCustomList(string listId, string newName)
    {
        var cleanName = (newName ?? "").Trim();
        if (string.IsNullOrWhiteSpace(listId) || cleanName.Length == 0) return false;

        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return false;
            var idx = _activeProfile.CustomLists.FindIndex(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            if (idx < 0) return false;

            if (_activeProfile.CustomLists.Where((_, i) => i != idx)
                .Any(l => string.Equals(l.Name, cleanName, StringComparison.CurrentCultureIgnoreCase)))
            {
                return false;
            }

            var existing = _activeProfile.CustomLists[idx];
            var updated = existing with
            {
                Name = cleanName,
                UpdatedAtUtc = DateTimeOffset.UtcNow,
            };
            _activeProfile.CustomLists[idx] = updated;
            EnqueueCustomListHeaderUpsertLocked(
                _activeProfile.Username,
                updated,
                postCommitEvents: [CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged)]);
            BumpInMemoryGenerationLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Deletes a custom list by <paramref name="listId"/> from the active profile.</summary>
    public bool DeleteCustomList(string listId)
    {
        if (string.IsNullOrWhiteSpace(listId)) return false;

        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return false;
            var removed = _activeProfile.CustomLists.RemoveAll(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            if (removed == 0) return false;
            _broker.SubmitWriteRequest(new StateSyncWriteRequest
            {
                RequestId = Guid.NewGuid().ToString("N"),
                SenderInstanceId = _instanceId,
                DbPath = SqliteDbPath,
                Kind = nameof(SqliteProfileStore.WriteKind.DeleteCustomList),
                Username = _activeProfile.Username,
                Key = listId,
                PostCommitEvents = [CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged)],
            });
            BumpInMemoryGenerationLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Adds <paramref name="videoPath"/> to the custom list identified by <paramref name="listId"/>.</summary>
    public bool AddVideoToCustomList(string listId, string videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (string.IsNullOrWhiteSpace(listId) || normalized.Length == 0) return false;

        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return false;
            var idx = _activeProfile.CustomLists.FindIndex(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            if (idx < 0) return false;

            // Ensure video fingerprint is recorded so future file renames/moves migrate this custom list entry too.
            _ = GetOrCreateStatsLocked(_activeProfile, normalized, DateTimeOffset.UtcNow, persistNewVideoRowIfCreated: false);

            var existing = _activeProfile.CustomLists[idx];
            if (existing.VideoPaths.Any(p => string.Equals(p, normalized, StringComparison.OrdinalIgnoreCase)))
            {
                return true;
            }

            var updatedPaths = new List<string>(existing.VideoPaths) { normalized };
            var updated = existing with
            {
                VideoPaths = updatedPaths,
                UpdatedAtUtc = DateTimeOffset.UtcNow,
            };
            _activeProfile.CustomLists[idx] = updated;
            EnqueueCustomListHeaderUpsertLocked(_activeProfile.Username, updated);
            EnqueueCustomListItemAddLocked(
                _activeProfile.Username,
                updated.Id,
                updatedPaths.Count - 1,
                normalized,
                postCommitEvents: [CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged)]);
            BumpInMemoryGenerationLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return true;
    }

    /// <summary>Removes <paramref name="videoPath"/> from the custom list identified by <paramref name="listId"/>.</summary>
    public bool RemoveVideoFromCustomList(string listId, string videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (string.IsNullOrWhiteSpace(listId) || normalized.Length == 0) return false;

        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return false;
            _ = ResolveAndMigrateVideoKeyLocked(_activeProfile, normalized, out _, out _, out _);

            var idx = _activeProfile.CustomLists.FindIndex(l => string.Equals(l.Id, listId, StringComparison.OrdinalIgnoreCase));
            if (idx < 0) return false;

            var existing = _activeProfile.CustomLists[idx];
            var updatedPaths = existing.VideoPaths
                .Where(p => !string.Equals(p, normalized, StringComparison.OrdinalIgnoreCase))
                .ToList();
            if (updatedPaths.Count == existing.VideoPaths.Count) return false;

            var updated = existing with
            {
                VideoPaths = updatedPaths,
                UpdatedAtUtc = DateTimeOffset.UtcNow,
            };
            _activeProfile.CustomLists[idx] = updated;
            EnqueueCustomListHeaderUpsertLocked(_activeProfile.Username, updated);
            EnqueueCustomListItemRemoveLocked(
                _activeProfile.Username,
                updated.Id,
                normalized,
                postCommitEvents: [CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged)]);
            BumpInMemoryGenerationLocked();
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
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return false;
            _ = GetOrCreateStatsLocked(_activeProfile, normalized, DateTimeOffset.UtcNow, persistNewVideoRowIfCreated: false);

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

            var updated = existing with
            {
                VideoPaths = updatedPaths,
                UpdatedAtUtc = DateTimeOffset.UtcNow,
            };
            _activeProfile.CustomLists[idx] = updated;
            EnqueueCustomListHeaderUpsertLocked(_activeProfile.Username, updated);
            if (nowInList)
            {
                EnqueueCustomListItemAddLocked(
                    _activeProfile.Username,
                    updated.Id,
                    updatedPaths.Count - 1,
                    normalized,
                    postCommitEvents: [CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged)]);
            }
            else
            {
                EnqueueCustomListItemRemoveLocked(
                    _activeProfile.Username,
                    updated.Id,
                    normalized,
                    postCommitEvents: [CreateBrokerEvent(StateSyncTopics.Profiles, StateSyncEventKind.ListChanged)]);
            }
            BumpInMemoryGenerationLocked();
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
        return nowInList;
    }

    /// <summary>Returns all custom lists created by the active profile, or an empty list when logged out.</summary>
    public IReadOnlyList<UserVideoList> GetCustomLists()
    {
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return [];
            return _activeProfile.CustomLists.ToList();
        }
    }

    /// <summary>Returns all custom lists in the active profile that contain <paramref name="videoPath"/>.</summary>
    public IReadOnlyList<UserVideoList> GetCustomListsContaining(string? videoPath)
    {
        var normalized = NormalizeVideoPath(videoPath);
        if (normalized.Length == 0) return [];

        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return [];
            if (ResolveAndMigrateVideoKeyLocked(_activeProfile, normalized, out _, out _, out var migrated) && migrated)
            {
                BumpInMemoryGenerationLocked();
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

        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            if (_activeProfile is null) return false;
            if (ResolveAndMigrateVideoKeyLocked(_activeProfile, normalized, out _, out _, out var migrated) && migrated)
            {
                BumpInMemoryGenerationLocked();
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
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            EnsureSyncedIfChangedLocked();
            var now = DateTimeOffset.UtcNow;
            var rootCache = new Dictionary<string, bool>(StringComparer.OrdinalIgnoreCase);

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
                    if (norm.Length > 0 &&
                        !discoveredOnly.ContainsKey(norm) &&
                        IsVideoStorageAvailableCore(norm, rootCache))
                    {
                        discoveredOnly[norm] = VideoUserStats.CreateDefault(norm, now, computeFingerprintFromDisk: false);
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

            var knownProfilePaths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            var hasTrackableFingerprints = false;
            foreach (var v in _activeProfile.Videos.Values)
            {
                if (!string.IsNullOrEmpty(v.VideoPath))
                {
                    knownProfilePaths.Add(v.VideoPath);
                    if (!string.IsNullOrEmpty(v.ContentFingerprint))
                    {
                        hasTrackableFingerprints = true;
                    }
                }
            }

            var anyMigrated = false;
            var discoveredNormalized = new List<string>();
            if (additionalDiscoveredPaths is not null)
            {
                foreach (var rawPath in additionalDiscoveredPaths)
                {
                    var norm = NormalizeVideoPath(rawPath);
                    if (norm.Length == 0 || !IsVideoStorageAvailableCore(norm, rootCache)) continue;
                    discoveredNormalized.Add(norm);
                    if (hasTrackableFingerprints &&
                        !knownProfilePaths.Contains(norm) &&
                        ResolveAndMigrateVideoKeyLocked(_activeProfile, norm, out _, out _, out var migrated) &&
                        migrated)
                    {
                        anyMigrated = true;
                        knownProfilePaths.Add(norm);
                    }
                }
            }

            if (anyMigrated)
            {
                BumpInMemoryGenerationLocked();
            }

            // Build path-keyed map of all available profile videos + discovered videos + custom list videos
            var mergedByPath = new Dictionary<string, VideoUserStats>(StringComparer.OrdinalIgnoreCase);
            foreach (var (key, video) in _activeProfile.Videos)
            {
                if (!string.IsNullOrEmpty(video.VideoPath) &&
                    IsVideoStorageAvailableCore(video.VideoPath, rootCache))
                {
                    var withThumb = TryGetThumbnailBytesByKeyLocked(_activeProfile, key, video.VideoPath) is { } tb
                        ? video with { ThumbnailBytes = tb }
                        : video;
                    mergedByPath[video.VideoPath] = withThumb;
                }
            }

            foreach (var norm in discoveredNormalized)
            {
                if (!mergedByPath.ContainsKey(norm))
                {
                    mergedByPath[norm] = VideoUserStats.CreateDefault(norm, now, computeFingerprintFromDisk: false);
                }
            }

            foreach (var list in _activeProfile.CustomLists)
            {
                foreach (var rawPath in list.VideoPaths)
                {
                    var norm = NormalizeVideoPath(rawPath);
                    if (norm.Length > 0 &&
                        !mergedByPath.ContainsKey(norm) &&
                        IsVideoStorageAvailableCore(norm, rootCache))
                    {
                        mergedByPath[norm] = VideoUserStats.CreateDefault(norm, now, computeFingerprintFromDisk: false);
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
                    if (norm.Length == 0 || !IsVideoStorageAvailableCore(norm, rootCache)) continue;
                    listItems.Add(mergedByPath.TryGetValue(norm, out var stats)
                        ? stats
                        : VideoUserStats.CreateDefault(norm, now, computeFingerprintFromDisk: false));
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

    private bool ResolveAndMigrateVideoKeyLocked(
        ProfileEntry profile,
        string normalizedPath,
        out string resolvedKey,
        out VideoUserStats? resolvedStats,
        out bool migrated)
    {
        migrated = false;

        // Fast path 1: Direct path key lookup or in-memory cached fingerprint lookup (0 disk I/O!)
        if (profile.Videos.TryGetValue(normalizedPath, out var directByPath))
        {
            resolvedKey = normalizedPath;
            resolvedStats = directByPath;
            return true;
        }

        var cachedFp = TryGetCachedFingerprint(normalizedPath);
        if (cachedFp is not null && profile.Videos.TryGetValue(cachedFp, out var directByCachedFp))
        {
            if (string.Equals(directByCachedFp.VideoPath, normalizedPath, StringComparison.OrdinalIgnoreCase))
            {
                resolvedKey = cachedFp;
                resolvedStats = directByCachedFp;
                return true;
            }
        }

        // Fast path 2: Linear scan of in-memory Videos by VideoPath (0 disk I/O!)
        foreach (var (k, v) in profile.Videos)
        {
            if (string.Equals(v.VideoPath, normalizedPath, StringComparison.OrdinalIgnoreCase))
            {
                resolvedKey = k;
                resolvedStats = v;
                return true;
            }
        }

        // Slow path: Only compute disk fingerprint if profile.Videos actually has an entry whose ContentFingerprint
        // has the exact same byte length ("fp:<length>:") and whose VideoPath differs from normalizedPath.
        string? fingerprint = cachedFp;
        if (fingerprint is null && profile.Videos.Count > 0)
        {
            try
            {
                var info = new FileInfo(normalizedPath);
                if (info.Exists && info.Length > 0)
                {
                    var expectedPrefix = string.Create(CultureInfo.InvariantCulture, $"fp:{info.Length}:");
                    var hasSameSizeCandidate = false;
                    foreach (var v in profile.Videos.Values)
                    {
                        if (v.ContentFingerprint is { } fp &&
                            fp.StartsWith(expectedPrefix, StringComparison.OrdinalIgnoreCase))
                        {
                            hasSameSizeCandidate = true;
                            break;
                        }
                    }

                    if (hasSameSizeCandidate)
                    {
                        fingerprint = TryComputeFingerprintOnly(normalizedPath);
                    }
                }
            }
            catch
            {
            }
        }

        var candidateKey = fingerprint ?? normalizedPath;

        string? foundKey = null;
        VideoUserStats? foundStats = null;

        if (fingerprint is not null && profile.Videos.TryGetValue(fingerprint, out var byFpKey))
        {
            foundKey = fingerprint;
            foundStats = byFpKey;
        }
        else if (fingerprint is not null)
        {
            foreach (var (k, v) in profile.Videos)
            {
                if (string.Equals(v.ContentFingerprint, fingerprint, StringComparison.OrdinalIgnoreCase))
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
                if (profile.DecodedThumbnails.Remove(foundKey, out var migratedThumb))
                {
                    profile.DecodedThumbnails[canonicalKey] = migratedThumb;
                }
                profile.MissingThumbnails.Remove(foundKey);
            }
            profile.Videos[canonicalKey] = updated;

            if (_sessionVaultKeys.TryGetValue(profile.Username, out var vaultKey))
            {
                var vBlob = EncryptVideoRowBlob(updated, vaultKey);
                var pBlob = EncryptWatchProgressRowBlob(updated, vaultKey);
                _broker.SubmitWriteRequest(new StateSyncWriteRequest
                {
                    RequestId = Guid.NewGuid().ToString("N"),
                    SenderInstanceId = _instanceId,
                    DbPath = SqliteDbPath,
                    Kind = nameof(SqliteProfileStore.WriteKind.MigrateVideoKey),
                    Username = profile.Username,
                    Key = foundKey,
                    SecondaryKey = canonicalKey,
                    Blob = vBlob,
                    SecondaryBlob = pBlob,
                    UpdatedTicks = updated.UpdatedAtUtc.UtcTicks,
                    ViewCount = updated.ViewCount,
                });
            }

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

    private void MigrateCustomListPathsLocked(ProfileEntry profile, string oldPath, string newPath)
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

            var updatedList = list with
            {
                VideoPaths = updatedPaths,
                UpdatedAtUtc = DateTimeOffset.UtcNow,
            };
            profile.CustomLists[i] = updatedList;
            EnqueueCustomListHeaderUpsertLocked(profile.Username, updatedList);
            if (_sessionVaultKeys.TryGetValue(profile.Username, out var vaultKey))
            {
                var itemRows = BuildEncryptedListItemRows(profile.Username, updatedList.Id, updatedList.VideoPaths, vaultKey);
                var dtoList = new List<StateSyncListItemDto>(itemRows.Count);
                foreach (var r in itemRows)
                {
                    dtoList.Add(new StateSyncListItemDto(
                        r.ListId,
                        r.ItemHash,
                        r.Ord,
                        r.EncryptedPathBlob));
                }
                _broker.SubmitWriteRequest(new StateSyncWriteRequest
                {
                    RequestId = Guid.NewGuid().ToString("N"),
                    SenderInstanceId = _instanceId,
                    DbPath = SqliteDbPath,
                    Kind = nameof(SqliteProfileStore.WriteKind.ReplaceCustomListItems),
                    Username = profile.Username,
                    Key = updatedList.Id,
                    BatchListItems = dtoList,
                });
            }
        }
    }

    private (string Key, VideoUserStats Stats) GetOrCreateStatsLocked(
        ProfileEntry profile,
        string normalizedPath,
        DateTimeOffset now,
        bool persistNewVideoRowIfCreated = false)
    {
        if (ResolveAndMigrateVideoKeyLocked(profile, normalizedPath, out var key, out var existing, out _) && existing is not null)
        {
            return (key, existing);
        }

        var fingerprint = TryGetCachedFingerprint(normalizedPath);
        if (fingerprint is null)
        {
            try
            {
                var info = new FileInfo(normalizedPath);
                if (info.Exists && info.Length > 0 && info.Length <= 64L * 1024L * 1024L)
                {
                    fingerprint = TryComputeFingerprintOnly(normalizedPath);
                }
                else if (info.Exists && info.Length > 64L * 1024L * 1024L)
                {
                    _ = Task.Run(() => TryComputeFingerprintOnly(normalizedPath));
                }
            }
            catch
            {
            }
        }

        var newKey = fingerprint ?? normalizedPath;
        var created = VideoUserStats.CreateDefault(normalizedPath, now, fingerprint, computeFingerprintFromDisk: false);
        profile.Videos[newKey] = created;
        if (persistNewVideoRowIfCreated)
        {
            EnqueueVideoAndProgressUpsertLocked(profile.Username, newKey, created, includeProgress: false);
        }
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

    private static byte[] EncryptBytesToBlob(ReadOnlySpan<byte> plaintext, byte[] vaultKey)
    {
        var nonce = RandomNumberGenerator.GetBytes(12);
        var combined = new byte[12 + 16 + plaintext.Length];
        Buffer.BlockCopy(nonce, 0, combined, 0, 12);
        var tagSpan = combined.AsSpan(12, 16);
        var cipherSpan = combined.AsSpan(28, plaintext.Length);

        using var aes = new AesGcm(vaultKey, 16);
        aes.Encrypt(nonce, plaintext, cipherSpan, tagSpan);
        return combined;
    }

    private static byte[]? DecryptBlobToBytes(ReadOnlySpan<byte> combined, byte[] vaultKey)
    {
        if (combined.Length < 28 || vaultKey.Length != 32) return null;
        try
        {
            var nonce = combined[..12];
            var tag = combined.Slice(12, 16);
            var cipher = combined[28..];
            var plaintext = new byte[cipher.Length];

            using var aes = new AesGcm(vaultKey, 16);
            aes.Decrypt(nonce, cipher, tag, plaintext);
            return plaintext;
        }
        catch
        {
            return null;
        }
    }

    private static string EncryptVaultMarker(byte[] vaultKey) =>
        Convert.ToBase64String(EncryptBytesToBlob(Encoding.UTF8.GetBytes(SqliteVaultMarkerPlaintext), vaultKey));

    private static byte[] EncryptVideoRowBlob(VideoUserStats video, byte[] vaultKey)
    {
        var dto = new VideoRowDto
        {
            ContentFingerprint = video.ContentFingerprint,
            VideoPath = video.VideoPath,
            FileName = video.FileName,
            DirectoryPath = video.DirectoryPath,
            ViewCount = video.ViewCount,
            IsLiked = video.IsLiked,
            Rating = video.Rating,
            LastWatchedAtUtc = video.LastWatchedAtUtc,
            UpdatedAtUtc = video.UpdatedAtUtc,
        };
        var utf8 = JsonSerializer.SerializeToUtf8Bytes(dto, CompactJsonOptions);
        return EncryptBytesToBlob(utf8, vaultKey);
    }

    private static VideoRowDto? DecryptVideoRowBlob(byte[] blob, byte[] vaultKey)
    {
        var plain = DecryptBlobToBytes(blob, vaultKey);
        if (plain is null) return null;
        try { return JsonSerializer.Deserialize<VideoRowDto>(plain, CompactJsonOptions); }
        catch { return null; }
    }

    private static byte[] EncryptWatchProgressRowBlob(VideoUserStats video, byte[] vaultKey)
    {
        var dto = new WatchProgressRowDto
        {
            LastPositionSeconds = video.LastPositionSeconds,
            DurationSeconds = video.DurationSeconds,
            UpdatedAtUtc = video.UpdatedAtUtc,
        };
        var utf8 = JsonSerializer.SerializeToUtf8Bytes(dto, CompactJsonOptions);
        return EncryptBytesToBlob(utf8, vaultKey);
    }

    private static WatchProgressRowDto? DecryptWatchProgressRowBlob(byte[] blob, byte[] vaultKey)
    {
        var plain = DecryptBlobToBytes(blob, vaultKey);
        if (plain is null) return null;
        try { return JsonSerializer.Deserialize<WatchProgressRowDto>(plain, CompactJsonOptions); }
        catch { return null; }
    }

    private static byte[] EncryptCustomListHeaderBlob(UserVideoList list, byte[] vaultKey)
    {
        var dto = new CustomListHeaderDto
        {
            Id = list.Id,
            Name = list.Name,
            CreatedAtUtc = list.CreatedAtUtc,
            UpdatedAtUtc = list.UpdatedAtUtc,
        };
        var utf8 = JsonSerializer.SerializeToUtf8Bytes(dto, CompactJsonOptions);
        return EncryptBytesToBlob(utf8, vaultKey);
    }

    private static CustomListHeaderDto? DecryptCustomListHeaderBlob(byte[] blob, byte[] vaultKey)
    {
        var plain = DecryptBlobToBytes(blob, vaultKey);
        if (plain is null) return null;
        try { return JsonSerializer.Deserialize<CustomListHeaderDto>(plain, CompactJsonOptions); }
        catch { return null; }
    }

    private static string ComputeListItemHash(string username, string listId, string normalizedVideoPath, byte[] vaultKey)
    {
        var input = Encoding.UTF8.GetBytes($"{username.ToUpperInvariant()}\n{listId.ToUpperInvariant()}\n{normalizedVideoPath.ToUpperInvariant()}");
        var mac = HMACSHA256.HashData(vaultKey, input);
        return Convert.ToHexStringLower(mac)[..32];
    }

    private static SqliteProfileStore.ListItemRow CreateEncryptedListItemRow(
        string username,
        string listId,
        int ord,
        string normalizedVideoPath,
        byte[] vaultKey)
    {
        var itemHash = ComputeListItemHash(username, listId, normalizedVideoPath, vaultKey);
        var encryptedPath = EncryptBytesToBlob(Encoding.UTF8.GetBytes(normalizedVideoPath), vaultKey);
        return new SqliteProfileStore.ListItemRow(listId, itemHash, ord, encryptedPath);
    }

    private static List<SqliteProfileStore.ListItemRow> BuildEncryptedListItemRows(
        string username,
        string listId,
        IReadOnlyList<string> paths,
        byte[] vaultKey)
    {
        var rows = new List<SqliteProfileStore.ListItemRow>(paths.Count);
        for (var i = 0; i < paths.Count; i++)
        {
            var norm = NormalizeVideoPath(paths[i]);
            if (norm.Length == 0) continue;
            rows.Add(CreateEncryptedListItemRow(username, listId, i, norm, vaultKey));
        }
        return rows;
    }

    private static byte[] EncryptThumbnailBlob(byte[] rawThumbnailBytes, byte[] vaultKey) =>
        EncryptBytesToBlob(rawThumbnailBytes, vaultKey);

    private static ProfileSecretVault? TryDecryptLegacyJsonVault(string? encryptedBase64, byte[] vaultKey)
    {
        if (string.IsNullOrWhiteSpace(encryptedBase64) || vaultKey.Length != 32) return null;
        try
        {
            var combined = Convert.FromBase64String(encryptedBase64);
            var plain = DecryptBlobToBytes(combined, vaultKey);
            if (plain is null || plain.Length == 0) return null;
            var text = Encoding.UTF8.GetString(plain);
            if (string.Equals(text, SqliteVaultMarkerPlaintext, StringComparison.Ordinal))
            {
                return null;
            }
            return JsonSerializer.Deserialize<ProfileSecretVault>(plain, JsonOptions);
        }
        catch
        {
            return null;
        }
    }

    private void EnqueueVideoAndProgressUpsertLocked(
        string username,
        string key,
        VideoUserStats stats,
        bool includeProgress,
        string? requestId = null,
        int viewCountDelta = 0,
        List<StateSyncEvent>? postCommitEvents = null)
    {
        if (!_sessionVaultKeys.TryGetValue(username, out var vaultKey)) return;
        var videoBlob = EncryptVideoRowBlob(stats, vaultKey);
        var reqId = string.IsNullOrWhiteSpace(requestId) ? Guid.NewGuid().ToString("N") : requestId;
        _broker.SubmitWriteRequest(new StateSyncWriteRequest
        {
            RequestId = reqId,
            SenderInstanceId = _instanceId,
            DbPath = SqliteDbPath,
            Kind = nameof(SqliteProfileStore.WriteKind.UpsertVideo),
            Username = username,
            Key = key,
            Blob = videoBlob,
            UpdatedTicks = stats.UpdatedAtUtc.UtcTicks,
            ViewCount = stats.ViewCount,
            ViewCountDelta = viewCountDelta,
            PostCommitEvents = includeProgress ? null : postCommitEvents,
        });
        if (includeProgress)
        {
            var progressBlob = EncryptWatchProgressRowBlob(stats, vaultKey);
            _broker.SubmitWriteRequest(new StateSyncWriteRequest
            {
                RequestId = $"{reqId}:progress",
                SenderInstanceId = _instanceId,
                DbPath = SqliteDbPath,
                Kind = nameof(SqliteProfileStore.WriteKind.UpsertWatchProgress),
                Username = username,
                Key = key,
                Blob = progressBlob,
                UpdatedTicks = stats.UpdatedAtUtc.UtcTicks,
                PostCommitEvents = postCommitEvents,
            });
        }
    }

    private void EnqueueWatchProgressUpsertOnlyLocked(
        string username,
        string key,
        VideoUserStats stats,
        List<StateSyncEvent>? postCommitEvents = null)
    {
        if (!_sessionVaultKeys.TryGetValue(username, out var vaultKey)) return;
        var progressBlob = EncryptWatchProgressRowBlob(stats, vaultKey);
        _broker.SubmitWriteRequest(new StateSyncWriteRequest
        {
            RequestId = Guid.NewGuid().ToString("N"),
            SenderInstanceId = _instanceId,
            DbPath = SqliteDbPath,
            Kind = nameof(SqliteProfileStore.WriteKind.UpsertWatchProgress),
            Username = username,
            Key = key,
            Blob = progressBlob,
            UpdatedTicks = stats.UpdatedAtUtc.UtcTicks,
            PostCommitEvents = postCommitEvents,
        });
    }

    private void EnqueueCustomListHeaderUpsertLocked(
        string username,
        UserVideoList list,
        List<StateSyncEvent>? postCommitEvents = null)
    {
        if (!_sessionVaultKeys.TryGetValue(username, out var vaultKey)) return;
        var blob = EncryptCustomListHeaderBlob(list, vaultKey);
        _broker.SubmitWriteRequest(new StateSyncWriteRequest
        {
            RequestId = Guid.NewGuid().ToString("N"),
            SenderInstanceId = _instanceId,
            DbPath = SqliteDbPath,
            Kind = nameof(SqliteProfileStore.WriteKind.UpsertCustomList),
            Username = username,
            Key = list.Id,
            Blob = blob,
            UpdatedTicks = list.UpdatedAtUtc.UtcTicks,
            PostCommitEvents = postCommitEvents,
        });
    }

    private void EnqueueCustomListItemAddLocked(
        string username,
        string listId,
        int ord,
        string normalizedPath,
        List<StateSyncEvent>? postCommitEvents = null)
    {
        if (!_sessionVaultKeys.TryGetValue(username, out var vaultKey)) return;
        var row = CreateEncryptedListItemRow(username, listId, ord, normalizedPath, vaultKey);
        _broker.SubmitWriteRequest(new StateSyncWriteRequest
        {
            RequestId = Guid.NewGuid().ToString("N"),
            SenderInstanceId = _instanceId,
            DbPath = SqliteDbPath,
            Kind = nameof(SqliteProfileStore.WriteKind.UpsertCustomListItem),
            Username = username,
            Key = listId,
            SecondaryKey = row.ItemHash,
            Ord = row.Ord,
            Blob = row.EncryptedPathBlob,
            PostCommitEvents = postCommitEvents,
        });
    }

    private void EnqueueCustomListItemRemoveLocked(
        string username,
        string listId,
        string normalizedPath,
        List<StateSyncEvent>? postCommitEvents = null)
    {
        if (!_sessionVaultKeys.TryGetValue(username, out var vaultKey)) return;
        var itemHash = ComputeListItemHash(username, listId, normalizedPath, vaultKey);
        _broker.SubmitWriteRequest(new StateSyncWriteRequest
        {
            RequestId = Guid.NewGuid().ToString("N"),
            SenderInstanceId = _instanceId,
            DbPath = SqliteDbPath,
            Kind = nameof(SqliteProfileStore.WriteKind.DeleteCustomListItem),
            Username = username,
            Key = listId,
            SecondaryKey = itemHash,
            PostCommitEvents = postCommitEvents,
        });
    }

    private static string ProtectVaultKey(byte[] vaultKey, byte[] salt)
    {
        var userEntropy = Encoding.UTF8.GetBytes($"SubtitleAdder.RememberKey.{Environment.UserName}.{Environment.MachineName}");
        var wrapKey = Rfc2898DeriveBytes.Pbkdf2(userEntropy, salt, 10_000, HashAlgorithmName.SHA256, 32);
        return Convert.ToBase64String(EncryptBytesToBlob(vaultKey, wrapKey));
    }

    private static byte[]? UnprotectVaultKey(string? protectedBase64, byte[] salt)
    {
        if (string.IsNullOrWhiteSpace(protectedBase64)) return null;
        try
        {
            var combined = Convert.FromBase64String(protectedBase64);
            var userEntropy = Encoding.UTF8.GetBytes($"SubtitleAdder.RememberKey.{Environment.UserName}.{Environment.MachineName}");
            var wrapKey = Rfc2898DeriveBytes.Pbkdf2(userEntropy, salt, 10_000, HashAlgorithmName.SHA256, 32);
            var plain = DecryptBlobToBytes(combined, wrapKey);
            return plain is { Length: 32 } ? plain : null;
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

    private byte[]? TryGetThumbnailBytesLocked(ProfileEntry profile, string normalizedPath)
    {
        if (ResolveAndMigrateVideoKeyLocked(profile, normalizedPath, out var key, out _, out _) &&
            TryGetThumbnailBytesByKeyLocked(profile, key, normalizedPath) is { } resolvedBytes)
        {
            return resolvedBytes;
        }

        var fp = TryGetCachedFingerprint(normalizedPath);
        if (fp is not null && TryGetThumbnailBytesByKeyLocked(profile, fp, normalizedPath) is { } fpBytes)
        {
            return fpBytes;
        }

        return TryGetThumbnailBytesByKeyLocked(profile, normalizedPath, normalizedPath);
    }

    private byte[]? TryGetThumbnailBytesByKeyLocked(ProfileEntry profile, string key, string normalizedPath)
    {
        if (profile.DecodedThumbnails.TryGetValue(key, out var cached))
        {
            return cached;
        }
        if (profile.MissingThumbnails.Contains(key))
        {
            return null;
        }
        if (!_sessionVaultKeys.TryGetValue(profile.Username, out var vaultKey))
        {
            return null;
        }

        var store = SqlStore;
        var blob = store.TryGetThumbnailBlob(profile.Username, key)
            ?? (!string.Equals(key, normalizedPath, StringComparison.OrdinalIgnoreCase)
                ? store.TryGetThumbnailBlob(profile.Username, normalizedPath)
                : null);
        if (blob is { Length: > 0 } && DecryptBlobToBytes(blob, vaultKey) is { Length: > 0 } decrypted)
        {
            profile.DecodedThumbnails[key] = decrypted;
            return decrypted;
        }

        profile.MissingThumbnails.Add(key);
        return null;
    }

    private bool DecryptIntoProfileEntryLocked(ProfileEntry entry, byte[] vaultKey)
    {
        entry.Videos.Clear();
        entry.CustomLists.Clear();
        entry.DecodedThumbnails.Clear();
        entry.MissingThumbnails.Clear();

        var store = SqlStore;
        var migratedLegacy = false;

        // 1. Check if profiles.json contains a legacy monolithic EncryptedVaultBase64 that needs migration to SQLite
        if (TryDecryptLegacyJsonVault(entry.EncryptedVaultBase64, vaultKey) is { } legacyVault)
        {
            var hasLegacyData =
                (legacyVault.Videos is { Count: > 0 }) ||
                (legacyVault.CustomLists is { Count: > 0 }) ||
                (legacyVault.Thumbnails is { Count: > 0 });

            if (hasLegacyData)
            {
                try
                {
                    if (File.Exists(FilePath) && !File.Exists(BackupFilePath))
                    {
                        File.Copy(FilePath, BackupFilePath, overwrite: false);
                    }
                }
                catch
                {
                }

                var videoRows = new List<(string Key, byte[] EncryptedBlob, long UpdatedTicks)>();
                var progressRows = new List<(string Key, byte[] EncryptedBlob, long UpdatedTicks)>();
                var listRows = new List<(string Key, byte[] EncryptedBlob, long UpdatedTicks)>();
                var listItemRows = new List<SqliteProfileStore.ListItemRow>();
                var thumbRows = new List<(string Key, byte[] EncryptedBlob, long UpdatedTicks)>();

                if (legacyVault.Videos is not null)
                {
                    foreach (var video in legacyVault.Videos)
                    {
                        var norm = NormalizeVideoPath(video.VideoPath);
                        if (norm.Length == 0) continue;
                        var fp = !string.IsNullOrWhiteSpace(video.ContentFingerprint)
                            ? video.ContentFingerprint.Trim()
                            : TryGetCachedFingerprint(norm);
                        var key = fp ?? norm;
                        var sanitized = video with
                        {
                            ContentFingerprint = fp,
                            VideoPath = norm,
                            FileName = string.IsNullOrWhiteSpace(video.FileName) ? Path.GetFileName(norm) : video.FileName,
                            DirectoryPath = video.DirectoryPath ?? (Path.GetDirectoryName(norm) ?? ""),
                            Rating = Math.Clamp(video.Rating, 0, 5),
                            ViewCount = Math.Max(0, video.ViewCount),
                        };
                        entry.Videos[key] = sanitized;
                        videoRows.Add((key, EncryptVideoRowBlob(sanitized, vaultKey), sanitized.UpdatedAtUtc.UtcTicks));
                        if (sanitized.LastPositionSeconds > 0 || sanitized.DurationSeconds > 0)
                        {
                            progressRows.Add((key, EncryptWatchProgressRowBlob(sanitized, vaultKey), sanitized.UpdatedAtUtc.UtcTicks));
                        }
                    }
                }

                if (legacyVault.CustomLists is not null)
                {
                    foreach (var list in legacyVault.CustomLists)
                    {
                        if (string.IsNullOrWhiteSpace(list.Id) || string.IsNullOrWhiteSpace(list.Name))
                            continue;
                        var normalizedPaths = (list.VideoPaths ?? [])
                            .Select(NormalizeVideoPath)
                            .Where(p => p.Length > 0)
                            .Distinct(StringComparer.OrdinalIgnoreCase)
                            .ToList();
                        var sanitizedList = list with
                        {
                            Name = list.Name.Trim(),
                            VideoPaths = normalizedPaths,
                        };
                        entry.CustomLists.Add(sanitizedList);
                        listRows.Add((sanitizedList.Id, EncryptCustomListHeaderBlob(sanitizedList, vaultKey), sanitizedList.UpdatedAtUtc.UtcTicks));
                        listItemRows.AddRange(BuildEncryptedListItemRows(entry.Username, sanitizedList.Id, normalizedPaths, vaultKey));
                    }
                }

                if (legacyVault.Thumbnails is not null)
                {
                    var nowTicks = DateTimeOffset.UtcNow.UtcTicks;
                    foreach (var (k, b64) in legacyVault.Thumbnails)
                    {
                        if (string.IsNullOrWhiteSpace(k) || TryDecodeBase64(b64) is not { Length: > 0 } rawThumbBytes)
                            continue;
                        entry.DecodedThumbnails[k] = rawThumbBytes;
                        thumbRows.Add((k, EncryptThumbnailBlob(rawThumbBytes, vaultKey), nowTicks));
                    }
                }

                store.EnqueueUpsertProfile(entry.Username, entry.SaltBase64, entry.PasswordHashBase64, entry.Iterations, entry.CreatedAtUtc);
                if (store.ImportLegacyVaultBatchAndVerify(entry.Username, videoRows, progressRows, listRows, listItemRows, thumbRows))
                {
                    entry.EncryptedVaultBase64 = EncryptVaultMarker(vaultKey);
                    migratedLegacy = true;
                }
            }
            else
            {
                entry.EncryptedVaultBase64 = EncryptVaultMarker(vaultKey);
                migratedLegacy = true;
            }
        }

        // 2. Load normalized encrypted rows from SQLite (videos, watch_progress, custom_lists, custom_list_items)
        var dbVideos = store.LoadVideosWithViewCounts(entry.Username);
        foreach (var (key, blob, dbViewCount) in dbVideos)
        {
            if (DecryptVideoRowBlob(blob, vaultKey) is not { } dto) continue;
            var norm = NormalizeVideoPath(dto.VideoPath);
            if (norm.Length == 0) continue;
            var fp = !string.IsNullOrWhiteSpace(dto.ContentFingerprint)
                ? dto.ContentFingerprint.Trim()
                : TryGetCachedFingerprint(norm);
            var canonicalKey = fp ?? key;
            var authoritativeViewCount = dbViewCount >= 0 ? (int)dbViewCount : Math.Max(0, dto.ViewCount);
            entry.Videos[canonicalKey] = new VideoUserStats
            {
                ContentFingerprint = fp,
                VideoPath = norm,
                FileName = string.IsNullOrWhiteSpace(dto.FileName) ? Path.GetFileName(norm) : dto.FileName,
                DirectoryPath = dto.DirectoryPath ?? (Path.GetDirectoryName(norm) ?? ""),
                ViewCount = authoritativeViewCount,
                IsLiked = dto.IsLiked,
                Rating = Math.Clamp(dto.Rating, 0, 5),
                LastPositionSeconds = 0,
                DurationSeconds = 0,
                LastWatchedAtUtc = dto.LastWatchedAtUtc,
                UpdatedAtUtc = dto.UpdatedAtUtc,
            };
        }

        var dbProgress = store.LoadWatchProgress(entry.Username);
        foreach (var (key, blob) in dbProgress)
        {
            if (DecryptWatchProgressRowBlob(blob, vaultKey) is not { } prog) continue;
            if (entry.Videos.TryGetValue(key, out var existingVideo))
            {
                entry.Videos[key] = existingVideo with
                {
                    LastPositionSeconds = Math.Max(0, prog.LastPositionSeconds),
                    DurationSeconds = Math.Max(0, prog.DurationSeconds),
                    UpdatedAtUtc = prog.UpdatedAtUtc > existingVideo.UpdatedAtUtc ? prog.UpdatedAtUtc : existingVideo.UpdatedAtUtc,
                };
            }
        }

        var dbListItemsByListId = new Dictionary<string, List<string>>(StringComparer.OrdinalIgnoreCase);
        foreach (var (listId, pathBlob) in store.LoadCustomListItems(entry.Username))
        {
            if (DecryptBlobToBytes(pathBlob, vaultKey) is not { Length: > 0 } pathBytes) continue;
            var norm = NormalizeVideoPath(Encoding.UTF8.GetString(pathBytes));
            if (norm.Length == 0) continue;
            if (!dbListItemsByListId.TryGetValue(listId, out var paths))
            {
                paths = [];
                dbListItemsByListId[listId] = paths;
            }
            if (!paths.Contains(norm, StringComparer.OrdinalIgnoreCase))
            {
                paths.Add(norm);
            }
            if (!entry.Videos.ContainsKey(norm))
            {
                var fp = TryGetCachedFingerprint(norm) ?? TryComputeFingerprintOnly(norm);
                var memKey = fp ?? norm;
                if (!entry.Videos.ContainsKey(memKey) &&
                    !entry.Videos.Values.Any(v => string.Equals(v.VideoPath, norm, StringComparison.OrdinalIgnoreCase)))
                {
                    entry.Videos[memKey] = VideoUserStats.CreateDefault(norm, DateTimeOffset.UtcNow, fp, computeFingerprintFromDisk: false);
                }
            }
        }

        var dbLists = store.LoadCustomLists(entry.Username);
        if (dbLists.Count > 0)
        {
            entry.CustomLists.Clear();
            foreach (var (listId, blob) in dbLists)
            {
                if (DecryptCustomListHeaderBlob(blob, vaultKey) is not { } header) continue;
                if (string.IsNullOrWhiteSpace(header.Id) || string.IsNullOrWhiteSpace(header.Name)) continue;
                var paths = dbListItemsByListId.TryGetValue(header.Id, out var items) ? items : [];
                entry.CustomLists.Add(new UserVideoList
                {
                    Id = header.Id,
                    Name = header.Name.Trim(),
                    VideoPaths = paths,
                    CreatedAtUtc = header.CreatedAtUtc,
                    UpdatedAtUtc = header.UpdatedAtUtc,
                });
            }
        }

        return migratedLegacy;
    }

    private void LoadFromDisk()
    {
        SqlStore.TryDrainWhenHealthyNonBlocking();
        lock (_sync)
        {
            SyncFromDiskLocked(force: true);
        }
    }

    private void BumpInMemoryGenerationLocked()
    {
        var nextGen = GlobalFileWriteGeneration.AddOrUpdate(FilePath, 1L, static (_, v) => v + 1L);
        _lastSyncedGeneration = nextGen;
    }

    private void EnsureSyncedIfChangedLocked()
    {
        var currentGen = GlobalFileWriteGeneration.GetValueOrDefault(FilePath, 0L);
        if (!_dirtyFromExternalBroker && _lastSyncedJson is not null && currentGen == _lastSyncedGeneration)
        {
            return;
        }
        SyncFromDiskLocked(force: true);
    }

    private bool SyncFromDiskLocked(bool force = false)
    {
        try
        {
            _dirtyFromExternalBroker = false;
            var currentGen = GlobalFileWriteGeneration.GetValueOrDefault(FilePath, 0L);
            if (!force && _lastSyncedJson is not null && currentGen == _lastSyncedGeneration)
            {
                return false;
            }

            if (AtomicFileStore.ReadAllTextShared(FilePath) is not { } json)
            {
                if (force)
                {
                    _profiles.Clear();
                    _lastSyncedJson = null;
                }
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

            var anyMigratedFromLegacyJson = false;
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
                            if (_activeUsername is null)
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
                    if (DecryptIntoProfileEntryLocked(entry, vaultKey))
                    {
                        anyMigratedFromLegacyJson = true;
                    }
                }

                _profiles[entry.Username] = entry;
            }

            if (_activeUsername is not null)
            {
                _activeProfile = _profiles.TryGetValue(_activeUsername, out var active) ? active : null;
                if (_activeProfile is null) _activeUsername = null;
            }

            _lastSyncedJson = json;
            _lastSyncedGeneration = GlobalFileWriteGeneration.GetValueOrDefault(FilePath, 0L);

            if (anyMigratedFromLegacyJson)
            {
                SaveAuthHeaderToDiskLocked();
            }

            return true;
        }
        catch (Exception ex) when (ex is IOException or JsonException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    private void SaveAuthHeaderToDiskLocked()
    {
        try
        {
            var dir = Path.GetDirectoryName(FilePath);
            if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);

            foreach (var entry in _profiles.Values)
            {
                if (string.IsNullOrWhiteSpace(entry.EncryptedVaultBase64) &&
                    _sessionVaultKeys.TryGetValue(entry.Username, out var vaultKey))
                {
                    entry.EncryptedVaultBase64 = EncryptVaultMarker(vaultKey);
                }
            }

            var doc = new ProfilesDocument
            {
                Version = 3,
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
                BumpInMemoryGenerationLocked();
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // Keep changes in memory if disk write is blocked.
        }
    }

    private StateSyncEvent CreateBrokerEvent(
        string topic,
        StateSyncEventKind kind,
        string? username = null,
        string? videoPath = null,
        int? viewCount = null,
        double? positionSeconds = null,
        double? durationSeconds = null,
        string? payloadBase64 = null) =>
        new()
        {
            EventId = Guid.NewGuid().ToString("N"),
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
        };

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
        _broker.Publish(topic, CreateBrokerEvent(topic, kind, username, videoPath, viewCount, positionSeconds, durationSeconds, payloadBase64));
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
            if (TryDecodeBase64(evt.PayloadBase64) is not { Length: > 0 } thumbBytes) return;
            lock (_sync)
            {
                if (_activeProfile is null) return;
                var normalized = NormalizeVideoPath(evt.VideoPath);
                if (normalized.Length == 0) return;
                var (key, _) = GetOrCreateStatsLocked(_activeProfile, normalized, DateTimeOffset.UtcNow);
                _activeProfile.DecodedThumbnails[key] = thumbBytes;
                _activeProfile.MissingThumbnails.Remove(key);
            }
            return;
        }

        if (string.Equals(evt.Topic, StateSyncTopics.Profiles, StringComparison.OrdinalIgnoreCase))
        {
            var updated = false;
            lock (_sync)
            {
                _dirtyFromExternalBroker = true;
                updated = SyncFromDiskLocked(force: true);
                if (evt.Kind == StateSyncEventKind.ProfileLogout && _rememberedUsername is null && _activeUsername is not null)
                {
                    _sessionVaultKeys.Remove(_activeUsername);
                    _activeProfile = null;
                    _activeUsername = null;
                    updated = true;
                }
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
        public Dictionary<string, byte[]> DecodedThumbnails { get; } = new(StringComparer.OrdinalIgnoreCase);
        public HashSet<string> MissingThumbnails { get; } = new(StringComparer.OrdinalIgnoreCase);
    }

    private sealed class VideoRowDto
    {
        public string? ContentFingerprint { get; set; }
        public string VideoPath { get; set; } = "";
        public string FileName { get; set; } = "";
        public string DirectoryPath { get; set; } = "";
        public int ViewCount { get; set; }
        public bool IsLiked { get; set; }
        public int Rating { get; set; }
        public DateTimeOffset? LastWatchedAtUtc { get; set; }
        public DateTimeOffset UpdatedAtUtc { get; set; }
    }

    private sealed class WatchProgressRowDto
    {
        public double LastPositionSeconds { get; set; }
        public double DurationSeconds { get; set; }
        public DateTimeOffset UpdatedAtUtc { get; set; }
    }

    private sealed class CustomListHeaderDto
    {
        public string Id { get; set; } = "";
        public string Name { get; set; } = "";
        public DateTimeOffset CreatedAtUtc { get; set; }
        public DateTimeOffset UpdatedAtUtc { get; set; }
    }

    private sealed class ProfileSecretVault
    {
        public List<VideoUserStats> Videos { get; set; } = [];
        public List<UserVideoList> CustomLists { get; set; } = [];
        public Dictionary<string, string> Thumbnails { get; set; } = new(StringComparer.OrdinalIgnoreCase);
    }

    private sealed class ProfilesDocument
    {
        public int Version { get; set; } = 3;
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
