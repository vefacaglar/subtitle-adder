using System.Buffers.Binary;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace SubtitleAdder.Core;

/// <summary>
/// Per-profile AES-256-GCM encrypted persistent SSD cache for video playback chunks (<see cref="BufferedVideoStream"/>).
/// <para>
/// Every 8 MB chunk read from a local video is encrypted with a key derived from the active profile's 256-bit
/// <c>vaultKey</c> (<c>HMAC-SHA256(vaultKey, profile | videoIdentity | chunkSize | chunkIndex)</c>) and stored under
/// <c>%LOCALAPPDATA%\SubtitleAdder\VideoCache\v3\&lt;ChunkTag&gt;.vcache</c> (one flat folder for every profile and video).
/// </para>
/// <list type="bullet">
/// <item><description>Seeking backward/forward or reopening a previously watched video reads directly from RAM or the encrypted SSD cache with <b>0</b> HDD reads.</description></item>
/// <item><description>File names are keyed hashes (<c>HMAC(vaultKey, ...)</c>) in one flat folder, and every file is padded to the full chunk size: without the profile's key nobody can tell which profile, video or position a file belongs to, how many videos a profile played or how much of each, nor confirm a candidate file by its length. File times are fixed on write and reads never update the last-access time, so they do not record when a video was played. Only the total amount cached is visible.</description></item>
/// <item><description>Nothing is cached while no profile is logged in, and a stream opened under one session never reads or writes under another.</description></item>
/// <item><description>Automatically enforces the user's configured maximum cache size (default <c>20 GB</c>).</description></item>
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

    /// <summary>Subfolder holding the current layout; everything else under <see cref="CacheDirectory"/> is a legacy layout.</summary>
    public const string LayoutFolderName = "v3";

    /// <summary>Size of the per-file header (8 magic + 4 padded length + 12 nonce + 16 tag).</summary>
    public const int HeaderSizeBytes = 40;
    private static ReadOnlySpan<byte> MagicHeader => "SAVC0003"u8;

    // Every cache file and folder gets this timestamp, so file times never reveal when a video was played.
    private static readonly DateTime FixedFileTimeUtc = new(2001, 1, 1, 0, 0, 0, DateTimeKind.Utc);

    public static string DefaultDirectory { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "SubtitleAdder",
        "VideoCache");

    public static EncryptedVideoCacheStore Shared { get; } = new();

    private readonly LocalProfileManager _profileManager;
    private readonly Lock _quotaLock = new();
    private long _maxCacheSizeBytes = (long)DefaultMaxSizeGb * 1024L * 1024L * 1024L;
    private int _writesSinceQuotaCheck;

    // Sessions handed out to open streams; ended (key copy zeroed) as soon as their profile logs out or switches.
    private readonly List<WeakReference<CacheSession>> _sessions = [];
    private readonly Lock _sessionsLock = new();

    /// <summary>
    /// The cache owner captured when a stream opens: the profile and a copy of its key. Reads and writes made with it
    /// are refused as soon as that profile is no longer the active, unlocked one, and the key copy is then zeroed so
    /// nothing of the ended session stays in memory while the stream lives on.
    /// </summary>
    public sealed class CacheSession
    {
        internal CacheSession(string username, byte[] vaultKey, string profileTag)
        {
            Username = username;
            VaultKey = vaultKey;
            ProfileTag = profileTag;
        }

        internal Lock Gate { get; } = new();
        internal string Username { get; }
        internal byte[] VaultKey { get; }
        internal string ProfileTag { get; }

        /// <summary>True once the session ended; it never becomes current again.</summary>
        public bool IsEnded { get; private set; }

        internal void End()
        {
            lock (Gate)
            {
                if (IsEnded) return;
                IsEnded = true;
                CryptographicOperations.ZeroMemory(VaultKey);
            }
        }
    }

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

        // Earlier layouts named folders after (unkeyed or keyed) hashes of the profile and the video, wrote the last
        // chunk at its real size, and also cached logged-out playback: delete them once, in the background.
        _ = Task.Run(PurgeLegacyLayout);

        // A logout or profile switch ends the open streams' sessions right away, not only on their next read.
        _profileManager.StateChanged += (_, _) => _ = Task.Run(EndStaleSessions);
    }

    /// <summary>Ends every handed-out session whose profile is no longer the active, unlocked one.</summary>
    public void EndStaleSessions()
    {
        List<CacheSession> live = [];
        lock (_sessionsLock)
        {
            _sessions.RemoveAll(w => !w.TryGetTarget(out var target) || target.IsEnded);
            foreach (var weak in _sessions)
            {
                if (weak.TryGetTarget(out var target)) live.Add(target);
            }
        }
        foreach (var session in live)
        {
            if (!IsSessionCurrent(session)) session.End();
        }
    }

    /// <summary>Root directory where encrypted <c>.vcache</c> chunk files are stored.</summary>
    public string CacheDirectory { get; }

    private string LayoutDirectory => Path.Combine(CacheDirectory, LayoutFolderName);

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
    /// Computes a deterministic identity string for a video file or stream without performing any disk seeks.
    /// It is never written to disk as is: file names are keyed hashes of it.
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
    /// Captures the active profile as the owner of a playback stream's cache, or <c>null</c> when no profile is
    /// logged in: logged-out playback is never cached on disk.
    /// </summary>
    public CacheSession? TryBeginSession()
    {
        if (!_profileManager.TryGetActiveProfileVaultKey(out var username, out var vaultKey) ||
            string.IsNullOrWhiteSpace(username) ||
            vaultKey.Length != 32)
        {
            return null;
        }
        var session = new CacheSession(username.Trim(), vaultKey, KeyedTag(vaultKey, "SubtitleAdder.VideoCache.ProfileDir.v2"));
        lock (_sessionsLock)
        {
            _sessions.RemoveAll(w => !w.TryGetTarget(out var target) || target.IsEnded);
            _sessions.Add(new WeakReference<CacheSession>(session));
        }
        return session;
    }

    /// <summary>True while <paramref name="session"/>'s profile is still the active, unlocked profile.</summary>
    public bool IsSessionCurrent(CacheSession? session)
    {
        if (session is null) return false;
        if (!_profileManager.TryGetActiveProfileVaultKey(out var username, out var vaultKey)) return false;
        try
        {
            lock (session.Gate)
            {
                return !session.IsEnded &&
                       string.Equals(username.Trim(), session.Username, StringComparison.OrdinalIgnoreCase) &&
                       CryptographicOperations.FixedTimeEquals(vaultKey, session.VaultKey);
            }
        }
        finally
        {
            CryptographicOperations.ZeroMemory(vaultKey);
        }
    }

    // Runs work that uses the session's key while holding its gate, so ending the session (zeroing the key) never
    // races a derivation half way through. A session found stale is ended. Callers keep file I/O out of `work`.
    private T WithCurrentSession<T>(CacheSession? session, T fallback, Func<CacheSession, T> work)
    {
        if (session is null) return fallback;
        lock (session.Gate)
        {
            if (session.IsEnded) return fallback;
            if (!IsSessionCurrent(session))
            {
                session.End();
                return fallback;
            }
            return work(session);
        }
    }

    // Everything a single chunk read or write needs from the session's key, derived while holding its gate so the
    // file I/O and the cipher work never run under it (a playback read must not wait behind a write-through write).
    private sealed class ChunkMaterial(string path, byte[] key, byte[] aad) : IDisposable
    {
        public string Path { get; } = path;
        public byte[] Key { get; } = key;
        public byte[] Aad { get; } = aad;
        public void Dispose() => CryptographicOperations.ZeroMemory(Key);
    }

    private ChunkMaterial? TryDeriveMaterial(CacheSession? session, string videoIdentity, int chunkSize, long chunkIndex, int plaintextLength) =>
        WithCurrentSession<ChunkMaterial?>(session, null, s => new ChunkMaterial(
            GetChunkFilePath(s, videoIdentity, chunkIndex),
            DeriveChunkKey(s, videoIdentity, chunkSize, chunkIndex),
            BuildChunkAad(s, videoIdentity, chunkSize, chunkIndex, plaintextLength)));

    /// <summary>
    /// Returns the set of chunk indices (<c>0 .. totalChunks - 1</c>) that already exist in the active profile's encrypted SSD cache.
    /// </summary>
    public HashSet<long> GetCachedChunkIndices(string videoIdentity, long totalChunks, int chunkSize, long totalStreamLength) =>
        GetCachedChunkIndices(TryBeginSession(), videoIdentity, totalChunks, chunkSize, totalStreamLength);

    /// <summary>Returns the chunk indices cached for <paramref name="session"/>; empty once that session has ended.</summary>
    public HashSet<long> GetCachedChunkIndices(CacheSession? session, string videoIdentity, long totalChunks, int chunkSize, long totalStreamLength)
    {
        if (string.IsNullOrWhiteSpace(videoIdentity) || totalChunks <= 0 || chunkSize <= 0 || totalStreamLength <= 0)
        {
            return [];
        }
        // Only the names are derived under the session's gate; the file checks run outside it.
        var paths = WithCurrentSession<string[]?>(session, null, s =>
        {
            var list = new string[totalChunks];
            for (long i = 0; i < totalChunks; i++) list[i] = GetChunkFilePath(s, videoIdentity, i);
            return list;
        });
        if (paths is null) return [];

        var result = new HashSet<long>();
        var expectedLength = (long)HeaderSizeBytes + chunkSize;
        for (long chunkIndex = 0; chunkIndex < paths.Length; chunkIndex++)
        {
            try
            {
                // A per-file attribute lookup, not a directory listing: it touches no last-access time.
                var info = new FileInfo(paths[chunkIndex]);
                if (info.Exists && info.Length == expectedLength) result.Add(chunkIndex);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
            }
        }
        return result;
    }

    /// <summary>
    /// Attempts to read and AES-256-GCM decrypt a cached video chunk for the currently active profile.
    /// Returns <c>null</c> when logged out, on cache miss, profile mismatch, or authentication tag failure.
    /// </summary>
    public byte[]? TryReadChunk(string videoIdentity, int chunkSize, long chunkIndex, int expectedPlaintextLength) =>
        TryReadChunk(TryBeginSession(), videoIdentity, chunkSize, chunkIndex, expectedPlaintextLength);

    /// <summary>Reads a chunk cached for <paramref name="session"/>; <c>null</c> once that session has ended.</summary>
    public byte[]? TryReadChunk(CacheSession? session, string videoIdentity, int chunkSize, long chunkIndex, int expectedPlaintextLength)
    {
        if (string.IsNullOrWhiteSpace(videoIdentity) || expectedPlaintextLength <= 0 || chunkIndex < 0 ||
            chunkSize <= 0 || expectedPlaintextLength > chunkSize)
        {
            return null;
        }
        using var material = TryDeriveMaterial(session, videoIdentity, chunkSize, chunkIndex, expectedPlaintextLength);
        return material is null ? null : TryReadChunkCore(material, chunkSize, expectedPlaintextLength);
    }

    private static byte[]? TryReadChunkCore(ChunkMaterial material, int chunkSize, int expectedPlaintextLength)
    {
        var chunkPath = material.Path;
        if (!File.Exists(chunkPath))
        {
            return null;
        }

        try
        {
            // Every chunk is stored padded to the full chunk size, so the file length says nothing about the video.
            var encrypted = new byte[HeaderSizeBytes + chunkSize];
            using (var fs = OpenForReadWithoutAccessTime(chunkPath))
            {
                if (fs.Length != encrypted.Length)
                {
                    return null;
                }

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

            if (BinaryPrimitives.ReadInt32LittleEndian(encrypted.AsSpan(8, 4)) != chunkSize)
            {
                return null;
            }

            var nonce = encrypted.AsSpan(12, 12);
            var tag = encrypted.AsSpan(24, 16);
            var ciphertext = encrypted.AsSpan(HeaderSizeBytes, chunkSize);
            var padded = new byte[chunkSize];

            // The real length is authenticated through the AAD: a chunk is only accepted at the length it was written with.
            using (var aes = new AesGcm(material.Key, 16))
            {
                aes.Decrypt(nonce, ciphertext, tag, padded, material.Aad);
            }

            // No "last used" touch: the file time would record when the video was played.
            return expectedPlaintextLength == chunkSize ? padded : padded.AsSpan(0, expectedPlaintextLength).ToArray();
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
    /// and writes it atomically to the SSD cache. Does nothing (returns <c>false</c>) when no profile is logged in.
    /// </summary>
    public bool TryWriteChunk(string videoIdentity, int chunkSize, long chunkIndex, ReadOnlySpan<byte> plaintext) =>
        TryWriteChunk(TryBeginSession(), videoIdentity, chunkSize, chunkIndex, plaintext);

    /// <summary>Writes a chunk for <paramref name="session"/>; refused once that session has ended.</summary>
    public bool TryWriteChunk(CacheSession? session, string videoIdentity, int chunkSize, long chunkIndex, ReadOnlySpan<byte> plaintext)
    {
        if (string.IsNullOrWhiteSpace(videoIdentity) || plaintext.IsEmpty || chunkIndex < 0 ||
            chunkSize <= 0 || plaintext.Length > chunkSize)
        {
            return false;
        }

        var maxBytes = MaxCacheSizeBytes;
        if ((long)HeaderSizeBytes + chunkSize > maxBytes)
        {
            return false;
        }

        // The session is checked right before the write, so a chunk read under one session is never written under
        // the next one. Only the derivation holds its gate; the write-through I/O and the quota scan do not.
        using var material = TryDeriveMaterial(session, videoIdentity, chunkSize, chunkIndex, plaintext.Length);
        return material is not null && TryWriteChunkCore(material, chunkSize, plaintext, maxBytes);
    }

    private bool TryWriteChunkCore(ChunkMaterial material, int chunkSize, ReadOnlySpan<byte> plaintext, long maxBytes)
    {
        var chunkPath = material.Path;
        var layoutDir = LayoutDirectory;

        try
        {
            Directory.CreateDirectory(layoutDir);

            // Padded to the full chunk size: a last chunk stored at (fileLength mod chunkSize) + header bytes would
            // confirm a candidate file by its length alone.
            var padded = new byte[chunkSize];
            plaintext.CopyTo(padded);

            var packet = new byte[HeaderSizeBytes + chunkSize];
            MagicHeader.CopyTo(packet.AsSpan(0, 8));
            BinaryPrimitives.WriteInt32LittleEndian(packet.AsSpan(8, 4), chunkSize);

            var nonce = packet.AsSpan(12, 12);
            RandomNumberGenerator.Fill(nonce);
            var tag = packet.AsSpan(24, 16);
            var ciphertext = packet.AsSpan(HeaderSizeBytes, chunkSize);

            using (var aes = new AesGcm(material.Key, 16))
            {
                aes.Encrypt(nonce, padded, ciphertext, tag, material.Aad);
            }

            var tempPath = Path.Combine(layoutDir, $".{Guid.NewGuid():N}.tmp");
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

            SetFixedTimes(chunkPath, isDirectory: false);
            SetFixedTimes(layoutDir, isDirectory: true);

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

    /// <summary>
    /// Deletes everything under <see cref="CacheDirectory"/> that is not the current layout: folders named after
    /// hashes of profile names and video paths (unkeyed in v1, keyed per profile/video in v2), unpadded last chunks,
    /// offset-numbered chunk files, and the chunks earlier versions cached for logged-out playback.
    /// </summary>
    public void PurgeLegacyLayout()
    {
        lock (_quotaLock)
        {
            try
            {
                if (!Directory.Exists(CacheDirectory)) return;
                foreach (var dir in Directory.EnumerateDirectories(CacheDirectory, "*", SearchOption.TopDirectoryOnly))
                {
                    if (string.Equals(Path.GetFileName(dir), LayoutFolderName, StringComparison.OrdinalIgnoreCase)) continue;
                    try { Directory.Delete(dir, recursive: true); } catch { }
                }
                foreach (var file in Directory.EnumerateFiles(CacheDirectory, "*", SearchOption.TopDirectoryOnly))
                {
                    try { File.Delete(file); } catch { }
                }
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
            }
        }
    }

    /// <summary>
    /// Deletes chunk files until total usage is at or below <paramref name="maxBytes"/>. File times are fixed for
    /// privacy, so eviction follows the (pseudo-random) keyed file names rather than last use.
    /// </summary>
    public void EnforceQuota(long? maxBytes = null)
    {
        var limit = maxBytes ?? MaxCacheSizeBytes;
        lock (_quotaLock)
        {
            try
            {
                if (!Directory.Exists(CacheDirectory)) return;
                var files = new List<(string FullPath, long Length)>();
                var total = 0L;

                foreach (var path in Directory.EnumerateFiles(CacheDirectory, "*.vcache", SearchOption.AllDirectories))
                {
                    try
                    {
                        var info = new FileInfo(path);
                        if (!info.Exists) continue;
                        var len = info.Length;
                        total += len;
                        files.Add((path, len));
                    }
                    catch
                    {
                    }
                }

                if (total <= limit) return;

                foreach (var candidate in files.OrderBy(f => Path.GetFileName(f.FullPath), StringComparer.Ordinal))
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

    private static void SetFixedTimes(string path, bool isDirectory)
    {
        try
        {
            if (isDirectory)
            {
                Directory.SetCreationTimeUtc(path, FixedFileTimeUtc);
                Directory.SetLastWriteTimeUtc(path, FixedFileTimeUtc);
                Directory.SetLastAccessTimeUtc(path, FixedFileTimeUtc);
            }
            else
            {
                File.SetCreationTimeUtc(path, FixedFileTimeUtc);
                File.SetLastWriteTimeUtc(path, FixedFileTimeUtc);
                File.SetLastAccessTimeUtc(path, FixedFileTimeUtc);
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException)
        {
        }
    }

    // Opens a chunk for reading on a handle that never updates the file's last-access time (SetFileTime with
    // 0xFFFFFFFF'FFFFFFFF), so reading a cached chunk leaves no "last played at" record on volumes that keep access
    // times. Falls back to a plain read-only stream where that is not available.
    private static FileStream OpenForReadWithoutAccessTime(string path)
    {
        if (OperatingSystem.IsWindows())
        {
            var handle = NativeFile.CreateFileW(
                path,
                NativeFile.GenericRead | NativeFile.FileWriteAttributes,
                FileShare.ReadWrite | FileShare.Delete,
                IntPtr.Zero,
                FileMode.Open,
                NativeFile.FileFlagSequentialScan,
                IntPtr.Zero);
            if (!handle.IsInvalid)
            {
                var preserve = -1L;
                _ = NativeFile.SetFileTime(handle, IntPtr.Zero, ref preserve, IntPtr.Zero);
                return new FileStream(handle, FileAccess.Read, bufferSize: 0);
            }
            handle.Dispose();
        }
        return new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete, 4096, FileOptions.SequentialScan);
    }

    private static class NativeFile
    {
        public const uint GenericRead = 0x80000000;
        public const uint FileWriteAttributes = 0x00000100;
        public const uint FileFlagSequentialScan = 0x08000000;

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true, ExactSpelling = true)]
        [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
        public static extern SafeFileHandle CreateFileW(
            string lpFileName,
            uint dwDesiredAccess,
            FileShare dwShareMode,
            IntPtr lpSecurityAttributes,
            FileMode dwCreationDisposition,
            uint dwFlagsAndAttributes,
            IntPtr hTemplateFile);

        [DllImport("kernel32.dll", SetLastError = true, ExactSpelling = true)]
        [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool SetFileTime(SafeFileHandle hFile, IntPtr lpCreationTime, ref long lpLastAccessTime, IntPtr lpLastWriteTime);
    }

    // Flat layout: every chunk of every profile and video sits directly in the layout folder, so neither the folder
    // structure nor per-folder file counts tell how many profiles exist, how many videos one played or how much of each.
    private string GetChunkFilePath(CacheSession session, string videoIdentity, long chunkIndex) =>
        Path.Combine(
            LayoutDirectory,
            KeyedTag(
                session.VaultKey,
                string.Create(CultureInfo.InvariantCulture, $"SubtitleAdder.VideoCache.ChunkFile.v3|{session.ProfileTag}|{videoIdentity}|{chunkIndex}")) + ".vcache");

    private static string KeyedTag(byte[] vaultKey, string input)
    {
        Span<byte> mac = stackalloc byte[32];
        HMACSHA256.HashData(vaultKey, Encoding.UTF8.GetBytes(input), mac);
        return Convert.ToHexStringLower(mac)[..32];
    }

    private static byte[] DeriveChunkKey(CacheSession session, string videoIdentity, int chunkSize, long chunkIndex)
    {
        var context = Encoding.UTF8.GetBytes(
            string.Create(CultureInfo.InvariantCulture, $"SubtitleAdder.VideoCache.ChunkKey.v3|{session.ProfileTag}|{videoIdentity}|{chunkSize}|{chunkIndex}"));
        return HMACSHA256.HashData(session.VaultKey, context);
    }

    private static byte[] BuildChunkAad(CacheSession session, string videoIdentity, int chunkSize, long chunkIndex, int plaintextLength) =>
        Encoding.UTF8.GetBytes(
            string.Create(CultureInfo.InvariantCulture, $"SAVC3|{session.ProfileTag}|{videoIdentity}|{chunkSize}|{chunkIndex}|{plaintextLength}"));
}
