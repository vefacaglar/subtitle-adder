using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;

namespace SubtitleAdder;

/// <summary>
/// Horizontal splitter between the player and the line list (the VSplitView of the macOS app): dragging resizes
/// <see cref="Target"/> within <see cref="Minimum"/> and the space left for the row below. Both rows are set as star
/// proportions, so the split keeps its ratio (and the row minimums) when the window is resized later.
/// </summary>
public sealed partial class SplitterHandle : Grid
{
    private double _startY;
    private double _startHeight;
    private bool _dragging;

    public SplitterHandle()
    {
        ProtectedCursor = InputSystemCursor.Create(InputSystemCursorShape.SizeNorthSouth);
        Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.Transparent);
        PointerPressed += OnPressed;
        PointerMoved += OnMoved;
        PointerReleased += (_, e) => Stop(e);
        PointerCaptureLost += (_, _) => _dragging = false;
    }

    public RowDefinition? Target { get; set; }
    public RowDefinition? Other { get; set; }
    public double Minimum { get; set; } = 200;
    public double OtherMinimum { get; set; } = 180;

    private void OnPressed(object sender, PointerRoutedEventArgs e)
    {
        if (Target is null || Parent is not FrameworkElement) return;
        _dragging = CapturePointer(e.Pointer);
        _startY = e.GetCurrentPoint(null).Position.Y;
        _startHeight = Target.ActualHeight;
        e.Handled = true;
    }

    private void OnMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging || Target is null || Parent is not FrameworkElement parent) return;
        var available = parent.ActualHeight - ActualHeight;
        var height = Math.Clamp(_startHeight + e.GetCurrentPoint(null).Position.Y - _startY, Minimum, Math.Max(Minimum, available - OtherMinimum));
        if (Other is not null)
        {
            var other = Math.Max(1, available - height);
            Target.Height = new GridLength(height, GridUnitType.Star);
            Other.Height = new GridLength(other, GridUnitType.Star);
        }
        else
        {
            Target.Height = new GridLength(height, GridUnitType.Pixel);
        }
        e.Handled = true;
    }

    private void Stop(PointerRoutedEventArgs e)
    {
        _dragging = false;
        ReleasePointerCapture(e.Pointer);
    }
}
