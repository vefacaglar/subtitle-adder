// See PlatformCompat.csproj. The client/server bodies are copied from
// apps/windows/src/SubtitleAdder/SingleInstanceCoordinator.cs with the pipe name as a parameter.
using System.IO.Pipes;
using System.Text;

Console.OutputEncoding = new UTF8Encoding(false);
switch (args.ElementAtOrDefault(0))
{
    case "client":
        Console.WriteLine(ForwardToPrimaryInstance(args[1], args.Skip(2).ToList()) ? "ok" : "fail");
        return 0;
    case "server":
        await Serve(args[1], int.Parse(args[2]));
        return 0;
    case "names":
        var safe = SanitizeToken(args[1]);
        Console.WriteLine(safe);
        Console.WriteLine($@"Local\SubtitleAdder.SingleInstance.{safe}");
        Console.WriteLine($"SubtitleAdder.SingleInstance.{safe}");
        return 0;
    default:
        Console.Error.WriteLine("usage: PlatformCompat client <pipe> <path...> | server <pipe> <count> | names <user>");
        return 2;
}

static bool ForwardToPrimaryInstance(string pipeName, IReadOnlyList<string> filePaths)
{
    try
    {
        using var client = new NamedPipeClientStream(".", pipeName, PipeDirection.Out, PipeOptions.CurrentUserOnly);
        client.Connect(timeout: 1200);
        using var writer = new StreamWriter(client, new UTF8Encoding(false)) { AutoFlush = true };
        foreach (var path in filePaths)
        {
            writer.WriteLine(path);
        }
        return true;
    }
    catch
    {
        return false;
    }
}

static async Task Serve(string pipeName, int count)
{
    // Ready marker: the Rust side waits for it before connecting.
    Console.WriteLine("ready");
    for (var i = 0; i < count; i++)
    {
        await using var server = new NamedPipeServerStream(
            pipeName,
            PipeDirection.In,
            maxNumberOfServerInstances: NamedPipeServerStream.MaxAllowedServerInstances,
            PipeTransmissionMode.Byte,
            PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
        await server.WaitForConnectionAsync().ConfigureAwait(false);
        using var reader = new StreamReader(server, Encoding.UTF8);
        var paths = new List<string>();
        while (await reader.ReadLineAsync().ConfigureAwait(false) is { } line)
        {
            var trimmed = line.Trim().Trim('"');
            if (trimmed.Length > 0 && File.Exists(trimmed))
            {
                paths.Add(Path.GetFullPath(trimmed));
            }
        }
        Console.WriteLine($"batch|{paths.Count}");
        foreach (var p in paths) Console.WriteLine($"path|{p}");
    }
}

static string SanitizeToken(string raw)
{
    if (string.IsNullOrWhiteSpace(raw)) return "default";
    var chars = raw.Select(c => char.IsLetterOrDigit(c) ? c : '_').ToArray();
    return new string(chars);
}
