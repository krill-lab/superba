<#
.SYNOPSIS
  Lists the DLLs Windows programs import and fails when one needs the Microsoft
  C/C++ runtime (VCRUNTIME140.dll, MSVCP140.dll, ucrtbase.dll, the UCRT
  api-ms-win-crt-* sets, ...).

.DESCRIPTION
  A clean Windows has no VC++ redistributable and the MSI does not ship it: a
  program that imports it exits at once with 0xC0000135 (user bug 43). The
  programs are built with the static CRT (.cargo/config.toml); this check keeps
  a RUSTFLAGS override or a new C dependency from bringing the DLLs back.
  Reads the PE import and delay-load import tables itself (no SDK tools needed).

.EXAMPLE
  ./scripts/release/check-imports.ps1 target/release/superba.exe target/release/spb.exe
#>
param([Parameter(Mandatory, ValueFromRemainingArguments)][string[]]$Path)
$ErrorActionPreference = 'Stop'

# Runtime DLLs that are not part of Windows itself (msvcrt.dll is, and is allowed).
$runtime = '^(vcruntime\d+(_\d+)?|msvcp\d+(_\w+)?|msvcr\d+|ucrtbased?|concrt\d+|vccorlib\d+|vcomp\d+)\.dll$|^api-ms-win-crt-'

function Read-U16([byte[]]$b, [long]$o) { [BitConverter]::ToUInt16($b, $o) }
function Read-U32([byte[]]$b, [long]$o) { [BitConverter]::ToUInt32($b, $o) }
function Read-CString([byte[]]$b, [long]$o) {
  $end = $o
  while ($end -lt $b.Length -and $b[$end] -ne 0) { $end++ }
  [Text.Encoding]::ASCII.GetString($b, $o, $end - $o)
}

function Get-PeImports([string]$file) {
  $b = [IO.File]::ReadAllBytes($file)
  if ($b.Length -lt 64 -or $b[0] -ne 0x4D -or $b[1] -ne 0x5A) { throw "$file is not a PE file" }
  $pe = Read-U32 $b 0x3C
  if ((Read-U32 $b $pe) -ne 0x00004550) { throw "$file has no PE signature" }
  $sections = Read-U16 $b ($pe + 6)
  $optSize = Read-U16 $b ($pe + 20)
  $opt = $pe + 24
  $magic = Read-U16 $b $opt
  $dirs = switch ($magic) { 0x20B { $opt + 112 } 0x10B { $opt + 96 } default { throw "$file has an unknown optional header ($magic)" } }
  $dirCount = Read-U32 $b ($dirs - 4)
  $table = $opt + $optSize
  function RvaToOffset([long]$rva) {
    for ($i = 0; $i -lt $sections; $i++) {
      $s = $table + 40 * $i
      $va = Read-U32 $b ($s + 12)
      $size = [Math]::Max((Read-U32 $b ($s + 8)), (Read-U32 $b ($s + 16)))
      if ($rva -ge $va -and $rva -lt $va + $size) { return $rva - $va + (Read-U32 $b ($s + 20)) }
    }
    throw "RVA $rva is outside every section of $file"
  }
  $names = New-Object System.Collections.Generic.List[string]
  # Directory 1: imports (20-byte descriptors, name RVA at +12).
  if ($dirCount -gt 1 -and (Read-U32 $b ($dirs + 8)) -ne 0) {
    $d = RvaToOffset (Read-U32 $b ($dirs + 8))
    while ($true) {
      $nameRva = Read-U32 $b ($d + 12)
      if ($nameRva -eq 0) { break }
      $names.Add((Read-CString $b (RvaToOffset $nameRva)))
      $d += 20
    }
  }
  # Directory 13: delay-load imports (32-byte descriptors, name RVA at +4).
  if ($dirCount -gt 13 -and (Read-U32 $b ($dirs + 104)) -ne 0) {
    $d = RvaToOffset (Read-U32 $b ($dirs + 104))
    while ($true) {
      $nameRva = Read-U32 $b ($d + 4)
      if ($nameRva -eq 0) { break }
      $names.Add((Read-CString $b (RvaToOffset $nameRva)) + ' (delay-load)')
      $d += 32
    }
  }
  $names
}

$failed = @()
foreach ($file in $Path) {
  $imports = @(Get-PeImports (Resolve-Path -LiteralPath $file).Path | Sort-Object -Unique)
  Write-Host "$file imports: $($imports -join ', ')"
  $bad = @($imports | Where-Object { ($_ -replace ' \(delay-load\)$', '') -match $runtime })
  if ($bad.Count -gt 0) { $failed += "$file needs $($bad -join ', ')" }
}
if ($failed.Count -gt 0) {
  throw "Programs import the C runtime DLLs a clean Windows lacks (build with +crt-static): $($failed -join '; ')"
}
Write-Host 'ok: no C runtime DLL imports'
