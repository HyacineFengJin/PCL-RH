#Requires -Version 7.0
# Read one explicitly supplied CI shortcut through Unicode COM interfaces.
# Never Resolve, Save, or execute the link or its target.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $IsWindows -or $env:GITHUB_ACTIONS -cne 'true') {
    throw 'Shortcut inspection requires a disposable native Windows GitHub Actions runner'
}
if ($args.Count -ne 0) {
    throw 'Supply the shortcut only through PCL_SETUP_LINK'
}
$link = $env:PCL_SETUP_LINK
if ([string]::IsNullOrWhiteSpace($link) -or
    -not [System.IO.Path]::IsPathFullyQualified($link) -or
    [System.IO.Path]::GetExtension($link) -ine '.lnk' -or
    -not [System.IO.File]::Exists($link)) {
    throw 'PCL_SETUP_LINK must name an existing absolute .lnk file'
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;

// Order and widths follow the IShellLinkW vtable in the Microsoft SDK.
// https://learn.microsoft.com/windows/win32/api/shobjidl_core/nn-shobjidl_core-ishelllinkw
[ComImport]
[Guid("000214F9-0000-0000-C000-000000000046")]
[InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
internal interface IPclRhShellLinkW
{
    [PreserveSig] int GetPath(
        [Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder file,
        int count, IntPtr findData, uint flags);
    [PreserveSig] int GetIDList(out IntPtr idList);
    [PreserveSig] int SetIDList(IntPtr idList);
    [PreserveSig] int GetDescription(
        [Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder description, int count);
    [PreserveSig] int SetDescription([MarshalAs(UnmanagedType.LPWStr)] string description);
    [PreserveSig] int GetWorkingDirectory(
        [Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder directory, int count);
    [PreserveSig] int SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string directory);
    [PreserveSig] int GetArguments(
        [Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder arguments, int count);
    [PreserveSig] int SetArguments([MarshalAs(UnmanagedType.LPWStr)] string arguments);
    [PreserveSig] int GetHotkey(out ushort hotkey);
    [PreserveSig] int SetHotkey(ushort hotkey);
    [PreserveSig] int GetShowCmd(out int command);
    [PreserveSig] int SetShowCmd(int command);
    [PreserveSig] int GetIconLocation(
        [Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder iconPath,
        int count, out int iconIndex);
    [PreserveSig] int SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string iconPath, int iconIndex);
    [PreserveSig] int SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path, uint reserved);
    [PreserveSig] int Resolve(IntPtr window, uint flags);
    [PreserveSig] int SetPath([MarshalAs(UnmanagedType.LPWStr)] string path);
}

public static class PclRhShortcutReader
{
    private const int Capacity = 32768;

    private static string ReadField(Func<StringBuilder, int> read, string operation)
    {
        var buffer = new StringBuilder(Capacity, Capacity);
        int result = read(buffer);
        Marshal.ThrowExceptionForHR(result);
        if (result != 0)
            throw new InvalidOperationException(operation + " did not return S_OK");
        if (buffer.Length >= Capacity - 1)
            throw new InvalidOperationException(operation + " exceeded the inspection buffer");
        return buffer.ToString();
    }

    public static string[] Read(string path)
    {
        object instance = null;
        try
        {
            var type = Type.GetTypeFromCLSID(new Guid("00021401-0000-0000-C000-000000000046"), true);
            instance = Activator.CreateInstance(type);
            var link = (IPclRhShellLinkW)instance;
            // STGM_READ = 0. Load initializes the object without opening the
            // target. No path resolution or link mutation is requested.
            ((IPersistFile)instance).Load(path, 0);
            return new string[] {
                // Microsoft documents pfd=NULL as omitting additional target
                // metadata. Use normal path flags, not the 8.3/short-path flag.
                ReadField(b => link.GetPath(b, Capacity, IntPtr.Zero, 0), "GetPath"),
                ReadField(b => link.GetWorkingDirectory(b, Capacity), "GetWorkingDirectory"),
                ReadField(b => link.GetArguments(b, Capacity), "GetArguments")
            };
        }
        finally
        {
            // Both interface casts share this private RCW. Release it once,
            // after copying the strings; never separately release each cast.
            if (instance != null && Marshal.IsComObject(instance))
                Marshal.FinalReleaseComObject(instance);
        }
    }
}
'@

[string[]]$values = [PclRhShortcutReader]::Read($link)
[ordered]@{
    targetPath = $values[0]
    workingDirectory = $values[1]
    arguments = $values[2]
} | ConvertTo-Json -Compress -EscapeHandling EscapeNonAscii
