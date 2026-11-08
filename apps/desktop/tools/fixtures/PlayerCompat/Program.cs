// Replays an operation script on the C# app's own PlaybackTimeline and PlayerSessionGate (see PlayerCompat.csproj).
// Usage: PlayerCompat <script-file>. One operation per line, fields separated by single spaces; doubles are written
// as "R" round-trip text ("NaN", "Infinity", "-Infinity" included); one JSON array per output line.
//
// Timeline (times are milliseconds after a fixed epoch):
//   t-reset                         -> ["t", position, duration]
//   t-seek <seconds> <ms>           -> ["t", returned, position, duration]
//   t-observe <pos> <dur> <ms>      -> ["t", position, duration]
//   fmt <seconds> <0|1>             -> ["fmt", FormatTime(seconds, hours)]
//   clamp <seconds> <duration>      -> ["clamp", Clamp(seconds, duration)]
// Gate (generation operands: a decimal number, "L" = LoadGeneration, "L-1" = LoadGeneration - 1, "A" =
// ActiveGeneration; source operands: "0" = null, else an id naming one shared object per id):
//   g-begin                         -> ["g", returned, state...]
//   g-clear                         -> ["g", null, state...]
//   g-activate <gen> <src>          -> ["g", returned, state...]
//   g-open <gen> <src> <0|1> <err|-> -> ["g", returned, state...]
//   g-state <gen> <src> <0|1> <0|1> -> ["g", returned, state...]
//   g-capture                       -> ["g", [generation, sourceId], state...]
//   g-accept <gen> <src> <cur>      -> ["g", returned, state...]
//   g-opened <gen> <src> <cur> <0|1>
//   g-failed <gen> <src> <cur> <0|1>
//   g-current <gen>                 -> ["g", returned, state...]
// state = load generation, active generation, active source id (0 = null), opened, failed, error.
// Doubles are printed as their IEEE-754 bit pattern (decimal Int64) so the comparison is exact.
using System.Globalization;
using System.Text.Json;
using SubtitleAdder.Core;

var stdout = new StreamWriter(Console.OpenStandardOutput(), new System.Text.UTF8Encoding(false)) { NewLine = "\n" };
void Emit(params object?[] fields) => stdout.WriteLine(JsonSerializer.Serialize(fields));
static long B(double v) => BitConverter.DoubleToInt64Bits(v);
static double D(string s) => double.Parse(s, NumberStyles.Float, CultureInfo.InvariantCulture);

var epoch = DateTimeOffset.UnixEpoch;
var timeline = new PlaybackTimeline();
var gate = new PlayerSessionGate();
var sources = new Dictionary<int, object>();
object? Src(string s)
{
    var id = int.Parse(s, CultureInfo.InvariantCulture);
    if (id == 0) return null;
    if (!sources.TryGetValue(id, out var o)) sources[id] = o = new object();
    return o;
}
int SrcId(object? o) => o is null ? 0 : sources.First(kv => ReferenceEquals(kv.Value, o)).Key;
int Gen(string s) => s switch
{
    "L" => gate.LoadGeneration,
    "L-1" => unchecked(gate.LoadGeneration - 1),
    "A" => gate.ActiveGeneration,
    _ => int.Parse(s, CultureInfo.InvariantCulture),
};
PlayerEventToken Tok(string g, string s) => gate.CreateBoundToken(Gen(g), Src(s));
void G(object? result) => Emit("g", result, gate.LoadGeneration, gate.ActiveGeneration, SrcId(gate.ActiveSource),
    gate.HasActiveSourceOpened, gate.HasActiveSourceFailed, gate.ActiveSourceError);

foreach (var line in File.ReadAllText(args[0]).Split('\n').Where(l => l.Length > 0))
{
    var f = line.Split(' ');
    switch (f[0])
    {
        case "t-reset":
            timeline.Reset();
            Emit("t", B(timeline.Position), B(timeline.Duration));
            break;
        case "t-seek":
        {
            var r = timeline.Seek(D(f[1]), epoch.AddMilliseconds(D(f[2])));
            Emit("t", B(r), B(timeline.Position), B(timeline.Duration));
            break;
        }
        case "t-observe":
            timeline.Observe(D(f[1]), D(f[2]), epoch.AddMilliseconds(D(f[3])));
            Emit("t", B(timeline.Position), B(timeline.Duration));
            break;
        case "fmt":
            Emit("fmt", PlaybackTimeline.FormatTime(D(f[1]), f[2] == "1"));
            break;
        case "clamp":
            Emit("clamp", B(PlaybackTimeline.Clamp(D(f[1]), D(f[2]))));
            break;
        case "g-begin":
            G(gate.BeginLoad());
            break;
        case "g-clear":
            gate.ClearActiveSource();
            G(null);
            break;
        case "g-activate":
            G(gate.TryActivateSource(Gen(f[1]), Src(f[2])));
            break;
        case "g-open":
            G(gate.NotifySourceOpenCompleted(Tok(f[1], f[2]), f[3] == "1", f[4] == "-" ? null : f[4]));
            break;
        case "g-state":
            G(gate.NotifySourceStateChanged(Tok(f[1], f[2]), f[3] == "1", f[4] == "1"));
            break;
        case "g-capture":
        {
            var t = gate.CaptureEventToken();
            G(new object[] { t.Generation, SrcId(t.Source) });
            break;
        }
        case "g-accept":
            G(gate.ShouldAcceptEvent(Tok(f[1], f[2]), Src(f[3])));
            break;
        case "g-opened":
            G(gate.ShouldAcceptMediaOpened(Tok(f[1], f[2]), Src(f[3]), f[4] == "1"));
            break;
        case "g-failed":
            G(gate.ShouldAcceptMediaFailed(Tok(f[1], f[2]), Src(f[3]), f[4] == "1"));
            break;
        case "g-current":
            G(gate.IsCurrentLoad(Gen(f[1])));
            break;
        default:
            throw new InvalidOperationException("unknown op " + f[0]);
    }
}
stdout.Flush();
