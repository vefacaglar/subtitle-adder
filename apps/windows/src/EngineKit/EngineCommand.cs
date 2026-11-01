namespace SubtitleAdder.EngineKit;

/// <summary>
/// Command that launches the engine. <c>uv run</c> in development, the PyInstaller onedir next to the app when packaged.
/// </summary>
public sealed record EngineCommand(string Executable, IReadOnlyList<string> Arguments)
{
    public EngineCommand(string executable) : this(executable, []) { }

    public bool Equals(EngineCommand? other) =>
        other is not null && Executable == other.Executable && Arguments.SequenceEqual(other.Arguments);

    public override int GetHashCode() => HashCode.Combine(Executable, Arguments.Count);

    /// <summary>
    /// Location of the packaged engine: <c>&lt;base&gt;\subtitle-engine\subtitle-engine.exe</c> (PyInstaller onedir next
    /// to SubtitleAdder.exe). <c>null</c> if not found.
    /// </summary>
    public static string? BundledEngine(string? baseDirectory = null)
    {
        var path = Path.Combine(baseDirectory ?? AppContext.BaseDirectory, "subtitle-engine", "subtitle-engine.exe");
        return File.Exists(path) ? path : null;
    }

    /// <summary>
    /// In order: the <c>SUBTITLE_ENGINE</c> environment variable (engine exe), the packaged engine next to the app, and in
    /// development the <c>engine\</c> project of the repo found by walking up from <paramref name="baseDirectory"/>
    /// (<c>uv run</c>). <paramref name="environment"/> defaults to the process environment and is also where PATH,
    /// USERPROFILE and APPDATA are read from. <c>null</c> when nothing is found.
    /// </summary>
    public static EngineCommand? Locate(IReadOnlyDictionary<string, string?>? environment = null, string? baseDirectory = null)
    {
        environment ??= ProcessEnvironment();
        baseDirectory ??= AppContext.BaseDirectory;
        if (Get(environment, "SUBTITLE_ENGINE") is { Length: > 0 } explicitPath)
            return new EngineCommand(explicitPath);
        if (BundledEngine(baseDirectory) is { } bundled)
            return new EngineCommand(bundled);
        if (FindRepoEngine(baseDirectory) is not { } repoEngine) return null;
        if (UvCandidates(environment).FirstOrDefault(File.Exists) is not { } uv) return null;
        return new EngineCommand(uv, ["run", "--quiet", "--project", repoEngine, "subtitle-engine"]);
    }

    /// <summary>The nearest ancestor of <paramref name="start"/> (inclusive) that has <c>engine\pyproject.toml</c>; returns that engine directory.</summary>
    internal static string? FindRepoEngine(string start)
    {
        for (var dir = new DirectoryInfo(Path.GetFullPath(start)); dir is not null; dir = dir.Parent)
        {
            var engine = Path.Combine(dir.FullName, "engine");
            if (File.Exists(Path.Combine(engine, "pyproject.toml"))) return engine;
        }
        return null;
    }

    /// <summary>
    /// Where uv may live, in lookup order: PATH entries, <c>%USERPROFILE%\.local\bin</c> (the official installer),
    /// <c>%USERPROFILE%\.cargo\bin</c>, then <c>%APPDATA%\Python\Python3*\Scripts</c> (<c>pip install --user uv</c>, newest first).
    /// </summary>
    internal static IEnumerable<string> UvCandidates(IReadOnlyDictionary<string, string?> environment)
    {
        foreach (var dir in (Get(environment, "PATH") ?? "").Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries))
        {
            var trimmed = dir.Trim().Trim('"');
            if (trimmed.Length > 0) yield return Path.Combine(trimmed, "uv.exe");
        }
        if (Get(environment, "USERPROFILE") is { Length: > 0 } home)
        {
            yield return Path.Combine(home, ".local", "bin", "uv.exe");
            yield return Path.Combine(home, ".cargo", "bin", "uv.exe");
        }
        if (Get(environment, "APPDATA") is { Length: > 0 } appData)
        {
            var python = Path.Combine(appData, "Python");
            string[] versions;
            try
            {
                versions = Directory.Exists(python) ? Directory.GetDirectories(python, "Python3*") : [];
            }
            catch (IOException)
            {
                versions = [];
            }
            catch (UnauthorizedAccessException)
            {
                versions = [];
            }
            // Python313 before Python312; numeric so that Python310 sorts after Python39.
            foreach (var version in versions.OrderByDescending(VersionKey))
                yield return Path.Combine(version, "Scripts", "uv.exe");
        }
    }

    private static int VersionKey(string directory) =>
        int.TryParse(Path.GetFileName(directory).AsSpan("Python".Length), out var n) ? n : 0;

    /// <summary>Case-insensitive lookup, as Windows environment variable names are.</summary>
    private static string? Get(IReadOnlyDictionary<string, string?> environment, string name)
    {
        if (environment.TryGetValue(name, out var value)) return value;
        foreach (var pair in environment)
            if (string.Equals(pair.Key, name, StringComparison.OrdinalIgnoreCase)) return pair.Value;
        return null;
    }

    internal static Dictionary<string, string?> ProcessEnvironment()
    {
        var result = new Dictionary<string, string?>(StringComparer.OrdinalIgnoreCase);
        foreach (System.Collections.DictionaryEntry entry in Environment.GetEnvironmentVariables())
            result[(string)entry.Key] = entry.Value as string;
        return result;
    }
}

/// <summary>Kinds of <see cref="EngineException"/>.</summary>
public enum EngineError
{
    /// <summary>No engine could be located.</summary>
    NotFound,

    /// <summary>The engine reported an error or failed; the message says why.</summary>
    Failed,
}

/// <summary>
/// An engine failure. <see cref="Exception.Message"/> is user-facing: the engine's own (localized) message for an
/// <c>error</c> event, or a localized EngineKit message (exit code + log tail, start failure, not found).
/// </summary>
public sealed class EngineException : Exception
{
    public EngineException(EngineError error, string message) : base(message) => Error = error;

    public EngineError Error { get; }

    /// <summary>The engine could not be located (message in <paramref name="language"/>, default the current UI language).</summary>
    public static EngineException NotFound(UILanguage? language = null) =>
        new(EngineError.NotFound, EngineStrings.Get("Altyazı motoru bulunamadı (uv kurulu mu, ya da SUBTITLE_ENGINE ayarlı mı?).", language));

    public static EngineException Failed(string message) => new(EngineError.Failed, message);
}
