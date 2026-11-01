using System.Globalization;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using SubtitleAdder.EngineKit;

namespace SubtitleAdder.Core;

/// <summary>
/// Parses existing subtitle files (<c>.srt</c>, <c>.vtt</c>, <c>.ass</c>/<c>.ssa</c>, <c>.json</c>) into
/// <see cref="Sentence"/> items so users who already have a subtitle file can view, edit, and translate
/// it immediately without re-extracting audio or running speech recognition.
/// </summary>
public static partial class SubtitleParser
{
    public static readonly IReadOnlyList<string> Extensions = [".srt", ".vtt", ".ass", ".ssa", ".json"];

    private static readonly Regex TimingArrowRegex = new(
        @"^\s*(?<start>(?:\d{1,2}:)?\d{1,2}:\d{2}[,\.]\d{1,3})\s*-->\s*(?<end>(?:\d{1,2}:)?\d{1,2}:\d{2}[,\.]\d{1,3})",
        RegexOptions.Compiled);

    private static readonly Regex HtmlTagRegex = new(@"<[^>]+>", RegexOptions.Compiled);
    private static readonly Regex AssOverrideTagRegex = new(@"\{[^}]*\}", RegexOptions.Compiled);

    public static bool IsSubtitleFile(string? path)
    {
        if (string.IsNullOrWhiteSpace(path)) return false;
        var ext = Path.GetExtension(path);
        return Extensions.Any(e => string.Equals(e, ext, StringComparison.OrdinalIgnoreCase));
    }

    /// <summary>Finds a sidecar subtitle file next to <paramref name="videoPath"/> with the same base name, if one exists.</summary>
    public static string? FindSidecarSubtitle(string? videoPath)
    {
        if (string.IsNullOrWhiteSpace(videoPath)) return null;
        var dir = Path.GetDirectoryName(videoPath);
        var stem = Path.GetFileNameWithoutExtension(videoPath);
        if (string.IsNullOrEmpty(dir) || string.IsNullOrEmpty(stem)) return null;
        foreach (var ext in Extensions)
        {
            var candidate = Path.Combine(dir, stem + ext);
            if (File.Exists(candidate)) return candidate;
        }
        return null;
    }

    /// <summary>Reads a subtitle file from disk (auto-detecting UTF-8 vs legacy code pages) and parses its lines.</summary>
    public static IReadOnlyList<Sentence> ParseFile(string path)
    {
        var bytes = File.ReadAllBytes(path);
        var text = DecodeBytes(bytes);
        return Parse(text, Path.GetExtension(path));
    }

    public static IReadOnlyList<Sentence> Parse(string content, string? extension = null)
    {
        if (string.IsNullOrWhiteSpace(content)) return [];
        var trimmed = content.TrimStart('\uFEFF', ' ', '\t', '\r', '\n');
        var ext = (extension ?? "").Trim().ToLowerInvariant();

        if (ext is ".ass" or ".ssa" || trimmed.Contains("[Events]", StringComparison.OrdinalIgnoreCase))
            return ParseAss(trimmed);
        if (ext == ".json" || (trimmed.StartsWith('[') && trimmed.TrimEnd().EndsWith(']')))
            return ParseJson(trimmed);
        return ParseSrtOrVtt(trimmed);
    }

