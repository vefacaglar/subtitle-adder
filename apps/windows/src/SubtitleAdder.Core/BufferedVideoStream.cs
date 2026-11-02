using System.Collections.Concurrent;

namespace SubtitleAdder.Core;

/// <summary>
/// YouTube-style chunked read-ahead RAM + per-profile AES-256-GCM encrypted SSD buffered stream for local video and audio playback.
/// <para>
/// Instead of letting Media Foundation issue thousands of tiny (16 KB – 64 KB) random reads that keep spinning
/// and wearing mechanical HDD heads, <see cref="BufferedVideoStream"/> reads the underlying file in large sequential
/// chunks (default <c>8 MB</c> per burst), caches a sliding window in RAM (default <c>32</c> chunks = <c>256 MB</c>),
/// and persists every read chunk in <see cref="EncryptedVideoCacheStore"/> (default <c>20 GB</c> quota, AES-256-GCM
/// encrypted with the active profile's vault key).
/// </para>
/// <list type="bullet">
/// <item><description>Mechanical HDDs perform a quick sequential burst read once and then stay completely idle.</description></item>
/// <item><description>Forward/backward seeks and even reopening the video later hit RAM or the encrypted SSD cache with <b>0</b> HDD re-reads.</description></item>
/// <item><description>Cached chunks on disk are encrypted per profile with AES-256-GCM; neither external apps nor other profiles can read them.</description></item>
/// </list>
/// </summary>
public sealed class BufferedVideoStream : Stream
{
    /// <summary>Default burst chunk size read sequentially from disk (8 MB).</summary>
    public const int DefaultChunkSizeBytes = 8 * 1024 * 1024;

    /// <summary>Default maximum number of 8 MB chunks kept in RAM simultaneously (32 * 8 MB = 256 MB).</summary>
    public const int DefaultMaxCachedChunks = 32;

    /// <summary>Default number of chunks prefetched ahead of the current playback position (4 * 8 MB = 32 MB).</summary>
    public const int DefaultReadAheadChunks = 4;

    private sealed class CachedChunk(long index, byte[] data, int length, long accessTick)
    {
        public long Index { get; } = index;
        public byte[] Data { get; } = data;
        public int Length { get; } = length;
        public long LastAccessTick { get; set; } = accessTick;
    }

    private readonly Stream _underlying;
    private readonly bool _ownsStream;
    private readonly long _length;
    private readonly int _chunkSize;
    private readonly int _maxCachedChunks;
    private readonly int _readAheadChunks;
    private readonly long _totalChunks;
    private readonly EncryptedVideoCacheStore? _cacheStore;
    private readonly string? _videoIdentity;

    private readonly Lock _cacheLock = new();
    private readonly Lock _streamIoLock = new();
    private readonly Dictionary<long, CachedChunk> _chunks = new();
    private readonly HashSet<long> _persistedChunks = new();
    private readonly ConcurrentQueue<(long ChunkIndex, byte[] Data, int Length)> _pendingSsdWrites = new();
    private readonly CancellationTokenSource _cts = new();

    private long _position;
    private long _lastReadChunkIndex = -1;
    private long _accessCounter;
    private int _underlyingReadCount;
    private long _underlyingBytesRead;
    private int _encryptedCacheReadCount;
    private int _foregroundReadWaiters;
    private long _targetReadAheadFromChunk = -1;
    private int _readAheadWorkerRunning;
    private int _ssdWriteWorkerRunning;
    private volatile bool _disposed;

    public BufferedVideoStream(
        string filePath,
        int chunkSizeBytes = DefaultChunkSizeBytes,
        int maxCachedChunks = DefaultMaxCachedChunks,
        int readAheadChunks = DefaultReadAheadChunks,
        EncryptedVideoCacheStore? cacheStore = null)
        : this(
            new FileStream(
                filePath,
                FileMode.Open,
                FileAccess.Read,
                FileShare.ReadWrite | FileShare.Delete,
                bufferSize: 1,
                FileOptions.SequentialScan),
            ownsStream: true,
            chunkSizeBytes: chunkSizeBytes,
            maxCachedChunks: maxCachedChunks,
            readAheadChunks: readAheadChunks,
            cacheStore: cacheStore ?? EncryptedVideoCacheStore.Shared,
            videoIdentity: null,
            filePath: filePath)
    {
    }

