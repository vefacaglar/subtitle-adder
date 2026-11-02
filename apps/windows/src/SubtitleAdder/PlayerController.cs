using System.IO;
using Microsoft.UI.Dispatching;
using SubtitleAdder.Core;
using SubtitleAdder.EngineKit;
using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.Storage.Streams;

namespace SubtitleAdder;

/// <summary>
/// Preview player (port of PlayerModel): wraps <see cref="MediaPlayer"/>, tracks the ~10 Hz playback time and the
/// line currently playing. MediaPlayer events arrive on worker threads; everything here is marshalled to the UI thread.
/// Uses <see cref="BufferedVideoStream"/> (chunked 8 MB sequential read-ahead RAM cache) so mechanical HDDs are not
/// thrashed by continuous small random reads and video switching/seeking responds instantaneously.
/// </summary>
public sealed class PlayerController : IDisposable
{
    private readonly DispatcherQueue _queue;
    private readonly DispatcherQueueTimer _timer;
    private readonly SemaphoreSlim _sourceGate = new(1, 1);
    private IReadOnlyList<Sentence> _sentences = [];
    private int _loadGeneration;
    private MediaSource? _source;                   // disposed when replaced so the old file is not kept open
    private IRandomAccessStream? _randomAccessStream;
    private BufferedVideoStream? _bufferedStream;   // 8 MB chunked RAM read-ahead cache
    private volatile bool _mediaReady;
    private volatile bool _disposed;
    private readonly PlaybackTimeline _timeline = new();
    private string? _currentPath;
    private double? _seekWhenReady;
    private bool _autoPlayWhenReady;

    public PlayerController(DispatcherQueue queue)
    {
        _queue = queue;
        Player.AutoPlay = false;
        Player.MediaOpened += (_, _) => _queue.TryEnqueue(() =>
        {
            if (_disposed) return;
            _mediaReady = true;
            UpdateVideoSize();
            if (_seekWhenReady is { } target)
            {
                _seekWhenReady = null;
                Seek(target);
            }
            if (_autoPlayWhenReady)
            {
                _autoPlayWhenReady = false;
                Player.Play();
                IsPlaying = true;
            }
            Tick();
        });
        Player.MediaFailed += (_, e) => _queue.TryEnqueue(() =>
        {
            _mediaReady = false;
            Failed?.Invoke(e.ErrorMessage);
        });
        Player.PlaybackSession.NaturalVideoSizeChanged += (_, _) => _queue.TryEnqueue(UpdateVideoSize);
        _timer = queue.CreateTimer();
        _timer.Interval = TimeSpan.FromMilliseconds(100);
        _timer.Tick += (_, _) => Tick();
        _timer.Start();
    }

    public MediaPlayer Player { get; } = new();

    public double CurrentTime => _timeline.Position;
    public double Duration => _timeline.Duration;
    public double BufferedRatio =>
        !_disposed && _bufferedStream is { } stream && Duration > 0
            ? stream.GetBufferedRatio(CurrentTime / Duration)
            : 0.0;
    public bool IsPlaying { get; private set; }
    public bool IsMuted => !_disposed && Player.IsMuted;
    public double Volume => _disposed ? 1.0 : Player.Volume;
    public bool HasMedia => !_disposed && _mediaReady;

    /// <summary>Natural video size; 16:9 until the real size is known.</summary>
    public (double Width, double Height) VideoSize { get; private set; } = (16, 9);

    /// <summary>Stable identity of the line currently playing; changes only when it changes.</summary>
    public Guid? ActiveId { get; private set; }

    public event Action? ActiveChanged;
    public event Action? VideoSizeChanged;
    public event Action? Ticked;
    public event Action<string>? Failed;
    public event Action? PlayEmptyRequested;

