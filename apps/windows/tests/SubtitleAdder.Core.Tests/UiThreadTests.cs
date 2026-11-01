using System.Collections.Concurrent;
using System.ComponentModel;

namespace SubtitleAdder.Core.Tests;

/// <summary>
/// A single-threaded SynchronizationContext pump, like the WinUI dispatcher: posted work runs on one dedicated thread,
/// so a state change made from the thread pool shows up as a wrong-thread notification.
/// </summary>
internal sealed class UiThread : SynchronizationContext, IDisposable
{
    private readonly BlockingCollection<(SendOrPostCallback Callback, object? State)> _queue = new();
    private readonly Thread _thread;

    public UiThread()
    {
        _thread = new Thread(() =>
        {
            SetSynchronizationContext(this);
            foreach (var (callback, state) in _queue.GetConsumingEnumerable()) callback(state);
        }) { IsBackground = true, Name = "test UI thread" };
        _thread.Start();
    }

    public int ThreadId => _thread.ManagedThreadId;

    /// <summary>Work posted after disposal is dropped, like a closed window's dispatcher.</summary>
    public override void Post(SendOrPostCallback d, object? state)
    {
        if (!_queue.IsAddingCompleted) _queue.TryAdd((d, state));
    }

    /// <summary>Runs <paramref name="body"/> on the UI thread; its awaits resume there too.</summary>
    public Task Run(Func<Task> body)
    {
        var done = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        Post(async _ =>
        {
            try
            {
                await body();
                done.SetResult();
            }
            catch (Exception error)
            {
                done.SetException(error);
            }
        }, null);
        return done.Task.WaitAsync(TimeSpan.FromSeconds(30));
    }

    public void Dispose() => _queue.CompleteAdding();
}

public class UiThreadTests
{
    /// <summary>Builds a view model on the UI thread and records every notification raised on another thread.</summary>
    private static (SubtitleViewModel Vm, FakeEngineClient Engine, ConcurrentQueue<string> Violations) Make(UiThread ui)
    {
        var engine = new FakeEngineClient();
        var catalog = new ModelCatalog(engine);
        var vm = new SubtitleViewModel(catalog, new InMemorySettingsStore(), new SynchronizationContextDispatcher(), new LanguageState());
        var violations = new ConcurrentQueue<string>();
        PropertyChangedEventHandler check(string owner) => (_, e) =>
        {
            if (Environment.CurrentManagedThreadId != ui.ThreadId) violations.Enqueue($"{owner}.{e.PropertyName}");
        };
        vm.PropertyChanged += check("vm");
        catalog.PropertyChanged += check("catalog");
        return (vm, engine, violations);
    }

    private static async Task Settle()
    {
        // Lets the job's last posted callbacks (finally block, catalog reload) run on the pump.
        for (var i = 0; i < 5; i++) await Task.Delay(20);
    }

    [Fact]
    public async Task GenerateTranslateAndCatalogReloadNotifyOnTheUiThread()
    {
        using var ui = new UiThread();
        ConcurrentQueue<string>? violations = null;
        await ui.Run(async () =>
        {
            var (vm, engine, found) = Make(ui);
            using var _ = vm;
            violations = found;
            vm.OpenVideo("a.mp4");
            vm.Language = "tr";
            vm.Generate();
            engine.Events.Writer.TryWrite(new EngineEvent.Stage(EngineStage.Transcribing, 0.5));
            await Core.Tests.Make.Eventually(() => vm.StatusFraction == 0.5, "stage");
            engine.Finish(Core.Tests.Make.Result(new Sentence(1, 0, 1, "Bir."), new Sentence(2, 1, 2, "İki.")));
            await vm.CurrentJob;
            await Settle();
            Assert.Equal(JobStatusKind.Finished, vm.StatusKind);
            Assert.Equal(1, engine.ModelsCalls);

            engine.Reset();
            vm.Translate();
            var request = engine.TranslateCalls[^1].Sentences;
            engine.Finish(new EngineEvent.Translation("en", [.. request.Select(s => new Sentence(s.Id, s.Start, s.End, "T" + s.Id))]));
            await vm.CurrentJob;
            await Settle();
            Assert.Equal("T1", vm.TranslationFor(vm.Sentences[0])?.Text);
        });
        Assert.Empty(violations!);
    }

    [Fact]
    public async Task CancelAndStaleJobsNotifyOnTheUiThread()
    {
        using var ui = new UiThread();
        ConcurrentQueue<string>? violations = null;
        await ui.Run(async () =>
        {
            var (vm, engine, found) = Make(ui);
            using var _ = vm;
            violations = found;
            vm.OpenVideo("a.mp4");
            vm.Generate();
            var cancelled = vm.CurrentJob;
            vm.Cancel();
            await engine.StreamCancelled.Task.WaitAsync(TimeSpan.FromSeconds(5));
            engine.Finish(Core.Tests.Make.Result(new Sentence(1, 0, 1, "late")));
            await cancelled;

            engine.Reset();
            vm.Generate();
            var oldEvents = engine.Events;
            var oldJob = vm.CurrentJob;
            engine.Reset();
            vm.Generate();
            oldEvents.Writer.TryWrite(Core.Tests.Make.Result(new Sentence(1, 0, 1, "old")));
            oldEvents.Writer.TryComplete();
            await oldJob;
            Assert.True(vm.IsWorking);
            engine.Finish(Core.Tests.Make.Result(new Sentence(1, 0, 1, "new")));
            await vm.CurrentJob;
            await Settle();
            Assert.Equal("new", Assert.Single(vm.Sentences).Text);
        });
        Assert.Empty(violations!);
    }
}
