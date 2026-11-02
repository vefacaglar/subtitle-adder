using System.Globalization;

namespace SubtitleAdder.Core;

/// <summary>Shared seek/display position while Media Foundation finishes an asynchronous seek.</summary>
public sealed class PlaybackTimeline
{
    private double? _pending;
    private DateTimeOffset _requestedAt;
    public double Position { get; private set; }
    public double Duration { get; private set; }

    public void Reset()
    {
        Position = Duration = 0;
        _pending = null;
    }

    public double Seek(double seconds, DateTimeOffset now)
    {
        Position = Clamp(seconds, Duration);
        _pending = Position;
        _requestedAt = now;
        return Position;
    }

    public void Observe(double position, double duration, DateTimeOffset now)
    {
        Duration = double.IsFinite(duration) ? Math.Max(0, duration) : 0;
        position = Clamp(position, Duration);
        if (_pending is { } pending)
        {
            pending = Clamp(pending, Duration);
            // Polls can still return the old frame after Position was assigned. Keep the requested
            // position briefly, but recover if the decoder cannot seek to that exact timestamp.
            if (Math.Abs(position - pending) > 0.25 && now - _requestedAt < TimeSpan.FromSeconds(2))
            {
                Position = pending;
                return;
            }
            _pending = null;
        }
        Position = position;
    }

    public static double Clamp(double seconds, double duration) =>
        double.IsFinite(seconds) ? Math.Clamp(seconds, 0, duration > 0 ? duration : double.MaxValue) : 0;

    public static string FormatTime(double seconds, bool hours = false)
    {
        var total = (long)Math.Floor(Math.Clamp(double.IsFinite(seconds) ? seconds : 0, 0, TimeSpan.MaxValue.TotalSeconds));
        return hours || total >= 3600
            ? string.Create(CultureInfo.InvariantCulture, $"{total / 3600:00}:{total / 60 % 60:00}:{total % 60:00}")
            : string.Create(CultureInfo.InvariantCulture, $"{total / 60:00}:{total % 60:00}");
    }
}

/// <summary>
/// Immutable snapshot of a player session/source bound at source creation or event dispatch time.
/// </summary>
public readonly record struct PlayerEventToken(int Generation, object? Source);

/// <summary>
/// Thread-safe session gate that correlates asynchronous <c>MediaPlayer</c> / <c>MediaSource</c> events
/// with the exact load generation and source instance that originated them, preventing late events from a previous
/// video from mutating or failing a newly loaded video even if the late event arrives after the UI thread has
/// already activated the new source.
/// </summary>
public sealed class PlayerSessionGate
{
    private readonly Lock _gate = new();
    private int _loadGeneration;
    private int _activeGeneration;
    private object? _activeSource;
    private bool _activeSourceOpened;
    private bool _activeSourceFailed;
    private string? _activeSourceError;

    public int LoadGeneration => Volatile.Read(ref _loadGeneration);
    public int ActiveGeneration => Volatile.Read(ref _activeGeneration);
    public object? ActiveSource => Volatile.Read(ref _activeSource);

    public bool HasActiveSourceOpened
    {
        get
        {
            lock (_gate) return _activeSourceOpened;
        }
    }

    public bool HasActiveSourceFailed
    {
        get
        {
            lock (_gate) return _activeSourceFailed;
        }
    }

    public string? ActiveSourceError
    {
        get
        {
            lock (_gate) return _activeSourceError;
        }
    }

    public int BeginLoad()
    {
        lock (_gate)
        {
            _activeSourceOpened = false;
            _activeSourceFailed = false;
            _activeSourceError = null;
            return Interlocked.Increment(ref _loadGeneration);
        }
    }

    public bool IsCurrentLoad(int generation) =>
        generation != 0 && generation == Volatile.Read(ref _loadGeneration);

    /// <summary>
    /// Creates a token permanently bound to a specific <paramref name="generation"/> and <paramref name="source"/> instance.
    /// Closures attached to <paramref name="source"/> carry this bound token so late events from a previous source
    /// never impersonate a newly activated source.
    /// </summary>
    public PlayerEventToken CreateBoundToken(int generation, object? source) =>
        new(generation, source);

    public void ClearActiveSource()
    {
        lock (_gate)
        {
            Volatile.Write(ref _activeGeneration, 0);
            Volatile.Write(ref _activeSource, null);
            _activeSourceOpened = false;
            _activeSourceFailed = false;
            _activeSourceError = null;
        }
    }

    public bool TryActivateSource(int generation, object? source)
    {
        lock (_gate)
        {
            if (generation == 0 || generation != Volatile.Read(ref _loadGeneration))
            {
                return false;
            }
            var isSameSource = ReferenceEquals(_activeSource, source) && _activeGeneration == generation;
            Volatile.Write(ref _activeGeneration, source is null ? 0 : generation);
            Volatile.Write(ref _activeSource, source);
            if (!isSameSource)
            {
                _activeSourceOpened = false;
                _activeSourceFailed = false;
                _activeSourceError = null;
            }
            return true;
        }
    }

