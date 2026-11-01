namespace SubtitleAdder.Core;

/// <summary>
/// Runs work on the UI thread. Core never references WinUI: the app passes a
/// <see cref="SynchronizationContextDispatcher"/> created on the UI thread (WinUI installs a
/// DispatcherQueue-backed SynchronizationContext there); tests use <see cref="ImmediateDispatcher"/>.
/// </summary>
public interface IUiDispatcher
{
    /// <summary>True when the caller is already on the UI thread (work can run inline).</summary>
    bool HasThreadAccess { get; }

    /// <summary>Queues <paramref name="action"/> on the UI thread.</summary>
    void Post(Action action);
}

public static class UiDispatcherExtensions
{
    /// <summary>Runs <paramref name="action"/> on the UI thread and completes when it has run.</summary>
    public static Task InvokeAsync(this IUiDispatcher dispatcher, Action action)
    {
        if (dispatcher.HasThreadAccess)
        {
            action();
            return Task.CompletedTask;
        }
        var done = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        dispatcher.Post(() =>
        {
            try
            {
                action();
                done.SetResult();
            }
            catch (Exception error)
            {
                done.SetException(error);
            }
        });
        return done.Task;
    }

    /// <summary>Starts <paramref name="work"/> on the UI thread and completes when it has finished.</summary>
    public static Task InvokeAsync(this IUiDispatcher dispatcher, Func<Task> work)
    {
        if (dispatcher.HasThreadAccess) return work();
        var done = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        dispatcher.Post(async () =>
        {
            try
            {
                await work();
                done.SetResult();
            }
            catch (Exception error)
            {
                done.SetException(error);
            }
        });
        return done.Task;
    }
}

/// <summary>Posts to the SynchronizationContext captured at construction (the UI thread's).</summary>
public sealed class SynchronizationContextDispatcher : IUiDispatcher
{
    private readonly SynchronizationContext _context;
    private readonly int _threadId;

    /// <summary>Must be created on the UI thread.</summary>
    public SynchronizationContextDispatcher()
    {
        _context = SynchronizationContext.Current
            ?? throw new InvalidOperationException("No SynchronizationContext on this thread; create the dispatcher on the UI thread.");
        _threadId = Environment.CurrentManagedThreadId;
    }

    public bool HasThreadAccess => Environment.CurrentManagedThreadId == _threadId;

    public void Post(Action action) => _context.Post(_ => action(), null);
}

/// <summary>Runs everything inline on the calling thread (tests, design time).</summary>
public sealed class ImmediateDispatcher : IUiDispatcher
{
    public static ImmediateDispatcher Instance { get; } = new();

    public bool HasThreadAccess => true;

    public void Post(Action action) => action();
}
