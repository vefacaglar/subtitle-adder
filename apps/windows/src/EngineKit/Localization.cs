using System.ComponentModel;
using System.Globalization;
using System.Reflection;
using System.Text;
using System.Text.Json;

namespace SubtitleAdder.EngineKit;

/// <summary>The two UI languages the app ships. <see cref="UILanguages.Code"/> is also what <c>SUBTITLE_ENGINE_LANG</c> expects.</summary>
public enum UILanguage
{
    Tr,
    En,
}

public static class UILanguages
{
    public static IReadOnlyList<UILanguage> All { get; } = [UILanguage.Tr, UILanguage.En];

    /// <summary>ISO code ("tr" / "en"); the raw value used in settings and in <c>SUBTITLE_ENGINE_LANG</c>.</summary>
    public static string Code(this UILanguage language) => language == UILanguage.En ? "en" : "tr";

    public static CultureInfo Culture(this UILanguage language) => CultureInfo.GetCultureInfo(language.Code());

    /// <summary>Parses "tr" / "en" (case-insensitive); <c>null</c> for anything else.</summary>
    public static UILanguage? Parse(string? code) => code?.Trim().ToLowerInvariant() switch
    {
        "tr" => UILanguage.Tr,
        "en" => UILanguage.En,
        _ => null,
    };

    /// <summary>
    /// Picks the language for "System": the first preferred language the app supports wins (Turkish or English),
    /// English when none matches. <paramref name="preferred"/> are BCP-47 tags (e.g. "tr-TR"; "tr_TR" also accepted);
    /// by default the current UI culture. The WinUI app passes the Windows user language list.
    /// </summary>
    public static UILanguage System(IEnumerable<string>? preferred = null)
    {
        preferred ??= [CultureInfo.CurrentUICulture.Name];
        foreach (var tag in preferred)
        {
            var primary = tag.Split('-', '_')[0];
            if (Parse(primary) is { } match) return match;
        }
        return UILanguage.En;
    }

    /// <summary>
    /// Looks <paramref name="key"/> up in <paramref name="catalogs"/> (first hit wins) and formats it with printf-style
    /// <paramref name="args"/>. Keys are the Turkish source strings, so a missing translation falls back to the key.
    /// With no arguments the text is returned as is (like Swift's <c>String(format:)</c> being skipped).
    /// </summary>
    public static string Localize(this UILanguage language, string key, IEnumerable<StringCatalog> catalogs, params object?[] args)
    {
        string? format = null;
        foreach (var catalog in catalogs)
        {
            format = catalog.Lookup(key, language);
            if (format is not null) break;
        }
        format ??= key;
        return args.Length == 0 ? format : Printf.Format(format, language.Culture(), args);
    }
}

/// <summary>A String Catalog (<c>Localizable.xcstrings</c>, JSON) read as key → language → value.</summary>
public sealed class StringCatalog
{
    private readonly Dictionary<string, Dictionary<string, string>> _table;

    private StringCatalog(Dictionary<string, Dictionary<string, string>> table) => _table = table;

    public static StringCatalog Empty { get; } = new([]);

    /// <summary>EngineKit's own strings (the macOS EngineKit catalog, embedded).</summary>
    public static StringCatalog EngineKit => LazyEngineKit.Value;

    private static readonly Lazy<StringCatalog> LazyEngineKit = new(() =>
        FromResource(typeof(StringCatalog).Assembly, "SubtitleAdder.EngineKit.Localizable.xcstrings"));

    public IReadOnlyCollection<string> Keys => _table.Keys;

    /// <summary>The translation of <paramref name="key"/>, or <c>null</c> when the catalog has none for that language.</summary>
    public string? Lookup(string key, UILanguage language) =>
        _table.TryGetValue(key, out var values) && values.TryGetValue(language.Code(), out var value) ? value : null;

    /// <summary>Reads an embedded catalog; an empty catalog if the resource is missing or unreadable.</summary>
    public static StringCatalog FromResource(Assembly assembly, string logicalName)
    {
        using var stream = assembly.GetManifestResourceStream(logicalName);
        if (stream is null) return Empty;
        using var reader = new StreamReader(stream, Encoding.UTF8);
        try
        {
            return Parse(reader.ReadToEnd());
        }
        catch (JsonException)
        {
            return Empty;
        }
    }

