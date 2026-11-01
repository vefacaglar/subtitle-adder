using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;

namespace SubtitleAdder;

/// <summary>
/// Splitter between the player and the line list: supports both vertical (top/bottom) and horizontal (side-by-side)
/// layouts. Dragging resizes <see cref="Target"/> / <see cref="TargetColumn"/> within <see cref="Minimum"/> and the
/// space left for the other pane. Both panes are set as star proportions so the ratio is preserved when the window resizes.
/// </summary>
public sealed partial class SplitterHandle : Grid
{
    private double _startPos;
    private double _startSize;
    private bool _dragging;
    private bool _isSideBySide;

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
    public ColumnDefinition? TargetColumn { get; set; }
    public ColumnDefinition? OtherColumn { get; set; }
    public double Minimum { get; set; } = 200;
    public double OtherMinimum { get; set; } = 180;

    public bool IsSideBySide
    {
        get => _isSideBySide;
        set
        {
            if (_isSideBySide == value) return;
            _isSideBySide = value;
            ProtectedCursor = InputSystemCursor.Create(
                value ? InputSystemCursorShape.SizeWestEast : InputSystemCursorShape.SizeNorthSouth);
        }
    }

    private void OnPressed(object sender, PointerRoutedEventArgs e)
    {
        if (Parent is not FrameworkElement) return;
        if (_isSideBySide)
        {
            if (TargetColumn is null) return;
            _dragging = CapturePointer(e.Pointer);
            _startPos = e.GetCurrentPoint(null).Position.X;
            _startSize = TargetColumn.ActualWidth;
        }
        else
        {
            if (Target is null) return;
            _dragging = CapturePointer(e.Pointer);
            _startPos = e.GetCurrentPoint(null).Position.Y;
            _startSize = Target.ActualHeight;
        }
        e.Handled = true;
    }

    private void OnMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging || Parent is not FrameworkElement parent) return;
        if (_isSideBySide)
        {
            if (TargetColumn is null) return;
            var available = parent.ActualWidth - ActualWidth;
            var width = Math.Clamp(_startSize + e.GetCurrentPoint(null).Position.X - _startPos, Minimum, Math.Max(Minimum, available - OtherMinimum));
            if (OtherColumn is not null)
            {
                var other = Math.Max(1, available - width);
                TargetColumn.Width = new GridLength(width, GridUnitType.Star);
                OtherColumn.Width = new GridLength(other, GridUnitType.Star);
            }
            else
            {
                TargetColumn.Width = new GridLength(width, GridUnitType.Pixel);
            }
        }
        else
        {
            if (Target is null) return;
            var available = parent.ActualHeight - ActualHeight;
            var height = Math.Clamp(_startSize + e.GetCurrentPoint(null).Position.Y - _startPos, Minimum, Math.Max(Minimum, available - OtherMinimum));
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
        }
        e.Handled = true;
    }

    private void Stop(PointerRoutedEventArgs e)
    {
        _dragging = false;
        ReleasePointerCapture(e.Pointer);
    }
}