    public BufferedVideoStream(
        Stream underlyingStream,
        bool ownsStream = true,
        int chunkSizeBytes = DefaultChunkSizeBytes,
        int maxCachedChunks = DefaultMaxCachedChunks,
        int readAheadChunks = DefaultReadAheadChunks,
        EncryptedVideoCacheStore? cacheStore = null,
        string? videoIdentity = null,
        string? filePath = null)
    {
        ArgumentNullException.ThrowIfNull(underlyingStream);
        if (!underlyingStream.CanRead || !underlyingStream.CanSeek)
        {
            throw new ArgumentException("Underlying stream must be readable and seekable.", nameof(underlyingStream));
        }

        FilePath = filePath;
        _underlying = underlyingStream;
        _ownsStream = ownsStream;
        _length = underlyingStream.Length;
        _chunkSize = Math.Max(64 * 1024, chunkSizeBytes);
        _maxCachedChunks = Math.Max(2, maxCachedChunks);
        _readAheadChunks = Math.Clamp(readAheadChunks, 0, _maxCachedChunks - 1);
        _totalChunks = _length <= 0 ? 0 : ((_length - 1) / _chunkSize) + 1;
        _cacheStore = cacheStore;
        _videoIdentity = !string.IsNullOrWhiteSpace(videoIdentity)
            ? videoIdentity
            : (cacheStore is not null && !string.IsNullOrWhiteSpace(filePath)
                ? EncryptedVideoCacheStore.ComputeVideoIdentity(filePath, _length, _chunkSize)
                : null);

        if (_cacheStore is not null && !string.IsNullOrWhiteSpace(_videoIdentity) && _totalChunks > 0)
        {
            if (ownsStream)
            {
                // Populate persisted chunk index map asynchronously so opening a video file returns in < 0.1 ms
                _ = Task.Run(LoadPersistedChunkIndices);
            }
            else
            {
                LoadPersistedChunkIndices();
            }
        }
    }

    private void LoadPersistedChunkIndices()
    {
        if (_disposed || _cacheStore is null || string.IsNullOrWhiteSpace(_videoIdentity) || _totalChunks <= 0) return;
        try
        {
            var existing = _cacheStore.GetCachedChunkIndices(_videoIdentity, _totalChunks, _chunkSize, _length);
            if (existing.Count == 0 || _disposed) return;
            lock (_cacheLock)
            {
                foreach (var idx in existing)
                {
                    _persistedChunks.Add(idx);
                }
            }
        }
        catch
        {
        }
    }

    /// <summary>Optional file path when backed by a local file.</summary>
    public string? FilePath { get; }

    /// <summary>Size of each sequential RAM cache chunk in bytes.</summary>
    public int ChunkSizeBytes => _chunkSize;

    /// <summary>Total number of burst chunk reads performed against the underlying disk stream (HDD).</summary>
    public int UnderlyingReadCount => Volatile.Read(ref _underlyingReadCount);

    /// <summary>Total bytes read from the underlying disk stream across all burst chunk loads.</summary>
    public long UnderlyingBytesRead => Interlocked.Read(ref _underlyingBytesRead);

    /// <summary>Total number of chunks satisfied from the AES-256-GCM encrypted persistent SSD cache without touching the underlying stream.</summary>
    public int EncryptedCacheReadCount => Volatile.Read(ref _encryptedCacheReadCount);

    /// <summary>Current number of chunks resident in the RAM cache.</summary>
    public int CachedChunkCount
    {
        get
        {
            lock (_cacheLock) return _chunks.Count;
        }
    }

