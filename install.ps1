<#
.SYNOPSIS
ZeroClaw installer for Windows (PowerShell sibling of install.sh).

.DESCRIPTION
Reads the SAME source of truth as install.sh — the [features] table in
Cargo.toml and the apps under apps/*/Cargo.toml — at runtime. Nothing about
the feature list is hardcoded here; adding a feature or app upstream surfaces
in both installers automatically. This is the project's DRY-installer
contract: shared data (Cargo.toml), per-platform thin frontends.

MVP scope: source builds (-Source) with preset/feature/app selection,
-ListFeatures, and -DryRun. Pre-built binary download is not implemented yet
(asset naming for Windows releases TBD) — use -Source.

.EXAMPLE
./install.ps1 -ListFeatures

.EXAMPLE
./install.ps1 -Source                                  # full (default features)

.EXAMPLE
./install.ps1 -Source -Preset minimal -Features agent-runtime,channel-discord

.EXAMPLE
./install.ps1 -Source -Apps none -DryRun
#>
[CmdletBinding()]
param(
    # Build from source (the only implemented path in this MVP).
    [switch]$Source,
    # Download a pre-built binary. NOT IMPLEMENTED YET.
    [switch]$Prebuilt,
    # Named preset: 'minimal' (kernel only) or 'full' (default features).
    [ValidateSet('minimal', 'full')]
    [string]$Preset,
    # Comma-separated feature list (exact set; implies --no-default-features).
    [string]$Features,
    # Comma-separated apps to install (e.g. zerocode), or 'none'.
    [string]$Apps,
    # Print all available features (from Cargo.toml) and exit.
    [switch]$ListFeatures,
    # Show what would run without building or installing.
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$RepoRoot = $PSScriptRoot
$CargoToml = Join-Path $RepoRoot 'Cargo.toml'

function Fail([string]$Message) { Write-Error $Message; exit 1 }
function Info([string]$Message) { Write-Host ">> $Message" }

if (-not (Test-Path $CargoToml)) { Fail "Cargo.toml not found at $CargoToml (run from the repo root)" }

# ── Cargo.toml parsing — the single source of truth (mirrors install.sh) ──

function Get-WorkspaceVersion {
    $inSection = $false
    foreach ($line in Get-Content $CargoToml) {
        if ($line -match '^\[workspace\.package\]') { $inSection = $true; continue }
        if ($line -match '^\[') { $inSection = $false }
        if ($inSection -and $line -match '^version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    return 'unknown'
}

# All feature names from the [features] table.
function Get-AllFeatures {
    $features = @()
    $inSection = $false
    foreach ($line in Get-Content $CargoToml) {
        if ($line -match '^\[features\]') { $inSection = $true; continue }
        if ($line -match '^\[') { $inSection = $false }
        if ($inSection -and $line -match '^([a-z][a-z0-9_-]*)\s*=') { $features += $Matches[1] }
    }
    return $features
}

# Members of one feature's array (spans multi-line array literals).
function Get-FeatureMembers([string]$Feature) {
    $members = @()
    $collecting = $false
    foreach ($line in Get-Content $CargoToml) {
        if (-not $collecting -and $line -match "^$([regex]::Escape($Feature))\s*=\s*\[") { $collecting = $true }
        if ($collecting) {
            foreach ($m in [regex]::Matches($line, '"([^"]+)"')) { $members += $m.Groups[1].Value }
            if ($line -match '\]') { break }
        }
    }
    return $members
}

# Aggregate/meta features — same exclusion set as install.sh ($NON_ROW_FEATURES).
$NonRowFeatures = @('default', 'default-channels', 'channels-full', 'ci-all', 'fantoccini', 'landlock', 'metrics', 'embedded-web')

function Expand-DefaultFeatures {
    $leaves = New-Object System.Collections.Generic.HashSet[string]
    $queue = [System.Collections.Generic.Queue[string]]::new()
    foreach ($f in (Get-FeatureMembers 'default')) { $queue.Enqueue($f) }
    while ($queue.Count -gt 0) {
        $f = $queue.Dequeue()
        if ($f -like 'dep:*' -or $f -like '*/*') { continue }
        if ($NonRowFeatures -contains $f) {
            foreach ($m in (Get-FeatureMembers $f)) { $queue.Enqueue($m) }
        }
        else { [void]$leaves.Add($f) }
    }
    return @($leaves)
}

# Apps discovered from apps/*/Cargo.toml — mirrors install.sh discover_apps.
function Get-Apps {
    $apps = @()
    foreach ($dir in Get-ChildItem -Path (Join-Path $RepoRoot 'apps') -Directory -ErrorAction SilentlyContinue) {
        $toml = Join-Path $dir.FullName 'Cargo.toml'
        if (-not (Test-Path $toml)) { continue }
        foreach ($line in Get-Content $toml) {
            if ($line -match '^name\s*=\s*"([^"]+)"') { $apps += $Matches[1]; break }
        }
    }
    return $apps
}

