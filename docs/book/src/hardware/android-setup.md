# Android Setup

ZeroClaw provides prebuilt binaries for Android devices.

## Supported Architectures

ZeroClaw publishes a prebuilt `aarch64-linux-android` binary for modern 64-bit
Android devices. The full set of prebuilt release targets (derived from the
release workflow) is:

{{#include ../_snippets/hardware-release-targets.md}}

Only `aarch64-linux-android` targets Android directly. 32-bit Android
(`armv7-linux-androideabi`) is not currently published as a prebuilt binary;
on a 32-bit device, build from source (see below).

## Installation via Termux

The easiest way to run ZeroClaw on Android is via [Termux](https://termux.dev/).

### 1. Install Termux

Download from [F-Droid](https://f-droid.org/packages/com.termux/) (recommended) or GitHub releases.

> ⚠️ **Note:** The Play Store version is outdated and unsupported.

### 2. Download ZeroClaw

<div class="os-tabs-src">

#### sh

```sh
# Check your architecture
uname -m
# aarch64 = 64-bit (prebuilt binary available)
# armv7l/armv8l = 32-bit (build from source — no prebuilt binary)

# Download the prebuilt 64-bit (aarch64) binary
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-linux-android.tar.gz
tar xzf zeroclaw-aarch64-linux-android.tar.gz
```

</div>

### 3. Install and Run

<div class="os-tabs-src">

#### sh

```sh
chmod +x zeroclaw
mv zeroclaw $PREFIX/bin/

# Verify installation
zeroclaw --version

# Run setup
zeroclaw quickstart
```

</div>

## Direct Installation via ADB

For advanced users who want to run ZeroClaw outside Termux:

<div class="os-tabs-src">

#### sh

```sh
# From your computer with ADB
adb push zeroclaw /data/local/tmp/
adb shell chmod +x /data/local/tmp/zeroclaw
adb shell /data/local/tmp/zeroclaw --version
```

</div>

> ⚠️ Running outside Termux requires a rooted device or specific permissions for full functionality.

## Limitations on Android

- **No systemd:** Use Termux's `termux-services` for daemon mode
- **Storage access:** Requires Termux storage permissions (`termux-setup-storage`)
- **Network:** Some features may require Android VPN permission for local binding

## Building from Source

To build for Android yourself:

<div class="os-tabs-src">

#### sh

```sh
# Install Android NDK
# Add targets
rustup target add armv7-linux-androideabi aarch64-linux-android

# Set NDK path
export ANDROID_NDK_HOME=/path/to/ndk
export PATH=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH

# Build
cargo build --release --target armv7-linux-androideabi
cargo build --release --target aarch64-linux-android
```

</div>

## Troubleshooting

### "Permission denied"

<div class="os-tabs-src">

#### sh

```sh
chmod +x zeroclaw
```

</div>

### "not found" or linker errors

Make sure you downloaded the correct architecture for your device.

### Old / 32-bit Android

There is no prebuilt 32-bit Android binary. On a 32-bit device, add the
`armv7-linux-androideabi` target and build from source as shown above.
