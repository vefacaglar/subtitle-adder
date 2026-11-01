namespace SubtitleAdder.EngineKit.Tests;

/// <summary>Every engine launch must carry <c>SUBTITLE_ENGINE_LANG</c> so engine error messages follow the UI language.</summary>
public class LanguageEnvTests
{
    /// <summary>The fake engine reports the env var it received as its error message (or as stdout for render).</summary>
    private static EngineClient EchoEngine(UILanguage? language) => Fake.Engine("""
        drain
        out {"event": "error", "message": "lang=${SUBTITLE_ENGINE_LANG}"}
        exit 1
        """, language);

    private static readonly Sentence[] Sample = [new(1, 0, 1, "A")];

    private static string Expected(UILanguage language) => $"lang={language.Code()}";

    public static TheoryData<UILanguage> Languages => new() { UILanguage.Tr, UILanguage.En };

    [Theory]
    [MemberData(nameof(Languages))]
    public async Task TranscribePassesLanguage(UILanguage language)
    {
        var error = await Assert.ThrowsAsync<EngineException>(() => Fake.Drain(EchoEngine(language).Transcribe(@"C:\v.mp4", Path.GetTempPath())));
        Assert.Equal(Expected(language), error.Message);
    }

    [Theory]
    [MemberData(nameof(Languages))]
    public async Task TranslatePassesLanguage(UILanguage language)
    {
        var error = await Assert.ThrowsAsync<EngineException>(() => Fake.Drain(EchoEngine(language).Translate(Sample, "en")));
        Assert.Equal(Expected(language), error.Message);
    }

    [Theory]
    [MemberData(nameof(Languages))]
    public async Task ModelsPassesLanguage(UILanguage language)
    {
        var error = await Assert.ThrowsAsync<EngineException>(() => EchoEngine(language).ModelsAsync());
        Assert.Equal(Expected(language), error.Message);
    }

    [Theory]
    [MemberData(nameof(Languages))]
    public async Task DeleteModelPassesLanguage(UILanguage language)
    {
        var error = await Assert.ThrowsAsync<EngineException>(() => EchoEngine(language).DeleteModelAsync("small"));
        Assert.Equal(Expected(language), error.Message);
    }

    [Fact]
    public async Task RenderPassesLanguage()
    {
        var engine = Fake.Engine("drain\nout ${SUBTITLE_ENGINE_LANG}", UILanguage.En);
        Assert.Equal("en", (await engine.RenderAsync(Sample, SubtitleFormat.Srt)).Trim());
    }

    [Fact]
    public void EnvironmentKeepsParentVariablesAndOverridesLanguage()
    {
        var engine = new EngineClient(new EngineCommand(@"C:\x"), UILanguage.En);
        var env = EngineClient.ProcessEnvironment(engine.ResolvedLanguage(), offline: true,
            new Dictionary<string, string?> { ["PATH"] = @"C:\bin", ["SUBTITLE_ENGINE_LANG"] = "tr", ["SUBTITLE_ENGINE_OFFLINE"] = "0" });
        Assert.Equal("en", env["SUBTITLE_ENGINE_LANG"]);
        Assert.Equal("1", env["SUBTITLE_ENGINE_OFFLINE"]);
        Assert.Equal(@"C:\bin", env["PATH"]);
    }

    [Fact]
    public async Task LaunchPassesOfflineMode()
    {
        var engine = Fake.Engine("drain\nout ${SUBTITLE_ENGINE_OFFLINE}", UILanguage.En);
        engine.Offline = true;
        Assert.Equal("1", (await engine.RenderAsync(Sample, SubtitleFormat.Srt)).Trim());
        engine.Offline = false;
        Assert.Equal("0", (await engine.RenderAsync(Sample, SubtitleFormat.Srt)).Trim());
    }

    [Fact]
    public void OfflineFollowsSharedStateUnlessSet()
    {
        var engine = new EngineClient(new EngineCommand(@"C:\x"));
        var saved = OfflineState.Shared.Enabled;
        try
        {
            OfflineState.Shared.Enabled = true;
            Assert.True(engine.ResolvedOffline());
            OfflineState.Shared.Enabled = false;
            Assert.False(engine.ResolvedOffline());
            engine.Offline = true;
            Assert.True(engine.ResolvedOffline());
        }
        finally
        {
            OfflineState.Shared.Enabled = saved;
        }
    }

    [Fact]
    public void DefaultFollowsCurrentUILanguage()
    {
        var follow = new EngineClient(new EngineCommand(@"C:\x"));
        Assert.Equal(UILanguage.En, follow.ResolvedLanguage(UILanguage.En));
        Assert.Equal(UILanguage.Tr, follow.ResolvedLanguage(UILanguage.Tr));
        var fix = new EngineClient(new EngineCommand(@"C:\x"), UILanguage.En);
        Assert.Equal(UILanguage.En, fix.ResolvedLanguage(UILanguage.Tr));
    }

