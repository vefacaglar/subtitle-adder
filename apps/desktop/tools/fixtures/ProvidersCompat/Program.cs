// Drives the C# app's own provider code for the sa-providers behavior comparison (see ProvidersCompat.csproj).
// Every output line is one JSON array, so strings with any characters survive the trip.
//
//   pure <file>        one JSON array per input line: [op, args...]; prints [line number, result]. Ops: endpoints,
//                      chat, opencode, cloud, merge, parse, normalize, mask, available, picker, error, preset, uri
//   load <dir>         loads every file of <dir> (ordinal name order) with a fresh TranslationProviderManager and prints
//                      [name, active index | null, [[id | "*", name, presetId, baseUrl, apiKey, model, async, models]...]]
//                      ("*" = an id the file did not contain, i.e. a fresh random one)
//   scenario <file>    runs one JSON object per input line against a real TranslationProviderManager +
//                      SubtitleViewModel in a temp folder and prints a state line after every op (see Run below)
using System.Net;
using System.Reflection;
using System.Text.Encodings.Web;
using System.Text.Json;
using System.Text.RegularExpressions;
using SubtitleAdder.Core;
using SubtitleAdder.EngineKit;

var json = new JsonSerializerOptions { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };
var stdout = new StreamWriter(Console.OpenStandardOutput(), new System.Text.UTF8Encoding(false)) { NewLine = "\n" };
void Emit(params object?[] fields) => stdout.WriteLine(JsonSerializer.Serialize(fields, json));
static IEnumerable<string> InputLines(string path) =>
    File.ReadAllText(path, new System.Text.UTF8Encoding(false)).Split('\n').Where(l => l.Trim().Length > 0);
static string? Str(JsonElement e) => e.ValueKind == JsonValueKind.Null ? null : e.GetString();
static List<string> Strs(JsonElement e) => e.ValueKind == JsonValueKind.Array ? e.EnumerateArray().Select(x => x.GetString()!).ToList() : [];
static StateSyncBroker NewBroker() => new("providers-compat-" + Guid.NewGuid().ToString("N"));
static string TempDir()
{
    var dir = Path.Combine(Path.GetTempPath(), "sa-providers-compat-" + Guid.NewGuid().ToString("N"));
    Directory.CreateDirectory(dir);
    return dir;
}

var managerType = typeof(TranslationProviderManager);
const BindingFlags AnyStatic = BindingFlags.Static | BindingFlags.NonPublic | BindingFlags.Public;
LanguageState.Shared.Language = UILanguage.Tr;

var cmd = args.Length > 0 ? args[0] : "";
switch (cmd)
{
    case "pure":
    {
        var n = 0;
        foreach (var line in InputLines(args[1]))
        {
            using var doc = JsonDocument.Parse(line);
            var a = doc.RootElement.EnumerateArray().ToArray();
            object? result = a[0].GetString() switch
            {
                "endpoints" => TranslationProviderManager.BuildCandidateModelEndpoints(a[1].GetString()!),
                "chat" => TranslationProviderManager.BuildChatCompletionsEndpoint(a[1].GetString()!),
                "opencode" => managerType.GetMethod("IsOpenCodeGoUrl", AnyStatic)!.Invoke(null, [a[1].GetString()]),
                "cloud" => TranslationProviderManager.IsCloudProviderUrl(Str(a[1])),
                "merge" => managerType.GetMethod("MergeModels", AnyStatic)!.Invoke(null, [Strs(a[1]), Strs(a[2]), Str(a[3])]),
                "parse" => SubtitleViewModel.ParseSuitableExternalModels(a[1].GetString()!),
                "normalize" => SubtitleViewModel.NormalizeExternalBaseUrl(a[1].GetString()!),
                "mask" => new TranslationProviderProfile { ApiKey = a[1].GetString()! }.MaskedApiKey,
                "available" => new TranslationProviderProfile
                {
                    PresetId = a[1].GetString()!, Model = a[2].GetString()!, DiscoveredModels = Strs(a[3]),
                }.AvailableModels,
                "picker" => Picker(a[1].GetString()!, a[2].GetString()!, Strs(a[3])),
                "error" => ProviderError(a[1].GetInt32(), a[2].GetString()!, Str(a[3])),
                "preset" => TranslationProviderManager.GetPreset(Str(a[1])).Id,
                // What the connection tests take from `Uri.TryCreate(s, Absolute)`: valid with a non-empty Host, and
                // that Host (the test message's host label).
                "uri" => Uri.TryCreate(a[1].GetString()!, UriKind.Absolute, out var uri) && !string.IsNullOrEmpty(uri.Host)
                    ? uri.Host : null,
                var other => throw new InvalidOperationException("unknown pure op " + other),
            };
            Emit(n++, result);
        }
        break;
    }
    case "load":
    {
        foreach (var file in Directory.GetFiles(args[1]).OrderBy(f => Path.GetFileName(f), StringComparer.Ordinal))
        {
            var dir = TempDir();
            var target = Path.Combine(dir, "translation-providers.json");
            File.Copy(file, target);
            var text = File.ReadAllText(file);
            TranslationProviderManager manager;
            try
            {
                manager = new TranslationProviderManager(target, null, NewBroker());
            }
            catch (Exception e)
            {
                // The C# app crashes on some inputs (e.g. a null apiKey of the active provider): reported, not compared.
                Emit(Path.GetFileName(file), "crash", e.GetType().Name);
                continue;
            }
            var providers = manager.Providers;
            var active = manager.ActiveProviderId is { } id ? providers.ToList().FindIndex(p => p.Id == id) : (int?)null;
            Emit(Path.GetFileName(file), active, providers.Select(p => new object?[]
            {
                text.Contains(p.Id.ToString(), StringComparison.OrdinalIgnoreCase) ? p.Id.ToString() : "*",
                p.Name, p.PresetId, p.BaseUrl, p.ApiKey, p.Model, p.AsyncTranslation, p.DiscoveredModels,
            }).ToList());
        }
        break;
    }
    case "scenario":
        await Run(args[1]);
        break;
    default:
        Console.Error.WriteLine("usage: ProvidersCompat pure|load|scenario <path>");
        return 2;
}
stdout.Flush();
return 0;

