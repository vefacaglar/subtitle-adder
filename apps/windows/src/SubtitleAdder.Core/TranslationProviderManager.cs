using System.Diagnostics;
using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using CommunityToolkit.Mvvm.ComponentModel;
using SubtitleAdder.EngineKit;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

/// <summary>
/// Built-in configuration template for an OpenAI-compatible translation API provider or local LAN server.
/// </summary>
public sealed record TranslationProviderPreset(
    string Id,
    string RawName,
    string BaseUrl,
    bool RequiresApiKey,
    IReadOnlyList<string> SuggestedModels,
    bool LocalizeName = false)
{
    public string Name => LocalizeName ? L(RawName) : RawName;
}

/// <summary>
/// A saved translation provider configuration profile (cloud API or LAN server).
/// </summary>
public sealed record TranslationProviderProfile
{
    public Guid Id { get; init; } = Guid.NewGuid();
    public string Name { get; init; } = "";
    public string PresetId { get; init; } = "custom";
    public string BaseUrl { get; init; } = "";
    public string ApiKey { get; init; } = "";
    public string Model { get; init; } = "";
    public bool AsyncTranslation { get; init; } = true;
    public IReadOnlyList<string> DiscoveredModels { get; init; } = [];

    [JsonIgnore]
    public string PresetName => TranslationProviderManager.GetPreset(PresetId).Name;

    [JsonIgnore]
    public string MaskedApiKey
    {
        get
        {
            var trimmed = (ApiKey ?? "").Trim();
            if (trimmed.Length == 0) return L("Anahtar yok");
            if (trimmed.Length <= 4) return "••••";
            if (trimmed.Length <= 8) return $"••••{trimmed[^4..]}";
            var prefixLen = trimmed.StartsWith("sk-", StringComparison.OrdinalIgnoreCase) ? 3 : 3;
            return $"{trimmed[..prefixLen]}••••{trimmed[^4..]}";
        }
    }

    [JsonIgnore]
    public IReadOnlyList<string> AvailableModels
    {
        get
        {
            var result = new List<string>();
            var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (var m in DiscoveredModels)
            {
                if (!string.IsNullOrWhiteSpace(m) && seen.Add(m.Trim()))
                    result.Add(m.Trim());
            }
            if (DiscoveredModels.Count == 0 || !string.Equals(PresetId, "lan", StringComparison.OrdinalIgnoreCase))
            {
                foreach (var m in TranslationProviderManager.GetPreset(PresetId).SuggestedModels)
                {
                    if (!string.IsNullOrWhiteSpace(m) && seen.Add(m.Trim()))
                        result.Add(m.Trim());
                }
            }
            if (!string.IsNullOrWhiteSpace(Model) && seen.Add(Model.Trim()))
            {
                result.Insert(0, Model.Trim());
            }
            return result;
        }
    }
}

