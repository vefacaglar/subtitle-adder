namespace SubtitleAdder.EngineKit;

/// <summary>
/// Process-wide strict offline switch, passed to the engine as <c>SUBTITLE_ENGINE_OFFLINE</c> on every launch (like the
/// UI language). When on, the engine never uses the network, not even to download a missing model. On by default.
/// </summary>
public sealed class OfflineState
{
    public static OfflineState Shared { get; } = new();

    private volatile bool _enabled = true;

    public bool Enabled
    {
        get => _enabled;
        set => _enabled = value;
    }
}
