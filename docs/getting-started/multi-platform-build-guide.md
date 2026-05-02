# Multi-Platform Build Guide — Windows / macOS / Android / iOS

End-to-end recipes for producing installable artifacts of MoA + ZeroClaw across the four platforms most commonly used for cross-device memory-sync and SLM (Gemma family) testing.

## Audience and scope

This guide is targeted at maintainers and power users who want to:

- Install MoA on their **own** Windows desktop and macOS laptop as production-style apps.
- Install MoA on their **own** Android phone and iOS Simulator for cross-device testing.
- Run an SLM (e.g. Gemma family via Ollama) on each device and verify that the personal memory layer ("first brain" / "second brain") synchronizes across devices.

It is **not** a release-engineering guide. For tagged releases and CI-driven artifacts, see [`../release-process.md`](../release-process.md).

## Build-mode policy used here

| Platform | Build mode | Reason |
|----------|-----------|--------|
| Windows (Tauri) | `release` | Long initial compile is acceptable once; you install the result. |
| macOS (Tauri) | `release` | Same as Windows. |
| Android | `debug` | Iteration-heavy; sideloaded; no signing required. |
| iOS Simulator | `debug` | Simulator only accepts simulator-target builds; no signing. |

Switch any of these to the opposite mode by following the per-platform notes — the build scripts in this repo accept both.

---

## 1) Windows — Tauri release + MSI/NSIS installer

This is the canonical "production install" path for Windows.

### 1.1 Prerequisites

Run from PowerShell (admin not required for these except where noted):

