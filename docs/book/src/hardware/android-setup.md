# Android setup

ZeroClaw supports two Android deployment modes:

| Mode | Use it when |
|---|---|
| **ZeroClaw Android APK (experimental)** | You want the agent to run as a standalone app with opt-in Accessibility, screenshot/vision, UI actions, Android device APIs, and a floating chat overlay. |
| **Termux binary** | You want a conventional shell-hosted ZeroClaw process without app-level UI control. |

Both modes currently target 64-bit ARM devices.

## ZeroClaw Android APK (experimental)

The source project lives at `apps/android/` in the ZeroClaw repository.
It packages the Rust gateway, dashboard, Android-native bridge, AccessibilityService, and generic
Android skill into one APK. The agent and the bridge share an app UID; the typed `android_*` tools
reach Accessibility and device APIs over an app-private Unix-domain socket.

The installed app and package retain the `zerodroid` name for upgrade compatibility; the bundled
agent runtime is ZeroClaw from the same repository revision.

The APK is a sideload build, not a Play Store package. Fresh installs keep phone tools, autonomous
control, encrypted remote access, SSH, boot start, and the overlay off. Accessibility and overlay access still
require explicit grants in Android Settings.

The APK requires Android 11 / API 30 or newer and a 64-bit ARM device. Follow
`apps/android/README.md` when building from source, then use
[Android-native tools](../tools/android.md) for tool configuration and security behavior.

## Termux and standalone binaries

The standalone binary path remains available for shell-hosted deployments that do not need the
APK's Android-native capability layer.

## Supported architectures

The stable prebuilt release targets (derived from the release workflow) are:

{{#include ../_snippets/hardware-release-targets.md}}

`aarch64-linux-android` is the only Android target. Its release binary is
experimental: it is attached when that build succeeds, but it is not
guaranteed for every release. If it is absent, build from source (see below).
32-bit Android (`armv7-linux-androideabi`) is not currently published as a
prebuilt binary.

## Installation via Termux

The easiest way to run ZeroClaw on Android is via [Termux](https://termux.dev/).

### 1. Install Termux

Download from [F-Droid](https://f-droid.org/packages/com.termux/) (recommended) or GitHub releases.

> ⚠️ **Note:** The Play Store version is outdated and unsupported.

### 2. Download ZeroClaw

```sh
# Check your architecture
uname -m
# aarch64 = 64-bit (experimental prebuilt binary may be available)
# armv7l/armv8l = 32-bit (build from source — no prebuilt binary)

# Optionally download the experimental 64-bit (aarch64) binary.
# A 404 means this release did not build it; use the source build below instead.
if curl -fLO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-linux-android.tar.gz; then
  tar xzf zeroclaw-aarch64-linux-android.tar.gz
else
  echo "Download failed. If GitHub reported 404, build from source below. Otherwise, check the error and retry."
fi
```

</div>

Continue to the next step only after the archive extracts successfully. Otherwise,
build from source when the asset is missing, or resolve the download error and retry.

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

## Direct binary installation via ADB

For advanced users who want to run ZeroClaw outside Termux:

```sh
# From your computer with ADB
adb push zeroclaw /data/local/tmp/
adb shell chmod +x /data/local/tmp/zeroclaw
adb shell /data/local/tmp/zeroclaw --version
```

> ⚠️ Running outside Termux requires a rooted device or specific permissions for full functionality.

## Termux/binary limitations

- **No systemd:** Use Termux's `termux-services` for daemon mode
- **Storage access:** Requires Termux storage permissions (`termux-setup-storage`)
- **Network:** Some features may require Android VPN permission for local binding

## Building the standalone binary from source

To build for Android yourself:

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

## Troubleshooting

### "Permission denied"

```sh
chmod +x zeroclaw
```

### "not found" or linker errors

Make sure you downloaded the correct architecture for your device.

### Old / 32-bit Android

There is no prebuilt 32-bit Android binary. On a 32-bit device, add the
`armv7-linux-androideabi` target and build from source as shown above.
