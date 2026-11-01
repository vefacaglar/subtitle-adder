using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using SubtitleAdder.Core;

namespace SubtitleAdder;

/// <summary>
/// XAML attached properties that set a localized string by catalog key, e.g. <c>loc:Localize.Text="Altyazı üret"</c>,
/// and set it again when the UI language changes (so Settings switches the language without a restart).
/// Keys are the Turkish source strings; the catalog completeness test scans these attributes.
/// </summary>
public static class Localize
{
    private enum Target { Text, Content, Header, PlaceholderText, ToolTip, Name, Label, Title, Message, OffContent, OnContent }

    private static readonly List<(WeakReference<DependencyObject> Element, Target Target, string Key)> Registered = [];
    private static bool _subscribed;
    private static int _pruneAt = 256;   // regenerated list items keep registering; dead entries are dropped in batches

    public static readonly DependencyProperty TextProperty = Register(nameof(Target.Text), Target.Text);
    public static readonly DependencyProperty ContentProperty = Register(nameof(Target.Content), Target.Content);
    public static readonly DependencyProperty HeaderProperty = Register(nameof(Target.Header), Target.Header);
    public static readonly DependencyProperty PlaceholderTextProperty = Register(nameof(Target.PlaceholderText), Target.PlaceholderText);
    public static readonly DependencyProperty ToolTipProperty = Register(nameof(Target.ToolTip), Target.ToolTip);
    /// <summary>Accessible name (<see cref="AutomationProperties.NameProperty"/>), for icon-only buttons.</summary>
    public static readonly DependencyProperty AutomationNameProperty = Register("AutomationName", Target.Name);
    public static readonly DependencyProperty LabelProperty = Register(nameof(Target.Label), Target.Label);
    public static readonly DependencyProperty TitleProperty = Register(nameof(Target.Title), Target.Title);
    public static readonly DependencyProperty MessageProperty = Register(nameof(Target.Message), Target.Message);
    public static readonly DependencyProperty OffContentProperty = Register(nameof(Target.OffContent), Target.OffContent);
    public static readonly DependencyProperty OnContentProperty = Register(nameof(Target.OnContent), Target.OnContent);

    public static string GetText(DependencyObject o) => (string)o.GetValue(TextProperty);
    public static void SetText(DependencyObject o, string v) => o.SetValue(TextProperty, v);
    public static string GetContent(DependencyObject o) => (string)o.GetValue(ContentProperty);
    public static void SetContent(DependencyObject o, string v) => o.SetValue(ContentProperty, v);
    public static string GetHeader(DependencyObject o) => (string)o.GetValue(HeaderProperty);
    public static void SetHeader(DependencyObject o, string v) => o.SetValue(HeaderProperty, v);
    public static string GetPlaceholderText(DependencyObject o) => (string)o.GetValue(PlaceholderTextProperty);
    public static void SetPlaceholderText(DependencyObject o, string v) => o.SetValue(PlaceholderTextProperty, v);
    public static string GetToolTip(DependencyObject o) => (string)o.GetValue(ToolTipProperty);
    public static void SetToolTip(DependencyObject o, string v) => o.SetValue(ToolTipProperty, v);
    public static string GetAutomationName(DependencyObject o) => (string)o.GetValue(AutomationNameProperty);
    public static void SetAutomationName(DependencyObject o, string v) => o.SetValue(AutomationNameProperty, v);
    public static string GetLabel(DependencyObject o) => (string)o.GetValue(LabelProperty);
    public static void SetLabel(DependencyObject o, string v) => o.SetValue(LabelProperty, v);
    public static string GetTitle(DependencyObject o) => (string)o.GetValue(TitleProperty);
    public static void SetTitle(DependencyObject o, string v) => o.SetValue(TitleProperty, v);
    public static string GetMessage(DependencyObject o) => (string)o.GetValue(MessageProperty);
    public static void SetMessage(DependencyObject o, string v) => o.SetValue(MessageProperty, v);
    public static string GetOffContent(DependencyObject o) => (string)o.GetValue(OffContentProperty);
    public static void SetOffContent(DependencyObject o, string v) => o.SetValue(OffContentProperty, v);
    public static string GetOnContent(DependencyObject o) => (string)o.GetValue(OnContentProperty);
    public static void SetOnContent(DependencyObject o, string v) => o.SetValue(OnContentProperty, v);

