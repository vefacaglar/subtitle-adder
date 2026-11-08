// Drives the C# app's own EngineKit for the sa-enginekit compatibility tests (see EngineKitCompat.csproj).
using System.Text.Json;
using System.Text.Json.Nodes;
using SubtitleAdder.EngineKit;

var options = EngineJson.Options;
Console.OutputEncoding = new System.Text.UTF8Encoding(false);
var stdout = new StreamWriter(Console.OpenStandardOutput(), new System.Text.UTF8Encoding(false)) { NewLine = "\n" };

string Line(JsonNode? node) => node is null ? "null" : node.ToJsonString(options);

switch (args.Length > 0 ? args[0] : "")
{
    case "edit":
    {
        using var doc = JsonDocument.Parse(File.ReadAllText(args[1]));
        foreach (var testCase in doc.RootElement.EnumerateArray())
        {
            var list = testCase.GetProperty("sentences").Deserialize<List<Sentence>>(options)!;
            var origin = list.Select((s, i) => (s.Uid, i)).ToDictionary(p => p.Uid, p => p.i);
            var results = new JsonArray();
            foreach (var op in testCase.GetProperty("ops").EnumerateArray())
            {
                var name = op.GetProperty("op").GetString();
                int I() => op.GetProperty("i").GetInt32();
                double D(string key) => op.GetProperty(key).GetDouble();
                JsonNode? result = name switch
                {
                    "set_start" => list.SetStart(D("v"), I()),
                    "set_end" => list.SetEnd(D("v"), I()),
                    "nudge_start" => list.NudgeStart(I(), D("d")),
                    "nudge_end" => list.NudgeEnd(I(), D("d")),
                    "set_text" => list.SetText(op.GetProperty("text").GetString()!, I()),
                    "set_speaker" => list.SetSpeaker(
                        op.GetProperty("speaker").ValueKind == JsonValueKind.Null ? null : op.GetProperty("speaker").GetInt32(), I()),
                    "merge" => list.MergeWithNext(I()),
                    "delete" => list.DeleteSentence(I()),
                    "split" => list.SplitSentence(I(), D("t")),
                    "can_split" => list.CanSplit(I(), D("t")),
                    "active" => list.ActiveIndex(D("t")) is { } a ? a : null,
                    "dashes" => new JsonArray(list.DialogueDashes().Select(b => (JsonNode?)b).ToArray()),
                    "display" => I() >= 0 && I() < list.Count ? list.DisplayText(I()) : null,
                    "speakers" => new JsonArray(list.Speakers().Select(s => (JsonNode?)s).ToArray()),
                    "next_speaker" => list.NextNewSpeaker(),
                    _ => throw new InvalidOperationException($"unknown op {name}"),
                };
                results.Add(result);
            }
            var fresh = new Dictionary<Guid, int>();
            var uids = new JsonArray(list.Select(s => (JsonNode?)(origin.TryGetValue(s.Uid, out var o)
                ? $"old{o}"
                : $"new{(fresh.TryGetValue(s.Uid, out var n) ? n : fresh[s.Uid] = fresh.Count)}")).ToArray());
            var line = new JsonObject
            {
                ["results"] = results,
                ["final"] = JsonSerializer.Serialize(list, options),
                ["uids"] = uids,
            };
            stdout.WriteLine(Line(line));
        }
        break;
    }
    case "tracker":
    {
        using var doc = JsonDocument.Parse(File.ReadAllText(args[1]));
        foreach (var testCase in doc.RootElement.EnumerateArray())
        {
            var init = testCase.GetProperty("init");
            var tracker = new DownloadPhaseTracker(init[0].GetBoolean(), init[1].GetBoolean(), init[2].GetBoolean());
            var labels = new JsonArray();
            foreach (var e in testCase.GetProperty("events").EnumerateArray())
            {
                var stage = EngineStages.Parse(e[0].GetString()!);
                double? fraction = e[1].ValueKind == JsonValueKind.Null ? null : e[1].GetDouble();
                labels.Add(tracker.Update(stage, fraction)?.ToString());
            }
            stdout.WriteLine(Line(labels));
        }
        break;
    }
    case "decode":
    {
        foreach (var raw in File.ReadAllLines(args[1]))
        {
            JsonNode result;
            try
            {
                result = EngineEvent.Decode(raw) switch
                {
                    EngineEvent.Stage s => new JsonObject { ["stage"] = s.Kind.RawValue(), ["fraction"] = s.Fraction },
                    EngineEvent.Partial p => new JsonObject
                        { ["partial"] = p.Language, ["sentences"] = JsonSerializer.Serialize(p.Sentences, options) },
                    EngineEvent.Translation t => new JsonObject
                        { ["translation"] = t.Language, ["sentences"] = JsonSerializer.Serialize(t.Sentences, options) },
                    EngineEvent.Result r => new JsonObject
                    {
                        ["result"] = r.Value.Language,
                        ["duration"] = r.Value.Duration,
                        ["audio"] = r.Value.Audio,
                        ["outputs"] = new JsonArray(r.Value.Outputs.Select(o => (JsonNode?)o).ToArray()),
                        ["sentences"] = JsonSerializer.Serialize(r.Value.Sentences, options),
                    },
                    EngineEvent.Error e => new JsonObject { ["error"] = e.Message },
                    _ => throw new InvalidOperationException(),
                };
            }
            catch (JsonException e) when (e.Message.StartsWith("bilinmeyen olay: ", StringComparison.Ordinal))
            {
                result = new JsonObject { ["unknown"] = true };
            }
            catch (Exception e) when (e is JsonException or InvalidOperationException or FormatException)
            {
                result = new JsonObject { ["invalid"] = true };
            }
            stdout.WriteLine(Line(result));
        }
        break;
    }
    case "graphemes":
    {
        // One JSON string per line in, StringInfo.LengthInTextElements out (what SplitSentence counts).
        foreach (var raw in File.ReadAllLines(args[1]))
            stdout.WriteLine(new System.Globalization.StringInfo(JsonSerializer.Deserialize<string>(raw)!).LengthInTextElements);
        break;
    }
    default:
        Console.Error.WriteLine("usage: EngineKitCompat edit|tracker|decode|graphemes <file>");
        return 2;
}
stdout.Flush();
return 0;
