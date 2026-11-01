using SubtitleAdder.Core;

namespace SubtitleAdder.Core.Tests;

/// <summary>Persisted choices: same keys and defaults as the macOS UserDefaults, and the reconcile rules.</summary>
public class ViewModelSettingsTests
{
    [Fact]
    public void DefaultsMatchMacOS()
    {
        var (vm, _, _) = Make.ViewModel();
        Assert.Equal("", vm.Language);
        Assert.Equal("large-v3", vm.Model);
        Assert.Equal("en", vm.TranslationTarget);
        Assert.Equal("madlad-3b", vm.TranslationModel);
        Assert.Equal(SubtitleDisplay.Original, vm.DisplayMode);
        Assert.True(vm.Diarize);
        Assert.Equal(0, vm.SpeakerCount);
        Assert.Null(vm.ModelNotice);
    }

    [Fact]
    public void ChoicesArePersistedUnderTheMacOSKeys()
    {
        var (vm, _, settings) = Make.ViewModel();
        vm.Language = "tr";
        vm.Model = "small";
        vm.TranslationTarget = "es";
        vm.TranslationModel = "nllb-1.3b";
        vm.DisplayMode = SubtitleDisplay.Both;
        vm.Diarize = false;
        vm.SpeakerCount = 3;
        Assert.Equal("tr", settings.GetString("selectedLanguage"));
        Assert.Equal("small", settings.GetString("selectedModel"));
        Assert.Equal("es", settings.GetString("translationTarget"));
        Assert.Equal("nllb-1.3b", settings.GetString("translationModel"));
        Assert.Equal("both", settings.GetString("subtitleDisplayMode"));
        Assert.False(settings.GetBool("diarizeSpeakers"));
        Assert.Equal(3, settings.GetInt("speakerCount"));

        var (reloaded, _, _) = Make.ViewModel(settings);
        Assert.Equal("tr", reloaded.Language);
        Assert.Equal("small", reloaded.Model);
        Assert.Equal("es", reloaded.TranslationTarget);
        Assert.Equal(SubtitleDisplay.Both, reloaded.DisplayMode);
        Assert.False(reloaded.Diarize);
        Assert.Equal(3, reloaded.SpeakerCount);
    }

    [Fact]
    public void InvalidStoredValuesFallBack()
    {
        var settings = new InMemorySettingsStore();
        settings.SetString("selectedLanguage", "Türkçe");   // old free text
        settings.SetString("translationTarget", "");       // automatic can't be a target
        settings.SetString("subtitleDisplayMode", "weird");
        settings.SetInt("speakerCount", 1);
        var (vm, _, _) = Make.ViewModel(settings);
        Assert.Equal("", vm.Language);
        Assert.Equal("en", vm.TranslationTarget);
        Assert.Equal(SubtitleDisplay.Original, vm.DisplayMode);
        Assert.Equal(0, vm.SpeakerCount);
    }

    [Fact]
    public void LanguageFilterPicksMostAccurateModelWithNotice()
    {
        using var _ = new LanguageScope(UILanguage.En);
        var (vm, _, _) = Make.ViewModel();
        vm.Model = "gigaam-v3";      // not a Whisper model: not shown for automatic
        vm.Language = "ru";           // gigaam supports ru: kept
        Assert.Equal("gigaam-v3", vm.Model);
        vm.Language = "";
        Assert.Equal("large-v3", vm.Model);
        Assert.Equal("Selected Most accurate for Automatic detection.", vm.ModelNoticeText);
        Assert.Contains(vm.AvailableModels, m => m.Id == "large-v3");
        Assert.DoesNotContain(vm.AvailableModels, m => m.Id == "parakeet-v3");
        // A manual model choice clears the notice.
        vm.Model = "small";
        Assert.Null(vm.ModelNotice);
    }

    [Fact]
    public async Task TieOnAccuracyPrefersFasterModel()
    {
        var (vm, engine, _) = Make.ViewModel();
        engine.CatalogModels =
        [
            Make.Model("slow", accuracy: 4, speed: 1),
            Make.Model("fast", accuracy: 4, speed: 5),
            Make.Model("also-fast", accuracy: 4, speed: 5),
            Make.Model("madlad-3b", kind: "translation", accuracy: 5),
        ];
        await vm.Catalog.LoadAsync();
        Assert.Equal("fast", vm.Model);
    }

    [Fact]
    public void TranslationTargetNeverEqualsSource()
    {
        var (vm, _, _) = Make.ViewModel();
        vm.Language = "en";
        Assert.Equal("tr", vm.TranslationTarget);   // first target that isn't the source
        Assert.False(vm.IsTranslationTargetEnabled(SubtitleLanguage.English));
        Assert.True(vm.IsTranslationTargetEnabled(SubtitleLanguage.Spanish));
    }

