using System.Collections.Concurrent;
using System.IO.Pipes;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace SubtitleAdder.Core;

/// <summary>
/// Standard pub/sub topic names routed by <see cref="StateSyncBroker"/> across all open application instances.
/// </summary>
public static class StateSyncTopics
{
    public const string All = "*";
    public const string Profiles = "profiles";
    public const string WatchProgress = "watch-progress";
    public const string Thumbnail = "thumbnail";
    public const string MediaIndex = "media-index";
    public const string Providers = "providers";
    public const string Settings = "settings";
}

/// <summary>
/// Types of state synchronization events broadcast by <see cref="StateSyncBroker"/> across running application instances.
/// </summary>
public enum StateSyncEventKind
{
    ProfileLogin,
    ProfileLogout,
    ProfileDeleted,
    VideoWatched,
    ProgressUpdated,
    VideoLiked,
    VideoRated,
    HistoryCleared,
    ListChanged,
    ThumbnailCaptured,
    MediaIndexed,
    ProvidersUpdated,
    SettingsUpdated,
}

/// <summary>
/// Lightweight pub/sub event payload broadcast across running <c>SubtitleAdder</c> instances via <see cref="StateSyncBroker"/>.
/// </summary>
public sealed record StateSyncEvent
{
    public string EventId { get; init; } = Guid.NewGuid().ToString("N");
    public string Topic { get; init; } = StateSyncTopics.Profiles;
    public required StateSyncEventKind Kind { get; init; }
    public required string SenderInstanceId { get; init; }
    public required string ScopePath { get; init; }
    public string? Username { get; init; }
    public string? VideoKey { get; init; }
    public string? VideoPath { get; init; }
    public double? PositionSeconds { get; init; }
    public double? DurationSeconds { get; init; }
    public int? ViewCount { get; init; }
    public bool? IsLiked { get; init; }
    public int? Rating { get; init; }
    public string? ListId { get; init; }
    public string? PayloadBase64 { get; init; }
    public DateTimeOffset TimestampUtc { get; init; } = DateTimeOffset.UtcNow;
}

public sealed record StateSyncBatchBlobDto(
    string Key,
    byte[] EncryptedBlob,
    long UpdatedTicks);

public sealed record StateSyncListItemDto(
    string ListId,
    string ItemHash,
    int Ord,
    byte[] EncryptedPathBlob);

/// <summary>
/// Centralized IPC write request submitted by an application window (<see cref="LocalProfileManager"/>)
/// over Named Pipe to the shared <see cref="StateSyncBroker"/> Data Worker.
/// Carries a unique <see cref="RequestId"/> so reconnections and retries are 100% idempotent.
/// </summary>
public sealed record StateSyncWriteRequest
{
    public string RequestId { get; init; } = Guid.NewGuid().ToString("N");
    public required string SenderInstanceId { get; init; }
    public required string DbPath { get; init; }
    public required string Kind { get; init; }
    public string Username { get; init; } = "";
    public string Key { get; init; } = "";
    public string SecondaryKey { get; init; } = "";
    public int Ord { get; init; }
    public byte[]? Blob { get; init; }
    public byte[]? SecondaryBlob { get; init; }
    public long UpdatedTicks { get; init; }
    public int ViewCount { get; init; } = -1;
    public int ViewCountDelta { get; init; }
    public string? SaltBase64 { get; init; }
    public string? PasswordHashBase64 { get; init; }
    public int Iterations { get; init; }
    public DateTimeOffset CreatedAtUtc { get; init; }
    public List<StateSyncBatchBlobDto>? BatchVideos { get; init; }
    public List<StateSyncBatchBlobDto>? BatchProgress { get; init; }
    public List<StateSyncListItemDto>? BatchListItems { get; init; }
    public List<StateSyncEvent>? PostCommitEvents { get; init; }
}

/// <summary>
/// Acknowledgment sent by the centralized Data Worker back to the requesting window after SQLite transaction commit.
/// </summary>
public sealed record StateSyncWriteAck
{
    public required string RequestId { get; init; }
    public bool Committed { get; init; }
    public bool IsDuplicate { get; init; }
    public string? Error { get; init; }
}

/// <summary>
/// Shared background Pub/Sub communication broker and centralized SQLite Data Worker engine that coordinates all
/// state updates and database writes across 1..N running <c>SubtitleAdder</c> instances without <see cref="FileSystemWatcher"/> polling.
/// <para>
/// Lifecycle ("Last Window Closes the Engine"):
/// <list type="bullet">
/// <item><description>The first application instance that opens starts the shared background Pub/Sub &amp; Data Worker Engine (<c>SubtitleAdder.StateBroker.&lt;Hash&gt;</c>).</description></item>
/// <item><description>Subsequent instances (2nd, 3rd, 4th, 5th+) register their active instance slot and connect to the running Engine over Named Pipes.</description></item>
/// <item><description>If the 1st instance (or any other instance) closes while other instances are still open (<see cref="HasOtherActiveInstances"/> is <c>true</c>), the Engine and its SQLite Data Worker stay alive so remaining windows continue writing and communicating without interruption.</description></item>
/// <item><description>Only when the very last open instance closes (<see cref="ActiveInstanceCount"/> drops to <c>0</c>) does that last instance flush all pending SQLite writes, dispose the Data Worker stores, and shut down the Engine (<see cref="ClosedEngineOnDispose"/> = <c>true</c>).</description></item>
/// </list>
/// </para>
/// </summary>
public sealed class StateSyncBroker : IDisposable
{
    private static readonly ConcurrentDictionary<string, StateSyncEngine> EnginesByScope = new(StringComparer.Ordinal);
    private static readonly ConcurrentDictionary<string, StateSyncBroker> ActiveBrokersById = new(StringComparer.Ordinal);
    private static int _totalActiveInProcessBrokers;

