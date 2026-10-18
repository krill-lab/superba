<#
.SYNOPSIS
  Builds the per-user Windows MSI from a folder of release files.

.DESCRIPTION
  -StageDir holds superba.exe, spb.exe, superba-router.exe, LICENSE-MIT,
  LICENSE-APACHE, THIRD-PARTY-NOTICES.txt and LICENSE-Geist-OFL.txt (the
  already signed binaries in a release), and spb-linux-x86_64: the static musl
  spb Superba runs inside WSL distributions to migrate their sessions
  (scripts/release/build-musl.sh). -Version is the release version
  without "v" (0.1.0 or 0.1.0-alpha.2): the MSI ProductVersion is its X.Y.Z
  and the full version is kept in the product name and the ARP comments.

  The MSI has the installer pages of packaging/windows/superba-ui.wxs in English
  and, as an embedded language transform named 2052, in Simplified Chinese:
  both cultures are built with the same ProductCode, the zh-CN MSI becomes the
  transform, and the package's languages are "1033,2052", so Windows Installer
  shows the Chinese pages on a Chinese Windows by itself (msiexec /i x.msi
  TRANSFORMS=:2052 forces them on any Windows).

  Needs the `wix` tool and two of its extensions:
    dotnet tool install --global wix --version 5.0.2
    wix extension add -g WixToolset.UI.wixext/5.0.2
    wix extension add -g WixToolset.Util.wixext/5.0.2
#>
param(
  [Parameter(Mandatory)][string]$StageDir,
  [Parameter(Mandatory)][string]$Version,
  [Parameter(Mandatory)][string]$Out,
  [string]$ProductVersion
)
$ErrorActionPreference = 'Stop'
$wixExtensions = @('-ext', 'WixToolset.UI.wixext/5.0.2', '-ext', 'WixToolset.Util.wixext/5.0.2')

