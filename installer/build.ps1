<#
.SYNOPSIS
Builds the release program and the Dodavo installer.

.DESCRIPTION
Builds target\release\Dodavo.exe, copies it to target\installer\staging, compiles installer\Dodavo.iss with Inno Setup
6.6 or later, and writes target\installer\Dodavo-<version>-setup.exe with a
.sha256 file next to it for the download page.

When a code signing certificate is given (parameters or environment
variables), Dodavo.exe, the setup and the uninstaller are signed with signtool
from the Windows SDK. Without one, everything is built unsigned.

.PARAMETER Test
Runs "cargo test" first. The end-to-end test needs internet access.

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
$env:DODAVO_SIGN_THUMBPRINT = "0123..."; .\installer\build.ps1 -Test
#>
[CmdletBinding()]
param(
    [switch]$Test,
    [string]$CertificateThumbprint = $env:DODAVO_SIGN_THUMBPRINT,
    [string]$PfxPath = $env:DODAVO_SIGN_PFX,
    [string]$PfxPassword = $env:DODAVO_SIGN_PFX_PASSWORD,
    [string]$TimestampUrl = $env:DODAVO_SIGN_TIMESTAMP_URL
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    function Invoke-Native {
        param([string]$Exe, [string[]]$Arguments)
        & $Exe @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "$(Split-Path -Leaf $Exe) failed with exit code $LASTEXITCODE."
        }
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

    # Version from Cargo.toml.
    $metadata = (cargo metadata --no-deps --format-version 1) | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed." }
    $version = ($metadata.packages | Where-Object { $_.name -eq 'dodavo' }).version
    if (-not $version) { throw "Package 'dodavo' not found in cargo metadata." }
    # Windows version resources take numbers only, e.g. 1.2.0 for 1.2.0-beta.1.
    $versionNumeric = ($version -split '[-+]')[0]
    Write-Host "Dodavo $version"

    if ($Test) {
        Invoke-Native 'cargo' @('test')
    }
    Invoke-Native 'cargo' @('build', '--release')
    # Sign and package a copy: target\release\Dodavo.exe is a hard link to
    # cargo's own output, which signing in place would change as well.
    $staging = Join-Path $root 'target\installer\staging'
    New-Item -ItemType Directory -Force $staging | Out-Null
    $exe = Join-Path $staging 'Dodavo.exe'
    Copy-Item (Join-Path $root 'target\release\Dodavo.exe') $exe -Force

    $iscc = Find-Tool 'ISCC.exe' @(
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
    ) 'Install Inno Setup 6.6 or later, e.g. "winget install JRSoftware.InnoSetup".'

    $isccArgs = @("/DAppVersion=$version", "/DAppVersionNumeric=$versionNumeric", "/DAppExe=$exe")

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

        Write-Host "Signing $exe"
        Invoke-Native $signtool ($signArgs + @($exe))

        # Inno Setup signs the setup and the uninstaller with the same command.
        # $q is a quote and $f the quoted file name, both filled in by ISCC.
        $quoted = $signArgs | ForEach-Object { if ($_ -match '\s') { '$q' + $_ + '$q' } else { $_ } }
        $isccArgs += @('/DSign', ('/Ssigntool=$q' + $signtool + '$q ' + ($quoted -join ' ') + ' $f'))
    } else {
        Write-Host 'No certificate configured: building an unsigned installer.'
    }

    Invoke-Native $iscc ($isccArgs + @((Join-Path $root 'installer\Dodavo.iss')))

    $setup = Join-Path $root "target\installer\Dodavo-$version-setup.exe"
    $hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLowerInvariant()
    # Same format as sha256sum, so "sha256sum -c" works too.
    Set-Content -Path "$setup.sha256" -Value "$hash  $(Split-Path -Leaf $setup)" -Encoding ASCII -NoNewline

    Write-Host ''
    Write-Host "Installer: $setup"
    Write-Host "SHA-256:   $hash"
} finally {
    Pop-Location
}
