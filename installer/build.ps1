<#
.SYNOPSIS
Builds the release programs and the Dodavo installer.

.DESCRIPTION
Builds Dodavo.exe for every Windows version and architecture the installer
supports, checks that each build imports nothing its Windows versions lack
(installer\check-imports.ps1), compiles installer\Dodavo.iss with Inno Setup
6.6 or later, and writes target\installer\Dodavo-<version>-setup.exe with a
.sha256 file next to it for the download page.

One installer carries four builds and installs the one that fits:

  x64  Windows 10/11 (also Windows 11 on ARM)  stable Rust, x86_64-pc-windows-msvc
  x86  32-bit Windows 10                        stable Rust, i686-pc-windows-msvc
  x64  Windows 7 SP1, 8 and 8.1                 pinned nightly, x86_64-win7-windows-msvc
  x86  32-bit Windows 7 SP1, 8 and 8.1          pinned nightly, i686-win7-windows-msvc

The Windows 7 targets are Tier 3 in Rust: rustup does not ship them, so their
standard library is built from source (-Zbuild-std) by the nightly toolchain
named in -Win7Toolchain, which is installed on first use together with
rust-src. The stable toolchain stays the default for everything else. The
nightly is used only here, only for the two win7 targets, and only in its own
target directory (target\win7).

When a code signing certificate is given (parameters or environment
variables), every Dodavo.exe, the setup and the uninstaller are signed with
signtool from the Windows SDK. Without one, everything is built unsigned.

.PARAMETER Test
Runs "cargo test" first. The end-to-end test needs internet access.

.PARAMETER SkipWin7
Builds only the two Windows 10 programs and an installer that requires
Windows 10, for example when the nightly toolchain cannot be installed.

.PARAMETER Win7Toolchain
The rustup toolchain that builds the win7 targets. The default is a pinned
nightly; move it only deliberately and rerun the full verification
(README.md, "Building"), because Tier 3 targets can break between nightlies.

.PARAMETER CertificateThumbprint
SHA-1 thumbprint of a certificate in the Windows certificate store (also a
hardware token). Default: $env:DODAVO_SIGN_THUMBPRINT.

.PARAMETER PfxPath
A .pfx certificate file, used when no thumbprint is given.
Default: $env:DODAVO_SIGN_PFX.

.PARAMETER PfxPassword
Password of the .pfx file. Default: $env:DODAVO_SIGN_PFX_PASSWORD.

.PARAMETER TimestampUrl
RFC 3161 timestamp server, so signatures stay valid after the certificate
expires. Default: $env:DODAVO_SIGN_TIMESTAMP_URL, or DigiCert's server.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File installer\build.ps1

.EXAMPLE
powershell -ExecutionPolicy Bypass -File installer\build.ps1 -SkipWin7