    public void Load(string? path, bool autoPlay = false)
    {
        var generation = Interlocked.Increment(ref _loadGeneration);
        _currentPath = path;
        _mediaReady = false;
        _timeline.Reset();
        _seekWhenReady = null;
        _autoPlayWhenReady = path is not null && autoPlay;
        IsPlaying = _autoPlayWhenReady;
        Ticked?.Invoke();
        // Don't use the old video's aspect ratio until the new size arrives.
        VideoSize = (16, 9);
        VideoSizeChanged?.Invoke();
        UpdateActive();

        // Run Media Foundation source swap and RAM read-ahead stream creation off the UI thread
        // so switching videos in the left sidebar responds instantaneously.
        _ = Task.Run(() => SwitchSourceAsync(path, generation));
    }

    private async Task SwitchSourceAsync(string? path, int generation)
    {
        await _sourceGate.WaitAsync().ConfigureAwait(false);
        MediaSource? oldSource = null;
        IRandomAccessStream? oldRandomStream = null;
        BufferedVideoStream? oldBufferedStream = null;
        try
        {
            if (_disposed) return;

            oldSource = _source;
            oldRandomStream = _randomAccessStream;
            oldBufferedStream = _bufferedStream;
            _source = null;
            _randomAccessStream = null;
            _bufferedStream = null;

            if (path is null || _disposed || generation != Volatile.Read(ref _loadGeneration))
            {
                try
                {
                    Player.Pause();
                    Player.Source = null;
                }
                catch
                {
                }
                return;
            }

            MediaSource? newSource = null;
            IRandomAccessStream? newRandomStream = null;
            BufferedVideoStream? newBufferedStream = null;

            try
            {
                if (File.Exists(path))
                {
                    newBufferedStream = new BufferedVideoStream(path);
                    newRandomStream = newBufferedStream.AsRandomAccessStream();
                    newSource = MediaSource.CreateFromStream(newRandomStream, BufferedVideoStream.GetMimeType(path));
                }
                else
                {
                    newSource = MediaSource.CreateFromUri(new Uri(path));
                }
            }
            catch
            {
                try { newRandomStream?.Dispose(); } catch { }
                try { newBufferedStream?.Dispose(); } catch { }
                newRandomStream = null;
                newBufferedStream = null;
                newSource = MediaSource.CreateFromUri(new Uri(path));
            }

            if (_disposed || generation != Volatile.Read(ref _loadGeneration))
            {
                try { newSource?.Dispose(); } catch { }
                try { newRandomStream?.Dispose(); } catch { }
                try { newBufferedStream?.Dispose(); } catch { }
                return;
            }

            _bufferedStream = newBufferedStream;
            _randomAccessStream = newRandomStream;
            _source = newSource;
            _queue.TryEnqueue(() =>
            {
                if (_disposed || generation != Volatile.Read(ref _loadGeneration)) return;
                try
                {
                    Player.Source = newSource;
                    if (_autoPlayWhenReady)
                    {
                        Player.Play();
                        IsPlaying = true;
                        Ticked?.Invoke();
                    }
                }
                catch
                {
                }
            });
        }
        catch (Exception error)
        {
            if (!_disposed && generation == Volatile.Read(ref _loadGeneration))
            {
                var message = error.Message;
                _queue.TryEnqueue(() => Failed?.Invoke(message));
            }
        }
        finally
        {
            _sourceGate.Release();
            if (oldSource is not null || oldRandomStream is not null || oldBufferedStream is not null)
            {
                _ = Task.Run(() =>
                {
                    try { oldSource?.Dispose(); } catch { }
                    try { oldRandomStream?.Dispose(); } catch { }
                    try { oldBufferedStream?.Dispose(); } catch { }
                });
            }
        }
    }

    public void SetSentences(IReadOnlyList<Sentence> sentences)
    {
        _sentences = sentences;
        UpdateActive();
    }

