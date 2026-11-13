# Exercise an actual parent console, without redirecting the GUI child's output.
$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class ConsoleOutputProbe {
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool AllocConsole();
    [DllImport("kernel32.dll")] public static extern bool FreeConsole();
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr CreateFile(string name, uint access, uint share, IntPtr security, uint creation, uint flags, IntPtr template);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool ReadConsoleOutputCharacter(IntPtr output, StringBuilder text, uint length, uint origin, out uint read);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    public static void CreateIsolatedConsole() {
        // A headless runner can already be attached without a console window.
        FreeConsole();
        if (!AllocConsole()) {
            int error = Marshal.GetLastWin32Error();
            throw new System.ComponentModel.Win32Exception(error,
                "Could not allocate test console (Win32 error " + error + ")");
        }
    }
    public static string Read() {
        IntPtr output = CreateFile("CONOUT$", 0x80000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
        if (output == new IntPtr(-1)) throw new Exception("Could not open console buffer");
        try {
            var text = new StringBuilder(32768);
            uint count;
            if (!ReadConsoleOutputCharacter(output, text, 32768, 0, out count))
                throw new Exception("Could not read console buffer");
            return text.ToString();
        } finally { CloseHandle(output); }
    }
}
'@
[ConsoleOutputProbe]::CreateIsolatedConsole()
try {
    $executable = (Resolve-Path (Join-Path $PSScriptRoot '..\..\target\release\aiterm.exe')).Path
    $cases = @(
        @{ Args = '--help'; Exit = 0; Text = 'Usage:' },
        @{ Args = '--aiterm-invalid-option'; Exit = 2; Text = 'unknown argument --aiterm-invalid-option' },
        @{ Args = '--config aiterm-nonexistent-test-config.toml'; Exit = 1; Text = 'config file not found:' }
    )
    foreach ($case in $cases) {
        $info = New-Object Diagnostics.ProcessStartInfo
        $info.FileName = $executable
        $info.Arguments = $case.Args
        $info.UseShellExecute = $false
        # No output redirection: the executable must attach to this console.
        $process = [Diagnostics.Process]::Start($info)
        try {
            if (-not $process.WaitForExit(10000)) {
                $process.Kill()
                throw 'CLI invocation did not exit (unexpected GUI dialog)'
            }
            if ($process.ExitCode -ne $case.Exit) { throw "Unexpected exit code for $($case.Args)" }
            if (-not [ConsoleOutputProbe]::Read().Contains($case.Text)) {
                throw "Output was invisible in the parent console: $($case.Args)"
            }
        } finally { $process.Dispose() }
    }
} finally {
    [ConsoleOutputProbe]::FreeConsole() | Out-Null
}
Write-Host 'Unredirected CLI help and error output passed.'
