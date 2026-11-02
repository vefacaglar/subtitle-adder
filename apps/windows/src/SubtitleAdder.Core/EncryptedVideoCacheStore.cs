using System.Buffers.Binary;
using System.Globalization;
using System.Security.Cryptography;
using System.Text;

namespace SubtitleAdder.Core;

/// <summary>
/// Per-profile AES-256-GCM encrypted persistent SSD cache for video playback chunks (<see cref="BufferedVideoStream"/>).
/// <para>
/// Every 8 MB chunk read from a local video is encrypted with a key derived from the active profile's 256-bit
/// <c>vaultKey</c> (<c>HMAC-SHA256(vaultKey, profile | videoIdentity | chunkSize | chunkIndex)</c>) and stored under
/// <c>%LOCALAPPDATA%\SubtitleAdder\VideoCache\&lt;ProfileHash&gt;\&lt;VideoHash&gt;\chunk_XXXXXX.vcache</c>.
/// </para>
/// <list type="bullet">
/// <item><description>Seeking backward/forward or reopening a previously watched video reads directly from RAM or the encrypted SSD cache with <b>0</b> HDD reads.</description></item>
/// <item><description>Neither video filenames nor plaintext media bytes ever appear in the cache folder; external processes and other profiles cannot read or decrypt any cached chunk.</description></item>
/// <item><description>Automatically enforces the user's configured maximum cache size (default <c>20 GB</c>) using LRU eviction.</description></item>
/// </list>
/// </summary>
public sealed class EncryptedVideoCacheStore
{
    /// <summary>Default persistent encrypted video cache limit in gigabytes (20 GB).</summary>
    public const int DefaultMaxSizeGb = 20;

    /// <summary>Minimum configurable cache limit in gigabytes (1 GB).</summary>
    public const int MinMaxSizeGb = 1;

    /// <summary>Maximum configurable cache limit in gigabytes (500 GB).</summary>
    public const int MaxMaxSizeGb = 500;

    private const int HeaderSizeBytes = 40; // 8 magic + 4 length + 12 nonce + 16 tag
    private static ReadOnlySpan<byte> MagicHeader => "SAVC0001"u8;

    public static string DefaultDirectory { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "SubtitleAdder",
        "VideoCache");

    public static EncryptedVideoCacheStore Shared { get; } = new();

    private readonly LocalProfileManager _profileManager;
    private readonly byte[] _anonymousSessionKey = RandomNumberGenerator.GetBytes(32);
    private readonly Lock _quotaLock = new();
    private long _maxCacheSizeBytes = (long)DefaultMaxSizeGb * 1024L * 1024L * 1024L;
    private int _writesSinceQuotaCheck;

    public EncryptedVideoCacheStore(
        string? cacheDirectory = null,
        LocalProfileManager? profileManager = null,
        long? maxCacheSizeBytes = null)
    {
        CacheDirectory = string.IsNullOrWhiteSpace(cacheDirectory)
            ? DefaultDirectory
            : Path.GetFullPath(cacheDirectory);
        _profileManager = profileManager ?? VideoPlatformViewModel.Shared.ProfileManager;
        if (maxCacheSizeBytes is { } customMax && customMax > 0)
        {
            _maxCacheSizeBytes = customMax;
        }
    }

    /// <summary>Root directory where encrypted <c>.vcache</c> chunk files are stored.</summary>
    public string CacheDirectory { get; }

    /// <summary>Maximum total disk space in bytes allowed for encrypted video chunks.</summary>
    public long MaxCacheSizeBytes
    {
        get => Interlocked.Read(ref _maxCacheSizeBytes);
        set
        {
            var clamped = Math.Max(64L * 1024L, value);
            Interlocked.Exchange(ref _maxCacheSizeBytes, clamped);
            EnforceQuota(clamped);
        }
    }

    /// <summary>Maximum total disk space in gigabytes allowed for encrypted video chunks (clamped to 1..500 GB).</summary>
    public int MaxCacheSizeGb
    {
        get => (int)Math.Clamp(MaxCacheSizeBytes / (1024L * 1024L * 1024L), MinMaxSizeGb, MaxMaxSizeGb);
        set
        {
            var clampedGb = Math.Clamp(value, MinMaxSizeGb, MaxMaxSizeGb);
            MaxCacheSizeBytes = (long)clampedGb * 1024L * 1024L * 1024L;
        }
    }

