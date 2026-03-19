# Cross-Compilation Guide

Cross-compile ZeroClaw for embedded devices like Orange Pi, Raspberry Pi Zero, and other ARM-based boards.

## Overview

ZeroClaw supports cross-compilation to ARM targets for deployment on small embedded devices. This guide covers two approaches:

1. **Using `cargo` with `--target`** — Direct compilation with system toolchains
2. **Using `cross`** — Docker-based cross-compilation (easier setup)

## Prerequisites

### Install Rust Target Support

Add the target architectures you need:

```bash
# 64-bit ARM (Raspberry Pi 4/5, Orange Pi 5, modern boards)
rustup target add aarch64-unknown-linux-gnu

# 32-bit ARM with hard-float (Raspberry Pi 2/3, Orange Pi)
rustup target add armv7-unknown-linux-gnueabihf

# ARMv6 (Raspberry Pi Zero/1 - older)
rustup target add arm-unknown-linux-gnueabihf

# 64-bit ARM with musl (static linking)
rustup target add aarch64-unknown-linux-musl
```

## Method 1: Using Cargo with --target

### Install Cross-Compilation Toolchains

#### Debian/Ubuntu

```bash
# For aarch64 (64-bit ARM)
sudo apt-get install gcc-aarch64-linux-gnu

# For armv7 (32-bit ARM hard-float)
sudo apt-get install gcc-arm-linux-gnueabihf

# For armv6 (Raspberry Pi Zero)
sudo apt-get install gcc-arm-linux-gnueabi
```

#### macOS

On macOS, use the `cross` tool (Method 2) or install toolchains via Homebrew:

```bash
# Install aarch64 toolchain
brew install aarch64-elf-gcc
```

### Build Commands

ZeroClaw includes pre-configured linker settings in `.cargo/config.toml`:

```bash
# Build for Raspberry Pi 4/5, Orange Pi 5 (64-bit)
cargo build --release --target aarch64-unknown-linux-gnu

# Build for Raspberry Pi 2/3, Orange Pi (32-bit hard-float)
cargo build --release --target armv7-unknown-linux-gnueabihf

# Build for Raspberry Pi Zero/1 (ARMv6)
cargo build --release --target arm-unknown-linux-gnueabihf
```

### Low-Memory Build Profile

For devices with limited RAM (Raspberry Pi 3 with 1GB or less), use the `release-arm` profile:

```bash
# Uses thin LTO to reduce peak memory usage
cargo build --profile release-arm --target aarch64-unknown-linux-gnu

# For very constrained systems (Pi Zero with 512MB), limit parallel jobs:
CARGO_BUILD_JOBS=1 cargo build --profile release-arm --target arm-unknown-linux-gnueabihf
```

## Method 2: Using `cross` (Recommended)

The `cross` tool uses Docker containers with pre-configured toolchains, eliminating the need to install system cross-compilers.

### Install cross

```bash
cargo install cross --git https://github.com/cross-rs/cross
```

### Build with cross

```bash
# Build for Raspberry Pi 4/5 (64-bit)
cross build --release --target aarch64-unknown-linux-gnu

# Build for Raspberry Pi 2/3, Orange Pi (32-bit)
cross build --release --target armv7-unknown-linux-gnueabihf

# Build for Raspberry Pi Zero/1
cross build --release --target arm-unknown-linux-gnueabihf
```

### Using Podman instead of Docker

```bash
CROSS_CONTAINER_ENGINE=podman cross build --release --target aarch64-unknown-linux-gnu
```

## Target Reference

| Target | Architecture | Devices | Notes |
|--------|--------------|---------|-------|
| `aarch64-unknown-linux-gnu` | ARM64 | Raspberry Pi 4/5, Orange Pi 5, modern ARM SBCs | Best performance, full 64-bit |
| `armv7-unknown-linux-gnueabihf` | ARMv7 HF | Raspberry Pi 2/3, Orange Pi PC/H3/H5 | Good compatibility |
| `arm-unknown-linux-gnueabihf` | ARMv6 HF | Raspberry Pi Zero/1, original Pi | For older/lowest-power devices |
| `aarch64-unknown-linux-musl` | ARM64 (static) | Any 64-bit ARM Linux | Static binary, no libc dependency |