    private static DependencyProperty Register(string name, Target target) =>
        DependencyProperty.RegisterAttached(name, typeof(string), typeof(Localize),
            new PropertyMetadata(null, (o, e) =>
            {
                if (e.NewValue is not string key) return;
                Subscribe();
                Registered.Add((new WeakReference<DependencyObject>(o), target, key));
                if (Registered.Count >= _pruneAt)
                {
                    Registered.RemoveAll(r => !r.Element.TryGetTarget(out var _));
                    _pruneAt = Math.Max(256, Registered.Count * 2);
                }
                Apply(o, target, key);
            }));

    private static void Subscribe()
    {
        if (_subscribed) return;
        _subscribed = true;
        // Raised on the UI thread (LocalizedStrings.Shared.Dispatcher is the UI dispatcher).
        LocalizedStrings.Shared.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName != string.Empty) return;
            Registered.RemoveAll(r => !r.Element.TryGetTarget(out var _));
            foreach (var (element, target, key) in Registered.ToArray())
            {
                if (element.TryGetTarget(out var o)) Apply(o, target, key);
            }
        };
    }

    private static void Apply(DependencyObject o, Target target, string key)
    {
        var text = Strings.L(key);
        switch (target)
        {
            case Target.Text:
                switch (o)
                {
                    case TextBlock t: t.Text = text; break;
                    case MenuFlyoutItem m: m.Text = text; break;
                    case MenuFlyoutSubItem s: s.Text = text; break;
                    case Microsoft.UI.Xaml.Documents.Run r: r.Text = text; break;
                    case TextBox b: b.Text = text; break;
                }
                break;
            case Target.Content:
                if (o is ContentControl c) c.Content = text;
                break;
            case Target.Header:
                switch (o)
                {
                    case ComboBox cb: cb.Header = text; break;
                    case ToggleSwitch ts: ts.Header = text; break;
                    case Slider sl: sl.Header = text; break;
                    case TextBox tb: tb.Header = text; break;
                    case RadioButtons rb: rb.Header = text; break;
                }
                break;
            case Target.PlaceholderText:
                switch (o)
                {
                    case TextBox tb: tb.PlaceholderText = text; break;
                    case ComboBox cb: cb.PlaceholderText = text; break;
                }
                break;
            case Target.ToolTip:
                ToolTipService.SetToolTip(o, text);
                break;
            case Target.Name:
                AutomationProperties.SetName(o, text);
                break;
            case Target.Label:
                if (o is AppBarButton ab) ab.Label = text;
                else if (o is AppBarToggleButton atb) atb.Label = text;
                break;
            case Target.Title:
                if (o is InfoBar ib) ib.Title = text;
                else if (o is ContentDialog cd) cd.Title = text;
                break;
            case Target.Message:
                if (o is InfoBar mb) mb.Message = text;
                break;
            case Target.OffContent:
                if (o is ToggleSwitch off) off.OffContent = text;
                break;
            case Target.OnContent:
                if (o is ToggleSwitch on) on.OnContent = text;
                break;
        }
    }
}

/// <summary>Small functions for <c>x:Bind</c> function bindings.</summary>
public static class Ui
{
    public static bool Not(bool value) => !value;

    public static Visibility Visible(bool value) => value ? Visibility.Visible : Visibility.Collapsed;

    public static Visibility Collapsed(bool value) => value ? Visibility.Collapsed : Visibility.Visible;

    public static Visibility VisibleIfText(string? text) => string.IsNullOrEmpty(text) ? Visibility.Collapsed : Visibility.Visible;

    public static Windows.UI.Color Color(RgbColor color, byte alpha = 255) => Windows.UI.Color.FromArgb(alpha, color.R, color.G, color.B);
}
