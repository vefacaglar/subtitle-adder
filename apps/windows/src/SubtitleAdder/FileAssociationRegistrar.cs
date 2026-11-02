using System.Runtime.InteropServices;
using Microsoft.Win32;

namespace SubtitleAdder;

/// <summary>
/// Registers Subtitle Adder in <c>HKEY_CURRENT_USER</c> (no administrator elevation required) as a video, audio, and
/// subtitle handler so double-clicking a media file in Explorer or choosing "Open with" / Windows Default Apps launches
/// Subtitle Adder directly.
/// </summary>
public static class FileAssociationRegistrar
{
    public const string MediaProgId = "SubtitleAdder.Media";
    public const string SubtitleProgId = "SubtitleAdder.Subtitle";
    public const string AppRegisteredName = "Subtitle Adder";
    private const string CapabilitiesRegistryPath = @"Software\SubtitleAdder\Capabilities";

    public static readonly IReadOnlyList<string> MediaExtensions =
    [
        ".mp4", ".mov", ".m4v", ".mkv", ".avi", ".webm", ".wmv", ".mpg", ".mpeg", ".ts", ".mts", ".m2ts", ".3gp", ".flv",
        ".mp3", ".wav", ".m4a", ".aac", ".flac", ".ogg", ".opus", ".wma",
    ];

    public static readonly IReadOnlyList<string> SubtitleExtensions =
    [
        ".srt", ".vtt", ".ass", ".ssa",
    ];

