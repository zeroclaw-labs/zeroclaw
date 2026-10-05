# Desktop app — testing notes

## Startup flow

The desktop app is a thin shell over a running ZeroClaw **web gateway**. There
is no longer a macOS/Windows/Linux permission-setup wizard — the app goes
straight to the gateway, and first-time setup happens in the web Quickstart.

On launch:

1. A small **splash** window (`apps/tauri/splash/index.html`) appears and polls
   the app's startup state (via the `get_startup` IPC command) every ~1.2s.
2. Once startup is ready, the splash calls the `open_dashboard` command,
   which pairs with the gateway (when pairing is required), creates the **main**
   window pointed at the gateway **root** (`http://127.0.0.1:42617/`), seeds the
   bearer token via an initialization script, and closes the splash.
3. The web app's fresh-install redirect (`FreshInstallRedirect` in
   `web/src/App.tsx`) sends first-time users — no agents yet, Quickstart never
   completed — to `/quickstart`. Returning users land on the dashboard.

> The app looks for a gateway on `127.0.0.1:42617`: it reuses a running daemon, or starts the discovered kernel with `zeroclaw service run-desktop-daemon --port 42617` (preferring a kernel bundled next to the app executable, then `PATH` and the common install dirs — see `src/daemon.rs::find_zeroclaw_binary`). Before launch, Desktop verifies that the kernel accepts this supervisor command. An externally installed kernel must therefore support the Desktop supervisor command; an older unsupported kernel produces an actionable startup error instead of falling back to an uncaptured daemon. The self-contained installer below bundles the matching kernel as a Tauri sidecar — the "full experience" distribution from architecture RFC fnd-001, D5.
>
> **Run `zeroclaw daemon`, not `zeroclaw gateway start`.** Both serve the
> dashboard on 42617, but only the daemon attaches the supervisor that powers
> in-place reload. After the Quickstart applies config it calls `/admin/reload`;
> a standalone `gateway start` has no supervisor and returns
> `503 "no daemon supervisor — running as standalone gateway"`, so the new agent
> won't go live until the process is restarted. The daemon hot-reloads instead.

### Readiness when the app starts its own daemon

When the kernel supports it (the app checks with
`zeroclaw service run-desktop-daemon --rpc-readiness --help`), the app launches
the supervisor with `--rpc-readiness`. The supervisor then:

1. resolves the daemon's RPC endpoint with the kernel's own resolver;
2. passes it to the daemon as `ZEROCLAW_SOCKET`;
3. reports `READY {"endpoint":…,"pid":…}` only once that endpoint accepts
   connections served by the daemon it started.

The app then dials that endpoint. On Unix it first checks that the endpoint is
served by the app's own OS account. It completes the RPC handshake and checks
the protocol version. For a kernel bundled beside the app, it also checks that
the kernel's version equals the app's. Only then does it wait, up to 60
seconds in all, for the dashboard's gateway. In this mode the supervisor pins
the daemon's gateway to `127.0.0.1` (`--host 127.0.0.1`). Over its verified
RPC connection, the app asks for a fresh possession challenge for the
dashboard address (`127.0.0.1:42617`). The listener must answer that challenge
on the HTTP connection the app opened there. A successful proof admits the
core's own listener or a registered external listener, regardless of the
core's own gateway diagnostics. RPC `health` still reports
`components.gateway.bound_addr` for the core's own listener; that report only
explains a failed proof. Process IDs reported over HTTP shape failure messages
and never admit an address.

The wait polls every second. Three consecutive answers without the proof
(`PROOF_MISMATCHES_HELD = 3`) end startup as `port_held`, and the app stops the
daemon it launched. A successful proof admits immediately; no own-gateway diagnostic
overrides it.

The app records the outcome as its startup state. The splash polls that state
and opens the dashboard only once it is ready, and `open_dashboard` itself
refuses, before and after pairing, unless it is. With a launched core, the
pairing requirement comes from the listener's proven `/health` response;
missing flags and failed proofs refuse opening. Every new dashboard window
also requires a fresh proof, even when pairing hands over no token: the
WebView may already hold credentials for that origin.

On Unix, native credential requests carry their proof and credential on the
same HTTP connection. The WebView opens its own connections from its first
navigation onward, so those connections are outside this proof. Windows
cannot yet verify the RPC pipe server's account. A process already trusted as
a local administrator can register a chosen listener key; when several live,
admitted registrations name one address, the most recent is vouched for.

An app that finds a gateway already running when it starts reuses it, as
before.

Each way this can fail shows its own message on the splash, and the dashboard
does not open:

| Splash kind | Cause |
|---|---|
| `endpoint_held` | Another process already serves the endpoint, or serves it as another account. |
| `incompatible` | The protocol differs, a bundled kernel's version differs from the app's, or the core predates the possession proof. |
| `port_held` | A failed proof is explained by the core's bind/address error, or three consecutive answers lack the proof. |
| `core_unavailable` | The core stopped answering before its gateway was ready. |
| `timeout` | A deadline passed: the kernel's capability check, the supervisor's readiness report (60 seconds with `--rpc-readiness`, 10 without), or the dashboard's `/health` after it. |

During initial readiness, the app stops the daemon it started on every
failure except a gateway timeout, where the daemon may still finish starting;
reopening the app then reuses it. A supervisor that never reports readiness is stopped.

A kernel without `--rpc-readiness` rejects the flag in that check. The app then
uses the original `READY` line and waits for `/health` with the same deadline.
Older apps never pass the flag, so a newer kernel answers them with the exact
`READY` line they expect.

The desktop supervisor writes combined daemon stdout and stderr to `<config-dir>/logs/zeroclaw-desktop-daemon.log`, where `<config-dir>` follows canonical config resolution precedence: `ZEROCLAW_CONFIG_DIR`, then `ZEROCLAW_DATA_DIR`, then deprecated `ZEROCLAW_WORKSPACE`, then Homebrew/default resolution. The capture is capped at 8 MiB and retains the newest tail when the cap is crossed.

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
- Inspect `<config-dir>/logs/zeroclaw-desktop-daemon.log` after startup to verify combined stdout/stderr capture; the file stays at or below 8 MiB and keeps the newest tail during continuous output.

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
