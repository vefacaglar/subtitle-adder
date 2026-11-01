using System.Text.Json.Nodes;
using SubtitleAdder.Core;

namespace SubtitleAdder.Core.Tests;

public class EditingTests
{
    private static SubtitleViewModel MakeModel()
    {
        var (vm, _, _) = Make.ViewModel(engine: false);
        vm.Sentences =
        [
            new Sentence(1, 0, 2, "one two"),
            new Sentence(2, 2, 4, "three four"),
            new Sentence(3, 4, 6, "five"),
        ];
        return vm;
    }

    [Fact]
    public void DraftIsNotUndoableUntilCommitted()
    {
        var vm = MakeModel();
        var first = vm.Sentences[0];
        vm.BeginEditing(first);
        vm.UpdateDraft("changed", first.Uid);
        Assert.Equal("changed", vm.DraftText(first));
        Assert.Equal("one two", vm.Sentences[0].Text);
        Assert.False(vm.CanUndo);
        vm.CommitDraft();
        Assert.Equal("changed", vm.Sentences[0].Text);
        Assert.Null(vm.Draft);
        Assert.Equal("Metni Değiştir", vm.UndoStack.UndoActionKey);
    }

    [Fact]
    public void DraftNewlinesBecomeSpaces()
    {
        var vm = MakeModel();
        vm.UpdateDraft("a\nb\r\nc", vm.Sentences[0].Uid);
        vm.CommitDraft();
        Assert.Equal("a b c", vm.Sentences[0].Text);
    }

    [Fact]
    public void UpdatingAnotherLineCommitsThePreviousDraft()
    {
        var vm = MakeModel();
        vm.UpdateDraft("first", vm.Sentences[0].Uid);
        vm.UpdateDraft("second", vm.Sentences[1].Uid);
        Assert.Equal("first", vm.Sentences[0].Text);
        Assert.Equal(vm.Sentences[1].Uid, vm.Draft?.Uid);
    }

    [Fact]
    public void UndoWithUncommittedDraftRevertsTheTyping()
    {
        var vm = MakeModel();
        vm.SetText("edited", vm.Sentences[1].Uid);
        vm.UpdateDraft("typing", vm.Sentences[0].Uid);
        vm.Undo();
        Assert.Equal("one two", vm.Sentences[0].Text);
        Assert.Equal("edited", vm.Sentences[1].Text);
        Assert.Null(vm.Draft);
        vm.Redo();
        Assert.Equal("typing", vm.Sentences[0].Text);
    }

    [Fact]
    public void StructuralCommandWithDraftUndoesAsOneGroup()
    {
        var vm = MakeModel();
        vm.Selection = vm.Sentences[0].Uid;
        vm.UpdateDraft("draft", vm.Sentences[0].Uid);
        vm.Nudge(TimingEdge.Start, 0.5);
        Assert.Equal("draft", vm.Sentences[0].Text);
        Assert.Equal(0.5, vm.Sentences[0].Start, 9);
        Assert.Equal("Zamanlamayı Değiştir", vm.UndoStack.UndoActionKey);
        vm.Undo();
        Assert.Equal("one two", vm.Sentences[0].Text);
        Assert.Equal(0, vm.Sentences[0].Start, 9);
        Assert.False(vm.CanUndo);
    }

    [Fact]
    public void DeleteSelectsNextLineAndUndoRestores()
    {
        var vm = MakeModel();
        vm.Selection = vm.Sentences[2].Uid;
        vm.DeleteSelected();
        Assert.Equal(2, vm.Sentences.Count);
        Assert.Equal(vm.Sentences[1].Uid, vm.Selection);
        vm.Undo();
        Assert.Equal(3, vm.Sentences.Count);
        Assert.Equal([1, 2, 3], vm.Sentences.Select(s => s.Id));
    }

