using System.Globalization;
using CommunityToolkit.Mvvm.ComponentModel;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

/// <summary>Settings keys for the appearance settings (same as the macOS UserDefaults keys).</summary>
public static class AppearanceKeys
{
    public const string Theme = "appTheme";
    public const string OverlayFont = "overlayFontFamily";
    public const string OverlaySize = "overlayFontSize";
    public const string OverlayTextColor = "overlayTextColor";
    public const string OverlayBackground = "overlayBackgroundStyle";
    public const string OverlayOpacity = "overlayBackgroundOpacity";
}

/// <summary>App theme choice. "System" follows Windows (the app maps it to ElementTheme.Default/Light/Dark).</summary>
public enum ThemeChoice
{
    System,
    Light,
    Dark,
}

public static class ThemeChoices
{
    public static IReadOnlyList<ThemeChoice> All { get; } = [ThemeChoice.System, ThemeChoice.Light, ThemeChoice.Dark];

    public static string RawValue(this ThemeChoice theme) => theme switch
    {
        ThemeChoice.Light => "light",
        ThemeChoice.Dark => "dark",
        _ => "system",
    };

    public static ThemeChoice? Parse(string? raw) => raw switch
    {
        "system" => ThemeChoice.System,
        "light" => ThemeChoice.Light,
        "dark" => ThemeChoice.Dark,
        _ => null,
    };

    public static string Title(this ThemeChoice theme) => theme switch
    {
        ThemeChoice.Light => L("Açık"),
        ThemeChoice.Dark => L("Koyu"),
        _ => L("Sistem"),
    };
}

public enum OverlayBackground
{
    /// <summary>Rounded box behind the text.</summary>
    Box,

    /// <summary>No box; a soft shadow keeps the text readable.</summary>
    Shadow,
}

public static class OverlayBackgrounds
{
    public static IReadOnlyList<OverlayBackground> All { get; } = [OverlayBackground.Box, OverlayBackground.Shadow];

    public static string RawValue(this OverlayBackground background) =>
        background == OverlayBackground.Shadow ? "shadow" : "box";

    public static OverlayBackground? Parse(string? raw) => raw switch
    {
        "box" => OverlayBackground.Box,
        "shadow" => OverlayBackground.Shadow,
        _ => null,
    };

    public static string Title(this OverlayBackground background) =>
        background == OverlayBackground.Shadow ? L("Gölge") : L("Kutu");
}

/// <summary>How the subtitle over the video looks. Only affects the in-app preview, never exported files.</summary>
public sealed record OverlayStyle
{
    public const double MinSize = 0.03;
    public const double MaxSize = 0.09;
    public const double DefaultSize = 0.05;
    public const string DefaultTextHex = "#FFFFFF";
    public const double DefaultOpacity = 0.6;

    /// <summary>Builds a style from raw values; out-of-range or malformed values fall back to safe ones.</summary>
    public OverlayStyle(string fontFamily, double sizeFraction, string textHex, OverlayBackground background, double backgroundOpacity)
    {
        FontFamily = fontFamily;
        SizeFraction = double.IsFinite(sizeFraction) ? Math.Clamp(sizeFraction, MinSize, MaxSize) : DefaultSize;
        TextHex = HexColor.Parse(textHex) is null ? DefaultTextHex : textHex;
        Background = background;
        BackgroundOpacity = double.IsFinite(backgroundOpacity) ? Math.Clamp(backgroundOpacity, 0, 1) : DefaultOpacity;
    }

    /// <summary>From stored values (unknown background names fall back to the box).</summary>
    public OverlayStyle(string storedFamily, double size, string textHex, string background, double opacity)
        : this(storedFamily, size, textHex, OverlayBackgrounds.Parse(background) ?? OverlayBackground.Box, opacity) { }

    public static OverlayStyle Standard { get; } = new("", DefaultSize, DefaultTextHex, OverlayBackground.Box, DefaultOpacity);

    /// <summary>Empty means the system font (Segoe UI Variable on Windows 11).</summary>
    public string FontFamily { get; }

    /// <summary>Font size as a fraction of the video height.</summary>
    public double SizeFraction { get; }

    public string TextHex { get; }
    public OverlayBackground Background { get; }
    public double BackgroundOpacity { get; }

    public double FontSize(double videoHeight) => Math.Max(12, videoHeight * SizeFraction);

    /// <summary>The text color; white if the stored value is malformed.</summary>
    public RgbColor TextColor => HexColor.ParseRgb(TextHex) ?? new RgbColor(255, 255, 255);

    /// <summary>Shadow opacity in the shadow style (never fainter than 0.5 so the text stays readable).</summary>
    public double ShadowOpacity => Math.Max(BackgroundOpacity, 0.5);
}

