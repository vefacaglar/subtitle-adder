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

/// <summary>
/// Shared background Pub/Sub communication engine that coordinates all state updates across 1..N running
/// <c>SubtitleAdder</c> instances without <see cref="FileSystemWatcher"/> polling.
/// <para>
/// Topology:
/// <list type="bullet">
/// <item><description>Any number of open instances (1, 2, 5, 10+) share a single Named Pipe channel (<c>SubtitleAdder.StateBroker.&lt;Hash&gt;</c>).</description></item>
/// <item><description>One instance automatically acquires an exclusive handle lock and acts as the Named Pipe Hub Leader, accepting connections from all other open instances and fanning out topic-filtered <see cref="StateSyncEvent"/> messages.</description></item>
/// <item><description>If the Leader window is closed, one of the remaining open instances automatically takes over as the new Hub Leader within milliseconds and all other instances reconnect seamlessly without losing queued events.</description></item>
/// </list>
/// </para>
/// </summary>
public sealed class StateSyncBroker : IDisposable
{
    public static StateSyncBroker Shared { get; } = new();

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private sealed record SubscriptionEntry(string Topic, Action<StateSyncEvent> Handler);

    private readonly string _pipeName;
    private readonly string _lockFilePath;
    private readonly CancellationTokenSource _cts = new();
    private readonly ConcurrentDictionary<Guid, SubscriptionEntry> _inProcessSubscribers = new();
    private readonly ConcurrentDictionary<Guid, StreamWriter> _connectedClients = new();
    private readonly ConcurrentQueue<string> _pendingOutboundLines = new();
    private readonly Lock _clientWriteLock = new();

    private StreamWriter? _outboundClientWriter;
    private volatile bool _isLeader;
    private volatile bool _isConnectedToLeader;
    private volatile bool _disposed;

