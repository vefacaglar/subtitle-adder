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
    private FlyoutBase? _volumeFlyout;
    private bool _volumeOpen;

    public PreviewTransportControls() => DefaultStyleKey = typeof(MediaTransportControls);
    public event Action<double>? SeekRequested;
    public event Action? InteractionChanged;
    public bool IsInteracting => _volumeOpen || _scrubbing;

    protected override void OnApplyTemplate()
    {
        if (_volumeFlyout is not null)
        {
            _volumeFlyout.Opening -= VolumeFlyout_Opening;
            _volumeFlyout.Closed -= VolumeFlyout_Closed;
        }
        _volumeOpen = false;
        base.OnApplyTemplate();
        _seek = null;
        _time = null;
        _readout = null;
        _scrubbing = false;
        // Hover visibility is controlled by the player surface. Remove the native slide/fade
        // storyboards so showing controls never moves them up from below the video.
        if (GetTemplateChild("RootGrid") is Grid root)
        {
            var groups = VisualStateManager.GetVisualStateGroups(root);
            var visibility = groups.FirstOrDefault(g => g.Name == "ControlPanelVisibilityStates");
            if (visibility is not null)
            {
                foreach (var state in visibility.States) state.Storyboard?.Stop();
                groups.Remove(visibility);
            }
        }
        if (GetTemplateChild("ControlPanel_ControlPanelVisibilityStates_Border") is Border panelBorder)
        {
            panelBorder.Opacity = 1;
            panelBorder.IsHitTestVisible = true;
        }
        if (GetTemplateChild("ControlPanelGrid") is Grid panel)
        {
            panel.RenderTransform = null;
        }
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
            _seek.AddHandler(PointerPressedEvent, new PointerEventHandler((_, _) => SetScrubbing(true)), true);
            _seek.AddHandler(PointerReleasedEvent, new PointerEventHandler((_, _) => SetScrubbing(false)), true);
            _seek.AddHandler(PointerCaptureLostEvent, new PointerEventHandler((_, _) => SetScrubbing(false)), true);
            timeline.Children.Add(_seek);
        }
        // Remove the native elapsed/remaining row entirely; there is one readout at the right.
        if (GetTemplateChild("TimeTextGrid") is Grid nativeTimes)
        {
            nativeTimes.Children.Clear();
            nativeTimes.Height = 0;
            nativeTimes.Visibility = Visibility.Collapsed;
        }
        if (GetTemplateChild("ControlPanelGrid") is Grid controls)
        {
            // Preserve the original native layout: play on the left, timeline in the middle,
            // volume and other native buttons on the right. Keep only the requested time readout.
            controls.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            _time = new TextBlock
            {
                FontFamily = new FontFamily("Consolas"), FontSize = 12,
                Margin = new Thickness(8, 0, 10, 0),
                HorizontalAlignment = HorizontalAlignment.Right,
                VerticalAlignment = VerticalAlignment.Center,
                TextTrimming = TextTrimming.CharacterEllipsis,
            };
            Grid.SetColumn(_time, controls.ColumnDefinitions.Count - 1);
            Grid.SetRow(_time, 1);
            controls.Children.Add(_time);
        }
        _volumeFlyout = (GetTemplateChild("VolumeMuteButton") as AppBarButton)?.Flyout;
        if (_volumeFlyout is not null)
        {
            _volumeFlyout.Opening += VolumeFlyout_Opening;
            _volumeFlyout.Closed += VolumeFlyout_Closed;
        }
        Synchronize(_position, _duration);
        InteractionChanged?.Invoke();
    }

    private void VolumeFlyout_Opening(object? sender, object e)
    {
        _volumeOpen = true;
        InteractionChanged?.Invoke();
    }

    private void VolumeFlyout_Closed(object? sender, object e)
    {
        _volumeOpen = false;
        InteractionChanged?.Invoke();
    }

    private void SetScrubbing(bool value)
    {
        _scrubbing = value;
        InteractionChanged?.Invoke();
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
                _time.FontSize = ActualWidth is > 0 and < 520 ? 10 : 12;
                var hours = _duration >= 3600;
                var elapsed = PlaybackTimeline.FormatTime(_scrubbing ? _seek?.Value ?? _position : _position, hours);
                var total = _duration > 0 ? PlaybackTimeline.FormatTime(_duration, hours) : "--:--";
                var text = $"{elapsed} / {total}";
                if (_readout != text)
                {
                    _readout = text;
                    ToolTipService.SetToolTip(_time, text);
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
