using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;

namespace SubtitleAdder.EngineKit;

/// <summary>Options of <see cref="EngineClient.Transcribe"/>.</summary>
public sealed record TranscribeOptions
{
    /// <summary>ISO 639-1 code; <c>null</c> lets the model detect it.</summary>
    public string? Language { get; init; }

    public string Model { get; init; } = "small";

    public IReadOnlyList<SubtitleFormat> Formats { get; init; } = [SubtitleFormat.Srt];

    /// <summary>Separate speakers (<c>--diarize</c>).</summary>
    public bool Diarize { get; init; }

    /// <summary>Known speaker count (<c>--speakers</c>); <c>null</c> means automatic. Only used with <see cref="Diarize"/>.</summary>
    public int? Speakers { get; init; }
}

/// <summary>Runs the engine as a subprocess and speaks protocol v1 (JSON line events on stdout).</summary>
public sealed class EngineClient : IEngineClient
{
    private static readonly UTF8Encoding Utf8 = new(encoderShouldEmitUTF8Identifier: false);

    public EngineClient(EngineCommand command, UILanguage? language = null)
    {
        Command = command;
        Language = language;
    }

    public EngineCommand Command { get; set; }

    /// <summary>
    /// Language passed to the engine as <c>SUBTITLE_ENGINE_LANG</c> (its error messages follow it).
    /// <c>null</c> follows the app's current UI language (<see cref="LanguageState.Shared"/>) at each launch.
    /// </summary>
    public UILanguage? Language { get; set; }

    /// <summary>
    /// Strict offline mode passed to the engine as <c>SUBTITLE_ENGINE_OFFLINE</c> (no network, not even for downloads).
    /// <c>null</c> follows <see cref="OfflineState.Shared"/> at each launch.
    /// </summary>
    public bool? Offline { get; set; }

    /// <summary>Path of the temporary stderr log for each launch; replaceable by tests.</summary>
    internal Func<string> LogPathFactory { get; set; } =
        () => Path.Combine(Path.GetTempPath(), $"subtitle-engine-{Guid.NewGuid()}.log");

    /// <summary>
    /// Environment for the engine process: <paramref name="baseEnvironment"/> (the parent's) plus <c>SUBTITLE_ENGINE_LANG</c>
    /// and <c>SUBTITLE_ENGINE_OFFLINE</c>.
    /// </summary>
    internal static Dictionary<string, string?> ProcessEnvironment(
        UILanguage language, bool offline, IReadOnlyDictionary<string, string?>? baseEnvironment = null)
    {
        var env = new Dictionary<string, string?>(baseEnvironment ?? EngineCommand.ProcessEnvironment(), StringComparer.OrdinalIgnoreCase)
        {
            ["SUBTITLE_ENGINE_LANG"] = language.Code(),
            ["SUBTITLE_ENGINE_OFFLINE"] = offline ? "1" : "0",
        };
        return env;
    }

    /// <summary>The explicit offline setting, or the app's current one when none was set.</summary>
    internal bool ResolvedOffline() => Offline ?? OfflineState.Shared.Enabled;

    /// <summary>The explicit language, or the app's current UI language (<paramref name="current"/>) when none was set.</summary>
    internal UILanguage ResolvedLanguage(UILanguage? current = null) => Language ?? current ?? LanguageState.Shared.Language;

    /// <summary>Arguments <see cref="Transcribe"/> passes; exposed for tests.</summary>
    internal static List<string> TranscribeArguments(string video, string outDir, TranscribeOptions options)
    {
        var args = new List<string> { "transcribe", video, "--out-dir", outDir, "--model", options.Model, "--json", "--format" };
        args.AddRange(options.Formats.Select(f => f.RawValue()));
        if (options.Language is { } language) args.AddRange(["--language", language]);
        if (options.Diarize)
        {
            args.Add("--diarize");
            if (options.Speakers is { } speakers) args.AddRange(["--speakers", speakers.ToString(System.Globalization.CultureInfo.InvariantCulture)]);
        }
        return args;
    }

    /// <summary>
    /// Processes the video; yields stage events and finally <see cref="EngineEvent.Result"/>. An engine <c>error</c> event
    /// is not yielded: it ends the stream with an <see cref="EngineException"/> carrying the engine's message.
    /// Cancelling <paramref name="cancellationToken"/> (or abandoning the enumeration) kills the engine process tree.
    /// </summary>
    public IAsyncEnumerable<EngineEvent> Transcribe(
        string video, string outDir, TranscribeOptions? options = null, CancellationToken cancellationToken = default) =>
        Stream(TranscribeArguments(video, outDir, options ?? new TranscribeOptions()), null, cancellationToken);

