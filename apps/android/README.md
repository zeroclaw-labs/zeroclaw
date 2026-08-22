# ZeroClaw for Android (zerodroid)

zerodroid packages the ZeroClaw gateway, Android-native APIs, and an AccessibilityService into one
arm64 APK. The agent runs on the phone; a browser or paired client is only a control surface.

`zerodroid` remains the Android distribution and package identity (`org.zerodroid.bridge`) so the
existing signing lineage and installed-app upgrades stay compatible. In the monorepo it is the
experimental ZeroClaw Android app, not a separate agent runtime.

> **Experimental sideload.** Accessibility can read and act in other apps. Fresh installs
> keep phone tools, autonomous control, encrypted remote access, SSH, boot start, and the floating overlay off.
> Enable only the capabilities you intend to use on a device you control.

## What ships

- The `aarch64-linux-android` ZeroClaw binary, packaged as
  `jniLibs/arm64-v8a/libzeroclaw.so` so Android extracts it to an executable location.
- A foreground service that supervises the Rust gateway as an app-UID child process.
- Android-native device, sensor, location, BLE, Wi-Fi, SMS, contacts, Intent, and ML Kit APIs.
- An AccessibilityService for UI-tree reads, screenshots, taps, swipes, scrolling, text, keys,
  foreground inspection, app launch, and system-dialog handling.
- A private Unix-domain socket (`filesDir/ui.sock`) between the Rust `android_*` tools and the
  AccessibilityService broker.
- A CellClaw-derived floating chat circle with agent-state colors and capture-hide coordination.
- The ZeroClaw dashboard and generic Android skill bundled as APK assets.
- An optional in-process, public-key-only SSH server that carries the remote shell and a constrained
  encrypted tunnel to the otherwise loopback-only gateway.

## Architecture

```text
┌──────────────────────────── Android app UID ─────────────────────────────┐
│                                                                          │
│  libzeroclaw.so gateway/agent                                            │
│      │                                                                   │
│      ├── android_* tools ── filesDir/ui.sock ── AccessibilityService     │
│      │                                      ├── UI tree + foreground     │
│      │                                      ├── screenshot → vision      │
│      │                                      └── tap/swipe/type/launch    │
│      │                                                                   │
│      └── token-auth loopback bridge ── Android/GMS/ML Kit native APIs    │
│                                                                          │
│  MainActivity ── app-private provider prefs + generated ZeroClaw config  │
│  OverlayService ── local paired gateway chat                             │
└──────────────────────────────────────────────────────────────────────────┘

Optional paired client ── pubkey SSH tunnel ── loopback gateway + pairing ── phone agent
```

The Unix socket is intentionally phone-local. Network clients talk to the paired gateway; they do
not receive direct access to Accessibility or the socket.

## Full and lite flavors

| Flavor | Minimum Android | Difference |
|---|---:|---|
| `full` | Android 12 / API 31 | Includes Google AI Edge/AICore support for on-device Gemini Nano |
| `lite` | Android 11 / API 30 | Omits AI Edge; cloud providers and all Android tools remain available |

Both flavors are arm64-only, use application ID `org.zerodroid.bridge`, and cannot be installed
side by side.

## Safe first run

1. Install and open the APK.
2. Select a provider, enter a current credential, and run **Test provider key**.
3. Turn on **Phone tools (read-only)** and manually enable zerodroid in Android Accessibility.
4. Start the agent.
5. Enable **Autonomous control** only while supervising the device. It pre-approves the typed
   Android action tools; shell and file mutation remain separately gated.
6. Enable the floating circle only after granting **Draw over other apps**.
7. For remote access, paste an SSH public key and enable the encrypted SSH tunnel. The gateway never
   binds directly to Wi-Fi; forward local port 42617 as shown in the app, then pair normally.

Changing either Android capability switch restarts a running gateway so stale sessions cannot keep
an older policy.

## Development build

Prerequisites:

- Rust stable with target `aarch64-linux-android`
- Android NDK r28c and `cargo-ndk`
- JDK 17
- Android SDK platform/build-tools 36
- Node 24 for the dashboard

From the repository root:

```sh
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/28.2.13676358"
cargo ndk -t arm64-v8a build --release --locked -p zeroclaw --bin zeroclaw

cargo web build

apps/android/scripts/stage-jnilibs.sh
cd apps/android
./gradlew --no-daemon \
  :app:testLiteDebugUnitTest :app:testFullDebugUnitTest \
  :app:lintLiteDebug :app:lintFullDebug \
  :app:assembleLiteDebug :app:assembleFullDebug
```

Outputs:

```text
apps/android/app/build/outputs/apk/full/debug/app-full-debug.apk
apps/android/app/build/outputs/apk/lite/debug/app-lite-debug.apk
```

## Signed sideload build

`scripts/build-release.sh` fails closed unless release signing and the expected public certificate
fingerprint are supplied. It verifies:

- a clean checkout, then builds the dashboard and Rust binary from that exact revision;
- 16/64 KiB ELF LOAD alignment for every bundled arm64 library and APK 16 KiB zip alignment;
- full/lite unit tests, lint, and release assemblies;
- signing certificate, embedded binary hash, arm64-only native payload;
- bundled dashboard and the generic Android skill allowlist.

See [`release/README.md`](release/README.md) for the required environment variables and artifact
verification steps.

## Device verification

Use [`docs/06-on-device-verification.md`](docs/06-on-device-verification.md). The release gate has
been exercised on a physical arm64 Android 16 device through:

- clean install and same-certificate update;
- encrypted pairing persistence;
- paired WebSocket chat with Gemini;
- `android_ui_read` against a real foreground Settings screen;
- Accessibility grant persistence and autonomous-policy restart behavior.

Provider credentials, quota, OEM background restrictions, and manual Android grants remain
environment-specific. Provider preferences use Keystore-backed encryption and fail closed when the
OEM Keystore is unavailable. Generated config stores provider and gateway credentials only as
ZeroClaw-compatible `enc2:` ciphertext under a 0600 master key.

## Attribution

The Android packaging and bridge originated in zerodroid. Accessibility and overlay portions are
based on CellClaw and incorporated through this contribution. Zerodroid-origin portions remain
Apache-2.0. See [`NOTICE`](NOTICE).
