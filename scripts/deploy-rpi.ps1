param(
    [string]$RpiHost,
    [string]$RpiUser = "pi",
    [int]$RpiPort = 22,
    [string]$RpiDir,
    [string]$PackagePath,
    [string]$ServiceName = "quantclaw-rust",
    [switch]$EnsureSwap,
    [int]$SwapSizeMB = 1024,
    [int]$MinFreeDiskMB = 2048
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not $RpiHost) {
    throw "You must provide -RpiHost."
}

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

if (-not $RpiDir) {
    $RpiDir = "/home/$RpiUser/quantclaw_rust_app"
}

function Require-Command {
    param([string]$Name)

    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Command not found: $Name"
    }
}

Require-Command ssh
Require-Command scp

if (-not $PackagePath) {
    $latest = Get-ChildItem -Path (Join-Path $repoRoot "dist") -Filter "quantclaw-*-aarch64-linux-gnu.tar.gz" |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if (-not $latest) {
        throw "No release package found. Run scripts/build-release-aarch64.ps1 first."
    }
    $PackagePath = $latest.FullName
}

if (-not (Test-Path $PackagePath)) {
    throw "Package does not exist: $PackagePath"
}

$sshTarget = "$RpiUser@$RpiHost"
$remotePackage = "/tmp/" + [IO.Path]::GetFileName($PackagePath)
$packageName = [IO.Path]::GetFileNameWithoutExtension([IO.Path]::GetFileNameWithoutExtension($PackagePath))
$packageExtractDir = "/tmp/$packageName"

function Invoke-Ssh {
    param([string]$Command)

    & ssh -p $RpiPort -o StrictHostKeyChecking=no -o ConnectTimeout=10 $sshTarget $Command
    if ($LASTEXITCODE -ne 0) {
        throw "SSH command failed: $Command"
    }
}

Write-Host "=== QuantClaw Windows -> Raspberry Pi deploy ==="
Write-Host "Target: $sshTarget"
Write-Host "Deploy dir: $RpiDir"
Write-Host "Service: $ServiceName"
Write-Host "Package: $PackagePath"
Write-Host ""

Write-Host "[*] Checking Raspberry Pi resources..."
$resourceScript = @"
set -e
echo "ARCH=\$(uname -m)"
echo "MEM_AVAILABLE_KB=\$(awk '/MemAvailable/ {print \$2}' /proc/meminfo)"
echo "SWAP_TOTAL_KB=\$(awk '/SwapTotal/ {print \$2}' /proc/meminfo)"
echo "ROOT_AVAIL_KB=\$(df --output=avail / | tail -n 1 | tr -d ' ')"
"@
$resourceOutput = & ssh -p $RpiPort -o StrictHostKeyChecking=no -o ConnectTimeout=10 $sshTarget $resourceScript
if ($LASTEXITCODE -ne 0) {
    throw "Failed to connect to the Raspberry Pi for preflight checks."
}

$resourceMap = @{}
foreach ($line in $resourceOutput) {
    if ($line -match "^([^=]+)=(.*)$") {
        $resourceMap[$matches[1]] = $matches[2]
    }
}

if ($resourceMap["ARCH"] -ne "aarch64") {
    throw "Target machine is not aarch64. Detected: $($resourceMap["ARCH"])"
}

$memAvailableMB = [math]::Round(([double]$resourceMap["MEM_AVAILABLE_KB"]) / 1024, 1)
$swapTotalMB = [math]::Round(([double]$resourceMap["SWAP_TOTAL_KB"]) / 1024, 1)
$rootAvailMB = [math]::Round(([double]$resourceMap["ROOT_AVAIL_KB"]) / 1024, 1)

Write-Host "    Available memory: ${memAvailableMB} MB"
Write-Host "    Total swap: ${swapTotalMB} MB"
Write-Host "    Root free disk: ${rootAvailMB} MB"

if ($EnsureSwap) {
    if ($rootAvailMB -lt $MinFreeDiskMB) {
        throw "Not enough free disk space to create a swapfile safely. Current: ${rootAvailMB} MB, required: ${MinFreeDiskMB} MB."
    }

    if ($swapTotalMB -lt $SwapSizeMB) {
        Write-Host "[*] Creating or resizing swapfile to ${SwapSizeMB} MB..."
        $swapScript = @"
set -e
if sudo swapon --show=NAME --noheadings | grep -q '^/swapfile$'; then
  sudo swapoff /swapfile || true
fi
sudo fallocate -l ${SwapSizeMB}M /swapfile 2>/dev/null || sudo dd if=/dev/zero of=/swapfile bs=1M count=${SwapSizeMB} status=progress
sudo chmod 600 /swapfile
sudo mkswap /swapfile >/dev/null
sudo swapon /swapfile
if ! grep -q '^/swapfile ' /etc/fstab; then
  echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab >/dev/null
fi
swapon --show
"@
        Invoke-Ssh $swapScript
    } else {
        Write-Host "[*] Existing swap already satisfies the requirement."
    }
}

