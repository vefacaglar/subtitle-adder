using Microsoft.UI.Xaml;
using SubtitleAdder.Core;
using Windows.System.UserProfile;

namespace SubtitleAdder;

/// <summary>
/// Composition root. One view model and one model catalog are shared by the main window and Settings,
/// so a model deleted in Settings disappears from the toolbar picker immediately (same as macOS).
/// </summary>
public partial class App : Application
{
    private MainWindow? _window;
    private SettingsWindow? _settingsWindow;

    public App()
    {
        InitializeComponent();
        // Unhandled UI-thread errors go to %TEMP%\SubtitleAdder-crash.log (there is no console in a WinExe).
        UnhandledException += (_, e) => LogCrash(e.Exception);
        AppDomain.CurrentDomain.UnhandledException += (_, e) => LogCrash(e.ExceptionObject as Exception);
    }

    private static void LogCrash(Exception? error)
    {
        try
        {
            File.AppendAllText(Path.Combine(Path.GetTempPath(), "SubtitleAdder-crash.log"), $"{DateTime.Now:O}\n{error}\n\n");
        }
        catch (IOException)
        {
            // Nothing else to do.
        }
    }

    public static new App Current => (App)Application.Current;

    public ISettingsStore Settings { get; private set; } = null!;
    public AppLanguage Language { get; private set; } = null!;
    public AppearanceSettings Appearance { get; private set; } = null!;
    public ModelCatalog Catalog { get; private set; } = null!;
    public SubtitleViewModel ViewModel { get; private set; } = null!;

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        Settings = new JsonFileSettingsStore();
        // Must exist before anything builds a localized string.
        Language = new AppLanguage(Settings, () => GlobalizationPreferences.Languages);
        var ui = new SynchronizationContextDispatcher();
        LocalizedStrings.Shared.Dispatcher = ui;
        Appearance = new AppearanceSettings(Settings);
        Catalog = ModelCatalog.Default();
        ViewModel = new SubtitleViewModel(Catalog, Settings, ui);

        _window = new MainWindow();
        _window.Closed += (_, _) =>
        {
            _settingsWindow?.Close();
            ViewModel.Dispose();
        };
        _window.Activate();

        // "Open with" / a path on the command line.
        var path = Environment.GetCommandLineArgs().Skip(1).FirstOrDefault(File.Exists);
        if (path is not null) ViewModel.OpenVideo(Path.GetFullPath(path));
    }

    /// <summary>Opens Settings (one window), optionally on a page.</summary>
    public void ShowSettings(string? page = null)
    {
        if (_settingsWindow is null)
        {
            _settingsWindow = new SettingsWindow();
            _settingsWindow.Closed += (_, _) => _settingsWindow = null;
        }
        if (page is not null) _settingsWindow.Navigate(page);
        _settingsWindow.Activate();
    }
}