    /// <summary>
    /// Translates the sentences to <paramref name="to"/> with a local translation model; yields stage events and finally
    /// <see cref="EngineEvent.Translation"/>. If <paramref name="from"/> is <c>null</c>, the model detects the source
    /// itself (NLLB does not accept that).
    /// </summary>
    public IAsyncEnumerable<EngineEvent> Translate(
        IReadOnlyList<Sentence> sentences, string to, string? from = null, string model = "madlad-3b",
        CancellationToken cancellationToken = default)
    {
        var args = new List<string> { "translate", "--to", to, "--model", model, "--json" };
        if (from is not null) args.AddRange(["--from", from]);
        return Stream(args, Encode(sentences), cancellationToken);
    }

    /// <summary>Shared process stream: decodes the JSON events on stdout and throws the error at the end of the stream.</summary>
    private async IAsyncEnumerable<EngineEvent> Stream(
        List<string> arguments, byte[]? input, [EnumeratorCancellation] CancellationToken cancellationToken)
    {
        // On every exit path (error, invalid line, cancel, abandoned enumeration) the process tree is killed and the log deleted.
        await using var run = Start(arguments, input, cancellationToken);
        string? engineError = null;
        while (true)
        {
            string? line;
            try
            {
                line = await run.Stdout.ReadLineAsync(CancellationToken.None).ConfigureAwait(false);
            }
            catch (Exception) when (cancellationToken.IsCancellationRequested)
            {
                break;
            }
            if (line is null) break;
            if (line.Length == 0) continue;
            cancellationToken.ThrowIfCancellationRequested();
            var engineEvent = EngineEvent.Decode(line);
            if (engineEvent is EngineEvent.Error error)
                engineError = error.Message;
            else
                yield return engineEvent;
        }
        var status = await run.ExitCodeAsync().ConfigureAwait(false);
        cancellationToken.ThrowIfCancellationRequested();
        if (engineError is not null) throw EngineException.Failed(engineError);
        if (status != 0) throw await run.FailureAsync(status).ConfigureAwait(false);
    }

    /// <summary>
    /// Starts the engine on the thread pool: resolving it, opening the log and <c>Process.Start</c> can take hundreds of
    /// milliseconds (antivirus), and these methods are called from the UI thread.
    /// </summary>
    private Task<RunningEngine> StartOffThread(List<string> arguments, byte[]? input, CancellationToken cancellationToken) =>
        Task.Run(() => Start(arguments, input, cancellationToken), CancellationToken.None);

    /// <summary>Sends the sentences (possibly edited in the app) to the engine and gets text in the requested format.</summary>
    public async Task<string> RenderAsync(
        IReadOnlyList<Sentence> sentences, SubtitleFormat format, CancellationToken cancellationToken = default)
    {
        await using var run = await StartOffThread(["render", "--format", format.RawValue()], Encode(sentences), cancellationToken).ConfigureAwait(false);
        var output = await ReadToEndAsync(run, cancellationToken).ConfigureAwait(false);
        var status = await run.ExitCodeAsync().ConfigureAwait(false);
        cancellationToken.ThrowIfCancellationRequested();
        if (status != 0) throw await run.FailureAsync(status).ConfigureAwait(false);
        return output;
    }

    /// <summary>Gets the model catalog and local download status (<c>models --json</c>, a single <c>models</c> event line).</summary>
    public async Task<IReadOnlyList<EngineModel>> ModelsAsync(CancellationToken cancellationToken = default)
    {
        await using var run = await StartOffThread(["models", "--json"], null, cancellationToken).ConfigureAwait(false);
        var line = LastLine(await ReadToEndAsync(run, cancellationToken).ConfigureAwait(false));
        var status = await run.ExitCodeAsync().ConfigureAwait(false);
        cancellationToken.ThrowIfCancellationRequested();
        using var payload = TryParse(line);
        var root = payload?.RootElement;
        var eventName = StringProperty(root, "event");
        if (eventName == "error" && StringProperty(root, "message") is { } message) throw EngineException.Failed(message);
        if (status != 0) throw await run.FailureAsync(status).ConfigureAwait(false);
        List<EngineModel>? models = null;
        if (eventName == "models" && root!.Value.TryGetProperty("models", out var array) && array.ValueKind == JsonValueKind.Array)
        {
            try
            {
                models = array.Deserialize<List<EngineModel>>(EngineJson.Options);
            }
            catch (JsonException)
            {
                models = null;
            }
        }
        return models ?? throw EngineException.Failed(EngineStrings.Get("Motor model listesi yazmadı.", run.Language));
    }