static IReadOnlyList<string> Picker(string presetId, string model, List<string> discovered)
{
    // The profile exactly as given (a loaded file), so no editing rule touches the model or the list.
    var dir = TempDir();
    var path = Path.Combine(dir, "translation-providers.json");
    var id = Guid.NewGuid();
    File.WriteAllText(path, JsonSerializer.Serialize(new
    {
        activeProviderId = id,
        providers = new[] { new { id, name = "x", presetId, baseUrl = "https://x.test/v1", model, discoveredModels = discovered } },
    }));
    var manager = new TranslationProviderManager(path, null, NewBroker());
    return TranslationModelSelection.GetModels(manager, null!, id).Select(c => c.Id).ToList();
}

static string ProviderError(int status, string body, string? key)
{
    var response = new HttpResponseMessage((HttpStatusCode)status) { Content = new StringContent(body) };
    var task = (Task<string>)typeof(TranslationProviderManager).GetMethod("ReadProviderErrorAsync", BindingFlags.Static | BindingFlags.NonPublic)!
        .Invoke(null, [response, key, CancellationToken.None])!;
    return task.GetAwaiter().GetResult();
}

// Scenario ops (one JSON object per line), `index` = position in the list of provider ids in the order they first
// appeared (-1 = an id that never existed, null = none):
//   lang {value}; add {preset,name,url,key,model,async,active}; update {index,name,url,key,model,async};
//   model {index,model}; remove {index}; active {index}; discovered {index,models,selected}; vm {enabled,url,model,
//   async,models} (prints nothing); migrate; sync; apply; select_provider {index}; select_model {index,model};
//   picker {index}; test {index,http}; draft {preset,url,key,model,http}; file.
// `http` rules: [method | "*", url suffix (case-insensitive), status (0 = transport error with body as message), body];
// the first match answers, otherwise 404.
// State line: ["state", op number, op, result, providers, active index, external active, [ActiveUrl, ActiveModel,
// ActiveApiKey], vm]
static async Task Run(string script)
{
    var json = new JsonSerializerOptions { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };
    var stdout = new StreamWriter(Console.OpenStandardOutput(), new System.Text.UTF8Encoding(false)) { NewLine = "\n" };
    void Emit(params object?[] fields) => stdout.WriteLine(JsonSerializer.Serialize(fields, json));

    var dir = TempDir();
    var file = Path.Combine(dir, "translation-providers.json");
    var broker = NewBroker();
    var handler = new ScriptedHandler();
    var manager = new TranslationProviderManager(file, handler, broker);
    var session = (string)typeof(TranslationProviderManager).GetField("_instanceId", BindingFlags.Instance | BindingFlags.NonPublic)!.GetValue(manager)!;
    var profiles = new LocalProfileManager(Path.Combine(dir, "profiles.json"), broker);
    var cache = new EncryptedVideoCacheStore(Path.Combine(dir, "VideoCache"), profiles, 1L << 30);
    var vm = new SubtitleViewModel(new ModelCatalog(null), new InMemorySettingsStore(), null, null, cache);
    FixtureHooks.SharedProviders = manager;
    var known = new List<Guid>();

    void Learn()
    {
        foreach (var p in manager.Providers)
            if (!known.Contains(p.Id)) known.Add(p.Id);
    }
    Guid? Id(JsonElement o)
    {
        if (!o.TryGetProperty("index", out var i) || i.ValueKind == JsonValueKind.Null) return null;
        var n = i.GetInt32();
        return n < 0 ? Guid.NewGuid() : known[n];
    }
    static string Ms(string message) => Regex.Replace(message, @"· \d+ ms ·", "· N ms ·");

    var opNumber = 0;
    foreach (var line in InputLines(script))
    {
        using var doc = JsonDocument.Parse(line);
        var o = doc.RootElement;
        var op = o.GetProperty("op").GetString()!;
        object? result = null;
        handler.Rules = o.TryGetProperty("http", out var http) ? http.EnumerateArray().Select(r => r.EnumerateArray().ToArray())
            .Select(r => (r[0].GetString()!, r[1].GetString()!, r[2].GetInt32(), r[3].GetString()!)).ToList() : [];
        handler.Requests.Clear();
        switch (op)
        {
            case "lang":
                LanguageState.Shared.Language = o.GetProperty("value").GetString() == "en" ? UILanguage.En : UILanguage.Tr;
                continue;
            case "add":
                manager.AddProvider(o.GetProperty("preset").GetString()!, Str(o.GetProperty("name")), Str(o.GetProperty("url")),
                    Str(o.GetProperty("key")), Str(o.GetProperty("model")), o.GetProperty("async").GetBoolean(), o.GetProperty("active").GetBoolean());
                break;
            case "update":
                manager.UpdateProvider(Id(o) ?? Guid.Empty, o.GetProperty("name").GetString()!, o.GetProperty("url").GetString()!,
                    o.GetProperty("key").GetString()!, o.GetProperty("model").GetString()!, o.GetProperty("async").GetBoolean());
                break;
            case "model":
                manager.UpdateProviderModel(Id(o) ?? Guid.Empty, o.GetProperty("model").GetString()!);
                break;
            case "remove":
                manager.RemoveProvider(Id(o) ?? Guid.Empty);
                break;
            case "active":
                manager.SetActiveProvider(Id(o));
                break;
            case "discovered":
                manager.UpdateProviderDiscoveredModels(Id(o) ?? Guid.Empty, Strs(o.GetProperty("models")), Str(o.GetProperty("selected")));
                break;
            case "vm":
                // The view model's LAN setters also write ExternalLlmState while no cloud provider is active
                // (SubtitleViewModel behavior, not the manager's): undone here so the scenario compares the manager.
                var saved = (ExternalLlmState.Shared.Enabled, ExternalLlmState.Shared.Url, ExternalLlmState.Shared.Model, ExternalLlmState.Shared.ApiKey);
                vm.ExternalLlmEnabled = o.GetProperty("enabled").GetBoolean();
                vm.ExternalLlmUrl = o.GetProperty("url").GetString()!;
                vm.ExternalLlmModel = o.GetProperty("model").GetString()!;
                vm.ExternalLlmAsync = o.GetProperty("async").GetBoolean();
                vm.SetExternalLlmModels(Strs(o.GetProperty("models")));
                (ExternalLlmState.Shared.Enabled, ExternalLlmState.Shared.Url, ExternalLlmState.Shared.Model, ExternalLlmState.Shared.ApiKey) = saved;
                continue;
            case "migrate":
                manager.EnsureMigratedFromViewModel(vm);
                break;
            case "sync":
                manager.SyncFromLegacyLanSettings(vm);
                break;
            case "apply":
                manager.ApplyToViewModel(vm);
                break;
            case "select_provider":
                result = TranslationModelSelection.SelectProvider(manager, vm, Id(o));
                break;
            case "select_model":
                result = TranslationModelSelection.SelectModel(manager, vm, Id(o), o.GetProperty("model").GetString()!);
                break;
            case "picker":
                result = TranslationModelSelection.GetModels(manager, vm, Id(o)).Select(c => c.Id).ToList();
                break;
            case "test":
            {
                var (ok, message, models) = await manager.TestAndFetchModelsAsync(Id(o) ?? Guid.Empty);
                result = new object?[] { ok, Ms(message), models, handler.Report(session) };
                break;
            }
            case "draft":
            {
                var (ok, message, models) = await manager.TestAndFetchModelsAsync(o.GetProperty("preset").GetString()!,
                    o.GetProperty("url").GetString()!, o.GetProperty("key").GetString()!, Str(o.GetProperty("model")));
                result = new object?[] { ok, Ms(message), models, handler.Report(session) };
                break;
            }
            case "file":
                result = File.Exists(file) ? File.ReadAllText(file) : null;
                break;
            default:
                throw new InvalidOperationException("unknown scenario op " + op);
        }
        Learn();
        var providers = manager.Providers;
        var state = ExternalLlmState.Shared;
        Emit("state", opNumber++, op, result,
            providers.Select(p => new object?[] { known.IndexOf(p.Id), p.Name, p.PresetId, p.BaseUrl, p.ApiKey, p.Model, p.AsyncTranslation, p.DiscoveredModels }).ToList(),
            manager.ActiveProviderId is { } active ? known.IndexOf(active) : null,
            manager.IsExternalProviderActive,
            new object?[] { state.ActiveUrl, state.ActiveModel, state.ActiveApiKey },
            new object?[] { vm.ExternalLlmEnabled, vm.ExternalLlmUrl, vm.ExternalLlmModel, vm.ExternalLlmAsync, vm.ExternalLlmModels,
                vm.ActiveProviderName, vm.ActiveProviderModel, vm.ActiveProviderAsync, vm.TranslationModel });
    }
    stdout.Flush();
}

