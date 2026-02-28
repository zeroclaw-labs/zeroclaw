# Building ZeroClaw on Raspberry Pi Zero W

Complete guide to compile ZeroClaw on Raspberry Pi Zero W (512MB RAM, ARMv6).

Last verified: **February 28, 2026**.

## Overview

The Raspberry Pi Zero W is a constrained device with only **512MB of RAM**. Compiling Rust on this device requires special considerations:

| Requirement | Minimum | Recommended |
|-------------|---------|-------------|
| RAM | 512MB | 512MB + 2GB swap |
| Free disk | 4GB | 6GB+ |
| OS | Raspberry Pi OS (32-bit) | Raspberry Pi OS Lite (32-bit) |
| Architecture | armv6l | armv6l |

**Important:** This guide assumes you are building **natively on the Pi Zero W**, not cross-compiling from a more powerful machine.

## Target Abi: gnueabihf vs musleabihf

When building for Raspberry Pi Zero W, you have two target ABI choices:

| ABI | Full Target | Description | Binary Size | Static Linking | Recommended |
|-----|-------------|-------------|-------------|----------------|-------------|
| **musleabihf** | `armv6l-unknown-linux-musleabihf` | Uses musl libc | Smaller | Yes (fully static) | **Yes** |
| gnueabihf | `armv6l-unknown-linux-gnueabihf` | Uses glibc | Larger | Partial | No |

**Why musleabihf is preferred:**

1. **Smaller binary size** — musl produces more compact binaries, critical for embedded devices
2. **Fully static linking** — No runtime dependency on system libc versions; binary works across different Raspberry Pi OS versions
3. **Better security** — Smaller attack surface with musl's minimal libc implementation
4. **Portability** — Static binary runs on any ARMv6 Linux distribution without compatibility concerns

