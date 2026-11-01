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