    /// <summary>
    /// Returns the normalized ratio (<c>0.0 .. 1.0</c>) of the media file currently buffered in RAM or the active
    /// profile's encrypted SSD cache at and ahead of <paramref name="currentPlaybackRatio"/>.
    /// </summary>
    public double GetBufferedRatio(double currentPlaybackRatio = 0.0)
    {
        if (_disposed || _length <= 0 || _totalChunks <= 0) return 0.0;
        var clampedPlayback = Math.Clamp(double.IsFinite(currentPlaybackRatio) ? currentPlaybackRatio : 0.0, 0.0, 1.0);

        lock (_cacheLock)
        {
            if (_chunks.Count == 0 && _persistedChunks.Count == 0) return clampedPlayback;
            if (IsAllChunksBufferedLocked()) return 1.0;

            var approxByteOffset = (long)(clampedPlayback * _length);
            var startChunk = Math.Clamp(approxByteOffset / _chunkSize, 0L, _totalChunks - 1);

            var contiguousEndByte = approxByteOffset;
            for (var c = startChunk; c < _totalChunks; c++)
            {
                if (!IsChunkBufferedLocked(c)) break;
                contiguousEndByte = Math.Min(_length, (c + 1) * _chunkSize);
            }

            var ratioFromOffset = Math.Clamp((double)contiguousEndByte / _length, clampedPlayback, 1.0);

            var lastReadChunk = Interlocked.Read(ref _lastReadChunkIndex);
            if (lastReadChunk >= 0 && lastReadChunk < _totalChunks && IsChunkBufferedLocked(lastReadChunk))
            {
                var contiguousChunks = 0L;
                for (var c = lastReadChunk; c < _totalChunks; c++)
                {
                    if (!IsChunkBufferedLocked(c)) break;
                    contiguousChunks++;
                }
                if (lastReadChunk + contiguousChunks >= _totalChunks)
                {
                    return 1.0;
                }
                var aheadBytes = contiguousChunks * _chunkSize;
                var ratioFromLastRead = Math.Clamp(clampedPlayback + ((double)aheadBytes / _length), clampedPlayback, 1.0);
                return Math.Max(ratioFromOffset, ratioFromLastRead);
            }

            return ratioFromOffset;
        }
    }

    private bool IsChunkBufferedLocked(long chunkIndex) =>
        _chunks.ContainsKey(chunkIndex) || _persistedChunks.Contains(chunkIndex);

    private bool IsAllChunksBufferedLocked()
    {
        if (_chunks.Count >= _totalChunks || _persistedChunks.Count >= _totalChunks) return true;
        if (_chunks.Count + _persistedChunks.Count < _totalChunks) return false;
        for (var c = 0L; c < _totalChunks; c++)
        {
            if (!IsChunkBufferedLocked(c)) return false;
        }
        return true;
    }

    public override bool CanRead => !_disposed;
    public override bool CanSeek => !_disposed;
    public override bool CanWrite => false;
    public override long Length => _length;

    public override long Position
    {
        get => Interlocked.Read(ref _position);
        set => Seek(value, SeekOrigin.Begin);
    }

    public override void Flush()
    {
        DrainPendingSsdWrites();
    }

    public override long Seek(long offset, SeekOrigin origin)
    {
        if (_disposed) return Interlocked.Read(ref _position);
        var next = origin switch
        {
            SeekOrigin.Begin => offset,
            SeekOrigin.Current => Interlocked.Read(ref _position) + offset,
            SeekOrigin.End => _length + offset,
            _ => throw new ArgumentOutOfRangeException(nameof(origin)),
        };
        if (next < 0) throw new IOException("Cannot seek before the beginning of the stream.");
        Interlocked.Exchange(ref _position, next);
        return next;
    }

    public override void SetLength(long value) => throw new NotSupportedException();

    public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();

    public override int Read(byte[] buffer, int offset, int count)
    {
        ArgumentNullException.ThrowIfNull(buffer);
        ArgumentOutOfRangeException.ThrowIfNegative(offset);
        ArgumentOutOfRangeException.ThrowIfNegative(count);
        if (offset + count > buffer.Length) throw new ArgumentException("Buffer offset and count exceed buffer length.");
        return Read(buffer.AsSpan(offset, count));
    }