    [Fact]
    public async Task CatalogReloadReconcilesTranslationModel()
    {
        var settings = new InMemorySettingsStore();
        settings.SetString("translationModel", "old-model");
        var (vm, engine, _) = Make.ViewModel(settings);
        engine.CatalogModels = [.. ModelCatalog.FallbackModels.Select(m => m with { })];
        // Reconciled against the fallback catalog at once (macOS does it on the first catalog onChange).
        Assert.Equal("madlad-3b", vm.TranslationModel);
        vm.TranslationModel = "gone";
        engine.CatalogModels = [Make.Model("base"), Make.Model("nllb-1.3b", kind: "translation", accuracy: 4)];
        await vm.Catalog.LoadAsync();
        Assert.Equal("nllb-1.3b", vm.TranslationModel);
    }
}

public class ViewModelJobTests
{
    private static string TempWav()
    {
        var path = Path.Combine(Path.GetTempPath(), $"subtitle-adder-test-{Guid.NewGuid()}.wav");
        File.WriteAllBytes(path, [1, 2, 3]);
        return path;
    }

    [Fact]
    public async Task GenerateRunsTranscriptionAndFinishes()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var (vm, engine, _) = Make.ViewModel();
        vm.OpenVideo(@"C:\videos\clip.mp4");
        vm.Language = "tr";
        vm.Model = "small";
        vm.SpeakerCount = 3;
        Assert.True(vm.GenerateCommand.CanExecute(null));
        vm.Generate();

        Assert.True(vm.IsWorking);
        Assert.Equal("Başlıyor…", vm.StatusText);
        Assert.False(vm.GenerateCommand.CanExecute(null));
        Assert.True(vm.CancelCommand.CanExecute(null));
        Assert.Equal(new HashSet<string> { "small", "speaker-diarization" }, vm.Catalog.ActiveModels);
        var call = Assert.Single(engine.TranscribeCalls);
        Assert.Equal(@"C:\videos\clip.mp4", call.Video);
        Assert.Equal(Path.Combine(Path.GetTempPath(), "SubtitleAdder"), call.OutDir);
        Assert.Equal("tr", call.Options.Language);
        Assert.Equal("small", call.Options.Model);
        Assert.True(call.Options.Diarize);
        Assert.Equal(3, call.Options.Speakers);

        engine.Events.Writer.TryWrite(new EngineEvent.Stage(EngineStage.Transcribing, 0.5));
        await Make.Eventually(() => vm.StatusText == "Konuşma metne çevriliyor… 50%", "transcribing label");
        Assert.Equal(0.5, vm.StatusFraction);

        var wav = TempWav();
        engine.Finish(Make.Result("tr", wav, new Sentence(1, 0, 1, "Merhaba."), new Sentence(2, 1, 2, "Selam.")));
        await vm.CurrentJob;

