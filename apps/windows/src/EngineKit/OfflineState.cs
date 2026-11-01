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
    private volatile string? _cachedResolvedHostIp;

    public bool Enabled
    {
        get => _enabled;
        set => _enabled = value;
    }

    public string Url
    {
        get => _url;
        set
        {
            var next = value ?? "";
            if (_url != next) _cachedResolvedHostIp = null;
            _url = next;
        }
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
    /// IPv4 address (falling back to fast LAN subnet port discovery when mesh routers like TP-Link Deco block mDNS) while
    /// keeping the saved <see cref="Url"/> as the hostname so DHCP IP changes never break the connection.
    /// </summary>
    public string? ResolveActiveUrl()
    {
        if (ActiveUrl is not { } raw) return null;
        var candidate = raw.StartsWith("http://", StringComparison.OrdinalIgnoreCase) ||
                        raw.StartsWith("https://", StringComparison.OrdinalIgnoreCase)
            ? raw
            : "http://" + raw;
        if (!Uri.TryCreate(candidate, UriKind.Absolute, out var uri)) return raw;
        var port = uri.IsDefaultPort ? 11434 : uri.Port;
        if (uri.Host.EndsWith(".local", StringComparison.OrdinalIgnoreCase))
        {
            try
            {
                var addresses = System.Net.Dns.GetHostAddresses(uri.Host);
                var ipv4 = addresses.FirstOrDefault(a => a.AddressFamily == System.Net.Sockets.AddressFamily.InterNetwork);
                if (ipv4 is not null)
                {
                    _cachedResolvedHostIp = ipv4.ToString();
                    var builder = new UriBuilder(uri) { Host = _cachedResolvedHostIp };
                    return builder.Uri.ToString().TrimEnd('/');
                }
            }
            catch (Exception)
            {
            }

            if (_cachedResolvedHostIp is { Length: > 0 } cached && CanConnectTcp(cached, port, 250))
            {
                var builder = new UriBuilder(uri) { Host = cached };
                return builder.Uri.ToString().TrimEnd('/');
            }

            if (DiscoverLanServerIpAsync(port).GetAwaiter().GetResult() is { } discovered)
            {
                _cachedResolvedHostIp = discovered;
                var builder = new UriBuilder(uri) { Host = discovered };
                return builder.Uri.ToString().TrimEnd('/');
            }
        }
        return raw;
    }

    public async Task<string?> DiscoverLanServerIpAsync(int port, CancellationToken cancellationToken = default)
    {
        var localIp = GetLocalLanIpv4();
        if (localIp is null) return null;
        var lastDot = localIp.LastIndexOf('.');
        if (lastDot <= 0) return null;
        var prefix = localIp[..(lastDot + 1)];

        if (_cachedResolvedHostIp is { Length: > 0 } cached && CanConnectTcp(cached, port, 250))
        {
            return cached;
        }

        using var cts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        cts.CancelAfter(TimeSpan.FromMilliseconds(900));
        var token = cts.Token;

        var tasks = new List<Task<string?>>(254);
        for (var i = 1; i <= 254; i++)
        {
            var ip = prefix + i.ToString(System.Globalization.CultureInfo.InvariantCulture);
            if (ip == localIp) continue;
            tasks.Add(ProbeTcpAsync(ip, port, token));
        }

        while (tasks.Count > 0 && !token.IsCancellationRequested)
        {
            var finished = await Task.WhenAny(tasks).ConfigureAwait(false);
            tasks.Remove(finished);
            var winner = await finished.ConfigureAwait(false);
            if (winner is not null)
            {
                _cachedResolvedHostIp = winner;
                await cts.CancelAsync().ConfigureAwait(false);
                return winner;
            }
        }
        return null;
    }

    private static async Task<string?> ProbeTcpAsync(string ip, int port, CancellationToken token)
    {
        try
        {
            using var client = new System.Net.Sockets.TcpClient();
            await client.ConnectAsync(ip, port, token).ConfigureAwait(false);
            return ip;
        }
        catch (Exception)
        {
            return null;
        }
    }

    private static bool CanConnectTcp(string ip, int port, int timeoutMs)
    {
        try
        {
            using var client = new System.Net.Sockets.TcpClient();
            var task = client.ConnectAsync(ip, port);
            return task.Wait(timeoutMs) && client.Connected;
        }
        catch (Exception)
        {
            return false;
        }
    }

    private static string? GetLocalLanIpv4()
    {
        try
        {
            using var socket = new System.Net.Sockets.Socket(
                System.Net.Sockets.AddressFamily.InterNetwork,
                System.Net.Sockets.SocketType.Dgram,
                System.Net.Sockets.ProtocolType.Udp);
            socket.Connect("8.8.8.8", 80);
            return (socket.LocalEndPoint as System.Net.IPEndPoint)?.Address.ToString();
        }
        catch (Exception)
        {
            return null;
        }
    }
}
