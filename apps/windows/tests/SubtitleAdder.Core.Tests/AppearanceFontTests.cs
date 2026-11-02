using SubtitleAdder.Core;

namespace SubtitleAdder.Core.Tests;

public class AppearanceFontTests
{
    [Fact]
    public void UiFontPersistsIndependentlyAndSurvivesSubtitleReset()
    {
        var store = new InMemorySettingsStore();
        var appearance = new AppearanceSettings(store);
        appearance.FontFamily = "Arial";
        appearance.Background = OverlayBackground.Outline;
        var raised = new List<string?>();
        appearance.PropertyChanged += (_, e) => raised.Add(e.PropertyName);
        appearance.UiFontFamily = "  Verdana  ";
        Assert.Equal("Verdana", appearance.UiFontFamily);
        Assert.Equal("Arial", appearance.Style.FontFamily);
        Assert.Contains(nameof(AppearanceSettings.UiFontFamily), raised);
        Assert.DoesNotContain(nameof(AppearanceSettings.Style), raised);

        var restored = new AppearanceSettings(store);
        Assert.Equal("Verdana", restored.UiFontFamily);
        Assert.Equal(OverlayBackground.Outline, restored.Background);
        restored.ResetOverlay();
        Assert.Equal(OverlayStyle.Standard, restored.Style);
        Assert.Equal("Verdana", restored.UiFontFamily);
        restored.UiFontFamily = "";
        Assert.Equal("", new AppearanceSettings(store).UiFontFamily);
    }

    [Theory]
    [InlineData("box", OverlayBackground.Box)]
    [InlineData("shadow", OverlayBackground.Shadow)]
    [InlineData("outline", OverlayBackground.Outline)]
    public void BackgroundModesRoundTripWithoutChangingExistingChoices(string stored, OverlayBackground expected)
    {
        var appearance = new AppearanceSettings(new InMemorySettingsStore());
        appearance.Background = expected;
        Assert.Equal(expected, appearance.Style.Background);
        Assert.Equal(stored, expected.RawValue());
        Assert.Equal(expected, OverlayBackgrounds.Parse(stored));
        Assert.Contains(expected, OverlayBackgrounds.All);
    }
}
