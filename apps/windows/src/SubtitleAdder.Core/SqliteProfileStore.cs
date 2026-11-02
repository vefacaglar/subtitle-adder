using System.Collections.Concurrent;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading.Channels;

namespace SubtitleAdder.Core;

/// <summary>
/// Embedded SQLite WAL storage engine backed by Windows built-in <c>winsqlite3.dll</c> with a dedicated
/// asynchronous <see cref="Channel{T}"/> write queue and normalized encrypted tables:
/// <list type="bullet">
/// <item><description><c>meta</c>: global session metadata</description></item>
/// <item><description><c>profiles</c>: PBKDF2-SHA256 authentication records</description></item>
/// <item><description><c>videos</c>: AES-256-GCM encrypted per-video metadata, view counts, likes, and ratings</description></item>
/// <item><description><c>watch_progress</c>: AES-256-GCM encrypted per-video playback position &amp; duration (coalesced)</description></item>
/// <item><description><c>custom_lists</c>: AES-256-GCM encrypted custom list headers</description></item>
/// <item><description><c>custom_list_items</c>: AES-256-GCM encrypted individual video memberships per list</description></item>
/// <item><description><c>thumbnails</c>: AES-256-GCM encrypted binary <c>BLOB</c> thumbnails (never Base64)</description></item>
/// </list>
/// </summary>
internal sealed class SqliteProfileStore : IDisposable
{
    private const string SqliteDll = "winsqlite3.dll";
    private const int SqliteOk = 0;
    private const int SqliteBusy = 5;
    private const int SqliteLocked = 6;
    private const int SqliteRow = 100;
    private const int SqliteDone = 101;
    private const int SqliteOpenReadWrite = 0x00000002;
    private const int SqliteOpenCreate = 0x00000004;
    private const int SqliteOpenFullMutex = 0x00010000;
    private static readonly IntPtr SqliteTransient = new(-1);