    [Fact]
    public void SetSelectedTimeAndSplitAvailability()
    {
        var vm = MakeModel();
        Assert.False(vm.CanSplitSelected(1));
        Assert.False(vm.CanMergeSelected);
        vm.Selection = vm.Sentences[0].Uid;
        Assert.True(vm.CanSplitSelected(1));
        Assert.True(vm.CanMergeSelected);
        Assert.True(vm.MergeSelectedCommand.CanExecute(null));
        vm.SetSelected(TimingEdge.Start, 0.5);
        Assert.Equal(0.5, vm.Sentences[0].Start, 9);
        Assert.Equal("00:00.50", vm.SelectedStartText);
        vm.Selection = vm.Sentences[2].Uid;
        Assert.False(vm.CanMergeSelected);
    }

    [Fact]
    public void NoOpEditsLeaveNoUndoRecord()
    {
        var vm = MakeModel();
        vm.SetText("one two", vm.Sentences[0].Uid);
        vm.SetSpeaker(null, vm.Sentences[0].Uid);
        Assert.False(vm.CanUndo);
    }

    [Fact]
    public void NewEditClearsRedo()
    {
        var vm = MakeModel();
        vm.SetText("a", vm.Sentences[0].Uid);
        vm.Undo();
        Assert.True(vm.CanRedo);
        vm.SetText("b", vm.Sentences[0].Uid);
        Assert.False(vm.CanRedo);
    }

    [Fact]
    public void UndoTitlesAreLocalized()
    {
        using var _ = new LanguageScope(UILanguage.En);
        var vm = MakeModel();
        Assert.Equal("Undo", vm.UndoTitle);
        vm.Selection = vm.Sentences[0].Uid;
        vm.DeleteSelected();
        Assert.StartsWith("Undo ", vm.UndoTitle);
        Assert.NotEqual("Undo Satırı Sil", vm.UndoTitle);
        LanguageState.Shared.Language = UILanguage.Tr;
        Assert.Equal("Satırı Sil İşlemini Geri Al", vm.UndoTitle);
    }

    [Fact]
    public void SpeakerTitles()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        Assert.Equal("Konuşmacı 1", SubtitleViewModel.SpeakerTitle(0));
        Assert.Equal("Konuşmacı yok", SubtitleViewModel.SpeakerTitle(null));
    }
}

public class UndoStackTests
{
    [Fact]
    public void GroupsUndoTogetherInReverseOrder()
    {
        var stack = new UndoStack();
        var log = new List<string>();
        using (stack.BeginGroup())
        {
            stack.Register("a", () => log.Add("undo a"));
            stack.Register("b", () => log.Add("undo b"));
        }
        Assert.Equal("b", stack.UndoActionKey);
        stack.Undo();
        Assert.Equal(["undo b", "undo a"], log);
        Assert.False(stack.CanUndo);
        Assert.False(stack.CanRedo);   // nothing registered while undoing
    }

    [Fact]
    public void RegistrationsWhileUndoingFillRedo()
    {
        var stack = new UndoStack();
        var value = 0;
        void Set(int v)
        {
            var old = value;
            value = v;
            stack.Register("set", () => Set(old));
        }
        Set(1);
        Set(2);
        stack.Undo();
        Assert.Equal(1, value);
        stack.Redo();
        Assert.Equal(2, value);
        stack.Undo();
        stack.Undo();
        Assert.Equal(0, value);
        Assert.False(stack.CanUndo);
        stack.Redo();
        Assert.Equal(1, value);
        stack.Clear();
        Assert.False(stack.CanRedo);
    }
}

public class ModelCatalogTests
{
    [Fact]
    public void ListsByKindAndLanguage()
    {
        var catalog = new ModelCatalog(null);
        Assert.Equal(["nllb-1.3b", "madlad-3b"], catalog.TranslationModels.Select(m => m.Id));
        Assert.Equal(["speaker-diarization"], catalog.DiarizationModels.Select(m => m.Id));
        Assert.DoesNotContain(catalog.TranscriptionModels, m => m.IsTranslation || m.IsDiarization);
        Assert.Equal(["base", "small", "medium", "large-v3-turbo", "large-v3"], catalog.ModelsFor("").Select(m => m.Id));
        Assert.Contains(catalog.ModelsFor("ru"), m => m.Id == "gigaam-v3");
        Assert.DoesNotContain(catalog.ModelsFor("tr"), m => m.Id == "parakeet-v3");
        Assert.Contains(catalog.ModelsFor("pt"), m => m.Id == "canary-1b-v2");
        Assert.Equal(0, catalog.TotalDiskMB);
    }

