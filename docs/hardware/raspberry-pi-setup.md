# Raspberry Pi Setup

This guide covers deploying ZeroClaw on Raspberry Pi devices.

## Supported Models

- Raspberry Pi 5 (8GB, 4GB)
- Raspberry Pi 4 (4GB, 2GB)
- Raspberry Pi 3 (1GB)
- Raspberry Pi Zero 2 W

## Installation Methods

### Method 1: Pre-built Binary (Recommended)

Download the pre-built binary for your architecture:

```bash
# For 64-bit Raspberry Pi OS (aarch64)
curl -Lo zeroclaw.tar.gz https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-unknown-linux-gnu.tar.gz

# For 32-bit Raspberry Pi OS (armv7l)
curl -Lo zeroclaw.tar.gz https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-armv7-unknown-linux-gnueabihf.tar.gz

# Extract and install
tar -xzf zeroclaw.tar.gz
sudo mv zeroclaw /usr/local/bin/
zeroclaw --version
```

### Method 2: Build from Source

Building on Raspberry Pi requires additional memory. For Pi 4/5:

```bash
# Add swap space to prevent OOM during builds
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile

# Clone and build
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo build --profile release-fast

# Install
sudo cp target/release-fast/zeroclaw /usr/local/bin/
```

### Method 3: Cross-compile

Build on a more powerful machine and copy to Pi:

```bash
# On x86_64 Linux/macOS
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu

# Copy to Pi
scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@<pi-ip>:/usr/local/bin/
```

## Post-Installation

Initialize ZeroClaw:

```bash
zeroclaw init
```

Run as a service:

```bash
zeroclaw service install
zeroclaw service start
```

## GPIO Support

To enable GPIO tools, build with the `peripheral-rpi` feature:

```bash
cargo build --profile release-fast --features peripheral-rpi
```

## Troubleshooting

### Out of Memory during build

Increase swap space:
```bash
sudo fallocate -l 4G /swapfile
sudo swapon /swapfile
```

### Slow builds

Use a USB SSD instead of SD card for faster I/O.

### Architecture mismatch

Check your architecture with `uname -m`:
- `aarch64` = 64-bit OS
- `armv7l` = 32-bit OS
