using System.Text;
using System.Text.Json;

var options = new JsonSerializerOptions
{
    WriteIndented = true,
    PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
    PropertyNameCaseInsensitive = true,
};

if (args.Length != 2)
{
    Console.Error.WriteLine("usage: ProvidersFixture write|read <path>");
    return 2;
}

if (args[0] == "write")
{
    var state = new PersistedState
    {
        ActiveProviderId = Guid.Parse("adf244d7-d14a-4a40-abf1-2da9a43ba655"),
        Providers =
        [
            new TranslationProviderProfile
            {
                Id = Guid.Parse("1bed0ce2-2d54-4a5d-a9fa-52fd7ad76929"),
                Name = "Yerel Ağ (Ollama / LM Studio)",
                PresetId = "lan",
                BaseUrl = "http://192.168.68.58:11434",
                ApiKey = "",
                Model = "gemma4:12b",
                AsyncTranslation = true,
                DiscoveredModels = ["gemma4:e2b", "qwen3.5:4b"],
            },
            new TranslationProviderProfile
            {
                Id = Guid.Parse("adf244d7-d14a-4a40-abf1-2da9a43ba655"),
                Name = "Şirket <test> & 'quote' + `tick` \"dq\" \\ 😀",
                PresetId = "opencode-go",
                BaseUrl = "https://opencode.ai/inference/go/openai/v1",
                ApiKey = "oc_csharp_plaintext_0123456789",
                Model = "glm-5.3-flash",
                AsyncTranslation = false,
                DiscoveredModels = [],
            },
        ],
    };
    File.WriteAllText(args[1], JsonSerializer.Serialize(state, options), new UTF8Encoding(false));
    return 0;
}

if (args[0] == "read")
{
    var state = JsonSerializer.Deserialize<PersistedState>(File.ReadAllText(args[1]), options)!;
    Console.OutputEncoding = new UTF8Encoding(false);
    Console.WriteLine($"active={state.ActiveProviderId}");
    foreach (var p in state.Providers)
    {
        Console.WriteLine(string.Join("|", p.Id, p.Name, p.PresetId, p.BaseUrl, p.ApiKey, p.Model, p.AsyncTranslation, string.Join(",", p.DiscoveredModels)));
    }
    return 0;
}

Console.Error.WriteLine("unknown mode");
return 2;

// Copied from TranslationProviderManager.cs (serialized members only).
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
}

public sealed class PersistedState
{
    public Guid? ActiveProviderId { get; set; }
    public List<TranslationProviderProfile> Providers { get; set; } = [];
}
