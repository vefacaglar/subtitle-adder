using SubtitleAdder.Core;

namespace SubtitleAdder.Core.Tests;

/// <summary>Port of apps/macos/Tests/SubtitleAdderAppTests/LocalizationAndAppearanceTests.swift.</summary>
public class LocalizationAndAppearanceTests
{
    [Fact]
    public void AppCatalogHasEnglishAndTurkish()
    {
        Assert.Equal("Generate Subtitles", Strings.Localize(UILanguage.En, "Altyazı üret"));
        Assert.Equal("Altyazı üret", Strings.Localize(UILanguage.Tr, "Altyazı üret"));
        Assert.Equal("Lines: 3 · language: tr", Strings.Localize(UILanguage.En, "%lld satır · dil: %@", 3, "tr"));
        Assert.Equal("Selected Fast for Turkish.", Strings.Localize(UILanguage.En, "%@ için %@ seçildi.", "Turkish", "Fast"));
        Assert.Equal("Translating… 42%", Strings.Localize(UILanguage.En, "Çevriliyor… %lld%%", 42));
        Assert.Equal("Çevriliyor… %42", Strings.Localize(UILanguage.Tr, "Çevriliyor… %lld%%", 42));
        // A key missing from the catalog falls back to the Turkish source text.
        Assert.Equal("Katalogda yok", Strings.Localize(UILanguage.En, "Katalogda yok"));
    }

    [Fact]
    public void LanguageChoiceResolution()
    {
        Assert.Equal(UILanguage.Tr, LanguageChoice.Turkish.Resolved(["en-US"]));
        Assert.Equal(UILanguage.En, LanguageChoice.English.Resolved(["tr-TR"]));
        Assert.Equal(UILanguage.Tr, LanguageChoice.System.Resolved(["tr-TR"]));
        Assert.Equal(UILanguage.En, LanguageChoice.System.Resolved(["fr-FR"]));
    }

    [Fact]
    public void ThemeMapping()
    {
        // macOS maps to NSAppearance names; Windows stores the same raw values and maps them to ElementTheme in the app.
        Assert.Equal("system", ThemeChoice.System.RawValue());
        Assert.Equal("light", ThemeChoice.Light.RawValue());
        Assert.Equal("dark", ThemeChoice.Dark.RawValue());
        Assert.Equal(ThemeChoice.Dark, ThemeChoices.Parse("dark"));
        Assert.Null(ThemeChoices.Parse("sepia"));
    }

    [Fact]
    public void OverlayStyleClampsStoredValues()
    {
        var style = new OverlayStyle("Menlo", 5, "zzz", "nope", -1);
        Assert.Equal(OverlayStyle.MaxSize, style.SizeFraction);
        Assert.Equal(OverlayStyle.DefaultTextHex, style.TextHex);
        Assert.Equal(OverlayBackground.Box, style.Background);
        Assert.Equal(0, style.BackgroundOpacity);
        Assert.Equal(OverlayStyle.DefaultSize, new OverlayStyle("", double.NaN, "#000000", "shadow", 7).SizeFraction);
        Assert.Equal(1, new OverlayStyle("", 0, "#000000", "shadow", 7).BackgroundOpacity);
    }

    [Fact]
    public void OverlayFontSize()
    {
        var style = OverlayStyle.Standard;
        Assert.Equal(50, style.FontSize(1000), 9);
        Assert.Equal(12, style.FontSize(100));  // minimum readable size
    }

    [Fact]
    public void HexColorRoundTrip()
    {
        var parsed = HexColor.Parse("#FF8000");
        Assert.NotNull(parsed);
        var (r, g, b) = parsed.Value;
        Assert.Equal(1, r);
        Assert.True(Math.Abs(g - 128.0 / 255) < 0.0001);
        Assert.Equal("#FF8000", HexColor.Format(r, g, b));
        Assert.Null(HexColor.Parse("12345"));
        Assert.Null(HexColor.Parse("#GGGGGG"));
        Assert.Equal(1, HexColor.Parse("00ff00")?.G);
    }
}

/// <summary>Port of OverlayTextTests.swift.</summary>
public class OverlayTextTests
{
    private readonly Sentence[] _list =
    [
        new(1, 0, 1, "Hello.", speaker: 0),
        new(2, 1, 2, "More.", speaker: 0),
        new(3, 2, 3, "- Hi.", speaker: 1),
    ];

    private Dictionary<Guid, TranslationEntry> Translations() =>
        _list.ToDictionary(s => s.Uid, s => new TranslationEntry("T:" + s.Text, s.Text, "es"));

