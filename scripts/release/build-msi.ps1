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
  Needs the `wix` tool (dotnet tool install --global wix --version 5.0.2).
#>
param(
  [Parameter(Mandatory)][string]$StageDir,
  [Parameter(Mandatory)][string]$Version,
  [Parameter(Mandatory)][string]$Out,
  [string]$ProductVersion
)
$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^(\d+)\.(\d+)\.(\d+)(-(alpha|beta|rc)\.\d+)?$') {
  throw "Unsupported version '$Version'"
}
if (-not $ProductVersion) { $ProductVersion = "$($Matches[1]).$($Matches[2]).$($Matches[3])" }
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$wxs = Join-Path $root 'packaging/windows/superba.wxs'
$marker = Join-Path $root 'packaging/windows/superba-msi.marker'
$icon = Join-Path $root 'assets/icon/superba.ico'
$stage = (Resolve-Path $StageDir).Path
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
  $outDir = Split-Path -Parent $Out
  if ($outDir) { New-Item -ItemType Directory -Path $outDir -Force | Out-Null }
  & wix build $wxs -arch x64 -d "ProductVersion=$ProductVersion" -d "DisplayVersion=$Version" `
    -d "StageDir=$work" -o $Out 2>&1 | Out-Host
  if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
} finally {
  Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host "Built $Out (ProductVersion $ProductVersion, $Version)"
