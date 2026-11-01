using System.Globalization;
using SubtitleAdder.EngineKit;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

/// <summary>The language picker in the toolbar. An empty code means automatic detection.</summary>
public sealed class SubtitleLanguage
{
    private readonly Func<string> _title;
    private readonly Func<string> _shortName;
    private readonly Func<string> _targetName;

    private SubtitleLanguage(string code, Func<string> title, Func<string> shortName, Func<string> targetName)
    {
        Code = code;
        _title = title;
        _shortName = shortName;
        _targetName = targetName;
    }

    public static SubtitleLanguage Automatic { get; } = new("", () => L("Otomatik"), () => L("Otomatik algılama"), () => L("Otomatik"));
    public static SubtitleLanguage Turkish { get; } = new("tr", () => L("Türkçe (tr)"), () => L("Türkçe"), () => L("Türkçe"));
    public static SubtitleLanguage Portuguese { get; } = new("pt", () => L("Portekizce – Brezilya (pt)"), () => L("Portekizce"), () => L("Portekizce – Brezilya"));
    public static SubtitleLanguage Spanish { get; } = new("es", () => L("İspanyolca (es)"), () => L("İspanyolca"), () => L("İspanyolca"));
    public static SubtitleLanguage Russian { get; } = new("ru", () => L("Rusça (ru)"), () => L("Rusça"), () => L("Rusça"));
    public static SubtitleLanguage English { get; } = new("en", () => L("İngilizce (en)"), () => L("İngilizce"), () => L("İngilizce"));
    public static SubtitleLanguage German { get; } = new("de", () => L("Almanca (de)"), () => L("Almanca"), () => L("Almanca"));
    public static SubtitleLanguage French { get; } = new("fr", () => L("Fransızca (fr)"), () => L("Fransızca"), () => L("Fransızca"));
    public static SubtitleLanguage Japanese { get; } = new("ja", () => L("Japonca (ja)"), () => L("Japonca"), () => L("Japonca"));
    public static SubtitleLanguage Chinese { get; } = new("zh", () => L("Çince (zh)"), () => L("Çince"), () => L("Çince"));

    /// <summary>Menu order (same as macOS).</summary>
    public static IReadOnlyList<SubtitleLanguage> All { get; } = [Automatic, Turkish, Portuguese, Spanish, Russian, English, German, French, Japanese, Chinese];

    /// <summary>Languages that can be a translation target; "Otomatik" can't be a target.</summary>
    public static IReadOnlyList<SubtitleLanguage> TranslationTargets { get; } = [.. All.Where(l => l != Automatic)];

    /// <summary>ISO 639-1 code; empty for automatic.</summary>
    public string Code { get; }

    /// <summary>Name shown in the menu.</summary>
    public string Title => _title();

    /// <summary>Name for the short info in the status bar.</summary>
    public string ShortName => _shortName();

    /// <summary>Name in the target language menu (without the code).</summary>
    public string TargetName => _targetName();

    public static SubtitleLanguage? FromCode(string? code) => All.FirstOrDefault(l => l.Code == code);

    public override string ToString() => Title;
}

/// <summary>
/// Localized names of the model ids and the texts of the catalog rows.
/// Labels are kept only here; the toolbar picker and Settings use the same text.
/// </summary>
public static class ModelLabel
{
    public static string Name(string id) => id switch
    {
        "base" => L("Hızlı"),
        "small" => L("Dengeli"),
        "medium" => L("Doğru"),
        "large-v3-turbo" => L("Çok doğru (turbo)"),
        "large-v3" => L("En doğru"),
        "parakeet-v3" => L("Parakeet v3 (hızlı, çok dilli)"),
        "canary-1b-v2" => L("Canary (çok dilli)"),
        "gigaam-v3" => L("GigaAM (Rusça)"),
        "nllb-1.3b" => L("NLLB-200 1.3B"),
        "qwen3.5-4b" => L("Qwen 3.5 4B"),
        "madlad-3b" => L("MADLAD-400 3B"),
        "gemma-4-e4b" => L("Gemma 4 E4B"),
        "qwen3.5-9b" => L("Qwen 3.5 9B"),
        "gemma-4-12b" => L("Gemma 4 12B"),
        "speaker-diarization" => L("Konuşmacı ayırma"),
        _ => id,
    };

    public static string MenuTitle(EngineModel model) => $"{Name(model.Id)} · {Detail(model)}";

    public static string Detail(EngineModel model) =>
        model.Downloaded ? L("hazır") : L("ilk kullanımda ~%@ indirilir", SizeText(model.SizeMB));

    /// <summary>Short language summary: "Tüm diller" or "bg, hr, cs…".</summary>
    public static string LanguageSummary(EngineModel model)
    {
        if (model.IsDiarization) return L("Dilden bağımsız");
        if (model.Languages is not { } languages) return L("Tüm diller");
        var shown = string.Join(", ", languages.Take(3));
        return languages.Count > 3 ? shown + "…" : shown;
    }

    public static string StorageStatus(EngineModel model) => model.Downloaded
        ? L("Yüklü — diskte %@", SizeText(model.DiskMB ?? model.SizeMB))
        : L("Yüklü değil — ~%@ indirilir", SizeText(model.SizeMB));

    /// <summary>License text in the Settings row.</summary>
    public static string LicenseText(EngineModel model) => L("Lisans: %@", model.License ?? L("bilinmiyor"));

    public static string SizeText(int sizeMB)
    {
        if (sizeMB < 1000) return $"{sizeMB} MB";
        // The decimal separator follows the UI language (Turkish comma, English point).
        var separator = LanguageState.Shared.Language == UILanguage.Tr ? "," : ".";
        var text = (sizeMB / 1000.0).ToString("0.0", CultureInfo.InvariantCulture).Replace(".", separator);
        if (text.EndsWith(separator + "0", StringComparison.Ordinal)) text = text[..^2];
        return $"{text} GB";
    }
}
