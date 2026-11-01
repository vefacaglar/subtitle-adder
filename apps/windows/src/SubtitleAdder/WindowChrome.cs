using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using SubtitleAdder.Core;
using Windows.Graphics;

namespace SubtitleAdder;

/// <summary>
/// Shared window setup: Mica, content extended into the title bar, the app icon, a minimum and a persisted size,
/// and the theme from Settings (applied to the content and the caption buttons, and kept up to date).
/// </summary>
public static class WindowChrome
{
    public static string IconPath { get; } = Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico");

    /// <param name="sizeKey">Settings key prefix for the persisted size, or null to not persist it.</param>
    public static void Setup(Window window, FrameworkElement root, UIElement titleBar, int minWidth, int minHeight,
        int defaultWidth, int defaultHeight, string? sizeKey)
    {
        window.SystemBackdrop = new MicaBackdrop();
        window.ExtendsContentIntoTitleBar = true;
        window.SetTitleBar(titleBar);
        var appWindow = window.AppWindow;
        appWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Tall;
        if (File.Exists(IconPath)) appWindow.SetIcon(IconPath);

        var scale = GetScale(window);
        if (appWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.PreferredMinimumWidth = (int)(minWidth * scale);
            presenter.PreferredMinimumHeight = (int)(minHeight * scale);
        }

        var settings = App.Current.Settings;
        var width = sizeKey is null ? null : settings.GetInt(sizeKey + "Width");
        var height = sizeKey is null ? null : settings.GetInt(sizeKey + "Height");
        var size = width is > 0 && height is > 0
            ? new SizeInt32(Math.Max(width.Value, (int)(minWidth * scale)), Math.Max(height.Value, (int)(minHeight * scale)))
            : new SizeInt32((int)(defaultWidth * scale), (int)(defaultHeight * scale));
        // Keep the window on the work area of its display.
        var area = DisplayArea.GetFromWindowId(appWindow.Id, DisplayAreaFallback.Primary).WorkArea;
        size = new SizeInt32(Math.Min(size.Width, area.Width), Math.Min(size.Height, area.Height));
        appWindow.Resize(size);
        appWindow.Move(new PointInt32(area.X + (area.Width - size.Width) / 2, area.Y + (area.Height - size.Height) / 2));

        if (sizeKey is not null)
        {
            window.Closed += (_, _) =>
            {
                if (appWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Restored })
                {
                    settings.SetInt(sizeKey + "Width", appWindow.Size.Width);
                    settings.SetInt(sizeKey + "Height", appWindow.Size.Height);
                }
            };
        }

        ApplyTheme(window, root);
        var appearance = App.Current.Appearance;
        void OnAppearance(object? s, System.ComponentModel.PropertyChangedEventArgs e)
        {
            if (e.PropertyName == nameof(AppearanceSettings.Theme)) ApplyTheme(window, root);
        }
        appearance.PropertyChanged += OnAppearance;
        root.ActualThemeChanged += (_, _) => ApplyCaptionColors(window, root);
        window.Closed += (_, _) => appearance.PropertyChanged -= OnAppearance;
    }

    public static double GetScale(Window window) =>
        GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(window)) / 96.0;

    private static void ApplyTheme(Window window, FrameworkElement root)
    {
        root.RequestedTheme = App.Current.Appearance.Theme switch
        {
            ThemeChoice.Light => ElementTheme.Light,
            ThemeChoice.Dark => ElementTheme.Dark,
            _ => ElementTheme.Default,
        };
        ApplyCaptionColors(window, root);
    }

    /// <summary>Caption buttons (minimize/maximize/close) follow the content theme, not only the system one.</summary>
    private static void ApplyCaptionColors(Window window, FrameworkElement root)
    {
        var bar = window.AppWindow.TitleBar;
        bar.PreferredTheme = root.ActualTheme == ElementTheme.Dark ? TitleBarTheme.Dark : TitleBarTheme.Light;
        bar.ButtonBackgroundColor = Colors.Transparent;
        bar.ButtonInactiveBackgroundColor = Colors.Transparent;
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hwnd);
}