function Get-AppDir([string]$App) {
    foreach ($dir in Get-ChildItem -Path (Join-Path $RepoRoot 'apps') -Directory -ErrorAction SilentlyContinue) {
        $toml = Join-Path $dir.FullName 'Cargo.toml'
        if (-not (Test-Path $toml)) { continue }
        foreach ($line in Get-Content $toml) {
            if ($line -match '^name\s*=\s*"([^"]+)"' ) {
                if ($Matches[1] -eq $App) { return $dir.FullName }
                break
            }
        }
    }
    return $null
}

# ── -ListFeatures ──────────────────────────────────────────────────

if ($ListFeatures) {
    $version = Get-WorkspaceVersion
    Write-Host ""
    Write-Host "ZeroClaw v$version — available build features" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "  Default (included unless -Preset minimal):"
    Write-Host "    $((Get-FeatureMembers 'default') -join ',')"
    Write-Host ""
    $groups = [ordered]@{ 'Channels' = @(); 'Observability' = @(); 'Other' = @() }
    foreach ($feat in (Get-AllFeatures)) {
        if ($NonRowFeatures -contains $feat) { continue }
        if ($feat -like 'channel-*') { $groups['Channels'] += $feat }
        elseif ($feat -like 'observability-*') { $groups['Observability'] += $feat }
        else { $groups['Other'] += $feat }
    }
    foreach ($name in $groups.Keys) {
        if ($groups[$name].Count -gt 0) {
            Write-Host "  ${name}:"
            Write-Host "    $($groups[$name] -join ', ')"
            Write-Host ""
        }
    }
    Write-Host "  Apps (apps/*/):"
    Write-Host "    $((Get-Apps) -join ', ')"
    exit 0
}

# ── Mode selection ─────────────────────────────────────────────────

if ($Prebuilt) {
    Fail "Pre-built install is not implemented in install.ps1 yet — use -Source (or see setup.bat)."
}
if (-not $Source) {
    Fail "Specify -Source to build from source, or -ListFeatures to inspect features. (-Prebuilt is not implemented yet.)"
}

# ── Compose cargo flags — mirrors install.sh semantics ─────────────
#   (no preset/features)        -> default features
#   -Preset minimal             -> --no-default-features [+ -Features extras]
#   -Features X,Y (no preset)   -> exact set: --no-default-features --features X,Y

$cargoFlags = @()
if ($Preset -eq 'minimal') {
    $cargoFlags += '--no-default-features'
    if ($Features) { $cargoFlags += @('--features', $Features) }
}
elseif ($Features) {
    $cargoFlags += @('--no-default-features', '--features', $Features)
}

# Apps: default to zerocode (the TUI), 'none' skips all — mirrors install.sh.
$appList = @()
if ($Apps -eq 'none') { $appList = @() }
elseif ($Apps) { $appList = $Apps -split ',' | ForEach-Object { $_.Trim() } }
else { $appList = @('zerocode') | Where-Object { (Get-Apps) -contains $_ } }

$knownApps = Get-Apps
foreach ($app in $appList) {
    if ($knownApps -notcontains $app) { Fail "Unknown app '$app'. Installable apps: $($knownApps -join ', ')" }
}

# ── Build + install ────────────────────────────────────────────────

Info "ZeroClaw v$(Get-WorkspaceVersion) source install"
Info "cargo flags: $(if ($cargoFlags) { $cargoFlags -join ' ' } else { '(default features)' })"
Info "apps: $(if ($appList) { $appList -join ', ' } else { '(none)' })"

$mainArgs = @('install', '--path', '.', '--locked', '--force') + $cargoFlags
if ($DryRun) {
    Info "[dry-run] Would run: cargo $($mainArgs -join ' ')"
}
else {
    Push-Location $RepoRoot
    try { & cargo @mainArgs; if ($LASTEXITCODE -ne 0) { Fail "cargo install failed" } }
    finally { Pop-Location }
}

foreach ($app in $appList) {
    $dir = Get-AppDir $app
    if (-not $dir) { Fail "could not locate app directory for '$app'" }
    $appArgs = @('install', '--path', $dir, '--locked', '--force')
    if ($DryRun) { Info "[dry-run] Would run: cargo $($appArgs -join ' ')" }
    else { & cargo @appArgs; if ($LASTEXITCODE -ne 0) { Fail "cargo install for '$app' failed" } }
}

Info "done. Binaries are in `$env:USERPROFILE\.cargo\bin (ensure it is on PATH)."
