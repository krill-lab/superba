<#
.SYNOPSIS
  Lists the desktop app's processes (and Codex processes) with the facts a quit
  report needs: pid, parent, architecture, start time, Electron process type,
  image path. CI diagnostics for the Windows app jobs (user bug 53).

.DESCRIPTION
  The architecture comes from GetProcessInformation(ProcessMachineTypeInfo)
  (Windows 11), the start time and parent from Win32_Process.
#>
param([string]$Label = 'processes')
$ErrorActionPreference = 'Continue'
if (-not ('SbProcArch' -as [type])) {
  Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SbProcArch {
  [StructLayout(LayoutKind.Sequential)]
  struct MachineInfo { public ushort ProcessMachine; public ushort Res0; public uint Attributes; }
  [DllImport("kernel32.dll", SetLastError = true)]
  static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
  [DllImport("kernel32.dll")]
  static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll", SetLastError = true)]
  static extern bool GetProcessInformation(IntPtr h, int cls, out MachineInfo info, uint size);
  public static string Of(uint pid) {
    IntPtr h = OpenProcess(0x1000, false, pid);
    if (h == IntPtr.Zero) return "open failed (" + Marshal.GetLastWin32Error() + ")";
    try {
      MachineInfo info;
      if (!GetProcessInformation(h, 9, out info, (uint)Marshal.SizeOf(typeof(MachineInfo))))
        return "unknown (" + Marshal.GetLastWin32Error() + ")";
      switch (info.ProcessMachine) {
        case 0xAA64: return "arm64";
        case 0x8664: return "x64";
        case 0x014C: return "x86";
        default: return "0x" + info.ProcessMachine.ToString("X4");
      }
    } finally { CloseHandle(h); }
  }
}
'@
}
"--- ${Label} (OS $([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture), this shell $([System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture))"
Get-CimInstance Win32_Process |
  Where-Object { $_.ExecutablePath -like '*\WindowsApps\OpenAI.*' -or $_.Name -like 'codex*' -or $_.Name -eq 'ChatGPT.exe' -or $_.Name -like 'spb*' -or $_.Name -like 'superba*' } |
  Sort-Object CreationDate |
  ForEach-Object {
    $type = if ($_.CommandLine -match '--type=([\w-]+)') { $Matches[1] } elseif ($_.CommandLine -match ' app-server') { 'app-server' } else { 'main' }
    '  pid {0,-6} parent {1,-6} {2,-6} {3:HH:mm:ss.fff} {4,-17} {5}' -f $_.ProcessId, $_.ParentProcessId, [SbProcArch]::Of([uint32]$_.ProcessId), $_.CreationDate, $type, $_.ExecutablePath
  }