    public StateSyncBroker(string? channelScope = null)
    {
        var rawScope = string.IsNullOrWhiteSpace(channelScope)
            ? $"{Environment.UserDomainName}\\{Environment.UserName}"
            : channelScope;
        var hash = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(rawScope)))[..16];
        _pipeName = $"SubtitleAdder.StateBroker.{hash}";

        var lockDir = Path.Combine(Path.GetTempPath(), "SubtitleAdder");
        try { Directory.CreateDirectory(lockDir); } catch { }
        _lockFilePath = Path.Combine(lockDir, $"state-broker-{hash}.lock");

        _ = Task.Run(() => RunBrokerLoopAsync(_cts.Token));
    }

    /// <summary>True when this broker instance is currently the active Named Pipe Hub Leader.</summary>
    public bool IsLeader => _isLeader;

    /// <summary>True when this broker instance is connected as a client to the active Named Pipe Hub Leader.</summary>
    public bool IsConnectedToLeader => _isConnectedToLeader;

    /// <summary>True when this broker is either the active Hub Leader or connected to the active Hub Leader.</summary>
    public bool IsReady => _isLeader || _isConnectedToLeader;

    /// <summary>Number of remote instances currently connected to this broker when acting as Hub Leader.</summary>
    public int ConnectedClientCount => _connectedClients.Count;

    /// <summary>Waits until this broker instance has either become the Hub Leader or connected to the Hub Leader.</summary>
    public async Task<bool> WaitUntilReadyAsync(TimeSpan timeout)
    {
        var deadline = Environment.TickCount64 + (long)timeout.TotalMilliseconds;
        while (!_disposed && Environment.TickCount64 < deadline)
        {
            if (IsReady) return true;
            await Task.Delay(15).ConfigureAwait(false);
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

    /// <summary>Publishes a state sync event on a specific <paramref name="topic"/> to all local and remote subscribers.</summary>
    public void Publish(string topic, StateSyncEvent syncEvent)
    {
        if (syncEvent is null) return;
        Publish(syncEvent with { Topic = string.IsNullOrWhiteSpace(topic) ? StateSyncTopics.Profiles : topic.Trim() });
    }

    /// <summary>Publishes a state sync event to all local and remote instances connected via the broker.</summary>
    public void Publish(StateSyncEvent syncEvent)
    {
        if (_disposed || syncEvent is null) return;

        // 1. Deliver immediately to topic-matching subscribers attached to this broker instance.
        DispatchToLocalSubscribers(syncEvent);

        // 2. Broadcast over the Named Pipe hub to all other running instances (1..N).
        try
        {
            var jsonLine = JsonSerializer.Serialize(syncEvent, JsonOptions);
            if (_isLeader)
            {
                BroadcastToConnectedPipeClients(jsonLine, excludeClientId: null);
            }
            else if (!TrySendToLeaderPipe(jsonLine))
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
        while (_pendingOutboundLines.TryDequeue(out var line))
        {
            if (_isLeader)
            {
                BroadcastToConnectedPipeClients(line, excludeClientId: null);
            }
            else if (!TrySendToLeaderPipe(line))
            {
                EnqueuePendingLine(line);
                break;
            }
        }
    }

    private void DispatchToLocalSubscribers(StateSyncEvent syncEvent)
    {
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

    private bool TrySendToLeaderPipe(string jsonLine)
    {
        lock (_clientWriteLock)
        {
            if (_outboundClientWriter is null) return false;
            try
            {
                _outboundClientWriter.WriteLine(jsonLine);
                _outboundClientWriter.Flush();
                return true;
            }
            catch
            {
                _outboundClientWriter = null;
                _isConnectedToLeader = false;
                return false;
            }
        }
    }

    private void BroadcastToConnectedPipeClients(string jsonLine, Guid? excludeClientId)
    {
        foreach (var (clientId, writer) in _connectedClients)
        {
            if (excludeClientId.HasValue && clientId == excludeClientId.Value)
                continue;

            try
            {
                lock (writer)
                {
                    writer.WriteLine(jsonLine);
                    writer.Flush();
                }
            }
            catch
            {
                _connectedClients.TryRemove(clientId, out _);
            }
        }
    }

    private FileStream? TryAcquireLeaderLock()
    {
        try
        {
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

    private async Task RunBrokerLoopAsync(CancellationToken ct)
    {
        while (!ct.IsCancellationRequested && !_disposed)
        {
            FileStream? leaderLock = null;
            try
            {
                leaderLock = TryAcquireLeaderLock();
                if (leaderLock is not null)
                {
                    _isLeader = true;
                    _isConnectedToLeader = false;
                    FlushPendingOutboundLines();
                    await RunLeaderServerLoopAsync(ct).ConfigureAwait(false);
                }
                else
                {
                    _isLeader = false;
                    await RunClientConnectionAsync(ct).ConfigureAwait(false);
                }
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested)
            {
                break;
            }
            catch
            {
                // Transient pipe/lock error; retry after short backoff allowing instant leader election.
            }
            finally
            {
                _isLeader = false;
                _isConnectedToLeader = false;
                if (leaderLock is not null)
                {
                    try { leaderLock.Dispose(); } catch { }
                }
            }

            if (!ct.IsCancellationRequested && !_disposed)
            {
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

    private async Task RunLeaderServerLoopAsync(CancellationToken ct)
    {
        // Maintain 6 concurrent accept loops so even if 5+ app instances start or failover simultaneously,
        // every client connects on the first attempt without waiting.
        const int concurrentAcceptLoops = 6;
        var loops = new Task[concurrentAcceptLoops];
        for (var i = 0; i < concurrentAcceptLoops; i++)
        {
            loops[i] = AcceptClientsLoopAsync(ct);
        }

        try
        {
            await Task.WhenAll(loops).ConfigureAwait(false);
        }
        finally
        {
            foreach (var (_, writer) in _connectedClients)
            {
                try { writer.Dispose(); } catch { }
            }
            _connectedClients.Clear();
        }
    }

    private async Task AcceptClientsLoopAsync(CancellationToken ct)
    {
        var clientTasks = new List<Task>();
        while (!ct.IsCancellationRequested && !_disposed)
        {
            var server = new NamedPipeServerStream(
                _pipeName,
                PipeDirection.InOut,
                NamedPipeServerStream.MaxAllowedServerInstances,
                PipeTransmissionMode.Byte,
                PipeOptions.Asynchronous);

            try
            {
                await server.WaitForConnectionAsync(ct).ConfigureAwait(false);
                clientTasks.RemoveAll(t => t.IsCompleted);
                clientTasks.Add(Task.Run(() => HandleConnectedClientAsync(server, ct), CancellationToken.None));
            }
            catch
            {
                try { await server.DisposeAsync().ConfigureAwait(false); } catch { }
                if (ct.IsCancellationRequested || _disposed) break;
            }
        }

        try { await Task.WhenAll(clientTasks).ConfigureAwait(false); } catch { }
    }

    private async Task HandleConnectedClientAsync(NamedPipeServerStream server, CancellationToken ct)
    {
        var clientId = Guid.NewGuid();
        using var reg = ct.Register(() =>
        {
            try { server.Dispose(); } catch { }
        });

        await using (server)
        {
            using var reader = new StreamReader(server, Encoding.UTF8, detectEncodingFromByteOrderMarks: false, bufferSize: 4096, leaveOpen: true);
            using var writer = new StreamWriter(server, new UTF8Encoding(false), bufferSize: 4096, leaveOpen: true);
            _connectedClients[clientId] = writer;

            try
            {
                while (!ct.IsCancellationRequested && !_disposed && server.IsConnected)
                {
                    var line = await reader.ReadLineAsync(ct).ConfigureAwait(false);
                    if (line is null) break;
                    if (string.IsNullOrWhiteSpace(line)) continue;

                    StateSyncEvent? evt = null;
                    try
                    {
                        evt = JsonSerializer.Deserialize<StateSyncEvent>(line, JsonOptions);
                    }
                    catch
                    {
                    }

                    if (evt is not null)
                    {
                        DispatchToLocalSubscribers(evt);
                        BroadcastToConnectedPipeClients(line, excludeClientId: clientId);
                    }
                }
            }
            catch
            {
                // Client disconnected or broker shutting down.
            }
            finally
            {
                _connectedClients.TryRemove(clientId, out _);
            }
        }
    }

    private async Task RunClientConnectionAsync(CancellationToken ct)
    {
        await using var client = new NamedPipeClientStream(".", _pipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
        using var reg = ct.Register(() =>
        {
            try { client.Dispose(); } catch { }
        });

        await client.ConnectAsync(200, ct).ConfigureAwait(false);

        using var reader = new StreamReader(client, Encoding.UTF8, detectEncodingFromByteOrderMarks: false, bufferSize: 4096, leaveOpen: true);
        using var writer = new StreamWriter(client, new UTF8Encoding(false), bufferSize: 4096, leaveOpen: true);

        lock (_clientWriteLock)
        {
            _outboundClientWriter = writer;
            _isConnectedToLeader = true;
        }

        FlushPendingOutboundLines();

        try
        {
            while (!ct.IsCancellationRequested && !_disposed && client.IsConnected)
            {
                var line = await reader.ReadLineAsync(ct).ConfigureAwait(false);
                if (line is null) break;
                if (string.IsNullOrWhiteSpace(line)) continue;

                StateSyncEvent? evt = null;
                try
                {
                    evt = JsonSerializer.Deserialize<StateSyncEvent>(line, JsonOptions);
                }
                catch
                {
                }

                if (evt is not null)
                {
                    DispatchToLocalSubscribers(evt);
                }
            }
        }
        finally
        {
            lock (_clientWriteLock)
            {
                if (ReferenceEquals(_outboundClientWriter, writer))
                {
                    _outboundClientWriter = null;
                }
                _isConnectedToLeader = false;
            }
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        try { _cts.Cancel(); } catch { }
        foreach (var (_, writer) in _connectedClients)
        {
            try { writer.Dispose(); } catch { }
        }
        _connectedClients.Clear();
        try { _cts.Dispose(); } catch { }
        _inProcessSubscribers.Clear();
    }

    private sealed class SubscriptionToken(StateSyncBroker broker, Guid id) : IDisposable
    {
        public void Dispose()
        {
            broker._inProcessSubscribers.TryRemove(id, out _);
        }
    }
}
