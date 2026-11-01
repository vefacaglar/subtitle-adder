using System.Text.Json;
using System.Text.Json.Serialization;

namespace SubtitleAdder.EngineKit;

/// <summary>Pipeline stages of the <c>stage</c> event. Anything the app does not know maps to <see cref="Unknown"/>.</summary>
public enum EngineStage
{
    ExtractingAudio,
    DownloadingModel,
    LoadingModel,
    Transcribing,
    Diarizing,
    Segmenting,
    Translating,
    Unknown,
}

public static class EngineStages
{
    /// <summary>Wire name of the stage (e.g. "downloading_model"); "unknown" for <see cref="EngineStage.Unknown"/>.</summary>
    public static string RawValue(this EngineStage stage) => stage switch
    {
        EngineStage.ExtractingAudio => "extracting_audio",
        EngineStage.DownloadingModel => "downloading_model",
        EngineStage.LoadingModel => "loading_model",
        EngineStage.Transcribing => "transcribing",
        EngineStage.Diarizing => "diarizing",
        EngineStage.Segmenting => "segmenting",
        EngineStage.Translating => "translating",
        _ => "unknown",
    };

    public static EngineStage Parse(string raw) => raw switch
    {
        "extracting_audio" => EngineStage.ExtractingAudio,
        "downloading_model" => EngineStage.DownloadingModel,
        "loading_model" => EngineStage.LoadingModel,
        "transcribing" => EngineStage.Transcribing,
        "diarizing" => EngineStage.Diarizing,
        "segmenting" => EngineStage.Segmenting,
        "translating" => EngineStage.Translating,
        _ => EngineStage.Unknown,
    };
}

/// <summary>Events the engine writes line by line to stdout in <c>--json</c> mode (see engine/src/subtitle_engine/cli.py).</summary>
public abstract record EngineEvent
{
    private EngineEvent() { }

    public sealed record Stage(EngineStage Kind, double? Fraction) : EngineEvent;

    public sealed record Result(EngineResult Value) : EngineEvent;

    public sealed record Translation(string Language, IReadOnlyList<Sentence> Sentences) : EngineEvent
    {
        public bool Equals(Translation? other) =>
            other is not null && Language == other.Language && Sentences.SequenceEqual(other.Sentences);

        public override int GetHashCode() => HashCode.Combine(Language, Sentences.Count);
    }

    public sealed record Error(string Message) : EngineEvent;

    /// <summary>Decodes one stdout line. Throws <see cref="JsonException"/> on invalid JSON or an unknown event.</summary>
    public static EngineEvent Decode(string line)
    {
        using var document = JsonDocument.Parse(line);
        var root = document.RootElement;
        if (root.ValueKind != JsonValueKind.Object
            || !root.TryGetProperty("event", out var eventName)
            || eventName.ValueKind != JsonValueKind.String)
        {
            throw new JsonException("event missing");
        }
        var name = eventName.GetString();
        var options = EngineJson.Options;
        switch (name)
        {
            case "stage":
                var stage = root.Deserialize<StagePayload>(options) ?? throw new JsonException("stage payload");
                return new Stage(EngineStages.Parse(stage.Stage ?? throw new JsonException("stage missing")), stage.Fraction);
            case "result":
                return new Result(root.Deserialize<EngineResult>(options) ?? throw new JsonException("result payload"));
            case "translation":
                var translation = root.Deserialize<TranslationPayload>(options) ?? throw new JsonException("translation payload");
                return new Translation(
                    translation.Language ?? throw new JsonException("language missing"),
                    translation.Sentences ?? throw new JsonException("sentences missing"));
            case "error":
                var error = root.Deserialize<ErrorPayload>(options) ?? throw new JsonException("error payload");
                return new Error(error.Message ?? throw new JsonException("message missing"));
            default:
                throw new JsonException($"bilinmeyen olay: {name}");
        }
    }

    private sealed record StagePayload(
        [property: JsonPropertyName("stage")] string Stage,
        [property: JsonPropertyName("fraction")] double? Fraction = null);

    private sealed record TranslationPayload(
        [property: JsonPropertyName("language")] string Language,
        [property: JsonPropertyName("sentences")] IReadOnlyList<Sentence> Sentences);

    private sealed record ErrorPayload([property: JsonPropertyName("message")] string Message);
}

/// <summary>Payload of the <c>result</c> event.</summary>
public sealed record EngineResult
{
    [JsonPropertyName("language")] public required string Language { get; init; }
    [JsonPropertyName("duration")] public required double Duration { get; init; }
    [JsonPropertyName("audio")] public required string Audio { get; init; }
    [JsonPropertyName("outputs")] public required IReadOnlyList<string> Outputs { get; init; }
    [JsonPropertyName("sentences")] public required IReadOnlyList<Sentence> Sentences { get; init; }

    public bool Equals(EngineResult? other) =>
        other is not null && Language == other.Language && Duration.Equals(other.Duration) && Audio == other.Audio
        && Outputs.SequenceEqual(other.Outputs) && Sentences.SequenceEqual(other.Sentences);

    public override int GetHashCode() => HashCode.Combine(Language, Duration, Audio, Sentences.Count);
}