    public static bool IsRegistered()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey($@"Software\Classes\{MediaProgId}\shell\open\command");
            return key?.GetValue(null) is string cmd && !string.IsNullOrWhiteSpace(cmd);
        }
        catch
        {
            return false;
        }
    }

    public static void Register(string? exePath = null, string? iconPath = null)
    {
        exePath ??= Environment.ProcessPath ?? Path.Combine(AppContext.BaseDirectory, "SubtitleAdder.exe");
        iconPath ??= File.Exists(WindowChrome.IconPath) ? WindowChrome.IconPath : $"{exePath},0";
        var command = $"\"{exePath}\" \"%1\"";

        RegisterProgId(MediaProgId, "Subtitle Adder Media File", iconPath, command);
        RegisterProgId(SubtitleProgId, "Subtitle Adder Subtitle File", iconPath, command);

        // Register under Software\Classes\Applications\SubtitleAdder.exe for the "Open with" menu.
        using (var appKey = Registry.CurrentUser.CreateSubKey(@"Software\Classes\Applications\SubtitleAdder.exe"))
        {
            appKey.SetValue("FriendlyAppName", AppRegisteredName);
            using (var iconKey = appKey.CreateSubKey("DefaultIcon"))
            {
                iconKey.SetValue(null, iconPath);
            }
            using (var cmdKey = appKey.CreateSubKey(@"shell\open\command"))
            {
                cmdKey.SetValue(null, command);
            }
            using (var typesKey = appKey.CreateSubKey("SupportedTypes"))
            {
                foreach (var ext in MediaExtensions) typesKey.SetValue(ext, "");
                foreach (var ext in SubtitleExtensions) typesKey.SetValue(ext, "");
            }
        }

        // Register Capabilities and RegisteredApplications for Windows Default Apps settings.
        using (var capKey = Registry.CurrentUser.CreateSubKey(CapabilitiesRegistryPath))
        {
            capKey.SetValue("ApplicationName", AppRegisteredName);
            capKey.SetValue("ApplicationDescription", "Subtitle Adder — Media Player & AI Subtitle Studio");
            capKey.SetValue("ApplicationIcon", iconPath);
            using var assocKey = capKey.CreateSubKey("FileAssociations");
            foreach (var ext in MediaExtensions) assocKey.SetValue(ext, MediaProgId);
            foreach (var ext in SubtitleExtensions) assocKey.SetValue(ext, SubtitleProgId);
        }

        using (var regApps = Registry.CurrentUser.CreateSubKey(@"Software\RegisteredApplications"))
        {
            regApps.SetValue(AppRegisteredName, CapabilitiesRegistryPath);
        }

        // Register OpenWithProgids for each extension.
        foreach (var ext in MediaExtensions)
        {
            RegisterExtension(ext, MediaProgId);
        }
        foreach (var ext in SubtitleExtensions)
        {
            RegisterExtension(ext, SubtitleProgId);
        }

        NotifyShellAssociationsChanged();
    }

    public static void Unregister()
    {
        foreach (var ext in MediaExtensions) UnregisterExtension(ext, MediaProgId);
        foreach (var ext in SubtitleExtensions) UnregisterExtension(ext, SubtitleProgId);

        using (var regApps = Registry.CurrentUser.OpenSubKey(@"Software\RegisteredApplications", writable: true))
        {
            regApps?.DeleteValue(AppRegisteredName, throwOnMissingValue: false);
        }

        Registry.CurrentUser.DeleteSubKeyTree(@"Software\SubtitleAdder", throwOnMissingSubKey: false);
        Registry.CurrentUser.DeleteSubKeyTree(@"Software\Classes\Applications\SubtitleAdder.exe", throwOnMissingSubKey: false);
        Registry.CurrentUser.DeleteSubKeyTree($@"Software\Classes\{MediaProgId}", throwOnMissingSubKey: false);
        Registry.CurrentUser.DeleteSubKeyTree($@"Software\Classes\{SubtitleProgId}", throwOnMissingSubKey: false);

        NotifyShellAssociationsChanged();
    }

    private static void RegisterProgId(string progId, string friendlyTypeName, string iconPath, string command)
    {
        using var progKey = Registry.CurrentUser.CreateSubKey($@"Software\Classes\{progId}");
        progKey.SetValue(null, friendlyTypeName);
        progKey.SetValue("FriendlyTypeName", friendlyTypeName);
        using (var iconKey = progKey.CreateSubKey("DefaultIcon"))
        {
            iconKey.SetValue(null, iconPath);
        }
        using (var openKey = progKey.CreateSubKey(@"shell\open"))
        {
            openKey.SetValue("FriendlyAppName", AppRegisteredName);
            using var cmdKey = openKey.CreateSubKey("command");
            cmdKey.SetValue(null, command);
        }
    }

    private static void RegisterExtension(string ext, string progId)
    {
        using var extKey = Registry.CurrentUser.CreateSubKey($@"Software\Classes\{ext}");
        if (extKey.GetValue(null) is not string existing || string.IsNullOrWhiteSpace(existing))
        {
            extKey.SetValue(null, progId);
        }
        using var openWith = extKey.CreateSubKey("OpenWithProgids");
        openWith.SetValue(progId, "");
    }

    private static void UnregisterExtension(string ext, string progId)
    {
        using var extKey = Registry.CurrentUser.OpenSubKey($@"Software\Classes\{ext}", writable: true);
        if (extKey is null) return;
        if (string.Equals(extKey.GetValue(null) as string, progId, StringComparison.OrdinalIgnoreCase))
        {
            extKey.DeleteValue("", throwOnMissingValue: false);
        }
        using var openWith = extKey.OpenSubKey("OpenWithProgids", writable: true);
        openWith?.DeleteValue(progId, throwOnMissingValue: false);
    }

    private static void NotifyShellAssociationsChanged()
    {
        try
        {
            // SHCNE_ASSOCCHANGED = 0x08000000, SHCNF_IDLIST = 0x0000
            SHChangeNotify(0x08000000, 0x0000, IntPtr.Zero, IntPtr.Zero);
        }
        catch
        {
            // Ignore shell notification failures in headless environments.
        }
    }

    [DllImport("shell32.dll")]
    private static extern void SHChangeNotify(uint wEventId, uint uFlags, IntPtr dwItem1, IntPtr dwItem2);
}