    /// <summary>Deletes a downloaded model from the cache and returns the freed MB (<c>delete-model &lt;id&gt; --json</c>).</summary>
    public async Task<int> DeleteModelAsync(string id, CancellationToken cancellationToken = default)
    {
        await using var run = await StartOffThread(["delete-model", id, "--json"], null, cancellationToken).ConfigureAwait(false);
        var line = LastLine(await ReadToEndAsync(run, cancellationToken).ConfigureAwait(false));
        var status = await run.ExitCodeAsync().ConfigureAwait(false);
        cancellationToken.ThrowIfCancellationRequested();
        using var payload = TryParse(line);
        var root = payload?.RootElement;
        var eventName = StringProperty(root, "event");
        if (eventName == "error" && StringProperty(root, "message") is { } message) throw EngineException.Failed(message);
        if (status != 0) throw await run.FailureAsync(status).ConfigureAwait(false);
        if (eventName == "deleted" && StringProperty(root, "id") == id
            && root!.Value.TryGetProperty("freed_mb", out var freed) && freed.ValueKind == JsonValueKind.Number
            && freed.TryGetInt32(out var mb))
            return mb;
        throw EngineException.Failed(EngineStrings.Get("Motor silme sonucunu yazmadı.", run.Language));
    }

    private static byte[] Encode(IReadOnlyList<Sentence> sentences) =>
        JsonSerializer.SerializeToUtf8Bytes(sentences, EngineJson.Options);

    private static async Task<string> ReadToEndAsync(RunningEngine run, CancellationToken cancellationToken)
    {
        try
        {
            return await run.Stdout.ReadToEndAsync(CancellationToken.None).ConfigureAwait(false);
        }
        catch (Exception) when (cancellationToken.IsCancellationRequested)
        {
            throw new OperationCanceledException(cancellationToken);
        }
    }

    private static string? LastLine(string output) =>
        output.Split('\n').Select(l => l.TrimEnd('\r')).LastOrDefault(l => l.Length > 0);

