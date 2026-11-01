using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using SubtitleAdder.Core;

namespace SubtitleAdder;

/// <summary>
/// The subtitle text as drawn over the video (and in the Settings preview): semibold, centered, at most three lines,
/// either on a rounded box or with a soft shadow. XAML text has no shadow, so the shadow is drawn with offset copies.
/// </summary>
public sealed class OverlayLabel : Grid
{
    private readonly Border _box = new() { CornerRadius = new CornerRadius(8), Padding = new Thickness(12, 6, 12, 6) };
    private readonly Grid _stack = new();
    private readonly TextBlock _main = MakeText();
    private readonly TextBlock[] _shadows = [MakeText(), MakeText(), MakeText(), MakeText()];

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
        var family = string.IsNullOrEmpty(style.FontFamily) ? null : new FontFamily(style.FontFamily);
        var color = Ui.Color(style.TextColor);
        var box = style.Background == OverlayBackground.Box;
        _box.Background = box
            ? new SolidColorBrush(Windows.UI.Color.FromArgb((byte)Math.Round(style.BackgroundOpacity * 255), 0, 0, 0))
            : null;

        (double X, double Y, double Opacity)[] offsets = box
            ? []
            : [(0, 1.5, style.ShadowOpacity), (0, 0.8, 0.8), (-0.8, 0, 0.8), (0.8, 0, 0.8)];
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

    private static void Configure(TextBlock block, string text, double size, FontFamily? family, Windows.UI.Color color)
    {
        block.Text = text;
        block.FontSize = size;
        if (family is null) block.ClearValue(TextBlock.FontFamilyProperty);
        else block.FontFamily = family;
        block.Foreground = new SolidColorBrush(color);
    }
}
