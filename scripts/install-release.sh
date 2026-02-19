#!/bin/bash
set -e

echo "🦀 ZeroClaw Installer (Custom Build for Linux)"
echo "=============================================="

# 1. Install Basic Dependencies
echo "📦 [1/4] Checking system dependencies..."
if command -v apt-get &> /dev/null; then
    sudo apt-get update
    sudo apt-get install -y curl build-essential
elif command -v dnf &> /dev/null; then
    sudo dnf groupinstall -y "Development Tools"
    sudo dnf install -y curl
elif command -v pacman &> /dev/null; then
    sudo pacman -S --noconfirm base-devel curl
fi

# 2. Install Rust (Requested)
echo "🦀 [2/4] Checking Rust installation..."
if ! command -v rustc &> /dev/null; then
    echo "   Rust not found. Installing Rust (via rustup)..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

    # Source the environment for the current session
    source "$HOME/.cargo/env"

    echo "   ✅ Rust installed successfully."
else
    echo "   ✅ Rust is already installed."
fi

# 3. Download & Install ZeroClaw Binary
echo "⬇️  [3/4] Downloading latest ZeroClaw binary..."

# Fetch the latest release data from GitHub API
if command -v jq &> /dev/null; then
    DOWNLOAD_URL=$(curl -s https://api.github.com/repos/0xshitcode/zeroclaw/releases/latest | jq -r '.assets[] | select(.name=="zeroclaw") | .browser_download_url')
else
    # Fallback using grep if jq is not installed
    DOWNLOAD_URL=$(curl -s https://api.github.com/repos/0xshitcode/zeroclaw/releases/latest | grep "browser_download_url" |cut -d '"' -f 4 | grep "/zeroclaw$")
fi

if [ -z "$DOWNLOAD_URL" ]; then
    echo "❌ Error: Could not find the binary in the latest release."
    echo "   Please make sure the automated build has finished successfully in GitHub Actions."
    exit 1
fi

echo "   Fetching: $DOWNLOAD_URL"
curl -L -o zeroclaw_temp "$DOWNLOAD_URL"
chmod +x zeroclaw_temp

echo "   Installing to /usr/local/bin/zeroclaw..."
sudo mv zeroclaw_temp /usr/local/bin/zeroclaw

# 4. Onboard
echo "🚀 [4/4] Starting Setup..."
echo "   Running 'zeroclaw onboard'..."
echo ""
zeroclaw onboard