    public override int Read(Span<byte> buffer)
    {
        if (_disposed || buffer.IsEmpty) return 0;

        var currentPos = Interlocked.Read(ref _position);
        if (currentPos >= _length) return 0;

        var totalCopied = 0;
        var lastChunkIndex = -1L;

        while (!buffer.IsEmpty && currentPos < _length && !_disposed)
        {
            var chunkIndex = currentPos / _chunkSize;
            var chunkOffset = (int)(currentPos % _chunkSize);
            var chunk = GetOrLoadChunk(chunkIndex, isForegroundRead: true);
            var availableInChunk = chunk.Length - chunkOffset;
            if (availableInChunk <= 0) break;

            var toCopy = Math.Min(buffer.Length, availableInChunk);
            chunk.Data.AsSpan(chunkOffset, toCopy).CopyTo(buffer);

            buffer = buffer[toCopy..];
            currentPos += toCopy;
            totalCopied += toCopy;
            lastChunkIndex = chunkIndex;
        }

        Interlocked.Exchange(ref _position, currentPos);

        if (lastChunkIndex >= 0)
        {
            var prevChunk = Interlocked.Exchange(ref _lastReadChunkIndex, lastChunkIndex);
            var isSequential = prevChunk < 0 || lastChunkIndex == prevChunk || lastChunkIndex == prevChunk + 1;
            if (_readAheadChunks > 0 && isSequential && !_disposed)
            {
                ScheduleReadAhead(lastChunkIndex + 1);
            }
        }

        return totalCopied;
    }

