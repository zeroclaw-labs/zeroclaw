# Desktop app — testing notes

## Startup flow

The desktop app is a thin shell over a running ZeroClaw **web gateway**. There
is no longer a macOS/Windows/Linux permission-setup wizard — the app goes
straight to the gateway, and first-time setup happens in the web Quickstart.

On launch:

1. A small **splash** window (`apps/tauri/splash/index.html`) appears and polls
   the gateway's `/health` (via the `get_health` IPC command) every ~1.2s.
2. Once the gateway is healthy, the splash calls the `open_dashboard` command,
   which pairs with the gateway (when pairing is required), creates the **main**
   window pointed at the gateway **root** (`http://127.0.0.1:42617/`), seeds the
   bearer token via an initialization script, and closes the splash.
3. The web app's fresh-install redirect (`FreshInstallRedirect` in
   `web/src/App.tsx`) sends first-time users — no agents yet, Quickstart never
   completed — to `/quickstart`. Returning users land on the dashboard.

> The app looks for a gateway on `127.0.0.1:42617`: it reuses a running
> daemon, or spawns `zeroclaw daemon` itself (preferring a kernel bundled
> next to the app executable, then `PATH` and the common install dirs — see
> `src/daemon.rs::find_zeroclaw_binary`). The self-contained installer below
> bundles the kernel as a Tauri sidecar — the "full experience" distribution
> from architecture RFC fnd-001, D5.
>
> **Run `zeroclaw daemon`, not `zeroclaw gateway start`.** Both serve the
> dashboard on 42617, but only the daemon attaches the supervisor that powers
> in-place reload. After the Quickstart applies config it calls `/admin/reload`;
> a standalone `gateway start` has no supervisor and returns
> `503 "no daemon supervisor — running as standalone gateway"`, so the new agent
> won't go live until the process is restarted. The daemon hot-reloads instead.

## Self-contained build (bundled kernel)

The plain `cargo tauri build` produces an app that *finds* an installed
`zeroclaw`. To produce the zero-install artifact — double-click on a machine
with nothing pre-installed and get a running agent — bundle the kernel as a
sidecar:

```sh
# 1. Build the dashboard, then embed it in the staged kernel.
cargo web build
scripts/desktop/prepare-kernel.sh --features embedded-web
scripts/desktop/prepare-kernel.sh --target universal-apple-darwin --features embedded-web

# 2. Bundle with the sidecar overlay (adds bundle.externalBin).
cd apps/tauri && cargo tauri build --config tauri.bundled.conf.json
```

`ZEROCLAW_KERNEL_PATH` can reuse a prebuilt single-target kernel, but that
binary must already have been built with `--features embedded-web`; the staging
script cannot add embedded assets to an existing executable.

The overlay keeps the default config untouched, so `cargo tauri build`
without the staged kernel keeps working. Tauri places the sidecar next to the
app executable as `zeroclaw`, which is the first place
`find_zeroclaw_binary()` looks — so the bundled app starts its own daemon
from its own kernel.

To verify self-containment, launch on a machine (or shell) where `zeroclaw`
is not on `PATH` and not in `~/.cargo/bin`, then check the daemon's process
path points inside the app bundle:

```sh
pgrep -fl 'zeroclaw daemon'   # expect .../ZeroClaw.app/Contents/MacOS/zeroclaw
```

> Size note: the kernel dominates the artifact. A stripped release kernel is
> ~146 MB per arch (~55–65 MB compressed dmg); a universal (two-slice) kernel
> roughly doubles that. The unstripped dev kernel is ~228 MB — always let
> `prepare-kernel.sh` strip it.

## macOS (current target)

### Reset to fresh-install state
```sh
pkill -f 'target/debug/zeroclaw-desktop'
rm "$HOME/Library/Application Support/ai.zeroclawlabs.desktop/settings.json"
killall Dock                                   # if dock icon looks stale
bash dev/run-tauri-dev.sh
```

To exercise the full first-run path, also reset the gateway's config so the
Quickstart auto-launches (the gateway reports `quickstart_completed=false` and
an empty agents list via `GET /api/quickstart/state`).

For a real installed-bundle test:
```sh
cd apps/tauri && cargo tauri build
cp -R target/release/bundle/macos/ZeroClaw.app /Applications/
xattr -dr com.apple.quarantine /Applications/ZeroClaw.app
open /Applications/ZeroClaw.app
```

### What to verify
- With **no gateway running**: splash shows "Connecting to your ZeroClaw
  gateway…" and, after a few seconds, the "make sure the gateway is running"
  hint. The tray icon shows Disconnected.
- Start the daemon (`cargo run -p zeroclaw -- daemon`, or `zeroclaw daemon`):
  within ~1–2s the splash hands off — the dashboard window opens, splash closes.
- **First run** (fresh gateway config): the dashboard opens straight onto the
  **Quickstart**; completing it configures an agent and the gateway becomes
  usable. After completion, relaunching the app lands on the dashboard.
- **Returning run** (agent already configured): the dashboard opens on the
  normal dashboard, not the Quickstart.
- Quit from the tray → relaunch → splash → dashboard again (tray icon persists
  in the menu bar).

### Native command boundary

The Rust app still registers `take_screenshot` and `run_applescript`, but the
gateway-served main window receives no remote Tauri capability and cannot invoke
them. Exposing either command requires a separate, narrowly scoped approval and
ACL design.

## Linux / Windows

The app builds and runs the same splash → gateway → Quickstart flow. Bundle
targets are unchanged (`.deb`/`.AppImage` on Linux, `.exe`/`.msi` on Windows).
Screen capture and AppleScript capabilities remain macOS-only; the other
platforms register stubs that return an unsupported-platform error.

### How to build
```sh
cd apps/tauri
cargo tauri build          # native build on each platform
# Or cross-compile with the appropriate target + toolchain:
#   cargo build --release --target x86_64-unknown-linux-gnu
#   cargo build --release --target x86_64-pc-windows-msvc
```

## CI matrix to add (separate issue)

```yaml
# Suggested when #6501 lands — run all three at minimum on cargo check
matrix:
  os: [macos-14, ubuntu-22.04, windows-2022]
```
