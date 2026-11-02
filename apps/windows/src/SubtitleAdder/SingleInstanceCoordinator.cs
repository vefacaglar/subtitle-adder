using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.UI.Xaml;

namespace SubtitleAdder;

/// <summary>
/// Coordinates multiple instances of Subtitle Adder.
/// - When the user launches Subtitle Adder directly with NO file arguments (e.g. Start Menu, shortcut, or a second window),
///   allows the second instance to open normally alongside the first instance without crashing.
/// - When the user double-clicks a media/subtitle file in Explorer (<c>filePaths.Count &gt; 0</c>), forwards the file paths
///   to the primary instance if responsive; if forwarding fails, opens the file in the new instance as a safe fallback.
/// </summary>
public sealed class SingleInstanceCoordinator : IDisposable
{
    private static readonly string SafeUserName = SanitizeToken(Environment.UserName);
    private static readonly string MutexName = $@"Local\SubtitleAdder.SingleInstance.{SafeUserName}";
    private static readonly string PipeName = $"SubtitleAdder.SingleInstance.{SafeUserName}";

    private readonly Mutex? _mutex;
    private readonly bool _ownsMutex;
    private readonly CancellationTokenSource _cts = new();
    private bool _disposed;

    private SingleInstanceCoordinator(Mutex? mutex, bool ownsMutex)
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
            // If mutex creation fails in a restricted sandbox, continue as an independent instance.
            try { mutex?.Dispose(); } catch { }
            return true;
        }

        if (!ownsMutex)
        {
            try { mutex.Dispose(); } catch { }

            // If the user explicitly launched a 2nd SubtitleAdder window with no file arguments,
            // allow the 2nd instance to run concurrently alongside the first instance!
            if (filePaths.Count == 0)
            {
                return true;
            }

            // If launched with file arguments (e.g. Explorer double-click), try forwarding to the primary instance.
            // If the primary instance is busy or unreachable, open in this new instance instead of failing.
            if (ForwardToPrimaryInstance(filePaths))
            {
                return false;
            }
            return true;
        }

        coordinator = new SingleInstanceCoordinator(mutex, ownsMutex: true);
        coordinator.StartListening(onFilesReceived);
        return true;
    }

    private static bool ForwardToPrimaryInstance(IReadOnlyList<string> filePaths)
    {
        try
        {
            using var client = new NamedPipeClientStream(".", PipeName, PipeDirection.Out);
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
                        maxNumberOfServerInstances: NamedPipeServerStream.MaxAllowedServerInstances,
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
                    try
                    {
                        await Task.Delay(200, token).ConfigureAwait(false);
                    }
                    catch (OperationCanceledException)
                    {
                        break;
                    }
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

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        try { _cts.Cancel(); } catch { }
        try { _cts.Dispose(); } catch { }
        if (_ownsMutex && _mutex is not null)
        {
            try
            {
                _mutex.ReleaseMutex();
            }
            catch (ApplicationException)
            {
                // ReleaseMutex can throw if called from a different thread than the one that acquired it.
            }
        }
        try { _mutex?.Dispose(); } catch { }
    }

    private static string SanitizeToken(string raw)
    {
        if (string.IsNullOrWhiteSpace(raw)) return "default";
        var chars = raw.Select(c => char.IsLetterOrDigit(c) ? c : '_').ToArray();
        return new string(chars);
    }

    private const int SW_RESTORE = 9;

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    private static extern bool IsIconic(IntPtr hWnd);
}