    public override ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken cancellationToken = default)
    {
        if (cancellationToken.IsCancellationRequested)
        {
            return ValueTask.FromCanceled<int>(cancellationToken);
        }
        if (_disposed || buffer.IsEmpty)
        {
            return ValueTask.FromResult(0);
        }

        var pos = Interlocked.Read(ref _position);
        if (pos >= _length)
        {
            return ValueTask.FromResult(0);
        }

        var startChunk = pos / _chunkSize;
        var endChunk = Math.Min(_totalChunks - 1, (pos + buffer.Length - 1) / _chunkSize);
        var allInRam = true;
        lock (_cacheLock)
        {
            for (var c = startChunk; c <= endChunk; c++)
            {
                if (!_chunks.ContainsKey(c))
                {
                    allInRam = false;
                    break;
                }
            }
        }

        if (allInRam)
        {
            try
            {
                return ValueTask.FromResult(Read(buffer.Span));
            }
            catch (Exception ex)
            {
                return ValueTask.FromException<int>(ex);
            }
        }

        return new ValueTask<int>(Task.Run(() => Read(buffer.Span), cancellationToken));
    }

    public override Task<int> ReadAsync(byte[] buffer, int offset, int count, CancellationToken cancellationToken) =>
        ReadAsync(buffer.AsMemory(offset, count), cancellationToken).AsTask();

    /// <summary>Returns the standard MIME content type for a media file path so Media Foundation selects the right demuxer.</summary>
    public static string GetMimeType(string? filePath)
    {
        var ext = Path.GetExtension(filePath)?.ToLowerInvariant();
        return ext switch
        {
            ".mp4" or ".m4v" => "video/mp4",
            ".mkv" => "video/x-matroska",
            ".webm" => "video/webm",
            ".mov" => "video/quicktime",
            ".avi" => "video/x-msvideo",
            ".wmv" => "video/x-ms-wmv",
            ".mpg" or ".mpeg" => "video/mpeg",
            ".ts" or ".mts" or ".m2ts" => "video/mp2t",
            ".3gp" => "video/3gpp",
            ".flv" => "video/x-flv",
            ".mp3" => "audio/mpeg",
            ".wav" => "audio/wav",
            ".m4a" => "audio/mp4",
            ".aac" => "audio/aac",
            ".flac" => "audio/flac",
            ".ogg" or ".opus" => "audio/ogg",
            ".wma" => "audio/x-ms-wma",
            _ => "video/mp4",
        };
    }

    private CachedChunk GetOrLoadChunk(long chunkIndex, bool isForegroundRead)
    {
        // Fast path 1: Already in RAM cache (0 ms)
        lock (_cacheLock)
        {
            if (_chunks.TryGetValue(chunkIndex, out var cached))
            {
                cached.LastAccessTick = Interlocked.Increment(ref _accessCounter);
                return cached;
            }
        }

        var chunkStartOffset = chunkIndex * _chunkSize;
        var expectedBytes = (int)Math.Min(_chunkSize, Math.Max(0L, _length - chunkStartOffset));

        // Fast path 2: Already in per-profile AES-256-GCM encrypted SSD cache -> decrypt into RAM without locking HDD stream!
        if (expectedBytes > 0 && _cacheStore is not null && !string.IsNullOrWhiteSpace(_videoIdentity))
        {
            var decrypted = _cacheStore.TryReadChunk(_videoIdentity, _chunkSize, chunkIndex, expectedBytes);
            if (decrypted is not null && decrypted.Length == expectedBytes)
            {
                Interlocked.Increment(ref _encryptedCacheReadCount);
                var fromSsd = new CachedChunk(chunkIndex, decrypted, expectedBytes, Interlocked.Increment(ref _accessCounter));
                lock (_cacheLock)
                {
                    if (_chunks.TryGetValue(chunkIndex, out var existingRam))
                    {
                        existingRam.LastAccessTick = Interlocked.Increment(ref _accessCounter);
                        return existingRam;
                    }
                    EvictIfNeededLocked(protectedChunkIndex: chunkIndex);
                    _chunks[chunkIndex] = fromSsd;
                    _persistedChunks.Add(chunkIndex);
                }
                return fromSsd;
            }

            lock (_cacheLock)
            {
                _persistedChunks.Remove(chunkIndex);
            }
        }

        // Path 3: Read burst chunk sequentially from underlying stream (HDD)
        if (isForegroundRead)
        {
            Interlocked.Increment(ref _foregroundReadWaiters);
        }

        byte[] data;
        int totalRead;
        CachedChunk loaded;

        try
        {
            lock (_streamIoLock)
            {
                lock (_cacheLock)
                {
                    if (_chunks.TryGetValue(chunkIndex, out var cachedAfterWait))
                    {
                        cachedAfterWait.LastAccessTick = Interlocked.Increment(ref _accessCounter);
                        return cachedAfterWait;
                    }
                }

                if (_disposed)
                {
                    return new CachedChunk(chunkIndex, [], 0, 0);
                }

                data = new byte[expectedBytes];
                totalRead = 0;

                if (expectedBytes > 0)
                {
                    if (_underlying.Position != chunkStartOffset)
                    {
                        _underlying.Seek(chunkStartOffset, SeekOrigin.Begin);
                    }

                    while (totalRead < expectedBytes)
                    {
                        var n = _underlying.Read(data, totalRead, expectedBytes - totalRead);
                        if (n <= 0) break;
                        totalRead += n;
                    }

                    Interlocked.Increment(ref _underlyingReadCount);
                    Interlocked.Add(ref _underlyingBytesRead, totalRead);
                }

                loaded = new CachedChunk(chunkIndex, data, totalRead, Interlocked.Increment(ref _accessCounter));

                lock (_cacheLock)
                {
                    EvictIfNeededLocked(protectedChunkIndex: chunkIndex);
                    _chunks[chunkIndex] = loaded;
                    if (totalRead > 0 && _cacheStore is not null && !string.IsNullOrWhiteSpace(_videoIdentity))
                    {
                        _persistedChunks.Add(chunkIndex);
                    }
                }
            }
        }
        finally
        {
            if (isForegroundRead)
            {
                Interlocked.Decrement(ref _foregroundReadWaiters);
            }
        }

        if (totalRead > 0 && _cacheStore is not null && !string.IsNullOrWhiteSpace(_videoIdentity))
        {
            if (_ownsStream)
            {
                // Write encrypted chunk to SSD asynchronously so foreground Media Foundation reads return in 0 ms
                EnqueueSsdWrite(chunkIndex, data, totalRead);
            }
            else
            {
                // Synchronous write for in-memory test streams so unit tests can immediately inspect .vcache files
                _cacheStore.TryWriteChunk(_videoIdentity, _chunkSize, chunkIndex, data.AsSpan(0, totalRead));
            }
        }

        return loaded;
    }

    private void EnqueueSsdWrite(long chunkIndex, byte[] data, int length)
    {
        if (_disposed || _cacheStore is null || string.IsNullOrWhiteSpace(_videoIdentity)) return;
        _pendingSsdWrites.Enqueue((chunkIndex, data, length));
        if (Interlocked.CompareExchange(ref _ssdWriteWorkerRunning, 1, 0) == 0)
        {
            _ = Task.Run(DrainPendingSsdWrites);
        }
    }

    private void DrainPendingSsdWrites()
    {
        try
        {
            while (_pendingSsdWrites.TryDequeue(out var item))
            {
                if (_cacheStore is null || string.IsNullOrWhiteSpace(_videoIdentity)) break;
                try
                {
                    _cacheStore.TryWriteChunk(_videoIdentity, _chunkSize, item.ChunkIndex, item.Data.AsSpan(0, item.Length));
                }
                catch
                {
                }
            }
        }
        finally
        {
            Interlocked.Exchange(ref _ssdWriteWorkerRunning, 0);
            if (!_pendingSsdWrites.IsEmpty && Interlocked.CompareExchange(ref _ssdWriteWorkerRunning, 1, 0) == 0)
            {
                _ = Task.Run(DrainPendingSsdWrites);
            }
        }
    }

    private void EvictIfNeededLocked(long protectedChunkIndex)
    {
        while (_chunks.Count >= _maxCachedChunks)
        {
            long victimKey = -1;
            var oldestTick = long.MaxValue;

            foreach (var (key, entry) in _chunks)
            {
                if (key == protectedChunkIndex) continue;
                if (entry.LastAccessTick < oldestTick)
                {
                    oldestTick = entry.LastAccessTick;
                    victimKey = key;
                }
            }

            if (victimKey < 0 || !_chunks.Remove(victimKey))
            {
                break;
            }
        }
    }

    private void ScheduleReadAhead(long startChunkIndex)
    {
        if (startChunkIndex < 0 || startChunkIndex >= _totalChunks || _disposed) return;
        Interlocked.Exchange(ref _targetReadAheadFromChunk, startChunkIndex);

        if (Interlocked.CompareExchange(ref _readAheadWorkerRunning, 1, 0) == 0)
        {
            var token = _cts.Token;
            _ = Task.Run(() => RunReadAheadLoop(token), CancellationToken.None);
        }
    }

    private void RunReadAheadLoop(CancellationToken ct)
    {
        try
        {
            while (!ct.IsCancellationRequested && !_disposed)
            {
                if (Volatile.Read(ref _foregroundReadWaiters) > 0)
                {
                    Thread.Yield();
                    continue;
                }

                var startChunk = Interlocked.Exchange(ref _targetReadAheadFromChunk, -1);
                if (startChunk < 0) break;

                var endChunk = Math.Min(_totalChunks - 1, startChunk + _readAheadChunks - 1);
                for (var idx = startChunk; idx <= endChunk && !ct.IsCancellationRequested && !_disposed; idx++)
                {
                    if (Volatile.Read(ref _foregroundReadWaiters) > 0)
                    {
                        // Yield immediately to foreground Media Foundation seeks/reads
                        Interlocked.CompareExchange(ref _targetReadAheadFromChunk, idx, -1);
                        break;
                    }

                    bool alreadyInRam;
                    lock (_cacheLock)
                    {
                        alreadyInRam = _chunks.ContainsKey(idx);
                    }
                    if (!alreadyInRam)
                    {
                        try
                        {
                            _ = GetOrLoadChunk(idx, isForegroundRead: false);
                        }
                        catch
                        {
                            return;
                        }
                    }
                }
            }
        }
        finally
        {
            Interlocked.Exchange(ref _readAheadWorkerRunning, 0);
        }
    }

    protected override void Dispose(bool disposing)
    {
        if (!_disposed)
        {
            _disposed = true;
            try { _cts.Cancel(); } catch { }

            lock (_cacheLock)
            {
                _chunks.Clear();
            }

            if (disposing)
            {
                DrainPendingSsdWrites();
                lock (_streamIoLock)
                {
                    if (_ownsStream)
                    {
                        try { _underlying.Dispose(); } catch { }
                    }
                }
                try { _cts.Dispose(); } catch { }
            }
        }
        base.Dispose(disposing);
    }
}