    /// <summary>
    /// Computes a deterministic, non-blocking, privacy-preserving identity string for a video file or stream
    /// without performing any synchronous disk seeks.
    /// </summary>
    public static string ComputeVideoIdentity(string? filePath, long streamLength, int chunkSize)
    {
        if (!string.IsNullOrWhiteSpace(filePath))
        {
            var normalized = LocalProfileManager.NormalizeVideoPath(filePath);
            if (!string.IsNullOrWhiteSpace(normalized))
            {
                return string.Create(CultureInfo.InvariantCulture, $"{normalized.ToUpperInvariant()}|len:{streamLength}|cs:{chunkSize}");
            }
        }
        return string.Create(CultureInfo.InvariantCulture, $"stream|len:{streamLength}|cs:{chunkSize}");
    }

    /// <summary>
    /// Returns the set of chunk indices (<c>0 .. totalChunks - 1</c>) that already exist in the active profile's encrypted SSD cache.
    /// </summary>
    public HashSet<long> GetCachedChunkIndices(string videoIdentity, long totalChunks, int chunkSize, long totalStreamLength)
    {
        var result = new HashSet<long>();
        if (string.IsNullOrWhiteSpace(videoIdentity) || totalChunks <= 0 || chunkSize <= 0 || totalStreamLength <= 0)
        {
            return result;
        }

        try
        {
            var (profileId, _) = GetActiveProfileMaterial();
            var videoDir = GetVideoCacheDirectory(profileId, videoIdentity);
            if (!Directory.Exists(videoDir)) return result;

            foreach (var file in Directory.EnumerateFiles(videoDir, "chunk_*.vcache", SearchOption.TopDirectoryOnly))
            {
                var name = Path.GetFileNameWithoutExtension(file);
                if (!name.StartsWith("chunk_", StringComparison.OrdinalIgnoreCase)) continue;
                if (!long.TryParse(name.AsSpan(6), NumberStyles.None, CultureInfo.InvariantCulture, out var chunkIndex))
                {
                    continue;
                }
                if (chunkIndex < 0 || chunkIndex >= totalChunks) continue;

                var chunkStartOffset = chunkIndex * chunkSize;
                var expectedPlainBytes = (int)Math.Min(chunkSize, Math.Max(0L, totalStreamLength - chunkStartOffset));
                var info = new FileInfo(file);
                if (info.Exists && info.Length == HeaderSizeBytes + expectedPlainBytes)
                {
                    result.Add(chunkIndex);
                }
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
        }

        return result;
    }

    /// <summary>
    /// Attempts to read and AES-256-GCM decrypt a cached video chunk for the currently active profile.
    /// Returns <c>null</c> on cache miss, profile mismatch, or authentication tag failure.
    /// </summary>
    public byte[]? TryReadChunk(string videoIdentity, int chunkSize, long chunkIndex, int expectedPlaintextLength)
    {
        if (string.IsNullOrWhiteSpace(videoIdentity) || expectedPlaintextLength <= 0 || chunkIndex < 0)
        {
            return null;
        }

        var (profileId, masterKey) = GetActiveProfileMaterial();
        var chunkPath = GetChunkFilePath(profileId, videoIdentity, chunkIndex);
        if (!File.Exists(chunkPath))
        {
            return null;
        }

        try
        {
            byte[] encrypted;
            using (var fs = new FileStream(
                chunkPath,
                FileMode.Open,
                FileAccess.Read,
                FileShare.ReadWrite | FileShare.Delete,
                bufferSize: 4096,
                FileOptions.SequentialScan))
            {
                if (fs.Length != HeaderSizeBytes + expectedPlaintextLength)
                {
                    return null;
                }

                encrypted = new byte[HeaderSizeBytes + expectedPlaintextLength];
                var totalRead = 0;
                while (totalRead < encrypted.Length)
                {
                    var n = fs.Read(encrypted, totalRead, encrypted.Length - totalRead);
                    if (n <= 0) break;
                    totalRead += n;
                }
                if (totalRead != encrypted.Length)
                {
                    return null;
                }
            }

            if (!encrypted.AsSpan(0, 8).SequenceEqual(MagicHeader))
            {
                return null;
            }

            var storedLength = BinaryPrimitives.ReadInt32LittleEndian(encrypted.AsSpan(8, 4));
            if (storedLength != expectedPlaintextLength)
            {
                return null;
            }

            var nonce = encrypted.AsSpan(12, 12);
            var tag = encrypted.AsSpan(24, 16);
            var ciphertext = encrypted.AsSpan(HeaderSizeBytes, expectedPlaintextLength);
            var plaintext = new byte[expectedPlaintextLength];

            var chunkKey = DeriveChunkKey(masterKey, profileId, videoIdentity, chunkSize, chunkIndex);
            var aad = BuildChunkAad(profileId, videoIdentity, chunkSize, chunkIndex, expectedPlaintextLength);

            using (var aes = new AesGcm(chunkKey, 16))
            {
                aes.Decrypt(nonce, ciphertext, tag, plaintext, aad);
            }

            CryptographicOperations.ZeroMemory(chunkKey);

            // Best-effort touch for LRU tracking
            try
            {
                File.SetLastWriteTimeUtc(chunkPath, DateTime.UtcNow);
            }
            catch
            {
            }

            return plaintext;
        }
        catch (CryptographicException)
        {
            // Authentication tag mismatch or corrupted ciphertext -> remove invalid cache file
            try { File.Delete(chunkPath); } catch { }
            return null;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return null;
        }
    }

    /// <summary>
    /// Encrypts <paramref name="plaintext"/> with AES-256-GCM using a key derived from the active profile's vault key
    /// and writes it atomically to the SSD cache.
    /// </summary>
    public bool TryWriteChunk(string videoIdentity, int chunkSize, long chunkIndex, ReadOnlySpan<byte> plaintext)
    {
        if (string.IsNullOrWhiteSpace(videoIdentity) || plaintext.IsEmpty || chunkIndex < 0)
        {
            return false;
        }

        var maxBytes = MaxCacheSizeBytes;
        if (HeaderSizeBytes + plaintext.Length > maxBytes)
        {
            return false;
        }

        var (profileId, masterKey) = GetActiveProfileMaterial();
        var videoDir = GetVideoCacheDirectory(profileId, videoIdentity);
        var chunkPath = GetChunkFilePath(profileId, videoIdentity, chunkIndex);

        try
        {
            Directory.CreateDirectory(videoDir);

            var packet = new byte[HeaderSizeBytes + plaintext.Length];
            MagicHeader.CopyTo(packet.AsSpan(0, 8));
            BinaryPrimitives.WriteInt32LittleEndian(packet.AsSpan(8, 4), plaintext.Length);

            var nonce = packet.AsSpan(12, 12);
            RandomNumberGenerator.Fill(nonce);
            var tag = packet.AsSpan(24, 16);
            var ciphertext = packet.AsSpan(HeaderSizeBytes, plaintext.Length);

            var chunkKey = DeriveChunkKey(masterKey, profileId, videoIdentity, chunkSize, chunkIndex);
            var aad = BuildChunkAad(profileId, videoIdentity, chunkSize, chunkIndex, plaintext.Length);

            using (var aes = new AesGcm(chunkKey, 16))
            {
                aes.Encrypt(nonce, plaintext, ciphertext, tag, aad);
            }

            CryptographicOperations.ZeroMemory(chunkKey);

            var tempPath = Path.Combine(videoDir, $".chunk_{chunkIndex:D6}.{Guid.NewGuid():N}.tmp");
            try
            {
                using (var fs = new FileStream(
                    tempPath,
                    FileMode.Create,
                    FileAccess.Write,
                    FileShare.None,
                    bufferSize: 64 * 1024,
                    FileOptions.WriteThrough))
                {
                    fs.Write(packet, 0, packet.Length);
                }

                File.Move(tempPath, chunkPath, overwrite: true);
            }
            finally
            {
                try
                {
                    if (File.Exists(tempPath)) File.Delete(tempPath);
                }
                catch
                {
                }
            }

            if (Interlocked.Increment(ref _writesSinceQuotaCheck) % 4 == 1)
            {
                EnforceQuota(maxBytes);
            }

            return true;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or CryptographicException)
        {
            return false;
        }
    }

    /// <summary>Returns the total bytes currently occupied by all encrypted <c>.vcache</c> files on disk.</summary>
    public long GetTotalUsedBytes()
    {
        try
        {
            if (!Directory.Exists(CacheDirectory)) return 0L;
            var sum = 0L;
            foreach (var file in Directory.EnumerateFiles(CacheDirectory, "*.vcache", SearchOption.AllDirectories))
            {
                try
                {
                    var info = new FileInfo(file);
                    if (info.Exists) sum += info.Length;
                }
                catch
                {
                }
            }
            return sum;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return 0L;
        }
    }

    /// <summary>Deletes all encrypted video cache files across all profiles on disk.</summary>
    public void ClearAll()
    {
        lock (_quotaLock)
        {
            try
            {
                if (!Directory.Exists(CacheDirectory)) return;
                foreach (var file in Directory.EnumerateFiles(CacheDirectory, "*", SearchOption.AllDirectories))
                {
                    try { File.Delete(file); } catch { }
                }
                foreach (var dir in Directory.EnumerateDirectories(CacheDirectory, "*", SearchOption.TopDirectoryOnly))
                {
                    try { Directory.Delete(dir, recursive: true); } catch { }
                }
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
            }
        }
    }

    /// <summary>Evicts least-recently-used <c>.vcache</c> chunk files until total usage is at or below <paramref name="maxBytes"/>.</summary>
    public void EnforceQuota(long? maxBytes = null)
    {
        var limit = maxBytes ?? MaxCacheSizeBytes;
        lock (_quotaLock)
        {
            try
            {
                if (!Directory.Exists(CacheDirectory)) return;
                var files = new List<(string FullPath, long Length, DateTime LastWriteUtc)>();
                var total = 0L;

                foreach (var path in Directory.EnumerateFiles(CacheDirectory, "*.vcache", SearchOption.AllDirectories))
                {
                    try
                    {
                        var info = new FileInfo(path);
                        if (!info.Exists) continue;
                        var len = info.Length;
                        total += len;
                        files.Add((path, len, info.LastWriteTimeUtc));
                    }
                    catch
                    {
                    }
                }

                if (total <= limit) return;

                foreach (var candidate in files.OrderBy(f => f.LastWriteUtc))
                {
                    if (total <= limit) break;
                    try
                    {
                        File.Delete(candidate.FullPath);
                        total -= candidate.Length;
                    }
                    catch
                    {
                    }
                }
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
            }
        }
    }

    private (string ProfileId, byte[] MasterKey) GetActiveProfileMaterial()
    {
        if (_profileManager.TryGetActiveProfileVaultKey(out var username, out var vaultKey) &&
            !string.IsNullOrWhiteSpace(username) &&
            vaultKey.Length == 32)
        {
            return (username.Trim().ToUpperInvariant(), vaultKey);
        }

        var anonCopy = new byte[32];
        Buffer.BlockCopy(_anonymousSessionKey, 0, anonCopy, 0, 32);
        return ("__ANONYMOUS_SESSION__", anonCopy);
    }

    private string GetVideoCacheDirectory(string normalizedProfileId, string videoIdentity)
    {
        var profileHash = ComputeShortHash("SubtitleAdder.VideoCache.Profile.v1:" + normalizedProfileId);
        var videoHash = ComputeShortHash("SubtitleAdder.VideoCache.Video.v1:" + videoIdentity);
        return Path.Combine(CacheDirectory, profileHash, videoHash);
    }

    private string GetChunkFilePath(string normalizedProfileId, string videoIdentity, long chunkIndex) =>
        Path.Combine(GetVideoCacheDirectory(normalizedProfileId, videoIdentity), $"chunk_{chunkIndex:D6}.vcache");

    private static string ComputeShortHash(string input)
    {
        Span<byte> hash = stackalloc byte[32];
        SHA256.HashData(Encoding.UTF8.GetBytes(input), hash);
        return Convert.ToHexStringLower(hash)[..32];
    }

    private static byte[] DeriveChunkKey(byte[] masterKey, string normalizedProfileId, string videoIdentity, int chunkSize, long chunkIndex)
    {
        var context = Encoding.UTF8.GetBytes(
            string.Create(CultureInfo.InvariantCulture, $"SubtitleAdder.VideoCache.ChunkKey.v1|{normalizedProfileId}|{videoIdentity}|{chunkSize}|{chunkIndex}"));
        return HMACSHA256.HashData(masterKey, context);
    }

    private static byte[] BuildChunkAad(string normalizedProfileId, string videoIdentity, int chunkSize, long chunkIndex, int plaintextLength) =>
        Encoding.UTF8.GetBytes(
            string.Create(CultureInfo.InvariantCulture, $"SAVC1|{normalizedProfileId}|{videoIdentity}|{chunkSize}|{chunkIndex}|{plaintextLength}"));
}