    static StateSyncBroker()
    {
        SqliteProfileStore.BeforeFlushPathHook = FlushInProcessPendingWritesForDbPath;
    }

    public static StateSyncBroker Shared { get; } = new();

    /// <summary>Total number of currently active (non-disposed) <see cref="StateSyncBroker"/> instances in this process.</summary>
    public static int TotalActiveInProcessBrokers => Volatile.Read(ref _totalActiveInProcessBrokers);

    /// <summary>Returns <c>true</c> if the shared Pub/Sub Engine for <paramref name="channelScope"/> is currently running.</summary>
    public static bool IsChannelEngineRunning(string? channelScope = null)
    {
        var hash = ComputeScopeHash(channelScope);
        return EnginesByScope.TryGetValue(hash, out var engine) && engine.IsRunning;
    }

    internal static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private sealed record SubscriptionEntry(string Topic, Action<StateSyncEvent> Handler);

    private sealed class PendingWriteEntry(StateSyncWriteRequest request)
    {
        public StateSyncWriteRequest Request { get; } = request;
        public TaskCompletionSource<bool> Completion { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
    }

    private readonly string _scopeHash;
    private readonly string _pipeName;
    private readonly string _instancesDir;
    private readonly StateSyncEngine _engine;
    private readonly CancellationTokenSource _cts = new();
    private readonly ConcurrentDictionary<Guid, SubscriptionEntry> _inProcessSubscribers = new();
    private readonly ConcurrentQueue<string> _pendingOutboundLines = new();
    private readonly ConcurrentDictionary<string, PendingWriteEntry> _pendingWriteRequests = new(StringComparer.Ordinal);
    private readonly ConcurrentDictionary<string, byte> _seenEventIds = new(StringComparer.Ordinal);
    private readonly ConcurrentQueue<string> _seenEventIdOrder = new();
    private readonly Lock _clientWriteLock = new();

    private FileStream? _instanceSlotStream;
    private NamedPipeClientStream? _activeClientStream;
    private volatile bool _isConnectedToLeader;
    private volatile bool _disposed;
    private volatile bool _closedEngineOnDispose;

    public StateSyncBroker(string? channelScope = null)
    {
        InstanceId = Guid.NewGuid().ToString("N");
        ChannelScope = string.IsNullOrWhiteSpace(channelScope)
            ? $"{Environment.UserDomainName}\\{Environment.UserName}"
            : channelScope.Trim();

        _scopeHash = ComputeScopeHash(ChannelScope);
        _pipeName = $"SubtitleAdder.StateBroker.{_scopeHash}";

        var baseDir = Path.Combine(Path.GetTempPath(), "SubtitleAdder", $"broker-{_scopeHash}");
        _instancesDir = Path.Combine(baseDir, "instances");
        try { Directory.CreateDirectory(_instancesDir); } catch { }

        _instanceSlotStream = TryCreateInstanceSlotFile(_instancesDir, InstanceId);
        Interlocked.Increment(ref _totalActiveInProcessBrokers);
        ActiveBrokersById[InstanceId] = this;

        _engine = AcquireOrStartEngine(_scopeHash, _pipeName, baseDir, _instancesDir, InstanceId);
        _ = Task.Run(() => RunClientLoopAsync(_cts.Token));
    }

    /// <summary>Unique identifier of this broker/window instance.</summary>
    public string InstanceId { get; }

    /// <summary>Channel scope name shared by communicating instances.</summary>
    public string ChannelScope { get; }

    /// <summary>True while the shared background Pub/Sub Engine for this channel is running.</summary>
    public bool IsEngineRunning => _engine.IsRunning;

    /// <summary>True when this process's shared Engine is actively hosting the Named Pipe Hub Server.</summary>
    public bool IsLeader => !_disposed && _engine.IsHostingPipeServer;

    /// <summary>True when this broker instance is connected over Named Pipe to the running Engine.</summary>
    public bool IsConnectedToLeader => !_disposed && _isConnectedToLeader;

    /// <summary>True when the Engine is running and this broker instance is connected to it.</summary>
    public bool IsReady => !_disposed && _engine.IsRunning && _isConnectedToLeader;

    /// <summary>Number of client instances currently connected to the shared Named Pipe Engine.</summary>
    public int ConnectedClientCount => _engine.ConnectedClientCount;

    /// <summary>Total number of currently open application/broker instances sharing this channel.</summary>
    public int ActiveInstanceCount => _engine.GetActiveInstanceCount();

    /// <summary>True when at least one other application/broker instance is still open on this channel.</summary>
    public bool HasOtherActiveInstances => ActiveInstanceCount > (_disposed ? 0 : 1);

    /// <summary>True when this is the last remaining open application/broker instance on this channel.</summary>
    public bool IsLastInstance => !_disposed && ActiveInstanceCount <= 1;