# Windows Installer automation (late-bound COM: works in Windows PowerShell 5.1 and pwsh).
function Invoke-Msi($obj, [string]$name, [object[]]$params = @(), [string]$kind = 'InvokeMethod') {
  $obj.GetType().InvokeMember($name, [Reflection.BindingFlags]::$kind, $null, $obj, $params)
}
function Close-Msi([object[]]$objs) {
  foreach ($o in $objs) { if ($null -ne $o) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($o) } }
  [GC]::Collect(); [GC]::WaitForPendingFinalizers()
}
# Embeds what turns $Msi into $LanguageMsi (same package, other culture) as the
# transform storage "<Lcid>" and lists the language in the package's Template
# ("x64;1033" -> "x64;1033,2052"): Windows Installer then applies it on a Windows
# in that language (the multi-language MSI layout of the Windows SDK's
# WiGenXfm.vbs / WiSubStg.vbs / WiLangId.vbs).
function Add-LanguageTransform([string]$Msi, [string]$LanguageMsi, [int]$Lcid, [string]$Work) {
  $mst = [string](Join-Path $Work "$Lcid.mst")   # a plain string: COM rejects PowerShell's wrapped one
  $wi = New-Object -ComObject WindowsInstaller.Installer
  $base = Invoke-Msi $wi 'OpenDatabase' @($Msi, 0)
  $lang = Invoke-Msi $wi 'OpenDatabase' @($LanguageMsi, 0)
  try {
    if (-not (Invoke-Msi $lang 'GenerateTransform' @($base, $mst))) { throw "no difference between $Msi and $LanguageMsi" }
    # Applying it ignores the six row/table/codepage errors (msiTransformError*,
    # 0x3F: the codepage changes 1252 -> 936) and checks only the ProductCode (2).
    [void](Invoke-Msi $lang 'CreateTransformSummaryInfo' @($base, $mst, 0x3F, 0x2))
  } finally { Close-Msi @($lang, $base) }
  $db = Invoke-Msi $wi 'OpenDatabase' @($Msi, 1)   # msiOpenDatabaseModeTransact
  try {
    $view = Invoke-Msi $db 'OpenView' @('SELECT `Name`, `Data` FROM `_Storages`')
    [void](Invoke-Msi $view 'Execute')
    $rec = Invoke-Msi $wi 'CreateRecord' @(2)
    [void](Invoke-Msi $rec 'StringData' @(1, "$Lcid") 'SetProperty')
    [void](Invoke-Msi $rec 'SetStream' @(2, $mst))
    [void](Invoke-Msi $view 'Modify' @(3, $rec))   # msiViewModifyAssign
    [void](Invoke-Msi $view 'Close')
    $si = Invoke-Msi $db 'SummaryInformation' @(1) 'GetProperty'
    $template = Invoke-Msi $si 'Property' @(7) 'GetProperty'   # PID_TEMPLATE, "x64;1033"
    if ($template -notmatch '^[^;]*;1033$') { throw "unexpected package Template '$template'" }
    [void](Invoke-Msi $si 'Property' @(7, "$template,$Lcid") 'SetProperty')
    [void](Invoke-Msi $si 'Persist')
    [void](Invoke-Msi $db 'Commit')
  } finally { Close-Msi @($si, $rec, $view, $db, $wi) }
  Write-Host "Embedded the $Lcid language transform ($((Get-Item $mst).Length) bytes) in $Msi"
}
if ($Version -notmatch '^(\d+)\.(\d+)\.(\d+)(-(alpha|beta|rc)\.\d+)?$') {
  throw "Unsupported version '$Version'"
}
if (-not $ProductVersion) { $ProductVersion = "$($Matches[1]).$($Matches[2]).$($Matches[3])" }
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$pkg = Join-Path $root 'packaging/windows'
$wxs = Join-Path $pkg 'superba.wxs'
$marker = Join-Path $pkg 'superba-msi.marker'
$icon = Join-Path $root 'assets/icon/superba.ico'
$stage = (Resolve-Path $StageDir).Path
# Absolute: the Windows Installer COM object resolves paths against the process's directory.
$Out = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)
# The marker lives in the repository; the MSI takes everything from one folder.
$work = Join-Path ([IO.Path]::GetTempPath()) ("superba-msi-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
  foreach ($f in 'superba.exe','spb.exe','superba-router.exe','LICENSE-MIT','LICENSE-APACHE',
                 'THIRD-PARTY-NOTICES.txt','LICENSE-Geist-OFL.txt','spb-linux-x86_64') {
    $src = Join-Path $stage $f
    if (-not (Test-Path -LiteralPath $src)) { throw "Missing $f in $stage" }
    Copy-Item -LiteralPath $src -Destination $work
  }
  # The WSL helper must be an x86-64 Linux program (ELF64, e_machine 0x3E).
  $elf = [IO.File]::ReadAllBytes((Join-Path $work 'spb-linux-x86_64'))
  if ($elf.Length -lt 20 -or $elf[0] -ne 0x7F -or $elf[1] -ne 0x45 -or $elf[2] -ne 0x4C -or $elf[3] -ne 0x46 -or
      [BitConverter]::ToUInt16($elf, 18) -ne 0x3E) {
    throw 'spb-linux-x86_64 is not an x86-64 Linux program'
  }
  Copy-Item -LiteralPath $marker -Destination $work
  Copy-Item -LiteralPath $icon -Destination $work
  foreach ($f in 'superba-dialog.bmp', 'superba-banner.bmp') {
    Copy-Item -LiteralPath (Join-Path $pkg $f) -Destination $work
  }
  $outDir = Split-Path -Parent $Out
  if ($outDir) { New-Item -ItemType Directory -Path $outDir -Force | Out-Null }
  # One build per culture with the same ProductCode (a language transform must not change it).
  $productCode = [guid]::NewGuid().ToString('B').ToUpperInvariant()
  $zhMsi = Join-Path $work 'superba-zh-CN.msi'
  foreach ($c in @(@{ Culture = 'en-US'; Msi = $Out }, @{ Culture = 'zh-CN'; Msi = $zhMsi })) {
    & wix build $wxs (Join-Path $pkg 'superba-ui.wxs') -arch x64 -culture $c.Culture `
      -loc (Join-Path $pkg "superba.$($c.Culture).wxl") @wixExtensions `
      -d "ProductVersion=$ProductVersion" -d "DisplayVersion=$Version" -d "StageDir=$work" `
      -d "ProductCode=$productCode" -o $c.Msi 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "wix build ($($c.Culture)) failed ($LASTEXITCODE)" }
  }
  Add-LanguageTransform -Msi $Out -LanguageMsi $zhMsi -Lcid 2052 -Work $work
} finally {
  Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host "Built $Out (ProductVersion $ProductVersion, $Version)"
