using System.Text.RegularExpressions;
using System.Threading.Channels;

namespace SubtitleAdder.Core;

/// <summary>
/// Non-blocking, thread-safe diagnostic logger that appends timestamped trace lines to
/// <c>%TEMP%\SubtitleAdder-diagnostics.log</c> on a single background consumer so neither the UI thread
/// nor worker pipelines ever block on log I/O.
/// <para>
/// The log is plaintext and readable without any profile key, so it never holds file paths: every absolute
/// path in a message is replaced by <see cref="RedactedPath"/>, and lines written by earlier versions (which logged
/// the paths of opened, failed and transcribed videos) are scrubbed the same way before the first new line.
/// </para>
/// </summary>
public static partial class DiagnosticLog
{
    private const long MaxLogBytes = 2L * 1024L * 1024L;

    /// <summary>Placeholder written instead of a file path.</summary>
    public const string RedactedPath = "<path>";

    private static readonly Channel<string> Queue = Channel.CreateBounded<string>(new BoundedChannelOptions(1024)
    {
        SingleReader = true,
        SingleWriter = false,
        FullMode = BoundedChannelFullMode.DropOldest,
    });

    public static string LogFilePath { get; } = Path.Combine(Path.GetTempPath(), "SubtitleAdder-diagnostics.log");

    /// <summary>Player failure log written by earlier versions with the full path of every video that failed to open.</summary>
    public static string LegacyPlayerLogPath { get; } = Path.Combine(Path.GetTempPath(), "SubtitleAdder-player.log");

    /// <summary>Unhandled-exception log written by the WinUI app; scrubbed like the diagnostics log.</summary>
    public static string CrashLogPath { get; } = Path.Combine(Path.GetTempPath(), "SubtitleAdder-crash.log");

    static DiagnosticLog()
    {
        _ = Task.Run(ConsumeLoopAsync);
    }

    /// <summary>Starts the background writer, which first scrubs paths from logs written by earlier versions.</summary>
    public static void Initialize()
    {
        // Touching the type runs the static constructor; nothing else to do.
    }

    public static void Write(string category, string message)
    {
        var line = $"{DateTimeOffset.Now:O} [T{Environment.CurrentManagedThreadId}] [{category}] {Redact(message)}{Environment.NewLine}";
        Queue.Writer.TryWrite(line);
    }

    /// <summary>
    /// Replaces absolute Windows paths (<c>C:\...</c>, <c>C:/...</c>, <c>\\server\...</c>) and <c>file:</c> URIs in
    /// <paramref name="message"/> with <see cref="RedactedPath"/>. A path runs until a quote, a line break or the end.
    /// </summary>
    public static string Redact(string? message)
    {
        if (string.IsNullOrEmpty(message)) return message ?? "";
        return PathPattern().Replace(message, RedactedPath);
    }

    [GeneratedRegex(@"(?:file:/{2,3}|\b[A-Za-z]:[\\/]|\\\\[^\\\s'""]+\\)[^'""\r\n]*", RegexOptions.CultureInvariant)]
    private static partial Regex PathPattern();

    // Serializes crash-log appends with the scrub, so an entry appended while the scrub runs is never overwritten.
    private static readonly Lock CrashLogLock = new();

    /// <summary>
    /// Appends <paramref name="text"/> (already redacted) to <see cref="CrashLogPath"/>; returns <c>false</c> when the
    /// file could not be written. Safe to call from any thread, including while the startup scrub runs.
    /// </summary>
    public static bool AppendCrash(string text)
    {
        try
        {
            lock (CrashLogLock)
            {
                File.AppendAllText(CrashLogPath, text);
            }
            return true;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    /// <summary>Rewrites the existing log files without paths and deletes the legacy player log.</summary>
    internal static void ScrubExistingLogs()
    {
        try
        {
            if (File.Exists(LegacyPlayerLogPath)) File.Delete(LegacyPlayerLogPath);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
        }

        foreach (var path in new[] { LogFilePath, LogFilePath + ".old", CrashLogPath })
        {
            lock (CrashLogLock)
            {
                ScrubLogFile(path);
            }
        }
    }

    // Writes the scrubbed text to a temp file and swaps it in with an atomic rename, only if the file did not grow
    // meanwhile (another process appending). On any I/O failure the original is left in place, never deleted, and
    // the scrub is retried at the next start.
    private static void ScrubLogFile(string path)
    {
        for (var attempt = 0; attempt < 3; attempt++)
        {
            var tempPath = path + "." + Environment.ProcessId.ToString(System.Globalization.CultureInfo.InvariantCulture) + ".scrub.tmp";
            try
            {
                if (!File.Exists(path)) return;
                string text;
                long length;
                using (var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete))
                using (var reader = new StreamReader(fs))
                {
                    length = fs.Length;
                    text = reader.ReadToEnd();
                }
                var scrubbed = string.Join('\n', text.Split('\n').Select(Redact));
                if (string.Equals(text, scrubbed, StringComparison.Ordinal)) return;

                File.WriteAllText(tempPath, scrubbed);
                if (new FileInfo(path).Length != length)
                {
                    File.Delete(tempPath);
                    continue;
                }
                File.Move(tempPath, path, overwrite: true);
                return;
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                try { if (File.Exists(tempPath)) File.Delete(tempPath); } catch { }
                return;
            }
        }
    }

    private static async Task ConsumeLoopAsync()
    {
        // Runs on the single consumer before any new line is appended, so it never races a write.
        ScrubExistingLogs();
        try
        {
            while (await Queue.Reader.WaitToReadAsync().ConfigureAwait(false))
            {
                try
                {
                    var info = new FileInfo(LogFilePath);
                    if (info.Exists && info.Length > MaxLogBytes)
                    {
                        var backupPath = LogFilePath + ".old";
                        File.Move(LogFilePath, backupPath, overwrite: true);
                    }
                }
                catch
                {
                }

                try
                {
                    await using var stream = new FileStream(
                        LogFilePath,
                        FileMode.Append,
                        FileAccess.Write,
                        FileShare.ReadWrite,
                        bufferSize: 4096,
                        useAsync: true);
                    await using var writer = new StreamWriter(stream);
                    while (Queue.Reader.TryRead(out var entry))
                    {
                        await writer.WriteAsync(entry).ConfigureAwait(false);
                    }
                }
                catch
                {
                    // Drain queued items if disk is temporarily locked so we don't spin.
                    while (Queue.Reader.TryRead(out _))
                    {
                    }
                }
            }
        }
        catch
        {
        }
    }
}
