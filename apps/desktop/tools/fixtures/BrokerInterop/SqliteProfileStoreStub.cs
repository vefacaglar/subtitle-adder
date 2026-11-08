// In-memory stand-in for the C# app's SqliteProfileStore with exactly the surface StateSyncBroker.cs uses.
// Commits synchronously; keeps the processed_requests idempotency contract (a repeated request id is acked
// committed + isDuplicate and its post-commit events are dropped).
namespace SubtitleAdder.Core;

public sealed class SqliteProfileStore : IDisposable
{
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
        PurgeLegacyVideoKeys,
        RetagOwner,
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

    private readonly HashSet<string> _processed = new(StringComparer.Ordinal);
    private readonly Lock _lock = new();
    private bool _disposed;

    public SqliteProfileStore(string dbPath) => DbPath = dbPath;

    public string DbPath { get; }

    public Action<IReadOnlyList<StateSyncEvent>>? PostCommitBroadcastHandler { get; set; }

    /// <summary>Every executed item (tests print them).</summary>
    internal List<WriteItem> Executed { get; } = new();

    internal void EnqueueItem(WriteItem item)
    {
        bool duplicate;
        lock (_lock)
        {
            if (_disposed)
            {
                item.OnCompleted?.Invoke(false, false, "Store is disposed.");
                return;
            }
            duplicate = item.RequestId.Length > 0 && !_processed.Add(item.RequestId);
            if (!duplicate) Executed.Add(item);
        }
        Program.Log("CS-EXEC " + item.Kind + " " + item.RequestId + " dup=" + duplicate.ToString().ToLowerInvariant()
                    + " blob=" + (item.Blob is null ? "-" : Convert.ToBase64String(item.Blob))
                    + " videos=" + (item.BatchVideos?.Count ?? 0) + " items=" + (item.BatchListItems?.Count ?? 0)
                    + " created=" + item.CreatedAtUtc.ToString("O"));
        item.OnCompleted?.Invoke(true, duplicate, null);
        if (!duplicate && item.PostCommitEvents is { Count: > 0 } events)
        {
            PostCommitBroadcastHandler?.Invoke(events);
        }
    }

    public Task<bool> FlushAsync()
    {
        try { BeforeFlushPathHook?.Invoke(DbPath); } catch { }
        return Task.FromResult(!_disposed);
    }

    public bool Flush(TimeSpan? timeout = null)
    {
        try { BeforeFlushPathHook?.Invoke(DbPath); } catch { }
        return !_disposed;
    }

    public void Dispose()
    {
        lock (_lock) _disposed = true;
    }
}
