# Cross-Compilation Guide for Embedded Devices

This guide explains how to cross-compile ZeroClaw for ARM-based embedded devices like Raspberry Pi, Orange Pi, and other single-board computers.

## Prerequisites

Before cross-compiling, ensure you have:

- Rust toolchain installed on your host machine
- Cross-compilation target support
- The `cross` tool (recommended) or manual cross-compilation setup

## Installing the Cross Tool

The easiest way to cross-compile Rust projects is using the [`cross`](https://github.com/cross-rs/cross) tool, which handles cross-compilation in Docker containers.

### Install via cargo:

```bash
cargo install cross --git https://github.com/cross-rs/cross
```

### Verify installation:

```bash
cross --version
```

## Supported Targets

ZeroClaw supports the following ARM targets:

| Target | Architecture | Common Devices |
|--------|--------------|----------------|
| `aarch64-unknown-linux-gnu` | ARM64 / ARMv8 | Raspberry Pi 3/4/5, Orange Pi 5, modern ARM boards |
| `armv7-unknown-linux-gnueabihf` | ARMv7 (32-bit with hard float) | Raspberry Pi 2, older ARM boards |

## Cross-Compiling for Raspberry Pi

### Raspberry Pi 4 / 5 (ARM64)

```bash
# Add the target
rustup target add aarch64-unknown-linux-gnu

# Build using cross
cross build --target aarch64-unknown-linux-gnu --release
```

### Raspberry Pi 2 / 3 (32-bit)

```bash
# Add the target
rustup target add armv7-unknown-linux-gnueabihf

# Build using cross
cross build --target armv7-unknown-linux-gnueabihf --release
```

### Manual Cross-Compilation (without cross tool)

If you prefer not to use Docker, install the cross-compiler toolchain:

**Ubuntu/Debian:**
```bash
sudo apt-get install gcc-aarch64-linux-gnu
sudo apt-get install gcc-arm-linux-gnueabihf
```

**macOS (using Homebrew):**
```bash
# For ARM64
brew install aarch64-linux-gnu

# For ARMv7
brew install arm-linux-gnueabihf
```

Then configure Cargo:

```bash
# For ARM64
cat >> ~/.cargo/config.toml << 'CARGOCONFIG'
[target.aarch64-unknown-linux-gnu]
linker = "aarch64-linux-gnu-gcc"
CARGOCONFIG

# For ARMv7
cat >> ~/.cargo/config.toml << 'CARGOCONFIG'
[target.armv7-unknown-linux-gnueabihf]
linker = "arm-linux-gnueabihf-gcc"
CARGOCONFIG
```

Build:

```bash
cargo build --target aarch64-unknown-linux-gnu --release
# or
cargo build --target armv7-unknown-linux-gnueabihf --release
```

## Cross-Compiling for Orange Pi

Orange Pi boards typically use ARM64 architecture:

```bash
# Orange Pi 5 and newer
cross build --target aarch64-unknown-linux-gnu --release

# Older Orange Pi models (32-bit)
cross build --target armv7-unknown-linux-gnueabihf --release
```

## Deploying to the Target Device

After building, transfer the binary to your device:

```bash
# For Raspberry Pi 4/5 (ARM64)
scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@raspberrypi.local:/home/pi/

# For Raspberry Pi 2/3 (ARMv7)
scp target/armv7-unknown-linux-gnueabihf/release/zeroclaw pi@raspberrypi.local:/home/pi/

# For Orange Pi
scp target/aarch64-unknown-linux-gnu/release/zeroclaw root@orangepi.local:/usr/local/bin/
```

## Verification on Target Device

SSH into your device and verify:

```bash
# Check binary architecture
file zeroclaw

# Expected output for ARM64:
# zeroclaw: ELF 64-bit LSB executable, ARM aarch64, version 1 (SYSV), statically linked

# Run ZeroClaw
./zeroclaw --version
./zeroclaw status
```

## Troubleshooting

### Build Errors

#### "linker 'cc' not found"

Install the appropriate cross-compiler:

```bash
# Ubuntu/Debian
sudo apt-get install gcc-aarch64-linux-gnu gcc-arm-linux-gnueabihf

# Fedora/RHEL
sudo dnf install gcc-aarch64-linux-gnu gcc-arm-linux-gnu
```

#### Missing system libraries

If you encounter missing library errors, use the `cross` tool which includes all necessary dependencies in its Docker images.

### Runtime Errors

#### "cannot execute binary file: Exec format error"

This indicates a target mismatch. Verify:
1. You're using the correct target for your device
2. The binary was successfully transferred (not corrupted)
3. Device architecture matches the build target

Check device architecture:
```bash
uname -m
# aarch64 = ARM64
# armv7l = ARMv7 32-bit
```

#### Missing shared libraries

For dynamic linking issues, either:
1. Install required libraries on the target device
2. Use static linking:
   ```bash
   RUSTFLAGS='-C target-feature=+crt-static' cross build --target aarch64-unknown-linux-gnu --release
   ```

### Docker Issues (when using cross)

#### Permission denied

Ensure your user is in the `docker` group:
```bash
sudo usermod -aG docker $USER
# Log out and back in for changes to take effect
```

#### Slow builds

Cross-compilation in Docker can be slow on first run. Subsequent builds will use cached layers.

## Quick Reference Commands

```bash
# Install cross tool
cargo install cross --git https://github.com/cross-rs/cross

# Add targets
rustup target add aarch64-unknown-linux-gnu
rustup target add armv7-unknown-linux-gnueabihf

# Build for Raspberry Pi 4/5 (ARM64)
cross build --target aarch64-unknown-linux-gnu --release

# Build for Raspberry Pi 2/3 (32-bit)
cross build --target armv7-unknown-linux-gnueabihf --release

# Static linking (no runtime dependencies)
RUSTFLAGS='-C target-feature=+crt-static' cross build --target aarch64-unknown-linux-gnu --release

# Deploy to Raspberry Pi
scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@raspberrypi.local:~/
```

## Further Reading

- [cross-rs documentation](https://github.com/cross-rs/cross)
- [Rust Embedded Book](https://docs.rust-embedded.org/book/)
- [ZeroClaw Hardware Guide](../hardware/README.md)
