#Requires -Version 7
<#
.SYNOPSIS
Bumps the project version across VERSION, engine/Cargo.toml, apps/desktop/Cargo.toml, and both Cargo.lock files.

.DESCRIPTION
    pwsh scripts/bump-version.ps1 [patch | minor | major | X.Y.Z] [-DryRun]

.PARAMETER Bump
Increment kind (patch, minor, major) or an explicit X.Y.Z semantic version. Defaults to patch.

.PARAMETER DryRun
Print the computed version and files that would be updated without modifying anything.
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$Bump = 'patch',
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$VersionFile = Join-Path $Root 'VERSION'
$EngineToml = Join-Path $Root 'engine\Cargo.toml'
$DesktopToml = Join-Path $Root 'apps\desktop\Cargo.toml'

if (-not (Test-Path $VersionFile)) {
    throw "VERSION file not found at $VersionFile"
}

$Current = (Get-Content -LiteralPath $VersionFile -Raw).Trim()
if ($Current -notmatch '^(\d+)\.(\d+)\.(\d+)$') {
    throw "Invalid semver in VERSION: '$Current' (expected X.Y.Z)"
}
[int]$Major = $Matches[1]
[int]$Minor = $Matches[2]
[int]$Patch = $Matches[3]

switch -Regex ($Bump.Trim()) {
    '^patch$' { $Next = "$Major.$Minor.$($Patch + 1)" }
    '^minor$' { $Next = "$Major.$($Minor + 1).0" }
    '^major$' { $Next = "$($Major + 1).0.0" }
    '^\d+\.\d+\.\d+$' { $Next = $Bump.Trim() }
    default { throw "Invalid bump argument '$Bump'. Use patch, minor, major, or X.Y.Z." }
}

if ($DryRun) {
    Write-Host "[dry-run] Version: $Current -> $Next"
    Write-Host "[dry-run] Would update: VERSION, engine/Cargo.toml, apps/desktop/Cargo.toml, engine/Cargo.lock, apps/desktop/Cargo.lock"
    exit 0
}

$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)

# 1. VERSION
[System.IO.File]::WriteAllText($VersionFile, "$Next`n", $Utf8NoBom)

# 2. Cargo.toml [workspace.package] version
function Update-WorkspaceVersion([string]$TomlPath, [string]$NewVersion) {
    $raw = [System.IO.File]::ReadAllText($TomlPath, $Utf8NoBom)
    $updated = [regex]::Replace(
        $raw,
        '(?ms)(^\[workspace\.package\]\s*(?:\r?\n|\r)(?:(?!^\[)[^\r\n]*(?:\r?\n|\r))*?^version\s*=\s*")[^"]+(")',
        ('${1}' + $NewVersion + '${2}'),
        1
    )
    if ($updated -eq $raw -and $raw -notmatch "(?m)^version\s*=\s*`"$([regex]::Escape($NewVersion))`"") {
        throw "Could not update [workspace.package] version in $TomlPath"
    }
    [System.IO.File]::WriteAllText($TomlPath, $updated, $Utf8NoBom)
}

Update-WorkspaceVersion -TomlPath $EngineToml -NewVersion $Next
Update-WorkspaceVersion -TomlPath $DesktopToml -NewVersion $Next

# 3. Sync Cargo.lock workspace entries offline
$Cargo = Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $Cargo -and $env:USERPROFILE) {
    $Candidate = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (Test-Path $Candidate) { $Cargo = $Candidate }
}
else {
    $Cargo = if ($Cargo) { $Cargo.Source } else { $null }
}

if ($Cargo) {
    foreach ($Dir in (Join-Path $Root 'engine'), (Join-Path $Root 'apps\desktop')) {
        Push-Location $Dir
        try {
            & $Cargo update --workspace --offline | Out-Null
            if ($LASTEXITCODE -ne 0) {
                throw "cargo update --workspace --offline failed in $Dir (exit code $LASTEXITCODE)"
            }
        }
        finally {
            Pop-Location
        }
    }
}
else {
    Write-Warning 'cargo not found; skipped Cargo.lock workspace sync.'
}

Write-Host "Bumped version: $Current -> $Next"
Write-Host "Updated: VERSION, engine/Cargo.toml, apps/desktop/Cargo.toml, engine/Cargo.lock, apps/desktop/Cargo.lock"