    /// <summary>
    /// Records the open result reported directly by a <c>MediaSource</c>'s own bound <paramref name="sourceToken"/>.
    /// Returns <c>false</c> and ignores the notification if <paramref name="sourceToken"/> belongs to a previous generation/source.
    /// </summary>
    public bool NotifySourceOpenCompleted(PlayerEventToken sourceToken, bool succeeded, string? errorDetails = null)
    {
        if (sourceToken.Generation == 0 || sourceToken.Source is null)
        {
            return false;
        }
        lock (_gate)
        {
            if (sourceToken.Generation != _loadGeneration ||
                (_activeSource is not null && !ReferenceEquals(sourceToken.Source, _activeSource)))
            {
                return false;
            }
            if (succeeded)
            {
                _activeSourceOpened = true;
                _activeSourceFailed = false;
                _activeSourceError = null;
            }
            else
            {
                _activeSourceFailed = true;
                _activeSourceError = errorDetails;
            }
            return true;
        }
    }

    /// <summary>
    /// Records state transitions reported directly by a <c>MediaSource</c>'s own bound <paramref name="sourceToken"/>.
    /// Returns <c>false</c> if <paramref name="sourceToken"/> belongs to a stale generation/source.
    /// </summary>
    public bool NotifySourceStateChanged(PlayerEventToken sourceToken, bool isOpened, bool isFailed)
    {
        if (sourceToken.Generation == 0 || sourceToken.Source is null)
        {
            return false;
        }
        lock (_gate)
        {
            if (sourceToken.Generation != _loadGeneration ||
                (_activeSource is not null && !ReferenceEquals(sourceToken.Source, _activeSource)))
            {
                return false;
            }
            if (isOpened)
            {
                _activeSourceOpened = true;
                _activeSourceFailed = false;
            }
            else if (isFailed)
            {
                _activeSourceFailed = true;
            }
            return true;
        }
    }

    /// <summary>
    /// Captures the currently active <c>(Generation, Source)</c> at the moment a shared <c>MediaPlayer</c> event fires.
    /// </summary>
    public PlayerEventToken CaptureEventToken()
    {
        lock (_gate)
        {
            return new PlayerEventToken(
                Volatile.Read(ref _activeGeneration),
                Volatile.Read(ref _activeSource));
        }
    }

    /// <summary>
    /// Validates inside the UI dispatcher callback that <paramref name="token"/> was raised by the currently active
    /// generation and source instance, and matches <paramref name="currentPlayerSource"/>.
    /// </summary>
    public bool ShouldAcceptEvent(PlayerEventToken token, object? currentPlayerSource)
    {
        if (token.Generation == 0 || token.Source is null)
        {
            return false;
        }
        lock (_gate)
        {
            var currentLoad = Volatile.Read(ref _loadGeneration);
            var activeGen = Volatile.Read(ref _activeGeneration);
            var activeSrc = Volatile.Read(ref _activeSource);
            return token.Generation == currentLoad &&
                   token.Generation == activeGen &&
                   activeSrc is not null &&
                   ReferenceEquals(token.Source, activeSrc) &&
                   ReferenceEquals(currentPlayerSource, activeSrc);
        }
    }

    /// <summary>
    /// Validates a <c>MediaOpened</c> event. Even if a delayed <c>Player.MediaOpened</c> from source A arrives after
    /// source B was activated, it is rejected unless <paramref name="token"/> matches the active source AND
    /// the active source itself has actually opened (<paramref name="isActiveSourceInOpenedState"/> or <see cref="HasActiveSourceOpened"/>).
    /// </summary>
    public bool ShouldAcceptMediaOpened(PlayerEventToken token, object? currentPlayerSource, bool isActiveSourceInOpenedState = true)
    {
        if (!ShouldAcceptEvent(token, currentPlayerSource))
        {
            return false;
        }
        lock (_gate)
        {
            if (_activeSourceFailed) return false;
            return isActiveSourceInOpenedState || _activeSourceOpened;
        }
    }

    /// <summary>
    /// Validates a <c>MediaFailed</c> event. Even if a delayed <c>Player.MediaFailed</c> from source A arrives on the shared
    /// <c>MediaPlayer</c> AFTER source B was already activated, it is rejected unless <paramref name="token"/> matches
    /// the active source AND the active source itself is in a failed state (<paramref name="isActiveSourceInFailedState"/> or <see cref="HasActiveSourceFailed"/>).
    /// </summary>
    public bool ShouldAcceptMediaFailed(PlayerEventToken token, object? currentPlayerSource, bool isActiveSourceInFailedState)
    {
        if (!ShouldAcceptEvent(token, currentPlayerSource))
        {
            return false;
        }
        lock (_gate)
        {
            return isActiveSourceInFailedState || _activeSourceFailed;
        }
    }
}