    private static readonly ConcurrentDictionary<string, ConcurrentDictionary<SqliteProfileStore, byte>> ActiveStoresByPath =
        new(StringComparer.OrdinalIgnoreCase);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_open_v2(byte[] filename, out IntPtr ppDb, int flags, IntPtr zVfs);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_close_v2(IntPtr db);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_busy_timeout(IntPtr db, int ms);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_exec(IntPtr db, byte[] sql, IntPtr callback, IntPtr arg, out IntPtr errmsg);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern void sqlite3_free(IntPtr ptr);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_prepare_v2(IntPtr db, byte[] zSql, int nByte, out IntPtr ppStmt, out IntPtr pzTail);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_step(IntPtr pStmt);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_finalize(IntPtr pStmt);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_bind_text(IntPtr pStmt, int index, byte[] value, int nByte, IntPtr destructor);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_bind_blob(IntPtr pStmt, int index, byte[] value, int nByte, IntPtr destructor);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_bind_int64(IntPtr pStmt, int index, long value);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_bind_null(IntPtr pStmt, int index);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern IntPtr sqlite3_column_text(IntPtr pStmt, int iCol);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern IntPtr sqlite3_column_blob(IntPtr pStmt, int iCol);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int sqlite3_column_bytes(IntPtr pStmt, int iCol);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(SqliteDll, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern long sqlite3_column_int64(IntPtr pStmt, int iCol);

    internal enum WriteKind
    {
        UpsertProfile,
        DeleteProfileCascade,
        UpsertVideo,
        DeleteVideo,
        ReplaceAllVideosAndProgress,
        UpsertWatchProgress,
        UpsertCustomList,
        DeleteCustomList,
        UpsertCustomListItem,
        DeleteCustomListItem,
        ReplaceCustomListItems,
        UpsertThumbnail,
        MigrateVideoKey,
        FlushBarrier,
    }

    internal readonly record struct ListItemRow(string ListId, string ItemHash, int Ord, byte[] EncryptedPathBlob);

    internal sealed record WriteItem(
        WriteKind Kind,
        string Username = "",
        string Key = "",
        string SecondaryKey = "",
        int Ord = 0,
        byte[]? Blob = null,
        byte[]? SecondaryBlob = null,
        long UpdatedTicks = 0,
        int ViewCount = -1,
        int ViewCountDelta = 0,
        string RequestId = "",
        string? SaltBase64 = null,
        string? PasswordHashBase64 = null,
        int Iterations = 0,
        DateTimeOffset CreatedAtUtc = default,
        IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)>? BatchVideos = null,
        IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)>? BatchProgress = null,
        IReadOnlyList<ListItemRow>? BatchListItems = null,
        IReadOnlyList<StateSyncEvent>? PostCommitEvents = null,
        Action<bool, bool, string?>? OnCompleted = null,
        TaskCompletionSource<bool>? Barrier = null);

    public static Action<string>? BeforeFlushPathHook { get; set; }

    private readonly Lock _dbLock = new();
    private readonly Lock _queueDrainLock = new();
    private readonly List<WriteItem> _retryItems = new(32);
    private readonly ConcurrentDictionary<(string User, string Key), byte[]> _recentThumbnailBlobs = new();
    private readonly Channel<WriteItem> _writeChannel = Channel.CreateUnbounded<WriteItem>(new UnboundedChannelOptions
    {
        SingleReader = false,
        SingleWriter = false,
        AllowSynchronousContinuations = false,
    });
    private readonly Task _writerLoopTask;
    private IntPtr _db;
    private volatile bool _disposed;
    private int _pendingWriteCount;
    private int _consecutiveFailures;
    private volatile bool _lastBatchSucceeded = true;
    private long _totalBatchesCommitted;
    private long _totalOperationsCommitted;
    private long _totalWatchProgressUpserts;
    private long _totalDuplicateRequestsIgnored;
    private volatile string? _lastWriteError;
    private volatile bool _hasPermanentWriteError;
    private const int MaxRetryAttempts = 60;

    public string DbPath { get; }
    public bool IsDisposed => _disposed;
    public int PendingWriteCount => Volatile.Read(ref _pendingWriteCount);
    public long TotalBatchesCommitted => Interlocked.Read(ref _totalBatchesCommitted);
    public long TotalOperationsCommitted => Interlocked.Read(ref _totalOperationsCommitted);
    public long TotalWatchProgressUpserts => Interlocked.Read(ref _totalWatchProgressUpserts);
    public long TotalDuplicateRequestsIgnored => Interlocked.Read(ref _totalDuplicateRequestsIgnored);
    public string? LastWriteError => _lastWriteError;
    public bool HasPermanentWriteError => _hasPermanentWriteError;
    public Action<IReadOnlyList<StateSyncEvent>>? PostCommitBroadcastHandler { get; set; }

    public SqliteProfileStore(string dbPath)
    {
        DbPath = Path.GetFullPath(dbPath);
        EnsureInitialized();
        var set = ActiveStoresByPath.GetOrAdd(DbPath, static _ => new ConcurrentDictionary<SqliteProfileStore, byte>());
        set[this] = 0;
        _writerLoopTask = Task.Run(ProcessWriteQueueAsync);
    }

    public static void FlushForPath(string dbPath)
    {
        var full = Path.GetFullPath(dbPath);
        try { BeforeFlushPathHook?.Invoke(full); } catch { }
        if (!ActiveStoresByPath.TryGetValue(full, out var stores)) return;
        foreach (var store in stores.Keys)
        {
            if (!store._disposed && store.PendingWriteCount > 0)
            {
                store.Flush(TimeSpan.FromSeconds(5));
            }
        }
    }

    public static void FlushAllForPath(string dbPath)
    {
        var full = Path.GetFullPath(dbPath);
        FlushForPath(full);
        if (!ActiveStoresByPath.TryGetValue(full, out var stores)) return;
        foreach (var store in stores.Keys)
        {
            if (!store._disposed && store.PendingWriteCount == 0)
            {
                store.CloseConnectionIfIdle();
            }
        }
    }

    public void CloseConnectionIfIdle()
    {
        if (PendingWriteCount > 0) return;
        lock (_dbLock)
        {
            if (PendingWriteCount == 0 && _db != IntPtr.Zero)
            {
                sqlite3_close_v2(_db);
                _db = IntPtr.Zero;
            }
        }
    }

    public Task<bool> FlushAsync()
    {
        if (_disposed) return Task.FromResult(false);
        try { BeforeFlushPathHook?.Invoke(DbPath); } catch { }
        var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        if (!_writeChannel.Writer.TryWrite(new WriteItem(WriteKind.FlushBarrier, Barrier: tcs)))
        {
            return Task.FromResult(false);
        }
        return tcs.Task;
    }

    public bool Flush(TimeSpan? timeout = null)
    {
        if (_disposed) return false;
        try
        {
            var task = FlushAsync();
            if (!task.IsCompleted)
            {
                if (!task.Wait(timeout ?? TimeSpan.FromSeconds(5)))
                {
                    return false;
                }
            }
            return task.IsCompletedSuccessfully && task.Result;
        }
        catch
        {
            return false;
        }
    }

    internal void EnqueueItem(WriteItem item) => Enqueue(item);

    private void Enqueue(WriteItem item)
    {
        if (_disposed)
        {
            item.OnCompleted?.Invoke(false, false, "Store is disposed.");
            return;
        }
        if (item.Kind == WriteKind.UpsertThumbnail && item.Blob is { Length: > 0 } thumbBlob)
        {
            _recentThumbnailBlobs[(item.Username.ToUpperInvariant(), item.Key.ToUpperInvariant())] = thumbBlob;
        }
        else if (item.Kind == WriteKind.MigrateVideoKey && !string.Equals(item.Key, item.SecondaryKey, StringComparison.OrdinalIgnoreCase))
        {
            if (_recentThumbnailBlobs.TryRemove((item.Username.ToUpperInvariant(), item.Key.ToUpperInvariant()), out var movedThumb))
            {
                _recentThumbnailBlobs[(item.Username.ToUpperInvariant(), item.SecondaryKey.ToUpperInvariant())] = movedThumb;
            }
        }
        else if (item.Kind == WriteKind.DeleteProfileCascade)
        {
            var userUpper = item.Username.ToUpperInvariant();
            foreach (var k in _recentThumbnailBlobs.Keys)
            {
                if (string.Equals(k.User, userUpper, StringComparison.Ordinal))
                {
                    _recentThumbnailBlobs.TryRemove(k, out _);
                }
            }
        }

        Interlocked.Increment(ref _pendingWriteCount);
        if (!_writeChannel.Writer.TryWrite(item))
        {
            Interlocked.Decrement(ref _pendingWriteCount);
            item.OnCompleted?.Invoke(false, false, "Write channel closed.");
        }
    }

    public void EnqueueUpsertProfile(
        string username,
        string saltBase64,
        string passwordHashBase64,
        int iterations,
        DateTimeOffset createdAtUtc,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.UpsertProfile,
            Username: username,
            RequestId: requestId ?? "",
            SaltBase64: saltBase64,
            PasswordHashBase64: passwordHashBase64,
            Iterations: iterations,
            CreatedAtUtc: createdAtUtc,
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueDeleteProfileCascade(
        string username,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.DeleteProfileCascade,
            Username: username,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueUpsertVideo(
        string username,
        string videoKey,
        byte[] encryptedBlob,
        long updatedAtTicks,
        byte[]? encryptedProgressBlob = null,
        int viewCount = -1,
        int viewCountDelta = 0,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.UpsertVideo,
            Username: username,
            Key: videoKey,
            Blob: encryptedBlob,
            SecondaryBlob: encryptedProgressBlob,
            UpdatedTicks: updatedAtTicks,
            ViewCount: viewCount,
            ViewCountDelta: viewCountDelta,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueUpsertWatchProgress(
        string username,
        string videoKey,
        byte[] encryptedBlob,
        long updatedAtTicks,
        byte[]? newVideoEncryptedBlob = null,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.UpsertWatchProgress,
            Username: username,
            Key: videoKey,
            Blob: encryptedBlob,
            SecondaryBlob: newVideoEncryptedBlob,
            UpdatedTicks: updatedAtTicks,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueDeleteVideo(
        string username,
        string videoKey,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.DeleteVideo,
            Username: username,
            Key: videoKey,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueReplaceAllVideosAndProgress(
        string username,
        IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)> videos,
        IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)> progress,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.ReplaceAllVideosAndProgress,
            Username: username,
            BatchVideos: videos,
            BatchProgress: progress,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueUpsertCustomList(
        string username,
        string listId,
        byte[] encryptedBlob,
        long updatedAtTicks,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.UpsertCustomList,
            Username: username,
            Key: listId,
            Blob: encryptedBlob,
            UpdatedTicks: updatedAtTicks,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueDeleteCustomList(
        string username,
        string listId,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.DeleteCustomList,
            Username: username,
            Key: listId,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueUpsertCustomListItem(
        string username,
        string listId,
        string itemHash,
        int ord,
        byte[] encryptedPathBlob,
        byte[]? listHeaderEncryptedBlob = null,
        long updatedAtTicks = 0,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.UpsertCustomListItem,
            Username: username,
            Key: listId,
            SecondaryKey: itemHash,
            Ord: ord,
            Blob: encryptedPathBlob,
            SecondaryBlob: listHeaderEncryptedBlob,
            UpdatedTicks: updatedAtTicks,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueDeleteCustomListItem(
        string username,
        string listId,
        string itemHash,
        byte[]? listHeaderEncryptedBlob = null,
        long updatedAtTicks = 0,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.DeleteCustomListItem,
            Username: username,
            Key: listId,
            SecondaryKey: itemHash,
            SecondaryBlob: listHeaderEncryptedBlob,
            UpdatedTicks: updatedAtTicks,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueReplaceCustomListItems(
        string username,
        string listId,
        IReadOnlyList<ListItemRow> items,
        byte[]? listHeaderEncryptedBlob = null,
        long updatedAtTicks = 0,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.ReplaceCustomListItems,
            Username: username,
            Key: listId,
            BatchListItems: items,
            SecondaryBlob: listHeaderEncryptedBlob,
            UpdatedTicks: updatedAtTicks,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueUpsertThumbnail(
        string username,
        string videoKey,
        byte[] encryptedBlob,
        long updatedAtTicks,
        byte[]? newVideoEncryptedBlob = null,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.UpsertThumbnail,
            Username: username,
            Key: videoKey,
            Blob: encryptedBlob,
            SecondaryBlob: newVideoEncryptedBlob,
            UpdatedTicks: updatedAtTicks,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    public void EnqueueMigrateVideoKey(
        string username,
        string oldKey,
        string newKey,
        byte[] newVideoBlob,
        byte[] newProgressBlob,
        long updatedAtTicks,
        int viewCount = -1,
        string? requestId = null,
        IReadOnlyList<StateSyncEvent>? postCommitEvents = null,
        Action<bool, bool, string?>? onCompleted = null) =>
        Enqueue(new WriteItem(
            WriteKind.MigrateVideoKey,
            Username: username,
            Key: oldKey,
            SecondaryKey: newKey,
            Blob: newVideoBlob,
            SecondaryBlob: newProgressBlob,
            UpdatedTicks: updatedAtTicks,
            ViewCount: viewCount,
            RequestId: requestId ?? "",
            PostCommitEvents: postCommitEvents,
            OnCompleted: onCompleted));

    private async Task ProcessWriteQueueAsync()
    {
        try
        {
            while (await _writeChannel.Reader.WaitToReadAsync().ConfigureAwait(false))
            {
                lock (_queueDrainLock)
                {
                    DrainAvailableWriteBatchLocked();
                }
            }
        }
        catch
        {
            while (_writeChannel.Reader.TryRead(out var leftover))
            {
                leftover.Barrier?.TrySetResult(false);
            }
        }
    }

    private void TryDrainWhenHealthyNonBlocking()
    {
        if (_disposed) return;
        try { BeforeFlushPathHook?.Invoke(DbPath); } catch { }
        if (Volatile.Read(ref _pendingWriteCount) <= 0 || _lastWriteError is not null)
        {
            return;
        }
        if (!_queueDrainLock.TryEnter())
        {
            return;
        }
        try
        {
            if (_retryItems.Count == 0 && Volatile.Read(ref _pendingWriteCount) > 0)
            {
                DrainAvailableWriteBatchLocked();
            }
        }
        finally
        {
            _queueDrainLock.Exit();
        }
    }

    private void DrainAvailableWriteBatchLocked()
    {
        var batch = new List<WriteItem>(32);
        var barriers = new List<TaskCompletionSource<bool>>(4);
        if (_retryItems.Count > 0)
        {
            batch.AddRange(_retryItems);
        }

        while (_writeChannel.Reader.TryRead(out var item))
        {
            if (item.Kind == WriteKind.FlushBarrier)
            {
                if (item.Barrier is not null) barriers.Add(item.Barrier);
            }
            else
            {
                batch.Add(item);
            }
        }

        if (batch.Count > 0)
        {
            long committedWatchProgressCount = 0;
            long duplicateCount = 0;
            bool[]? duplicateFlags = null;
            try
            {
                _lastBatchSucceeded = TryExecuteCoalescedBatch(
                    batch,
                    out committedWatchProgressCount,
                    out duplicateCount,
                    out duplicateFlags);
            }
            catch (Exception ex)
            {
                _lastBatchSucceeded = false;
                _lastWriteError = ex.Message;
                DiagnosticLog.Write("SQLite", $"Unhandled batch write exception: {ex.Message}");
            }

            if (_lastBatchSucceeded)
            {
                var committedCount = batch.Count;
                _retryItems.Clear();
                _consecutiveFailures = 0;
                _lastWriteError = null;
                Interlocked.Increment(ref _totalBatchesCommitted);
                Interlocked.Add(ref _totalOperationsCommitted, committedCount);
                if (committedWatchProgressCount > 0)
                {
                    Interlocked.Add(ref _totalWatchProgressUpserts, committedWatchProgressCount);
                }
                if (duplicateCount > 0)
                {
                    Interlocked.Add(ref _totalDuplicateRequestsIgnored, duplicateCount);
                }
                Interlocked.Add(ref _pendingWriteCount, -committedCount);

                for (var i = 0; i < batch.Count; i++)
                {
                    var bItem = batch[i];
                    var isDup = duplicateFlags is not null && i < duplicateFlags.Length && duplicateFlags[i];
                    try
                    {
                        bItem.OnCompleted?.Invoke(true, isDup, null);
                    }
                    catch
                    {
                    }
                    if (!isDup && bItem.PostCommitEvents is { Count: > 0 } postEvents)
                    {
                        try
                        {
                            PostCommitBroadcastHandler?.Invoke(postEvents);
                        }
                        catch
                        {
                        }
                    }
                }
            }
            else
            {
                _consecutiveFailures++;
                if (_consecutiveFailures >= MaxRetryAttempts || _disposed)
                {
                    _hasPermanentWriteError = true;
                    var droppedCount = batch.Count;
                    _retryItems.Clear();
                    Interlocked.Add(ref _pendingWriteCount, -droppedCount);
                    foreach (var bItem in batch)
                    {
                        try { bItem.OnCompleted?.Invoke(false, false, _lastWriteError); } catch { }
                    }
                }
                else
                {
                    _retryItems.Clear();
                    _retryItems.AddRange(batch);
                    _ = Task.Delay(100).ContinueWith(
                        _ =>
                        {
                            if (!_disposed)
                            {
                                _writeChannel.Writer.TryWrite(new WriteItem(WriteKind.FlushBarrier));
                            }
                        },
                        TaskScheduler.Default);
                }
            }
        }

        var barrierResult = _lastBatchSucceeded && !_hasPermanentWriteError;
        foreach (var barrier in barriers)
        {
            barrier.TrySetResult(barrierResult);
        }
    }

    private bool TryExecuteCoalescedBatch(
        List<WriteItem> batch,
        out long committedWatchProgressCount,
        out long duplicateCount,
        out bool[] duplicateFlags)
    {
        committedWatchProgressCount = 0;
        duplicateCount = 0;
        duplicateFlags = new bool[batch.Count];

        lock (_dbLock)
        {
            if (!EnsureConnectionLocked())
            {
                _lastWriteError = "Database connection is not open.";
                return false;
            }
            if (!TryExecNonQueryLocked("BEGIN IMMEDIATE;", out var beginErr))
            {
                _lastWriteError = beginErr ?? "BEGIN IMMEDIATE failed.";
                DiagnosticLog.Write("SQLite", $"BEGIN IMMEDIATE failed on '{DbPath}': {_lastWriteError}");
                return false;
            }
            try
            {
                // 1. Identify duplicate RequestIds (either already in processed_requests or repeated in the same batch).
                var seenRequestIdsInBatch = new HashSet<string>(StringComparer.Ordinal);
                long dupCount = 0;
                for (var i = 0; i < batch.Count; i++)
                {
                    var reqId = batch[i].RequestId;
                    if (!string.IsNullOrEmpty(reqId))
                    {
                        if (!seenRequestIdsInBatch.Add(reqId) || IsRequestProcessedLocked(reqId))
                        {
                            duplicateFlags[i] = true;
                            dupCount++;
                        }
                    }
                }

                // 2. Coalesce non-duplicate UpsertVideo, UpsertWatchProgress, and UpsertThumbnail operations on the same (Username, Key),
                // while summing ViewCountDelta across distinct RequestIds for the same (Username, Key).
                var lastIndexForVideoKey = new Dictionary<(string User, string Key), int>();
                var accumulatedViewCountDelta = new Dictionary<(string User, string Key), int>();
                var lastProgressBlobForVideoKey = new Dictionary<(string User, string Key), byte[]>();
                var lastIndexForProgressKey = new Dictionary<(string User, string Key), int>();
                var lastIndexForThumbKey = new Dictionary<(string User, string Key), int>();

                for (var i = 0; i < batch.Count; i++)
                {
                    if (duplicateFlags[i]) continue;
                    var item = batch[i];
                    var pair = (item.Username.ToUpperInvariant(), item.Key.ToUpperInvariant());
                    switch (item.Kind)
                    {
                        case WriteKind.UpsertVideo:
                            lastIndexForVideoKey[pair] = i;
                            if (item.ViewCountDelta > 0)
                            {
                                accumulatedViewCountDelta[pair] = accumulatedViewCountDelta.GetValueOrDefault(pair, 0) + item.ViewCountDelta;
                            }
                            if (item.SecondaryBlob is not null)
                            {
                                lastProgressBlobForVideoKey[pair] = item.SecondaryBlob;
                            }
                            break;
                        case WriteKind.UpsertWatchProgress:
                            lastIndexForProgressKey[pair] = i;
                            break;
                        case WriteKind.UpsertThumbnail:
                            lastIndexForThumbKey[pair] = i;
                            break;
                        case WriteKind.DeleteVideo or WriteKind.ReplaceAllVideosAndProgress or WriteKind.MigrateVideoKey or WriteKind.DeleteProfileCascade:
                            lastIndexForVideoKey.Clear();
                            accumulatedViewCountDelta.Clear();
                            lastProgressBlobForVideoKey.Clear();
                            lastIndexForProgressKey.Clear();
                            break;
                    }
                }

                long progressCount = 0;
                for (var i = 0; i < batch.Count; i++)
                {
                    if (duplicateFlags[i]) continue;
                    var item = batch[i];
                    var pair = (item.Username.ToUpperInvariant(), item.Key.ToUpperInvariant());
                    if (item.Kind == WriteKind.UpsertVideo &&
                        lastIndexForVideoKey.TryGetValue(pair, out var latestVideoIdx) &&
                        latestVideoIdx != i)
                    {
                        if (!string.IsNullOrEmpty(item.RequestId))
                        {
                            RecordProcessedRequestLocked(item.RequestId, item.Username, item.UpdatedTicks);
                        }
                        continue;
                    }
                    if (item.Kind == WriteKind.UpsertWatchProgress &&
                        lastIndexForProgressKey.TryGetValue(pair, out var latestProgIdx) &&
                        latestProgIdx != i)
                    {
                        if (!string.IsNullOrEmpty(item.RequestId))
                        {
                            RecordProcessedRequestLocked(item.RequestId, item.Username, item.UpdatedTicks);
                        }
                        continue;
                    }
                    if (item.Kind == WriteKind.UpsertThumbnail &&
                        lastIndexForThumbKey.TryGetValue(pair, out var latestThumbIdx) &&
                        latestThumbIdx != i)
                    {
                        if (!string.IsNullOrEmpty(item.RequestId))
                        {
                            RecordProcessedRequestLocked(item.RequestId, item.Username, item.UpdatedTicks);
                        }
                        continue;
                    }

                    var effectiveDelta = item.Kind == WriteKind.UpsertVideo
                        ? accumulatedViewCountDelta.GetValueOrDefault(pair, item.ViewCountDelta)
                        : item.ViewCountDelta;
                    var effectiveSecondaryBlob = item.Kind == WriteKind.UpsertVideo && item.SecondaryBlob is null &&
                        lastProgressBlobForVideoKey.TryGetValue(pair, out var coalescedProgBlob)
                        ? coalescedProgBlob
                        : item.SecondaryBlob;

                    ExecuteSingleWriteLocked(item, effectiveDelta, effectiveSecondaryBlob);
                    if (!string.IsNullOrEmpty(item.RequestId))
                    {
                        RecordProcessedRequestLocked(item.RequestId, item.Username, item.UpdatedTicks);
                    }
                    if ((item.Kind == WriteKind.UpsertWatchProgress && item.Blob is not null) ||
                        (item.Kind == WriteKind.UpsertVideo && effectiveSecondaryBlob is not null))
                    {
                        progressCount++;
                    }
                }
                if (!TryExecNonQueryLocked("COMMIT;", out var commitErr))
                {
                    throw new InvalidOperationException(commitErr ?? "COMMIT failed.");
                }
                committedWatchProgressCount = progressCount;
                duplicateCount = dupCount;
                return true;
            }
            catch (Exception ex)
            {
                _lastWriteError = ex.Message;
                DiagnosticLog.Write("SQLite", $"Batch rollback on '{DbPath}': {ex.Message}");
                TryExecNonQueryLocked("ROLLBACK;", out _);
                return false;
            }
        }
    }

    public bool IsRequestProcessed(string? requestId)
    {
        if (string.IsNullOrWhiteSpace(requestId)) return false;
        TryDrainWhenHealthyNonBlocking();
        lock (_dbLock)
        {
            if (!EnsureConnectionLocked()) return false;
            return IsRequestProcessedLocked(requestId.Trim());
        }
    }

    private bool IsRequestProcessedLocked(string requestId)
    {
        if (string.IsNullOrWhiteSpace(requestId)) return false;
        const string sql = "SELECT 1 FROM processed_requests WHERE request_id = ?1 LIMIT 1;";
        if (!TryPrepareLocked(sql, out var stmt))
        {
            throw new InvalidOperationException("Failed to prepare IsRequestProcessed statement.");
        }
        try
        {
            BindText(stmt, 1, requestId);
            return StepWithRetry(stmt) == SqliteRow;
        }
        finally
        {
            sqlite3_finalize(stmt);
        }
    }

    private void RecordProcessedRequestLocked(string requestId, string username, long ticks)
    {
        if (string.IsNullOrWhiteSpace(requestId)) return;
        const string sql = """
            INSERT OR IGNORE INTO processed_requests (request_id, username, processed_at_ticks)
            VALUES (?1, ?2, ?3);
            """;
        if (!TryPrepareLocked(sql, out var stmt))
        {
            throw new InvalidOperationException("Failed to prepare RecordProcessedRequest statement.");
        }
        try
        {
            BindText(stmt, 1, requestId);
            BindText(stmt, 2, username ?? "");
            sqlite3_bind_int64(stmt, 3, ticks > 0 ? ticks : DateTimeOffset.UtcNow.UtcTicks);
            StepDoneOrThrow(stmt, "RecordProcessedRequest");
        }
        finally
        {
            sqlite3_finalize(stmt);
        }
    }

    private void UpsertVideoRowLocked(
        string username,
        string key,
        byte[] encryptedBlob,
        long updatedAtTicks,
        int explicitViewCount,
        int viewCountDelta)
    {
        if (viewCountDelta > 0)
        {
            var initialViews = explicitViewCount >= viewCountDelta ? explicitViewCount : viewCountDelta;
            const string sql = """
                INSERT INTO videos (username, video_key, encrypted_blob, updated_at_ticks, view_count)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(username, video_key) DO UPDATE SET
                    encrypted_blob = excluded.encrypted_blob,
                    updated_at_ticks = excluded.updated_at_ticks,
                    view_count = CASE
                        WHEN videos.view_count >= 0 THEN videos.view_count + ?6
                        ELSE ?5
                    END;
                """;
            if (!TryPrepareLocked(sql, out var stmt))
            {
                throw new InvalidOperationException("Failed to prepare UpsertVideoRowDelta statement.");
            }
            try
            {
                BindText(stmt, 1, username);
                BindText(stmt, 2, key);
                BindBlob(stmt, 3, encryptedBlob);
                sqlite3_bind_int64(stmt, 4, updatedAtTicks);
                sqlite3_bind_int64(stmt, 5, initialViews);
                sqlite3_bind_int64(stmt, 6, viewCountDelta);
                StepDoneOrThrow(stmt, "UpsertVideoRowDelta");
            }
            finally
            {
                sqlite3_finalize(stmt);
            }
        }
        else
        {
            const string sql = """
                INSERT INTO videos (username, video_key, encrypted_blob, updated_at_ticks, view_count)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(username, video_key) DO UPDATE SET
                    encrypted_blob = excluded.encrypted_blob,
                    updated_at_ticks = excluded.updated_at_ticks,
                    view_count = CASE
                        WHEN ?5 >= 0 AND videos.view_count < 0 THEN ?5
                        ELSE videos.view_count
                    END;
                """;
            if (!TryPrepareLocked(sql, out var stmt))
            {
                throw new InvalidOperationException("Failed to prepare UpsertVideoRow statement.");
            }
            try
            {
                BindText(stmt, 1, username);
                BindText(stmt, 2, key);
                BindBlob(stmt, 3, encryptedBlob);
                sqlite3_bind_int64(stmt, 4, updatedAtTicks);
                sqlite3_bind_int64(stmt, 5, explicitViewCount);
                StepDoneOrThrow(stmt, "UpsertVideoRow");
            }
            finally
            {
                sqlite3_finalize(stmt);
            }
        }
    }

    private void ExecuteSingleWriteLocked(WriteItem item, int effectiveViewCountDelta, byte[]? effectiveSecondaryBlob)
    {
        switch (item.Kind)
        {
            case WriteKind.UpsertProfile:
                if (!TryPrepareLocked(
                        """
                        INSERT INTO profiles (username, display_username, salt_b64, password_hash_b64, iterations, created_at_utc)
                        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                        ON CONFLICT(username) DO UPDATE SET
                            display_username = excluded.display_username,
                            salt_b64 = excluded.salt_b64,
                            password_hash_b64 = excluded.password_hash_b64,
                            iterations = excluded.iterations,
                            created_at_utc = excluded.created_at_utc;
                        """,
                        out var profStmt))
                {
                    throw new InvalidOperationException("Failed to prepare UpsertProfile statement.");
                }
                try
                {
                    BindText(profStmt, 1, item.Username);
                    BindText(profStmt, 2, item.Username);
                    BindText(profStmt, 3, item.SaltBase64);
                    BindText(profStmt, 4, item.PasswordHashBase64);
                    sqlite3_bind_int64(profStmt, 5, item.Iterations);
                    BindText(profStmt, 6, item.CreatedAtUtc.ToString("O"));
                    StepDoneOrThrow(profStmt, "UpsertProfile");
                }
                finally
                {
                    sqlite3_finalize(profStmt);
                }
                break;

            case WriteKind.DeleteProfileCascade:
                DeleteByUsernameLocked("DELETE FROM processed_requests WHERE username = ?1;", item.Username);
                DeleteByUsernameLocked("DELETE FROM thumbnails WHERE username = ?1;", item.Username);
                DeleteByUsernameLocked("DELETE FROM custom_list_items WHERE username = ?1;", item.Username);
                DeleteByUsernameLocked("DELETE FROM custom_lists WHERE username = ?1;", item.Username);
                DeleteByUsernameLocked("DELETE FROM watch_progress WHERE username = ?1;", item.Username);
                DeleteByUsernameLocked("DELETE FROM videos WHERE username = ?1;", item.Username);
                DeleteByUsernameLocked("DELETE FROM profiles WHERE username = ?1;", item.Username);
                break;

            case WriteKind.UpsertVideo:
                if (item.Blob is not null)
                {
                    UpsertVideoRowLocked(
                        item.Username,
                        item.Key,
                        item.Blob,
                        item.UpdatedTicks,
                        item.ViewCount,
                        effectiveViewCountDelta);
                }
                if (effectiveSecondaryBlob is not null)
                {
                    UpsertKeyedBlobLocked(
                        """
                        INSERT INTO watch_progress (username, video_key, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4)
                        ON CONFLICT(username, video_key) DO UPDATE SET
                            encrypted_blob = excluded.encrypted_blob,
                            updated_at_ticks = excluded.updated_at_ticks;
                        """,
                        item.Username,
                        item.Key,
                        effectiveSecondaryBlob,
                        item.UpdatedTicks);
                }
                break;

            case WriteKind.UpsertWatchProgress:
                if (effectiveSecondaryBlob is not null)
                {
                    UpsertKeyedBlobLocked(
                        """
                        INSERT OR IGNORE INTO videos (username, video_key, encrypted_blob, updated_at_ticks, view_count)
                        VALUES (?1, ?2, ?3, ?4, 0);
                        """,
                        item.Username,
                        item.Key,
                        effectiveSecondaryBlob,
                        item.UpdatedTicks);
                }
                if (item.Blob is not null)
                {
                    UpsertKeyedBlobLocked(
                        """
                        INSERT INTO watch_progress (username, video_key, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4)
                        ON CONFLICT(username, video_key) DO UPDATE SET
                            encrypted_blob = excluded.encrypted_blob,
                            updated_at_ticks = excluded.updated_at_ticks;
                        """,
                        item.Username,
                        item.Key,
                        item.Blob,
                        item.UpdatedTicks);
                }
                break;

            case WriteKind.DeleteVideo:
                DeleteKeyedRowLocked("DELETE FROM watch_progress WHERE username = ?1 AND video_key = ?2;", item.Username, item.Key);
                DeleteKeyedRowLocked("DELETE FROM videos WHERE username = ?1 AND video_key = ?2;", item.Username, item.Key);
                break;

            case WriteKind.ReplaceAllVideosAndProgress:
                DeleteByUsernameLocked("DELETE FROM watch_progress WHERE username = ?1;", item.Username);
                DeleteByUsernameLocked("DELETE FROM videos WHERE username = ?1;", item.Username);
                if (item.BatchVideos is { Count: > 0 } vRows)
                {
                    UpsertBatchLocked(
                        """
                        INSERT OR REPLACE INTO videos (username, video_key, encrypted_blob, updated_at_ticks, view_count)
                        VALUES (?1, ?2, ?3, ?4, 0);
                        """,
                        item.Username,
                        vRows);
                }
                if (item.BatchProgress is { Count: > 0 } pRows)
                {
                    UpsertBatchLocked(
                        """
                        INSERT OR REPLACE INTO watch_progress (username, video_key, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4);
                        """,
                        item.Username,
                        pRows);
                }
                break;

            case WriteKind.UpsertCustomList:
                if (item.Blob is not null)
                {
                    UpsertKeyedBlobLocked(
                        """
                        INSERT INTO custom_lists (username, list_id, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4)
                        ON CONFLICT(username, list_id) DO UPDATE SET
                            encrypted_blob = excluded.encrypted_blob,
                            updated_at_ticks = excluded.updated_at_ticks;
                        """,
                        item.Username,
                        item.Key,
                        item.Blob,
                        item.UpdatedTicks);
                }
                break;

            case WriteKind.DeleteCustomList:
                DeleteKeyedRowLocked("DELETE FROM custom_list_items WHERE username = ?1 AND list_id = ?2;", item.Username, item.Key);
                DeleteKeyedRowLocked("DELETE FROM custom_lists WHERE username = ?1 AND list_id = ?2;", item.Username, item.Key);
                break;

            case WriteKind.UpsertCustomListItem:
                if (effectiveSecondaryBlob is not null)
                {
                    UpsertKeyedBlobLocked(
                        """
                        INSERT INTO custom_lists (username, list_id, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4)
                        ON CONFLICT(username, list_id) DO UPDATE SET
                            encrypted_blob = excluded.encrypted_blob,
                            updated_at_ticks = excluded.updated_at_ticks;
                        """,
                        item.Username,
                        item.Key,
                        effectiveSecondaryBlob,
                        item.UpdatedTicks);
                }
                if (item.Blob is not null)
                {
                    UpsertCustomListItemLocked(item.Username, item.Key, item.SecondaryKey, item.Ord, item.Blob);
                }
                break;

            case WriteKind.DeleteCustomListItem:
                if (effectiveSecondaryBlob is not null)
                {
                    UpsertKeyedBlobLocked(
                        """
                        INSERT INTO custom_lists (username, list_id, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4)
                        ON CONFLICT(username, list_id) DO UPDATE SET
                            encrypted_blob = excluded.encrypted_blob,
                            updated_at_ticks = excluded.updated_at_ticks;
                        """,
                        item.Username,
                        item.Key,
                        effectiveSecondaryBlob,
                        item.UpdatedTicks);
                }
                if (!TryPrepareLocked(
                        "DELETE FROM custom_list_items WHERE username = ?1 AND list_id = ?2 AND item_hash = ?3;",
                        out var delItemStmt))
                {
                    throw new InvalidOperationException("Failed to prepare DeleteCustomListItem statement.");
                }
                try
                {
                    BindText(delItemStmt, 1, item.Username);
                    BindText(delItemStmt, 2, item.Key);
                    BindText(delItemStmt, 3, item.SecondaryKey);
                    StepDoneOrThrow(delItemStmt, "DeleteCustomListItem");
                }
                finally
                {
                    sqlite3_finalize(delItemStmt);
                }
                break;

            case WriteKind.ReplaceCustomListItems:
                if (effectiveSecondaryBlob is not null)
                {
                    UpsertKeyedBlobLocked(
                        """
                        INSERT INTO custom_lists (username, list_id, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4)
                        ON CONFLICT(username, list_id) DO UPDATE SET
                            encrypted_blob = excluded.encrypted_blob,
                            updated_at_ticks = excluded.updated_at_ticks;
                        """,
                        item.Username,
                        item.Key,
                        effectiveSecondaryBlob,
                        item.UpdatedTicks);
                }
                DeleteKeyedRowLocked("DELETE FROM custom_list_items WHERE username = ?1 AND list_id = ?2;", item.Username, item.Key);
                if (item.BatchListItems is { Count: > 0 } listItems)
                {
                    foreach (var li in listItems)
                    {
                        UpsertCustomListItemLocked(item.Username, li.ListId, li.ItemHash, li.Ord, li.EncryptedPathBlob);
                    }
                }
                break;

            case WriteKind.UpsertThumbnail:
                if (effectiveSecondaryBlob is not null)
                {
                    UpsertKeyedBlobLocked(
                        """
                        INSERT OR IGNORE INTO videos (username, video_key, encrypted_blob, updated_at_ticks, view_count)
                        VALUES (?1, ?2, ?3, ?4, 0);
                        """,
                        item.Username,
                        item.Key,
                        effectiveSecondaryBlob,
                        item.UpdatedTicks);
                }
                if (item.Blob is not null)
                {
                    UpsertKeyedBlobLocked(
                        """
                        INSERT INTO thumbnails (username, video_key, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4)
                        ON CONFLICT(username, video_key) DO UPDATE SET
                            encrypted_blob = excluded.encrypted_blob,
                            updated_at_ticks = excluded.updated_at_ticks;
                        """,
                        item.Username,
                        item.Key,
                        item.Blob,
                        item.UpdatedTicks);
                }
                break;

            case WriteKind.MigrateVideoKey:
                if (!string.Equals(item.Key, item.SecondaryKey, StringComparison.OrdinalIgnoreCase))
                {
                    DeleteKeyedRowLocked("DELETE FROM watch_progress WHERE username = ?1 AND video_key = ?2;", item.Username, item.Key);
                    DeleteKeyedRowLocked("DELETE FROM videos WHERE username = ?1 AND video_key = ?2;", item.Username, item.Key);
                    if (!TryPrepareLocked(
                            """
                            UPDATE OR REPLACE thumbnails
                            SET video_key = ?3
                            WHERE username = ?1 AND video_key = ?2;
                            """,
                            out var moveThumbStmt))
                    {
                        throw new InvalidOperationException("Failed to prepare MigrateVideoKey thumbnail statement.");
                    }
                    try
                    {
                        BindText(moveThumbStmt, 1, item.Username);
                        BindText(moveThumbStmt, 2, item.Key);
                        BindText(moveThumbStmt, 3, item.SecondaryKey);
                        StepDoneOrThrow(moveThumbStmt, "MigrateVideoKey");
                    }
                    finally
                    {
                        sqlite3_finalize(moveThumbStmt);
                    }
                }
                if (item.Blob is not null)
                {
                    UpsertVideoRowLocked(
                        item.Username,
                        item.SecondaryKey,
                        item.Blob,
                        item.UpdatedTicks,
                        item.ViewCount,
                        viewCountDelta: 0);
                }
                if (effectiveSecondaryBlob is not null)
                {
                    UpsertKeyedBlobLocked(
                        """
                        INSERT OR REPLACE INTO watch_progress (username, video_key, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4);
                        """,
                        item.Username,
                        item.SecondaryKey,
                        effectiveSecondaryBlob,
                        item.UpdatedTicks);
                }
                break;
        }
    }

    private void UpsertCustomListItemLocked(string username, string listId, string itemHash, int ord, byte[] encryptedPathBlob)
    {
        const string sql = """
            INSERT INTO custom_list_items (username, list_id, item_hash, ord, encrypted_path_blob)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(username, list_id, item_hash) DO UPDATE SET
                ord = excluded.ord,
                encrypted_path_blob = excluded.encrypted_path_blob;
            """;
        if (!TryPrepareLocked(sql, out var stmt))
        {
            throw new InvalidOperationException("Failed to prepare UpsertCustomListItem statement.");
        }
        try
        {
            BindText(stmt, 1, username);
            BindText(stmt, 2, listId);
            BindText(stmt, 3, itemHash);
            sqlite3_bind_int64(stmt, 4, ord);
            BindBlob(stmt, 5, encryptedPathBlob);
            StepDoneOrThrow(stmt, "UpsertCustomListItem");
        }
        finally
        {
            sqlite3_finalize(stmt);
        }
    }

    public bool ImportLegacyVaultBatchAndVerify(
        string username,
        IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)> videos,
        IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)> progress,
        IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)> lists,
        IReadOnlyList<ListItemRow> listItems,
        IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)> thumbnails)
    {
        FlushForPath(DbPath);
        lock (_dbLock)
        {
            if (!EnsureConnectionLocked())
            {
                _lastWriteError = "Database connection is not open.";
                return false;
            }
            if (!TryExecNonQueryLocked("BEGIN IMMEDIATE;", out var beginErr))
            {
                _lastWriteError = beginErr ?? "BEGIN IMMEDIATE failed during legacy import.";
                DiagnosticLog.Write("SQLite", $"ImportLegacyVaultBatchAndVerify BEGIN IMMEDIATE failed: {_lastWriteError}");
                return false;
            }
            try
            {
                if (videos.Count > 0)
                {
                    UpsertBatchLocked(
                        """
                        INSERT OR REPLACE INTO videos (username, video_key, encrypted_blob, updated_at_ticks, view_count)
                        VALUES (?1, ?2, ?3, ?4, -1);
                        """,
                        username,
                        videos);
                }
                if (progress.Count > 0)
                {
                    UpsertBatchLocked(
                        """
                        INSERT OR REPLACE INTO watch_progress (username, video_key, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4);
                        """,
                        username,
                        progress);
                }
                if (lists.Count > 0)
                {
                    UpsertBatchLocked(
                        """
                        INSERT OR REPLACE INTO custom_lists (username, list_id, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4);
                        """,
                        username,
                        lists);
                }
                if (listItems.Count > 0)
                {
                    foreach (var li in listItems)
                    {
                        UpsertCustomListItemLocked(username, li.ListId, li.ItemHash, li.Ord, li.EncryptedPathBlob);
                    }
                }
                if (thumbnails.Count > 0)
                {
                    UpsertBatchLocked(
                        """
                        INSERT OR REPLACE INTO thumbnails (username, video_key, encrypted_blob, updated_at_ticks)
                        VALUES (?1, ?2, ?3, ?4);
                        """,
                        username,
                        thumbnails);
                }
                if (!TryExecNonQueryLocked("COMMIT;", out var commitErr))
                {
                    throw new InvalidOperationException(commitErr ?? "COMMIT failed during legacy import.");
                }

                var dbVideoCount = CountRowsByUserLocked("SELECT COUNT(*) FROM videos WHERE username = ?1;", username);
                var dbProgressCount = CountRowsByUserLocked("SELECT COUNT(*) FROM watch_progress WHERE username = ?1;", username);
                var dbListCount = CountRowsByUserLocked("SELECT COUNT(*) FROM custom_lists WHERE username = ?1;", username);
                var dbListItemCount = CountRowsByUserLocked("SELECT COUNT(*) FROM custom_list_items WHERE username = ?1;", username);
                var dbThumbCount = CountRowsByUserLocked("SELECT COUNT(*) FROM thumbnails WHERE username = ?1;", username);

                return dbVideoCount >= videos.Count &&
                       dbProgressCount >= progress.Count &&
                       dbListCount >= lists.Count &&
                       dbListItemCount >= listItems.Count &&
                       dbThumbCount >= thumbnails.Count;
            }
            catch (Exception ex)
            {
                _lastWriteError = ex.Message;
                DiagnosticLog.Write("SQLite", $"ImportLegacyVaultBatchAndVerify rollback: {ex.Message}");
                TryExecNonQueryLocked("ROLLBACK;", out _);
                return false;
            }
        }
    }

    public (long Videos, long WatchProgress, long CustomLists, long CustomListItems, long Thumbnails) GetTableRowCounts(string username)
    {
        FlushForPath(DbPath);
        lock (_dbLock)
        {
            if (!EnsureConnectionLocked()) return (0, 0, 0, 0, 0);
            return (
                CountRowsByUserLocked("SELECT COUNT(*) FROM videos WHERE username = ?1;", username),
                CountRowsByUserLocked("SELECT COUNT(*) FROM watch_progress WHERE username = ?1;", username),
                CountRowsByUserLocked("SELECT COUNT(*) FROM custom_lists WHERE username = ?1;", username),
                CountRowsByUserLocked("SELECT COUNT(*) FROM custom_list_items WHERE username = ?1;", username),
                CountRowsByUserLocked("SELECT COUNT(*) FROM thumbnails WHERE username = ?1;", username)
            );
        }
    }

    private long CountRowsByUserLocked(string sql, string username)
    {
        if (!TryPrepareLocked(sql, out var stmt)) return 0;
        try
        {
            BindText(stmt, 1, username);
            if (StepWithRetry(stmt) == SqliteRow)
            {
                return sqlite3_column_int64(stmt, 0);
            }
            return 0;
        }
        finally
        {
            sqlite3_finalize(stmt);
        }
    }

    private void EnsureInitialized()
    {
        lock (_dbLock)
        {
            if (_db != IntPtr.Zero || _disposed) return;
            var dir = Path.GetDirectoryName(DbPath);
            if (!string.IsNullOrEmpty(dir))
            {
                Directory.CreateDirectory(dir);
            }

            var pathBytes = ToUtf8NullTerminated(DbPath);
            var rc = sqlite3_open_v2(
                pathBytes,
                out _db,
                SqliteOpenReadWrite | SqliteOpenCreate | SqliteOpenFullMutex,
                IntPtr.Zero);
            if (rc != SqliteOk || _db == IntPtr.Zero)
            {
                if (_db != IntPtr.Zero)
                {
                    sqlite3_close_v2(_db);
                    _db = IntPtr.Zero;
                }
                return;
            }

            sqlite3_busy_timeout(_db, 150);
            TryExecNonQueryLocked(
                """
                PRAGMA journal_mode=WAL;
                PRAGMA synchronous=NORMAL;
                PRAGMA temp_store=MEMORY;
                CREATE TABLE IF NOT EXISTS meta (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT
                );
                CREATE TABLE IF NOT EXISTS profiles (
                    username TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
                    display_username TEXT NOT NULL,
                    salt_b64 TEXT NOT NULL,
                    password_hash_b64 TEXT NOT NULL,
                    iterations INTEGER NOT NULL,
                    created_at_utc TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS videos (
                    username TEXT NOT NULL COLLATE NOCASE,
                    video_key TEXT NOT NULL COLLATE NOCASE,
                    encrypted_blob BLOB NOT NULL,
                    updated_at_ticks INTEGER NOT NULL,
                    view_count INTEGER NOT NULL DEFAULT -1,
                    PRIMARY KEY (username, video_key)
                );
                CREATE TABLE IF NOT EXISTS watch_progress (
                    username TEXT NOT NULL COLLATE NOCASE,
                    video_key TEXT NOT NULL COLLATE NOCASE,
                    encrypted_blob BLOB NOT NULL,
                    updated_at_ticks INTEGER NOT NULL,
                    PRIMARY KEY (username, video_key)
                );
                CREATE TABLE IF NOT EXISTS custom_lists (
                    username TEXT NOT NULL COLLATE NOCASE,
                    list_id TEXT NOT NULL COLLATE NOCASE,
                    encrypted_blob BLOB NOT NULL,
                    updated_at_ticks INTEGER NOT NULL,
                    PRIMARY KEY (username, list_id)
                );
                CREATE TABLE IF NOT EXISTS custom_list_items (
                    username TEXT NOT NULL COLLATE NOCASE,
                    list_id TEXT NOT NULL COLLATE NOCASE,
                    item_hash TEXT NOT NULL COLLATE NOCASE,
                    ord INTEGER NOT NULL,
                    encrypted_path_blob BLOB NOT NULL,
                    PRIMARY KEY (username, list_id, item_hash)
                );
                CREATE INDEX IF NOT EXISTS idx_custom_list_items_order
                    ON custom_list_items (username, list_id, ord);
                CREATE TABLE IF NOT EXISTS thumbnails (
                    username TEXT NOT NULL COLLATE NOCASE,
                    video_key TEXT NOT NULL COLLATE NOCASE,
                    encrypted_blob BLOB NOT NULL,
                    updated_at_ticks INTEGER NOT NULL,
                    PRIMARY KEY (username, video_key)
                );
                CREATE TABLE IF NOT EXISTS processed_requests (
                    request_id TEXT PRIMARY KEY NOT NULL,
                    username TEXT NOT NULL DEFAULT '' COLLATE NOCASE,
                    processed_at_ticks INTEGER NOT NULL
                );
                """,
                out _);
            TryExecNonQueryLocked("ALTER TABLE videos ADD COLUMN view_count INTEGER NOT NULL DEFAULT -1;", out _);
        }
    }

    public List<string> ExplainQueryPlan(string sql)
    {
        var details = new List<string>();
        lock (_dbLock)
        {
            if (!EnsureConnectionLocked()) return details;
            if (!TryPrepareLocked("EXPLAIN QUERY PLAN " + sql, out var stmt)) return details;
            try
            {
                while (StepWithRetry(stmt) == SqliteRow)
                {
                    var detail = ReadTextColumn(stmt, 3);
                    details.Add(detail);
                }
            }
            finally
            {
                sqlite3_finalize(stmt);
            }
        }
        return details;
    }

    public List<(string Key, byte[] EncryptedBlob)> LoadVideos(string username)
    {
        TryDrainWhenHealthyNonBlocking();
        return LoadKeyedBlobs("SELECT video_key, encrypted_blob FROM videos WHERE username = ?1;", username);
    }

    public List<(string Key, byte[] EncryptedBlob, long ViewCount)> LoadVideosWithViewCounts(string username)
    {
        TryDrainWhenHealthyNonBlocking();
        var results = new List<(string Key, byte[] EncryptedBlob, long ViewCount)>();
        lock (_dbLock)
        {
            if (!EnsureConnectionLocked()) return results;
            const string sql = "SELECT video_key, encrypted_blob, view_count FROM videos WHERE username = ?1;";
            if (!TryPrepareLocked(sql, out var stmt)) return results;
            try
            {
                BindText(stmt, 1, username);
                while (StepWithRetry(stmt) == SqliteRow)
                {
                    var key = ReadTextColumn(stmt, 0);
                    var blob = ReadBlobColumn(stmt, 1);
                    var viewCount = sqlite3_column_int64(stmt, 2);
                    if (!string.IsNullOrEmpty(key) && blob is { Length: > 0 })
                    {
                        results.Add((key, blob, viewCount));
                    }
                }
            }
            finally
            {
                sqlite3_finalize(stmt);
            }
        }
        return results;
    }

    public List<(string Key, byte[] EncryptedBlob)> LoadWatchProgress(string username)
    {
        TryDrainWhenHealthyNonBlocking();
        return LoadKeyedBlobs("SELECT video_key, encrypted_blob FROM watch_progress WHERE username = ?1;", username);
    }

    public List<(string Key, byte[] EncryptedBlob)> LoadCustomLists(string username)
    {
        TryDrainWhenHealthyNonBlocking();
        return LoadKeyedBlobs("SELECT list_id, encrypted_blob FROM custom_lists WHERE username = ?1;", username);
    }

    public List<(string ListId, byte[] EncryptedPathBlob)> LoadCustomListItems(string username)
    {
        TryDrainWhenHealthyNonBlocking();
        return LoadKeyedBlobs(
            "SELECT list_id, encrypted_path_blob FROM custom_list_items WHERE username = ?1 ORDER BY list_id, ord ASC;",
            username);
    }

    public byte[]? TryGetThumbnailBlob(string username, string videoKey)
    {
        if (_recentThumbnailBlobs.TryGetValue((username.ToUpperInvariant(), videoKey.ToUpperInvariant()), out var cachedBlob))
        {
            return cachedBlob;
        }
        lock (_dbLock)
        {
            if (!EnsureConnectionLocked()) return null;
            const string sql = "SELECT encrypted_blob FROM thumbnails WHERE username = ?1 AND video_key = ?2 LIMIT 1;";
            if (!TryPrepareLocked(sql, out var stmt)) return null;
            try
            {
                BindText(stmt, 1, username);
                BindText(stmt, 2, videoKey);
                if (StepWithRetry(stmt) == SqliteRow)
                {
                    return ReadBlobColumn(stmt, 0);
                }
                return null;
            }
            finally
            {
                sqlite3_finalize(stmt);
            }
        }
    }

    private void UpsertKeyedBlobLocked(string sql, string username, string key, byte[] encryptedBlob, long updatedAtTicks)
    {
        if (!TryPrepareLocked(sql, out var stmt))
        {
            throw new InvalidOperationException("Failed to prepare UpsertKeyedBlob statement.");
        }
        try
        {
            BindText(stmt, 1, username);
            BindText(stmt, 2, key);
            BindBlob(stmt, 3, encryptedBlob);
            sqlite3_bind_int64(stmt, 4, updatedAtTicks);
            StepDoneOrThrow(stmt, "UpsertKeyedBlob");
        }
        finally
        {
            sqlite3_finalize(stmt);
        }
    }

    private void UpsertBatchLocked(
        string sql,
        string username,
        IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)> rows)
    {
        foreach (var (key, blob, ticks) in rows)
        {
            UpsertKeyedBlobLocked(sql, username, key, blob, ticks);
        }
    }

    private void DeleteKeyedRowLocked(string sql, string username, string key)
    {
        if (!TryPrepareLocked(sql, out var stmt))
        {
            throw new InvalidOperationException("Failed to prepare DeleteKeyedRow statement.");
        }
        try
        {
            BindText(stmt, 1, username);
            BindText(stmt, 2, key);
            StepDoneOrThrow(stmt, "DeleteKeyedRow");
        }
        finally
        {
            sqlite3_finalize(stmt);
        }
    }

    private void DeleteByUsernameLocked(string sql, string username)
    {
        if (!TryPrepareLocked(sql, out var stmt))
        {
            throw new InvalidOperationException("Failed to prepare DeleteByUsername statement.");
        }
        try
        {
            BindText(stmt, 1, username);
            StepDoneOrThrow(stmt, "DeleteByUsername");
        }
        finally
        {
            sqlite3_finalize(stmt);
        }
    }

    private List<(string Key, byte[] EncryptedBlob)> LoadKeyedBlobs(string sql, string username)
    {
        var results = new List<(string Key, byte[] EncryptedBlob)>();
        lock (_dbLock)
        {
            if (!EnsureConnectionLocked()) return results;
            if (!TryPrepareLocked(sql, out var stmt)) return results;
            try
            {
                BindText(stmt, 1, username);
                while (StepWithRetry(stmt) == SqliteRow)
                {
                    var key = ReadTextColumn(stmt, 0);
                    var blob = ReadBlobColumn(stmt, 1);
                    if (!string.IsNullOrEmpty(key) && blob is { Length: > 0 })
                    {
                        results.Add((key, blob));
                    }
                }
            }
            finally
            {
                sqlite3_finalize(stmt);
            }
        }
        return results;
    }

    private bool EnsureConnectionLocked()
    {
        if (_disposed) return false;
        if (_db == IntPtr.Zero) EnsureInitialized();
        return _db != IntPtr.Zero;
    }

    private bool TryExecNonQueryLocked(string sql, out string? error)
    {
        error = null;
        if (_db == IntPtr.Zero)
        {
            error = "Database handle is zero.";
            return false;
        }
        var sqlBytes = ToUtf8NullTerminated(sql);
        for (var attempt = 0; attempt < 3; attempt++)
        {
            var rc = sqlite3_exec(_db, sqlBytes, IntPtr.Zero, IntPtr.Zero, out var errPtr);
            string? msg = null;
            if (errPtr != IntPtr.Zero)
            {
                msg = Marshal.PtrToStringUTF8(errPtr);
                sqlite3_free(errPtr);
            }
            if (rc == SqliteOk)
            {
                return true;
            }
            error = string.IsNullOrWhiteSpace(msg) ? $"sqlite3_exec failed with rc={rc}" : $"{msg} (rc={rc})";
            if (rc is not (SqliteBusy or SqliteLocked))
            {
                break;
            }
            Thread.Sleep(10);
        }
        return false;
    }

    private static void StepDoneOrThrow(IntPtr stmt, string operationName)
    {
        var rc = StepWithRetry(stmt);
        if (rc != SqliteDone)
        {
            throw new InvalidOperationException($"{operationName} sqlite3_step failed with rc={rc}");
        }
    }

    private bool TryPrepareLocked(string sql, out IntPtr stmt)
    {
        stmt = IntPtr.Zero;
        if (_db == IntPtr.Zero) return false;
        var sqlBytes = ToUtf8NullTerminated(sql);
        var rc = sqlite3_prepare_v2(_db, sqlBytes, sqlBytes.Length - 1, out stmt, out _);
        if (rc != SqliteOk || stmt == IntPtr.Zero)
        {
            if (stmt != IntPtr.Zero) sqlite3_finalize(stmt);
            stmt = IntPtr.Zero;
            return false;
        }
        return true;
    }

    private static int StepWithRetry(IntPtr stmt)
    {
        for (var attempt = 0; attempt < 10; attempt++)
        {
            var rc = sqlite3_step(stmt);
            if (rc is not (SqliteBusy or SqliteLocked))
            {
                return rc;
            }
            Thread.Sleep(5);
        }
        return sqlite3_step(stmt);
    }

    private static void BindText(IntPtr stmt, int index, string? value)
    {
        if (value is null)
        {
            sqlite3_bind_null(stmt, index);
            return;
        }
        var bytes = Encoding.UTF8.GetBytes(value);
        sqlite3_bind_text(stmt, index, bytes, bytes.Length, SqliteTransient);
    }

    private static void BindBlob(IntPtr stmt, int index, byte[] value)
    {
        sqlite3_bind_blob(stmt, index, value, value.Length, SqliteTransient);
    }

    private static string ReadTextColumn(IntPtr stmt, int col)
    {
        var ptr = sqlite3_column_text(stmt, col);
        var len = sqlite3_column_bytes(stmt, col);
        if (ptr == IntPtr.Zero || len <= 0) return "";
        return Marshal.PtrToStringUTF8(ptr, len) ?? "";
    }

    private static byte[]? ReadBlobColumn(IntPtr stmt, int col)
    {
        var len = sqlite3_column_bytes(stmt, col);
        if (len <= 0) return null;
        var ptr = sqlite3_column_blob(stmt, col);
        if (ptr == IntPtr.Zero) return null;
        var buffer = new byte[len];
        Marshal.Copy(ptr, buffer, 0, len);
        return buffer;
    }

    private static byte[] ToUtf8NullTerminated(string text)
    {
        var count = Encoding.UTF8.GetByteCount(text);
        var bytes = new byte[count + 1];
        Encoding.UTF8.GetBytes(text, 0, text.Length, bytes, 0);
        bytes[count] = 0;
        return bytes;
    }

    public void Dispose()
    {
        if (_disposed) return;
        Flush(TimeSpan.FromSeconds(5));
        _disposed = true;
        _writeChannel.Writer.TryComplete();
        try
        {
            _writerLoopTask.Wait(TimeSpan.FromSeconds(2));
        }
        catch
        {
        }

        if (ActiveStoresByPath.TryGetValue(DbPath, out var set))
        {
            set.TryRemove(this, out _);
        }

        lock (_dbLock)
        {
            if (_db != IntPtr.Zero)
            {
                sqlite3_close_v2(_db);
                _db = IntPtr.Zero;
            }
        }
    }
}
