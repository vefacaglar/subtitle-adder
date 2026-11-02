using System.Net;
using System.Text;
using SubtitleAdder.Core;
using SubtitleAdder.EngineKit;

namespace SubtitleAdder.Core.Tests;

public sealed class TranslationProviderTests : IDisposable
{
    private readonly string _tempDir;
    private readonly string _filePath;
    private readonly bool _savedEnabled;
    private readonly string _savedUrl;
    private readonly string _savedModel;
    private readonly string _savedApiKey;

    public TranslationProviderTests()
    {
        _tempDir = Path.Combine(Path.GetTempPath(), $"subtitle-adder-providers-{Guid.NewGuid():N}");
        Directory.CreateDirectory(_tempDir);
        _filePath = Path.Combine(_tempDir, "translation-providers.json");

        _savedEnabled = ExternalLlmState.Shared.Enabled;
        _savedUrl = ExternalLlmState.Shared.Url;
        _savedModel = ExternalLlmState.Shared.Model;
        _savedApiKey = ExternalLlmState.Shared.ApiKey;
    }

    public void Dispose()
    {
        ExternalLlmState.Shared.Enabled = _savedEnabled;
        ExternalLlmState.Shared.Url = _savedUrl;
        ExternalLlmState.Shared.Model = _savedModel;
        ExternalLlmState.Shared.ApiKey = _savedApiKey;

        try
        {
            if (Directory.Exists(_tempDir))
                Directory.Delete(_tempDir, recursive: true);
        }
        catch (IOException)
        {
        }
    }

    [Fact]
    public void BuiltInPresetsIncludeAllRequiredProviders()
    {
        var ids = TranslationProviderManager.Presets.Select(p => p.Id).ToList();
        Assert.Contains("opencode-go", ids);
        Assert.Contains("openrouter", ids);
        Assert.Contains("openai", ids);
        Assert.Contains("deepseek", ids);
        Assert.Contains("groq", ids);
        Assert.Contains("gemini", ids);
        Assert.Contains("lan", ids);
        Assert.Contains("custom", ids);

        var openCode = TranslationProviderManager.GetPreset("opencode-go");
        Assert.Equal("OpenCode Go", openCode.Name);
        Assert.Equal("https://opencode.ai/zen/go/v1", openCode.BaseUrl);
        Assert.True(openCode.RequiresApiKey);
        Assert.Contains("glm-5", openCode.SuggestedModels);
        Assert.Contains("qwen3-235b", openCode.SuggestedModels);
    }

    [Fact]
    public void AddMultipleProvidersPersistsAndSyncsExternalLlmState()
    {
        var manager = new TranslationProviderManager(_filePath);
        Assert.Empty(manager.Providers);
        Assert.Null(manager.ActiveProviderId);
        Assert.False(manager.IsExternalProviderActive);
        Assert.False(ExternalLlmState.Shared.Enabled);

        var openCode = manager.AddProvider(
            presetId: "opencode-go",
            apiKey: "sk-oc-1234567890",
            model: "kimi-k2.5",
            setActive: true);

        Assert.Single(manager.Providers);
        Assert.Equal(openCode.Id, manager.ActiveProviderId);
        Assert.True(manager.IsExternalProviderActive);
        Assert.True(ExternalLlmState.Shared.Enabled);
        Assert.Equal("https://opencode.ai/zen/go/v1", ExternalLlmState.Shared.Url);
        Assert.Equal("kimi-k2.5", ExternalLlmState.Shared.Model);
        Assert.Equal("sk-oc-1234567890", ExternalLlmState.Shared.ApiKey);
        Assert.Equal("sk-••••7890", openCode.MaskedApiKey);

        var openRouter = manager.AddProvider(
            presetId: "openrouter",
            name: "My OpenRouter",
            apiKey: "sk-or-abcdef9876",
            model: "google/gemma-3-27b-it",
            setActive: false);

        var lan = manager.AddProvider(
            presetId: "lan",
            baseUrl: "http://192.168.1.50:11434",
            apiKey: "",
            model: "gemma4:e4b",
            setActive: false);

        Assert.Equal(3, manager.Providers.Count);
        // Active provider is still openCode because setActive was false for the next two.
        Assert.Equal(openCode.Id, manager.ActiveProviderId);

        // Switch active provider to OpenRouter
        manager.SetActiveProvider(openRouter.Id);
        Assert.Equal(openRouter.Id, manager.ActiveProviderId);
        Assert.Equal("https://openrouter.ai/api/v1", ExternalLlmState.Shared.Url);
        Assert.Equal("google/gemma-3-27b-it", ExternalLlmState.Shared.Model);
        Assert.Equal("sk-or-abcdef9876", ExternalLlmState.Shared.ApiKey);

        // Switch active provider to LAN Ollama (no API key)
        manager.SetActiveProvider(lan.Id);
        Assert.True(ExternalLlmState.Shared.Enabled);
        Assert.Equal("http://192.168.1.50:11434", ExternalLlmState.Shared.Url);
        Assert.Equal("gemma4:e4b", ExternalLlmState.Shared.Model);
        Assert.Null(ExternalLlmState.Shared.ActiveApiKey);

        // Reload from disk in a fresh manager instance
        var reloaded = new TranslationProviderManager(_filePath);
        Assert.Equal(3, reloaded.Providers.Count);
        Assert.Equal(lan.Id, reloaded.ActiveProviderId);

        // Switch back to local on-device models (null)
        reloaded.SetActiveProvider(null);
        Assert.Null(reloaded.ActiveProviderId);
        Assert.False(reloaded.IsExternalProviderActive);
        Assert.False(ExternalLlmState.Shared.Enabled);
        Assert.Null(ExternalLlmState.Shared.ActiveUrl);
        Assert.Null(ExternalLlmState.Shared.ActiveApiKey);
    }

