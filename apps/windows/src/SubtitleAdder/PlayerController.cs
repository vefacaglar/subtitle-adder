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
    private readonly PlayerSessionGate _sessionGate = new();
    private IReadOnlyList<Sentence> _sentences = [];
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
        Player.MediaOpened += (_, _) =>
        {
            var eventToken = _sessionGate.CaptureEventToken();
            _queue.TryEnqueue(() =>
            {
                var isSourceOpened = _source?.State == MediaSourceState.Opened || _sessionGate.HasActiveSourceOpened;
                if (_disposed || !_sessionGate.ShouldAcceptMediaOpened(eventToken, Player.Source, isSourceOpened))
                {
                    return;
                }
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
        };
        Player.MediaFailed += (sender, e) =>
        {
            var eventToken = _sessionGate.CaptureEventToken();
            var err = e.Error;
            var errMsg = e.ErrorMessage;
            var extErr = e.ExtendedErrorCode;
            _queue.TryEnqueue(() =>
            {
                var isSourceFailed = _source?.State == MediaSourceState.Failed || _sessionGate.HasActiveSourceFailed;
                if (_disposed || _currentPath is null || !_sessionGate.ShouldAcceptMediaFailed(eventToken, Player.Source, isSourceFailed))
                {
                    return;
                }
                _mediaReady = false;
                IsPlaying = false;
                Ticked?.Invoke();
                if (err == MediaPlayerError.Aborted) return;
                var details = !string.IsNullOrWhiteSpace(errMsg)
                    ? errMsg.Trim()
                    : extErr is { } ex && !string.IsNullOrWhiteSpace(ex.Message)
                        ? $"{err}: {ex.Message.Trim()} (0x{ex.HResult:X8})"
                        : $"{err} (0x{extErr?.HResult ?? 0:X8})";
                var failedPath = _currentPath;
                DiagnosticLog.Write("Player", $"MediaFailed gen={eventToken.Generation} path='{failedPath}': {details}");
                _ = Task.Run(() =>
                {
                    try
                    {
                        File.AppendAllText(
                            Path.Combine(Path.GetTempPath(), "SubtitleAdder-player.log"),
                            $"{DateTimeOffset.Now:O} [{failedPath}] {details}\n");
                    }
                    catch
                    {
                    }
                });
                Failed?.Invoke(details);
            });
        };
        Player.PlaybackSession.NaturalVideoSizeChanged += (_, _) =>
        {
            var eventToken = _sessionGate.CaptureEventToken();
            _queue.TryEnqueue(() =>
            {
                if (_disposed || !_sessionGate.ShouldAcceptEvent(eventToken, Player.Source))
                {
                    return;
                }
                UpdateVideoSize();
            });
        };
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
        var generation = _sessionGate.BeginLoad();
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
        try
        {
            static void DisposeMediaTuple(MediaSource? src, IRandomAccessStream? ras, BufferedVideoStream? buf)
            {
                if (src is null && ras is null && buf is null) return;
                _ = Task.Run(() =>
                {
                    try { src?.Dispose(); } catch { }
                    try { ras?.Dispose(); } catch { }
                    try { buf?.Dispose(); } catch { }
                });
            }

            // Never touch _source, _randomAccessStream, or _bufferedStream if this request is already stale or disposed.
            if (_disposed || !_sessionGate.IsCurrentLoad(generation))
            {
                return;
            }

            MediaSource? newSource = null;
            IRandomAccessStream? newRandomStream = null;
            BufferedVideoStream? newBufferedStream = null;

            if (path is not null)
            {
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

                if (_disposed || !_sessionGate.IsCurrentLoad(generation))
                {
                    DisposeMediaTuple(newSource, newRandomStream, newBufferedStream);
                    return;
                }

                var boundSource = newSource;
                var boundGen = generation;
                var boundPath = path;
                var boundToken = _sessionGate.CreateBoundToken(boundGen, boundSource);
                boundSource.StateChanged += (_, args) =>
                {
                    if (args.NewState == MediaSourceState.Opened)
                    {
                        _sessionGate.NotifySourceStateChanged(boundToken, isOpened: true, isFailed: false);
                    }
                    else if (args.NewState == MediaSourceState.Failed)
                    {
                        _sessionGate.NotifySourceStateChanged(boundToken, isOpened: false, isFailed: true);
                    }
                };
                boundSource.OpenOperationCompleted += (_, args) =>
                {
                    var openErr = args.Error?.ExtendedError;
                    if (openErr is null)
                    {
                        _sessionGate.NotifySourceOpenCompleted(boundToken, succeeded: true);
                        return;
                    }
                    _sessionGate.NotifySourceOpenCompleted(boundToken, succeeded: false, openErr.Message);
                    _queue.TryEnqueue(() =>
                    {
                        var isSourceFailed = _source?.State == MediaSourceState.Failed || _sessionGate.HasActiveSourceFailed;
                        if (_disposed || !_sessionGate.ShouldAcceptMediaFailed(boundToken, Player.Source, isSourceFailed))
                        {
                            return;
                        }
                        DiagnosticLog.Write("Player", $"OpenOperationCompleted error gen={boundGen} path='{boundPath}': {openErr.Message} (0x{openErr.HResult:X8})");
                    });
                };
            }

            MediaSource? oldSourceToDispose = null;
            IRandomAccessStream? oldRandomToDispose = null;
            BufferedVideoStream? oldBufferedToDispose = null;
            var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);

            if (!_queue.TryEnqueue(() =>
                {
                    if (_disposed || !_sessionGate.IsCurrentLoad(generation))
                    {
                        tcs.TrySetResult(false);
                        return;
                    }

                    oldSourceToDispose = _source;
                    oldRandomToDispose = _randomAccessStream;
                    oldBufferedToDispose = _bufferedStream;
                    _source = newSource;
                    _randomAccessStream = newRandomStream;
                    _bufferedStream = newBufferedStream;

                    try
                    {
                        _sessionGate.ClearActiveSource();
                        Player.Pause();
                        Player.Source = null;

                        if (newSource is not null && _sessionGate.TryActivateSource(generation, newSource))
                        {
                            Player.Source = newSource;
                            if (_autoPlayWhenReady)
                            {
                                Player.Play();
                                IsPlaying = true;
                                Ticked?.Invoke();
                            }
                        }
                    }
                    catch
                    {
                    }
                    finally
                    {
                        tcs.TrySetResult(true);
                    }
                }))
            {
                DisposeMediaTuple(newSource, newRandomStream, newBufferedStream);
                return;
            }

            var applied = await tcs.Task.ConfigureAwait(false);
            if (!applied)
            {
                DisposeMediaTuple(newSource, newRandomStream, newBufferedStream);
                return;
            }

            DisposeMediaTuple(oldSourceToDispose, oldRandomToDispose, oldBufferedToDispose);
        }
        catch (Exception error)
        {
            if (!_disposed && _sessionGate.IsCurrentLoad(generation) && !string.IsNullOrWhiteSpace(error.Message))
            {
                var message = error.Message;
                _queue.TryEnqueue(() => Failed?.Invoke(message));
            }
        }
        finally
        {
            _sourceGate.Release();
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
        _sessionGate.BeginLoad();
        _sessionGate.ClearActiveSource();
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
