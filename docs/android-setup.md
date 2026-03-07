# Android Setup

ZeroClaw provides prebuilt binaries for Android devices.

## Supported Architectures

| Target | Type | Android Version | Devices |
|--------|------|-----------------|---------|
| `aarch64-unknown-linux-musl` | musl (static) | Android 8.0+ (API 26+) | Modern 64-bit phones (recommended) |
| `armv7-unknown-linux-musleabihf` | musl (static) | Android 8.0+ (API 26+) | Older 32-bit phones |
| `x86_64-unknown-linux-musl` | musl (static) | Android 8.0+ (API 26+) | Android x86 emulators |
| `aarch64-linux-android` | Bionic (dynamic) | Android 5.0+ (API 21+) | Modern 64-bit phones |
| `armv7-linux-androideabi` | Bionic (dynamic) | Android 4.1+ (API 16+) | Older 32-bit phones |

### Which one should I choose?

**For most users:** Use `aarch64-unknown-linux-musl` (or `armv7-unknown-linux-musleabihf` for 32-bit devices).

**musl vs Bionic:**
- **musl (static)**: Single binary, no dependencies, works on Android 8.0+. Smaller, more portable.
- **Bionic (dynamic)**: Links against Android's libc, works on older Android versions (4.1+), but requires compatible system libraries.

**Check your architecture:**
```bash
uname -m
# aarch64 = 64-bit (most modern phones)
# armv7l/armv8l = 32-bit (older phones)
# x86_64 = emulator
```

## Installation via Termux

The easiest way to run ZeroClaw on Android is via [Termux](https://termux.dev/).

### 1. Install Termux

Download from [F-Droid](https://f-droid.org/packages/com.termux/) (recommended) or GitHub releases.

> ⚠️ **Note:** The Play Store version is outdated and unsupported.

### 2. Download ZeroClaw

```bash
# Check your architecture
uname -m

# Download the appropriate binary
# For 64-bit (aarch64) - RECOMMENDED:
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-unknown-linux-musl.tar.gz
tar xzf zeroclaw-aarch64-unknown-linux-musl.tar.gz

# For 32-bit (armv7) - RECOMMENDED:
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-armv7-unknown-linux-musleabihf.tar.gz
tar xzf zeroclaw-armv7-unknown-linux-musleabihf.tar.gz

# Alternative: Bionic builds for older Android versions
# For 64-bit (Android 5.0+):
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-linux-android.tar.gz
tar xzf zeroclaw-aarch64-linux-android.tar.gz

# For 32-bit (Android 4.1+):
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

### Build for musl (recommended)

```bash
# Install musl cross-compiler
sudo apt-get install musl-tools gcc-aarch64-linux-gnu gcc-arm-linux-gnueabihf

# Add targets
rustup target add aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf

# Build
cargo build --release --target aarch64-unknown-linux-musl
cargo build --release --target armv7-unknown-linux-musleabihf
```

### Build for Android (Bionic)

```bash
# Install Android NDK
# Download from: https://developer.android.com/ndk/downloads
export ANDROID_NDK_HOME=/path/to/ndk
export PATH=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH

# Add targets
rustup target add armv7-linux-androideabi aarch64-linux-android

# Build (API level 21 for Android 5.0+)
cargo build --release --target armv7-linux-androideabi
cargo build --release --target aarch64-linux-android
```

## Troubleshooting

### "Permission denied"
```bash
chmod +x zeroclaw
```

### "not found" or linker errors
Make sure you downloaded the correct architecture for your device.

### Old Android (4.x-7.x)
Use the Bionic builds (`*-linux-android` or `*-linux-androideabi`) instead of musl.

### Binary doesn't start on Android < 8.0
musl requires Android 8.0+ (API 26). Use the Bionic builds for older versions.