public static class FixtureHooks
{
    /// <summary>Stands in for <c>TranslationProviderManager.Shared</c> (never the user's real data folder).</summary>
    public static TranslationProviderManager SharedProviders
    {
        get => _shared ??= new TranslationProviderManager(
            Path.Combine(Path.GetTempPath(), "sa-providers-compat-shared-" + Environment.ProcessId, "translation-providers.json"),
            null, new StateSyncBroker("providers-compat-shared-" + Guid.NewGuid().ToString("N")));
        set => _shared = value;
    }

    private static TranslationProviderManager? _shared;
}

sealed class ScriptedHandler : HttpMessageHandler
{
    public List<(string Method, string Suffix, int Status, string Body)> Rules { get; set; } = [];
    public List<object?[]> Requests { get; } = [];
    private readonly List<(HttpRequestMessage Request, string? Body)> _seen = [];

    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        var body = request.Content is null ? null : await request.Content.ReadAsStringAsync(cancellationToken);
        _seen.Add((request, body));
        var url = request.RequestUri!.OriginalString;
        foreach (var (method, suffix, status, text) in Rules)
        {
            if ((method == "*" || method == request.Method.Method) && url.EndsWith(suffix, StringComparison.OrdinalIgnoreCase))
            {
                if (status == 0) throw new HttpRequestException(text);
                return new HttpResponseMessage((HttpStatusCode)status) { Content = new StringContent(text) };
            }
        }
        return new HttpResponseMessage(HttpStatusCode.NotFound) { Content = new StringContent("") };
    }

    /// <summary>[method, url, Authorization, User-Agent, Accept, x-opencode-session ("<session>" for the manager's
    /// instance id), Content-Type, body] per request, then forgets them.</summary>
    public List<object?[]> Report(string session)
    {
        var report = _seen.Select(s =>
        {
            var r = s.Request;
            string? Header(string name) => r.Headers.TryGetValues(name, out var v) ? string.Join(",", v) : null;
            var oc = Header("x-opencode-session");
            return new object?[]
            {
                r.Method.Method, r.RequestUri!.OriginalString, r.Headers.Authorization?.ToString(), Header("User-Agent"),
                r.Headers.Accept.Count > 0 ? r.Headers.Accept.ToString() : null, oc == session ? "<session>" : oc,
                r.Content?.Headers.ContentType?.ToString(), s.Body,
            };
        }).ToList();
        _seen.Clear();
        return report;
    }
}