        Assert.Equal(JobStatusKind.Finished, vm.StatusKind);
        Assert.Equal("2 satır · dil: tr", vm.StatusText);
        Assert.Equal(2, vm.Sentences.Count);
        Assert.Equal("tr", vm.DetectedLanguage);
        Assert.True(File.Exists(wav));                  // temp WAV is kept during session and deleted on Dispose
        Assert.Equal(1, engine.ModelsCalls);            // catalog reloaded after the job
        Assert.Empty(vm.Catalog.ActiveModels);
        Assert.True(vm.TranslateCommand.CanExecute(null));
        vm.Dispose();
        Assert.False(File.Exists(wav));                 // deleted when the app closes
    }

    [Fact]
    public async Task AutomaticLanguageUsesDetectedLanguageAsTranslationSource()
    {
        var (vm, engine, _) = Make.ViewModel();
        vm.OpenVideo("a.mp4");
        vm.Diarize = false;
        vm.Generate();
        Assert.Null(engine.TranscribeCalls[0].Options.Language);
        Assert.False(engine.TranscribeCalls[0].Options.Diarize);
        Assert.Equal(new HashSet<string> { "large-v3" }, vm.Catalog.ActiveModels);
        engine.Finish(Make.Result("en", "", new Sentence(1, 0, 1, "Hi.")));
        await vm.CurrentJob;
        Assert.Equal("en", vm.TranslationSource);
        Assert.Equal("tr", vm.TranslationTarget);  // target moved off the detected source
    }

    [Fact]
    public async Task DownloadLabelsTellTranscriptionAndDiarizationApart()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var (vm, engine, _) = Make.ViewModel();
        vm.OpenVideo("a.mp4");
        vm.Generate();   // large-v3 and speaker-diarization are not downloaded in the fallback list
        engine.Events.Writer.TryWrite(new EngineEvent.Stage(EngineStage.DownloadingModel, 0.25));
        await Make.Eventually(() => vm.StatusText == "Model indiriliyor… %25", "transcription download");
        engine.Events.Writer.TryWrite(new EngineEvent.Stage(EngineStage.DownloadingModel, 1));
        engine.Events.Writer.TryWrite(new EngineEvent.Stage(EngineStage.DownloadingModel, 0));
        await Make.Eventually(() => vm.StatusText == "Konuşmacı ayırma modeli indiriliyor… %0", "diarization download");
        engine.Finish();
        await vm.CurrentJob;
    }

    [Fact]
    public async Task EngineErrorFailsTheJob()
    {
        var (vm, engine, _) = Make.ViewModel();
        vm.OpenVideo("a.mp4");
        vm.Generate();
        engine.Events.Writer.TryComplete(EngineException.Failed("GigaAM yalnız Rusça destekler."));
        await vm.CurrentJob;
        Assert.Equal(JobStatusKind.Failed, vm.StatusKind);
        Assert.Equal("GigaAM yalnız Rusça destekler.", vm.StatusText);
        Assert.Empty(vm.Catalog.ActiveModels);
        Assert.Equal(0, engine.ModelsCalls);
    }

    [Fact]
    public async Task CancelStopsTheStreamAndIgnoresLateEvents()
    {
        var (vm, engine, _) = Make.ViewModel();
        vm.OpenVideo("a.mp4");
        vm.Generate();
        var job = vm.CurrentJob;
        vm.Cancel();
        Assert.Equal(JobStatusKind.Idle, vm.StatusKind);
        Assert.Empty(vm.Catalog.ActiveModels);
        await engine.StreamCancelled.Task.WaitAsync(TimeSpan.FromSeconds(5));
        engine.Finish(Make.Result(new Sentence(1, 0, 1, "late")));
        await job;
        Assert.Empty(vm.Sentences);
        Assert.Equal(JobStatusKind.Idle, vm.StatusKind);
    }

    [Fact]
    public async Task NewJobMakesTheOldOneStale()
    {
        var (vm, engine, _) = Make.ViewModel();
        vm.OpenVideo("a.mp4");
        vm.Generate();
        var oldEvents = engine.Events;
        var oldJob = vm.CurrentJob;
        engine.Reset();
        vm.Generate();
        // The old stream ends normally after the new job started: its result must not land.
        oldEvents.Writer.TryWrite(Make.Result(new Sentence(1, 0, 1, "old")));
        oldEvents.Writer.TryComplete();
        await oldJob;
        Assert.Empty(vm.Sentences);
        Assert.True(vm.IsWorking);
        engine.Finish(Make.Result(new Sentence(1, 0, 1, "new")));
        await vm.CurrentJob;
        Assert.Equal("new", Assert.Single(vm.Sentences).Text);
    }

    [Fact]
    public async Task OpenVideoResetsEverything()
    {
        var (vm, engine, _) = Make.ViewModel();
        vm.OpenVideo("a.mp4");
        vm.Generate();
        engine.Finish(Make.Result(new Sentence(1, 0, 1, "x")));
        await vm.CurrentJob;
        vm.Selection = vm.Sentences[0].Uid;
        vm.SetText("y", vm.Sentences[0].Uid);
        Assert.True(vm.CanUndo);

        vm.OpenVideo("a.mp4");   // same video: no change
        Assert.Single(vm.Sentences);
        vm.OpenVideo("b.mp4");
        Assert.Empty(vm.Sentences);
        Assert.Null(vm.Selection);
        Assert.Null(vm.DetectedLanguage);
        Assert.False(vm.CanUndo);
        Assert.Equal(JobStatusKind.Idle, vm.StatusKind);
        Assert.Equal("b.mp4", vm.WindowTitle);
    }

    [Fact]
    public void GenerateWithoutEngineFails()
    {
        using var _ = new LanguageScope(UILanguage.En);
        var (vm, _, _) = Make.ViewModel(engine: false);
        vm.OpenVideo("a.mp4");
        vm.Generate();
        Assert.Equal(JobStatusKind.Failed, vm.StatusKind);
        var english = vm.StatusText;
        LanguageState.Shared.Language = UILanguage.Tr;
        Assert.NotEqual(english, vm.StatusText);   // "engine not found" is re-localized on display
    }

    [Fact]
    public void IdleHintDependsOnVideoAndLines()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var (vm, _, _) = Make.ViewModel();
        Assert.Equal("Hazır", vm.StatusText);
        vm.OpenVideo("a.mp4");
        Assert.Equal("Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (Ctrl+Enter)", vm.StatusText);
    }

    [Fact]
    public void LanguageSwitchReRaisesAllProperties()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var (vm, _, _) = Make.ViewModel();
        var raised = new List<string?>();
        vm.PropertyChanged += (_, e) => raised.Add(e.PropertyName);
        var refreshed = 0;
        vm.LocalizationChanged += (_, _) => refreshed++;
        LanguageState.Shared.Language = UILanguage.En;
        Assert.Contains(string.Empty, raised);
        Assert.Equal(1, refreshed);
        Assert.Equal("Ready", vm.StatusText);
        vm.Dispose();
    }

    [Fact]
    public void StageLabels()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        Assert.Equal("Ses ayrılıyor…", SubtitleViewModel.LabelText(EngineStage.ExtractingAudio, null, JobPurpose.Transcription));
        Assert.Equal("Model indiriliyor…", SubtitleViewModel.LabelText(EngineStage.DownloadingModel, null, JobPurpose.Transcription));
        Assert.Equal("Çeviri modeli indiriliyor… %100", SubtitleViewModel.LabelText(EngineStage.DownloadingModel, 1.4, JobPurpose.Translation));
        Assert.Equal("Çeviri modeli yükleniyor…", SubtitleViewModel.LabelText(EngineStage.LoadingModel, null, JobPurpose.Translation));
        Assert.Equal("Model yükleniyor…", SubtitleViewModel.LabelText(EngineStage.LoadingModel, null, JobPurpose.Diarization));
        Assert.Equal("Konuşmacılar ayrılıyor… %43", SubtitleViewModel.LabelText(EngineStage.Diarizing, 0.425, JobPurpose.Transcription));
        Assert.Equal("Cümleler çıkarılıyor…", SubtitleViewModel.LabelText(EngineStage.Segmenting, null, JobPurpose.Transcription));
        Assert.Equal("Çevriliyor…", SubtitleViewModel.LabelText(EngineStage.Translating, null, JobPurpose.Translation));
        Assert.Equal("Çalışıyor…", SubtitleViewModel.LabelText(EngineStage.Unknown, null, JobPurpose.Transcription));
        Assert.Equal(SubtitleViewModel.Label(EngineStage.Translating, 0.5, JobPurpose.Translation),
            SubtitleViewModel.Label(EngineStage.Translating, 0.5, JobPurpose.Translation));
        Assert.NotEqual(SubtitleViewModel.Label(EngineStage.Translating, 0.5, JobPurpose.Translation),
            SubtitleViewModel.Label(EngineStage.Translating, 0.6, JobPurpose.Translation));
        using var en = new LanguageScope(UILanguage.En);
        Assert.Equal("Translating… 42%", SubtitleViewModel.LabelText(EngineStage.Translating, 0.42, JobPurpose.Translation));
    }
}

