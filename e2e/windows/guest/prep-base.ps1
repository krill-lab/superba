# Turns the `clean` snapshot into the harness base (`wintest`): a stock Windows 11
# as a retail machine has it. Run over SSH (admin token) on a freshly restored
# `clean`, then shut down and snapshot. Changes, all documented in MATRIX.md:
#   - UAC on with Windows' defaults (EnableLUA=1, ConsentPromptBehaviorAdmin=5,
#     PromptOnSecureDesktop=1): the image ships with UAC off, which gives every
#     process of the administrator account an elevated token. A retail Windows has
#     UAC on: programs the user starts (Start menu, Explorer) run unelevated. The
#     UAC-off machine is the `--base uacoff` variant of the harness.
#   - the Superba MSI of the `clean` snapshot (0.1.1-alpha.1) is uninstalled, so
#     every scenario installs the MSI under test on a machine that never had Superba.
# Nothing else is changed: no VC++ runtime, no GPU driver, no tools.
$ErrorActionPreference = 'Stop'
$sys = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
Set-ItemProperty -Path $sys -Name EnableLUA -Value 1 -Type DWord
Set-ItemProperty -Path $sys -Name ConsentPromptBehaviorAdmin -Value 5 -Type DWord
Set-ItemProperty -Path $sys -Name PromptOnSecureDesktop -Value 1 -Type DWord
"UAC: EnableLUA=1 ConsentPromptBehaviorAdmin=5 PromptOnSecureDesktop=1 (effective after reboot)"

# Per-user MSI products are listed by Windows Installer itself (not under the
# Uninstall registry keys): ask it for every product named Superba.
$installer = New-Object -ComObject WindowsInstaller.Installer
$products = $installer.GetType().InvokeMember('ProductsEx', 'GetProperty', $null, $installer, @('', '', 7))
foreach ($prod in $products) {
    $code = $prod.GetType().InvokeMember('ProductCode', 'GetProperty', $null, $prod, $null)
    $name = $prod.GetType().InvokeMember('InstallProperty', 'GetProperty', $null, $prod, @('ProductName'))
    if ($name -notlike 'Superba*') { continue }
    $p = Start-Process msiexec.exe -ArgumentList "/x $code /qn /norestart" -Wait -PassThru
    "uninstalled $name $code -> exit $($p.ExitCode)"
}
foreach ($d in @("$env:LOCALAPPDATA\Programs\Superba", "$env:LOCALAPPDATA\krill-code", "$env:APPDATA\krill-code")) {
    if (Test-Path $d) { Remove-Item -Recurse -Force $d; "removed $d" }
}
Remove-Item -Force -ErrorAction SilentlyContinue 'C:\Users\Public\Downloads\superba-*.msi'
$path = [Environment]::GetEnvironmentVariable('PATH', 'User')
"user PATH: $path"
"Run key: " + ((Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -ErrorAction SilentlyContinue | Out-String).Trim())
