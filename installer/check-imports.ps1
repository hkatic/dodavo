<#
.SYNOPSIS
Checks that a Dodavo.exe imports nothing newer than the Windows versions it
is meant for.

.DESCRIPTION
Reads the import and delay-import tables of a Windows executable (in pure
PowerShell, no SDK tools needed) and fails when it finds a DLL or function
that the target Windows versions do not have. The loader refuses to start a
program with such an import before any of its code runs, so this is the
check that keeps the program starting on old Windows.

Profiles:
  win10  Builds from the stable Rust toolchain (Windows 10 and later). The
         exe must not import the per-monitor DPI functions (Windows 10
         version 1607) or the dynamic C runtime (vcruntime140.dll, UCRT).
  win7   Builds from the *-win7-windows-msvc targets (Windows 7 SP1, 8 and
         8.1). In addition, nothing introduced in Windows 8 or later is
         allowed: combase.dll, bcryptprimitives.dll, the l1-2 API sets,
         WaitOnAddress, GetSystemTimePreciseAsFileTime, ProcessPrng, ...

Always prints the full list of imports so a new offender is easy to spot.

.PARAMETER Path
The executable to check.

.PARAMETER Profile
"win10" (default) or "win7", see above.

.PARAMETER ExpectedMachine
"x86" or "x64": also fail when the executable is for a different processor
architecture.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File installer\check-imports.ps1 target\release\Dodavo.exe

.EXAMPLE
installer\check-imports.ps1 target\win7\x86_64-win7-windows-msvc\release\Dodavo.exe -Profile win7 -ExpectedMachine x64
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory, Position = 0)]
    [string]$Path,
    [ValidateSet('win10', 'win7')]
    [string]$Profile = 'win10',
    [ValidateSet('x86', 'x64')]
    [string]$ExpectedMachine
)

$ErrorActionPreference = 'Stop'

# DLLs and functions that no build may import. Patterns are wildcards
# (-like), case-insensitive.
$deniedDlls = @(
    'vcruntime140*.dll', 'msvcp140*.dll', 'ucrtbase.dll', 'api-ms-win-crt-*'
)
$deniedFunctions = @(
    # Windows 10 version 1607 (src/ui/window.rs loads them at run time).
    'GetDpiForWindow', 'GetDpiForSystem', 'GetSystemMetricsForDpi',
    'SystemParametersInfoForDpi', 'AdjustWindowRectExForDpi',
    'SetThreadDpiAwarenessContext', 'SetProcessDpiAwarenessContext',
    'EnableNonClientDpiScaling', 'GetThreadDpiAwarenessContext',
    # Windows 8.1 (shcore.dll).
    'GetDpiForMonitor', 'SetProcessDpiAwareness', 'GetProcessDpiAwareness'
)
if ($Profile -eq 'win7') {
    # Introduced in Windows 8 or later.
    $deniedDlls += @(
        'combase.dll', 'bcryptprimitives.dll', 'shcore.dll',
        'api-ms-win-core-synch-l1-2-0.dll', 'api-ms-win-core-*-l1-2-*',
        'api-ms-win-core-*-l1-3-*', 'api-ms-win-core-*-l2-*'
    )
    $deniedFunctions += @(
        'WaitOnAddress', 'WakeByAddressSingle', 'WakeByAddressAll',
        'GetSystemTimePreciseAsFileTime', 'ProcessPrng',
        'CoIncrementMTAUsage', 'CoDecrementMTAUsage',
        'GetTempPath2W', 'SetThreadDescription', 'GetThreadDescription',
        'PrefetchVirtualMemory', 'GetOverlappedResultEx', 'CreateFile2',
        'RoInitialize', 'RoGetActivationFactory', 'WindowsCreateString'
    )
}

$bytes = [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Path).Path)

function U16([int]$o) { [BitConverter]::ToUInt16($bytes, $o) }
function U32([int]$o) { [BitConverter]::ToUInt32($bytes, $o) }
function U64([int]$o) { [BitConverter]::ToUInt64($bytes, $o) }
function Str([int]$o) {
    $end = $o
    while ($bytes[$end] -ne 0) { $end++ }
    [Text.Encoding]::ASCII.GetString($bytes, $o, $end - $o)
}

