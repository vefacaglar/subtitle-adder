using SubtitleAdder.Core;

namespace SubtitleAdder.Core.Tests;

/// <summary>Strict offline mode: on by default, passed to every engine launch, and missing models are reported up front.</summary>
public class OfflineModeTests
{
    private static async Task<(SubtitleViewModel Vm, FakeEngineClient Engine, InMemorySettingsStore Settings)> Offline(
        params EngineModel[] models)
    {
        var settings = new InMemorySettingsStore();
        var (vm, engine, _) = Make.ViewModel(settings);
        engine.CatalogModels = models;
        await vm.Catalog.LoadAsync();
        vm.OpenVideo(@"C:\videos\clip.mp4");
        return (vm, engine, settings);
    }

    [Fact]
    public void OnByDefaultAndPersisted()
    {
        var settings = new InMemorySettingsStore();
        var (vm, _, _) = Make.ViewModel(settings);
        Assert.True(vm.OfflineMode);
        Assert.True(OfflineState.Shared.Enabled);

        vm.OfflineMode = false;
        Assert.False(settings.GetBool(SubtitleViewModel.OfflineModeDefaultsKey));
        Assert.False(OfflineState.Shared.Enabled);
        Assert.False(Make.ViewModel(settings).Vm.OfflineMode);
        vm.OfflineMode = true;
    }

    [Fact]
    public async Task MissingModelBlocksGenerateAndGoingOnlineRetries()
    {
        using var _ = new LanguageScope(UILanguage.En);
        var (vm, engine, settings) = await Offline(
            Make.Model("large-v3", downloaded: false), Make.Model("speaker-diarization", downloaded: true, kind: "diarization"));

        vm.Generate();

        Assert.Empty(engine.TranscribeCalls);
        Assert.Equal(JobStatusKind.Failed, vm.StatusKind);
        Assert.Contains("offline mode", vm.StatusText);
        Assert.True(vm.CanGoOnlineAndRetry);

        vm.GoOnlineAndRetry();

        Assert.False(vm.OfflineMode);
        Assert.False(settings.GetBool(SubtitleViewModel.OfflineModeDefaultsKey));
        Assert.Single(engine.TranscribeCalls);
        Assert.True(vm.IsWorking);
        Assert.False(vm.CanGoOnlineAndRetry);
        vm.Cancel();
        vm.OfflineMode = true;
    }

    [Fact]
    public async Task MissingDiarizationModelCountsOnlyWhenDiarizing()
    {
        var (vm, engine, _) = await Offline(
            Make.Model("large-v3", downloaded: true), Make.Model("speaker-diarization", downloaded: false, kind: "diarization"));

        vm.Generate();
        Assert.Empty(engine.TranscribeCalls);
        Assert.True(vm.CanGoOnlineAndRetry);

        vm.Diarize = false;
        vm.Generate();
        Assert.Single(engine.TranscribeCalls);
        Assert.False(vm.CanGoOnlineAndRetry);
        vm.Cancel();
    }

    [Fact]
    public async Task MissingTranslationModelBlocksTranslate()
    {
        var (vm, engine, _) = await Offline(
            Make.Model("large-v3", downloaded: true), Make.Model("speaker-diarization", downloaded: true, kind: "diarization"),
            Make.Model("madlad-3b", downloaded: false, kind: "translation"));
        vm.Generate();
        engine.Finish(Make.Result("tr", "", new Sentence(1, 0, 1, "Merhaba.")));
        await vm.CurrentJob;

        vm.Translate();

        Assert.Empty(engine.TranslateCalls);
        Assert.True(vm.CanGoOnlineAndRetry);
    }

    [Fact]
    public async Task OtherFailuresOfferNoRetry()
    {
        var (vm, engine, _) = await Offline(
            Make.Model("large-v3", downloaded: true), Make.Model("speaker-diarization", downloaded: true, kind: "diarization"));
        vm.Generate();
        engine.Finish(new EngineEvent.Error("boom"));
        await vm.CurrentJob;

        Assert.Equal(JobStatusKind.Failed, vm.StatusKind);
        Assert.False(vm.CanGoOnlineAndRetry);
    }
}