public class ViewModelTranslationTests
{
    private static async Task<(SubtitleViewModel Vm, FakeEngineClient Engine)> Transcribed()
    {
        var (vm, engine, _) = Make.ViewModel();
        vm.OpenVideo(@"C:\v\clip.mp4");
        vm.Language = "tr";
        vm.Generate();
        engine.Finish(Make.Result(new Sentence(1, 0, 1, "Bir."), new Sentence(2, 1, 2, "İki."), new Sentence(3, 2, 3, "Üç.")));
        await vm.CurrentJob;
        engine.Reset();
        return (vm, engine);
    }

    private static async Task Translate(SubtitleViewModel vm, FakeEngineClient engine, string language = "en")
    {
        vm.Translate();
        var request = engine.TranslateCalls[^1].Sentences;
        engine.Finish(
            new EngineEvent.Stage(EngineStage.Translating, 0.5),
            new EngineEvent.Translation(language, [.. request.Select(s => new Sentence(s.Id, s.Start, s.End, "T" + s.Id))]));
        await vm.CurrentJob;
    }

    [Fact]
    public async Task TranslationIsMappedByUid()
    {
        var (vm, engine) = await Transcribed();
        vm.TranslationModel = "nllb-1.3b";
        await Translate(vm, engine);
        var call = Assert.Single(engine.TranslateCalls);
        Assert.Equal("en", call.To);
        Assert.Equal("tr", call.From);
        Assert.Equal("nllb-1.3b", call.Model);
        Assert.Equal(JobStatusKind.Finished, vm.StatusKind);
        Assert.Equal("T2", vm.TranslationFor(vm.Sentences[1])?.Text);
        Assert.Equal("en", vm.TranslationLanguage);
        Assert.Equal("en", vm.TranslationTarget);
        Assert.False(vm.IsTranslationStale(vm.Sentences[1]));
        Assert.Equal("clip.mp4", vm.WindowTitle);
    }

