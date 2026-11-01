using System.Numerics;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using SubtitleAdder.Core;
using Windows.Foundation;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder;

/// <summary>
/// macOS QuickTime-style floating bottom-center HUD pill:
/// sleek custom scrubber bar on top, optically centered circular Play/Pause button directly underneath,
/// and a matching minimal volume control on the right. Smoothly fades and glides in/out on hover.
/// </summary>
public sealed class PreviewTransportControls : Grid
{
    private readonly SleekScrubber _seek;
    private readonly TextBlock _elapsedText;
    private readonly TextBlock _totalText;
    private readonly Border _playPauseButton;
    private readonly FontIcon _playPauseIcon;
    private readonly Border _muteButton;
    private readonly FontIcon _volumeIcon;
    private readonly SleekScrubber _volumeScrubber;

    private bool _visible;
    private bool _playHovered;
    private bool _playPressed;
    private bool _muteHovered;
    private double _position;
    private double _duration;
    private bool _isPlaying;
    private bool _isMuted;
    private double _volume = 1.0;

    public event Action<double>? SeekRequested;
    public event Action? PlayPauseRequested;
    public event Action? MuteToggleRequested;
    public event Action<double>? VolumeRequested;
    public event Action? InteractionChanged;

    public bool IsInteracting => _seek.IsScrubbing || _volumeScrubber.IsScrubbing;

    public PreviewTransportControls()
    {
        Width = 336;
        Padding = new Thickness(16, 10, 16, 9);
        CornerRadius = new CornerRadius(16);

        var bg = new LinearGradientBrush { StartPoint = new Point(0, 0), EndPoint = new Point(0, 1) };
        bg.GradientStops.Add(new GradientStop { Color = ColorHelper.FromArgb(0xDC, 0x24, 0x24, 0x29), Offset = 0 });
        bg.GradientStops.Add(new GradientStop { Color = ColorHelper.FromArgb(0xEA, 0x14, 0x14, 0x18), Offset = 1 });
        Background = bg;

        var border = new LinearGradientBrush { StartPoint = new Point(0, 0), EndPoint = new Point(0, 1) };
        border.GradientStops.Add(new GradientStop { Color = ColorHelper.FromArgb(0x44, 0xFF, 0xFF, 0xFF), Offset = 0 });
        border.GradientStops.Add(new GradientStop { Color = ColorHelper.FromArgb(0x18, 0xFF, 0xFF, 0xFF), Offset = 1 });
        BorderBrush = border;
        BorderThickness = new Thickness(1);
        RequestedTheme = ElementTheme.Dark;

        Shadow = new ThemeShadow();
        Opacity = 0;
        IsHitTestVisible = false;
        Translation = new Vector3(0, 8, 24);
        OpacityTransition = new ScalarTransition { Duration = TimeSpan.FromMilliseconds(240) };
        TranslationTransition = new Vector3Transition { Duration = TimeSpan.FromMilliseconds(240) };

        var root = new Grid { RowSpacing = 6 };
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        // Row 0: macOS-style timeline bar with whisper-quiet tabular times at each end.
        var timelineRow = new Grid { ColumnSpacing = 8, VerticalAlignment = VerticalAlignment.Center };
        timelineRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        timelineRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        timelineRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var monoFont = new FontFamily("Segoe UI Variable Text, Segoe UI");
        _elapsedText = new TextBlock
        {
            Text = "00:00",
            FontFamily = monoFont,
            FontSize = 11,
            MinWidth = 31,
            TextAlignment = TextAlignment.Left,
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = new SolidColorBrush(ColorHelper.FromArgb(0xBF, 0xFF, 0xFF, 0xFF)),
            IsTextScaleFactorEnabled = false,
        };
        Grid.SetColumn(_elapsedText, 0);
        timelineRow.Children.Add(_elapsedText);

        _seek = new SleekScrubber(trackRestHeight: 4, trackHoverHeight: 6, thumbSize: 11);
        _seek.ValueChanged += ratio =>
        {
            if (_duration > 0) SeekRequested?.Invoke(ratio * _duration);
        };
        _seek.ScrubbingChanged += () => InteractionChanged?.Invoke();
        AutomationProperties.SetName(_seek, L("Video konumu"));
        Grid.SetColumn(_seek, 1);
        timelineRow.Children.Add(_seek);

        _totalText = new TextBlock
        {
            Text = "--:--",
            FontFamily = monoFont,
            FontSize = 11,
            MinWidth = 31,
            TextAlignment = TextAlignment.Right,
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = new SolidColorBrush(ColorHelper.FromArgb(0x78, 0xFF, 0xFF, 0xFF)),
            IsTextScaleFactorEnabled = false,
        };
        Grid.SetColumn(_totalText, 2);
        timelineRow.Children.Add(_totalText);

        Grid.SetRow(timelineRow, 0);
        root.Children.Add(timelineRow);

        // Row 1: Play/Pause button dead center under the bar, sleek volume control on the right.
        var controlsRow = new Grid();
        controlsRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        controlsRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        controlsRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        _playPauseIcon = new FontIcon
        {
            Glyph = "\uE768",
            FontSize = 14,
            Foreground = new SolidColorBrush(Colors.White),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(1.5, 0, 0, 0),
        };
        _playPauseButton = new Border
        {
            Width = 34,
            Height = 34,
            CornerRadius = new CornerRadius(17),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            BorderThickness = new Thickness(1),
            Child = _playPauseIcon,
        };
        _playPauseButton.PointerEntered += (_, _) => { _playHovered = true; UpdatePlayButtonVisual(); };
        _playPauseButton.PointerExited += (_, _) => { _playHovered = false; _playPressed = false; UpdatePlayButtonVisual(); };
        _playPauseButton.PointerPressed += (_, e) =>
        {
            if (_duration <= 0) return;
            _playPressed = true;
            UpdatePlayButtonVisual();
            e.Handled = true;
        };
        _playPauseButton.PointerReleased += (_, e) =>
        {
            if (!_playPressed) return;
            _playPressed = false;
            UpdatePlayButtonVisual();
            if (_duration > 0) PlayPauseRequested?.Invoke();
            e.Handled = true;
        };
        UpdatePlayButtonVisual();
        Grid.SetColumn(_playPauseButton, 1);
        controlsRow.Children.Add(_playPauseButton);

        var volumePanel = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Center,
            Background = new SolidColorBrush(Colors.Transparent),
        };
        volumePanel.PointerWheelChanged += VolumePanel_PointerWheelChanged;

