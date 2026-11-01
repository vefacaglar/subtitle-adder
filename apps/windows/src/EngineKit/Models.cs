using System.Text.Json;
using System.Text.Json.Serialization;

namespace SubtitleAdder.EngineKit;

/// <summary>A word with its timestamps (seconds).</summary>
public readonly record struct Word(
    [property: JsonPropertyName("start")] double Start,
    [property: JsonPropertyName("end")] double End,
    [property: JsonPropertyName("text")] string Text)
{
    /// <summary>Midpoint of the word's time range; used to pick the split point.</summary>
    internal double Midpoint => (Start + End) / 2;
}

/// <summary>
/// A subtitle line. Immutable with value semantics (use <c>with</c> to change it), like the Swift struct.
/// </summary>
[JsonConverter(typeof(SentenceJsonConverter))]
public sealed record Sentence
{
    public Sentence(
        int id, double start, double end, string text,
        IReadOnlyList<Word>? words = null, int? speaker = null, Guid? uid = null)
    {
        Id = id;
        Uid = uid ?? Guid.NewGuid();
        Start = start;
        End = end;
        Text = text;
        Words = words ?? [];
        Speaker = speaker;
    }

    /// <summary>Displayed sequence number (starts at 1); renumbered after merging/deleting.</summary>
    public int Id { get; init; }

    /// <summary>
    /// View identity and selection key. Does not change when <see cref="Id"/> is renumbered and is not sent to the engine.
    /// </summary>
    public Guid Uid { get; init; }

    public double Start { get; init; }
    public double End { get; init; }
    public string Text { get; init; }
    public IReadOnlyList<Word> Words { get; init; }

    /// <summary>Zero-based speaker index from diarization; <c>null</c> when diarization is off.</summary>
    public int? Speaker { get; init; }

    /// <summary>Content equality; the transient view identity (<see cref="Uid"/>) is not part of the comparison.</summary>
    public bool Equals(Sentence? other) =>
        other is not null
        && Id == other.Id && Start.Equals(other.Start) && End.Equals(other.End)
        && Text == other.Text && Words.SequenceEqual(other.Words) && Speaker == other.Speaker;

    public override int GetHashCode()
    {
        var hash = new HashCode();
        hash.Add(Id);
        hash.Add(Start);
        hash.Add(End);
        hash.Add(Text);
        foreach (var word in Words) hash.Add(word);
        hash.Add(Speaker);
        return hash.ToHashCode();
    }
}

/// <summary>
/// Engine protocol shape of a sentence: <c>uid</c> is app-internal and never written; <c>words</c> defaults to [];
/// <c>speaker</c> is written only when present.
/// </summary>
internal sealed class SentenceJsonConverter : JsonConverter<Sentence>
{
    public override Sentence Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        if (reader.TokenType != JsonTokenType.StartObject) throw new JsonException("sentence: object expected");
        int? id = null;
        double? start = null, end = null;
        string? text = null;
        IReadOnlyList<Word>? words = null;
        int? speaker = null;
        while (reader.Read())
        {
            if (reader.TokenType == JsonTokenType.EndObject)
            {
                if (id is null || start is null || end is null || text is null)
                    throw new JsonException("sentence: id/start/end/text missing");
                return new Sentence(id.Value, start.Value, end.Value, text, words, speaker);
            }
            var name = reader.GetString();
            reader.Read();
            switch (name)
            {
                case "id": id = reader.GetInt32(); break;
                case "start": start = reader.GetDouble(); break;
                case "end": end = reader.GetDouble(); break;
                case "text": text = reader.GetString(); break;
                case "words":
                    words = reader.TokenType == JsonTokenType.Null
                        ? null
                        : JsonSerializer.Deserialize<List<Word>>(ref reader, options);
                    break;
                case "speaker":
                    speaker = reader.TokenType == JsonTokenType.Null ? null : reader.GetInt32();
                    break;
                default: reader.Skip(); break;
            }
        }
        throw new JsonException("sentence: unexpected end");
    }

    public override void Write(Utf8JsonWriter writer, Sentence value, JsonSerializerOptions options)
    {
        writer.WriteStartObject();
        writer.WriteNumber("id", value.Id);
        writer.WriteNumber("start", value.Start);
        writer.WriteNumber("end", value.End);
        writer.WriteString("text", value.Text);
        writer.WritePropertyName("words");
        JsonSerializer.Serialize(writer, value.Words, options);
        if (value.Speaker is { } speaker) writer.WriteNumber("speaker", speaker);
        writer.WriteEndObject();
    }
}

