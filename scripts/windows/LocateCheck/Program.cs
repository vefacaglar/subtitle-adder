// Usage: LocateCheck <assembled app dir>
// Runs EngineCommand.Locate as SubtitleAdder.exe would from that directory (SUBTITLE_ENGINE unset) and exits 0
// only when it resolves to <dir>\subtitle-engine\subtitle-engine.exe, not to the repo's `uv run` fallback.
using SubtitleAdder.EngineKit;

if (args.Length != 1)
{
    Console.Error.WriteLine("usage: LocateCheck <app dir>");
    return 2;
}
var appDir = Path.GetFullPath(args[0]);
var expected = Path.Combine(appDir, "subtitle-engine", "subtitle-engine.exe");

var environment = new Dictionary<string, string?>(StringComparer.OrdinalIgnoreCase);
foreach (System.Collections.DictionaryEntry entry in Environment.GetEnvironmentVariables())
    environment[(string)entry.Key] = entry.Value as string;
environment.Remove("SUBTITLE_ENGINE");

var command = EngineCommand.Locate(environment, appDir + Path.DirectorySeparatorChar);
if (command is null)
{
    Console.Error.WriteLine("Locate found no engine.");
    return 1;
}
var resolved = string.Join(' ', [command.Executable, .. command.Arguments]);
if (!string.Equals(Path.GetFullPath(command.Executable), expected, StringComparison.OrdinalIgnoreCase) || command.Arguments.Count != 0)
{
    Console.Error.WriteLine($"Locate picked '{resolved}', expected '{expected}'.");
    return 1;
}
Console.WriteLine($"Locate -> {resolved}");
return 0;