Write-Host "[*] Uploading package..."
& scp -P $RpiPort -o StrictHostKeyChecking=no -o ConnectTimeout=10 $PackagePath "${sshTarget}:${remotePackage}"
if ($LASTEXITCODE -ne 0) {
    throw "Failed to upload package."
}

Write-Host "[*] Extracting and installing package..."
$installScript = @"
set -e
mkdir -p '$RpiDir'
rm -rf '$packageExtractDir'
mkdir -p '$packageExtractDir'
tar xzf '$remotePackage' -C /tmp
cd '$packageExtractDir'
if [ -f install.sh ]; then
  sudo chmod +x ./install.sh
  QUANTCLAW_USER='$RpiUser' QUANTCLAW_APP_DIR='$RpiDir/current' QUANTCLAW_CONFIG_DIR='$RpiDir/.quantclaw' sudo ./install.sh || true
fi
"@
Invoke-Ssh $installScript

Write-Host "[*] Ensuring runtime directory and default .env exist..."
$runtimeScript = @"
set -e
mkdir -p '$RpiDir'
mkdir -p '$RpiDir/.quantclaw/workspace'
mkdir -p '$RpiDir/releases'
rm -rf '$RpiDir/releases/$packageName'
cp -a '$packageExtractDir' '$RpiDir/releases/$packageName'
ln -sfn '$RpiDir/releases/$packageName' '$RpiDir/current'
if [ ! -f '$RpiDir/.env' ]; then
  printf '# Provider key (set one)\nOPENAI_API_KEY=\n' > '$RpiDir/.env'
  chmod 600 '$RpiDir/.env'
fi
if [ -f '$RpiDir/releases/$packageName/rpi-config.toml' ] && [ ! -f '$RpiDir/.quantclaw/config.toml' ]; then
  cp '$RpiDir/releases/$packageName/rpi-config.toml' '$RpiDir/.quantclaw/config.toml'
fi
chmod 600 '$RpiDir/.quantclaw/config.toml' 2>/dev/null || true
"@
Invoke-Ssh $runtimeScript

Write-Host "[*] Installing systemd service $ServiceName..."
$serviceScript = @"
set -e
sudo systemctl stop quantclaw 2>/dev/null || true
sudo systemctl disable quantclaw 2>/dev/null || true
if [ -f /etc/systemd/system/quantclaw.service ]; then
  sudo rm -f /etc/systemd/system/quantclaw.service
fi

if [ -f '$RpiDir/releases/$packageName/quantclaw-rust.service' ]; then
  sudo cp '$RpiDir/releases/$packageName/quantclaw-rust.service' '/etc/systemd/system/$ServiceName.service'
elif [ -f '$RpiDir/releases/$packageName/quantclaw.service' ]; then
  sudo cp '$RpiDir/releases/$packageName/quantclaw.service' '/etc/systemd/system/$ServiceName.service'
else
  cat >/tmp/$ServiceName.service <<'EOF'
[Unit]
Description=QuantClaw Rust Agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$RpiUser
SupplementaryGroups=gpio spi i2c
WorkingDirectory=$RpiDir/current
ExecStart=/usr/local/bin/quantclaw gateway --config-dir $RpiDir/.quantclaw
Restart=on-failure
RestartSec=5
EnvironmentFile=$RpiDir/.env
Environment=QUANTCLAW_CONFIG_DIR=$RpiDir/.quantclaw
Environment=RUST_LOG=info
Environment=HOME=/home/$RpiUser

[Install]
WantedBy=multi-user.target
EOF
  sudo mv /tmp/$ServiceName.service '/etc/systemd/system/$ServiceName.service'
fi

sudo sed -i \
  -e 's|^User=.*|User=$RpiUser|' \
  -e 's|^WorkingDirectory=.*|WorkingDirectory=$RpiDir/current|' \
  -e 's|^ExecStart=.*|ExecStart=/usr/local/bin/quantclaw gateway --config-dir $RpiDir/.quantclaw|' \
  -e 's|^EnvironmentFile=.*|EnvironmentFile=$RpiDir/.env|' \
  -e 's|^Environment=HOME=.*|Environment=HOME=/home/$RpiUser|' \
  '/etc/systemd/system/$ServiceName.service'
if ! grep -q '^Environment=QUANTCLAW_CONFIG_DIR=' '/etc/systemd/system/$ServiceName.service'; then
  sudo sed -i '/^Environment=RUST_LOG=.*/a Environment=QUANTCLAW_CONFIG_DIR=$RpiDir/.quantclaw' '/etc/systemd/system/$ServiceName.service'
fi

sudo systemctl daemon-reload
sudo systemctl enable --now '$ServiceName'
sudo systemctl status '$ServiceName' --no-pager || true
"@
Invoke-Ssh $serviceScript

Write-Host ""
Write-Host "=== Deploy complete ==="
Write-Host "Gateway URL: http://$RpiHost`:42617"
Write-Host "Health check: http://$RpiHost`:42617/health"
Write-Host "Logs: ssh -p $RpiPort $sshTarget 'journalctl -u $ServiceName -f'"
