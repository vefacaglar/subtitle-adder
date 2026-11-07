using System.Runtime.InteropServices;
using System.Runtime.Versioning;

namespace SubtitleAdder.Core;

/// <summary>
/// Minimal DPAPI (<c>CryptProtectData</c>, current-user scope) wrapper: data protected here can be unprotected only by
/// the same Windows account, so a copied file is useless on another account or machine.
/// </summary>
[SupportedOSPlatform("windows")]
internal static class WindowsDataProtection
{
    private const int CryptProtectUiForbidden = 0x1;

    [StructLayout(LayoutKind.Sequential)]
    private struct DataBlob
    {
        public int cbData;
        public IntPtr pbData;
    }

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern bool CryptProtectData(
        ref DataBlob pDataIn,
        string? szDataDescr,
        ref DataBlob pOptionalEntropy,
        IntPtr pvReserved,
        IntPtr pPromptStruct,
        int dwFlags,
        ref DataBlob pDataOut);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport("crypt32.dll", SetLastError = true)]
    private static extern bool CryptUnprotectData(
        ref DataBlob pDataIn,
        IntPtr ppszDataDescr,
        ref DataBlob pOptionalEntropy,
        IntPtr pvReserved,
        IntPtr pPromptStruct,
        int dwFlags,
        ref DataBlob pDataOut);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport("kernel32.dll")]
    private static extern IntPtr LocalFree(IntPtr hMem);

    public static byte[]? Protect(byte[] data, byte[] entropy) => Run(data, entropy, protect: true);

    public static byte[]? Unprotect(byte[] data, byte[] entropy) => Run(data, entropy, protect: false);

    private static byte[]? Run(byte[] data, byte[] entropy, bool protect)
    {
        var input = ToBlob(data);
        var optionalEntropy = ToBlob(entropy);
        var output = default(DataBlob);
        try
        {
            var ok = protect
                ? CryptProtectData(ref input, null, ref optionalEntropy, IntPtr.Zero, IntPtr.Zero, CryptProtectUiForbidden, ref output)
                : CryptUnprotectData(ref input, IntPtr.Zero, ref optionalEntropy, IntPtr.Zero, IntPtr.Zero, CryptProtectUiForbidden, ref output);
            if (!ok || output.pbData == IntPtr.Zero) return null;
            var result = new byte[output.cbData];
            Marshal.Copy(output.pbData, result, 0, output.cbData);
            return result;
        }
        catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException)
        {
            return null;
        }
        finally
        {
            FreeBlob(input, wipe: true);
            FreeBlob(optionalEntropy, wipe: false);
            if (output.pbData != IntPtr.Zero)
            {
                if (!protect)
                {
                    // Unprotected plaintext (a vault key) is wiped before the buffer goes back to the OS.
                    Marshal.Copy(new byte[output.cbData], 0, output.pbData, output.cbData);
                }
                LocalFree(output.pbData);
            }
        }
    }

    private static DataBlob ToBlob(byte[] bytes)
    {
        var blob = new DataBlob { cbData = bytes.Length, pbData = Marshal.AllocHGlobal(Math.Max(1, bytes.Length)) };
        Marshal.Copy(bytes, 0, blob.pbData, bytes.Length);
        return blob;
    }

    private static void FreeBlob(DataBlob blob, bool wipe)
    {
        if (blob.pbData == IntPtr.Zero) return;
        if (wipe && blob.cbData > 0)
        {
            Marshal.Copy(new byte[blob.cbData], 0, blob.pbData, blob.cbData);
        }
        Marshal.FreeHGlobal(blob.pbData);
    }
}
