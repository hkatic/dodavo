<#
.SYNOPSIS
Prepares (and optionally publishes) the files of a Dodavo release on GitHub.

.DESCRIPTION
Run installer\build.ps1 first. This script then

  1. builds the release tool (cargo --features release-tool),
  2. writes target\installer\update.json: the signed update manifest that
     installed copies of Dodavo fetch from
     https://github.com/<repo>/releases/latest/download/update.json,
  3. verifies the manifest against the installer and the public keys built
     into the program (src\update\keys.rs), and
  4. with -Publish, creates the GitHub release v<version> as a draft with the
     installer, its .sha256 file and update.json, and publishes it after a
     confirmation (needs the GitHub CLI, `gh`, logged in).

The release notes come from release-notes\<version>\<lang>.txt (en.txt is
required; hr.txt, sr-Latn.txt and sl.txt are shown to users of those
languages). The signing key is the file written by `dodavo-release keygen`;
give it with -KeyFile or in the DODAVO_UPDATE_KEY_FILE environment variable.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File installer\publish.ps1
powershell -ExecutionPolicy Bypass -File installer\publish.ps1 -Publish
#>
[CmdletBinding()]
param(
    # The version to publish; default: the version in Cargo.toml.
    [string]$Version,
    # The secret signing key (update-key.secret); default: $env:DODAVO_UPDATE_KEY_FILE.
    [string]$KeyFile = $env:DODAVO_UPDATE_KEY_FILE,
    # Folder with the release notes; default: release-notes\<version>.
    [string]$NotesDir,
    # The GitHub repository that hosts the releases.
    [string]$Repo = 'hkatic/dodavo',
    # Create and publish the GitHub release with `gh`.
    [switch]$Publish,
    # Mark the GitHub release as a pre-release (installed copies skip it).
    [switch]$PreRelease
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# Runs a program whose error output does not matter, for its output and
# $LASTEXITCODE. Windows PowerShell turns what a program writes to stderr into
# errors, which 'Stop' makes fatal even with 2>$null.
function Invoke-Quietly {
    param([string]$Exe, [string[]]$Arguments)
    $ErrorActionPreference = 'Continue'
    & $Exe @Arguments 2>$null
}

if (-not $Version) {
    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    $Version = ($metadata.packages | Where-Object { $_.name -eq 'dodavo' }).version
}
if (-not $Version) { throw 'Could not determine the version.' }
if (-not $NotesDir) { $NotesDir = Join-Path $root "release-notes\$Version" }

$setup = Join-Path $root "target\installer\Dodavo-$Version-setup.exe"
$sha = "$setup.sha256"
$manifest = Join-Path $root 'target\installer\update.json'

if (-not (Test-Path $setup)) {
    throw "$setup does not exist. Run installer\build.ps1 first (for version $Version)."
}
if (-not (Test-Path $sha)) {
    throw "$sha does not exist. Run installer\build.ps1 first."
}
if (-not (Test-Path (Join-Path $NotesDir 'en.txt'))) {
    throw "Release notes are missing: $NotesDir\en.txt (and hr.txt, sr-Latn.txt, sl.txt)."
}
if (-not $KeyFile) {
    throw 'Give the signing key with -KeyFile or set DODAVO_UPDATE_KEY_FILE (see the release guide).'
}
if (-not (Test-Path $KeyFile)) { throw "The signing key $KeyFile does not exist." }

# The tag the manifest's installer URL points at.
$tag = "v$Version"

Write-Host "Building the release tool..."
cargo build --release --features release-tool --bin dodavo-release
if ($LASTEXITCODE -ne 0) { throw 'Building the release tool failed.' }
$tool = Join-Path $root 'target\release\dodavo-release.exe'

Write-Host "Writing $manifest..."
& $tool manifest --installer $setup --key $KeyFile --notes $NotesDir --out $manifest --version $Version
if ($LASTEXITCODE -ne 0) { throw 'Writing the manifest failed.' }

Write-Host "Verifying the manifest against the keys built into Dodavo..."
& $tool verify --manifest $manifest --installer $setup
if ($LASTEXITCODE -ne 0) {
    throw 'The manifest does not verify with the keys in src\update\keys.rs. Installed copies would reject it.'
}

# The built installer should be the committed, tagged source.
$head = Invoke-Quietly 'git' @('describe', '--tags', '--exact-match')
if ($head -ne $tag) {
    Write-Warning "HEAD is not tagged $tag (git describe says '$head'). Tag the release commit: git tag $tag"
}
if (git status --porcelain) {
    Write-Warning 'The working tree has uncommitted changes.'
}

Write-Host ''
Write-Host "Release $tag is ready. Upload these three files to the GitHub release ${tag}:"
Write-Host "  $setup"
Write-Host "  $sha"
Write-Host "  $manifest"
Write-Host "Installed copies will fetch https://github.com/$Repo/releases/latest/download/update.json"
Write-Host ''

if (-not $Publish) {
    Write-Host 'Run again with -Publish to create the GitHub release with gh.'
    exit 0
}

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    throw 'The GitHub CLI (gh) is not installed. Upload the files by hand on the Releases page.'
}

$existing = Invoke-Quietly 'gh' @('release', 'view', $tag, '--repo', $Repo, '--json', 'isDraft', '--jq', '.isDraft')
if ($LASTEXITCODE -eq 0) {
    # A draft that macos/publish.sh has made: every release carries the files of
    # both systems (see README.md). One that is public already is left alone.
    if ("$existing".Trim() -ne 'true') {
        throw "The release $tag is public already on GitHub. Delete it or upload the files by hand."
    }
    Write-Host "Adding the files to the draft release $tag..."
    & gh release upload $tag --repo $Repo --clobber $setup $sha $manifest
    if ($LASTEXITCODE -ne 0) { throw 'gh release upload failed.' }
} else {
    $releaseArgs = @('release', 'create', $tag, '--repo', $Repo, '--draft',
        '--title', "Dodavo $Version", '--notes-file', (Join-Path $NotesDir 'en.txt'),
        $setup, $sha, $manifest)
    if ($PreRelease) { $releaseArgs += '--prerelease' }
    Write-Host "Creating the draft release $tag..."
    & gh @releaseArgs
    if ($LASTEXITCODE -ne 0) { throw 'gh release create failed.' }
}

Write-Host ''
Write-Host "Draft created. Check it at https://github.com/$Repo/releases"
$answer = Read-Host "Publish $tag now? Installed copies will see it within a day (y/N)"
if ($answer -match '^[yY]') {
    & gh release edit $tag --repo $Repo --draft=false
    if ($LASTEXITCODE -ne 0) { throw 'gh release edit failed.' }
    Write-Host "Published. Verify with: curl -L https://github.com/$Repo/releases/latest/download/update.json"
} else {
    Write-Host "Left as a draft. Publish it later on GitHub or with: gh release edit $tag --repo $Repo --draft=false"
}
