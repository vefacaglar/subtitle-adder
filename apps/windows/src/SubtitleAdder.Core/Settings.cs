using System.Text.Json;
using System.Text.Json.Nodes;

namespace SubtitleAdder.Core;

/// <summary>
/// Persistent key/value settings (the Windows counterpart of UserDefaults; same keys as macOS). Getters return
/// <c>null</c> when the key is missing or holds another type.
/// </summary>
public interface ISettingsStore
{
    string? GetString(string key);
    bool? GetBool(string key);
    int? GetInt(string key);
    double? GetDouble(string key);
    void SetString(string key, string value);
    void SetBool(string key, bool value);
    void SetInt(string key, int value);
    void SetDouble(string key, double value);
    void Remove(string key);

    /// <summary>Raised with the key after a value changed through this store.</summary>
    event EventHandler<string>? Changed;
}

/// <summary>Settings held in memory as JSON values; the base of the file store and the test store.</summary>
public class InMemorySettingsStore : ISettingsStore
{
    private readonly Lock _lock = new();
    protected readonly Dictionary<string, JsonNode?> Values;

    public InMemorySettingsStore() : this(new Dictionary<string, JsonNode?>(StringComparer.Ordinal)) { }

    protected InMemorySettingsStore(Dictionary<string, JsonNode?> values) => Values = values;

    public event EventHandler<string>? Changed;

    public string? GetString(string key) => Read(key, node =>
        node is JsonValue value && value.TryGetValue<string>(out var text) ? text : null);

    public bool? GetBool(string key) => Read<bool?>(key, node =>
        node is JsonValue value && value.TryGetValue<bool>(out var flag) ? flag : null);

    public int? GetInt(string key) => Read<int?>(key, node =>
    {
        if (node is not JsonValue value) return null;
        if (value.TryGetValue<int>(out var number)) return number;
        if (value.TryGetValue<double>(out var real) && real == Math.Floor(real) && real is >= int.MinValue and <= int.MaxValue)
            return (int)real;
        return null;
    });

    public double? GetDouble(string key) => Read<double?>(key, node =>
        node is JsonValue value && value.TryGetValue<double>(out var number) ? number : null);

    public void SetString(string key, string value) => Write(key, JsonValue.Create(value));
    public void SetBool(string key, bool value) => Write(key, JsonValue.Create(value));
    public void SetInt(string key, int value) => Write(key, JsonValue.Create(value));

    /// <summary>Non-finite numbers cannot be stored in JSON; they remove the key so the default applies.</summary>
    public void SetDouble(string key, double value)
    {
        if (double.IsFinite(value)) Write(key, JsonValue.Create(value));
        else Remove(key);
    }

    public void Remove(string key)
    {
        lock (_lock)
        {
            if (!Values.Remove(key)) return;
            Persist();
        }
        Changed?.Invoke(this, key);
    }

    private T Read<T>(string key, Func<JsonNode?, T> convert)
    {
        lock (_lock) return Values.TryGetValue(key, out var node) ? convert(node) : convert(null);
    }

    private void Write(string key, JsonNode value)
    {
        lock (_lock)
        {
            if (Values.TryGetValue(key, out var old) && JsonNode.DeepEquals(old, value)) return;
            Values[key] = value;
            Persist();
        }
        Changed?.Invoke(this, key);
    }

    /// <summary>Called under the lock after every change.</summary>
    protected virtual void Persist() { }

    protected JsonObject Snapshot()
    {
        var root = new JsonObject();
        foreach (var (key, node) in Values) root[key] = node?.DeepClone();
        return root;
    }
}

/// <summary>
/// Settings in a JSON file, by default <c>%LOCALAPPDATA%\SubtitleAdder\settings.json</c>. Every change is written
/// at once (temp file + replace); a missing or corrupt file starts with defaults.
/// </summary>
public sealed class JsonFileSettingsStore : InMemorySettingsStore
{
    private static readonly JsonSerializerOptions WriteOptions = new() { WriteIndented = true };

    public JsonFileSettingsStore(string? path = null) : base(Load(path ??= DefaultPath)) => FilePath = path;

    public static string DefaultPath { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "SubtitleAdder", "settings.json");

    public string FilePath { get; }

    private static Dictionary<string, JsonNode?> Load(string path)
    {
        var values = new Dictionary<string, JsonNode?>(StringComparer.Ordinal);
        try
        {
            if (File.Exists(path) && JsonNode.Parse(File.ReadAllText(path)) is JsonObject root)
            {
                foreach (var (key, node) in root) values[key] = node?.DeepClone();
            }
        }
        catch (Exception error) when (error is IOException or JsonException or UnauthorizedAccessException)
        {
            // Unreadable settings are not fatal; defaults apply and the next change rewrites the file.
        }
        return values;
    }

    protected override void Persist()
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(FilePath)!);
            var temp = FilePath + ".tmp";
            File.WriteAllText(temp, Snapshot().ToJsonString(WriteOptions));
            File.Move(temp, FilePath, overwrite: true);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // The value stays in memory for this session.
        }
    }
}