    private static string DecodeBytes(byte[] bytes)
    {
        if (bytes.Length == 0) return "";
        // Check BOM first.
        if (bytes.Length >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF)
            return Encoding.UTF8.GetString(bytes, 3, bytes.Length - 3);
        if (bytes.Length >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE)
            return Encoding.Unicode.GetString(bytes, 2, bytes.Length - 2);
        if (bytes.Length >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF)
            return Encoding.BigEndianUnicode.GetString(bytes, 2, bytes.Length - 2);

        try
        {
            var strictUtf8 = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true);
            return strictUtf8.GetString(bytes);
        }
        catch (DecoderFallbackException)
        {
            Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
            // Fall back to Windows-1254 (Turkish/Latin) which is a superset of CP1252 for most Western/Turkish SRT files.
            return Encoding.GetEncoding(1254).GetString(bytes);
        }
    }

    private static IReadOnlyList<Sentence> ParseJson(string json)
    {
        try
        {
            var list = JsonSerializer.Deserialize<List<Sentence>>(json);
            return list ?? [];
        }
        catch (JsonException)
        {
            return [];
        }
    }

    private static IReadOnlyList<Sentence> ParseSrtOrVtt(string content)
    {
        var lines = content.Replace("\r\n", "\n").Replace('\r', '\n').Split('\n');
        var results = new List<Sentence>();
        var i = 0;

        while (i < lines.Length)
        {
            var line = lines[i].Trim();
            if (line.Length == 0 || line.StartsWith("WEBVTT", StringComparison.OrdinalIgnoreCase))
            {
                i++;
                continue;
            }
            if (line.StartsWith("NOTE", StringComparison.Ordinal) || line.StartsWith("STYLE", StringComparison.Ordinal))
            {
                i++;
                while (i < lines.Length && lines[i].Trim().Length > 0) i++;
                continue;
            }

            var match = TimingArrowRegex.Match(line);
            if (!match.Success && i + 1 < lines.Length)
            {
                var nextMatch = TimingArrowRegex.Match(lines[i + 1].Trim());
                if (nextMatch.Success)
                {
                    match = nextMatch;
                    i++;
                }
            }

            if (!match.Success)
            {
                i++;
                continue;
            }

            var start = ParseTimestamp(match.Groups["start"].Value);
            var end = ParseTimestamp(match.Groups["end"].Value);
            i++;

            var textLines = new List<string>();
            while (i < lines.Length && lines[i].Trim().Length > 0)
            {
                var cleaned = CleanMarkup(lines[i].Trim());
                if (cleaned.Length > 0) textLines.Add(cleaned);
                i++;
            }

            var fullText = string.Join(" ", textLines).Trim();
            if (fullText.Length > 0 && end >= start)
            {
                results.Add(new Sentence(
                    id: results.Count + 1,
                    start: Math.Round(start, 3),
                    end: Math.Round(Math.Max(end, start + 0.05), 3),
                    text: fullText));
            }
        }

        return results;
    }

    private static IReadOnlyList<Sentence> ParseAss(string content)
    {
        var lines = content.Replace("\r\n", "\n").Replace('\r', '\n').Split('\n');
        var results = new List<Sentence>();
        var inEvents = false;
        var startIndex = 1;
        var endIndex = 2;
        var textIndex = 9;
        var fieldCount = 10;

        foreach (var raw in lines)
        {
            var line = raw.Trim();
            if (line.StartsWith('[') && line.EndsWith(']'))
            {
                inEvents = line.Equals("[Events]", StringComparison.OrdinalIgnoreCase);
                continue;
            }
            if (!inEvents) continue;

            if (line.StartsWith("Format:", StringComparison.OrdinalIgnoreCase))
            {
                var fields = line["Format:".Length..].Split(',').Select(f => f.Trim().ToLowerInvariant()).ToArray();
                fieldCount = fields.Length;
                var sIdx = Array.IndexOf(fields, "start");
                var eIdx = Array.IndexOf(fields, "end");
                var tIdx = Array.IndexOf(fields, "text");
                if (sIdx >= 0) startIndex = sIdx;
                if (eIdx >= 0) endIndex = eIdx;
                if (tIdx >= 0) textIndex = tIdx;
                continue;
            }

            if (!line.StartsWith("Dialogue:", StringComparison.OrdinalIgnoreCase)) continue;
            var payload = line["Dialogue:".Length..].TrimStart();
            var parts = payload.Split(',', fieldCount);
            if (parts.Length <= Math.Max(startIndex, Math.Max(endIndex, textIndex))) continue;

            var start = ParseTimestamp(parts[startIndex].Trim());
            var end = ParseTimestamp(parts[endIndex].Trim());
            var rawText = parts[textIndex].Replace(@"\N", " ").Replace(@"\n", " ").Replace(@"\h", " ");
            var text = CleanMarkup(rawText);
            if (text.Length > 0 && end >= start)
            {
                results.Add(new Sentence(
                    id: results.Count + 1,
                    start: Math.Round(start, 3),
                    end: Math.Round(Math.Max(end, start + 0.05), 3),
                    text: text));
            }
        }

        return results;
    }

    private static string CleanMarkup(string text)
    {
        var withoutAss = AssOverrideTagRegex.Replace(text, "");
        var withoutHtml = HtmlTagRegex.Replace(withoutAss, "");
        return Regex.Replace(withoutHtml, @"\s+", " ").Trim();
    }

    private static double ParseTimestamp(string raw)
    {
        var normalized = raw.Trim().Replace(',', '.');
        var parts = normalized.Split(':');
        if (parts.Length == 3 &&
            int.TryParse(parts[0], NumberStyles.Integer, CultureInfo.InvariantCulture, out var h) &&
            int.TryParse(parts[1], NumberStyles.Integer, CultureInfo.InvariantCulture, out var m) &&
            double.TryParse(parts[2], NumberStyles.Float, CultureInfo.InvariantCulture, out var s))
        {
            return h * 3600 + m * 60 + s;
        }
        if (parts.Length == 2 &&
            int.TryParse(parts[0], NumberStyles.Integer, CultureInfo.InvariantCulture, out var m2) &&
            double.TryParse(parts[1], NumberStyles.Float, CultureInfo.InvariantCulture, out var s2))
        {
            return m2 * 60 + s2;
        }
        return 0;
    }
}