/// <summary>"#RRGGBB" conversions for the stored overlay text color. Components are 0…1.</summary>
public static class HexColor
{
    public static (double R, double G, double B)? Parse(string hex)
    {
        var text = hex.Trim();
        if (text.StartsWith('#')) text = text[1..];
        if (text.Length != 6 || !uint.TryParse(text, NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out var value))
            return null;
        return (((value >> 16) & 0xFF) / 255.0, ((value >> 8) & 0xFF) / 255.0, (value & 0xFF) / 255.0);
    }

    public static RgbColor? ParseRgb(string hex) =>
        Parse(hex) is var (r, g, b) ? new RgbColor(Byte(r), Byte(g), Byte(b)) : null;

    public static string Format(double r, double g, double b) => $"#{Byte(r):X2}{Byte(g):X2}{Byte(b):X2}";

    public static string Format(RgbColor color) => color.Hex;

    private static byte Byte(double v) => (byte)Math.Round(Math.Clamp(v, 0, 1) * 255, MidpointRounding.AwayFromZero);
}

/// <summary>
/// The stored appearance settings (theme and overlay style), read from and written to <see cref="ISettingsStore"/>.
/// The app applies <see cref="Theme"/> to its windows and draws the overlay with <see cref="Style"/>.
/// </summary>
public sealed class AppearanceSettings : ObservableObject
{
    private readonly ISettingsStore _store;

    public AppearanceSettings(ISettingsStore store)
    {
        _store = store;
        _store.Changed += (_, key) =>
        {
            if (key == AppearanceKeys.Theme) OnPropertyChanged(nameof(Theme));
            else if (key.StartsWith("overlay", StringComparison.Ordinal)) RaiseStyle();
        };
    }

    public ThemeChoice Theme
    {
        get => ThemeChoices.Parse(_store.GetString(AppearanceKeys.Theme)) ?? ThemeChoice.System;
        set => _store.SetString(AppearanceKeys.Theme, value.RawValue());
    }

    /// <summary>Empty means the system font.</summary>
    public string FontFamily
    {
        get => _store.GetString(AppearanceKeys.OverlayFont) ?? "";
        set => _store.SetString(AppearanceKeys.OverlayFont, value);
    }

    /// <summary>Stored size fraction (clamped in <see cref="Style"/>).</summary>
    public double Size
    {
        get => Style.SizeFraction;
        set => _store.SetDouble(AppearanceKeys.OverlaySize, value);
    }

    public string TextHex
    {
        get => Style.TextHex;
        set => _store.SetString(AppearanceKeys.OverlayTextColor, value);
    }

    public OverlayBackground Background
    {
        get => Style.Background;
        set => _store.SetString(AppearanceKeys.OverlayBackground, value.RawValue());
    }

    public double Opacity
    {
        get => Style.BackgroundOpacity;
        set => _store.SetDouble(AppearanceKeys.OverlayOpacity, value);
    }

    /// <summary>"%42"-style label of the size slider.</summary>
    public string SizeText => L("%lld%%", (long)Math.Round(Style.SizeFraction * 100, MidpointRounding.AwayFromZero));

    /// <summary>Label of the opacity slider.</summary>
    public string OpacityText => L("%lld%%", (long)Math.Round(Style.BackgroundOpacity * 100, MidpointRounding.AwayFromZero));

    public OverlayStyle Style => new(
        _store.GetString(AppearanceKeys.OverlayFont) ?? "",
        _store.GetDouble(AppearanceKeys.OverlaySize) ?? OverlayStyle.DefaultSize,
        _store.GetString(AppearanceKeys.OverlayTextColor) ?? OverlayStyle.DefaultTextHex,
        _store.GetString(AppearanceKeys.OverlayBackground) ?? OverlayBackground.Box.RawValue(),
        _store.GetDouble(AppearanceKeys.OverlayOpacity) ?? OverlayStyle.DefaultOpacity);

    /// <summary>"Varsayılana döndür": resets the overlay settings (not the theme).</summary>
    public void ResetOverlay()
    {
        foreach (var key in new[]
                 {
                     AppearanceKeys.OverlayFont, AppearanceKeys.OverlaySize, AppearanceKeys.OverlayTextColor,
                     AppearanceKeys.OverlayBackground, AppearanceKeys.OverlayOpacity,
                 })
            _store.Remove(key);
    }

    /// <summary>Re-raises the localized labels after a UI language switch.</summary>
    public void RefreshLocalization()
    {
        OnPropertyChanged(nameof(SizeText));
        OnPropertyChanged(nameof(OpacityText));
    }

    private void RaiseStyle()
    {
        foreach (var name in new[] { nameof(FontFamily), nameof(Size), nameof(TextHex), nameof(Background), nameof(Opacity), nameof(SizeText), nameof(OpacityText), nameof(Style) })
            OnPropertyChanged(name);
    }
}
