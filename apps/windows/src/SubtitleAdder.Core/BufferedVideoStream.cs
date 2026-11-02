namespace SubtitleAdder.Core;

/// <summary>
/// YouTube-style chunked read-ahead RAM-buffered stream for local video and audio playback.
/// <para>
/// Instead of letting Media Foundation issue thousands of tiny (16 KB – 64 KB) random reads that keep spinning
/// and wearing mechanical HDD heads, <see cref="BufferedVideoStream"/> reads the underlying file in large sequential
/// chunks (default <c>8 MB</c> per burst) and caches a sliding window (default up to <c>12</c> chunks = <c>96 MB</c>)
/// in RAM with background sequential read-ahead (<c>32 MB</c> ahead of the playback cursor).
/// </para>
/// <list type="bullet">
/// <item><description>Mechanical HDDs perform a quick sequential burst read and then stay idle while playback runs purely from RAM.</description></item>
/// <item><description>Short forward/backward seeks and MP4 <c>moov</c> header/trailer lookups hit the LRU RAM cache in 0 ms.</description></item>
/// <item><description>Disposing the stream when switching or closing a video immediately stops background read-ahead and releases RAM chunks.</description></item>
/// </list>
/// </summary>
public sealed class BufferedVideoStream : Stream
{
    /// <summary>Default burst chunk size read sequentially from disk (8 MB).</summary>
    public const int DefaultChunkSizeBytes = 8 * 1024 * 1024;

    /// <summary>Default maximum number of 8 MB chunks kept in RAM simultaneously (12 * 8 MB = 96 MB).</summary>
    public const int DefaultMaxCachedChunks = 12;

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

    private readonly Lock _cacheLock = new();
    private readonly Lock _streamIoLock = new();
    private readonly Dictionary<long, CachedChunk> _chunks = new();
    private readonly CancellationTokenSource _cts = new();

    private long _position;
    private long _lastReadChunkIndex = -1;
    private long _accessCounter;
    private int _underlyingReadCount;
    private long _underlyingBytesRead;
    private long _targetReadAheadFromChunk = -1;
    private int _readAheadWorkerRunning;
    private volatile bool _disposed;

