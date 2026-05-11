param(
    [string]$Version,
    [string]$ImageTag = "quantclaw-builder:aarch64",
    [string]$DockerfilePath = "Dockerfile.build-aarch64",
    [switch]$KeepImage
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

function Require-Command {
    param([string]$Name)

    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Command not found: $Name"
    }
}

function Write-Utf8NoBomLfFile {
    param(
        [string]$Path,
        [string]$Content
    )

    $normalized = $Content -replace "`r`n", "`n"
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $normalized, $utf8NoBom)
}

Require-Command docker
Require-Command tar

try {
    docker info | Out-Null
} catch {
    throw "Docker Desktop is not running. Start it and retry."
}

if (-not $Version) {
    $cargoToml = Get-Content -Raw -Path (Join-Path $repoRoot "Cargo.toml")
    $versionMatch = [regex]::Match($cargoToml, '(?m)^\s*version\s*=\s*"([^"]+)"')
    if (-not $versionMatch.Success) {
        throw "Failed to parse version from Cargo.toml."
    }
    $Version = $versionMatch.Groups[1].Value
}

$packageName = "quantclaw-$Version-aarch64-linux-gnu"
$distRoot = Join-Path $repoRoot "dist"
$packageDir = Join-Path $distRoot $packageName
$tarballPath = Join-Path $distRoot "$packageName.tar.gz"

$rootFirmwareDir = Join-Path $repoRoot "firmware"
if (-not (Test-Path $rootFirmwareDir -PathType Container)) {
    throw "Missing required directory: firmware/"
}

$firmwarePointers = @(
    "crates\quantclaw-hardware\firmware",
    "crates\quantclaw-runtime\firmware",
    "crates\quantclaw-runtime\src\firmware"
)
foreach ($relativePath in $firmwarePointers) {
    $pointerPath = Join-Path $repoRoot $relativePath
    if (Test-Path $pointerPath -PathType Leaf) {
        $target = (Get-Content -Raw -Path $pointerPath).Trim()
        Write-Host "[!] Detected pointer file: $relativePath -> $target"
        Write-Host "    Dockerfile will normalize it to symlink during build."
    }
}

Write-Host "=== QuantClaw aarch64 Windows package build ==="
Write-Host "Version: $Version"
Write-Host "Package: $packageName"
Write-Host ""

if (Test-Path $packageDir) {
    Microsoft.PowerShell.Management\Remove-Item -LiteralPath $packageDir -Recurse -Force
}
New-Item -ItemType Directory -Path $packageDir | Out-Null

Write-Host "[*] Building aarch64 binary and web assets with Docker..."
docker build -f $DockerfilePath -t $ImageTag . --progress=plain

$containerName = "extract-aarch64-" + [guid]::NewGuid().ToString("N")

try {
    docker create --name $containerName $ImageTag | Out-Null

    Write-Host "[*] Extracting build artifacts..."
    $webDistDir = Join-Path $packageDir "web\dist"
    New-Item -ItemType Directory -Force -Path $webDistDir | Out-Null
    docker cp "${containerName}:/quantclaw" (Join-Path $packageDir "quantclaw") | Out-Null
    docker cp "${containerName}:/web-dist/." $webDistDir | Out-Null

    if (-not (Test-Path (Join-Path $packageDir "quantclaw"))) {
        throw "Failed to extract quantclaw binary."
    }

    if (-not (Test-Path $webDistDir)) {
        throw "Failed to extract web/dist."
    }
} finally {
    try {
        docker rm -f $containerName 2>$null | Out-Null
    } catch {
        # Best-effort cleanup
    }
    if (-not $KeepImage) {
        try {
            docker rmi $ImageTag 2>$null | Out-Null
        } catch {
            # Best-effort cleanup
        }
    }
}

Microsoft.PowerShell.Management\Copy-Item -LiteralPath (Join-Path $repoRoot "scripts\quantclaw-rust.service") -Destination (Join-Path $packageDir "quantclaw-rust.service")
Microsoft.PowerShell.Management\Copy-Item -LiteralPath (Join-Path $repoRoot "scripts\rpi-config.toml") -Destination (Join-Path $packageDir "rpi-config.toml")
if (Test-Path (Join-Path $repoRoot "README.md")) {
    Microsoft.PowerShell.Management\Copy-Item -LiteralPath (Join-Path $repoRoot "README.md") -Destination (Join-Path $packageDir "README.md")
}
if (Test-Path (Join-Path $repoRoot "LICENSE-MIT")) {
    Microsoft.PowerShell.Management\Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE-MIT") -Destination (Join-Path $packageDir "LICENSE-MIT")
}

$installScript = @'
#!/usr/bin/env bash
set -e

INSTALL_DIR="/usr/local/bin"
SERVICE_DIR="/etc/systemd/system"
QUANTCLAW_USER="${QUANTCLAW_USER:-pi}"
QUANTCLAW_HOME="$(getent passwd "$QUANTCLAW_USER" | cut -d: -f6 2>/dev/null || printf '/home/%s' "$QUANTCLAW_USER")"
INSTALL_SOURCE_DIR="$(pwd)"
APP_ROOT="${QUANTCLAW_APP_ROOT:-${QUANTCLAW_HOME}/quantclaw_rust_app}"
APP_DIR="${QUANTCLAW_APP_DIR:-${APP_ROOT}/current}"
CONFIG_DIR="${QUANTCLAW_CONFIG_DIR:-${APP_ROOT}/.quantclaw}"
ENV_FILE="${APP_ROOT}/.env"