    /// <summary>
    /// True if and only if this instance was the last remaining open instance when <see cref="Dispose"/> was called
    /// and therefore shut down the shared Pub/Sub Engine. False if other instances were still open when this instance closed.
    /// </summary>
    public bool ClosedEngineOnDispose => _closedEngineOnDispose;

    /// <summary>Number of unacknowledged IPC write requests currently tracked by this broker instance for retry on reconnect.</summary>
    public int PendingWriteRequestCount => _pendingWriteRequests.Count;

    /// <summary>Returns the centralized SQLite Data Worker store owned by the shared <see cref="StateSyncEngine"/> for <paramref name="dbPath"/>.</summary>
    internal SqliteProfileStore GetDataWorkerStore(string dbPath) => _engine.GetOrCreateStore(dbPath);

    /// <summary>Waits until this broker instance is connected to the shared Pub/Sub Engine.</summary>
    public async Task<bool> WaitUntilReadyAsync(TimeSpan timeout)
    {
        var deadline = Environment.TickCount64 + (long)timeout.TotalMilliseconds;
        while (!_disposed && Environment.TickCount64 < deadline)
        {
            if (IsReady) return true;
            await Task.Delay(10).ConfigureAwait(false);
        }
        return IsReady;
    }

    /// <summary>Subscribes to all state sync events across all topics. Dispose the returned token to unsubscribe.</summary>
    public IDisposable Subscribe(Action<StateSyncEvent> handler) =>
        Subscribe(StateSyncTopics.All, handler);

    /// <summary>Subscribes to state sync events for a specific <paramref name="topic"/> (or <see cref="StateSyncTopics.All"/>). Dispose the returned token to unsubscribe.</summary>
    public IDisposable Subscribe(string topic, Action<StateSyncEvent> handler)
    {
        ArgumentNullException.ThrowIfNull(handler);
        var normalizedTopic = string.IsNullOrWhiteSpace(topic) ? StateSyncTopics.All : topic.Trim();
        var id = Guid.NewGuid();
        _inProcessSubscribers[id] = new SubscriptionEntry(normalizedTopic, handler);
        return new SubscriptionToken(this, id);
    }

    /// <summary>
    /// Submits a database write request to the centralized <see cref="StateSyncEngine"/> Data Worker over Named Pipe IPC.
    /// Tracks the request by <see cref="StateSyncWriteRequest.RequestId"/> until acknowledged so any pipe reconnection
    /// automatically resends unacknowledged requests with the same <c>RequestId</c>.
    /// </summary>
    public void SubmitWriteRequest(StateSyncWriteRequest request)
    {
        if (_disposed || request is null) return;
        var normalized = string.IsNullOrWhiteSpace(request.RequestId)
            ? request with { RequestId = Guid.NewGuid().ToString("N") }
            : request;

        _pendingWriteRequests.TryAdd(normalized.RequestId, new PendingWriteEntry(normalized));

        try
        {
            var wireLine = "W:" + JsonSerializer.Serialize(normalized, JsonOptions);
            TrySendToEnginePipe(wireLine);
        }
        catch
        {
            // Kept in _pendingWriteRequests and automatically resent on pipe connect/reconnect.
        }
    }

    /// <summary>
    /// Forces any unacknowledged write requests on this broker to be resent over the Named Pipe with their original <c>RequestId</c>.
    /// </summary>
    public void ResendPendingWriteRequests()
    {
        foreach (var (_, entry) in _pendingWriteRequests)
        {
            try
            {
                var wireLine = "W:" + JsonSerializer.Serialize(entry.Request, JsonOptions);
                if (!TrySendToEnginePipe(wireLine))
                {
                    break;
                }
            }
            catch
            {
                break;
            }
        }
    }

    public Task<bool> FlushPendingWritesAsync(string dbPath)
    {
        if (_disposed) return Task.FromResult(false);
        PushPendingWritesToEngineForDbPath(dbPath);
        return _engine.GetOrCreateStore(dbPath).FlushAsync();
    }

    public bool FlushPendingWrites(string dbPath, TimeSpan? timeout = null)
    {
        if (_disposed) return false;
        PushPendingWritesToEngineForDbPath(dbPath);
        return _engine.GetOrCreateStore(dbPath).Flush(timeout);
    }

    private void PushPendingWritesToEngineForDbPath(string dbPath)
    {
        var full = Path.GetFullPath(dbPath);
        foreach (var (_, entry) in _pendingWriteRequests)
        {
            if (string.Equals(Path.GetFullPath(entry.Request.DbPath), full, StringComparison.OrdinalIgnoreCase) &&
                !_engine.HasEnqueuedRequest(entry.Request.RequestId))
            {
                _engine.EnqueueWriteRequestOnEngine(entry.Request, onAck: HandleWriteAck, fromFallbackFlush: true);
            }
        }
    }

    private static void FlushInProcessPendingWritesForDbPath(string dbPath)
    {
        var full = Path.GetFullPath(dbPath);
        foreach (var (_, broker) in ActiveBrokersById)
        {
            if (!broker._disposed)
            {
                broker.PushPendingWritesToEngineForDbPath(full);
            }
        }
    }

