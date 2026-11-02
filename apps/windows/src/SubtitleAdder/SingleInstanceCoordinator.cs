using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.UI.Xaml;

namespace SubtitleAdder;

/// <summary>
/// Ensures a single running instance of Subtitle Adder per user session. When the user double-clicks a video or
/// subtitle file in Explorer while Subtitle Adder is already open, forwards the file paths over a per-user named pipe
/// to the primary window and brings it to the foreground instead of spawning a duplicate window.
/// </summary>
public sealed class SingleInstanceCoordinator : IDisposable
{
    private static readonly string SafeUserName = SanitizeToken(Environment.UserName);
    private static readonly string MutexName = $@"Local\SubtitleAdder.SingleInstance.{SafeUserName}";
    private static readonly string PipeName = $"SubtitleAdder.SingleInstance.{SafeUserName}";

    private readonly Mutex _mutex;
    private readonly bool _ownsMutex;
    private readonly CancellationTokenSource _cts = new();
    private bool _disposed;

    private SingleInstanceCoordinator(Mutex mutex, bool ownsMutex)
    {
        _mutex = mutex;
        _ownsMutex = ownsMutex;
    }

    public static bool TryAcquireOrForward(
        IReadOnlyList<string> filePaths,
        Action<IReadOnlyList<string>> onFilesReceived,
        out SingleInstanceCoordinator? coordinator)
    {
        coordinator = null;
        Mutex? mutex = null;
        var ownsMutex = false;
        try
        {
            mutex = new Mutex(initiallyOwned: true, MutexName, out var createdNew);
            if (!createdNew)
            {
                try
                {
                    ownsMutex = mutex.WaitOne(0);
                }
                catch (AbandonedMutexException)
                {
                    ownsMutex = true;
                }
            }
            else
            {
                ownsMutex = true;
            }
        }
        catch
        {
            // If mutex creation fails in a restricted sandbox, continue as primary instance.
            return true;
        }

        if (!ownsMutex)
        {
            ForwardToPrimaryInstance(filePaths);
            mutex.Dispose();
            return false;
        }

        coordinator = new SingleInstanceCoordinator(mutex, ownsMutex: true);
        coordinator.StartListening(onFilesReceived);
        return true;
    }

    private static void ForwardToPrimaryInstance(IReadOnlyList<string> filePaths)
    {
        try
        {
            using var client = new NamedPipeClientStream(".", PipeName, PipeDirection.Out);
            client.Connect(timeout: 1500);
            using var writer = new StreamWriter(client, new UTF8Encoding(false)) { AutoFlush = true };
            if (filePaths.Count == 0)
            {
                writer.WriteLine();
            }
            else
            {
                foreach (var path in filePaths)
                {
                    writer.WriteLine(path);
                }
            }
        }
        catch
        {
            // Ignore pipe connection errors if primary instance is closing.
        }
    }

    private void StartListening(Action<IReadOnlyList<string>> onFilesReceived)
    {
        var token = _cts.Token;
        _ = Task.Run(async () =>
        {
            while (!token.IsCancellationRequested)
            {
                try
                {
                    await using var server = new NamedPipeServerStream(
                        PipeName,
                        PipeDirection.In,
                        maxNumberOfServerInstances: 1,
                        PipeTransmissionMode.Byte,
                        PipeOptions.Asynchronous);

                    await server.WaitForConnectionAsync(token).ConfigureAwait(false);
                    using var reader = new StreamReader(server, Encoding.UTF8);
                    var paths = new List<string>();
                    while (await reader.ReadLineAsync(token).ConfigureAwait(false) is { } line)
                    {
                        var trimmed = line.Trim().Trim('"');
                        if (trimmed.Length > 0 && File.Exists(trimmed))
                        {
                            paths.Add(Path.GetFullPath(trimmed));
                        }
                    }
                    onFilesReceived(paths);
                }
                catch (OperationCanceledException)
                {
                    break;
                }
                catch
                {
                    await Task.Delay(200, token).ConfigureAwait(false);
                }
            }
        }, token);
    }

    public static void BringToForeground(Window window)
    {
        try
        {
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
            if (hwnd != IntPtr.Zero)
            {
                if (IsIconic(hwnd))
                {
                    ShowWindow(hwnd, SW_RESTORE);
                }
                SetForegroundWindow(hwnd);
            }
            window.Activate();
        }
        catch
        {
            // Ignore window activation errors.
        }
    }

    private static string SanitizeToken(string value)
    {
        if (string.IsNullOrWhiteSpace(value)) return "Default";
        var chars = value.Select(c => char.IsLetterOrDigit(c) ? c : '_').ToArray();
        return new string(chars);
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        try { _cts.Cancel(); } catch { }
        _cts.Dispose();
        if (_ownsMutex)
        {
            try { _mutex.ReleaseMutex(); } catch { }
        }
        _mutex.Dispose();
    }

    private const int SW_RESTORE = 9;

    [DllImport("user32.dll")]
    private static extern bool IsIconic(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);
}