if ($bytes.Length -lt 0x40 -or (U16 0) -ne 0x5A4D) { throw "$Path is not an executable (no MZ header)." }
$pe = U32 0x3C
if ((U32 $pe) -ne 0x4550) { throw "$Path is not a PE executable." }
$coff = $pe + 4
$machine = U16 $coff
$sectionCount = U16 ($coff + 2)
$optionalSize = U16 ($coff + 16)
$optional = $coff + 20
$magic = U16 $optional
switch ($magic) {
    0x10B { $pe32Plus = $false; $dirs = $optional + 96 }
    0x20B { $pe32Plus = $true; $dirs = $optional + 112 }
    default { throw "Unknown optional header magic 0x$($magic.ToString('X'))." }
}
$machineName = switch ($machine) { 0x14C { 'x86' } 0x8664 { 'x64' } 0xAA64 { 'arm64' } default { "0x$($machine.ToString('X'))" } }

# Section table, for translating virtual addresses to file offsets.
$sections = @()
$sectionTable = $optional + $optionalSize
for ($i = 0; $i -lt $sectionCount; $i++) {
    $s = $sectionTable + 40 * $i
    $virtualSize = U32 ($s + 8)
    $rawSize = U32 ($s + 16)
    $sections += [pscustomobject]@{
        Va   = U32 ($s + 12)
        Size = [Math]::Max($virtualSize, $rawSize)
        Raw  = U32 ($s + 20)
    }
}
function Offset([uint32]$rva) {
    foreach ($s in $sections) {
        if ($rva -ge $s.Va -and $rva -lt $s.Va + $s.Size) { return [int]($rva - $s.Va + $s.Raw) }
    }
    throw "Virtual address 0x$($rva.ToString('X')) is outside every section."
}
function Directory([int]$index) {
    $rva = U32 ($dirs + 8 * $index)
    $size = U32 ($dirs + 8 * $index + 4)
    if ($rva -eq 0 -or $size -eq 0) { return $null }
    Offset $rva
}

# Reads a thunk array (import name table) into function names.
function Thunks([uint32]$rva) {
    $names = @()
    if ($rva -eq 0) { return $names }
    $o = Offset $rva
    while ($true) {
        if ($pe32Plus) {
            $t = U64 $o; $o += 8
            if ($t -eq 0) { break }
            $byOrdinal = ($t -band 0x8000000000000000) -ne 0
        } else {
            $t = U32 $o; $o += 4
            if ($t -eq 0) { break }
            $byOrdinal = ($t -band 0x80000000) -ne 0
        }
        if ($byOrdinal) {
            $names += '#' + ($t -band 0xFFFF)
        } else {
            $names += Str ((Offset ([uint32]($t -band 0x7FFFFFFF))) + 2)
        }
    }
    $names
}

# dll name (lower case) -> list of function names
$imports = [ordered]@{}
function Add-Import([string]$dll, [string[]]$functions) {
    $key = $dll.ToLowerInvariant()
    if (-not $imports.Contains($key)) { $imports[$key] = @() }
    $imports[$key] += $functions
}

$importDir = Directory 1
if ($null -ne $importDir) {
    for ($d = $importDir; ; $d += 20) {
        $nameRva = U32 ($d + 12)
        if ($nameRva -eq 0) { break }
        $int = U32 $d
        if ($int -eq 0) { $int = U32 ($d + 16) }
        Add-Import (Str (Offset $nameRva)) (Thunks $int)
    }
}
$delayDir = Directory 13
if ($null -ne $delayDir) {
    for ($d = $delayDir; ; $d += 32) {
        $nameRva = U32 ($d + 4)
        if ($nameRva -eq 0) { break }
        Add-Import ((Str (Offset $nameRva)) + ' (delay-loaded)') (Thunks (U32 ($d + 16)))
    }
}

Write-Host "$Path ($machineName, profile $Profile)"
$problems = @()
if ($ExpectedMachine -and $machineName -ne $ExpectedMachine) {
    $problems += "The executable is for $machineName, expected $ExpectedMachine."
}
foreach ($dll in $imports.Keys) {
    $functions = $imports[$dll] | Sort-Object -Unique
    Write-Host ("  {0} ({1}): {2}" -f $dll, $functions.Count, ($functions -join ' '))
    $baseName = ($dll -split ' ')[0]
    foreach ($pattern in $deniedDlls) {
        if ($baseName -like $pattern) { $problems += "$dll is not available on the target Windows versions." }
    }
    foreach ($f in $functions) {
        foreach ($pattern in $deniedFunctions) {
            if ($f -like $pattern) { $problems += "$dll!$f is not available on the target Windows versions." }
        }
    }
}

if ($problems.Count -gt 0) {
    Write-Host ''
    Write-Host "FAILED: $Path" -ForegroundColor Red
    $problems | Sort-Object -Unique | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    exit 1
}
Write-Host "OK: no imports newer than the $Profile baseline." -ForegroundColor Green
exit 0
