# macOS

Install, update, run as a LaunchAgent, and uninstall on macOS (Intel or Apple Silicon).

## Install

`install.sh` is the preferred path; Homebrew is a reasonable alternative if you want `brew services` integration.

### Option 1 — `install.sh` via curl (fastest)

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

### Option 2 — `install.sh` from a clone

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

### What the installer does

1. Asks whether you want a prebuilt binary or to build from source
2. Installs to `~/.cargo/bin/zeroclaw`
3. Runs `zeroclaw onboard` to complete first-time setup

Flags:

```bash
./install.sh --prebuilt                      # always prebuilt, skip the prompt
./install.sh --source                        # always build from source
./install.sh --minimal                       # kernel only (~6.6 MB)
./install.sh --source --features agent-runtime,channel-discord   # custom features
./install.sh --skip-onboard                  # install only; run `zeroclaw onboard` later
./install.sh --list-features                 # print available features and exit
./install.sh --help                          # full flag reference
```

### Option 3 — Homebrew

```bash
brew install zeroclaw
zeroclaw onboard
```

Gets you `brew services` integration. Binary lives at `$HOMEBREW_PREFIX/bin/zeroclaw`.

**Workspace location gotcha:** with Homebrew, the service user and the CLI user may be different, so the workspace lives at `$HOMEBREW_PREFIX/var/zeroclaw/` rather than `~/.zeroclaw/`. Point CLI invocations at the same workspace:

```bash
export ZEROCLAW_WORKSPACE="$HOMEBREW_PREFIX/var/zeroclaw"
```

Add that to your shell profile if you want it permanent.

## System dependencies

Most features work with a stock macOS install. Optional extras:

| Feature | Install |
|---|---|
| Docs translation | `brew install gettext` |
| Browser tool | Playwright pulls Chromium automatically on first use |
| Hardware | No native GPIO on macOS; use a USB peripheral like Aardvark. See [Hardware → Aardvark](../hardware/aardvark.md) |
| iMessage channel | Requires macOS 11+. See [Channels → Other chat platforms](../channels/chat-others.md) |

## Running as a service

```bash
zeroclaw service install   # writes ~/Library/LaunchAgents/com.zeroclaw.daemon.plist
zeroclaw service start
zeroclaw service status
```

Logs go to `~/Library/Logs/ZeroClaw/`:

```bash
tail -f ~/Library/Logs/ZeroClaw/zeroclaw.log
```

For Homebrew installs, prefer:

```bash
brew services start zeroclaw
brew services info zeroclaw
```

Both methods produce the same end state — a loaded LaunchAgent that starts on login. Pick one and stick with it.

Full details: [Service management](./service.md).

## Update

Re-run the installer — it detects the existing install and upgrades in place:

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash -s -- --skip-onboard
zeroclaw service restart
```

Or from a clone:

```bash
cd /path/to/zeroclaw
git pull
./install.sh --skip-onboard
zeroclaw service restart
```

If installed via Homebrew instead:

```bash
brew update && brew upgrade zeroclaw
brew services restart zeroclaw
```

## Uninstall

```bash
# stop and unregister the service
zeroclaw service stop
zeroclaw service uninstall

# Homebrew
brew uninstall zeroclaw

# bootstrap / cargo
rm ~/.cargo/bin/zeroclaw
```

Remove config and workspace (optional — this deletes conversation history):

```bash
# Homebrew workspace
rm -rf "$HOMEBREW_PREFIX/var/zeroclaw"

# Default workspace
rm -rf ~/.zeroclaw ~/.config/zeroclaw

# Logs
rm -rf ~/Library/Logs/ZeroClaw
```

## Gotchas

- **Homebrew config path mismatch.** The wizard warns if it detects Homebrew — the `brew services` daemon reads `$HOMEBREW_PREFIX/var/zeroclaw/config.toml`, not `~/.zeroclaw/config.toml`. If your service is reading stale config, check which one the daemon sees.
- **First launch of the browser tool** downloads Chromium (~150 MB) via Playwright.
- **Apple Silicon** and **Intel** builds are both released. The bootstrap script auto-detects. Homebrew auto-selects.

## Next

- [Service management](./service.md)
- [Quick start](../getting-started/quick-start.md)
- [Operations → Overview](../ops/overview.md)