.EXAMPLE
$env:DODAVO_SIGN_THUMBPRINT = "0123..."; .\installer\build.ps1 -Test
#>
[CmdletBinding()]
param(
    [switch]$Test,
    [switch]$SkipWin7,
    [string]$Win7Toolchain = 'nightly-2026-10-30',
    [string]$CertificateThumbprint = $env:DODAVO_SIGN_THUMBPRINT,
    [string]$PfxPath = $env:DODAVO_SIGN_PFX,
    [string]$PfxPassword = $env:DODAVO_SIGN_PFX_PASSWORD,
    [string]$TimestampUrl = $env:DODAVO_SIGN_TIMESTAMP_URL
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
$savedTargetDir = $env:CARGO_TARGET_DIR
try {
    function Invoke-Native {
        param([string]$Exe, [string[]]$Arguments)
        & $Exe @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "$(Split-Path -Leaf $Exe) failed with exit code $LASTEXITCODE."
        }
    }

    # SHA-256 of a file, lower-case hex. Get-FileHash is missing on some
    # Windows PowerShell 5.1 installations, so certutil is the fallback.
    function Get-Sha256 {
        param([string]$Path)
        if (Get-Command Get-FileHash -ErrorAction SilentlyContinue) {
            return (Get-FileHash $Path -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        $lines = & certutil -hashfile $Path SHA256
        if ($LASTEXITCODE -ne 0) {
            throw "certutil failed with exit code $LASTEXITCODE."
        }
        # The hash is the line between the header and the trailer; older
        # versions print it with spaces between the bytes.
        $hash = ($lines | Where-Object { $_ -match '^[0-9A-Fa-f ]+$' } | Select-Object -First 1) -replace ' ', ''
        if ($hash.Length -ne 64) {
            throw "Could not read the SHA-256 of $Path from certutil."
        }
        return $hash.ToLowerInvariant()
    }

    function Find-Tool {
        param([string]$Name, [string[]]$Candidates, [string]$Hint)
        $cmd = Get-Command $Name -ErrorAction SilentlyContinue
        if ($cmd) { return $cmd.Source }
        foreach ($c in $Candidates) {
            if ($c -and (Test-Path $c)) { return $c }
        }
        throw "$Name was not found. $Hint"
    }

    # The builds, in the order they are made. 'Define' is the ISCC symbol that
    # receives the staged file (see installer\Dodavo.iss).
    $builds = @(
        @{ Name = 'x64'; Target = 'x86_64-pc-windows-msvc'; Win7 = $false; Machine = 'x64'; Define = 'AppExe' }
        @{ Name = 'x86'; Target = 'i686-pc-windows-msvc'; Win7 = $false; Machine = 'x86'; Define = 'AppExeX86' }
        @{ Name = 'x64-win7'; Target = 'x86_64-win7-windows-msvc'; Win7 = $true; Machine = 'x64'; Define = 'AppExeX64Win7' }
        @{ Name = 'x86-win7'; Target = 'i686-win7-windows-msvc'; Win7 = $true; Machine = 'x86'; Define = 'AppExeX86Win7' }
    )
    if ($SkipWin7) {
        $builds = @($builds | Where-Object { -not $_.Win7 })
        Write-Host 'Skipping the Windows 7 builds: the installer will require Windows 10.'
    }

    # Version from Cargo.toml.
    $metadata = (cargo metadata --no-deps --format-version 1) | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed." }
    $version = ($metadata.packages | Where-Object { $_.name -eq 'dodavo' }).version
    if (-not $version) { throw "Package 'dodavo' not found in cargo metadata." }
    # Windows version resources take numbers only, e.g. 1.2.0 for 1.2.0-beta.1.
    $versionNumeric = ($version -split '[-+]')[0]
    Write-Host "Dodavo $version"

    $iscc = Find-Tool 'ISCC.exe' @(
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
    ) 'Install Inno Setup 6.6 or later (32-bit edition), e.g. "winget install JRSoftware.InnoSetup".'

    # Toolchains. "rustup target add" is a no-op when the target is installed.
    Invoke-Native 'rustup' @('target', 'add', 'i686-pc-windows-msvc')
    if (-not $SkipWin7) {
        Invoke-Native 'rustup' @('toolchain', 'install', $Win7Toolchain, '--profile', 'minimal', '--component', 'rust-src')
    }

    if ($Test) {
        Invoke-Native 'cargo' @('test')
    }

    $staging = Join-Path $root 'target\installer\staging'
    $checkImports = Join-Path $PSScriptRoot 'check-imports.ps1'
    $isccArgs = @("/DAppVersion=$version", "/DAppVersionNumeric=$versionNumeric")
    $stagedExes = @()
    foreach ($b in $builds) {
        Write-Host ''
        Write-Host "Building $($b.Name) ($($b.Target))"
        if ($b.Win7) {
            $env:CARGO_TARGET_DIR = Join-Path $root 'target\win7'
            Invoke-Native 'cargo' @("+$Win7Toolchain", 'build', '--release', '-Zbuild-std', '--target', $b.Target)
            $built = Join-Path $root "target\win7\$($b.Target)\release\Dodavo.exe"
        } else {
            $env:CARGO_TARGET_DIR = $null
            Invoke-Native 'cargo' @('build', '--release', '--target', $b.Target)
            $built = Join-Path $root "target\$($b.Target)\release\Dodavo.exe"
        }
        # Sign and package a copy: the file under target\ is cargo's own
        # output, which signing in place would change as well.
        $dir = Join-Path $staging $b.Name
        New-Item -ItemType Directory -Force $dir | Out-Null
        $exe = Join-Path $dir 'Dodavo.exe'
        Copy-Item $built $exe -Force

        # A build that imports something its Windows versions lack would not
        # start there, so refuse to package it.
        $profile = if ($b.Win7) { 'win7' } else { 'win10' }
        & $checkImports -Path $exe -Profile $profile -ExpectedMachine $b.Machine
        if ($LASTEXITCODE -ne 0) { throw "$exe imports functions that are missing on its target Windows versions." }

        $isccArgs += "/D$($b.Define)=$exe"
        $stagedExes += $exe
    }
    $env:CARGO_TARGET_DIR = $savedTargetDir

    if ($CertificateThumbprint -or $PfxPath) {
        $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
        $sdkSigntool = Get-ChildItem $kits -Directory -Filter '10.*' -ErrorAction SilentlyContinue |
            Sort-Object { [version]$_.Name } -Descending |
            ForEach-Object { Join-Path $_.FullName 'x64\signtool.exe' } |
            Where-Object { Test-Path $_ } |
            Select-Object -First 1
        $signtool = Find-Tool 'signtool.exe' @($sdkSigntool) 'Install the Windows SDK (Signing Tools for Desktop Apps).'

        if (-not $TimestampUrl) { $TimestampUrl = 'http://timestamp.digicert.com' }
        $signArgs = @('sign', '/fd', 'sha256', '/tr', $TimestampUrl, '/td', 'sha256')
        if ($CertificateThumbprint) {
            $signArgs += @('/sha1', ($CertificateThumbprint -replace '\s', ''))
        } else {
            if (-not (Test-Path $PfxPath)) { throw "Certificate file not found: $PfxPath" }
            $signArgs += @('/f', (Resolve-Path $PfxPath).Path)
            if ($PfxPassword) { $signArgs += @('/p', $PfxPassword) }
        }

        foreach ($exe in $stagedExes) {
            Write-Host "Signing $exe"
            Invoke-Native $signtool ($signArgs + @($exe))
        }

        # Inno Setup signs the setup and the uninstaller with the same command.
        # $q is a quote and $f the quoted file name, both filled in by ISCC.
        $quoted = $signArgs | ForEach-Object { if ($_ -match '\s') { '$q' + $_ + '$q' } else { $_ } }
        $isccArgs += @('/DSign', ('/Ssigntool=$q' + $signtool + '$q ' + ($quoted -join ' ') + ' $f'))
    } else {
        Write-Host 'No certificate configured: building an unsigned installer.'
    }

    Invoke-Native $iscc ($isccArgs + @((Join-Path $root 'installer\Dodavo.iss')))

    $setup = Join-Path $root "target\installer\Dodavo-$version-setup.exe"
    $hash = Get-Sha256 $setup
    # Same format as sha256sum, so "sha256sum -c" works too.
    Set-Content -Path "$setup.sha256" -Value "$hash  $(Split-Path -Leaf $setup)" -Encoding ASCII -NoNewline

    Write-Host ''
    Write-Host "Installer: $setup"
    Write-Host "SHA-256:   $hash"
} finally {
    $env:CARGO_TARGET_DIR = $savedTargetDir
    Pop-Location
}
