#!/usr/bin/env pwsh
<#
.SYNOPSIS
  Windows bootstrap entrypoint for ZeroClaw.

.DESCRIPTION
  Provides the core bootstrap flow for native Windows:
  - optional Rust toolchain install
  - optional prebuilt binary install
  - source build + cargo install fallback
  - optional onboarding

  This script is intentionally scoped to Windows and does not replace
  Docker/bootstrap.sh flows for Linux/macOS.
#>

[CmdletBinding()]
param(
    [switch]$InstallRust,
    [switch]$PreferPrebuilt,
    [switch]$PrebuiltOnly,
    [switch]$ForceSourceBuild,
    [switch]$SkipBuild,
    [switch]$SkipInstall,
    [switch]$Onboard,
    [switch]$InteractiveOnboard,
    [string]$ApiKey = "",
    [string]$Provider = "openrouter",
    [string]$Model = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Write-Info {
    param([string]$Message)
    Write-Host "==> $Message"
}

function Write-Warn {
    param([string]$Message)
    Write-Warning $Message
}

function Ensure-RustToolchain {
    if (Get-Command cargo -ErrorAction SilentlyContinue) {
        Write-Info "cargo is already available."
        return
    }

    if (-not $InstallRust) {
        throw "cargo is not installed. Re-run with -InstallRust or install Rust manually from https://rustup.rs/"
    }

    Write-Info "Installing Rust toolchain via rustup-init.exe"
    $tempDir = Join-Path $env:TEMP "zeroclaw-bootstrap-rustup"
    New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
    $rustupExe = Join-Path $tempDir "rustup-init.exe"
    Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $rustupExe
    & $rustupExe -y --profile minimal --default-toolchain stable

    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if (-not ($env:Path -split ";" | Where-Object { $_ -eq $cargoBin })) {
        $env:Path = "$cargoBin;$env:Path"
    }

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "Rust installation did not expose cargo in PATH. Open a new shell and retry."
    }
}

function Install-PrebuiltBinary {
    $target = "x86_64-pc-windows-msvc"
    $url = "https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-$target.zip"
    $tempDir = Join-Path $env:TEMP ("zeroclaw-prebuilt-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
    $archivePath = Join-Path $tempDir "zeroclaw-$target.zip"
    $extractDir = Join-Path $tempDir "extract"
    New-Item -ItemType Directory -Path $extractDir -Force | Out-Null

    try {
        Write-Info "Downloading prebuilt binary: $url"
        Invoke-WebRequest -Uri $url -OutFile $archivePath
        Expand-Archive -Path $archivePath -DestinationPath $extractDir -Force

        $binary = Get-ChildItem -Path $extractDir -Recurse -Filter "zeroclaw.exe" | Select-Object -First 1
        if (-not $binary) {
            throw "Downloaded archive does not contain zeroclaw.exe"
        }

        $installDir = Join-Path $env:USERPROFILE ".cargo\bin"
        New-Item -ItemType Directory -Path $installDir -Force | Out-Null
        $dest = Join-Path $installDir "zeroclaw.exe"
        Copy-Item -Path $binary.FullName -Destination $dest -Force
        Write-Info "Installed prebuilt binary to $dest"
        return $true
    }
    catch {
        Write-Warn "Prebuilt install failed: $($_.Exception.Message)"
        return $false
    }
    finally {
        Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-SourceBuildInstall {
    param(
        [string]$RepoRoot
    )

    if (-not $SkipBuild) {
        Write-Info "Running cargo build --release --locked"
        & cargo build --release --locked
    }
    else {
        Write-Info "Skipping build (-SkipBuild)"
    }

    if (-not $SkipInstall) {
        Write-Info "Running cargo install --path . --force --locked"
        & cargo install --path . --force --locked
    }
    else {
        Write-Info "Skipping cargo install (-SkipInstall)"
    }
}

function Resolve-ZeroClawBinary {
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin\zeroclaw.exe"
    if (Test-Path $cargoBin) {
        return $cargoBin
    }

    $fromPath = Get-Command zeroclaw -ErrorAction SilentlyContinue
    if ($fromPath) {
        return $fromPath.Source
    }

    return $null
}

function Run-Onboarding {
    param(
        [string]$BinaryPath
    )

    if (-not $BinaryPath) {
        throw "Onboarding requested but zeroclaw binary is not available."
    }

    if ($InteractiveOnboard) {
        Write-Info "Running interactive onboarding"
        & $BinaryPath onboard --interactive
        return
    }

    $resolvedApiKey = $ApiKey
    if (-not $resolvedApiKey) {
        $resolvedApiKey = $env:ZEROCLAW_API_KEY
    }

    if (-not $resolvedApiKey) {
        throw "Onboarding requires -ApiKey (or ZEROCLAW_API_KEY) unless using -InteractiveOnboard."
    }

    $cmd = @("onboard", "--api-key", $resolvedApiKey, "--provider", $Provider)
    if ($Model) {
        $cmd += @("--model", $Model)
    }
    Write-Info "Running onboarding with provider '$Provider'"
    & $BinaryPath @cmd
}

if ($IsLinux -or $IsMacOS) {
    throw "bootstrap.ps1 is for Windows. Use ./bootstrap.sh on Linux/macOS."
}

if ($PrebuiltOnly -and $ForceSourceBuild) {
    throw "-PrebuiltOnly cannot be combined with -ForceSourceBuild."
}

if ($InteractiveOnboard) {
    $Onboard = $true
}

$repoRoot = Split-Path -Parent $PSCommandPath
Set-Location $repoRoot

Ensure-RustToolchain

$didPrebuiltInstall = $false
if (($PreferPrebuilt -or $PrebuiltOnly) -and -not $ForceSourceBuild) {
    $didPrebuiltInstall = Install-PrebuiltBinary
    if ($PrebuiltOnly -and -not $didPrebuiltInstall) {
        throw "Prebuilt-only mode requested but prebuilt install failed."
    }
}

if (-not $didPrebuiltInstall -and -not $PrebuiltOnly) {
    Invoke-SourceBuildInstall -RepoRoot $repoRoot
}

$zeroclawBin = Resolve-ZeroClawBinary
if (-not $zeroclawBin) {
    throw "ZeroClaw binary was not found after bootstrap."
}

Write-Info "ZeroClaw bootstrap completed."
Write-Info "Binary: $zeroclawBin"

if ($Onboard) {
    Run-Onboarding -BinaryPath $zeroclawBin
}