    [Fact]
    public void UpdateAndRemoveProviderUpdatesStateAndDisk()
    {
        var manager = new TranslationProviderManager(_filePath);
        var p1 = manager.AddProvider("openai", apiKey: "sk-old-1111", model: "gpt-4o-mini", setActive: true);
        var p2 = manager.AddProvider("deepseek", apiKey: "sk-ds-2222", model: "deepseek-chat", setActive: false);

        manager.UpdateProvider(
            p1.Id,
            name: "OpenAI Pro",
            baseUrl: "https://api.openai.com/v1",
            apiKey: "sk-new-9999",
            model: "gpt-4.1-mini",
            asyncTranslation: false);

        Assert.Equal("OpenAI Pro", manager.ActiveProvider?.Name);
        Assert.Equal("gpt-4.1-mini", ExternalLlmState.Shared.Model);
        Assert.Equal("sk-new-9999", ExternalLlmState.Shared.ApiKey);
        Assert.False(manager.ActiveProvider?.AsyncTranslation ?? true);

        // Removing the active provider falls back to local mode (ActiveProviderId == null)
        manager.RemoveProvider(p1.Id);
        Assert.Single(manager.Providers);
        Assert.Equal(p2.Id, manager.Providers[0].Id);
        Assert.Null(manager.ActiveProviderId);
        Assert.False(ExternalLlmState.Shared.Enabled);
    }

    [Fact]
    public async Task TestAndFetchModelsSendsBearerHeaderAndFallsBackToChatCompletions()
    {
        var requests = new List<(string Url, string? Auth, string Method)>();
        var handler = new FakeHttpHandler(req =>
        {
            requests.Add((req.RequestUri!.ToString(), req.Headers.Authorization?.ToString(), req.Method.Method));
            if (req.RequestUri.AbsolutePath.EndsWith("/models", StringComparison.OrdinalIgnoreCase))
            {
                // Simulate a gateway like OpenCode Go that might return 404 on GET /models
                return new HttpResponseMessage(HttpStatusCode.NotFound);
            }
            if (req.RequestUri.AbsolutePath.EndsWith("/chat/completions", StringComparison.OrdinalIgnoreCase))
            {
                return new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new StringContent("""{"choices":[{"message":{"content":"ok"}}]}""", Encoding.UTF8, "application/json"),
                };
            }
            return new HttpResponseMessage(HttpStatusCode.NotFound);
        });

        var manager = new TranslationProviderManager(_filePath, handler);
        var profile = manager.AddProvider("opencode-go", apiKey: "sk-opencode-secret", model: "glm-5");

        var (success, _, models) = await manager.TestAndFetchModelsAsync(profile.Id);
        Assert.True(success);
        Assert.Contains("glm-5", models);
        Assert.Equal(2, requests.Count);
        Assert.Equal("https://opencode.ai/zen/go/v1/models", requests[0].Url);
        Assert.Equal("Bearer sk-opencode-secret", requests[0].Auth);
        Assert.Equal("https://opencode.ai/zen/go/v1/chat/completions", requests[1].Url);
        Assert.Equal("Bearer sk-opencode-secret", requests[1].Auth);
        Assert.Equal("POST", requests[1].Method);
    }

    private sealed class FakeHttpHandler(Func<HttpRequestMessage, HttpResponseMessage> responder) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) =>
            Task.FromResult(responder(request));
    }
}