    public BufferedVideoStream(
        string filePath,
        int chunkSizeBytes = DefaultChunkSizeBytes,
        int maxCachedChunks = DefaultMaxCachedChunks,
        int readAheadChunks = DefaultReadAheadChunks)
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
            readAheadChunks: readAheadChunks)
    {
        FilePath = filePath;
    }

    public BufferedVideoStream(
        Stream underlyingStream,
        bool ownsStream = true,
        int chunkSizeBytes = DefaultChunkSizeBytes,
        int maxCachedChunks = DefaultMaxCachedChunks,
        int readAheadChunks = DefaultReadAheadChunks)
    {
        ArgumentNullException.ThrowIfNull(underlyingStream);
        if (!underlyingStream.CanRead || !underlyingStream.CanSeek)
        {
            throw new ArgumentException("Underlying stream must be readable and seekable.", nameof(underlyingStream));
        }

        _underlying = underlyingStream;
        _ownsStream = ownsStream;
        _length = underlyingStream.Length;
        _chunkSize = Math.Max(64 * 1024, chunkSizeBytes);
        _maxCachedChunks = Math.Max(2, maxCachedChunks);
        _readAheadChunks = Math.Clamp(readAheadChunks, 0, _maxCachedChunks - 1);
        _totalChunks = _length <= 0 ? 0 : ((_length - 1) / _chunkSize) + 1;
    }

    /// <summary>Optional file path when backed by a local file.</summary>
    public string? FilePath { get; }

    /// <summary>Size of each sequential RAM cache chunk in bytes.</summary>
    public int ChunkSizeBytes => _chunkSize;

    /// <summary>Total number of burst chunk reads performed against the underlying disk stream.</summary>
    public int UnderlyingReadCount => Volatile.Read(ref _underlyingReadCount);

    /// <summary>Total bytes read from the underlying disk stream across all burst chunk loads.</summary>
    public long UnderlyingBytesRead => Interlocked.Read(ref _underlyingBytesRead);

    /// <summary>Current number of chunks resident in the RAM cache.</summary>
    public int CachedChunkCount
    {
        get
        {
            lock (_cacheLock) return _chunks.Count;
        }
    }

    /// <summary>
    /// Returns the normalized ratio (<c>0.0 .. 1.0</c>) of the media file currently buffered in RAM at and ahead of
    /// <paramref name="currentPlaybackRatio"/>, used to render the YouTube-style buffered progress bar on the player timeline.
    /// </summary>
    public double GetBufferedRatio(double currentPlaybackRatio = 0.0)
    {
        if (_disposed || _length <= 0 || _totalChunks <= 0) return 0.0;
        var clampedPlayback = Math.Clamp(double.IsFinite(currentPlaybackRatio) ? currentPlaybackRatio : 0.0, 0.0, 1.0);

        lock (_cacheLock)
        {
            if (_chunks.Count == 0) return clampedPlayback;
            if (_chunks.Count >= _totalChunks) return 1.0;

            var approxByteOffset = (long)(clampedPlayback * _length);
            var startChunk = Math.Clamp(approxByteOffset / _chunkSize, 0L, _totalChunks - 1);

            var contiguousEndByte = approxByteOffset;
            for (var c = startChunk; c < _totalChunks; c++)
            {
                if (!_chunks.ContainsKey(c)) break;
                contiguousEndByte = Math.Min(_length, (c + 1) * _chunkSize);
            }

            var ratioFromOffset = Math.Clamp((double)contiguousEndByte / _length, clampedPlayback, 1.0);

            var lastReadChunk = Interlocked.Read(ref _lastReadChunkIndex);
            if (lastReadChunk >= 0 && lastReadChunk < _totalChunks && _chunks.ContainsKey(lastReadChunk))
            {
                var contiguousChunks = 0L;
                for (var c = lastReadChunk; c < _totalChunks; c++)
                {
                    if (!_chunks.ContainsKey(c)) break;
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
    }

    public override long Seek(long offset, SeekOrigin origin)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
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
        ObjectDisposedException.ThrowIf(_disposed, this);
        if (buffer.IsEmpty) return 0;

        var currentPos = Interlocked.Read(ref _position);
        if (currentPos >= _length) return 0;

        var totalCopied = 0;
        var lastChunkIndex = -1L;

        while (!buffer.IsEmpty && currentPos < _length && !_disposed)
        {
            var chunkIndex = currentPos / _chunkSize;
            var chunkOffset = (int)(currentPos % _chunkSize);
            var chunk = GetOrLoadChunk(chunkIndex);
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
            Interlocked.Exchange(ref _lastReadChunkIndex, lastChunkIndex);
            if (_readAheadChunks > 0 && !_disposed)
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
        try
        {
            var read = Read(buffer.Span);
            return ValueTask.FromResult(read);
        }
        catch (Exception ex)
        {
            return ValueTask.FromException<int>(ex);
        }
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

    private CachedChunk GetOrLoadChunk(long chunkIndex)
    {
        lock (_cacheLock)
        {
            if (_chunks.TryGetValue(chunkIndex, out var cached))
            {
                cached.LastAccessTick = Interlocked.Increment(ref _accessCounter);
                return cached;
            }
        }

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

            ObjectDisposedException.ThrowIf(_disposed, this);

            var chunkStartOffset = chunkIndex * _chunkSize;
            var expectedBytes = (int)Math.Min(_chunkSize, Math.Max(0L, _length - chunkStartOffset));
            var data = new byte[expectedBytes];
            var totalRead = 0;

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

            var loaded = new CachedChunk(chunkIndex, data, totalRead, Interlocked.Increment(ref _accessCounter));

            lock (_cacheLock)
            {
                EvictIfNeededLocked(protectedChunkIndex: chunkIndex);
                _chunks[chunkIndex] = loaded;
            }

            return loaded;
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
                var startChunk = Interlocked.Exchange(ref _targetReadAheadFromChunk, -1);
                if (startChunk < 0) break;

                var endChunk = Math.Min(_totalChunks - 1, startChunk + _readAheadChunks - 1);
                for (var idx = startChunk; idx <= endChunk && !ct.IsCancellationRequested && !_disposed; idx++)
                {
                    bool alreadyCached;
                    lock (_cacheLock)
                    {
                        alreadyCached = _chunks.ContainsKey(idx);
                    }
                    if (!alreadyCached)
                    {
                        try
                        {
                            _ = GetOrLoadChunk(idx);
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
