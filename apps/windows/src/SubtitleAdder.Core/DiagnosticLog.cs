using System.Threading.Channels;

namespace SubtitleAdder.Core;

/// <summary>
/// Non-blocking, thread-safe diagnostic logger that appends timestamped trace lines to
/// <c>%TEMP%\SubtitleAdder-diagnostics.log</c> on a single background consumer so neither the UI thread
/// nor worker pipelines ever block on log I/O.
/// </summary>
public static class DiagnosticLog
{
    private const long MaxLogBytes = 2L * 1024L * 1024L;

    private static readonly Channel<string> Queue = Channel.CreateBounded<string>(new BoundedChannelOptions(1024)
    {
        SingleReader = true,
        SingleWriter = false,
        FullMode = BoundedChannelFullMode.DropOldest,
    });

    public static string LogFilePath { get; } = Path.Combine(Path.GetTempPath(), "SubtitleAdder-diagnostics.log");

    static DiagnosticLog()
    {
        _ = Task.Run(ConsumeLoopAsync);
    }

    public static void Write(string category, string message)
    {
        var line = $"{DateTimeOffset.Now:O} [T{Environment.CurrentManagedThreadId}] [{category}] {message}{Environment.NewLine}";
        Queue.Writer.TryWrite(line);
    }

    private static async Task ConsumeLoopAsync()
    {
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