    private static JsonDocument? TryParse(string? line)
    {
        if (line is null) return null;
        try
        {
            var document = JsonDocument.Parse(line);
            if (document.RootElement.ValueKind == JsonValueKind.Object) return document;
            document.Dispose();
            return null;
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static string? StringProperty(JsonElement? element, string name) =>
        element is { } e && e.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString()
            : null;

    private RunningEngine Start(List<string> arguments, byte[]? input, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var language = ResolvedLanguage();
        var info = new ProcessStartInfo(Command.Executable)
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = Utf8,
            StandardErrorEncoding = Utf8,
            StandardInputEncoding = Utf8,
        };
        foreach (var argument in Command.Arguments) info.ArgumentList.Add(argument);
        foreach (var argument in arguments) info.ArgumentList.Add(argument);
        info.Environment["SUBTITLE_ENGINE_LANG"] = language.Code();
        info.Environment["SUBTITLE_ENGINE_OFFLINE"] = ResolvedOffline() ? "1" : "0";

        // stderr goes to a file (drained continuously), so model download progress can't fill the pipe and hang the engine.
        var logPath = LogPathFactory();
        var log = new FileStream(logPath, FileMode.Create, FileAccess.Write, FileShare.ReadWrite | FileShare.Delete);
        var process = new Process { StartInfo = info };
        try
        {
            process.Start();
        }
        catch (Exception error) when (error is Win32Exception or InvalidOperationException or FileNotFoundException)
        {
            log.Dispose();
            process.Dispose();
            TryDelete(logPath);
            throw EngineException.Failed(EngineStrings.Get("Motor başlatılamadı: %@", language, error.Message));
        }
        var run = new RunningEngine(process, logPath, log, language);
        run.WriteInput(input);
        run.KillOn(cancellationToken);
        return run;
    }

    internal static void TryDelete(string path)
    {
        try
        {
            File.Delete(path);
        }
        catch (IOException)
        {
        }
        catch (UnauthorizedAccessException)
        {
        }
    }

    /// <summary>The running engine process. Disposing kills the process tree (if still running) and deletes the log.</summary>
    private sealed class RunningEngine : IAsyncDisposable
    {
        private readonly Process _process;
        private readonly string _logPath;
        private readonly Task _stderrCopy;
        private Task _stdinWrite = Task.CompletedTask;
        private CancellationTokenRegistration _cancellation;

        public RunningEngine(Process process, string logPath, FileStream log, UILanguage language)
        {
            _process = process;
            _logPath = logPath;
            Language = language;
            var stderr = process.StandardError.BaseStream;
            _stderrCopy = Task.Run(async () =>
            {
                await using (log)
                {
                    try
                    {
                        await stderr.CopyToAsync(log).ConfigureAwait(false);
                    }
                    catch (IOException)
                    {
                    }
                    catch (ObjectDisposedException)
                    {
                    }
                }
            });
        }

        public UILanguage Language { get; }

        public StreamReader Stdout => _process.StandardOutput;

        /// <summary>
        /// Writes stdin off the caller's thread: a large payload can block until the engine reads it, and the caller
        /// (often the UI thread) must not wait. If the engine exits before reading, the write fails with a broken pipe
        /// (IOException), which is ignored; the exit code is reported anyway. With no input stdin is closed at once (EOF).
        /// </summary>
        public void WriteInput(byte[]? input)
        {
            var stdin = _process.StandardInput;
            _stdinWrite = Task.Run(async () =>
            {
                try
                {
                    if (input is not null)
                    {
                        await stdin.BaseStream.WriteAsync(input).ConfigureAwait(false);
                        await stdin.BaseStream.FlushAsync().ConfigureAwait(false);
                    }
                }
                catch (IOException)
                {
                }
                catch (ObjectDisposedException)
                {
                }
                finally
                {
                    try
                    {
                        stdin.Close();
                    }
                    catch (IOException)
                    {
                    }
                    catch (ObjectDisposedException)
                    {
                    }
                }
            });
        }

        /// <summary>Reading stdout is not cancellation-aware on anonymous pipes; cancelling kills the tree, which ends the read.</summary>
        public void KillOn(CancellationToken cancellationToken)
        {
            if (cancellationToken.CanBeCanceled) _cancellation = cancellationToken.Register(Terminate);
        }

        public async Task<int> ExitCodeAsync()
        {
            await _process.WaitForExitAsync().ConfigureAwait(false);
            return _process.ExitCode;
        }

        /// <summary>
        /// Kills the engine and all its descendants (<c>uv run</c> spawns a Python child; Windows has no SIGTERM to forward).
        /// </summary>
        public void Terminate()
        {
            try
            {
                if (!_process.HasExited) _process.Kill(entireProcessTree: true);
            }
            catch (InvalidOperationException)
            {
            }
            catch (Win32Exception)
            {
            }
            catch (NotSupportedException)
            {
            }
        }

        public async Task<EngineException> FailureAsync(int status)
        {
            await WaitQuietly(_stderrCopy, TimeSpan.FromSeconds(2)).ConfigureAwait(false);
            var tail = "";
            try
            {
                using var stream = new FileStream(_logPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                using var reader = new StreamReader(stream, Utf8);
                var lines = (await reader.ReadToEndAsync().ConfigureAwait(false))
                    .Split('\n')
                    .Select(l => l.TrimEnd('\r'))
                    .Where(l => l.Length > 0)
                    .ToList();
                tail = string.Join("\n", lines.TakeLast(5));
            }
            catch (IOException)
            {
            }
            catch (UnauthorizedAccessException)
            {
            }
            return EngineException.Failed(EngineStrings.Get("Motor hata koduyla çıktı (%lld). %@", Language, (long)status, tail));
        }

        public async ValueTask DisposeAsync()
        {
            await _cancellation.DisposeAsync().ConfigureAwait(false);
            Terminate();
            await WaitQuietly(_stdinWrite, TimeSpan.FromSeconds(2)).ConfigureAwait(false);
            await WaitQuietly(_stderrCopy, TimeSpan.FromSeconds(2)).ConfigureAwait(false);
            TryDelete(_logPath);
            _process.Dispose();
        }

        private static async Task WaitQuietly(Task task, TimeSpan timeout)
        {
            try
            {
                await task.WaitAsync(timeout).ConfigureAwait(false);
            }
            catch (TimeoutException)
            {
            }
        }
    }
}
