// See PlatformCompat.csproj. The client/server bodies are copied from
// apps/windows/src/SubtitleAdder/SingleInstanceCoordinator.cs with the pipe name as a parameter; the registrar from
// FileAssociationRegistrar.cs (icon path as a parameter, no shell notification), the font list from SystemFonts.cs,
// the path rules from MediaLibraryIndex.NormalizeDirectoryPath and LocalProfileManager.IsDriveRootReady.
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using Microsoft.Win32;

Console.OutputEncoding = new UTF8Encoding(false);
switch (args.ElementAtOrDefault(0))
{
    case "client":
        Console.WriteLine(ForwardToPrimaryInstance(args[1], args.Skip(2).ToList()) ? "ok" : "fail");
        return 0;
    case "server":
        await Serve(args[1], int.Parse(args[2]));
        return 0;
    case "names":
        var safe = SanitizeToken(args[1]);
        Console.WriteLine(safe);
        Console.WriteLine($@"Local\SubtitleAdder.SingleInstance.{safe}");
        Console.WriteLine($"SubtitleAdder.SingleInstance.{safe}");
        return 0;
    case "assoc-register":
        using (Redirect.CurrentUserTo(args[1]))
        {
            FileAssociationRegistrar.Register(args[2], args.ElementAtOrDefault(3));
        }
        Console.WriteLine("ok");
        return 0;
    case "assoc-unregister":
        using (Redirect.CurrentUserTo(args[1]))
        {
            FileAssociationRegistrar.Unregister();
        }
        Console.WriteLine("ok");
        return 0;
    case "assoc-registered":
        using (Redirect.CurrentUserTo(args[1]))
        {
            Console.WriteLine(FileAssociationRegistrar.IsRegistered() ? "true" : "false");
        }
        return 0;
    case "dump":
        using (var root = Registry.CurrentUser.OpenSubKey(args[1]))
        {
            var lines = new List<string>();
            if (root is not null) Dump(root, "", lines);
            lines.Sort(StringComparer.Ordinal);
            foreach (var line in lines) Console.WriteLine(line);
        }
        return 0;
    case "fonts":
        foreach (var family in SystemFonts.Families) Console.WriteLine(family);
        return 0;
    case "paths":
        foreach (var path in args.Skip(1))
        {
            Console.WriteLine(JsonSerializer.Serialize(new Dictionary<string, object?>
            {
                ["root"] = Path.GetPathRoot(path),
                ["hasExtension"] = Path.HasExtension(path),
                ["withoutExtension"] = Path.GetFileNameWithoutExtension(path),
                ["normalizedDirectory"] = NormalizeDirectoryPath(path),
                ["storageAvailable"] = IsVideoStorageAvailable(path),
            }));
        }
        return 0;
    default:
        Console.Error.WriteLine("usage: PlatformCompat client <pipe> <path...> | server <pipe> <count> | names <user> | "
            + "assoc-register <scratch> <exe> [icon] | assoc-unregister <scratch> | assoc-registered <scratch> | "
            + "dump <scratch> | fonts | paths <path...>");
        return 2;
}

static void Dump(RegistryKey key, string relative, List<string> lines)
{
    lines.Add($"{relative}|");
    foreach (var name in key.GetValueNames())
    {
        var kind = key.GetValueKind(name);
        var data = key.GetValue(name, null, RegistryValueOptions.DoNotExpandEnvironmentNames);
        lines.Add($"{relative}|{name}|{kind}|{data}");
    }
    foreach (var sub in key.GetSubKeyNames())
    {
        using var child = key.OpenSubKey(sub);
        if (child is not null) Dump(child, relative.Length == 0 ? sub : $@"{relative}\{sub}", lines);
    }
}

// MediaLibraryIndex.NormalizeDirectoryPath (verbatim).
static string NormalizeDirectoryPath(string directoryPath)
{
    if (string.IsNullOrWhiteSpace(directoryPath)) return "";
    try
    {
        var full = Path.GetFullPath(directoryPath.Trim());
        var root = Path.GetPathRoot(full);
        if (!string.IsNullOrEmpty(root) && string.Equals(full, root, StringComparison.OrdinalIgnoreCase))
        {
            return full;
        }
        return full.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
    }
    catch (Exception)
    {
        return directoryPath.Trim();
    }
}