## Feature Flags for Embedded

Reduce binary size by disabling unused features:

```bash
# Minimal build for embedded (no Matrix, no Prometheus metrics)
cargo build --release --target aarch64-unknown-linux-gnu --no-default-features

# With specific channels only
cargo build --release --target aarch64-unknown-linux-gnu \
  --no-default-features --features channel-matrix

# With hardware peripheral support (Raspberry Pi GPIO)
cargo build --release --target aarch64-unknown-linux-gnu \
  --features peripheral-rpi
```

## Transferring to Target Device

### Using SCP

```bash
# Copy binary to Raspberry Pi
scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@raspberrypi.local:/usr/local/bin/

# Copy to Orange Pi
scp target/armv7-unknown-linux-gnueabihf/release/zeroclaw root@orangepi.local:/usr/local/bin/
```

### Using rsync

```bash
rsync -avz --progress target/aarch64-unknown-linux-gnu/release/zeroclaw pi@raspberrypi.local:~/
ssh pi@raspberrypi.local 'sudo mv ~/zeroclaw /usr/local/bin/ && sudo chmod +x /usr/local/bin/zeroclaw'
```

## Verification

After transferring, verify the binary works on the target:

```bash
# Check binary architecture
ssh pi@raspberrypi.local 'file /usr/local/bin/zeroclaw'
# Expected: ELF 64-bit LSB executable, ARM aarch64

# Test run
ssh pi@raspberrypi.local 'zeroclaw --version'
ssh pi@raspberrypi.local 'zeroclaw status'
```

## Troubleshooting

### "linker not found" Error

Install the appropriate cross-compiler:

```bash
# Ubuntu/Debian
sudo apt-get install gcc-aarch64-linux-gnu  # for aarch64
sudo apt-get install gcc-arm-linux-gnueabihf  # for armv7
```

### "undefined reference" Errors

Ensure you're using the correct target. `arm-unknown-linux-gnueabihf` and `armv7-unknown-linux-gnueabihf` are different targets.

### Out of Memory During Build

Use the `release-arm` profile or limit build jobs:

```bash
# Limit to single job (slower but uses less RAM)
CARGO_BUILD_JOBS=1 cargo build --profile release-arm --target aarch64-unknown-linux-gnu

# Or use cross (uses container, doesn't exhaust host memory)
cross build --release --target aarch64-unknown-linux-gnu
```

### Binary Won't Run on Target

Check architecture compatibility:

```bash
# On target device
uname -m
# aarch64 = 64-bit ARM
# armv7l = 32-bit ARM

# Check what the binary expects
file zeroclaw
```

### SSL/TLS Issues on Target

If you get SSL errors when running on the target, the binary may need CA certificates:

```bash
# On Debian/Ubuntu target
sudo apt-get install ca-certificates

# Or use static musl build for easier deployment
cargo build --release --target aarch64-unknown-linux-musl
```

## Example: Complete Raspberry Pi 4 Deployment

```bash
# On development machine
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu

# Transfer and install
scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@raspberrypi.local:~/
ssh pi@raspberrypi.local << 'REMOTE'
  sudo mv ~/zeroclaw /usr/local/bin/
  sudo chmod +x /usr/local/bin/zeroclaw
  zeroclaw --version
  zeroclaw onboard --api-key YOUR_API_KEY --provider openrouter
  zeroclaw service install
REMOTE
```

## Example: Orange Pi Zero (ARMv7)

```bash
# On development machine
rustup target add armv7-unknown-linux-gnueabihf
cargo build --profile release-arm --target armv7-unknown-linux-gnueabihf

# Transfer to Orange Pi
scp target/armv7-unknown-linux-gnueabihf/release/zeroclaw root@orangepi.local:/usr/local/bin/

# Test on Orange Pi
ssh root@orangepi.local 'zeroclaw status'
```

## See Also

- [Hardware & Peripherals](../hardware/README.md) — GPIO and board-specific setup
- [Operations Runbook](../ops/operations-runbook.md) — Production deployment
- [Troubleshooting](../ops/troubleshooting.md) — Common issues and fixes