    private void HandleWriteAck(StateSyncWriteAck ack)
    {
        if (ack is null || string.IsNullOrEmpty(ack.RequestId)) return;
        if (ack.Committed)
        {
            if (_pendingWriteRequests.TryRemove(ack.RequestId, out var entry))
            {
                entry.Completion.TrySetResult(true);
            }
        }
        else if (_pendingWriteRequests.TryGetValue(ack.RequestId, out var failedEntry))
        {
            failedEntry.Completion.TrySetResult(false);
        }
    }

    /// <summary>Publishes a state sync event on a specific <paramref name="topic"/> to all local and remote subscribers.</summary>
    public void Publish(string topic, StateSyncEvent syncEvent)
    {
        if (syncEvent is null) return;
        Publish(syncEvent with { Topic = string.IsNullOrWhiteSpace(topic) ? StateSyncTopics.Profiles : topic.Trim() });
    }

    /// <summary>Publishes a state sync event to all local and remote instances connected via the shared Engine.</summary>
    public void Publish(StateSyncEvent syncEvent)
    {
        if (_disposed || syncEvent is null) return;

        // 1. Deliver immediately to topic-matching subscribers attached to this broker instance.
        DispatchToLocalSubscribers(syncEvent);

        // 2. Broadcast over the Named Pipe Engine to all other running instances (1..N).
        try
        {
            var jsonLine = JsonSerializer.Serialize(syncEvent, JsonOptions);
            if (!TrySendToEnginePipe(jsonLine))
            {
                EnqueuePendingLine(jsonLine);
            }
        }
        catch
        {
            // Never fail caller operations if pipe transport is reconnecting.
        }
    }

    private void EnqueuePendingLine(string jsonLine)
    {
        while (_pendingOutboundLines.Count >= 256 && _pendingOutboundLines.TryDequeue(out _))
        {
        }
        _pendingOutboundLines.Enqueue(jsonLine);
    }

    private void FlushPendingOutboundLines()
    {
        ResendPendingWriteRequests();
        while (_pendingOutboundLines.TryDequeue(out var line))
        {
            if (!TrySendToEnginePipe(line))
            {
                EnqueuePendingLine(line);
                break;
            }
        }
    }

