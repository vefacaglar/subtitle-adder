using SubtitleAdder.Core;
using SubtitleAdder.EngineKit;

namespace SubtitleAdder.Core.Tests;

public class SettingsResponsivenessTests
{
    [Fact]
    public async Task TranslationProgressDoesNotRefreshSettingsControls()
    {
        var (vm, engine, _) = Make.ViewModel();
        using var lifetime = vm;
        vm.OpenVideo("settings-progress.mp4");
        vm.AutoSaveSubtitle = false;
        var callbacks = new System.Collections.Concurrent.ConcurrentQueue<Action>();
        var refreshes = 0;
        using var observer = new SettingsRefreshSubscription(vm, callbacks.Enqueue, _ => refreshes++);
        vm.Generate();
        for (var i = 0; i < 100; i++)
            engine.Events.Writer.TryWrite(new EngineEvent.Stage(EngineStage.Translating, i / 100.0));
        await Make.Eventually(() => vm.StatusFraction == 0.99, "last progress update");
        Assert.Empty(callbacks);
        Assert.Equal(0, refreshes);
        vm.Cancel();
    }

    [Fact]
    public void SettingsChangesCoalesceAndQueuedRefreshIsIgnoredAfterClose()
    {
        var (vm, _, _) = Make.ViewModel();
        using var lifetime = vm;
        var callbacks = new Queue<Action>();
        var refreshes = new List<bool>();
        var observer = new SettingsRefreshSubscription(vm, callbacks.Enqueue, refreshes.Add);
        vm.Language = "en";
        vm.Diarize = !vm.Diarize;
        vm.AutoPlayVideo = !vm.AutoPlayVideo;
        observer.RequestRefresh(refill: true);
        Assert.Single(callbacks);
        callbacks.Dequeue()();
        Assert.Equal(new[] { true }, refreshes);

        vm.ExternalLlmAsync = !vm.ExternalLlmAsync;
        Assert.Single(callbacks);
        observer.Dispose();
        callbacks.Dequeue()();
        vm.Language = "de";
        Assert.Empty(callbacks);
        Assert.Single(refreshes);
    }
}