**Trade-offs:**
- musleabihf builds may take slightly longer to compile
- Some niche dependencies may not support musl (ZeroClaw's dependencies are musl-compatible)

## Option A: Native Compilation

### Step 1: Prepare System

First, ensure your system is up to date:

```bash
sudo apt update
sudo apt upgrade -y
```

### Step 2: Add Swap Space (Critical)

Due to limited RAM (512MB), **adding swap is mandatory** for successful compilation:

```bash
# Create 2GB swap file
sudo fallocate -l 2G /swapfile

# Set proper permissions
sudo chmod 600 /swapfile

# Format as swap
sudo mkswap /swapfile

# Enable swap
sudo swapon /swapfile

# Verify swap is active
free -h
```

To make swap persistent across reboots:

```bash
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

### Step 3: Install Rust Toolchain

Install Rust via rustup:

```bash
# Install rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Source the environment
source $HOME/.cargo/env

# Verify installation
rustc --version
cargo --version
```

### Step 4: Install Build Dependencies

Install required system packages:

```bash
sudo apt install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    libsqlite3-dev \
    git \
    curl
```

### Step 5: Clone ZeroClaw Repository

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
```

Or if you already have the repository:

```bash
cd /path/to/zeroclaw
git fetch --all
git checkout main
git pull
```

### Step 6: Configure Build for Low Memory

ZeroClaw's `Cargo.toml` is already configured for low-memory devices (`codegen-units = 1` in release profile). For additional safety on Pi Zero W:

```bash
# Set CARGO_BUILD_JOBS=1 to prevent memory exhaustion
export CARGO_BUILD_JOBS=1
```

### Step 7: Choose Target ABI and Build ZeroClaw

This step will take **30-60 minutes** depending on your storage speed and chosen target.

**For native build, the default target is gnueabihf (matches your system):**

```bash
# Build with default target (gnueabihf)
cargo build --release

# Alternative: Build with specific features only (smaller binary)
cargo build --release --no-default-features --features "wasm-tools"
```

**For musleabihf (smaller, static binary — requires musl tools):**

```bash
# Install musl development tools
sudo apt install -y musl-tools musl-dev

# Add musl target
rustup target add armv6l-unknown-linux-musleabihf

# Build for musleabihf (smaller, static binary)
cargo build --release --target armv6l-unknown-linux-musleabihf
```

**Note:** If the build fails with "out of memory" errors, you may need to increase swap size to 4GB:

```bash
sudo swapoff /swapfile
sudo rm /swapfile
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
```

Then retry the build.

### Step 8: Install ZeroClaw

```bash
# For gnueabihf (default target)
sudo cp target/release/zeroclaw /usr/local/bin/

# For musleabihf
sudo cp target/armv6l-unknown-linux-musleabihf/release/zeroclaw /usr/local/bin/

# Verify installation
zeroclaw --version

# Verify binary is statically linked (musleabihf only)
file /usr/local/bin/zeroclaw
# Should show "statically linked" for musleabihf
```

## Option B: Cross-Compilation (Recommended)

For faster builds, cross-compile from a more powerful machine (Linux, macOS, or Windows). A native build on Pi Zero W can take **30-60 minutes**, while cross-compilation typically completes in **5-10 minutes**.

### Why Cross-Compile?

| Factor | Native (Pi Zero W) | Cross-Compile (x86_64) |
|--------|-------------------|------------------------|
| Build time | 30-60 minutes | 5-10 minutes |
| RAM required | 512MB + 2GB swap | 4GB+ typical |
| CPU load | High (single core) | Low relative to host |
| Iteration speed | Slow | Fast |

### Prerequisites

On your build host (Linux x86_64 example):

```bash
# Install ARM cross-compilation toolchain
# Note: We use gcc-arm-linux-gnueabihf as the linker tool,
# but Rust's target configuration produces a static musl binary
sudo apt install -y musl-tools musl-dev gcc-arm-linux-gnueabihf

# Verify cross-compiler is available
arm-linux-gnueabihf-gcc --version
```

**Why gnueabihf for musl builds?**

Pure `arm-linux-musleabihf-gcc` cross-compilers are not available in standard Ubuntu/Debian repositories. The workaround:
1. Use `gcc-arm-linux-gnueabihf` as the linker tool (available in repos)
2. Rust's target spec (`armv6l-unknown-linux-musleabihf.json`) sets `env: "musl"`
3. Static linking (`-C link-arg=-static`) eliminates glibc dependency
4. Result: a portable static musl binary that works on any ARMv6 Linux

**macOS:** Install via Homebrew:
```bash
brew install musl-cross
```

**Windows:** Use WSL2 or install mingw-w64 cross-compilers.

### Build for musleabihf (Recommended)

The ZeroClaw repository includes pre-configured `.cargo/config.toml` and `.cargo/armv6l-unknown-linux-musleabihf.json` for static linking.

```bash
# Clone ZeroClaw repository
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# Add ARMv6 musl target to rustup
rustup target add armv6l-unknown-linux-musleabihf

# The repository's .cargo/config.toml already contains:
# [target.armv6l-unknown-linux-musleabihf]
# rustflags = ["-C", "link-arg=-static"]
#
# And .cargo/armv6l-unknown-linux-musleabihf.json provides
# the target specification for proper ARMv6 support.

# Build for target (static binary, no runtime dependencies)
cargo build --release --target armv6l-unknown-linux-musleabihf
```

### Understanding Static Linking Benefits

The `rustflags = ["-C", "link-arg=-static"]` flag ensures **fully static linking**:

| Benefit | Description |
|---------|-------------|
| **No libc dependency** | Binary works on any ARMv6 Linux distribution |
| **Smaller size** | musl produces more compact binaries than glibc |
| **Version-agnostic** | Runs on Raspberry Pi OS Bullseye, Bookworm, or future versions |
| **Secure by default** | Reduced attack surface with musl's minimal libc |
| **Portable** | Same binary works across different Pi models with ARMv6 |

### Verify Static Linking

After building, confirm the binary is statically linked:

```bash
file target/armv6l-unknown-linux-musleabihf/release/zeroclaw
# Output should include: "statically linked"

ldd target/armv6l-unknown-linux-musleabihf/release/zeroclaw
# Output should be: "not a dynamic executable"
```

### Build for gnueabihf (Alternative)

If you need dynamic linking or have specific glibc dependencies:

```bash
# Add ARMv6 glibc target
rustup target add armv6l-unknown-linux-gnueabihf

# Build for target
cargo build --release --target armv6l-unknown-linux-gnueabihf
```

**Note:** gnueabihf binaries will be larger and depend on the target system's glibc version.

### Build with Custom Features

Reduce binary size by building only needed features:

```bash
# Minimal build (agent core only)
cargo build --release --target armv6l-unknown-linux-musleabihf --no-default-features

# Specific feature set
cargo build --release --target armv6l-unknown-linux-musleabihf --features "telegram,discord"

# Use dist profile for size-optimized binary
cargo build --profile dist --target armv6l-unknown-linux-musleabihf
```

### Transfer to Pi Zero W

```bash
# From build machine (adjust target as needed)
scp target/armv6l-unknown-linux-musleabihf/release/zeroclaw pi@zero-w-ip:/home/pi/

# On Pi Zero W
sudo mv ~/zeroclaw /usr/local/bin/
sudo chmod +x /usr/local/bin/zeroclaw
zeroclaw --version

# Verify it's statically linked (no dependencies on target system)
ldd /usr/local/bin/zeroclaw
# Should output: "not a dynamic executable"
```

### Cross-Compilation Workflow Summary

```
┌─────────────────┐     Clone/Fork     ┌─────────────────────┐
│  ZeroClaw Repo  │ ──────────────────> │   Your Build Host   │
│  (GitHub)       │                    │  (Linux/macOS/Win)  │
└─────────────────┘                    └─────────────────────┘
                                                │
                                                │ rustup target add
                                                │ cargo build --release
                                                ▼
                                        ┌─────────────────────┐
                                        │  Static Binary      │
                                        │  (armv6l-musl)      │
                                        └─────────────────────┘
                                                │
                                                │ scp / rsync
                                                ▼
                                        ┌─────────────────────┐
                                        │  Raspberry Pi       │
                                        │  Zero W             │
                                        │  /usr/local/bin/    │
                                        └─────────────────────┘
```

## Post-Installation Configuration

### Initialize ZeroClaw

```bash
# Run interactive setup
zeroclaw setup

# Or configure manually
mkdir -p ~/.config/zeroclaw
nano ~/.config/zeroclaw/config.toml
```

### Enable Hardware Features (Optional)

For Raspberry Pi GPIO support:

```bash
# Build with peripheral-rpi feature (native build only)
cargo build --release --features peripheral-rpi
```

### Run as System Service (Optional)

Create a systemd service:

```bash
sudo nano /etc/systemd/system/zeroclaw.service
```

Add the following:

```ini
[Unit]
Description=ZeroClaw AI Agent
After=network.target

[Service]
Type=simple
User=pi
WorkingDirectory=/home/pi
ExecStart=/usr/local/bin/zeroclaw agent
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable zeroclaw
sudo systemctl start zeroclaw
```

## Troubleshooting

### Build Fails with "Out of Memory"

**Solution:** Increase swap size:

```bash
sudo swapoff /swapfile
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
```

### Linker Errors

**Solution:** Ensure proper toolchain is installed:

```bash
sudo apt install -y build-essential pkg-config libssl-dev
```

### SSL/TLS Errors at Runtime

**Solution:** Install SSL certificates:

```bash
sudo apt install -y ca-certificates
```

### Binary Too Large

**Solution:** Build with minimal features:

```bash
cargo build --release --no-default-features --features "wasm-tools"
```

Or use the `.dist` profile:

```bash
cargo build --profile dist
```

## Performance Tips

1. **Use Lite OS:** Raspberry Pi OS Lite has lower overhead
2. **Overclock (Optional):** Add `arm_freq=1000` to `/boot/config.txt`
3. **Disable GUI:** `sudo systemctl disable lightdm` (if using desktop)
4. **Use external storage:** Build on USB 3.0 drive if available

## Related Documents

- [Hardware Peripherals Design](../hardware-peripherals-design.md) - Architecture
- [One-Click Bootstrap](../one-click-bootstrap.md) - General installation
- [Operations Runbook](../operations/operations-runbook.md) - Running in production

## References

- [Raspberry Pi Zero W Specifications](https://www.raspberrypi.com/products/raspberry-pi-zero-w/)
- [Rust Cross-Compilation Guide](https://rust-lang.github.io/rustc/platform-support.html)
- [Cargo Profile Configuration](https://doc.rust-lang.org/cargo/reference/profiles.html)