    /// <summary>Parses catalog JSON (<c>strings.&lt;key&gt;.localizations.&lt;lang&gt;.stringUnit.value</c>).</summary>
    public static StringCatalog Parse(string json)
    {
        var table = new Dictionary<string, Dictionary<string, string>>(StringComparer.Ordinal);
        using var document = JsonDocument.Parse(json);
        if (!document.RootElement.TryGetProperty("strings", out var strings) || strings.ValueKind != JsonValueKind.Object)
            return new StringCatalog(table);
        foreach (var entry in strings.EnumerateObject())
        {
            if (entry.Value.ValueKind != JsonValueKind.Object
                || !entry.Value.TryGetProperty("localizations", out var localizations)
                || localizations.ValueKind != JsonValueKind.Object) continue;
            foreach (var localization in localizations.EnumerateObject())
            {
                if (localization.Value.ValueKind == JsonValueKind.Object
                    && localization.Value.TryGetProperty("stringUnit", out var unit)
                    && unit.ValueKind == JsonValueKind.Object
                    && unit.TryGetProperty("value", out var value)
                    && value.ValueKind == JsonValueKind.String)
                {
                    if (!table.TryGetValue(entry.Name, out var values))
                        table[entry.Name] = values = new Dictionary<string, string>(StringComparer.Ordinal);
                    values[localization.Name] = value.GetString()!;
                }
            }
        }
        return new StringCatalog(table);
    }
}

/// <summary>
/// Formats Apple/printf-style format strings as found in the String Catalogs: <c>%@</c>, <c>%d</c>/<c>%i</c>/<c>%u</c>
/// with any length modifier (<c>%lld</c>, <c>%ld</c>), <c>%f</c> with precision (<c>%.1f</c>), <c>%s</c>, <c>%%</c>,
/// and positional arguments (<c>%1$@</c>, <c>%2$lld</c>). Unknown specifiers are left untouched.
/// </summary>
public static class Printf
{
    public static string Format(string format, CultureInfo culture, params object?[] args)
    {
        var output = new StringBuilder(format.Length + 16);
        var next = 0;
        var i = 0;
        while (i < format.Length)
        {
            var c = format[i];
            if (c != '%')
            {
                output.Append(c);
                i++;
                continue;
            }
            var start = i;
            i++;
            if (i < format.Length && format[i] == '%')
            {
                output.Append('%');
                i++;
                continue;
            }
            // Optional positional index "n$".
            int? position = null;
            var digitsStart = i;
            while (i < format.Length && char.IsAsciiDigit(format[i])) i++;
            if (i > digitsStart && i < format.Length && format[i] == '$')
            {
                position = int.Parse(format.AsSpan(digitsStart, i - digitsStart), CultureInfo.InvariantCulture);
                i++;
            }
            else
            {
                i = digitsStart;
            }
            // Flags and width are accepted and ignored (the catalogs use none).
            while (i < format.Length && "-+ #0".Contains(format[i])) i++;
            while (i < format.Length && char.IsAsciiDigit(format[i])) i++;
            int? precision = null;
            if (i < format.Length && format[i] == '.')
            {
                i++;
                var precisionStart = i;
                while (i < format.Length && char.IsAsciiDigit(format[i])) i++;
                precision = i > precisionStart
                    ? int.Parse(format.AsSpan(precisionStart, i - precisionStart), CultureInfo.InvariantCulture)
                    : 0;
            }
            while (i < format.Length && "hlqLztj".Contains(format[i])) i++;
            if (i >= format.Length)
            {
                output.Append(format, start, format.Length - start);
                break;
            }
            var conversion = format[i];
            i++;
            var index = position is { } p ? p - 1 : next++;
            var arg = index >= 0 && index < args.Length ? args[index] : null;
            switch (conversion)
            {
                case '@':
                case 's':
                    output.Append(Convert.ToString(arg, culture));
                    break;
                case 'd':
                case 'i':
                case 'u':
                    output.Append(arg is null ? "" : Convert.ToInt64(arg, CultureInfo.InvariantCulture).ToString(CultureInfo.InvariantCulture));
                    break;
                case 'f':
                case 'F':
                    output.Append(arg is null ? "" : Convert.ToDouble(arg, CultureInfo.InvariantCulture).ToString("F" + (precision ?? 6), culture));
                    break;
                default:
                    output.Append(format, start, i - start);
                    break;
            }
        }
        return output.ToString();
    }
}

/// <summary>
/// Process-wide current UI language. Views (or view models) subscribe to <see cref="PropertyChanged"/> /
/// <see cref="LanguageChanged"/> to rebuild their text immediately when the language changes in Settings.
/// </summary>
public sealed class LanguageState : INotifyPropertyChanged
{
    public static LanguageState Shared { get; } = new();

    private readonly Lock _lock = new();
    private UILanguage _language = UILanguage.Tr;

    public event PropertyChangedEventHandler? PropertyChanged;

    /// <summary>Raised with the new language after it changed (not raised when set to the same value).</summary>
    public event EventHandler<UILanguage>? LanguageChanged;

    public UILanguage Language
    {
        get
        {
            lock (_lock) return _language;
        }
        set
        {
            lock (_lock)
            {
                if (_language == value) return;
                _language = value;
            }
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Language)));
            LanguageChanged?.Invoke(this, value);
        }
    }
}

/// <summary>EngineKit's own user-facing strings (the embedded EngineKit catalog).</summary>
internal static class EngineStrings
{
    public static string Get(string key, UILanguage? language = null, params object?[] args) =>
        (language ?? LanguageState.Shared.Language).Localize(key, [StringCatalog.EngineKit], args);
}