    [Fact]
    public void SystemLanguageFallsBackToEnglishUnlessTurkish()
    {
        Assert.Equal(UILanguage.Tr, UILanguages.System(["tr-TR", "en-US"]));
        Assert.Equal(UILanguage.Tr, UILanguages.System(["tr"]));
        // The first preferred language the app supports wins.
        Assert.Equal(UILanguage.Tr, UILanguages.System(["de-DE", "tr-TR"]));
        Assert.Equal(UILanguage.En, UILanguages.System(["de-DE", "en-US", "tr-TR"]));
        Assert.Equal(UILanguage.En, UILanguages.System(["pt-BR"]));
        Assert.Equal(UILanguage.En, UILanguages.System([]));
        Assert.Equal(UILanguage.Tr, UILanguages.System(["tr_TR"]));
    }

    [Fact]
    public void EngineStringsAreTranslated()
    {
        Assert.Equal("The engine did not output a model list.", EngineStrings.Get("Motor model listesi yazmadı.", UILanguage.En));
        Assert.Equal("Motor model listesi yazmadı.", EngineStrings.Get("Motor model listesi yazmadı.", UILanguage.Tr));
        Assert.Equal("The engine exited with an error code (3). x", EngineStrings.Get("Motor hata koduyla çıktı (%lld). %@", UILanguage.En, 3, "x"));
        Assert.Equal("Motor hata koduyla çıktı (3). x", EngineStrings.Get("Motor hata koduyla çıktı (%lld). %@", UILanguage.Tr, 3, "x"));
    }

    [Fact]
    public void MissingKeyFallsBackToKey()
    {
        Assert.Equal("Yok böyle bir şey 5", EngineStrings.Get("Yok böyle bir şey %d", UILanguage.En, 5));
    }

    [Fact]
    public void CatalogsAreSearchedInOrder()
    {
        var windows = StringCatalog.Parse("""
            {"sourceLanguage": "tr", "strings": {"Motor model listesi yazmadı.": {"localizations": {"en": {"stringUnit": {"state": "translated", "value": "Override"}}}}}}
            """);
        Assert.Equal("Override", UILanguage.En.Localize("Motor model listesi yazmadı.", [windows, StringCatalog.EngineKit]));
        // tr is missing in the first catalog, so the shared one answers.
        Assert.Equal("Motor model listesi yazmadı.", UILanguage.Tr.Localize("Motor model listesi yazmadı.", [windows, StringCatalog.EngineKit]));
        Assert.True(StringCatalog.EngineKit.Keys.Count >= 5);
    }

    [Fact]
    public void PrintfSpecifiersAreConverted()
    {
        var tr = UILanguage.Tr.Culture();
        Assert.Equal("%42", Printf.Format("%%%lld", tr, 42L));
        Assert.Equal("42%", Printf.Format("%lld%%", tr, 42));
        Assert.Equal("b a 7", Printf.Format("%2$@ %1$@ %3$ld", tr, "a", "b", 7));
        Assert.Equal("x 3", Printf.Format("%@ %d", tr, "x", 3));
        Assert.Equal("1,5", Printf.Format("%.1f", tr, 1.5));
        Assert.Equal("1.5", Printf.Format("%.1f", UILanguage.En.Culture(), 1.5));
        Assert.Equal("trailing %", Printf.Format("trailing %", tr, 1));
        // No arguments: the text is returned unformatted, like the macOS lookup.
        Assert.Equal("%lld%%", UILanguage.Tr.Localize("%lld%%", [StringCatalog.Empty]));
    }

    [Fact]
    public void LanguageStateNotifiesChanges()
    {
        var state = new LanguageState();
        var changes = new List<UILanguage>();
        var properties = new List<string?>();
        state.LanguageChanged += (_, language) => changes.Add(language);
        state.PropertyChanged += (_, e) => properties.Add(e.PropertyName);
        Assert.Equal(UILanguage.Tr, state.Language);
        state.Language = UILanguage.En;
        state.Language = UILanguage.En;
        state.Language = UILanguage.Tr;
        Assert.Equal([UILanguage.En, UILanguage.Tr], changes);
        Assert.Equal(["Language", "Language"], properties);
    }

    [Fact]
    public void ParsesLanguageCodes()
    {
        Assert.Equal(UILanguage.En, UILanguages.Parse("EN"));
        Assert.Equal(UILanguage.Tr, UILanguages.Parse("tr"));
        Assert.Null(UILanguages.Parse("de"));
        Assert.Equal("tr", UILanguage.Tr.Code());
    }
}
