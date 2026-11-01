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

/// <summary>
/// Process-wide external LAN LLM server settings passed to the engine as <c>SUBTITLE_ENGINE_EXTERNAL_LLM_URL</c> and
/// <c>SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL</c> on each launch when enabled.
/// </summary>
public sealed class ExternalLlmState
{
    public static ExternalLlmState Shared { get; } = new();

    private volatile bool _enabled;
    private volatile string _url = "";
    private volatile string _model = "";

    public bool Enabled
    {
        get => _enabled;
        set => _enabled = value;
    }

    public string Url
    {
        get => _url;
        set => _url = value ?? "";
    }

    public string Model
    {
        get => _model;
        set => _model = value ?? "";
    }

    public string? ActiveUrl => _enabled && !string.IsNullOrWhiteSpace(_url) ? _url.Trim() : null;

    public string? ActiveModel => _enabled && !string.IsNullOrWhiteSpace(_model) ? _model.Trim() : null;

    /// <summary>
    /// Resolves <c>.local</c> mDNS hostnames (e.g. <c>mac-studio.local</c>) dynamically at launch time to their current
    /// IPv4 address while keeping the saved <see cref="Url"/> as the hostname so DHCP IP changes never break the connection.
    /// </summary>
    public string? ResolveActiveUrl()
    {
        if (ActiveUrl is not { } raw) return null;
        var candidate = raw.StartsWith("http://", StringComparison.OrdinalIgnoreCase) ||
                        raw.StartsWith("https://", StringComparison.OrdinalIgnoreCase)
            ? raw
            : "http://" + raw;
        if (Uri.TryCreate(candidate, UriKind.Absolute, out var uri) &&
            uri.Host.EndsWith(".local", StringComparison.OrdinalIgnoreCase))
        {
            try
            {
                var addresses = System.Net.Dns.GetHostAddresses(uri.Host);
                var ipv4 = addresses.FirstOrDefault(a => a.AddressFamily == System.Net.Sockets.AddressFamily.InterNetwork);
                if (ipv4 is not null)
                {
                    var builder = new UriBuilder(uri) { Host = ipv4.ToString() };
                    return builder.Uri.ToString().TrimEnd('/');
                }
            }
            catch (Exception)
            {
            }
        }
        return raw;
    }
}
