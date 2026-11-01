using SubtitleAdder.EngineKit;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

/// <summary>What to show in the subtitle over the video; the choice is persistent.</summary>
public enum SubtitleDisplay
{
    Original,
    Translation,
    Both,
}

public static class SubtitleDisplays
{
    public static IReadOnlyList<SubtitleDisplay> All { get; } =
        [SubtitleDisplay.Original, SubtitleDisplay.Translation, SubtitleDisplay.Both];

    /// <summary>Stored value ("original", "translation", "both"; same as macOS).</summary>
    public static string RawValue(this SubtitleDisplay display) => display switch
    {
        SubtitleDisplay.Translation => "translation",
        SubtitleDisplay.Both => "both",
        _ => "original",
    };

    public static SubtitleDisplay? Parse(string? raw) => raw switch
    {
        "original" => SubtitleDisplay.Original,
        "translation" => SubtitleDisplay.Translation,
        "both" => SubtitleDisplay.Both,
        _ => null,
    };

    public static string Title(this SubtitleDisplay display) => display switch
    {
        SubtitleDisplay.Translation => L("Çeviri"),
        SubtitleDisplay.Both => L("İkisi"),
        _ => L("Orijinal"),
    };
}

/// <summary>
/// A line's translation. <see cref="SourceText"/> is the original text at the time it was translated; if the original
/// changes later, the translation counts as "eski" (stale) and is marked in the UI.
/// </summary>
public sealed record TranslationEntry(string Text, string SourceText, string Language)
{
    public bool IsStale(Sentence sentence) => SourceText != sentence.Text;
}

/// <summary>Composes the text drawn over the video, including the dialogue dash.</summary>
public static class OverlayText
{
    /// <summary>
    /// The dash rule is evaluated on the lines actually shown (original, translation, or both, per line):
    /// it follows the speaker turn rule and is skipped when the shown text already starts with a dash.
    /// </summary>
    public static string? Compose(
        IReadOnlyList<Sentence> sentences,
        IReadOnlyDictionary<Guid, TranslationEntry> translations,
        SubtitleDisplay display,
        Guid uid)
    {
        if (sentences.IndexOfUid(uid) is not { } index) return null;
        var dash = sentences.StartsDialogueTurn(index);
        var original = sentences[index].Text.Trim();
        var translated = translations.TryGetValue(uid, out var entry) ? entry.Text.Trim() : null;
        string[] lines = display switch
        {
            SubtitleDisplay.Translation => [string.IsNullOrEmpty(translated) ? original : translated],
            SubtitleDisplay.Both => [original, translated ?? ""],
            _ => [original],
        };
        var shown = lines.Where(l => l.Length > 0).Select(l => DialogueDash.Apply(dash, l)).ToList();
        return shown.Count == 0 ? null : string.Join("\n", shown);
    }
}

/// <summary>An sRGB color as plain values (Core has no UI color type).</summary>
public readonly record struct RgbColor(byte R, byte G, byte B)
{
    /// <summary>"#RRGGBB".</summary>
    public string Hex => $"#{R:X2}{G:X2}{B:X2}";
}

/// <summary>
/// Small fixed palette for speaker dots, with a light and a dark variant of each color (the macOS system colors
/// blue, orange, green, purple, pink, teal).
/// </summary>
public static class SpeakerPalette
{
    private static readonly RgbColor[] Light =
    [
        new(0, 122, 255), new(255, 149, 0), new(40, 205, 65),
        new(175, 82, 222), new(255, 45, 85), new(89, 173, 196),
    ];

    private static readonly RgbColor[] Dark =
    [
        new(10, 132, 255), new(255, 159, 10), new(50, 215, 75),
        new(191, 90, 242), new(255, 55, 95), new(106, 196, 220),
    ];

    public static int Count => Light.Length;

    public static RgbColor Color(int speaker, bool dark = false)
    {
        var colors = dark ? Dark : Light;
        return colors[((speaker % colors.Length) + colors.Length) % colors.Length];
    }
}