    [Fact]
    public async Task LoadWithoutEngineReportsNotFound()
    {
        var catalog = new ModelCatalog(null);
        await catalog.LoadAsync();
        Assert.NotNull(catalog.LoadError);
        Assert.Equal(ModelCatalog.FallbackModels, catalog.Models);
    }

    [Fact]
    public async Task LoadErrorKeepsTheList()
    {
        var engine = new FakeEngineClient { ModelsError = EngineException.Failed("boom") };
        var catalog = new ModelCatalog(engine);
        await catalog.LoadAsync();
        Assert.Equal("boom", catalog.LoadError?.Text);
        Assert.Equal(ModelCatalog.FallbackModels, catalog.Models);
        engine.ModelsError = null;
        engine.CatalogModels = [Make.Model("base", downloaded: true, diskMB: 140)];
        await catalog.LoadAsync();
        Assert.Null(catalog.LoadError);
        Assert.Equal(140, catalog.TotalDiskMB);
    }

    [Fact]
    public async Task DeleteMarksRowNotDownloadedAndReturnsFreedMB()
    {
        var engine = new FakeEngineClient
        {
            CatalogModels = [Make.Model("small", downloaded: true, diskMB: 464), Make.Model("base", downloaded: true)],
            Delete = _ => 464,
        };
        var catalog = new ModelCatalog(engine);
        await catalog.LoadAsync();
        var small = catalog.Models[0];
        Assert.True(catalog.CanDelete(small));
        Assert.Equal(464, await catalog.DeleteAsync(small));
        Assert.False(catalog.Models[0].Downloaded);
        Assert.Null(catalog.Models[0].DiskMB);
        Assert.Empty(catalog.Deleting);
        Assert.Equal(["small"], engine.DeleteCalls);
        Assert.False(catalog.TranscriptionRows[0].ShowsDeleteButton);
        // Not downloaded any more: no second call.
        Assert.Null(await catalog.DeleteAsync(catalog.Models[0]));
        Assert.Single(engine.DeleteCalls);
    }

    [Fact]
    public async Task ActiveModelsCannotBeDeleted()
    {
        var engine = new FakeEngineClient { CatalogModels = [Make.Model("base", downloaded: true)] };
        var catalog = new ModelCatalog(engine);
        await catalog.LoadAsync();
        catalog.ActiveModels = new HashSet<string> { "base" };
        Assert.False(catalog.CanDelete(catalog.Models[0]));
        Assert.False(catalog.TranscriptionRows[0].CanDelete);
        Assert.Null(await catalog.DeleteAsync(catalog.Models[0]));
        Assert.Empty(engine.DeleteCalls);
    }

    [Fact]
    public async Task DeleteErrorIsKeptPerModel()
    {
        var engine = new FakeEngineClient
        {
            CatalogModels = [Make.Model("base", downloaded: true)],
            Delete = _ => throw EngineException.Failed("kısmen silindi"),
        };
        var catalog = new ModelCatalog(engine);
        await catalog.LoadAsync();
        Assert.Null(await catalog.DeleteAsync(catalog.Models[0]));
        Assert.Equal("kısmen silindi", catalog.DeleteErrors["base"].Text);
        Assert.Equal("kısmen silindi", catalog.TranscriptionRows[0].ErrorText);
        Assert.True(catalog.Models[0].Downloaded);
    }