echo "=== QuantClaw 树莓派安装 ==="
echo "=== QuantClaw Raspberry Pi install ==="
echo ""

if [[ $(uname -m) != "aarch64" ]]; then
    echo "[!] Warning: current system is not aarch64"
    echo "    Detected: $(uname -m)"
    read -p "Continue anyway? (y/N) " -n 1 -r
    echo
    [[ $REPLY =~ ^[Yy]$ ]] || exit 1
fi

if [[ $EUID -ne 0 ]]; then
   echo "[!] Run this install script with sudo"
   exit 1
fi

echo "[*] Installing quantclaw..."
cp quantclaw "$INSTALL_DIR/"
chmod +x "$INSTALL_DIR/quantclaw"

echo "[*] Creating runtime directory..."
mkdir -p "$APP_ROOT"
ln -sfn "$INSTALL_SOURCE_DIR" "$APP_DIR"
if [[ ! -f "$ENV_FILE" ]]; then
    cat > "$ENV_FILE" << 'ENVEOF'
# Provider key (set one)
OPENAI_API_KEY=
ENVEOF
    chmod 600 "$ENV_FILE"
fi

echo "[*] Creating config directory..."
mkdir -p "$CONFIG_DIR"
mkdir -p "$CONFIG_DIR/workspace"

if [[ ! -f "$CONFIG_DIR/config.toml" ]]; then
    echo "[*] Creating default config..."
    if [[ -f "rpi-config.toml" ]]; then
        cp "rpi-config.toml" "$CONFIG_DIR/config.toml"
    fi
fi

if [[ -d "web/dist" ]]; then
    echo "[*] Installing web assets..."
    mkdir -p "/usr/local/share/quantclaw"
    cp -r web/dist "/usr/local/share/quantclaw/"
fi

chmod 600 "$CONFIG_DIR/config.toml" 2>/dev/null || true

if [[ -f "quantclaw-rust.service" ]]; then
    echo "[*] Installing systemd service..."
    cp quantclaw-rust.service "$SERVICE_DIR/quantclaw-rust.service"
    systemctl disable quantclaw 2>/dev/null || true
    rm -f "$SERVICE_DIR/quantclaw.service"
    sed -i \
        -e "s|^User=.*|User=${QUANTCLAW_USER}|" \
        -e "s|^WorkingDirectory=.*|WorkingDirectory=${APP_DIR}|" \
        -e "s|^ExecStart=.*|ExecStart=${INSTALL_DIR}/quantclaw gateway --config-dir ${CONFIG_DIR}|" \
        -e "s|^EnvironmentFile=.*|EnvironmentFile=${ENV_FILE}|" \
        -e "s|^Environment=HOME=.*|Environment=HOME=${QUANTCLAW_HOME}|" \
        "$SERVICE_DIR/quantclaw-rust.service" 2>/dev/null || true
    if ! grep -q '^Environment=QUANTCLAW_CONFIG_DIR=' "$SERVICE_DIR/quantclaw-rust.service"; then
        sed -i "/^Environment=RUST_LOG=.*/a Environment=QUANTCLAW_CONFIG_DIR=${CONFIG_DIR}" "$SERVICE_DIR/quantclaw-rust.service"
    fi
    systemctl daemon-reload
    systemctl enable quantclaw-rust
fi

chown -R "$QUANTCLAW_USER:$QUANTCLAW_USER" "$APP_DIR" 2>/dev/null || true
chown -R "$QUANTCLAW_USER:$QUANTCLAW_USER" "$CONFIG_DIR" 2>/dev/null || true

echo ""
echo "=== Install complete ==="
echo ""
echo "Config path: ${CONFIG_DIR}/config.toml"
echo "Gateway URL: http://$(hostname -I | awk '{print $1}'):42617"
echo ""
'@
Write-Utf8NoBomLfFile -Path (Join-Path $packageDir "install.sh") -Content $installScript

$uninstallScript = @'
#!/usr/bin/env bash
set -e

if [[ $EUID -ne 0 ]]; then
   echo "[!] Run this uninstall script with sudo"
   exit 1
fi

systemctl stop quantclaw 2>/dev/null || true
systemctl stop quantclaw-rust 2>/dev/null || true
systemctl disable quantclaw-rust 2>/dev/null || true
systemctl disable quantclaw 2>/dev/null || true
rm -f "/usr/local/bin/quantclaw"
rm -f "/etc/systemd/system/quantclaw-rust.service"
rm -f "/etc/systemd/system/quantclaw.service"
rm -rf "/usr/local/share/quantclaw"
systemctl daemon-reload

echo "Config and runtime directories are preserved. Remove ~/quantclaw_rust_app manually if needed."
'@
Write-Utf8NoBomLfFile -Path (Join-Path $packageDir "uninstall.sh") -Content $uninstallScript

$readmeTxt = @"
QuantClaw for Raspberry Pi (aarch64)
=====================================

Install steps:
1. Copy the tarball to the Raspberry Pi
2. Extract:
   tar xzf $packageName.tar.gz
   cd $packageName
3. Install:
   QUANTCLAW_USER=pi sudo ./install.sh

Default gateway:
- http://<树莓派IP>:42617
"@
Write-Utf8NoBomLfFile -Path (Join-Path $packageDir "README.txt") -Content $readmeTxt

if (Test-Path $tarballPath) {
    Microsoft.PowerShell.Management\Remove-Item -LiteralPath $tarballPath -Force
}

Write-Host "[*] Creating tarball..."
tar -czf $tarballPath -C $distRoot $packageName

Write-Host ""
Write-Host "=== Build complete ==="
Write-Host "Artifact: $tarballPath"