    [Fact]
    public async Task EditingTheOriginalMarksTranslationStale()
    {
        var (vm, engine) = await Transcribed();
        await Translate(vm, engine);
        vm.SetText("Değişti.", vm.Sentences[0].Uid);
        Assert.True(vm.IsTranslationStale(vm.Sentences[0]));
        vm.Undo();
        Assert.False(vm.IsTranslationStale(vm.Sentences[0]));
    }

    [Fact]
    public async Task MergeAndSplitClearAffectedTranslationsAndUndoRestores()
    {
        var (vm, engine) = await Transcribed();
        await Translate(vm, engine);
        vm.Selection = vm.Sentences[0].Uid;
        vm.MergeSelectedWithNext();
        Assert.Null(vm.TranslationFor(vm.Sentences[0]));
        Assert.Equal("T3", vm.TranslationFor(vm.Sentences[1])?.Text);
        vm.Undo();
        Assert.Equal(3, vm.Sentences.Count);
        Assert.Equal("T1", vm.TranslationFor(vm.Sentences[0])?.Text);
        Assert.Equal("T2", vm.TranslationFor(vm.Sentences[1])?.Text);
    }

    [Fact]
    public async Task UndoingAnEditMadeBeforeTranslatingKeepsTheTranslation()
    {
        var (vm, engine) = await Transcribed();
        vm.SetText("Değişti.", vm.Sentences[2].Uid);
        await Translate(vm, engine, "es");
        vm.Undo();
        Assert.Equal("Üç.", vm.Sentences[2].Text);
        Assert.Equal(["T1", "T2", "T3"], vm.Sentences.Select(s => vm.TranslationFor(s)?.Text));
        vm.Redo();
        Assert.Equal("Değişti.", vm.Sentences[2].Text);
        Assert.Equal("T3", vm.TranslationFor(vm.Sentences[2])?.Text);
    }

    [Fact]
    public async Task RedoingAMergeClearsTheMergedTranslationAgain()
    {
        var (vm, engine) = await Transcribed();
        await Translate(vm, engine);
        vm.Selection = vm.Sentences[0].Uid;
        vm.MergeSelectedWithNext();
        vm.Undo();
        Assert.Equal("T1", vm.TranslationFor(vm.Sentences[0])?.Text);
        vm.Redo();
        Assert.Equal(2, vm.Sentences.Count);
        Assert.Null(vm.TranslationFor(vm.Sentences[0]));
        Assert.Equal("T3", vm.TranslationFor(vm.Sentences[1])?.Text);
    }

    [Fact]
    public async Task UndoingATranslationTextEditAfterRetranslatingKeepsTheNewTranslation()
    {
        var (vm, engine) = await Transcribed();
        await Translate(vm, engine, "es");
        vm.SetTranslationText("Editado", vm.Sentences[0].Uid);
        engine.Reset();
        vm.TranslationTarget = "pt";
        await Translate(vm, engine, "pt");
        vm.Undo();
        Assert.All(vm.Sentences, s => Assert.Equal("pt", vm.TranslationFor(s)?.Language));
        Assert.Equal("T1", vm.TranslationFor(vm.Sentences[0])?.Text);
    }

    [Fact]
    public async Task TranslationDraftCommitsAsOneUndoableEdit()
    {
        var (vm, engine) = await Transcribed();
        await Translate(vm, engine);
        var first = vm.Sentences[0];
        vm.BeginTranslationEditing(first);
        vm.UpdateTranslationDraft("Ed", first.Uid);
        vm.UpdateTranslationDraft("Edited\nline", first.Uid);
        Assert.Equal("Edited\nline", vm.TranslationDraftText(first));
        Assert.Equal("T1", vm.TranslationFor(first)?.Text);   // not committed yet
        Assert.False(vm.CanUndo && vm.UndoStack.UndoActionKey == "Çeviriyi Değiştir");
        vm.CommitTranslationDraft();
        Assert.Equal("Edited line", vm.TranslationFor(first)?.Text);
        Assert.Equal("Çeviriyi Değiştir", vm.UndoStack.UndoActionKey);
        vm.Undo();
        Assert.Equal("T1", vm.TranslationFor(first)?.Text);
        vm.Redo();
        Assert.Equal("Edited line", vm.TranslationFor(first)?.Text);
    }