    [Fact]
    public void ModelLabels()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var madlad = ModelCatalog.FallbackModels.First(m => m.Id == "madlad-3b");
        Assert.Equal("MADLAD-400 3B", ModelLabel.Name("madlad-3b"));
        Assert.Equal("unknown-id", ModelLabel.Name("unknown-id"));
        Assert.Equal("2,8 GB", ModelLabel.SizeText(2839));
        Assert.Equal("3 GB", ModelLabel.SizeText(3000));
        Assert.Equal("465 MB", ModelLabel.SizeText(465));
        Assert.Equal("tr, en, pt…", ModelLabel.LanguageSummary(madlad));
        Assert.Equal("Tüm diller", ModelLabel.LanguageSummary(ModelCatalog.FallbackModels[0]));
        Assert.Equal("Dilden bağımsız", ModelLabel.LanguageSummary(ModelCatalog.FallbackModels.First(m => m.IsDiarization)));
        Assert.Equal("Lisans: Apache-2.0", ModelLabel.LicenseText(madlad));
        Assert.Equal("Dengeli · ilk kullanımda ~465 MB indirilir", ModelLabel.MenuTitle(ModelCatalog.FallbackModels[2]));
        Assert.Equal("Yüklü — diskte 120 MB", ModelLabel.StorageStatus(Make.Model("x", downloaded: true, diskMB: 120)));
        using var en = new LanguageScope(UILanguage.En);
        Assert.Equal("2.8 GB", ModelLabel.SizeText(2839));
        Assert.True(ModelCatalog.FallbackModels.First(m => m.Id == "nllb-1.3b").HasNonCommercialLicense);
    }
}

public class PresentationTests
{
    [Fact]
    public void SubtitleLanguages()
    {
        using var _ = new LanguageScope(UILanguage.En);
        Assert.Equal(["", "tr", "pt", "es", "ru", "en"], SubtitleLanguage.All.Select(l => l.Code));
        Assert.Equal(["tr", "pt", "es", "ru", "en"], SubtitleLanguage.TranslationTargets.Select(l => l.Code));
        Assert.Null(SubtitleLanguage.FromCode("de"));
        Assert.Equal("Turkish", SubtitleLanguage.Turkish.ShortName);
        LanguageState.Shared.Language = UILanguage.Tr;
        Assert.Equal("Türkçe (tr)", SubtitleLanguage.Turkish.Title);
        Assert.Equal("Portekizce – Brezilya", SubtitleLanguage.Portuguese.TargetName);
    }

    [Fact]
    public void DisplayModesRoundTrip()
    {
        foreach (var mode in SubtitleDisplays.All) Assert.Equal(mode, SubtitleDisplays.Parse(mode.RawValue()));
        Assert.Null(SubtitleDisplays.Parse("x"));
    }

    [Fact]
    public void SpeakerPaletteWrapsAndHandlesNegatives()
    {
        Assert.Equal(SpeakerPalette.Color(0), SpeakerPalette.Color(SpeakerPalette.Count));
        Assert.Equal(SpeakerPalette.Color(SpeakerPalette.Count - 1), SpeakerPalette.Color(-1));
        Assert.Equal("#007AFF", SpeakerPalette.Color(0).Hex);
        Assert.NotEqual(SpeakerPalette.Color(0), SpeakerPalette.Color(0, dark: true));
    }

    [Fact]
    public void SubtitleTimeFormat()
    {
        Assert.Equal("01:05.25", SubtitleTime.Short(65.25));
        Assert.Equal("00:00.00", SubtitleTime.Short(0));
    }

    [Fact]
    public void LocalizedMessageComparesById()
    {
        Assert.Equal(LocalizedMessage.Key("Hazır"), LocalizedMessage.Key("Hazır"));
        Assert.NotEqual(LocalizedMessage.Key("Hazır"), LocalizedMessage.Verbatim("Hazır"));
        using var _ = new LanguageScope(UILanguage.En);
        var message = LocalizedMessage.Key("Hazır");
        Assert.Equal("Ready", message.Text);
        LanguageState.Shared.Language = UILanguage.Tr;
        Assert.Equal("Hazır", message.Text);
    }

    [Fact]
    public void BindableStringsRaiseIndexerOnLanguageChange()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var strings = new LocalizedStrings(LanguageState.Shared);
        var raised = new List<string?>();
        strings.PropertyChanged += (_, e) => raised.Add(e.PropertyName);
        Assert.Equal("Altyazı üret", strings["Altyazı üret"]);
        LanguageState.Shared.Language = UILanguage.En;
        Assert.Contains("Item[]", raised);
        Assert.Equal("Generate Subtitles", strings["Altyazı üret"]);
    }
}