/// <summary>
/// Manages multiple OpenAI-compatible API translation providers (OpenCode Go, OpenRouter, OpenAI, DeepSeek,
/// Groq, Google Gemini, LAN Ollama/LM Studio, Custom), persists them to <c>translation-providers.json</c>,
/// and synchronizes the active provider with <see cref="ExternalLlmState.Shared"/>.
/// </summary>
public sealed partial class TranslationProviderManager : ObservableObject
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = true,
    };

    public static IReadOnlyList<TranslationProviderPreset> Presets { get; } =
    [
        new(
            Id: "opencode-go",
            RawName: "OpenCode Go",
            BaseUrl: "https://opencode.ai/zen/go/v1",
            RequiresApiKey: true,
            SuggestedModels: ["glm-5", "kimi-k2.5", "minimax-m2.5", "deepseek-v3", "qwen3-235b"]),
        new(
            Id: "openrouter",
            RawName: "OpenRouter",
            BaseUrl: "https://openrouter.ai/api/v1",
            RequiresApiKey: true,
            SuggestedModels: ["google/gemma-3-27b-it", "deepseek/deepseek-chat-v3-0324", "qwen/qwen-2.5-72b-instruct", "openai/gpt-4o-mini"]),
        new(
            Id: "openai",
            RawName: "OpenAI",
            BaseUrl: "https://api.openai.com/v1",
            RequiresApiKey: true,
            SuggestedModels: ["gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano"]),
        new(
            Id: "deepseek",
            RawName: "DeepSeek",
            BaseUrl: "https://api.deepseek.com/v1",
            RequiresApiKey: true,
            SuggestedModels: ["deepseek-chat"]),
        new(
            Id: "groq",
            RawName: "Groq",
            BaseUrl: "https://api.groq.com/openai/v1",
            RequiresApiKey: true,
            SuggestedModels: ["llama-3.3-70b-versatile", "gemma2-9b-it"]),
        new(
            Id: "gemini",
            RawName: "Google Gemini",
            BaseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
            RequiresApiKey: true,
            SuggestedModels: ["gemini-2.5-flash", "gemini-2.5-pro"]),
        new(
            Id: "lan",
            RawName: K("Yerel Ağ (Ollama / LM Studio)"),
            BaseUrl: "http://127.0.0.1:11434",
            RequiresApiKey: false,
            SuggestedModels: ["gemma4:e4b", "qwen3.5:4b"],
            LocalizeName: true),
        new(
            Id: "custom",
            RawName: K("Özel (OpenAI Uyumlu)"),
            BaseUrl: "",
            RequiresApiKey: true,
            SuggestedModels: [],
            LocalizeName: true),
    ];

    public static TranslationProviderPreset GetPreset(string? presetId) =>
        Presets.FirstOrDefault(p => string.Equals(p.Id, presetId, StringComparison.OrdinalIgnoreCase))
        ?? Presets[^1];

    public static string DefaultFilePath { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "SubtitleAdder",
        "translation-providers.json");

    public static TranslationProviderManager Shared { get; } = new();

    private readonly Lock _lock = new();
    private readonly HttpClient _http;
    private List<TranslationProviderProfile> _providers = [];
    private Guid? _activeProviderId;
    private bool _isTesting;

    public TranslationProviderManager(string? filePath = null, HttpMessageHandler? httpHandler = null)
    {
        FilePath = filePath ?? DefaultFilePath;
        _http = httpHandler is not null
            ? new HttpClient(httpHandler) { Timeout = TimeSpan.FromSeconds(8) }
            : new HttpClient { Timeout = TimeSpan.FromSeconds(8) };
        LoadFromDisk();
        SyncToEngineState();
    }

    public string FilePath { get; }

    public event EventHandler? Changed;

    public IReadOnlyList<TranslationProviderProfile> Providers
    {
        get
        {
            lock (_lock) return [.. _providers];
        }
    }

    public Guid? ActiveProviderId
    {
        get
        {
            lock (_lock) return _activeProviderId;
        }
        set => SetActiveProvider(value);
    }

    public TranslationProviderProfile? ActiveProvider
    {
        get
        {
            lock (_lock)
            {
                return _activeProviderId is { } id
                    ? _providers.FirstOrDefault(p => p.Id == id)
                    : null;
            }
        }
    }

    public bool IsExternalProviderActive => ActiveProvider is not null;

    public bool IsTesting
    {
        get => _isTesting;
        private set => SetProperty(ref _isTesting, value);
    }

    public TranslationProviderProfile AddProvider(
        string presetId,
        string? name = null,
        string? baseUrl = null,
        string? apiKey = null,
        string? model = null,
        bool asyncTranslation = true,
        bool setActive = true)
    {
        var preset = GetPreset(presetId);
        var resolvedName = !string.IsNullOrWhiteSpace(name) ? name.Trim() : preset.Name;
        var resolvedUrl = !string.IsNullOrWhiteSpace(baseUrl) ? baseUrl.Trim() : preset.BaseUrl;
        var resolvedKey = (apiKey ?? "").Trim();
        var resolvedModel = !string.IsNullOrWhiteSpace(model)
            ? model.Trim()
            : (preset.SuggestedModels.FirstOrDefault() ?? "");

        var profile = new TranslationProviderProfile
        {
            Id = Guid.NewGuid(),
            Name = resolvedName,
            PresetId = preset.Id,
            BaseUrl = resolvedUrl,
            ApiKey = resolvedKey,
            Model = resolvedModel,
            AsyncTranslation = asyncTranslation,
            DiscoveredModels = [],
        };

        lock (_lock)
        {
            _providers.Add(profile);
            if (setActive)
            {
                _activeProviderId = profile.Id;
            }
            SaveToDiskLocked();
        }

        SyncToEngineState();
        NotifyStateChanged();
        return profile;
    }

    public void UpdateProvider(
        Guid id,
        string name,
        string baseUrl,
        string apiKey,
        string model,
        bool asyncTranslation)
    {
        var updated = false;
        lock (_lock)
        {
            var index = _providers.FindIndex(p => p.Id == id);
            if (index < 0) return;
            var existing = _providers[index];
            var preset = GetPreset(existing.PresetId);
            _providers[index] = existing with
            {
                Name = !string.IsNullOrWhiteSpace(name) ? name.Trim() : preset.Name,
                BaseUrl = (baseUrl ?? "").Trim(),
                ApiKey = (apiKey ?? "").Trim(),
                Model = (model ?? "").Trim(),
                AsyncTranslation = asyncTranslation,
            };
            SaveToDiskLocked();
            updated = true;
        }

        if (updated)
        {
            SyncToEngineState();
            NotifyStateChanged();
        }
    }

    public void UpdateProviderModel(Guid id, string model)
    {
        var updated = false;
        lock (_lock)
        {
            var index = _providers.FindIndex(p => p.Id == id);
            if (index < 0) return;
            var existing = _providers[index];
            var nextModel = (model ?? "").Trim();
            if (existing.Model == nextModel) return;
            _providers[index] = existing with { Model = nextModel };
            SaveToDiskLocked();
            updated = true;
        }

        if (updated)
        {
            SyncToEngineState();
            NotifyStateChanged();
        }
    }

    public void RemoveProvider(Guid id)
    {
        var removed = false;
        lock (_lock)
        {
            var count = _providers.RemoveAll(p => p.Id == id);
            if (count > 0)
            {
                if (_activeProviderId == id)
                {
                    _activeProviderId = null;
                }
                SaveToDiskLocked();
                removed = true;
            }
        }

        if (removed)
        {
            SyncToEngineState();
            NotifyStateChanged();
        }
    }

    public void SetActiveProvider(Guid? id)
    {
        var changed = false;
        lock (_lock)
        {
            Guid? resolved = null;
            if (id is { } targetId && _providers.Any(p => p.Id == targetId))
            {
                resolved = targetId;
            }
            if (_activeProviderId != resolved)
            {
                _activeProviderId = resolved;
                SaveToDiskLocked();
                changed = true;
            }
        }

        SyncToEngineState();
        if (changed)
        {
            NotifyStateChanged();
        }
    }

    /// <summary>
    /// Synchronizes <see cref="ActiveProvider"/> into <see cref="ExternalLlmState.Shared"/> (<c>Enabled</c>, <c>Url</c>,
    /// <c>Model</c>, and <c>ApiKey</c>).
    /// </summary>
    public void SyncToEngineState()
    {
        var active = ActiveProvider;
        if (active is not null && !string.IsNullOrWhiteSpace(active.BaseUrl))
        {
            ExternalLlmState.Shared.Enabled = true;
            ExternalLlmState.Shared.Url = active.BaseUrl.Trim();
            ExternalLlmState.Shared.Model = active.Model.Trim();
            ExternalLlmState.Shared.ApiKey = active.ApiKey.Trim();
        }
        else
        {
            ExternalLlmState.Shared.Enabled = false;
            ExternalLlmState.Shared.Url = "";
            ExternalLlmState.Shared.Model = "";
            ExternalLlmState.Shared.ApiKey = "";
        }
    }

    public void UpdateProviderDiscoveredModels(Guid id, IReadOnlyList<string> discoveredModels, string? selectedModel = null)
    {
        var updated = false;
        lock (_lock)
        {
            var idx = _providers.FindIndex(p => p.Id == id);
            if (idx < 0) return;
            var current = _providers[idx];
            var nextModel = !string.IsNullOrWhiteSpace(selectedModel)
                ? selectedModel.Trim()
                : (!string.IsNullOrWhiteSpace(current.Model) ? current.Model : (discoveredModels.FirstOrDefault() ?? ""));
            _providers[idx] = current with
            {
                DiscoveredModels = [.. discoveredModels],
                Model = nextModel,
            };
            SaveToDiskLocked();
            updated = true;
        }
        if (updated)
        {
            SyncToEngineState();
            NotifyStateChanged();
        }
    }

    /// <summary>
    /// Migrates or synchronizes existing LAN/Ollama settings from <paramref name="viewModel"/> into saved providers on startup
    /// so upgrading users never lose their active Ollama configuration or models.
    /// </summary>
    public void EnsureMigratedFromViewModel(SubtitleViewModel? viewModel)
    {
        if (viewModel is null) return;

        var vmUrl = (viewModel.ExternalLlmUrl ?? "").Trim();
        var vmModel = (viewModel.ExternalLlmModel ?? "").Trim();
        var vmAsync = viewModel.ExternalLlmAsync;
        var vmEnabled = viewModel.ExternalLlmEnabled;

        if (!string.IsNullOrWhiteSpace(vmUrl))
        {
            TranslationProviderProfile? lanProfile = null;
            var changed = false;
            lock (_lock)
            {
                lanProfile = _providers.FirstOrDefault(p =>
                    string.Equals(p.PresetId, "lan", StringComparison.OrdinalIgnoreCase) ||
                    string.Equals(p.BaseUrl.Trim().TrimEnd('/'), vmUrl.TrimEnd('/'), StringComparison.OrdinalIgnoreCase));

                if (lanProfile is null && (_providers.Count == 0 || vmEnabled))
                {
                    var preset = GetPreset("lan");
                    lanProfile = new TranslationProviderProfile
                    {
                        Id = Guid.NewGuid(),
                        Name = preset.Name,
                        PresetId = "lan",
                        BaseUrl = vmUrl,
                        ApiKey = "",
                        Model = vmModel,
                        AsyncTranslation = vmAsync,
                        DiscoveredModels = viewModel.ExternalLlmModels.Count > 0 ? [.. viewModel.ExternalLlmModels] : [],
                    };
                    _providers.Add(lanProfile);
                    if (vmEnabled && _activeProviderId is null)
                    {
                        _activeProviderId = lanProfile.Id;
                    }
                    SaveToDiskLocked();
                    changed = true;
                }
                else if (lanProfile is not null)
                {
                    var idx = _providers.FindIndex(p => p.Id == lanProfile.Id);
                    if (idx >= 0)
                    {
                        var updatedModel = !string.IsNullOrWhiteSpace(vmModel) ? vmModel : lanProfile.Model;
                        var updatedModels = viewModel.ExternalLlmModels.Count > 0
                            ? [.. viewModel.ExternalLlmModels]
                            : lanProfile.DiscoveredModels;
                        var nextProfile = lanProfile with
                        {
                            BaseUrl = vmUrl,
                            Model = updatedModel,
                            AsyncTranslation = vmAsync,
                            DiscoveredModels = updatedModels,
                        };
                        if (nextProfile != lanProfile)
                        {
                            _providers[idx] = nextProfile;
                            changed = true;
                        }
                        if (vmEnabled && _activeProviderId is null)
                        {
                            _activeProviderId = lanProfile.Id;
                            changed = true;
                        }
                        if (changed)
                        {
                            SaveToDiskLocked();
                        }
                    }
                }
            }

            if (changed)
            {
                SyncToEngineState();
                NotifyStateChanged();
            }
        }

        if (IsExternalProviderActive)
        {
            ApplyToViewModel(viewModel);
        }
    }

    /// <summary>
    /// Keeps the saved LAN/Ollama provider synchronized when the user edits the LAN LLM section in Settings.
    /// </summary>
    public void SyncFromLegacyLanSettings(SubtitleViewModel? viewModel)
    {
        if (viewModel is null) return;
        var vmUrl = (viewModel.ExternalLlmUrl ?? "").Trim();
        var vmModel = (viewModel.ExternalLlmModel ?? "").Trim();
        var vmAsync = viewModel.ExternalLlmAsync;
        var vmEnabled = viewModel.ExternalLlmEnabled;

        if (vmEnabled && !string.IsNullOrWhiteSpace(vmUrl))
        {
            Guid targetId;
            lock (_lock)
            {
                var existing = (_activeProviderId is { } activeId
                        ? _providers.FirstOrDefault(p => p.Id == activeId && string.Equals(p.PresetId, "lan", StringComparison.OrdinalIgnoreCase))
                        : null)
                    ?? _providers.FirstOrDefault(p =>
                        string.Equals(p.PresetId, "lan", StringComparison.OrdinalIgnoreCase) ||
                        string.Equals(p.BaseUrl.Trim().TrimEnd('/'), vmUrl.TrimEnd('/'), StringComparison.OrdinalIgnoreCase));

                if (existing is not null)
                {
                    var idx = _providers.FindIndex(p => p.Id == existing.Id);
                    var discovered = viewModel.ExternalLlmModels.Count > 0
                        ? [.. viewModel.ExternalLlmModels]
                        : existing.DiscoveredModels;
                    _providers[idx] = existing with
                    {
                        BaseUrl = vmUrl,
                        Model = !string.IsNullOrWhiteSpace(vmModel) ? vmModel : existing.Model,
                        AsyncTranslation = vmAsync,
                        DiscoveredModels = discovered,
                    };
                    _activeProviderId = existing.Id;
                    targetId = existing.Id;
                    SaveToDiskLocked();
                }
                else
                {
                    var preset = GetPreset("lan");
                    var created = new TranslationProviderProfile
                    {
                        Id = Guid.NewGuid(),
                        Name = preset.Name,
                        PresetId = "lan",
                        BaseUrl = vmUrl,
                        ApiKey = "",
                        Model = vmModel,
                        AsyncTranslation = vmAsync,
                        DiscoveredModels = viewModel.ExternalLlmModels.Count > 0 ? [.. viewModel.ExternalLlmModels] : [],
                    };
                    _providers.Add(created);
                    _activeProviderId = created.Id;
                    targetId = created.Id;
                    SaveToDiskLocked();
                }
            }
            SyncToEngineState();
            NotifyStateChanged();
            _ = targetId;
        }
        else if (!vmEnabled)
        {
            if (ActiveProvider is not null)
            {
                SetActiveProvider(null);
            }
        }
    }

    /// <summary>
    /// Synchronizes the active provider into both <see cref="ExternalLlmState.Shared"/> and <paramref name="viewModel"/>.
    /// </summary>
    public void ApplyToViewModel(SubtitleViewModel? viewModel)
    {
        SyncToEngineState();
        if (viewModel is null) return;
        var active = ActiveProvider;
        if (active is not null && !string.IsNullOrWhiteSpace(active.BaseUrl))
        {
            viewModel.ActiveProviderName = string.Equals(active.PresetId, "lan", StringComparison.OrdinalIgnoreCase)
                ? null
                : active.Name;
            viewModel.ExternalLlmUrl = active.BaseUrl.Trim();
            if (!string.IsNullOrWhiteSpace(active.Model))
            {
                viewModel.ExternalLlmModel = active.Model.Trim();
            }
            viewModel.ExternalLlmAsync = active.AsyncTranslation;
            viewModel.ExternalLlmEnabled = true;
            if (active.DiscoveredModels.Count > 0 &&
                !string.Equals(active.PresetId, "lan", StringComparison.OrdinalIgnoreCase))
            {
                viewModel.SetExternalLlmModels(active.DiscoveredModels);
            }
            ExternalLlmState.Shared.ApiKey = active.ApiKey.Trim();
        }
        else
        {
            viewModel.ActiveProviderName = null;
            viewModel.ExternalLlmEnabled = false;
            ExternalLlmState.Shared.ApiKey = "";
        }
    }

    /// <summary>
    /// Tests connectivity to a saved provider, fetches available models from <c>/models</c> (or falls back to a minimal
    /// <c>/chat/completions</c> probe for gateways like OpenCode Go that only expose chat completions), merges with
    /// the preset's suggested models, and updates <see cref="TranslationProviderProfile.DiscoveredModels"/>.
    /// </summary>
    public async Task<(bool Success, string Message, IReadOnlyList<string> Models)> TestAndFetchModelsAsync(
        Guid providerId,
        CancellationToken cancellationToken = default)
    {
        TranslationProviderProfile? profile;
        lock (_lock)
        {
            profile = _providers.FirstOrDefault(p => p.Id == providerId);
        }
        if (profile is null)
        {
            return (false, L("Sağlayıcı bulunamadı."), []);
        }

        var (success, message, models) = await TestAndFetchModelsAsync(
            profile.PresetId,
            profile.BaseUrl,
            profile.ApiKey,
            profile.Model,
            cancellationToken).ConfigureAwait(true);

        if (models.Count > 0)
        {
            var updated = false;
            lock (_lock)
            {
                var idx = _providers.FindIndex(p => p.Id == providerId);
                if (idx >= 0)
                {
                    var current = _providers[idx];
                    var nextModel = string.IsNullOrWhiteSpace(current.Model) ? models[0] : current.Model;
                    _providers[idx] = current with
                    {
                        DiscoveredModels = models,
                        Model = nextModel,
                    };
                    SaveToDiskLocked();
                    updated = true;
                }
            }
            if (updated)
            {
                SyncToEngineState();
                NotifyStateChanged();
            }
        }

        return (success, message, models);
    }

    /// <summary>
    /// Tests connectivity and fetches models for arbitrary provider credentials (used both for saved profiles and
    /// the "Add Provider" draft form before saving).
    /// </summary>
    public async Task<(bool Success, string Message, IReadOnlyList<string> Models)> TestAndFetchModelsAsync(
        string presetId,
        string baseUrl,
        string apiKey,
        string? model = null,
        CancellationToken cancellationToken = default)
    {
        var preset = GetPreset(presetId);
        var rawUrl = (baseUrl ?? "").Trim().TrimEnd('/');
        if (rawUrl.Length == 0)
        {
            return (false, L("Lütfen geçerli bir sunucu adresi girin."), preset.SuggestedModels);
        }
        if (!rawUrl.StartsWith("http://", StringComparison.OrdinalIgnoreCase) &&
            !rawUrl.StartsWith("https://", StringComparison.OrdinalIgnoreCase))
        {
            rawUrl = "http://" + rawUrl;
        }
        if (!Uri.TryCreate(rawUrl, UriKind.Absolute, out var uri) || string.IsNullOrEmpty(uri.Host))
        {
            return (false, L("Lütfen geçerli bir sunucu adresi girin."), preset.SuggestedModels);
        }

        IsTesting = true;
        try
        {
            var resolvedUrl = rawUrl;
            var hostLabel = uri.Host;
            if (uri.Host.EndsWith(".local", StringComparison.OrdinalIgnoreCase))
            {
                var port = uri.IsDefaultPort ? 11434 : uri.Port;
                string? resolvedIp = null;
                try
                {
                    var addresses = await System.Net.Dns.GetHostAddressesAsync(uri.Host, cancellationToken).ConfigureAwait(true);
                    resolvedIp = addresses.FirstOrDefault(a => a.AddressFamily == System.Net.Sockets.AddressFamily.InterNetwork)?.ToString();
                }
                catch (Exception) when (!cancellationToken.IsCancellationRequested)
                {
                }
                resolvedIp ??= await ExternalLlmState.Shared.DiscoverLanServerIpAsync(port, cancellationToken).ConfigureAwait(true);
                if (resolvedIp is not null)
                {
                    hostLabel = $"{uri.Host} → {resolvedIp}";
                    var builder = new UriBuilder(uri) { Host = resolvedIp };
                    resolvedUrl = builder.Uri.ToString().TrimEnd('/');
                }
            }

            var sw = Stopwatch.StartNew();
            var discovered = new List<string>();
            var reachable = false;
            int? authFailureCode = null;
            string? lastError = null;

            foreach (var endpoint in BuildCandidateModelEndpoints(resolvedUrl))
            {
                try
                {
                    using var req = CreateRequest(HttpMethod.Get, endpoint, apiKey);
                    using var resp = await _http.SendAsync(req, cancellationToken).ConfigureAwait(true);
                    var statusCode = (int)resp.StatusCode;
                    if (statusCode is 401 or 403)
                    {
                        authFailureCode = statusCode;
                        break;
                    }
                    if (resp.IsSuccessStatusCode)
                    {
                        var json = await resp.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(true);
                        reachable = true;
                        discovered = SubtitleViewModel.ParseSuitableExternalModels(json);
                        if (discovered.Count > 0) break;
                    }
                }
                catch (Exception ex) when (!cancellationToken.IsCancellationRequested)
                {
                    lastError = ex.Message;
                }
            }

            if (authFailureCode is { } code)
            {
                return (false, L("API anahtarı geçersiz veya yetkisiz (HTTP %lld).", (long)code), preset.SuggestedModels);
            }

            // If /models isn't exposed by the gateway (e.g. OpenCode Go or custom proxy), probe /chat/completions with max_tokens: 1.
            if (!reachable)
            {
                var chatEndpoint = BuildChatCompletionsEndpoint(resolvedUrl);
                var testModel = !string.IsNullOrWhiteSpace(model)
                    ? model.Trim()
                    : (preset.SuggestedModels.FirstOrDefault() ?? "gpt-4o-mini");
                var probePayload = JsonSerializer.Serialize(new
                {
                    model = testModel,
                    messages = new[] { new { role = "user", content = "hi" } },
                    max_tokens = 1,
                    stream = false,
                });

                try
                {
                    using var req = CreateRequest(HttpMethod.Post, chatEndpoint, apiKey);
                    req.Content = new StringContent(probePayload, Encoding.UTF8, "application/json");
                    using var resp = await _http.SendAsync(req, cancellationToken).ConfigureAwait(true);
                    var statusCode = (int)resp.StatusCode;
                    if (statusCode is 401 or 403)
                    {
                        return (false, L("API anahtarı geçersiz veya yetkisiz (HTTP %lld).", (long)statusCode), preset.SuggestedModels);
                    }
                    if (resp.IsSuccessStatusCode)
                    {
                        reachable = true;
                    }
                    else
                    {
                        lastError = $"HTTP {statusCode}";
                    }
                }
                catch (Exception ex) when (!cancellationToken.IsCancellationRequested)
                {
                    lastError = ex.Message;
                }
            }

            sw.Stop();
            var suggestedFallback = discovered.Count > 0 && string.Equals(preset.Id, "lan", StringComparison.OrdinalIgnoreCase)
                ? []
                : preset.SuggestedModels;
            var merged = MergeModels(discovered, suggestedFallback, model);
            if (!reachable)
            {
                return (false, L("Sunucuya ulaşılamadı: %@", lastError ?? "timeout"), merged);
            }

            var ms = Math.Max(1, sw.ElapsedMilliseconds);
            return (true, L("Bağlantı başarılı (%@ · %lld ms · %lld uygun model bulundu)",
                hostLabel, ms, (long)merged.Count), merged);
        }
        finally
        {
            IsTesting = false;
        }
    }

    private static HttpRequestMessage CreateRequest(HttpMethod method, string url, string? apiKey)
    {
        var req = new HttpRequestMessage(method, url);
        req.Headers.TryAddWithoutValidation("User-Agent", "SubtitleAdder/1.0");
        req.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("application/json"));
        var trimmedKey = (apiKey ?? "").Trim();
        if (trimmedKey.Length > 0)
        {
            req.Headers.Authorization = new AuthenticationHeaderValue("Bearer", trimmedKey);
        }
        return req;
    }

    internal static IReadOnlyList<string> BuildCandidateModelEndpoints(string resolvedUrl)
    {
        var clean = resolvedUrl.TrimEnd('/');
        var endpoints = new List<string>();

        void AddUnique(string candidate)
        {
            if (!endpoints.Contains(candidate, StringComparer.OrdinalIgnoreCase))
                endpoints.Add(candidate);
        }

        if (clean.EndsWith("/chat/completions", StringComparison.OrdinalIgnoreCase))
        {
            clean = clean[..^"/chat/completions".Length].TrimEnd('/');
        }
        else if (clean.EndsWith("/models", StringComparison.OrdinalIgnoreCase))
        {
            AddUnique(clean);
            return endpoints;
        }

        if (clean.EndsWith("/v1", StringComparison.OrdinalIgnoreCase) ||
            clean.EndsWith("/openai", StringComparison.OrdinalIgnoreCase))
        {
            AddUnique($"{clean}/models");
        }
        else
        {
            AddUnique($"{clean}/v1/models");
            AddUnique($"{clean}/models");
            AddUnique($"{clean}/api/tags");
        }

        return endpoints;
    }

    internal static string BuildChatCompletionsEndpoint(string resolvedUrl)
    {
        var clean = resolvedUrl.TrimEnd('/');
        if (clean.EndsWith("/chat/completions", StringComparison.OrdinalIgnoreCase) ||
            clean.EndsWith("/api/chat", StringComparison.OrdinalIgnoreCase))
        {
            return clean;
        }
        if (clean.EndsWith("/v1", StringComparison.OrdinalIgnoreCase) ||
            clean.EndsWith("/openai", StringComparison.OrdinalIgnoreCase))
        {
            return $"{clean}/chat/completions";
        }
        return $"{clean}/v1/chat/completions";
    }

    private static List<string> MergeModels(
        IEnumerable<string> discovered,
        IEnumerable<string> suggested,
        string? explicitModel)
    {
        var merged = new List<string>();
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        foreach (var item in discovered)
        {
            var trimmed = item?.Trim();
            if (!string.IsNullOrEmpty(trimmed) && seen.Add(trimmed))
                merged.Add(trimmed);
        }
        foreach (var item in suggested)
        {
            var trimmed = item?.Trim();
            if (!string.IsNullOrEmpty(trimmed) && seen.Add(trimmed))
                merged.Add(trimmed);
        }
        if (!string.IsNullOrWhiteSpace(explicitModel))
        {
            var trimmed = explicitModel.Trim();
            if (seen.Add(trimmed))
                merged.Insert(0, trimmed);
        }
        return merged;
    }

    private void NotifyStateChanged()
    {
        OnPropertyChanged(nameof(Providers));
        OnPropertyChanged(nameof(ActiveProviderId));
        OnPropertyChanged(nameof(ActiveProvider));
        OnPropertyChanged(nameof(IsExternalProviderActive));
        Changed?.Invoke(this, EventArgs.Empty);
    }

    private sealed class PersistedState
    {
        public Guid? ActiveProviderId { get; set; }
        public List<TranslationProviderProfile> Providers { get; set; } = [];
    }

    private void LoadFromDisk()
    {
        lock (_lock)
        {
            try
            {
                if (!File.Exists(FilePath)) return;
                var json = File.ReadAllText(FilePath, Encoding.UTF8);
                var state = JsonSerializer.Deserialize<PersistedState>(json, JsonOptions);
                if (state is null) return;
                _providers = state.Providers ?? [];
                _activeProviderId = state.ActiveProviderId is { } id && _providers.Any(p => p.Id == id)
                    ? id
                    : null;
            }
            catch (Exception ex) when (ex is IOException or JsonException or UnauthorizedAccessException)
            {
                _providers = [];
                _activeProviderId = null;
            }
        }
    }

    private void SaveToDiskLocked()
    {
        try
        {
            var dir = Path.GetDirectoryName(FilePath);
            if (!string.IsNullOrEmpty(dir))
            {
                Directory.CreateDirectory(dir);
            }
            var state = new PersistedState
            {
                ActiveProviderId = _activeProviderId,
                Providers = _providers,
            };
            var json = JsonSerializer.Serialize(state, JsonOptions);
            var temp = FilePath + ".tmp";
            File.WriteAllText(temp, json, Encoding.UTF8);
            File.Move(temp, FilePath, overwrite: true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // Keep state in memory for the current session if disk write fails.
        }
    }
}
