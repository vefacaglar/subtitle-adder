using Microsoft.UI.Dispatching;
using SubtitleAdder.EngineKit;
using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.Storage;

namespace SubtitleAdder;

/// <summary>
/// Preview player (port of PlayerModel): wraps <see cref="MediaPlayer"/>, tracks the ~10 Hz playback time and the
/// line currently playing. MediaPlayer events arrive on worker threads; everything here is marshalled to the UI thread.
/// </summary>
public sealed class PlayerController : IDisposable
{
    private readonly DispatcherQueue _queue;
    private readonly DispatcherQueueTimer _timer;
    private IReadOnlyList<Sentence> _sentences = [];
    private int _loadGeneration;
    private MediaSource? _source;   // disposed when replaced so the old file is not kept open

    public PlayerController(DispatcherQueue queue)
    {
        _queue = queue;
        Player.AutoPlay = false;
        Player.MediaOpened += (_, _) => _queue.TryEnqueue(UpdateVideoSize);
        Player.MediaFailed += (_, e) => _queue.TryEnqueue(() => Failed?.Invoke(e.ErrorMessage));
        Player.PlaybackSession.NaturalVideoSizeChanged += (_, _) => _queue.TryEnqueue(UpdateVideoSize);
        _timer = queue.CreateTimer();
        _timer.Interval = TimeSpan.FromMilliseconds(100);
        _timer.Tick += (_, _) => Tick();
        _timer.Start();
    }

    public MediaPlayer Player { get; } = new();

    public double CurrentTime { get; private set; }
    public bool IsPlaying { get; private set; }
    public bool HasMedia => Player.Source is not null;

    /// <summary>Natural video size; 16:9 until the real size is known.</summary>
    public (double Width, double Height) VideoSize { get; private set; } = (16, 9);

    /// <summary>Stable identity of the line currently playing; changes only when it changes.</summary>
    public Guid? ActiveId { get; private set; }

    public event Action? ActiveChanged;
    public event Action? VideoSizeChanged;
    public event Action? Ticked;
    public event Action<string>? Failed;

    public async void Load(string? path)
    {
        var generation = ++_loadGeneration;
        Player.Pause();
        IsPlaying = false;
        CurrentTime = 0;
        // Don't use the old video's aspect ratio until the new size arrives.
        VideoSize = (16, 9);
        VideoSizeChanged?.Invoke();
        UpdateActive();
        ReleaseSource();
        if (path is null) return;
        MediaSource source;
        try
        {
            source = MediaSource.CreateFromStorageFile(await StorageFile.GetFileFromPathAsync(path));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or ArgumentException or System.Runtime.InteropServices.COMException)
        {
            source = MediaSource.CreateFromUri(new Uri(path));
        }
        if (generation != _loadGeneration)
        {
            source.Dispose();
            return;
        }
        _source = source;
        Player.Source = source;
    }

    private void ReleaseSource()
    {
        Player.Source = null;
        _source?.Dispose();
        _source = null;
    }

    public void SetSentences(IReadOnlyList<Sentence> sentences)
    {
        _sentences = sentences;
        UpdateActive();
    }

    public void Seek(double seconds)
    {
        var target = Math.Max(0, seconds);
        CurrentTime = target;
        UpdateActive();
        if (HasMedia) Player.PlaybackSession.Position = TimeSpan.FromSeconds(target);
    }

    public void TogglePlayback()
    {
        if (!HasMedia) return;
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
    }

    private void Tick()
    {
        if (!HasMedia) return;
        var session = Player.PlaybackSession;
        var playing = session.PlaybackState == MediaPlaybackState.Playing;
        // Also follows scrubbing with the transport controls while paused.
        var seconds = session.Position.TotalSeconds;
        if (double.IsFinite(seconds)) CurrentTime = seconds;
        IsPlaying = playing;
        UpdateActive();
        Ticked?.Invoke();
    }

    private void UpdateVideoSize()
    {
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
        _timer.Stop();
        ReleaseSource();
        Player.Dispose();
    }
}
