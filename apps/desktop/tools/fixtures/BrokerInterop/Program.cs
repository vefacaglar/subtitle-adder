// Drives the C# StateSyncBroker for the Rust interop tests. Usage: BrokerInterop <scope>
// Writes "CS-…" lines to stdout; reads one command per line from stdin:
//   publish <event json>            Publish(evt) on the broker (its own topic)
//   rawwrite <request json>         a raw CurrentUserOnly pipe client sends "W:"+json and prints the A: line it gets
//   roundtrip-request <json>        STJ deserialize + serialize a StateSyncWriteRequest (CS-REQ <json>)
//   roundtrip-event <json>          same for a StateSyncEvent (CS-EVT <json>)
//   try-event / try-request / try-ack <json>   STJ parse check (CS-TRY ok <json> | null | fail)
//   try-kind <text>                 Enum.TryParse<WriteKind>(ignoreCase) (CS-TRY ok <n> | fail)
//   ieq <a><TAB><b>                 string.Equals(a, b, OrdinalIgnoreCase) (CS-IEQ true|false)
//   readlines <base64>              the broker's pipe reader (StreamReader(UTF8, no BOM detection, 4096).ReadLine
//                                   until null) over those bytes, plus IsNullOrWhiteSpace per line
//                                   (CS-LINES [{"t":line,"b":blank},...])
//   status | scope | quit
using System.IO.Pipes;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace SubtitleAdder.Core;

internal static class Program
{
    private static readonly Lock OutLock = new();

    internal static void Log(string line)
    {
        lock (OutLock)
        {
            Console.Out.WriteLine(line);
            Console.Out.Flush();
        }
    }

    private static async Task<int> Main(string[] args)
    {
        Console.InputEncoding = new UTF8Encoding(false);
        Console.OutputEncoding = new UTF8Encoding(false);
        var scope = args.Length > 0 ? args[0] : "broker-interop";
        var options = StateSyncBroker.JsonOptions;
        using var broker = new StateSyncBroker(scope);
        using var sub = broker.Subscribe(evt => Log("CS-RECV " + JsonSerializer.Serialize(evt, options)));
        var ready = await broker.WaitUntilReadyAsync(TimeSpan.FromSeconds(10));
        Log($"CS-READY {ready.ToString().ToLowerInvariant()} leader={broker.IsLeader.ToString().ToLowerInvariant()}");

        string? line;
        while ((line = Console.In.ReadLine()) is not null)
        {
            var space = line.IndexOf(' ');
            var cmd = space < 0 ? line : line[..space];
            var arg = space < 0 ? "" : line[(space + 1)..];
            try
            {
                switch (cmd)
                {
                    case "publish":
                        broker.Publish(JsonSerializer.Deserialize<StateSyncEvent>(arg, options)!);
                        Log("CS-OK");
                        break;
                    case "rawwrite":
                        Log("CS-ACK " + await RawWriteAsync(scope, arg));
                        break;
                    case "roundtrip-request":
                        Log("CS-REQ " + JsonSerializer.Serialize(JsonSerializer.Deserialize<StateSyncWriteRequest>(arg, options), options));
                        break;
                    case "roundtrip-event":
                        Log("CS-EVT " + JsonSerializer.Serialize(JsonSerializer.Deserialize<StateSyncEvent>(arg, options), options));
                        break;
                    case "try-event":
                        Log("CS-TRY " + TryParse<StateSyncEvent>(arg, options));
                        break;
                    case "try-request":
                        Log("CS-TRY " + TryParse<StateSyncWriteRequest>(arg, options));
                        break;
                    case "try-ack":
                        Log("CS-TRY " + TryParse<StateSyncWriteAck>(arg, options));
                        break;
                    case "try-kind":
                        // The server's kind check (StateSyncEngine.EnqueueWriteRequestOnEngine).
                        Log("CS-TRY " + (Enum.TryParse<SqliteProfileStore.WriteKind>(arg, ignoreCase: true, out var wk)
                            ? "ok " + ((int)wk).ToString(System.Globalization.CultureInfo.InvariantCulture)
                            : "fail"));
                        break;
                    case "ieq":
                        // DispatchToLocalSubscribers' topic test: OrdinalIgnoreCase.
                        var tab = arg.IndexOf('\t');
                        Log("CS-IEQ " + string.Equals(arg[..tab], arg[(tab + 1)..], StringComparison.OrdinalIgnoreCase).ToString().ToLowerInvariant());
                        break;
                    case "readlines":
                        Log("CS-LINES " + ReadLines(Convert.FromBase64String(arg)));
                        break;
                    case "status":
                        Log($"CS-STATUS leader={broker.IsLeader.ToString().ToLowerInvariant()} ready={broker.IsReady.ToString().ToLowerInvariant()} count={broker.ActiveInstanceCount} clients={broker.ConnectedClientCount}");
                        break;
                    case "scope":
                        Log($"CS-SCOPE {Environment.UserDomainName}\\{Environment.UserName}");
                        break;
                    case "quit":
                        broker.Dispose();
                        Log($"CS-BYE closed={broker.ClosedEngineOnDispose.ToString().ToLowerInvariant()}");
                        return 0;
                }
            }
            catch (Exception ex)
            {
                Log("CS-ERR " + ex.GetType().Name + ": " + ex.Message.Replace('\n', ' '));
            }
        }
        return 0;
    }

    /// "ok <re-serialized json>" when STJ reads <paramref name="json"/> into a non-null value, "null" for a JSON null,
    /// "fail" when it throws (the broker drops such a line).
    private static string TryParse<T>(string json, JsonSerializerOptions options) where T : class
    {
        try
        {
            var value = JsonSerializer.Deserialize<T>(json, options);
            return value is null ? "null" : "ok " + JsonSerializer.Serialize(value, options);
        }
        catch
        {
            return "fail";
        }
    }

    /// Exactly how StateSyncBroker reads a pipe (client loop and server sessions): ReadLine until null; the broker then
    /// skips every line for which IsNullOrWhiteSpace is true.
    private static string ReadLines(byte[] bytes)
    {
        using var reader = new StreamReader(new MemoryStream(bytes), Encoding.UTF8, detectEncodingFromByteOrderMarks: false, bufferSize: 4096, leaveOpen: false);
        var lines = new List<Dictionary<string, object>>();
        string? line;
        while ((line = reader.ReadLine()) is not null)
        {
            lines.Add(new Dictionary<string, object> { ["t"] = line, ["b"] = string.IsNullOrWhiteSpace(line) });
        }
        return JsonSerializer.Serialize(lines);
    }

    private static async Task<string> RawWriteAsync(string scope, string requestJson)
    {
        var hash = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(scope)))[..16];
        using var client = new NamedPipeClientStream(".", $"SubtitleAdder.StateBroker.{hash}", PipeDirection.InOut,
            PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
        await client.ConnectAsync(3000);
        var bytes = Encoding.UTF8.GetBytes("W:" + requestJson + "\n");
        await client.WriteAsync(bytes);
        await client.FlushAsync();
        using var reader = new StreamReader(client, new UTF8Encoding(false), false, 4096, leaveOpen: true);
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        while (true)
        {
            var reply = await reader.ReadLineAsync(cts.Token);
            if (reply is null) return "EOF";
            if (!reply.StartsWith("A:", StringComparison.Ordinal)) continue;
            var ack = JsonSerializer.Deserialize<StateSyncWriteAck>(reply.AsSpan(2), StateSyncBroker.JsonOptions)!;
            return JsonSerializer.Serialize(ack, StateSyncBroker.JsonOptions);
        }
    }
}