    [Fact]
    public async Task ExportUsesTranslationTextWithOriginalTimes()
    {
        var (vm, engine) = await Transcribed();
        await Translate(vm, engine, "es");
        vm.SetTranslationText("", vm.Sentences[2].Uid);   // empty translation falls back to the original
        var exported = vm.ExportSentences(true);
        Assert.Equal(["T1", "T2", "Üç."], exported.Select(s => s.Text));
        Assert.All(exported, s => Assert.Empty(s.Words));
        Assert.Equal(vm.Sentences.Select(s => s.Start), exported.Select(s => s.Start));

        vm.ExportLanguage = ExportLanguage.Translated;
        vm.ExportFormat = SubtitleFormat.Vtt;
        Assert.Equal("clip.es.vtt", vm.ExportFileName);
        Assert.Equal("T1|T2|Üç.", await vm.RenderExportAsync());
        Assert.Equal(SubtitleFormat.Vtt, engine.RenderCalls[^1].Format);
        vm.ExportLanguage = ExportLanguage.Original;
        Assert.Equal("clip.vtt", vm.ExportFileName);
        Assert.Equal("Bir.|İki.|Üç.", await vm.RenderAsync(SubtitleFormat.Srt));
    }

    [Fact]
    public async Task RenderErrorIsShownInStatus()
    {
        var (vm, engine) = await Transcribed();
        engine.Render = (_, _) => throw EngineException.Failed("render failed");
        Assert.Null(await vm.RenderAsync(SubtitleFormat.Srt));
        Assert.Equal("render failed", vm.StatusText);
    }

    [Fact]
    public async Task SameSourceAndTargetIsRejected()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var (vm, engine) = await Transcribed();
        // Bypass the reconcile rule the way a stale UI could.
        typeof(SubtitleViewModel).GetField("_translationTarget", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!
            .SetValue(vm, "tr");
        vm.Translate();
        Assert.Equal("Kaynak ve hedef dil aynı olamaz.", vm.StatusText);
        Assert.Empty(engine.TranslateCalls);
    }