        _volumeIcon = new FontIcon
        {
            Glyph = "\uE767",
            FontSize = 13,
            Foreground = new SolidColorBrush(ColorHelper.FromArgb(0xDF, 0xFF, 0xFF, 0xFF)),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
        };
        _muteButton = new Border
        {
            Width = 26,
            Height = 26,
            CornerRadius = new CornerRadius(13),
            VerticalAlignment = VerticalAlignment.Center,
            Child = _volumeIcon,
        };
        _muteButton.PointerEntered += (_, _) => { _muteHovered = true; UpdateMuteButtonVisual(); };
        _muteButton.PointerExited += (_, _) => { _muteHovered = false; UpdateMuteButtonVisual(); };
        _muteButton.PointerPressed += (_, e) =>
        {
            MuteToggleRequested?.Invoke();
            e.Handled = true;
        };
        UpdateMuteButtonVisual();

        _volumeScrubber = new SleekScrubber(trackRestHeight: 3.5, trackHoverHeight: 5, thumbSize: 9)
        {
            Width = 56,
            VerticalAlignment = VerticalAlignment.Center,
        };
        _volumeScrubber.SetRatio(1.0);
        _volumeScrubber.ValueChanged += ratio => VolumeRequested?.Invoke(ratio);
        _volumeScrubber.ScrubbingChanged += () => InteractionChanged?.Invoke();

        volumePanel.Children.Add(_muteButton);
        volumePanel.Children.Add(_volumeScrubber);
        Grid.SetColumn(volumePanel, 2);
        controlsRow.Children.Add(volumePanel);

        Grid.SetRow(controlsRow, 1);
        root.Children.Add(controlsRow);

