using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using SubtitleAdder.Core;

namespace SubtitleAdder;

/// <summary>
/// The subtitle text as drawn over the video (and in the Settings preview): semibold, centered, at most three lines,
/// on a rounded box, with a soft shadow, or with an opaque outline drawn using offset glyph copies.
/// </summary>
public sealed class OverlayLabel : Grid
{
    private readonly Border _box = new() { CornerRadius = new CornerRadius(8), Padding = new Thickness(12, 6, 12, 6) };
    private readonly Grid _stack = new();
    private readonly TextBlock _main = MakeText();
    private readonly TextBlock[] _shadows = Enumerable.Range(0, 8).Select(_ => MakeText()).ToArray();
    private static readonly FontFamily DefaultSubtitleFont = new("XamlAutoFontFamily");

    public OverlayLabel()
    {
        HorizontalAlignment = HorizontalAlignment.Center;
        VerticalAlignment = VerticalAlignment.Bottom;
        IsHitTestVisible = false;
        foreach (var shadow in _shadows) _stack.Children.Add(shadow);
        _stack.Children.Add(_main);
        _box.Child = _stack;
        Children.Add(_box);
        Visibility = Visibility.Collapsed;
    }

    private static TextBlock MakeText() => new()
    {
        TextAlignment = TextAlignment.Center,
        HorizontalAlignment = HorizontalAlignment.Center,
        TextWrapping = TextWrapping.Wrap,
        MaxLines = 3,
        TextTrimming = TextTrimming.CharacterEllipsis,
        FontWeight = FontWeights.SemiBold,
        IsTextScaleFactorEnabled = false,
    };

    public void Update(string? text, OverlayStyle style, double videoHeight)
    {
        if (string.IsNullOrEmpty(text))
        {
            Visibility = Visibility.Collapsed;
            return;
        }
        Visibility = Visibility.Visible;
        var size = style.FontSize(videoHeight);
        // Even the system subtitle font is independent of the selected application font.
        var family = string.IsNullOrEmpty(style.FontFamily) ? DefaultSubtitleFont : new FontFamily(style.FontFamily);
        var color = Ui.Color(style.TextColor);
        var box = style.Background == OverlayBackground.Box;
        _box.Background = box
            ? new SolidColorBrush(Windows.UI.Color.FromArgb((byte)Math.Round(style.BackgroundOpacity * 255), 0, 0, 0))
            : null;

        var radius = style.OutlineThickness(videoHeight);
        var diagonal = radius / Math.Sqrt(2);
        (double X, double Y, double Opacity)[] offsets = style.Background switch
        {
            OverlayBackground.Box => [],
            OverlayBackground.Outline =>
            [
                (-radius, 0, 1), (radius, 0, 1), (0, -radius, 1), (0, radius, 1),
                (-diagonal, -diagonal, 1), (diagonal, -diagonal, 1),
                (-diagonal, diagonal, 1), (diagonal, diagonal, 1),
            ],
            _ => [(0, 1.5, style.ShadowOpacity), (0, 0.8, 0.8), (-0.8, 0, 0.8), (0.8, 0, 0.8)],
        };
        for (var i = 0; i < _shadows.Length; i++)
        {
            var shadow = _shadows[i];
            if (i >= offsets.Length)
            {
                shadow.Visibility = Visibility.Collapsed;
                continue;
            }
            shadow.Visibility = Visibility.Visible;
            Configure(shadow, text, size, family, Windows.UI.Color.FromArgb(255, 0, 0, 0));
            shadow.Opacity = offsets[i].Opacity;
            shadow.RenderTransform = new TranslateTransform { X = offsets[i].X, Y = offsets[i].Y };
        }
        Configure(_main, text, size, family, color);
    }

    private static void Configure(TextBlock block, string text, double size, FontFamily family, Windows.UI.Color color)
    {
        block.Text = text;
        block.FontSize = size;
        block.FontFamily = family;
        block.Foreground = new SolidColorBrush(color);
    }
}
