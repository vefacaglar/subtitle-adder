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
    private SingleInstanceCoordinator? _singleInstance;

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
        var cliPaths = Environment.GetCommandLineArgs()
            .Skip(1)
            .Select(p => p.Trim().Trim('"'))
            .Where(File.Exists)
            .Select(Path.GetFullPath)
            .ToList();

        if (!SingleInstanceCoordinator.TryAcquireOrForward(cliPaths, paths =>
            {
                _window?.DispatcherQueue.TryEnqueue(() =>
                {
                    if (_window is null) return;
                    SingleInstanceCoordinator.BringToForeground(_window);
                    if (paths.Count > 0) _window.OpenExternalFiles(paths);
                });
            }, out _singleInstance))
        {
            Environment.Exit(0);
            return;
        }

        SubtitleViewModel.CleanupTempDirectory();
        Settings = new JsonFileSettingsStore();

        if (string.Equals(Path.GetFileName(Environment.ProcessPath), "SubtitleAdder.exe", StringComparison.OrdinalIgnoreCase) &&
            Settings.GetBool("fileAssociationsDisabled") != true)
        {
            try
            {
                FileAssociationRegistrar.Register();
            }
            catch
            {
                // Ignore if registry access is restricted.
            }
        }

        // Must exist before anything builds a localized string.
        Language = new AppLanguage(Settings, () => GlobalizationPreferences.Languages);
        ApplySystemLanguage(Language.Effective);
        var ui = new SynchronizationContextDispatcher();
        LocalizedStrings.Shared.Dispatcher = ui;
        Appearance = new AppearanceSettings(Settings);
        Catalog = ModelCatalog.Default();
        ViewModel = new SubtitleViewModel(Catalog, Settings, ui);

        _window = new MainWindow();
        _window.Closed += (_, _) =>
        {
            _settingsWindow?.Close();
            _singleInstance?.Dispose();
            ViewModel.Dispose();
            (Settings as IDisposable)?.Dispose();   // writes pending settings
        };
        _window.Activate();

        // "Open with" / double-clicked media or subtitle files on the command line.
        if (cliPaths.Count > 0)
        {
            _window.OpenExternalFiles(cliPaths);
        }
    }

    /// <summary>
    /// Built-in WinUI and system text (transport control tooltips, text box menus, pickers) follows the process language
    /// override, which takes effect for windows created afterwards; Settings tells the user a restart applies it.
    /// </summary>
    private static void ApplySystemLanguage(EngineKit.UILanguage language)
    {
        try
        {
            Microsoft.Windows.Globalization.ApplicationLanguages.PrimaryLanguageOverride = language == EngineKit.UILanguage.En ? "en-US" : "tr-TR";
        }
        catch (Exception error) when (error is System.Runtime.InteropServices.COMException or ArgumentException)
        {
            // Not fatal: system strings stay in the OS language.
        }
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