// LocalProfileManager.IsVideoStorageAvailable / IsDriveRootReady without the test filter and the root cache.
static bool IsVideoStorageAvailable(string normalizedPath)
{
    if (string.IsNullOrWhiteSpace(normalizedPath)) return false;
    try
    {
        var root = Path.GetPathRoot(normalizedPath);
        if (string.IsNullOrWhiteSpace(root)) return true;
        if (root.StartsWith(@"\\", StringComparison.Ordinal))
        {
            return Directory.Exists(root);
        }
        var drive = new DriveInfo(root);
        return drive.IsReady;
    }
    catch
    {
        return false;
    }
}

static bool ForwardToPrimaryInstance(string pipeName, IReadOnlyList<string> filePaths)
{
    try
    {
        using var client = new NamedPipeClientStream(".", pipeName, PipeDirection.Out, PipeOptions.CurrentUserOnly);
        client.Connect(timeout: 1200);
        using var writer = new StreamWriter(client, new UTF8Encoding(false)) { AutoFlush = true };
        foreach (var path in filePaths)
        {
            writer.WriteLine(path);
        }
        return true;
    }
    catch
    {
        return false;
    }
}

static async Task Serve(string pipeName, int count)
{
    // Ready marker: the Rust side waits for it before connecting.
    Console.WriteLine("ready");
    for (var i = 0; i < count; i++)
    {
        await using var server = new NamedPipeServerStream(
            pipeName,
            PipeDirection.In,
            maxNumberOfServerInstances: NamedPipeServerStream.MaxAllowedServerInstances,
            PipeTransmissionMode.Byte,
            PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
        await server.WaitForConnectionAsync().ConfigureAwait(false);
        using var reader = new StreamReader(server, Encoding.UTF8);
        var paths = new List<string>();
        while (await reader.ReadLineAsync().ConfigureAwait(false) is { } line)
        {
            var trimmed = line.Trim().Trim('"');
            if (trimmed.Length > 0 && File.Exists(trimmed))
            {
                paths.Add(Path.GetFullPath(trimmed));
            }
        }
        Console.WriteLine($"batch|{paths.Count}");
        foreach (var p in paths) Console.WriteLine($"path|{p}");
    }
}

static string SanitizeToken(string raw)
{
    if (string.IsNullOrWhiteSpace(raw)) return "default";
    var chars = raw.Select(c => char.IsLetterOrDigit(c) ? c : '_').ToArray();
    return new string(chars);
}


/// <summary>Redirects HKEY_CURRENT_USER of this process to HKCU\&lt;scratch&gt; (RegOverridePredefKey) while alive.</summary>
sealed class Redirect : IDisposable
{
    private static readonly IntPtr HkeyCurrentUser = new(unchecked((int)0x80000001));
    private readonly RegistryKey _scratch;

    private Redirect(RegistryKey scratch) => _scratch = scratch;

    public static Redirect CurrentUserTo(string subkey)
    {
        var scratch = Registry.CurrentUser.CreateSubKey(subkey);
        var hr = RegOverridePredefKey(HkeyCurrentUser, scratch.Handle.DangerousGetHandle());
        if (hr != 0) throw new InvalidOperationException($"RegOverridePredefKey failed: {hr}");
        return new Redirect(scratch);
    }

    public void Dispose()
    {
        RegOverridePredefKey(HkeyCurrentUser, IntPtr.Zero);
        _scratch.Dispose();
    }

    [DllImport("advapi32.dll")]
    private static extern int RegOverridePredefKey(IntPtr hKey, IntPtr hNewHKey);
}

/// <summary>apps/windows/src/SubtitleAdder/FileAssociationRegistrar.cs, verbatim except: the icon fallback takes
/// the exe path (no WindowChrome.IconPath next to this tool) and the shell is not notified.</summary>
static class FileAssociationRegistrar
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

    public static void Register(string exePath, string? iconPath = null)
    {
        iconPath ??= $"{exePath},0";
        var command = $"\"{exePath}\" \"%1\"";

        RegisterProgId(MediaProgId, "Subtitle Adder Media File", iconPath, command);
        RegisterProgId(SubtitleProgId, "Subtitle Adder Subtitle File", iconPath, command);

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

        foreach (var ext in MediaExtensions)
        {
            RegisterExtension(ext, MediaProgId);
        }
        foreach (var ext in SubtitleExtensions)
        {
            RegisterExtension(ext, SubtitleProgId);
        }
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
}

/// <summary>apps/windows/src/SubtitleAdder/SystemFonts.cs (verbatim).</summary>
static unsafe class SystemFonts
{
    private static readonly Lazy<IReadOnlyList<string>> LazyFamilies = new(Load);

    public static IReadOnlyList<string> Families => LazyFamilies.Value;

    private static readonly Guid IidFactory = new("b859ee5a-d838-4b5b-a2e8-1adc7d93db48");

    [DllImport("dwrite.dll")]
    private static extern int DWriteCreateFactory(int factoryType, in Guid iid, out IntPtr factory);

    private static IReadOnlyList<string> Load()
    {
        try
        {
            return Enumerate();
        }
        catch (Exception error) when (error is COMException or DllNotFoundException or EntryPointNotFoundException or InvalidOperationException)
        {
            return ["Arial", "Calibri", "Consolas", "Segoe UI", "Segoe UI Variable", "Tahoma", "Times New Roman", "Verdana"];
        }
    }

    private static void** Vtbl(IntPtr obj) => *(void***)obj;

    private static void Release(IntPtr obj)
    {
        if (obj != IntPtr.Zero) ((delegate* unmanaged[Stdcall]<IntPtr, uint>)Vtbl(obj)[2])(obj);
    }

    private static void Check(int hr)
    {
        if (hr < 0) Marshal.ThrowExceptionForHR(hr);
    }

    private static IReadOnlyList<string> Enumerate()
    {
        Check(DWriteCreateFactory(0 /* shared */, IidFactory, out var factory));
        IntPtr collection = IntPtr.Zero;
        var names = new SortedSet<string>(StringComparer.CurrentCultureIgnoreCase);
        try
        {
            Check(((delegate* unmanaged[Stdcall]<IntPtr, IntPtr*, int, int>)Vtbl(factory)[3])(factory, &collection, 0));
            var count = ((delegate* unmanaged[Stdcall]<IntPtr, uint>)Vtbl(collection)[3])(collection);
            for (uint i = 0; i < count; i++)
            {
                IntPtr family = IntPtr.Zero, localized = IntPtr.Zero;
                try
                {
                    Check(((delegate* unmanaged[Stdcall]<IntPtr, uint, IntPtr*, int>)Vtbl(collection)[4])(collection, i, &family));
                    Check(((delegate* unmanaged[Stdcall]<IntPtr, IntPtr*, int>)Vtbl(family)[6])(family, &localized));
                    if (ReadName(localized) is { Length: > 0 } name && !name.StartsWith('@')) names.Add(name);
                }
                finally
                {
                    Release(localized);
                    Release(family);
                }
            }
        }
        finally
        {
            Release(collection);
            Release(factory);
        }
        if (names.Count == 0) throw new InvalidOperationException("no fonts");
        return [.. names];
    }

    private static string? ReadName(IntPtr strings)
    {
        uint index = 0;
        int exists = 0;
        fixed (char* locale = "en-us")
        {
            Check(((delegate* unmanaged[Stdcall]<IntPtr, char*, uint*, int*, int>)Vtbl(strings)[4])(strings, locale, &index, &exists));
        }
        if (exists == 0) index = 0;
        uint length = 0;
        Check(((delegate* unmanaged[Stdcall]<IntPtr, uint, uint*, int>)Vtbl(strings)[7])(strings, index, &length));
        var buffer = new char[length + 1];
        fixed (char* p = buffer)
        {
            Check(((delegate* unmanaged[Stdcall]<IntPtr, uint, char*, uint, int>)Vtbl(strings)[8])(strings, index, p, length + 1));
        }
        return new string(buffer, 0, (int)length);
    }
}
