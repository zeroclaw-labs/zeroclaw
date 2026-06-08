# Desktop app — testing notes

## macOS (current target)

### Reset to fresh-install state
```sh
pkill -f 'target/debug/zeroclaw-desktop'
rm "$HOME/Library/Application Support/ai.zeroclawlabs.desktop/settings.json"
tccutil reset All ai.zeroclawlabs.desktop      # for installed .app only — see notes
killall Dock                                   # if dock icon looks stale
bash dev/run-tauri-dev.sh
```

`tccutil reset` only matches by bundle id, which is set on the `.app`, not the dev binary. For real fresh-permission tests, build the `.app` first:
```sh
cd apps/tauri && cargo tauri build
cp -R target/release/bundle/macos/ZeroClaw.app /Applications/
xattr -dr com.apple.quarantine /Applications/ZeroClaw.app
tccutil reset All ai.zeroclawlabs.desktop
open /Applications/ZeroClaw.app
```

### What to verify in the wizard
- 8 steps render with progress dots
- Each Grant button either opens the right System Settings pane (deep-link via `x-apple.systempreferences:`) or fires the native macOS prompt (Screen Recording, Microphone, Camera, Input Monitoring)
- Status pills flip to Granted within 2s of toggling in System Settings (driven by the 2s polling loop in `onboarding/index.html`)
- "Start ZeroClaw" closes onboarding, opens the main dashboard window pointed at the gateway, and fires `POST /api/devices/me/capabilities` (best-effort)
- Quit + relaunch → wizard does not reappear; only the tray icon

### Known macOS-only pieces
- `apps/tauri/src/macos/permissions.rs` is `#[cfg(target_os = "macos")]`
- IOKit (`IOHIDCheckAccess`) for Input Monitoring
- `ApplicationServices.framework` (`AXIsProcessTrusted`) for Accessibility
- `CoreGraphics.framework` (`CGRequestScreenCaptureAccess`) for Screen Recording
- Swift CLI bridges (`swift -e`) for AVFoundation, UNUserNotificationCenter, SFSpeechRecognizer
- `osascript` bridge for Automation
- `open x-apple.systempreferences:...?Privacy_*` for deep-linking

## Linux

### What works now
- App builds with Linux-specific permission probes and onboarding sections.
- Onboarding is simplified to a short Linux flow (welcome + ready).
- Screen capture reports **denied** on Linux for now (capture is still macOS-only in the desktop capability layer):
  - **X11**: denied
  - **Wayland + xdg-desktop-portal available**: denied
  - **Wayland without portal**: denied
- Notifications report granted (no centralized Linux privacy gate).
- Bundle targets remain `.deb` and `.AppImage`.

### What to test
- Fresh `.deb` install on Ubuntu 22.04+ → onboarding shows simplified ~2-step flow → tray works
- Fresh `.AppImage` on Fedora/Arch → same
- Wayland host with portal: screen capture remains Denied (no Linux capture path yet)
- X11 host: screen capture remains Denied
- Notification appears via D-Bus

### How to attempt a build today (will compile, won't be useful)
```sh
cd apps/tauri
cargo build --release --target x86_64-unknown-linux-gnu   # needs cross toolchain
# Or build natively on a Linux box:
cargo tauri build
```

## Windows

### What works now
- App builds with Windows-specific permission probes and onboarding sections.
- Onboarding is Windows-focused: Mic + Camera + optional Input Monitoring + Notifications.
- Mic and Camera status are surfaced from CapabilityAccessManager consent-store keys.
- Requesting Mic/Camera opens the matching `ms-settings:` privacy pages.
- Input Monitoring status reflects admin elevation state.
- Notifications report granted for Action Center.
- Bundle targets remain `.exe` and `.msi`.

### What to test
- Fresh `.msi` install on Windows 11 → onboarding shows ~3–4-step Windows flow → system tray works
- Clicking Grant for Mic/Camera opens corresponding Privacy settings pages
- Toggling privacy settings updates status pills in the wizard polling loop
- Notifications appear in Action Center
- Non-admin run reports Input Monitoring as denied; admin run reports granted

### How to attempt a build today (will compile, won't be useful)
```sh
cd apps/tauri
cargo build --release --target x86_64-pc-windows-msvc     # needs cross toolchain
# Or build natively on Windows:
cargo tauri build
```

## CI matrix to add (separate issue)

```yaml
# Suggested when #6501 lands — run all three at minimum on cargo check
matrix:
  os: [macos-14, ubuntu-22.04, windows-2022]
```

## Capability sync end-to-end test (gateway-side)

Today the gateway's `POST /api/devices/me/capabilities` is implemented in this branch but the running gateway behind the SSH tunnel is an older build. To verify capabilities actually land in the DB:

```sh
# Run a local gateway from this branch
cargo run -p zeroclaw -- gateway

# In another terminal, walk the wizard, click "Start ZeroClaw"
# Then query the local devices.db (path depends on workspace config):
sqlite3 <workspace>/devices.db "SELECT id, capabilities FROM devices;"
# Expected: one row with a JSON array of granted permission names.
```

When the production gateway is rebuilt from this branch, the same query against the VPS DB will show the production Mac's capabilities.