public class AppLanguageAndSettingsTests
{
    [Fact]
    public void AppLanguageAppliesImmediatelyAndPersists()
    {
        using var _ = new LanguageScope(UILanguage.Tr);
        var settings = new InMemorySettingsStore();
        var state = new LanguageState();
        var language = new AppLanguage(settings, () => ["tr-TR"], state);
        Assert.Equal(LanguageChoice.System, language.Choice);
        Assert.Equal(UILanguage.Tr, state.Language);
        language.Choice = LanguageChoice.English;
        Assert.Equal(UILanguage.En, state.Language);
        Assert.Equal("en", settings.GetString("uiLanguage"));
        var reloaded = new AppLanguage(settings, () => ["tr-TR"], state);
        Assert.Equal(LanguageChoice.English, reloaded.Choice);
        reloaded.Choice = LanguageChoice.System;
        Assert.Equal(UILanguage.Tr, state.Language);
        Assert.Equal(UILanguage.En, new AppLanguage(new InMemorySettingsStore(), () => ["de-DE", "fr-FR"], state).Effective);
    }

    [Fact]
    public void JsonFileStoreRoundTripsAndSurvivesCorruption()
    {
        var path = Path.Combine(Path.GetTempPath(), $"subtitle-adder-settings-{Guid.NewGuid()}", "settings.json");
        try
        {
            var store = new JsonFileSettingsStore(path);
            store.SetString("selectedModel", "small");
            store.SetBool("diarizeSpeakers", false);
            store.SetInt("speakerCount", 3);
            store.SetDouble("overlayFontSize", 0.07);
            var reloaded = new JsonFileSettingsStore(path);
            Assert.Equal("small", reloaded.GetString("selectedModel"));
            Assert.False(reloaded.GetBool("diarizeSpeakers"));
            Assert.Equal(3, reloaded.GetInt("speakerCount"));
            Assert.Equal(0.07, reloaded.GetDouble("overlayFontSize"));
            Assert.Null(reloaded.GetString("speakerCount"));   // wrong type reads as missing
            Assert.Equal("small", JsonNode.Parse(File.ReadAllText(path))!["selectedModel"]!.GetValue<string>());

            File.WriteAllText(path, "{ not json");
            var corrupt = new JsonFileSettingsStore(path);
            Assert.Null(corrupt.GetString("selectedModel"));
            corrupt.SetString("selectedModel", "base");
            Assert.Equal("base", new JsonFileSettingsStore(path).GetString("selectedModel"));
        }
        finally
        {
            Directory.Delete(Path.GetDirectoryName(path)!, recursive: true);
        }
    }

    [Fact]
    public void AppearanceSettingsReadClampedValuesAndReset()
    {
        var store = new InMemorySettingsStore();
        var appearance = new AppearanceSettings(store);
        Assert.Equal(ThemeChoice.System, appearance.Theme);
        Assert.Equal(OverlayStyle.Standard, appearance.Style);
        var raised = new List<string?>();
        appearance.PropertyChanged += (_, e) => raised.Add(e.PropertyName);
        appearance.Theme = ThemeChoice.Dark;
        appearance.Size = 1;
        appearance.Background = OverlayBackground.Shadow;
        Assert.Equal("dark", store.GetString("appTheme"));
        Assert.Equal(OverlayStyle.MaxSize, appearance.Style.SizeFraction);
        Assert.Equal("shadow", store.GetString("overlayBackgroundStyle"));
        Assert.Contains(nameof(AppearanceSettings.Theme), raised);
        Assert.Contains(nameof(AppearanceSettings.Style), raised);
        appearance.ResetOverlay();
        Assert.Equal(OverlayStyle.Standard, appearance.Style);
        Assert.Equal(ThemeChoice.Dark, appearance.Theme);
    }
}
