<#
.SYNOPSIS
  Installs, upgrades and removes the Superba MSI per-user and checks each step.
  Run by .github/workflows/package-test.yml on windows-latest (changes the
  current user's profile: use a disposable machine).
#>
param(
  [Parameter(Mandatory)][string]$StageDir,
  [string]$BaseVersion = '0.1.0',
  [string]$UpgradeVersion = '0.1.1-alpha.1',
  [string]$UpgradeVersion2 = '0.1.1-alpha.2'
)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$work = Join-Path $env:RUNNER_TEMP 'msi-test'
New-Item -ItemType Directory -Path $work -Force | Out-Null
$installDir = Join-Path $env:LOCALAPPDATA 'Programs\Superba'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$files = 'superba.exe','spb.exe','superba-router.exe','LICENSE-MIT','LICENSE-APACHE',
         'THIRD-PARTY-NOTICES.txt','LICENSE-Geist-OFL.txt','superba-msi.marker','spb-linux-x86_64'

function Fail($m) { throw "MSI test failed: $m" }
function Check($ok, $m) { if (-not $ok) { Fail $m } else { Write-Host "ok: $m" } }

function Build($version, $name) {
  $out = Join-Path $work $name
  & "$here\build-msi.ps1" -StageDir $StageDir -Version $version -Out $out
  return $out
}
function Msi($verb, $msi, $log) {
  $p = Start-Process msiexec.exe -ArgumentList @($verb, "`"$msi`"", '/qn', '/norestart', '/l*v', "`"$log`"") -Wait -PassThru
  if ($p.ExitCode -ne 0) {
    Get-Content $log -Tail 60 | Write-Host
    Fail "msiexec $verb exited with $($p.ExitCode)"
  }
}
# Superba's entries in Apps & features: the products Windows Installer has for
# the UpgradeCode of packaging/windows/superba.wxs. A per-user MSI is listed from
# the installer's own registration (no HKCU ...\Uninstall key is written for it).
$upgradeCode = '{669520BE-07D0-496D-8173-085AA3DEE133}'
function ComGet($obj, $name, [object[]]$params) {
  $obj.GetType().InvokeMember($name, [Reflection.BindingFlags]::GetProperty, $null, $obj, $params)
}
function ArpEntries {
  $wi = New-Object -ComObject WindowsInstaller.Installer
  # The pipeline enumerates the returned StringList: one product code each.
  foreach ($code in @(ComGet $wi 'RelatedProducts' @($upgradeCode))) {
    [pscustomobject]@{
      ProductCode    = $code
      DisplayName    = ComGet $wi 'ProductInfo' @($code, 'ProductName')
      DisplayVersion = ComGet $wi 'ProductInfo' @($code, 'VersionString')
    }
  }
}
# The installed WSL helper (user bug 54(d)): an x86-64 Linux program (ELF64,
# e_machine 0x3E) without a program interpreter (PT_INTERP), i.e. static musl.
function CheckLinuxHelper($label) {
  $b = [IO.File]::ReadAllBytes((Join-Path $installDir 'spb-linux-x86_64'))
  $elf = $b.Length -ge 64 -and $b[0] -eq 0x7F -and $b[1] -eq 0x45 -and $b[2] -eq 0x4C -and $b[3] -eq 0x46 -and $b[4] -eq 2
  Check ($elf -and [BitConverter]::ToUInt16($b, 18) -eq 0x3E) "${label}: spb-linux-x86_64 is an x86-64 Linux program"
  $phoff = [BitConverter]::ToUInt64($b, 32)
  $phentsize = [BitConverter]::ToUInt16($b, 54)
  $interp = $false
  for ($i = 0; $i -lt [BitConverter]::ToUInt16($b, 56); $i++) {
    if ([BitConverter]::ToUInt32($b, [int]($phoff + $i * $phentsize)) -eq 3) { $interp = $true }
  }
  Check (-not $interp) "${label}: spb-linux-x86_64 is static ($($b.Length) bytes)"
}
function UserPath { [Environment]::GetEnvironmentVariable('Path', 'User') }
# The user PATH folders without a trailing '\': the MSI writes [INSTALLFOLDER],
# which always ends in one (Windows finds programs either way).
function UserPathDirs {
  @((UserPath) -split ';' | Where-Object { $_ } | ForEach-Object { $_.TrimEnd('\') })
}
function RouterProcs {
  Get-Process -Name superba-router -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($installDir, [StringComparison]::OrdinalIgnoreCase) }
}

$msi1 = Build $BaseVersion 'base.msi'
$msi2 = Build $UpgradeVersion 'upgrade1.msi'
$msi3 = Build $UpgradeVersion2 'upgrade2.msi'
# Not installed here: a far newer release for trying the self-update by hand.
$null = Build '9.0.0' 'superba-9.0.0-windows-x86_64.msi'

# --- install ---------------------------------------------------------------
Msi '/i' $msi1 (Join-Path $work 'install.log')
foreach ($f in $files) { Check (Test-Path (Join-Path $installDir $f)) "installed $f" }
CheckLinuxHelper 'install'
$v = & (Join-Path $installDir 'spb.exe') --version
Check ($LASTEXITCODE -eq 0 -and "$v" -match '\d+\.\d+\.\d+') "spb --version ($v)"
Check ((UserPathDirs) -contains $installDir) 'install folder is on the user PATH'
$arp = @(ArpEntries)
Check ($arp.Count -eq 1) "one Add/Remove Programs entry ($($arp | ForEach-Object { "$($_.DisplayName) $($_.ProductCode)" }))"
Check ($arp[0].DisplayName -eq "Superba $BaseVersion") "ARP name ($($arp[0].DisplayName))"
Check ($arp[0].DisplayVersion -eq $BaseVersion.Split('-')[0]) "ARP version ($($arp[0].DisplayVersion))"
$lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Superba.lnk'
Check (Test-Path $lnk) 'Start menu shortcut'
Check (-not (Test-Path 'HKLM:\Software\Krill\Superba')) 'nothing was written machine-wide'

# A login-startup entry and a running router from the install folder, to see
# the upgrade keep the entry (and stop the process) and the removal drop both.
$rtData = Join-Path $work 'router-data'
New-Item -ItemType Directory -Path $rtData -Force | Out-Null
$routerExe = Join-Path $installDir 'superba-router.exe'
$runCmd = "`"$routerExe`" supervise --data-dir `"$rtData`""
Set-ItemProperty -Path $runKey -Name 'SuperbaRouter' -Value $runCmd
$proc = Start-Process $routerExe -ArgumentList @('supervise', '--data-dir', "`"$rtData`"") -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 3
$routerAlive = -not $proc.HasExited
Write-Host "router process alive before the upgrade: $routerAlive"

# --- upgrade (higher X.Y.Z, then a pre-release of the same X.Y.Z) -----------
Msi '/i' $msi2 (Join-Path $work 'upgrade1.log')
$arp = @(ArpEntries)
Check ($arp.Count -eq 1) 'upgrade leaves one ARP entry'
Check ($arp[0].DisplayName -eq "Superba $UpgradeVersion") "ARP name after the upgrade ($($arp[0].DisplayName))"
foreach ($f in $files) { Check (Test-Path (Join-Path $installDir $f)) "after the upgrade: $f" }
CheckLinuxHelper 'upgrade'
Check (@(UserPathDirs | Where-Object { $_ -eq $installDir }).Count -eq 1) 'PATH has the folder exactly once'
Check ($null -ne (Get-ItemProperty $runKey).SuperbaRouter) 'upgrade keeps the login-startup entry'
Check (@(RouterProcs).Count -eq 0) 'upgrade stopped the router of the install'
& (Join-Path $installDir 'spb.exe') --version | Out-Null
Check ($LASTEXITCODE -eq 0) 'spb runs after the upgrade'

Msi '/i' $msi3 (Join-Path $work 'upgrade2.log')
$arp = @(ArpEntries)
Check ($arp.Count -eq 1 -and $arp[0].DisplayName -eq "Superba $UpgradeVersion2") 'same X.Y.Z pre-release upgrades in place'

# --- uninstall ---------------------------------------------------------------
$proc2 = Start-Process $routerExe -ArgumentList @('supervise', '--data-dir', "`"$rtData`"") -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 3
Msi '/x' $msi3 (Join-Path $work 'uninstall.log')
Check (-not (Test-Path (Join-Path $installDir 'spb.exe'))) 'spb.exe removed'
Check (-not (Test-Path (Join-Path $installDir 'spb-linux-x86_64'))) 'spb-linux-x86_64 removed'
Check ((-not (Test-Path $installDir)) -or @(Get-ChildItem $installDir -Force).Count -eq 0) 'install folder is gone'
Check (-not ((UserPathDirs) -contains $installDir)) 'PATH entry removed'
Check (@(ArpEntries).Count -eq 0) 'ARP entry removed'
Check (-not (Test-Path $lnk)) 'Start menu shortcut removed'
Check ($null -eq (Get-ItemProperty $runKey -ErrorAction SilentlyContinue).SuperbaRouter) 'login-startup entry removed'
Check (@(RouterProcs).Count -eq 0) 'no router process left'
Write-Host 'MSI install, upgrade and uninstall checks passed.'
