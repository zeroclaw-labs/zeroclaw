# HarmonyOS Build Guide

This document describes how to build ZeroClaw for HarmonyOS devices.

## Prerequisites

### 1. Install Rust Toolchain

First, install Rust using rustup:

**Windows:**
```powershell
# Download and run rustup-init.exe from https://rustup.rs/
# Or use winget:
winget install Rustlang.Rustup
```

**Linux/macOS:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. Install HarmonyOS NDK

Download the HarmonyOS Command Line Tools from the official HarmonyOS developer website.

Set the `OHOS_NDK_HOME` environment variable to point to the NDK's native directory:

**Windows (PowerShell):**
```powershell
$env:OHOS_NDK_HOME = "\path\to\commandline-tools-windows\sdk\default\openharmony\native"
```

**Linux/macOS:**
```bash
export OHOS_NDK_HOME="/path/to/commandline-tools/sdk/default/openharmony/native"
```

## Building

### 

build with nightly rust

```
rustup component add rust-src --toolchain nightly-x86_64-unknown-linux-gnu
```

### Quick Build

Use the provided build script:

```bash
# Build for HarmonyOS ARM64 (aarch64)
./scripts/build-ohos.sh

# Build with custom NDK path
OHOS_NDK_HOME=/path/to/ndk ./scripts/build-ohos.sh
```

### Manual Build

1. Set up environment variables:

```bash
# Windows (Git Bash)
export OHOS_NDK_HOME="/path/to/commandline-tools-windows/sdk/default/openharmony/native"
export PATH="${OHOS_NDK_HOME}/llvm/bin:${PATH}"

# Set linker environment variables
export CARGO_TARGET_AARCH64_LINUX_OHOS_LINKER="${OHOS_NDK_HOME}/llvm/bin/clang"
export CC_aarch64_linux_ohos="${OHOS_NDK_HOME}/llvm/bin/clang"
export AR_aarch64_linux_ohos="${OHOS_NDK_HOME}/llvm/bin/llvm-ar"
```

2. Build with custom target:

```bash
cargo build --target aarch64-linux-ohos --release \
  --config .cargo/ohos-targets/aarch64-linux-ohos.json
```

## Deploying to Device

### Prerequisites

- HarmonyOS device with developer mode enabled
- `hdc` tool (HarmonyOS Device Connector) in PATH

### Deploy Binary

```bash
# Push binary to device
hdc file send ./target/aarch64-linux-ohos/release/zeroclaw /data/local/bin/

# Make executable and run
hdc shell
chmod +x /data/local/bin/zeroclaw
/data/local/bin/zeroclaw --version
```

## Target Architecture

| Target Triple | Architecture | Use Case |
|--------------|--------------|----------|
| `aarch64-linux-ohos` | ARM64 | Modern HarmonyOS devices |
| `armv7-linux-ohos` | ARM32 | Legacy 32-bit devices (not yet tested) |

## Known Limitations

1. **Sandbox**: HarmonyOS uses its own sandboxing mechanism. ZeroClaw will fall back to application-layer security when Landlock/Firejail are not available.

2. **Hardware Features**: GPIO and USB device discovery features may have different behavior on HarmonyOS compared to standard Linux.

3. **Service Management**: systemd service installation is not available on HarmonyOS.

## Troubleshooting

### Linker Errors

If you encounter linker errors, ensure:
1. `OHOS_NDK_HOME` is correctly set
2. The NDK's `llvm/bin` directory is in PATH
3. The sysroot exists at `${OHOS_NDK_HOME}/sysroot`

### Missing Libraries

If the binary fails to run on the device due to missing libraries:
1. Ensure the device runs HarmonyOS API 22 or later
2. Check if required shared libraries are present in `/system/lib64/`

### Build Failures

Some crates may have C dependencies that need compilation:
- `ring`: Uses assembly optimizations; should work with OHOS LLVM
- `rusqlite`: Uses bundled SQLite; should compile correctly

If a crate fails to compile, try:
1. Check if the crate has `target_env` or `target_os` specific code
2. Look for HarmonyOS-specific patches or workarounds
3. Report issues to the ZeroClaw repository

## Contributing

If you successfully build and test ZeroClaw on HarmonyOS, please consider contributing:
1. Report any issues or workarounds you discovered
2. Submit pull requests to improve HarmonyOS support
3. Help test on different HarmonyOS versions and devices