    public void Seek(double seconds)
    {
        if (_disposed) return;
        if (HasMedia)
            _timeline.Observe(CurrentTime, Player.PlaybackSession.NaturalDuration.TotalSeconds, DateTimeOffset.UtcNow);
        var target = _timeline.Seek(seconds, DateTimeOffset.UtcNow);
        if (HasMedia) Player.PlaybackSession.Position = TimeSpan.FromSeconds(target);
        else _seekWhenReady = target;
        UpdateActive();
        Ticked?.Invoke();
    }

    public void TogglePlayback()
    {
        if (_disposed) return;
        if (!HasMedia)
        {
            if (_currentPath is not null)
            {
                _autoPlayWhenReady = !_autoPlayWhenReady;
                IsPlaying = _autoPlayWhenReady;
                if (_autoPlayWhenReady)
                {
                    try { Player.Play(); } catch { }
                }
                else
                {
                    try { Player.Pause(); } catch { }
                }
                Ticked?.Invoke();
            }
            else
            {
                PlayEmptyRequested?.Invoke();
            }
            return;
        }
        var session = Player.PlaybackSession;
        if (session.PlaybackState != MediaPlaybackState.Playing)
        {
            var duration = session.NaturalDuration.TotalSeconds;
            if (duration > 0 && CurrentTime >= duration - 0.05) Seek(0);
            Player.Play();
            IsPlaying = true;
        }
        else
        {
            Player.Pause();
            IsPlaying = false;
        }
        Ticked?.Invoke();
    }

    public void ToggleMute()
    {
        if (_disposed) return;
        Player.IsMuted = !Player.IsMuted;
        Ticked?.Invoke();
    }

    public void SetVolume(double volume)
    {
        if (_disposed) return;
        var clamped = Math.Clamp(volume, 0.0, 1.0);
        Player.Volume = clamped;
        if (clamped > 0 && Player.IsMuted) Player.IsMuted = false;
        Ticked?.Invoke();
    }

    private void Tick()
    {
        if (_disposed || !HasMedia) return;
        var session = Player.PlaybackSession;
        var playing = session.PlaybackState == MediaPlaybackState.Playing;
        // Also follows scrubbing with the transport controls while paused.
        var seconds = session.Position.TotalSeconds;
        _timeline.Observe(seconds, session.NaturalDuration.TotalSeconds, DateTimeOffset.UtcNow);
        IsPlaying = playing;
        UpdateActive();
        Ticked?.Invoke();
    }

    private void UpdateVideoSize()
    {
        if (_disposed) return;
        var session = Player.PlaybackSession;
        if (session.NaturalVideoWidth > 1 && session.NaturalVideoHeight > 1)
        {
            VideoSize = (session.NaturalVideoWidth, session.NaturalVideoHeight);
            VideoSizeChanged?.Invoke();
        }
    }

    private void UpdateActive()
    {
        Guid? id = _sentences.ActiveIndex(CurrentTime) is { } index ? _sentences[index].Uid : null;
        if (id == ActiveId) return;
        ActiveId = id;
        ActiveChanged?.Invoke();
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _mediaReady = false;
        Interlocked.Increment(ref _loadGeneration);
        _timer.Stop();
        var player = Player;
        // Media Foundation pipeline/decoder teardown can block for up to a second on the UI thread;
        // dispose the agile WinRT MediaPlayer, MediaSource, and RAM cache stream on the thread pool after detaching from XAML.
        _ = Task.Run(async () =>
        {
            await _sourceGate.WaitAsync().ConfigureAwait(false);
            try
            {
                var source = _source;
                var randomStream = _randomAccessStream;
                var bufferedStream = _bufferedStream;
                _source = null;
                _randomAccessStream = null;
                _bufferedStream = null;
                player.Pause();
                player.Source = null;
                source?.Dispose();
                randomStream?.Dispose();
                bufferedStream?.Dispose();
                player.Dispose();
            }
            catch
            {
                // Ignore teardown errors during window shutdown.
            }
            finally
            {
                _sourceGate.Release();
            }
        });
    }
}
