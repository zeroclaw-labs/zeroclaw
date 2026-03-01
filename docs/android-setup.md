# Android Setup

ZeroClaw provides prebuilt binaries for Android devices.

## Supported Architectures

| Target | Android Version | Devices |
|--------|-----------------|---------|
| `armv7-linux-androideabi` | Android 4.1+ (API 16+) | Older 32-bit phones (Galaxy S3, etc.) |
| `aarch64-linux-android` | Android 5.0+ (API 21+) | Modern 64-bit phones |

## Installation via Termux

The easiest way to run ZeroClaw on Android is via [Termux](https://termux.dev/).

### 1. Install Termux

Download from [F-Droid](https://f-droid.org/packages/com.termux/) (recommended) or GitHub releases.

> ⚠️ **Note:** The Play Store version is outdated and unsupported.

### 2. Download ZeroClaw

```bash
# Check your architecture
uname -m
# aarch64 = 64-bit, armv7l/armv8l = 32-bit

# Download the appropriate binary
# For 64-bit (aarch64):
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-linux-android.tar.gz
tar xzf zeroclaw-aarch64-linux-android.tar.gz

# For 32-bit (armv7):
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-armv7-linux-androideabi.tar.gz
tar xzf zeroclaw-armv7-linux-androideabi.tar.gz
```

### 3. Install and Run

```bash
chmod +x zeroclaw
mv zeroclaw $PREFIX/bin/

# Verify installation
zeroclaw --version

# Run setup
zeroclaw onboard
```

## Direct Installation via ADB

For advanced users who want to run ZeroClaw outside Termux:

```bash
# From your computer with ADB
adb push zeroclaw /data/local/tmp/
adb shell chmod +x /data/local/tmp/zeroclaw
adb shell /data/local/tmp/zeroclaw --version
```

> ⚠️ Running outside Termux requires a rooted device or specific permissions for full functionality.

## Limitations on Android

- **No systemd:** Use Termux's `termux-services` for daemon mode
- **Storage access:** Requires Termux storage permissions (`termux-setup-storage`)
- **Network:** Some features may require Android VPN permission for local binding

## Building from Source

ZeroClaw supports two Android source-build workflows.

### A) Build directly inside Termux (on-device)

Use this when compiling natively on your phone/tablet.

```bash
# Termux prerequisites
pkg update
pkg install -y clang pkg-config

# Add Android Rust targets (aarch64 target is enough for most devices)
rustup target add aarch64-linux-android armv7-linux-androideabi

# Build for your current device arch
cargo build --release --target aarch64-linux-android
```

Notes:
- `.cargo/config.toml` uses `clang` for Android targets by default.
- You do not need NDK-prefixed linkers such as `aarch64-linux-android21-clang` for native Termux builds.
- The `wasm-tools` runtime is currently unavailable on Android builds; WASM tools fall back to a stub implementation.

### B) Cross-compile from Linux/macOS with Android NDK

Use this when building Android binaries from a desktop CI/dev machine.

```bash
# Add targets
rustup target add armv7-linux-androideabi aarch64-linux-android

# Configure Android NDK toolchain
export ANDROID_NDK_HOME=/path/to/ndk
export NDK_TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
export PATH="$NDK_TOOLCHAIN:$PATH"

# Override Cargo defaults with NDK wrapper linkers
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$NDK_TOOLCHAIN/armv7a-linux-androideabi21-clang"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$NDK_TOOLCHAIN/aarch64-linux-android21-clang"

# Ensure cc-rs build scripts use the same compilers
export CC_armv7_linux_androideabi="$CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"

# Build
cargo build --release --target armv7-linux-androideabi
cargo build --release --target aarch64-linux-android
```

### Quick environment self-check

Use the built-in checker to validate linker/toolchain setup before long builds:

```bash
# From repo root
scripts/android/termux_source_build_check.sh --target aarch64-linux-android

# Force Termux-native diagnostics
scripts/android/termux_source_build_check.sh --target aarch64-linux-android --mode termux-native

# Force desktop NDK-cross diagnostics
scripts/android/termux_source_build_check.sh --target aarch64-linux-android --mode ndk-cross

# Run an actual cargo check after environment validation
scripts/android/termux_source_build_check.sh --target aarch64-linux-android --run-cargo-check
```

When `--run-cargo-check` fails, the script now analyzes common linker/`cc-rs` errors and prints
copy-paste fix commands for the selected mode.

You can also diagnose a previously captured cargo log directly:

```bash
scripts/android/termux_source_build_check.sh \
  --target aarch64-linux-android \
  --mode ndk-cross \
  --diagnose-log /path/to/cargo-error.log
```

For CI automation, emit a machine-readable report:

```bash
scripts/android/termux_source_build_check.sh \
  --target aarch64-linux-android \
  --mode ndk-cross \
  --diagnose-log /path/to/cargo-error.log \
  --json-output /tmp/zeroclaw-android-selfcheck.json
```

For pipeline usage, output JSON directly to stdout:

```bash
scripts/android/termux_source_build_check.sh \
  --target aarch64-linux-android \
  --mode ndk-cross \
  --diagnose-log /path/to/cargo-error.log \
  --json-output - \
  --quiet
```

JSON report highlights:
- `status`: `ok` or `error`
- `error_code`: stable classifier (`NONE`, `BAD_ARGUMENT`, `MISSING_DIAGNOSE_LOG`, `CARGO_CHECK_FAILED`, etc.)
- `detection_codes`: structured diagnosis codes (`CC_RS_TOOL_NOT_FOUND`, `LINKER_RESOLUTION_FAILURE`, `MISSING_RUST_TARGET_STDLIB`, ...)
- `suggestions`: copy-paste recovery commands

Enable strict gating when integrating into CI:

```bash
scripts/android/termux_source_build_check.sh \
  --target aarch64-linux-android \
  --mode ndk-cross \
  --diagnose-log /path/to/cargo-error.log \
  --json-output /tmp/zeroclaw-android-selfcheck.json \
  --strict
```

## Troubleshooting

### "Permission denied"

```bash
chmod +x zeroclaw
```

### "not found" or linker errors

Make sure you downloaded the correct architecture for your device.

For native Termux builds, make sure `clang` exists and remove stale NDK overrides:

```bash
unset CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER
unset CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER
unset CC_aarch64_linux_android
unset CC_armv7_linux_androideabi
command -v clang
```

For cross-compilation, ensure `ANDROID_NDK_HOME` and `CARGO_TARGET_*_LINKER` point to valid NDK binaries.
If build scripts (for example `ring`/`aws-lc-sys`) still report `failed to find tool "aarch64-linux-android-clang"`,
also export `CC_aarch64_linux_android` / `CC_armv7_linux_androideabi` to the same NDK clang wrappers.

### "WASM tools are unavailable on Android"

This is expected today. Android builds run the WASM tool loader in stub mode; build on Linux/macOS/Windows if you need runtime `wasm-tools` execution.

### Old Android (4.x)

Use the `armv7-linux-androideabi` build with API level 16+.
