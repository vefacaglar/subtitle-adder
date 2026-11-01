using System.Runtime.InteropServices;

namespace SubtitleAdder;

/// <summary>
/// Installed font family names from the DirectWrite system font collection (the Windows counterpart of
/// <c>NSFontManager.availableFontFamilies</c>). WinUI has no API for this, so it calls the COM vtables directly.
/// </summary>
public static unsafe class SystemFonts
{
    private static readonly Lazy<IReadOnlyList<string>> LazyFamilies = new(Load);

    /// <summary>Sorted, de-duplicated family names; a short fallback list if DirectWrite is unavailable.</summary>
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
            // IDWriteFactory::GetSystemFontCollection (slot 3).
            Check(((delegate* unmanaged[Stdcall]<IntPtr, IntPtr*, int, int>)Vtbl(factory)[3])(factory, &collection, 0));
            // IDWriteFontCollection::GetFontFamilyCount (3), GetFontFamily (4).
            var count = ((delegate* unmanaged[Stdcall]<IntPtr, uint>)Vtbl(collection)[3])(collection);
            for (uint i = 0; i < count; i++)
            {
                IntPtr family = IntPtr.Zero, localized = IntPtr.Zero;
                try
                {
                    Check(((delegate* unmanaged[Stdcall]<IntPtr, uint, IntPtr*, int>)Vtbl(collection)[4])(collection, i, &family));
                    // IDWriteFontFamily::GetFamilyNames (6).
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

    /// <summary>The en-us name when present (that is what XAML FontFamily accepts everywhere), else the first one.</summary>
    private static string? ReadName(IntPtr strings)
    {
        uint index = 0;
        int exists = 0;
        fixed (char* locale = "en-us")
        {
            // IDWriteLocalizedStrings::FindLocaleName (4).
            Check(((delegate* unmanaged[Stdcall]<IntPtr, char*, uint*, int*, int>)Vtbl(strings)[4])(strings, locale, &index, &exists));
        }
        if (exists == 0) index = 0;
        uint length = 0;
        // GetStringLength (7), GetString (8).
        Check(((delegate* unmanaged[Stdcall]<IntPtr, uint, uint*, int>)Vtbl(strings)[7])(strings, index, &length));
        var buffer = new char[length + 1];
        fixed (char* p = buffer)
        {
            Check(((delegate* unmanaged[Stdcall]<IntPtr, uint, char*, uint, int>)Vtbl(strings)[8])(strings, index, p, length + 1));
        }
        return new string(buffer, 0, (int)length);
    }
}