    [Fact]
    public async Task NllbNeedsSourceLanguage()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var (vm, engine, _) = Make.ViewModel();
        vm.OpenVideo("a.mp4");
        vm.Sentences = [new Sentence(1, 0, 1, "x")];   // automatic language, nothing detected yet
        await Task.CompletedTask;
        vm.TranslationModel = "nllb-1.3b";
        vm.Translate();
        Assert.Equal("NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç.", vm.StatusText);
    }

    [Fact]
    public async Task TranslationJobMarksModelActiveAndCanBeCancelled()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var (vm, engine) = await Transcribed();
        vm.Translate();
        Assert.Equal("Çeviri hazırlanıyor…", vm.StatusText);
        Assert.Equal(new HashSet<string> { "madlad-3b" }, vm.Catalog.ActiveModels);
        engine.Events.Writer.TryWrite(new EngineEvent.Stage(EngineStage.DownloadingModel, 0.5));
        await Make.Eventually(() => vm.StatusText == "Çeviri modeli indiriliyor… %50", "translation download");
        var job = vm.CurrentJob;
        vm.Cancel();
        await job;
        Assert.Equal(JobStatusKind.Idle, vm.StatusKind);
        Assert.Empty(vm.Translations);
        Assert.Empty(vm.Catalog.ActiveModels);
    }

    [Fact]
    public async Task NewTranscriptionClearsTranslations()
    {
        var (vm, engine) = await Transcribed();
        await Translate(vm, engine);
        engine.Reset();
        vm.Generate();
        Assert.Empty(vm.Translations);
        Assert.Null(vm.TranslationLanguage);
        engine.Finish();
        await vm.CurrentJob;
    }

    [Fact]
    public async Task TranslationSwitchesDisplayModeAndSyncsWithExportLanguage()
    {
        var (vm, engine) = await Transcribed();
        Assert.Equal(SubtitleDisplay.Original, vm.DisplayMode);
        Assert.Equal(ExportLanguage.Original, vm.ExportLanguage);

        await Translate(vm, engine, "es");
        Assert.Equal(SubtitleDisplay.Translation, vm.DisplayMode);
        Assert.Equal(ExportLanguage.Translated, vm.ExportLanguage);

        // Switching export language back to Original also switches video display to Original.
        vm.ExportLanguage = ExportLanguage.Original;
        Assert.Equal(SubtitleDisplay.Original, vm.DisplayMode);

        // Switching video display back to Translation also switches export language to Translated.
        vm.DisplayMode = SubtitleDisplay.Translation;
        Assert.Equal(ExportLanguage.Translated, vm.ExportLanguage);
    }

    [Fact]
    public async Task SaveSubtitleToVideoFolderWritesNextToVideoWithSameName()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var dir = Path.Combine(Path.GetTempPath(), $"subtitle-adder-save-{Guid.NewGuid()}");
        Directory.CreateDirectory(dir);
        try
        {
            var videoPath = Path.Combine(dir, "My Video.mp4");
            var (vm, engine, _) = Make.ViewModel();
            vm.OpenVideo(videoPath);
            Assert.False(vm.CanSaveSubtitle);
            vm.Generate();
            engine.Finish(Make.Result("en", "", new Sentence(1, 0, 1, "Hello."), new Sentence(2, 1, 2, "World.")));
            await vm.CurrentJob;
            engine.Reset();

            Assert.True(vm.CanSaveSubtitle);
            Assert.Equal(Path.Combine(dir, "My Video.srt"), vm.VideoSubtitlePath);

            // Even when translated, saving to video folder uses the exact video name ("My Video.srt").
            await Translate(vm, engine, "tr");
            var savedPath = await vm.SaveSubtitleToVideoFolderAsync();
            Assert.Equal(Path.Combine(dir, "My Video.srt"), savedPath);
            Assert.Equal("T1|T2", await File.ReadAllTextAsync(savedPath!));
            Assert.Contains("kaydedildi: My Video.srt", vm.StatusText);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public async Task OpenVideoAutoLoadsSidecarSrtAndAllowsImmediateIncrementalTranslation()
    {
        var dir = Path.Combine(Path.GetTempPath(), $"subtitle-adder-sidecar-{Guid.NewGuid()}");
        Directory.CreateDirectory(dir);
        try
        {
            var videoPath = Path.Combine(dir, "Movie.mp4");
            var srtPath = Path.Combine(dir, "Movie.srt");
            await File.WriteAllTextAsync(
                srtPath,
                "1\n00:00:01,250 --> 00:00:03,500\n<i>Hello</i> world!\n\n2\n00:00:04,000 --> 00:00:06,000\nSecond line.\n");

            var (vm, engine, _) = Make.ViewModel();
            vm.OpenVideo(videoPath);

            // Sidecar .srt is loaded immediately without running transcription or audio extraction.
            Assert.Empty(engine.TranscribeCalls);
            Assert.Equal(2, vm.Sentences.Count);
            Assert.Equal("Hello world!", vm.Sentences[0].Text);
            Assert.Equal(1.25, vm.Sentences[0].Start);
            Assert.Equal(3.5, vm.Sentences[0].End);
            Assert.Equal("Second line.", vm.Sentences[1].Text);
            Assert.True(vm.TranslateCommand.CanExecute(null));

            // Incremental translation updates lines live as batches arrive.
            vm.Translate();
            var request = Assert.Single(engine.TranslateCalls).Sentences;
            engine.Events.Writer.TryWrite(new EngineEvent.Translation(
                "en",
                [new Sentence(request[0].Id, request[0].Start, request[0].End, "Chunk 1")]));
            await Make.Eventually(() => vm.TranslationFor(vm.Sentences[0])?.Text == "Chunk 1", "first translation batch");
            Assert.Null(vm.TranslationFor(vm.Sentences[1]));
            Assert.Equal(SubtitleDisplay.Translation, vm.DisplayMode);

            engine.Finish(new EngineEvent.Translation(
                "en",
                [
                    new Sentence(request[0].Id, request[0].Start, request[0].End, "Chunk 1"),
                    new Sentence(request[1].Id, request[1].Start, request[1].End, "Chunk 2"),
                ]));
            await vm.CurrentJob;
            Assert.Equal("Chunk 2", vm.TranslationFor(vm.Sentences[1])?.Text);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void SubtitleParserHandlesVttAndAssFormats()
    {
        var vtt = SubtitleParser.Parse(
            "WEBVTT\n\n00:01.000 --> 00:02.500 align:start\nFirst VTT cue\n",
            ".vtt");
        var singleVtt = Assert.Single(vtt);
        Assert.Equal(1.0, singleVtt.Start);
        Assert.Equal(2.5, singleVtt.End);
        Assert.Equal("First VTT cue", singleVtt.Text);

        var ass = SubtitleParser.Parse(
            "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:05.00,0:00:07.50,Default,,0,0,0,,{\\i1}Styled{\\i0}\\NASS line\n",
            ".ass");
        var singleAss = Assert.Single(ass);
        Assert.Equal(5.0, singleAss.Start);
        Assert.Equal(7.5, singleAss.End);
        Assert.Equal("Styled ASS line", singleAss.Text);
    }

    [Fact]
    public async Task GenerateAndTranslateChainsTranscriptionTranslationAndAutoSave()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var dir = Path.Combine(Path.GetTempPath(), $"subtitle-adder-e2e-{Guid.NewGuid()}");
        Directory.CreateDirectory(dir);
        try
        {
            var videoPath = Path.Combine(dir, "Episode.mp4");
            var (vm, engine, _) = Make.ViewModel();
            vm.OpenVideo(videoPath);
            vm.TranslationTarget = "tr";
            Assert.True(vm.AutoSaveSubtitle);

            vm.GenerateAndTranslate();
            Assert.Single(engine.TranscribeCalls);

            // Finish transcription: ViewModel should automatically chain into Translate().
            engine.Finish(Make.Result("pt", "", new Sentence(1, 0, 1, "Olá."), new Sentence(2, 1, 2, "Tudo bem?")));
            await Make.Eventually(() => engine.TranslateCalls.Count == 1, "chained translation call");

            var req = engine.TranslateCalls[0].Sentences;
            engine.Finish(new EngineEvent.Translation(
                "tr",
                [
                    new Sentence(req[0].Id, req[0].Start, req[0].End, "Merhaba."),
                    new Sentence(req[1].Id, req[1].Start, req[1].End, "Nasılsın?"),
                ]));
            await vm.CurrentJob;

            var expectedSrt = Path.Combine(dir, "Episode.srt");
            Assert.True(File.Exists(expectedSrt));
            Assert.Equal("Merhaba.|Nasılsın?", await File.ReadAllTextAsync(expectedSrt));
            Assert.Contains("kaydedildi: Episode.srt", vm.StatusText);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public async Task PlaylistProcessesMultipleVideosWithPerFileLanguagesAndSavesToEachFolder()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var rootDir = Path.Combine(Path.GetTempPath(), $"subtitle-adder-playlist-{Guid.NewGuid()}");
        var dirA = Path.Combine(rootDir, "folderA");
        var dirB = Path.Combine(rootDir, "folderB");
        Directory.CreateDirectory(dirA);
        Directory.CreateDirectory(dirB);
        try
        {
            var videoA = Path.Combine(dirA, "ClipA.mp4");
            var videoB = Path.Combine(dirB, "ClipB.mkv");
            var (vm, engine, _) = Make.ViewModel();
            vm.TranslationTarget = "tr";

            vm.AddToPlaylist([videoA, videoB]);
            Assert.Equal(2, vm.Playlist.Count);

            // Set all to automatic first, then override videoA to "pt" and videoB to "ja".
            vm.SetAllPlaylistLanguages("");
            Assert.All(vm.Playlist, item => Assert.Equal("", item.SourceLanguage));
            vm.SetPlaylistItemLanguage(vm.Playlist[0].Id, "pt");
            vm.SetPlaylistItemLanguage(vm.Playlist[1].Id, "ja");
            Assert.Equal("pt", vm.Playlist[0].SourceLanguage);
            Assert.Equal("ja", vm.Playlist[1].SourceLanguage);

            var runTask = vm.RunPlaylistAsync();

            // Video A: transcribe (pt) -> translate (to tr)
            await Make.Eventually(() => engine.TranscribeCalls.Count == 1, "video A transcribe");
            Assert.Equal(videoA, engine.TranscribeCalls[0].Video);
            Assert.Equal("pt", engine.TranscribeCalls[0].Options.Language);
            engine.Finish(Make.Result("pt", "", new Sentence(1, 0, 1, "Caralho!")));

            await Make.Eventually(() => engine.TranslateCalls.Count == 1, "video A translate");
            Assert.Equal("tr", engine.TranslateCalls[0].To);
            Assert.Equal("pt", engine.TranslateCalls[0].From);
            var reqA = engine.TranslateCalls[0].Sentences;
            engine.Finish(new EngineEvent.Translation("tr", [new Sentence(reqA[0].Id, reqA[0].Start, reqA[0].End, "Hassiktir!")]));

            // Video B: transcribe (ja) -> translate (to tr)
            await Make.Eventually(() => engine.TranscribeCalls.Count == 2, "video B transcribe");
            Assert.Equal(videoB, engine.TranscribeCalls[1].Video);
            Assert.Equal("ja", engine.TranscribeCalls[1].Options.Language);
            engine.Finish(Make.Result("ja", "", new Sentence(1, 0, 1, "こんにちは")));

            await Make.Eventually(() => engine.TranslateCalls.Count == 2, "video B translate");
            Assert.Equal("tr", engine.TranslateCalls[1].To);
            Assert.Equal("ja", engine.TranslateCalls[1].From);
            var reqB = engine.TranslateCalls[1].Sentences;
            engine.Finish(new EngineEvent.Translation("tr", [new Sentence(reqB[0].Id, reqB[0].Start, reqB[0].End, "Merhaba")]));

            await runTask;

            Assert.All(vm.Playlist, item => Assert.Equal(PlaylistItemState.Completed, item.State));
            Assert.Equal("2 / 2 tamamlandı", vm.PlaylistSummaryText);
            Assert.Equal("Hassiktir!", await File.ReadAllTextAsync(Path.Combine(dirA, "ClipA.srt")));
            Assert.Equal("Merhaba", await File.ReadAllTextAsync(Path.Combine(dirB, "ClipB.srt")));
        }
        finally
        {
            Directory.Delete(rootDir, recursive: true);
        }
    }
}
