using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using SubtitleAdder.Core;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder;

/// <summary>Keeps the native media buttons, with one app-owned timeline for seeks and playback.</summary>
public sealed class PreviewTransportControls : MediaTransportControls
{
    private Slider? _seek;
    private TextBlock? _time;
    private bool _updating;
    private bool _scrubbing;
    private double _position;
    private double _duration;
    private string? _readout;

    public PreviewTransportControls() => DefaultStyleKey = typeof(MediaTransportControls);
    public event Action<double>? SeekRequested;

    protected override void OnApplyTemplate()
    {
        base.OnApplyTemplate();
        _seek = null;
        _time = null;
        _readout = null;
        _scrubbing = false;
        // These named parts are supplied by the pinned WinUI MediaTransportControls template.
        // The native slider's private playback tracking can lag after an external subtitle seek.
        if (GetTemplateChild("ProgressSlider") is Slider native && VisualTreeHelper.GetParent(native) is Grid timeline)
        {
            native.Visibility = Visibility.Collapsed;
            _seek = new Slider
            {
                Margin = native.Margin, Height = native.Height, MinWidth = native.MinWidth,
                VerticalAlignment = VerticalAlignment.Center, IsThumbToolTipEnabled = false,
                StepFrequency = 0.1, SmallChange = 1, LargeChange = 10,
            };
            Grid.SetRow(_seek, Grid.GetRow(native));
            Grid.SetColumn(_seek, Grid.GetColumn(native));
            _seek.ValueChanged += Seek_ValueChanged;
            _seek.AddHandler(PointerPressedEvent, new PointerEventHandler((_, _) => _scrubbing = true), true);
            _seek.AddHandler(PointerReleasedEvent, new PointerEventHandler((_, _) => _scrubbing = false), true);
            _seek.AddHandler(PointerCaptureLostEvent, new PointerEventHandler((_, _) => _scrubbing = false), true);
            timeline.Children.Add(_seek);
        }
        if (GetTemplateChild("TimeTextGrid") is Grid timeGrid)
        {
            foreach (var child in timeGrid.Children) child.Opacity = 0;
            _time = new TextBlock
            {
                FontFamily = new FontFamily("Consolas"), FontSize = 12,
                HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center,
            };
            timeGrid.Children.Add(_time);
        }
        Synchronize(_position, _duration);
    }

    public void Synchronize(double position, double duration)
    {
        _duration = double.IsFinite(duration) ? Math.Max(0, duration) : 0;
        _position = PlaybackTimeline.Clamp(position, _duration);
        _updating = true;
        try
        {
            if (_seek is not null)
            {
                _seek.IsEnabled = _duration > 0;
                _seek.Maximum = Math.Max(1, _duration);
                if (!_scrubbing) _seek.Value = Math.Min(_position, _seek.Maximum);
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_seek, L("Video konumu"));
            }
            if (_time is not null)
            {
                var hours = _duration >= 3600;
                var elapsed = PlaybackTimeline.FormatTime(_scrubbing ? _seek?.Value ?? _position : _position, hours);
                var total = _duration > 0 ? PlaybackTimeline.FormatTime(_duration, hours) : "--:--";
                var text = $"{elapsed} / {total}";
                if (_readout != text)
                {
                    _readout = text;
                    _time.Inlines.Clear();
                    _time.Inlines.Add(new Run { Text = elapsed });
                    _time.Inlines.Add(new Run { Text = $" / {total}", Foreground = new SolidColorBrush(Microsoft.UI.Colors.White) { Opacity = 0.65 } });
                }
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_time, L("Oynatma süresi"));
            }
        }
        finally { _updating = false; }
    }

    private void Seek_ValueChanged(object sender, RangeBaseValueChangedEventArgs e)
    {
        if (!_updating && _duration > 0) SeekRequested?.Invoke(e.NewValue);
    }
}