| Requirement | Verify | Install |
|-------------|--------|---------|
| Rust toolchain (1.81+) | `cargo --version` | `winget install Rustlang.Rustup` then `rustup default stable` |
| Node.js (LTS) | `node --version` | `winget install OpenJS.NodeJS.LTS` |
| MSVC C++ Build Tools | "Visual Studio Installer" lists "Desktop development with C++" | Install [Build Tools for Visual Studio](https://visualstudio.microsoft.com/downloads/) → check **"Desktop development with C++"** workload (admin) |
| WebView2 runtime | Built into Windows 11; on 10 install [Evergreen runtime](https://developer.microsoft.com/microsoft-edge/webview2/) | One-shot installer |
| Git Bash | `bash --version` | Comes with Git for Windows |

> Tauri on Windows targets MSVC; the GNU toolchain is not supported. Verify with `rustup show` that the active toolchain is `stable-x86_64-pc-windows-msvc`.

### 1.2 Build

From the repo root in **Git Bash** (the shell script uses POSIX syntax):

```bash
bash scripts/build-tauri.sh
```

What the script does:

1. `cargo build --release` — produces `target/release/zeroclaw.exe`.
2. Copies the binary to `clients/tauri/src-tauri/binaries/zeroclaw-<host-triple>.exe` as a Tauri sidecar.
3. `npm install` (first run only) inside `clients/tauri/`.
4. `npx tauri build` — produces the installer bundle.

Expected first-run cost: 30–40 minutes on a typical laptop. Incremental rebuilds: 3–6 minutes.

### 1.3 Artifacts

After a successful run:

```
clients/tauri/src-tauri/target/release/bundle/
├── msi/MoA - Master of AI_0.1.0_x64_en-US.msi
└── nsis/MoA - Master of AI_0.1.0_x64-setup.exe
```

Either installer is fine. MSI is preferred for managed environments; NSIS is smaller and faster to run.

### 1.4 Install

Double-click the MSI or `-setup.exe`. The app installs under `C:\Program Files\MoA - Master of AI\` and adds a Start Menu entry. Uninstall via Settings → Apps.

### 1.5 Debug variant

```bash
bash scripts/build-tauri.sh --debug
```

Produces a debug build under `clients/tauri/src-tauri/target/debug/bundle/`. Use when you want faster compiles and richer panic traces.

---

## 2) macOS — Tauri release + .dmg

Cross-compiling macOS bundles from Windows is **not** supported (codesign + Apple SDK constraints). This step must run on a Mac.

### 2.1 Prerequisites (on the Mac)

```bash
# Xcode CLI tools
xcode-select --install

# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Node.js (Homebrew)
brew install node
```

### 2.2 Clone and build

```bash
git clone https://github.com/Kimjaechol/MoA_new.git
cd MoA_new
bash scripts/build-tauri.sh
```

The host triple is auto-detected:

- Apple Silicon → `aarch64-apple-darwin`
- Intel → `x86_64-apple-darwin`

To build a **universal** binary that runs on both architectures:

```bash
rustup target add x86_64-apple-darwin aarch64-apple-darwin
bash scripts/build-tauri.sh --target universal-apple-darwin
```

### 2.3 Artifact and install

```
clients/tauri/src-tauri/target/release/bundle/dmg/MoA - Master of AI_0.1.0_*.dmg
```

Open the `.dmg` and drag the MoA icon into Applications.

> **First launch on macOS:** Because this build is not Apple-notarized, Gatekeeper will block it. Resolve via **System Settings → Privacy & Security → "Open Anyway"** for the MoA app, or run once: `xattr -dr com.apple.quarantine "/Applications/MoA - Master of AI.app"`.

### 2.4 Update / uninstall lifecycle

See [`macos-update-uninstall.md`](macos-update-uninstall.md) for the standard tear-down flow.

---

## 3) Android — debug binary or debug APK

There are two supported Android paths in this repo. **Pick one** based on what you want to test.

| Path | Command surface | UI | When to choose |
|------|-----------------|----|----------------|
| **A. Termux + ZeroClaw binary** | CLI only (the same `zeroclaw` you run on desktop) | None | Memory-sync and SLM testing — fastest iteration |
| **B. Native MoA Android app (`clients/android/`)** | Embedded ZeroClaw + native UI | Full Android UI | UI dogfooding, end-user-style testing |

For the cross-device-memory + SLM test scenario in this guide, **Path A is the recommended starting point**. Switch to Path B once you want to verify the Android UI flows.

### 3.1 Path A — Termux + cross-compiled ZeroClaw binary

Build on Windows (or macOS/Linux), transfer the binary to Termux, run.

#### 3.1.1 Cross-compile on Windows

```bash
# One-time setup
rustup target add aarch64-linux-android   # 64-bit Android (most modern phones)
cargo install cross                        # NDK-aware cargo wrapper

# Build
cross build --release --target aarch64-linux-android
# Output: target/aarch64-linux-android/release/zeroclaw
```

For 32-bit phones substitute `armv7-linux-androideabi`.

> `cross` runs the NDK toolchain inside a container, which avoids manual `ANDROID_NDK_HOME` setup. Docker Desktop must be running on Windows for `cross` to work.

#### 3.1.2 Install Termux on the phone

Install Termux from **F-Droid** (the Play Store version is outdated — see [`../android-setup.md`](../android-setup.md) for the warning).

#### 3.1.3 Transfer and run

```bash
# In Termux
termux-setup-storage
# Place the binary into ~/storage/shared via USB transfer or cloud sync
cp ~/storage/shared/zeroclaw $PREFIX/bin/
chmod +x $PREFIX/bin/zeroclaw
zeroclaw --version
zeroclaw onboard
```

Reference: [`../android-setup.md`](../android-setup.md) covers Termux specifics, `termux-services` for daemonization, and the on-device build path if you prefer to compile inside Termux.

### 3.2 Path B — Native MoA Android app (APK)

#### 3.2.1 Prerequisites

- Android Studio with Android SDK (API 34 recommended).
- Java 17 (bundled with Android Studio).
- Optional for command-line builds: ensure `ANDROID_HOME` is set.

#### 3.2.2 Build

Open `clients/android` in Android Studio and run **Build → Build Bundle(s) / APK(s) → Build APK(s)**. Or from the CLI:

```bash
cd clients/android
./gradlew assembleDebug
```

Artifact:

```
clients/android/app/build/outputs/apk/debug/app-debug.apk
```

#### 3.2.3 Install on the device

```bash
# Enable USB debugging on the phone (Settings → About → tap Build number 7 times → Developer options → USB debugging)
adb install clients/android/app/build/outputs/apk/debug/app-debug.apk
```

Or transfer the APK to the phone and tap to install (requires "Install unknown apps" permission for the file manager).

---

## 4) iOS Simulator — debug build

This step requires a Mac with Xcode 15+. Windows cannot build iOS targets.

### 4.1 Prerequisites (on the Mac)

```bash
# Xcode (install via App Store first)
xcode-select --install

# Simulator target
rustup target add aarch64-apple-ios-sim

# Real-device target (only if you also plan to install on a physical iPhone)
rustup target add aarch64-apple-ios
```

### 4.2 Build the static library + Xcode project

The repo provides `scripts/build-ios.sh`:

```bash
# Default = simulator debug
bash scripts/build-ios.sh

# Library only (skip the Xcode project step)
bash scripts/build-ios.sh lib-only

# Real-device archive (release)
bash scripts/build-ios.sh release
```

What the simulator-debug path does:

1. `cargo build --target aarch64-apple-ios-sim` inside `clients/ios-bridge/`.
2. Produces `libzeroclaw_ios.a`.
3. Builds the Xcode project at `clients/ios/MoA.xcodeproj` for the iPhone 16 simulator.

### 4.3 Run in the Simulator

Easiest path: open `clients/ios/MoA.xcodeproj` in Xcode, choose a simulator from the device picker, and press **Run** (`Cmd + R`).

CLI install into a booted simulator:

```bash
xcrun simctl install booted clients/ios/build/Debug-iphonesimulator/MoA.app
xcrun simctl launch booted com.moa.agent
```

### 4.4 Real-device install (release)

`bash scripts/build-ios.sh release` produces an `.xcarchive`. Export an `.ipa` with:

```bash
xcodebuild -exportArchive \
  -archivePath clients/ios/build/MoA.xcarchive \
  -exportPath clients/ios/build/ \
  -exportOptionsPlist clients/ios/ExportOptions.plist
```

Real-device sideloading requires a paid Apple Developer account and provisioning profiles. For a personal Apple ID, the 7-day free signing flow via Xcode → Devices & Simulators is sufficient.

---

## 5) Cross-device test scenario — memory sync + SLM

Once at least two of the four platforms are installed, you can verify the cross-device behavior.

### 5.1 Install Ollama and pull the SLM on each device

Verify the exact tag with `ollama search gemma` before pulling — the Gemma series is updated frequently and the tag string in this section is illustrative, not pinned.

```bash
# Windows / macOS
# Install Ollama from https://ollama.com (one-shot installer)
ollama pull gemma3:4b   # adjust to whichever Gemma variant you intend to test
ollama pull gemma2:2b   # smaller fallback for the phone
```

For Termux on Android, prefer the smaller variant — 4B-class models are routinely too large for phone RAM. Run Ollama via the Termux package or a containerized fallback as documented in [`../android-setup.md`](../android-setup.md).

### 5.2 Confirm the sync backend before testing

The "first brain / second brain" pair is implemented inside the `vault` module of this repo. Before you build a cross-device test plan, identify which sync transport is configured:

- Local-only on-disk SQLite (no sync).
- Filesystem sync via a cloud-mounted folder (iCloud Drive, Nextcloud, etc.).
- Network-based sync (R2/S3, P2P, custom server).

The transport choice changes which devices can actually see each other's memories, what credentials must be on each device, and what failure modes you should look for. See `src/vault/` and `docs/reference/` for the current authoritative answer.

### 5.3 Suggested smoke test

1. **Device A (Windows)** — open MoA, write a note: *"Tomorrow at 3 PM I'm presenting the Q2 retrospective."*
2. Wait for sync (or trigger it manually if a sync command exists).
3. **Device B (Android Termux)** — `zeroclaw memory list` and confirm the note appears.
4. **Device C (iOS Simulator)** — open the MoA app and ask the local Gemma model: *"What's on my schedule tomorrow?"* Verify the model cites the note from Device A.
5. Edit the note on Device B; verify Device A and C reflect the edit after sync.
6. Disconnect Device B from the network, edit on Device A, reconnect Device B — verify conflict resolution behavior matches expectations.

---

## 6) Recommended ordering for a first-time pass

If you are starting from zero and want the lowest total time-to-test:

1. **Windows Tauri release** — start the build first; it dominates wall time.
2. While it compiles, decide between Android Path A vs Path B and install Termux or Android Studio accordingly.
3. After Windows is installed, install Ollama + pull Gemma; do a single-device sanity check.
4. **Android Path A** — fastest to a second device; gives you a real cross-device sync test.
5. **macOS** — only when you reach the Mac; same `bash scripts/build-tauri.sh` flow.
6. **iOS Simulator** — last; useful for verifying the iOS UI surface but contributes no new sync transport.

---

## 7) Troubleshooting hints

| Symptom | Likely cause | Where to look |
|---------|--------------|---------------|
| `link.exe not found` on Windows | MSVC C++ Build Tools missing | Section 1.1 |
| `tauri: command not found` | `npm install` not run inside `clients/tauri/` | The script does this automatically; if you ran cargo manually, run `npm install` once |
| Tauri bundle missing on macOS | DMG step needs `create-dmg`; install via `brew install create-dmg` | Re-run script after install |
| `cross` fails on Windows | Docker Desktop not running | Start Docker Desktop, retry |
| Android APK installs but crashes on launch | ABI mismatch between bundled native lib and device | Check `clients/android/app/src/main/jniLibs/` ABI folders |
| iOS Simulator shows blank screen | Simulator did not finish booting before install | `xcrun simctl bootstatus booted -b` then retry |
| Gemma reply ignores synced memory | Sync did not run, or vault transport mismatch | Section 5.2 — verify transport first |

For repository-wide troubleshooting, see [`../troubleshooting.md`](../troubleshooting.md).

---

## 8) Related references

- [`../android-setup.md`](../android-setup.md) — deeper Termux + on-device Android build details.
- [`../one-click-bootstrap.md`](../one-click-bootstrap.md) — non-build operator setup.
- [`macos-update-uninstall.md`](macos-update-uninstall.md) — macOS lifecycle.
- [`../release-process.md`](../release-process.md) — tagged-release workflow (this guide is for personal/dev installs, not releases).
- [`../operations/README.md`](../operations/README.md) — runtime operations once installed.