    private string? Text(int index, SubtitleDisplay display) =>
        OverlayText.Compose(_list, Translations(), display, _list[index].Uid);

    [Fact]
    public void DashFollowsTurnRuleOnOriginal()
    {
        Assert.Equal("- Hello.", Text(0, SubtitleDisplay.Original));
        Assert.Equal("More.", Text(1, SubtitleDisplay.Original));
    }

    [Fact]
    public void AlreadyDashedShownTextGetsNoSecondDash()
    {
        Assert.Equal("- Hi.", Text(2, SubtitleDisplay.Original));
        // The translation text has no dash of its own, so it does get one.
        Assert.Equal("- T:- Hi.", Text(2, SubtitleDisplay.Translation));
    }

    [Fact]
    public void BothModeDashesEachLine()
    {
        Assert.Equal("- Hello.\n- T:Hello.", Text(0, SubtitleDisplay.Both));
        Assert.Equal("More.\nT:More.", Text(1, SubtitleDisplay.Both));
    }

    [Fact]
    public void TranslationFallsBackToOriginalWithoutEntry()
    {
        var output = OverlayText.Compose(_list, new Dictionary<Guid, TranslationEntry>(), SubtitleDisplay.Translation, _list[0].Uid);
        Assert.Equal("- Hello.", output);
    }

    [Fact]
    public void NoDashWithoutSpeakers()
    {
        Sentence[] plain = [new(1, 0, 1, "A")];
        Assert.Equal("A", OverlayText.Compose(plain, new Dictionary<Guid, TranslationEntry>(), SubtitleDisplay.Original, plain[0].Uid));
    }

    [Fact]
    public void UnknownUidGivesNothing()
    {
        Assert.Null(OverlayText.Compose(_list, Translations(), SubtitleDisplay.Original, Guid.NewGuid()));
    }
}

/// <summary>Port of SpeakerEditingTests.swift (UndoManager grouping → one UndoStack group per command).</summary>
public class SpeakerEditingTests
{
    private static SubtitleViewModel MakeModel()
    {
        var (vm, _, _) = Make.ViewModel(engine: false);
        vm.Sentences =
        [
            new Sentence(1, 0, 4, "one two three four", speaker: 0),
            new Sentence(2, 4, 6, "reply", speaker: 1),
        ];
        return vm;
    }

    private static int?[] Speakers(SubtitleViewModel vm) => [.. vm.Sentences.Select(s => s.Speaker)];

    [Fact]
    public void ChangeSpeakerUndoAndRedo()
    {
        var vm = MakeModel();
        vm.SetSpeaker(1, vm.Sentences[0].Uid);
        Assert.Equal([1, 1], Speakers(vm));
        vm.Undo();
        Assert.Equal([0, 1], Speakers(vm));
        vm.Redo();
        Assert.Equal([1, 1], Speakers(vm));
    }

    [Fact]
    public void NewSpeakerGetsUnusedIndexAndUndoes()
    {
        var vm = MakeModel();
        vm.AssignNewSpeaker(vm.Sentences[1].Uid);
        Assert.Equal([0, 2], Speakers(vm));
        vm.Undo();
        Assert.Equal([0, 1], Speakers(vm));
    }

    [Fact]
    public void MergeKeepsFirstSpeakerAndUndoRestores()
    {
        var vm = MakeModel();
        vm.Selection = vm.Sentences[0].Uid;
        vm.MergeSelectedWithNext();
        Assert.Equal([0], Speakers(vm));
        vm.Undo();
        Assert.Equal([0, 1], Speakers(vm));
    }

    [Fact]
    public void SplitKeepsSpeakerOnBothHalvesAndUndoRestores()
    {
        var vm = MakeModel();
        vm.Selection = vm.Sentences[0].Uid;
        vm.SplitSelected(2);
        Assert.Equal([0, 0, 1], Speakers(vm));
        vm.Undo();
        Assert.Equal([0, 1], Speakers(vm));
    }

    [Fact]
    public void RemoveSpeakerUndoes()
    {
        var vm = MakeModel();
        vm.SetSpeaker(null, vm.Sentences[1].Uid);
        Assert.Equal([0, null], Speakers(vm));
        vm.Undo();
        Assert.Equal([0, 1], Speakers(vm));
    }

    [Fact]
    public void SpeakerModelIsProtectedWhileDiarizing()
    {
        var catalog = new ModelCatalog(null) { ActiveModels = new HashSet<string> { "base", SubtitleViewModel.DiarizationModelId } };
        Assert.Contains("speaker-diarization", catalog.ActiveModels);
    }
}
