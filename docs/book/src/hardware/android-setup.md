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
control, LAN access, SSH, boot start, and the overlay off. Accessibility and overlay access still
require explicit grants in Android Settings.

| Flavor | Minimum Android | Difference |
|---|---:|---|
| `full` | Android 12 / API 31 | Includes Google AI Edge/AICore support for on-device Gemini Nano. |
| `lite` | Android 11 / API 30 | Omits AI Edge; cloud providers and Android-native tools are unchanged. |

Both flavors are arm64-only and use the same application ID, so install one at a time. Follow
`apps/android/README.md` when building from source, then use
[Android-native tools](../tools/android.md) for tool configuration and security behavior.

## Termux and standalone binaries

The standalone binary path remains available for shell-hosted deployments that do not need the
APK's Android-native capability layer.

## Supported architectures

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

```sh
# Check your architecture
uname -m
# aarch64 = 64-bit (prebuilt binary available)
# armv7l/armv8l = 32-bit (build from source — no prebuilt binary)

# Download the prebuilt 64-bit (aarch64) binary
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-linux-android.tar.gz
tar xzf zeroclaw-aarch64-linux-android.tar.gz
```

### 3. Install and run

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
