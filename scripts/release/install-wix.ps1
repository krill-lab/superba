<#
.SYNOPSIS
  Installs WiX 5.0.2 and the two WiX extensions build-msi.ps1 uses (CI and release jobs).

.DESCRIPTION
  dotnet tool install --global wix --version 5.0.2, then
  wix extension add -g WixToolset.UI.wixext/5.0.2 and WixToolset.Util.wixext/5.0.2
  (the installer pages and the "Launch Superba" action of packaging/windows/superba-ui.wxs).
  Each extension DLL must have the pinned sha256: the DLL inside the 5.0.2 nupkg whose
  sha512 matches nuget.org's catalog. On GitHub Actions the tool folder is added to the PATH
  of the following steps.
#>
$ErrorActionPreference = 'Stop'
$version = '5.0.2'
$extensions = [ordered]@{
  'WixToolset.UI.wixext'   = '5ce7f958b6a2b57bcb98c19615c5a3da46976a6e78a6af055fdef99e9e9e8f06'
  'WixToolset.Util.wixext' = 'c76cd00e8800cf785ff3cffd2ed3475915b487c7a3d441a266175cdc326912eb'
}
dotnet tool install --global wix --version $version
if ($LASTEXITCODE -ne 0) { throw 'wix install failed' }
$tools = Join-Path $env:USERPROFILE '.dotnet\tools'
$wix = Join-Path $tools 'wix.exe'
foreach ($id in $extensions.Keys) {
  & $wix extension add -g "$id/$version"
  if ($LASTEXITCODE -ne 0) { throw "wix extension add $id/$version failed" }
  $dll = Join-Path $env:USERPROFILE ".wix\extensions\$id\$version\wixext5\$id.dll"
  $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $dll).Hash.ToLowerInvariant()
  if ($got -ne $extensions[$id]) { throw "$id ${version}: sha256 $got, expected $($extensions[$id])" }
  Write-Host "$id $version ok ($got)"
}
if ($env:GITHUB_PATH) { $tools >> $env:GITHUB_PATH }
