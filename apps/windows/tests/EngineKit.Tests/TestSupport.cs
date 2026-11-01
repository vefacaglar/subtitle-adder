using System.Diagnostics;
using System.Text;

namespace SubtitleAdder.EngineKit.Tests;

/// <summary>Builds EngineClients backed by FakeEngine.exe, so real process handling is exercised.</summary>
internal static class Fake
{
    private static readonly UTF8Encoding Utf8 = new(false);

    public static string Executable { get; } = Path.Combine(AppContext.BaseDirectory, "FakeEngine.exe");

    /// <summary>A client whose engine runs <paramref name="script"/> (see tests/FakeEngine/Program.cs for the commands).</summary>
    public static EngineClient Engine(string script, UILanguage? language = UILanguage.Tr)
    {
        var path = TempPath("fake-engine", ".txt");
        File.WriteAllText(path, script.Replace("\r\n", "\n"), Utf8);
        return new EngineClient(new EngineCommand(Executable, [path]), language);
    }

    public static string TempPath(string prefix, string extension = "") =>
        Path.Combine(Path.GetTempPath(), $"{prefix}-{Guid.NewGuid()}{extension}");

    public static string Read(string path) => File.ReadAllText(path, Utf8);

    /// <summary>Waits until the PID file is written.</summary>
    public static async Task<int> ReadPid(string file)
    {
        for (var i = 0; i < 200; i++)
        {
            try
            {
                if (int.TryParse(File.ReadAllText(file).Trim(), out var pid)) return pid;
            }
            catch (IOException)
            {
            }
            await Task.Delay(50);
        }
        throw new TimeoutException($"Could not read PID from {file}");
    }

    public static bool IsAlive(int pid)
    {
        try
        {
            using var process = Process.GetProcessById(pid);
            return !process.HasExited;
        }
        catch (ArgumentException)
        {
            return false;
        }
        catch (InvalidOperationException)
        {
            return false;
        }
    }

    public static async Task<bool> WaitUntilDead(int pid)
    {
        for (var i = 0; i < 60; i++)
        {
            if (!IsAlive(pid)) return true;
            await Task.Delay(50);
        }
        return !IsAlive(pid);
    }

    public static async Task<List<EngineEvent>> Collect(IAsyncEnumerable<EngineEvent> events)
    {
        var list = new List<EngineEvent>();
        await foreach (var e in events) list.Add(e);
        return list;
    }

    public static async Task Drain(IAsyncEnumerable<EngineEvent> events)
    {
        await foreach (var _ in events)
        {
        }
    }
}

/// <summary>A fact that runs only with <c>ENGINE_E2E=1</c> (real engine via uv).</summary>
public sealed class E2EFactAttribute : FactAttribute
{
    public E2EFactAttribute()
    {
        if (Environment.GetEnvironmentVariable("ENGINE_E2E") != "1") Skip = "Set ENGINE_E2E=1 to run against the real engine.";
    }
}
