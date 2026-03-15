#!/bin/bash
# ZeroClaw Bridge - User-level systemd service installer
# Usage: ./install-bridge.sh
set -e

echo "Installing zeroclaw-bridge as user-level systemd service..."

# Create config directory
mkdir -p ~/.zeroclaw
mkdir -p ~/.config/systemd/user

# Copy config template if not exists
if [ ! -f ~/.zeroclaw/bridge.toml ]; then
  if [ -f crates/zeroclaw-bridge/bridge.toml.example ]; then
    cp crates/zeroclaw-bridge/bridge.toml.example ~/.zeroclaw/bridge.toml
    echo "✓ Config template copied to ~/.zeroclaw/bridge.toml"
  else
    echo "⚠ Warning: bridge.toml.example not found, skipping config copy"
  fi
fi

# Install binary to ~/.cargo/bin (assumes cargo install or manual copy)
if [ -f target/release/zeroclaw-bridge ]; then
  mkdir -p ~/.cargo/bin
  cp target/release/zeroclaw-bridge ~/.cargo/bin/
  chmod +x ~/.cargo/bin/zeroclaw-bridge
  echo "✓ Binary installed to ~/.cargo/bin/zeroclaw-bridge"
else
  echo "⚠ Warning: target/release/zeroclaw-bridge not found"
  echo "  Run 'cargo build --release -p zeroclaw-bridge' first"
fi

# Install systemd service
cp scripts/zeroclaw-bridge.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable zeroclaw-bridge

echo ""
echo "✓ Installation complete!"
echo ""
echo "Next steps:"
echo "  1. Edit ~/.zeroclaw/bridge.toml with your MQTT/WebSocket settings"
echo "  2. Start service: systemctl --user start zeroclaw-bridge"
echo "  3. Check status: systemctl --user status zeroclaw-bridge"