/// <summary>Subtitle formats the engine can write.</summary>
public enum SubtitleFormat
{
    Srt,
    Vtt,
    Json,
}

public static class SubtitleFormats
{
    public static IReadOnlyList<SubtitleFormat> All { get; } = [SubtitleFormat.Srt, SubtitleFormat.Vtt, SubtitleFormat.Json];

    /// <summary>The engine's name for the format ("srt", "vtt", "json"); also the file extension.</summary>
    public static string RawValue(this SubtitleFormat format) => format switch
    {
        SubtitleFormat.Srt => "srt",
        SubtitleFormat.Vtt => "vtt",
        SubtitleFormat.Json => "json",
        _ => throw new ArgumentOutOfRangeException(nameof(format)),
    };

    public static SubtitleFormat? Parse(string raw) => raw switch
    {
        "srt" => SubtitleFormat.Srt,
        "vtt" => SubtitleFormat.Vtt,
        "json" => SubtitleFormat.Json,
        _ => null,
    };
}

/// <summary>A catalog row from the engine's <c>models --json</c> output.</summary>
public sealed record EngineModel
{
    [JsonPropertyName("id")]
    public required string Id { get; init; }

    /// <summary>"transcription" (speech recognition), "translation" or "diarization" (speaker separation).</summary>
    // Older engine output has no kind/license/backend/languages/disk_mb;
    // defaults for a subtitle model and Whisper keep it backward compatible.
    [JsonPropertyName("kind")]
    public string Kind { get; init => field = value ?? "transcription"; } = "transcription";

    /// <summary>"whisper" (supports all languages, detects language) or "onnx" (fixed language list).</summary>
    [JsonPropertyName("backend")]
    public string Backend { get; init => field = value ?? "whisper"; } = "whisper";

    /// <summary>Supported ISO 639-1 codes; <c>null</c> means all languages.</summary>
    [JsonPropertyName("languages")]
    public IReadOnlyList<string>? Languages { get; init; }

    [JsonPropertyName("size_mb")]
    public required int SizeMB { get; init; }

    /// <summary>Actual space taken in the cache (MB); <c>null</c> if not downloaded.</summary>
    [JsonPropertyName("disk_mb")]
    public int? DiskMB { get; init; }

    [JsonPropertyName("speed")]
    public required int Speed { get; init; }

    [JsonPropertyName("accuracy")]
    public required int Accuracy { get; init; }

    /// <summary>SPDX short name from the model card (e.g. "MIT", "CC-BY-NC-4.0").</summary>
    [JsonPropertyName("license")]
    public string? License { get; init; }

    [JsonPropertyName("downloaded")]
    public required bool Downloaded { get; init; }

    [JsonIgnore] public bool IsTranslation => Kind == "translation";
    [JsonIgnore] public bool IsDiarization => Kind == "diarization";
    [JsonIgnore] public bool IsTranscription => Kind == "transcription";

    /// <summary>Licenses that forbid commercial use ("NC" appears in the SPDX name).</summary>
    [JsonIgnore]
    public bool HasNonCommercialLicense => License?.ToUpperInvariant().Contains("NC", StringComparison.Ordinal) ?? false;

    /// <summary>Only the Whisper backend does automatic language detection.</summary>
    [JsonIgnore] public bool DetectsLanguage => Backend == "whisper";

    /// <summary>If <see cref="Languages"/> is <c>null</c>, the model supports every language.</summary>
    public bool Supports(string language) => Languages?.Contains(language) ?? true;

    public bool Equals(EngineModel? other) =>
        other is not null
        && Id == other.Id && Kind == other.Kind && Backend == other.Backend
        && (Languages is null ? other.Languages is null : other.Languages is not null && Languages.SequenceEqual(other.Languages))
        && SizeMB == other.SizeMB && DiskMB == other.DiskMB && Speed == other.Speed && Accuracy == other.Accuracy
        && License == other.License && Downloaded == other.Downloaded;

    public override int GetHashCode() => HashCode.Combine(Id, Kind, Backend, SizeMB, DiskMB, License, Downloaded);
}

/// <summary>Shared JSON settings: UTF-8 text is written unescaped (the engine reads UTF-8).</summary>
internal static class EngineJson
{
    public static readonly JsonSerializerOptions Options = new()
    {
        Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
        // Like Swift's Decodable: a missing non-optional field is an error, not a silent default.
        RespectRequiredConstructorParameters = true,
    };
}