    private void DispatchToLocalSubscribers(StateSyncEvent syncEvent)
    {
        if (!string.IsNullOrEmpty(syncEvent.EventId))
        {
            if (!_seenEventIds.TryAdd(syncEvent.EventId, 0))
            {
                return;
            }
            _seenEventIdOrder.Enqueue(syncEvent.EventId);
            while (_seenEventIdOrder.Count > 2048 && _seenEventIdOrder.TryDequeue(out var oldId))
            {
                _seenEventIds.TryRemove(oldId, out _);
            }
        }

        var eventTopic = syncEvent.Topic ?? StateSyncTopics.Profiles;
        foreach (var (_, sub) in _inProcessSubscribers)
        {
            if (sub.Topic != StateSyncTopics.All &&
                !string.Equals(sub.Topic, eventTopic, StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            try
            {
                sub.Handler(syncEvent);
            }
            catch
            {
                // Isolate subscriber exceptions.
            }
        }
    }

    private bool TrySendToEnginePipe(string jsonLine)
    {
        lock (_clientWriteLock)
        {
            var stream = _activeClientStream;
            if (stream is null || !stream.IsConnected) return false;
            try
            {
                var bytes = Encoding.UTF8.GetBytes(jsonLine + "\n");
                stream.Write(bytes, 0, bytes.Length);
                return true;
            }
            catch
            {
                _isConnectedToLeader = false;
                return false;
            }
        }
    }

    private async Task RunClientLoopAsync(CancellationToken ct)
    {
        while (!ct.IsCancellationRequested && !_disposed)
        {
            NamedPipeClientStream? client = null;
            try
            {
                client = new NamedPipeClientStream(".", _pipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
                await client.ConnectAsync(150, ct).ConfigureAwait(false);

                lock (_clientWriteLock)
                {
                    if (_disposed || ct.IsCancellationRequested)
                    {
                        try { client.Dispose(); } catch { }
                        break;
                    }
                    _activeClientStream = client;
                    _isConnectedToLeader = true;
                }

                FlushPendingOutboundLines();

                using var reader = new StreamReader(client, Encoding.UTF8, detectEncodingFromByteOrderMarks: false, bufferSize: 4096, leaveOpen: true);
                while (!ct.IsCancellationRequested && !_disposed && client.IsConnected)
                {
                    var line = await reader.ReadLineAsync(ct).ConfigureAwait(false);
                    if (line is null) break;
                    if (string.IsNullOrWhiteSpace(line)) continue;

                    if (line.StartsWith("A:", StringComparison.Ordinal))
                    {
                        try
                        {
                            var ack = JsonSerializer.Deserialize<StateSyncWriteAck>(line.AsSpan(2), JsonOptions);
                            if (ack is not null)
                            {
                                HandleWriteAck(ack);
                            }
                        }
                        catch
                        {
                        }
                        continue;
                    }

                    StateSyncEvent? evt = null;
                    try
                    {
                        evt = JsonSerializer.Deserialize<StateSyncEvent>(line, JsonOptions);
                    }
                    catch
                    {
                    }

                    if (evt is not null && !string.Equals(evt.SenderInstanceId, InstanceId, StringComparison.Ordinal))
                    {
                        DispatchToLocalSubscribers(evt);
                    }
                }
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested || _disposed)
            {
                break;
            }
            catch
            {
                // Transient disconnect; reconnect while broker is active.
            }
            finally
            {
                lock (_clientWriteLock)
                {
                    if (ReferenceEquals(_activeClientStream, client))
                    {
                        _activeClientStream = null;
                    }
                    _isConnectedToLeader = false;
                }
                if (client is not null)
                {
                    try { client.Dispose(); } catch { }
                }
            }

            if (!ct.IsCancellationRequested && !_disposed)
            {
                try
                {
                    await Task.Delay(15, ct).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    break;
                }
            }
        }
    }

    public void Dispose()
    {
        if (_disposed) return;

        // Ensure any pending write requests from this window are handed off to the centralized Engine before closing.
        foreach (var (_, entry) in _pendingWriteRequests)
        {
            try
            {
                if (!_engine.HasEnqueuedRequest(entry.Request.RequestId))
                {
                    _engine.EnqueueWriteRequestOnEngine(entry.Request, onAck: null, fromFallbackFlush: true);
                }
            }
            catch
            {
            }
        }

        _disposed = true;
        ActiveBrokersById.TryRemove(InstanceId, out _);

        Interlocked.Decrement(ref _totalActiveInProcessBrokers);
        try { _cts.Cancel(); } catch { }

        lock (_clientWriteLock)
        {
            _isConnectedToLeader = false;
            var client = _activeClientStream;
            _activeClientStream = null;
            if (client is not null)
            {
                try { client.Dispose(); } catch { }
            }
        }

        var slot = Interlocked.Exchange(ref _instanceSlotStream, null);
        if (slot is not null)
        {
            try { slot.Dispose(); } catch { }
        }

        // Unregister this instance from the shared Engine; ONLY the last closing instance shuts down the Engine!
        _closedEngineOnDispose = ReleaseInstanceAndShutdownEngineIfLast(_scopeHash, _engine, InstanceId);

        try { _cts.Dispose(); } catch { }
        _inProcessSubscribers.Clear();
    }

    private static string ComputeScopeHash(string? channelScope)
    {
        var rawScope = string.IsNullOrWhiteSpace(channelScope)
            ? $"{Environment.UserDomainName}\\{Environment.UserName}"
            : channelScope.Trim();
        return Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(rawScope)))[..16];
    }

    private static FileStream? TryCreateInstanceSlotFile(string instancesDir, string instanceId)
    {
        try
        {
            Directory.CreateDirectory(instancesDir);
            var slotPath = Path.Combine(instancesDir, $"instance-{instanceId}.slot");
            return new FileStream(
                slotPath,
                FileMode.Create,
                FileAccess.ReadWrite,
                FileShare.None,
                bufferSize: 1,
                FileOptions.DeleteOnClose);
        }
        catch
        {
            return null;
        }
    }

    private static StateSyncEngine AcquireOrStartEngine(
        string scopeHash,
        string pipeName,
        string baseDir,
        string instancesDir,
        string instanceId)
    {
        while (true)
        {
            var engine = EnginesByScope.GetOrAdd(
                scopeHash,
                _ => new StateSyncEngine(scopeHash, pipeName, baseDir, instancesDir));
            if (engine.TryRegisterInstance(instanceId))
            {
                return engine;
            }
            EnginesByScope.TryRemove(KeyValuePair.Create(scopeHash, engine));
        }
    }

    private static bool ReleaseInstanceAndShutdownEngineIfLast(
        string scopeHash,
        StateSyncEngine engine,
        string instanceId)
    {
        var shutDown = engine.UnregisterInstanceAndShutdownIfLast(instanceId);
        if (shutDown)
        {
            EnginesByScope.TryRemove(KeyValuePair.Create(scopeHash, engine));
        }
        return shutDown;
    }

    /// <summary>
    /// Decoupled background Named Pipe Hub &amp; SQLite Data Worker Engine shared by all open <see cref="StateSyncBroker"/> instances
    /// on the same channel. Stays alive across window closes and only shuts down when the last open instance closes.
    /// </summary>
    private sealed class StateSyncEngine
    {
        private readonly string _scopeHash;
        private readonly string _pipeName;
        private readonly string _baseDir;
        private readonly string _instancesDir;
        private readonly string _lockFilePath;
        private readonly CancellationTokenSource _cts = new();
        private readonly ConcurrentDictionary<string, byte> _registeredInstances = new(StringComparer.Ordinal);
        private readonly ConcurrentDictionary<Guid, NamedPipeServerStream> _allServerStreams = new();
        private readonly ConcurrentDictionary<Guid, ConnectedClientSession> _connectedClients = new();
        private readonly ConcurrentDictionary<string, SqliteProfileStore> _storesByDbPath = new(StringComparer.OrdinalIgnoreCase);
        private readonly ConcurrentDictionary<string, byte> _fallbackFlushedRequestIds = new(StringComparer.Ordinal);
        private readonly ConcurrentDictionary<string, byte> _enqueuedRequestIds = new(StringComparer.Ordinal);
        private readonly Lock _lifecycleLock = new();

        private FileStream? _leaderLockStream;
        private volatile bool _isRunning = true;
        private volatile bool _isHostingPipeServer;

        private sealed class ConnectedClientSession(NamedPipeServerStream stream)
        {
            public NamedPipeServerStream Stream { get; } = stream;
            public Lock WriteLock { get; } = new();
        }

        public StateSyncEngine(string scopeHash, string pipeName, string baseDir, string instancesDir)
        {
            _scopeHash = scopeHash;
            _pipeName = pipeName;
            _baseDir = baseDir;
            _instancesDir = instancesDir;
            try { Directory.CreateDirectory(_baseDir); } catch { }
            _lockFilePath = Path.Combine(_baseDir, $"engine-{_scopeHash}.lock");

            _ = Task.Run(() => RunEngineLoopAsync(_cts.Token));
        }

        public bool IsRunning => _isRunning;
        public bool IsHostingPipeServer => _isHostingPipeServer;
        public int ConnectedClientCount => _connectedClients.Count;

        public SqliteProfileStore GetOrCreateStore(string dbPath)
        {
            var full = Path.GetFullPath(dbPath);
            return _storesByDbPath.GetOrAdd(
                full,
                path =>
                {
                    var store = new SqliteProfileStore(path)
                    {
                        PostCommitBroadcastHandler = BroadcastPostCommitEvents,
                    };
                    return store;
                });
        }

        public bool HasEnqueuedRequest(string requestId) =>
            !string.IsNullOrEmpty(requestId) && _enqueuedRequestIds.ContainsKey(requestId);

        public void EnqueueWriteRequestOnEngine(
            StateSyncWriteRequest request,
            Action<StateSyncWriteAck>? onAck,
            bool fromFallbackFlush = false)
        {
            if (request is null || string.IsNullOrWhiteSpace(request.DbPath)) return;

            var reqId = request.RequestId ?? "";
            if (fromFallbackFlush)
            {
                if (!string.IsNullOrEmpty(reqId) && !_enqueuedRequestIds.TryAdd(reqId, 0))
                {
                    return;
                }
                if (!string.IsNullOrEmpty(reqId))
                {
                    _fallbackFlushedRequestIds[reqId] = 0;
                }
            }
            else if (!string.IsNullOrEmpty(reqId))
            {
                _enqueuedRequestIds.TryAdd(reqId, 0);
                // If an explicit synchronous Flush already pushed this exact pipe message milliseconds ago,
                // acknowledge the pipe copy without re-enqueuing the same message twice from the fallback.
                if (_fallbackFlushedRequestIds.TryRemove(reqId, out _))
                {
                    onAck?.Invoke(new StateSyncWriteAck
                    {
                        RequestId = reqId,
                        Committed = true,
                        IsDuplicate = false,
                    });
                    return;
                }
            }

            var store = GetOrCreateStore(request.DbPath);
            if (!Enum.TryParse<SqliteProfileStore.WriteKind>(request.Kind, ignoreCase: true, out var writeKind))
            {
                onAck?.Invoke(new StateSyncWriteAck
                {
                    RequestId = reqId,
                    Committed = false,
                    IsDuplicate = false,
                    Error = $"Unknown WriteKind '{request.Kind}'.",
                });
                return;
            }

            IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)>? batchVideos = null;
            if (request.BatchVideos is { Count: > 0 } bv)
            {
                var list = new List<(string Key, byte[] EncryptedBlob, long UpdatedTicks)>(bv.Count);
                foreach (var v in bv) list.Add((v.Key, v.EncryptedBlob, v.UpdatedTicks));
                batchVideos = list;
            }

            IReadOnlyList<(string Key, byte[] EncryptedBlob, long UpdatedTicks)>? batchProgress = null;
            if (request.BatchProgress is { Count: > 0 } bp)
            {
                var list = new List<(string Key, byte[] EncryptedBlob, long UpdatedTicks)>(bp.Count);
                foreach (var p in bp) list.Add((p.Key, p.EncryptedBlob, p.UpdatedTicks));
                batchProgress = list;
            }

            IReadOnlyList<SqliteProfileStore.ListItemRow>? batchListItems = null;
            if (request.BatchListItems is { Count: > 0 } bli)
            {
                var list = new List<SqliteProfileStore.ListItemRow>(bli.Count);
                foreach (var li in bli) list.Add(new SqliteProfileStore.ListItemRow(li.ListId, li.ItemHash, li.Ord, li.EncryptedPathBlob));
                batchListItems = list;
            }

            var writeItem = new SqliteProfileStore.WriteItem(
                Kind: writeKind,
                Username: request.Username,
                Key: request.Key,
                SecondaryKey: request.SecondaryKey,
                Ord: request.Ord,
                Blob: request.Blob,
                SecondaryBlob: request.SecondaryBlob,
                UpdatedTicks: request.UpdatedTicks,
                ViewCount: request.ViewCount,
                ViewCountDelta: request.ViewCountDelta,
                RequestId: reqId,
                SaltBase64: request.SaltBase64,
                PasswordHashBase64: request.PasswordHashBase64,
                Iterations: request.Iterations,
                CreatedAtUtc: request.CreatedAtUtc,
                BatchVideos: batchVideos,
                BatchProgress: batchProgress,
                BatchListItems: batchListItems,
                PostCommitEvents: request.PostCommitEvents,
                OnCompleted: (committed, isDuplicate, error) =>
                {
                    var ack = new StateSyncWriteAck
                    {
                        RequestId = reqId,
                        Committed = committed,
                        IsDuplicate = isDuplicate,
                        Error = error,
                    };
                    onAck?.Invoke(ack);
                    if (ActiveBrokersById.TryGetValue(request.SenderInstanceId, out var senderBroker))
                    {
                        senderBroker.HandleWriteAck(ack);
                    }
                });

            store.EnqueueItem(writeItem);
        }

        private void BroadcastPostCommitEvents(IReadOnlyList<StateSyncEvent> events)
        {
            if (events is null || events.Count == 0) return;
            foreach (var evt in events)
            {
                if (evt is null) continue;
                try
                {
                    var jsonLine = JsonSerializer.Serialize(evt, JsonOptions);
                    BroadcastToAllClients(jsonLine);
                }
                catch
                {
                }

                // Also deliver to in-process brokers registered on this Engine (deduplicated via EventId).
                foreach (var instanceId in _registeredInstances.Keys)
                {
                    if (ActiveBrokersById.TryGetValue(instanceId, out var broker) && !broker._disposed)
                    {
                        broker.DispatchToLocalSubscribers(evt);
                    }
                }
            }
        }

        public bool TryRegisterInstance(string instanceId)
        {
            lock (_lifecycleLock)
            {
                if (!_isRunning) return false;
                _registeredInstances[instanceId] = 1;
                return true;
            }
        }

        public int GetActiveInstanceCount()
        {
            lock (_lifecycleLock)
            {
                if (!_isRunning) return 0;
                var inProcessCount = _registeredInstances.Count;
                var crossProcessSlotCount = CountActiveSlotFiles();
                return Math.Max(inProcessCount, crossProcessSlotCount);
            }
        }

        public bool UnregisterInstanceAndShutdownIfLast(string instanceId)
        {
            lock (_lifecycleLock)
            {
                _registeredInstances.TryRemove(instanceId, out _);
                if (!_isRunning) return false;

                var remainingInProcess = _registeredInstances.Count;
                var remainingSlotFiles = CountActiveSlotFiles();
                var totalRemaining = Math.Max(remainingInProcess, remainingSlotFiles);

                if (totalRemaining > 0)
                {
                    // Other application windows are still open -> keep the Engine and SQLite Data Worker running!
                    return false;
                }

                // This was the LAST open application window -> shut down the Engine and flush/dispose Data Worker now!
                _isRunning = false;
                _isHostingPipeServer = false;
            }

            ShutdownEngineCore();
            return true;
        }

        private int CountActiveSlotFiles()
        {
            try
            {
                if (!Directory.Exists(_instancesDir)) return 0;
                var activeCount = 0;
                foreach (var file in Directory.EnumerateFiles(_instancesDir, "instance-*.slot"))
                {
                    try
                    {
                        // Slot files of live instances are held with FileShare.None.
                        // If we can open/delete one exclusively, it was an orphaned file; otherwise it is actively held.
                        File.Delete(file);
                    }
                    catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
                    {
                        activeCount++;
                    }
                }
                return activeCount;
            }
            catch
            {
                return _registeredInstances.Count;
            }
        }

        private FileStream? TryAcquireEngineLock()
        {
            try
            {
                Directory.CreateDirectory(_baseDir);
                return new FileStream(
                    _lockFilePath,
                    FileMode.OpenOrCreate,
                    FileAccess.ReadWrite,
                    FileShare.None,
                    bufferSize: 1,
                    FileOptions.None);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                return null;
            }
        }

        private async Task RunEngineLoopAsync(CancellationToken ct)
        {
            while (!ct.IsCancellationRequested && _isRunning)
            {
                var lockStream = TryAcquireEngineLock();
                if (lockStream is not null)
                {
                    _leaderLockStream = lockStream;
                    _isHostingPipeServer = true;
                    try
                    {
                        var accept1 = AcceptClientsLoopAsync(ct);
                        var accept2 = AcceptClientsLoopAsync(ct);
                        await Task.WhenAll(accept1, accept2).ConfigureAwait(false);
                    }
                    catch
                    {
                    }
                    finally
                    {
                        _isHostingPipeServer = false;
                        var held = Interlocked.Exchange(ref _leaderLockStream, null);
                        if (held is not null)
                        {
                            try { held.Dispose(); } catch { }
                        }
                    }
                }
                else
                {
                    // Another OS process is currently hosting the Named Pipe Engine for this scope;
                    // standby and automatically take over if that process exits while our instances are still open.
                    _isHostingPipeServer = false;
                    try
                    {
                        await Task.Delay(25, ct).ConfigureAwait(false);
                    }
                    catch (OperationCanceledException)
                    {
                        break;
                    }
                }
            }
        }

        private async Task AcceptClientsLoopAsync(CancellationToken ct)
        {
            while (!ct.IsCancellationRequested && _isRunning)
            {
                var streamId = Guid.NewGuid();
                NamedPipeServerStream? server = null;
                try
                {
                    server = new NamedPipeServerStream(
                        _pipeName,
                        PipeDirection.InOut,
                        NamedPipeServerStream.MaxAllowedServerInstances,
                        PipeTransmissionMode.Byte,
                        PipeOptions.Asynchronous);
                    _allServerStreams[streamId] = server;

                    await server.WaitForConnectionAsync(ct).ConfigureAwait(false);
                    if (ct.IsCancellationRequested || !_isRunning)
                    {
                        _allServerStreams.TryRemove(streamId, out _);
                        try { server.Dispose(); } catch { }
                        break;
                    }

                    _ = Task.Run(() => HandleConnectedClientAsync(streamId, server, ct), CancellationToken.None);
                }
                catch
                {
                    _allServerStreams.TryRemove(streamId, out _);
                    if (server is not null)
                    {
                        try { server.Dispose(); } catch { }
                    }
                    if (ct.IsCancellationRequested || !_isRunning) break;
                }
            }
        }

        private async Task HandleConnectedClientAsync(Guid streamId, NamedPipeServerStream server, CancellationToken ct)
        {
            var session = new ConnectedClientSession(server);
            _connectedClients[streamId] = session;

            try
            {
                using var reader = new StreamReader(server, Encoding.UTF8, detectEncodingFromByteOrderMarks: false, bufferSize: 4096, leaveOpen: true);
                while (!ct.IsCancellationRequested && _isRunning && server.IsConnected)
                {
                    var line = await reader.ReadLineAsync(ct).ConfigureAwait(false);
                    if (line is null) break;
                    if (string.IsNullOrWhiteSpace(line)) continue;

                    if (line.StartsWith("W:", StringComparison.Ordinal))
                    {
                        StateSyncWriteRequest? writeReq = null;
                        try
                        {
                            writeReq = JsonSerializer.Deserialize<StateSyncWriteRequest>(line.AsSpan(2), JsonOptions);
                        }
                        catch
                        {
                        }

                        if (writeReq is not null)
                        {
                            EnqueueWriteRequestOnEngine(
                                writeReq,
                                onAck: ack => SendLineToClient(streamId, "A:" + JsonSerializer.Serialize(ack, JsonOptions)),
                                fromFallbackFlush: false);
                        }
                        continue;
                    }

                    BroadcastToOtherClients(line, excludeClientId: streamId);
                }
            }
            catch
            {
                // Client disconnected or Engine shutting down.
            }
            finally
            {
                _connectedClients.TryRemove(streamId, out _);
                _allServerStreams.TryRemove(streamId, out _);
                try { server.Dispose(); } catch { }
            }
        }

        private void SendLineToClient(Guid clientId, string wireLine)
        {
            if (!_connectedClients.TryGetValue(clientId, out var session)) return;
            var bytes = Encoding.UTF8.GetBytes(wireLine + "\n");
            try
            {
                lock (session.WriteLock)
                {
                    if (session.Stream.IsConnected)
                    {
                        session.Stream.Write(bytes, 0, bytes.Length);
                    }
                }
            }
            catch
            {
                _connectedClients.TryRemove(clientId, out _);
            }
        }

        private void BroadcastToAllClients(string jsonLine)
        {
            var bytes = Encoding.UTF8.GetBytes(jsonLine + "\n");
            foreach (var (clientId, session) in _connectedClients)
            {
                try
                {
                    lock (session.WriteLock)
                    {
                        if (session.Stream.IsConnected)
                        {
                            session.Stream.Write(bytes, 0, bytes.Length);
                        }
                    }
                }
                catch
                {
                    _connectedClients.TryRemove(clientId, out _);
                }
            }
        }

        private void BroadcastToOtherClients(string jsonLine, Guid excludeClientId)
        {
            var bytes = Encoding.UTF8.GetBytes(jsonLine + "\n");
            foreach (var (clientId, session) in _connectedClients)
            {
                if (clientId == excludeClientId) continue;
                try
                {
                    lock (session.WriteLock)
                    {
                        if (session.Stream.IsConnected)
                        {
                            session.Stream.Write(bytes, 0, bytes.Length);
                        }
                    }
                }
                catch
                {
                    _connectedClients.TryRemove(clientId, out _);
                }
            }
        }

        private void ShutdownEngineCore()
        {
            try { _cts.Cancel(); } catch { }

            foreach (var (_, store) in _storesByDbPath)
            {
                try
                {
                    store.Flush(TimeSpan.FromSeconds(5));
                    store.Dispose();
                }
                catch
                {
                }
            }
            _storesByDbPath.Clear();

            var heldLock = Interlocked.Exchange(ref _leaderLockStream, null);
            if (heldLock is not null)
            {
                try { heldLock.Dispose(); } catch { }
            }

            foreach (var (id, stream) in _allServerStreams)
            {
                _allServerStreams.TryRemove(id, out _);
                try { stream.Dispose(); } catch { }
            }
            _connectedClients.Clear();

            try
            {
                if (File.Exists(_lockFilePath)) File.Delete(_lockFilePath);
                if (Directory.Exists(_instancesDir) && !Directory.EnumerateFileSystemEntries(_instancesDir).Any())
                {
                    Directory.Delete(_instancesDir, recursive: false);
                }
                if (Directory.Exists(_baseDir) && !Directory.EnumerateFileSystemEntries(_baseDir).Any())
                {
                    Directory.Delete(_baseDir, recursive: false);
                }
            }
            catch
            {
            }

            try { _cts.Dispose(); } catch { }
        }
    }

    private sealed class SubscriptionToken(StateSyncBroker broker, Guid id) : IDisposable
    {
        public void Dispose()
        {
            broker._inProcessSubscribers.TryRemove(id, out _);
        }
    }
}
