<#
.SYNOPSIS
Writes res\third-party-licenses.txt, the licenses of the crates built into
Dodavo, the Rust standard library included.

.DESCRIPTION
The licenses of Dodavo's dependencies (MIT, Apache-2.0, BSD, ISC and others)
require their notices to go with every copy of the program, and MPL-2.0 that
recipients are told where the source code is. Dodavo shows the file in
Help > License, and Setup installs it as THIRD-PARTY-LICENSES.txt next to
the program.

The crates in Cargo.lock come from cargo-about, with installer\about.toml
(which crates and licenses) and installer\about.hbs (the layout). The Rust
standard library, which every Rust program contains, is not in Cargo.lock:
its notices come from the COPYRIGHT-library.html that each Rust toolchain
ships, for the toolchain cargo uses here (the Windows 10 builds) and the
nightly of the Windows 7 builds (build.ps1). Of its crates, those are listed
that std depends on (rust-src's library\Cargo.lock) and that the toolchain
builds for Windows (the .rlib files of its standard library).

The file is committed, so a plain "cargo build" needs nothing else, and it is
written only when its text changes, so that cargo does not rebuild the
program for nothing. installer\build.ps1 runs this script before every
installer build. Run it after changing dependencies or toolchains and commit
the result together with Cargo.lock.

Needs cargo-about (cargo install cargo-about --locked --features cli) and
rust-src for both toolchains, which the script installs with rustup. It reads
the crate sources through cargo (which may download them) and may look up
license information online.

.PARAMETER Win7Toolchain
The nightly that builds the win7 targets. The default is the one pinned in
build.ps1 (its -Win7Toolchain).

.EXAMPLE
powershell -ExecutionPolicy Bypass -File installer\licenses.ps1
#>
[CmdletBinding()]
param(
    [string]$Win7Toolchain
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    function Invoke-Native {
        param([string]$Exe, [string[]]$Arguments)
        & $Exe @Arguments | Out-Host
        if ($LASTEXITCODE -ne 0) {
            throw "$Exe failed with exit code $LASTEXITCODE."
        }
    }

    # The license ids to include for an SPDX expression: every part of an
    # AND, and of a choice (OR, or the old "MIT/Apache-2.0") MIT where
    # offered, as for the other crates (about.toml), else the first.
    function Select-Licenses {
        param([string]$Expression)
        foreach ($part in (($Expression -replace '[()]', '' -replace '/', ' OR ') -split ' AND ')) {
            $choices = @($part -split ' OR ' | ForEach-Object { $_.Trim() })
            $pick = if ($choices -contains 'MIT') { 'MIT' } else { $choices[0] }
            $pick -split ' WITH ' | ForEach-Object { $_.Trim() }
        }
    }

    function Get-HtmlText {
        param([string]$Html)
        ([Net.WebUtility]::HtmlDecode($Html) -replace "`r`n", "`n").Trim()
    }

    # The standard library's notices in one toolchain: its in-tree entries
    # (path, license, copyright) and its crates from crates.io with their
    # license and notice files.
    function Get-StdNotices {
        param([string]$Toolchain)
        $sysroot = (& rustc "+$Toolchain" --print sysroot | Out-String).Trim()
        $hostLine = & rustc "+$Toolchain" -vV | Where-Object { $_ -like 'host: *' }
        $hostTarget = ($hostLine -replace '^host: ', '').Trim()
        $lockPath = Join-Path $sysroot 'lib\rustlib\src\rust\library\Cargo.lock'
        $htmlPath = Join-Path $sysroot 'share\doc\rust\COPYRIGHT-library.html'
        $libDir = Join-Path $sysroot "lib\rustlib\$hostTarget\lib"
        foreach ($path in @($lockPath, $htmlPath, $libDir)) {
            if (-not (Test-Path $path)) { throw "$path is missing (toolchain $Toolchain)." }
        }

        # The packages of the library workspace, by "name version".
        $packages = @{}
        $byName = @{}
        foreach ($block in @([IO.File]::ReadAllText($lockPath) -split '\[\[package\]\]' | Select-Object -Skip 1)) {
            $name = [regex]::Match($block, '(?m)^name = "([^"]+)"').Groups[1].Value
            $version = [regex]::Match($block, '(?m)^version = "([^"]+)"').Groups[1].Value
            $list = [regex]::Match($block, '(?s)\ndependencies = \[(.*?)\]')
            $deps = @()
            if ($list.Success) {
                $deps = @([regex]::Matches($list.Groups[1].Value, '"([^"]+)"') | ForEach-Object { $_.Groups[1].Value })
            }
            $package = @{
                Name = $name
                Version = $version
                Registry = $block -match '(?m)^source = "registry\+'
                Deps = $deps
            }
            $packages["$name $version"] = $package
            if (-not $byName.ContainsKey($name)) { $byName[$name] = New-Object System.Collections.ArrayList }
            [void]$byName[$name].Add($package)
        }
        if (-not $byName.ContainsKey('std')) { throw "$lockPath has no package std." }

        # Everything std depends on (for any target, and its tests)...
        $seen = @{}
        $queue = New-Object System.Collections.Queue
        $queue.Enqueue($byName['std'][0])
        while ($queue.Count -gt 0) {
            $package = $queue.Dequeue()
            $key = "$($package.Name) $($package.Version)"
            if ($seen.ContainsKey($key)) { continue }
            $seen[$key] = $package
            foreach ($dep in $package.Deps) {
                $parts = $dep -split ' '
                $next = if ($parts.Count -gt 1) {
                    $packages["$($parts[0]) $($parts[1])"]
                } elseif ($byName.ContainsKey($parts[0])) {
                    $byName[$parts[0]][0]
                }
                if (-not $next) { throw "library\Cargo.lock: dependency $dep not found." }
                $queue.Enqueue($next)
            }
        }
        # ...of which the toolchain builds for Windows what it ships as .rlib.
        $built = @{}
        Get-ChildItem $libDir -Filter '*.rlib' | ForEach-Object {
            $built[($_.BaseName -replace '^lib', '' -replace '-[0-9a-f]+$', '')] = $true
        }

        $html = [IO.File]::ReadAllText($htmlPath)
        $crates = @()
        foreach ($package in $seen.Values) {
            if (-not $package.Registry -or -not $built.ContainsKey(($package.Name -replace '-', '_'))) { continue }
            $id = "$($package.Name)-$($package.Version)"
            $entry = [regex]::Match($html, '(?s)<h3>\S+ ' + [regex]::Escape($id) + '</h3>(.*?)(?=<h3>|<h2|</body>)')
            if (-not $entry.Success) { throw "$htmlPath has no entry for $id." }
            $license = Get-HtmlText ([regex]::Match($entry.Groups[1].Value, '<b>License:</b>([^<]*)</p>').Groups[1].Value)
            $notices = @([regex]::Matches($entry.Groups[1].Value, '(?s)<summary><code>([^<]+)</code></summary>\s*<pre>(.*?)</pre>') | ForEach-Object {
                @{ File = Get-HtmlText $_.Groups[1].Value; Text = Get-HtmlText $_.Groups[2].Value }
            })
            # Only the notice of the license chosen, like for the other crates.
            $chosen = @(Select-Licenses $license)
            if ($chosen.Count -eq 1 -and $chosen[0] -eq 'MIT') {
                $mit = @($notices | Where-Object { $_.File -match 'mit' })
                if ($mit.Count -gt 0) { $notices = $mit }
            }
            $crates += @{ Name = $package.Name; Version = $package.Version; License = $license; Notices = $notices }
        }

        # The in-tree entries, nested like the directories they describe.
        $start = $html.IndexOf('id="in-tree-files"')
        $end = $html.IndexOf('id="out-of-tree-dependencies"')
        if ($start -lt 0 -or $end -lt $start) { throw "$htmlPath has no in-tree section." }
        $tokens = [regex]::Matches($html.Substring($start, $end - $start),
            '<div\b[^>]*>|</div>|<b>File/Directory:</b>\s*<code>([^<]*)</code>|<b>License:</b>([^<]*)</p>|<b>Copyright:</b>([^<]*)</p>')
        $stack = New-Object System.Collections.Stack
        $files = @()
        foreach ($t in $tokens) {
            if ($t.Value.StartsWith('<div')) {
                $parent = if ($stack.Count -gt 0) { $stack.Peek().Path } else { '' }
                $node = @{ Parent = $parent; Path = $null; License = ''; Copyright = @() }
                $stack.Push($node)
                $files += $node
            } elseif ($t.Value -eq '</div>') {
                if ($stack.Count -gt 0) { [void]$stack.Pop() }
            } elseif ($stack.Count -gt 0) {
                $node = $stack.Peek()
                if ($t.Groups[1].Success) {
                    $code = Get-HtmlText $t.Groups[1].Value
                    $node.Path = if ($code -eq '.') { '' } elseif ($node.Parent) { "$($node.Parent)/$code" } else { $code }
                } elseif ($t.Groups[2].Success) {
                    $node.License = Get-HtmlText $t.Groups[2].Value
                } else {
                    $node.Copyright += Get-HtmlText $t.Groups[3].Value
                }
            }
        }
        $files = @($files | Where-Object { $null -ne $_.Path })
        if ($files.Count -eq 0) { throw "${htmlPath}: no in-tree files found." }

        @{
            Files = $files
            Crates = $crates
            LicenseDir = Join-Path $sysroot 'share\doc\rust\licenses'
        }
    }

    # The section of the file for the standard library of these toolchains.
    function Get-StdSection {
        param([string[]]$Toolchains)
        $files = [ordered]@{}
        $crates = @{}
        $licenseDirs = @()
        foreach ($toolchain in $Toolchains) {
            $notices = Get-StdNotices $toolchain
            foreach ($f in $notices.Files) {
                $files["$($f.Path)|$($f.License)|$($f.Copyright -join '; ')"] = $f
            }
            foreach ($c in $notices.Crates) {
                $crates["$($c.Name) $($c.Version)"] = $c
            }
            $licenseDirs += $notices.LicenseDir
        }
        $crateList = @($crates.Values | Sort-Object { $_.Name }, { [version](($_.Version -split '[-+]')[0]) })

        $rule = '-' * 80
        $out = New-Object System.Collections.Generic.List[string]
        $out.Add('=' * 80)
        $out.Add('THE RUST STANDARD LIBRARY')
        $out.Add('')
        $out.Add('Every Rust program contains parts of the Rust standard library: core, alloc,')
        $out.Add('std and the crates they are built from. These notices come from the')
        $out.Add('COPYRIGHT-library.html of the Rust toolchains Dodavo is built with. Source')
        $out.Add('code: https://github.com/rust-lang/rust, and crates.io for the crates.')
        $out.Add('')
        $out.Add('Files of the Rust project:')
        foreach ($f in $files.Values) {
            $path = if ($f.Path) { $f.Path } else { 'All files not listed below' }
            $out.Add("- ${path}: $($f.License). Copyright: $($f.Copyright -join '; ')")
        }
        $out.Add('')
        $out.Add('Crates:')
        foreach ($c in $crateList) {
            $out.Add("- $($c.Name) $($c.Version): https://crates.io/crates/$($c.Name) ($($c.License))")
        }
        $out.Add('')
        $out.Add('Where a license offers a choice, Dodavo uses the MIT license.')

        # The license texts of the files, without the template's copyright
        # line: the holders are listed above.
        $ids = @($files.Values | ForEach-Object { Select-Licenses $_.License } | Select-Object -Unique)
        foreach ($id in $ids) {
            $path = $licenseDirs | ForEach-Object { Join-Path $_ "$id.txt" } | Where-Object { Test-Path $_ } | Select-Object -First 1
            if (-not $path) { throw "No license text for $id in the toolchains' share\doc\rust\licenses." }
            $text = (([IO.File]::ReadAllText($path) -replace "`r`n", "`n" -split "`n" |
                Where-Object { $_ -notmatch '^Copyright.*<' }) -join "`n") -replace "`n{3,}", "`n`n"
            $out.Add($rule)
            $out.Add("$id, for the files of the Rust project listed above")
            $out.Add('')
            $out.Add($text.Trim())
        }

        # The crates' own notices; one text that several share is shown once.
        $texts = [ordered]@{}
        foreach ($c in $crateList) {
            foreach ($n in $c.Notices) {
                if (-not $texts.Contains($n.Text)) { $texts[$n.Text] = @() }
                $texts[$n.Text] += "$($c.Name) $($c.Version) ($($n.File))"
            }
        }
        foreach ($text in $texts.Keys) {
            $out.Add($rule)
            $out.Add($texts[$text] -join ', ')
            $out.Add('')
            $out.Add($text)
        }
        ($out -join "`n") + "`n"
    }

    if (-not (Get-Command 'cargo-about' -ErrorAction SilentlyContinue)) {
        throw 'cargo-about was not found. Install it with "cargo install cargo-about --locked --features cli".'
    }
    if (-not $Win7Toolchain) {
        # The default of build.ps1's -Win7Toolchain, the one place it is pinned.
        $ast = [Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'build.ps1'), [ref]$null, [ref]$null)
        $param = $ast.ParamBlock.Parameters | Where-Object { $_.Name.VariablePath.UserPath -eq 'Win7Toolchain' }
        $Win7Toolchain = $param.DefaultValue.Value
        if (-not $Win7Toolchain) { throw 'build.ps1 has no default for -Win7Toolchain.' }
    }
    # The toolchain cargo uses here, which builds the Windows 10 programs.
    $toolchain = ((& rustup show active-toolchain | Out-String).Trim() -split '\s+')[0]
    if (-not $toolchain) { throw 'rustup show active-toolchain failed.' }

    Write-Host "Installing rust-src for $toolchain and $Win7Toolchain (if missing)"
    Invoke-Native 'rustup' @('component', 'add', 'rust-src', '--toolchain', $toolchain)
    Invoke-Native 'rustup' @('toolchain', 'install', $Win7Toolchain, '--profile', 'minimal', '--component', 'rust-src')

    $output = Join-Path $root 'res\third-party-licenses.txt'
    $temp = Join-Path $root 'target\third-party-licenses.txt'
    New-Item -ItemType Directory -Force (Split-Path $temp) | Out-Null

    Write-Host 'Collecting the licenses of the dependencies (cargo-about)'
    & cargo about generate --locked --fail -c installer\about.toml -o $temp installer\about.hbs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo about failed with exit code $LASTEXITCODE."
    }
    Write-Host "Collecting the licenses of the Rust standard library ($toolchain, $Win7Toolchain)"
    $std = Get-StdSection @($toolchain, $Win7Toolchain)

    # LF line ends, like the other files in the repository (some license
    # files have CRLF); the program converts them for the text field.
    $text = ([IO.File]::ReadAllText($temp) -replace "`r`n", "`n").TrimEnd() + "`n`n" + $std
    Remove-Item $temp
    $before = if (Test-Path $output) { [IO.File]::ReadAllText($output) } else { $null }
    if ($text -ceq $before) {
        Write-Host 'res\third-party-licenses.txt is up to date.'
    } else {
        [IO.File]::WriteAllText($output, $text, (New-Object Text.UTF8Encoding $false))
        Write-Warning 'res\third-party-licenses.txt has changed: commit it.'
    }
}
finally {
    Pop-Location
}