        Children.Add(root);
        Synchronize(0, 0);
    }

    public void SetVisible(bool visible)
    {
        if (_visible == visible) return;
        _visible = visible;
        Opacity = visible ? 1 : 0;
        Translation = visible ? new Vector3(0, 0, 24) : new Vector3(0, 8, 24);
        IsHitTestVisible = visible;
    }

    private void UpdatePlayButtonVisual()
    {
        var enabled = _duration > 0;
        _playPauseButton.Opacity = enabled ? 1.0 : 0.4;
        var fillAlpha = !enabled ? (byte)0x12 : _playPressed ? (byte)0x14 : _playHovered ? (byte)0x34 : (byte)0x1E;
        var strokeAlpha = !enabled ? (byte)0x18 : _playHovered ? (byte)0x4C : (byte)0x28;
        _playPauseButton.Background = new SolidColorBrush(ColorHelper.FromArgb(fillAlpha, 0xFF, 0xFF, 0xFF));
        _playPauseButton.BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(strokeAlpha, 0xFF, 0xFF, 0xFF));
    }

    private void UpdateMuteButtonVisual()
    {
        _muteButton.Background = new SolidColorBrush(ColorHelper.FromArgb(_muteHovered ? (byte)0x24 : (byte)0x00, 0xFF, 0xFF, 0xFF));
        _volumeIcon.Foreground = new SolidColorBrush(ColorHelper.FromArgb(_muteHovered ? (byte)0xFF : (byte)0xD8, 0xFF, 0xFF, 0xFF));
    }

    public void Synchronize(double position, double duration, bool isPlaying = false, bool isMuted = false, double volume = 1.0)
    {
        _duration = double.IsFinite(duration) ? Math.Max(0, duration) : 0;
        _position = PlaybackTimeline.Clamp(position, _duration);
        _isPlaying = isPlaying;
        _isMuted = isMuted;
        _volume = Math.Clamp(double.IsFinite(volume) ? volume : 1.0, 0.0, 1.0);

        _seek.IsInteractive = _duration > 0;
        if (!_seek.IsScrubbing)
        {
            _seek.SetRatio(_duration > 0 ? Math.Clamp(_position / _duration, 0.0, 1.0) : 0.0);
        }

        var hours = _duration >= 3600;
        var displayPos = _seek.IsScrubbing ? _seek.Ratio * _duration : _position;
        var elapsed = PlaybackTimeline.FormatTime(displayPos, hours);
        var total = _duration > 0 ? PlaybackTimeline.FormatTime(_duration, hours) : "--:--";
        if (_elapsedText.Text != elapsed) _elapsedText.Text = elapsed;
        if (_totalText.Text != total) _totalText.Text = total;
        AutomationProperties.SetName(_totalText, L("Oynatma süresi"));

        _playPauseIcon.Glyph = _isPlaying ? "\uE769" : "\uE768";
        // Optical centering: play triangle's visual weight sits slightly left of its glyph box center.
        _playPauseIcon.Margin = _isPlaying ? new Thickness(0) : new Thickness(1.5, 0, 0, 0);
        UpdatePlayButtonVisual();

        var effectiveVolume = _isMuted ? 0.0 : _volume;
        _volumeIcon.Glyph = effectiveVolume switch
        {
            <= 0.001 => "\uE74F",
            < 0.33 => "\uE992",
            < 0.66 => "\uE993",
            _ => "\uE767",
        };
        if (!_volumeScrubber.IsScrubbing)
        {
            _volumeScrubber.SetRatio(effectiveVolume);
        }
    }

    private void VolumePanel_PointerWheelChanged(object sender, PointerRoutedEventArgs e)
    {
        var delta = e.GetCurrentPoint(this).Properties.MouseWheelDelta;
        if (delta == 0) return;
        var step = delta > 0 ? 0.05 : -0.05;
        var baseVolume = _isMuted ? 0.0 : _volume;
        VolumeRequested?.Invoke(Math.Clamp(baseVolume + step, 0.0, 1.0));
        e.Handled = true;
    }

    /// <summary>
    /// Minimalist macOS-style pill track with a luminous white fill and a circular thumb that fades in on hover/drag.
    /// </summary>
    private sealed class SleekScrubber : Grid
    {
        private readonly Border _track;
        private readonly Border _fill;
        private readonly Ellipse _thumb;
        private readonly double _restHeight;
        private readonly double _hoverHeight;
        private readonly double _thumbSize;
        private bool _hovered;

        public double Ratio { get; private set; }
        public bool IsScrubbing { get; private set; }
        public bool IsInteractive { get; set; } = true;

        public event Action<double>? ValueChanged;
        public event Action? ScrubbingChanged;

        public SleekScrubber(double trackRestHeight, double trackHoverHeight, double thumbSize)
        {
            _restHeight = trackRestHeight;
            _hoverHeight = trackHoverHeight;
            _thumbSize = thumbSize;

            Height = 18;
            Background = new SolidColorBrush(Colors.Transparent);

            _track = new Border
            {
                Height = _restHeight,
                CornerRadius = new CornerRadius(_restHeight / 2),
                Background = new SolidColorBrush(ColorHelper.FromArgb(0x38, 0xFF, 0xFF, 0xFF)),
                VerticalAlignment = VerticalAlignment.Center,
                HorizontalAlignment = HorizontalAlignment.Stretch,
            };

            _fill = new Border
            {
                Height = _restHeight,
                CornerRadius = new CornerRadius(_restHeight / 2),
                Background = new SolidColorBrush(ColorHelper.FromArgb(0xF0, 0xFF, 0xFF, 0xFF)),
                VerticalAlignment = VerticalAlignment.Center,
                HorizontalAlignment = HorizontalAlignment.Left,
                Width = 0,
            };

            _thumb = new Ellipse
            {
                Width = _thumbSize,
                Height = _thumbSize,
                Fill = new SolidColorBrush(Colors.White),
                VerticalAlignment = VerticalAlignment.Center,
                HorizontalAlignment = HorizontalAlignment.Left,
                Opacity = 0,
                OpacityTransition = new ScalarTransition { Duration = TimeSpan.FromMilliseconds(150) },
                IsHitTestVisible = false,
            };

            Children.Add(_track);
            Children.Add(_fill);
            Children.Add(_thumb);

            SizeChanged += (_, _) => UpdateLayoutFromRatio();
            PointerEntered += (_, _) => { _hovered = true; UpdateHoverState(); };
            PointerExited += (_, _) => { _hovered = false; UpdateHoverState(); };
            PointerPressed += OnPointerPressed;
            PointerMoved += OnPointerMoved;
            PointerReleased += OnPointerReleased;
            PointerCaptureLost += OnPointerCaptureLost;
        }

        public void SetRatio(double ratio)
        {
            Ratio = Math.Clamp(double.IsFinite(ratio) ? ratio : 0.0, 0.0, 1.0);
            UpdateLayoutFromRatio();
        }

        private void UpdateHoverState()
        {
            var active = (_hovered || IsScrubbing) && IsInteractive;
            var h = active ? _hoverHeight : _restHeight;
            _track.Height = h;
            _track.CornerRadius = new CornerRadius(h / 2);
            _fill.Height = h;
            _fill.CornerRadius = new CornerRadius(h / 2);
            _thumb.Opacity = active ? 1 : 0;
        }

        private void UpdateLayoutFromRatio()
        {
            var w = ActualWidth;
            if (w <= 0) return;
            var fillWidth = Math.Clamp(w * Ratio, 0, w);
            _fill.Width = fillWidth;
            var thumbLeft = Math.Clamp(fillWidth - (_thumbSize / 2), 0, Math.Max(0, w - _thumbSize));
            _thumb.Margin = new Thickness(thumbLeft, 0, 0, 0);
        }

        private void ScrubToPoint(Point pt)
        {
            var w = ActualWidth;
            if (w <= 0) return;
            SetRatio(pt.X / w);
            ValueChanged?.Invoke(Ratio);
        }

        private void OnPointerPressed(object sender, PointerRoutedEventArgs e)
        {
            if (!IsInteractive) return;
            CapturePointer(e.Pointer);
            IsScrubbing = true;
            UpdateHoverState();
            ScrubbingChanged?.Invoke();
            ScrubToPoint(e.GetCurrentPoint(this).Position);
            e.Handled = true;
        }

        private void OnPointerMoved(object sender, PointerRoutedEventArgs e)
        {
            if (!IsScrubbing) return;
            ScrubToPoint(e.GetCurrentPoint(this).Position);
            e.Handled = true;
        }

        private void OnPointerReleased(object sender, PointerRoutedEventArgs e)
        {
            if (!IsScrubbing) return;
            ReleasePointerCapture(e.Pointer);
            IsScrubbing = false;
            UpdateHoverState();
            ScrubbingChanged?.Invoke();
            e.Handled = true;
        }

        private void OnPointerCaptureLost(object sender, PointerRoutedEventArgs e)
        {
            if (!IsScrubbing) return;
            IsScrubbing = false;
            UpdateHoverState();
            ScrubbingChanged?.Invoke();
        }
    }
}
