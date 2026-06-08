# macOS

Install, update, run as a LaunchAgent, and uninstall on macOS (Intel or Apple Silicon).

## Install

`install.sh` is the preferred path; Homebrew is a reasonable alternative if you want `brew services` integration.

### Option 1: `install.sh` via curl (fastest)

<div class="os-tabs-src">

#### sh

```sh
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

</div>

### Option 2: `install.sh` from a clone

<div class="os-tabs-src">

#### sh

```sh
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

</div>

### What the installer does

1. Asks whether you want a prebuilt binary or to build from source
2. Installs to `~/.cargo/bin/zeroclaw`
3. Runs `zeroclaw quickstart` to complete first-time setup

Flags:

<div class="os-tabs-src">

#### sh

```sh
./install.sh --prebuilt                      # always prebuilt, skip the prompt
./install.sh --source                        # always build from source
./install.sh --minimal                       # foundation only, no default features
./install.sh --source --features agent-runtime,channel-discord   # custom features
./install.sh --skip-quickstart                  # install only; run `zeroclaw quickstart` later
./install.sh --list-features                 # print available features and exit
./install.sh --help                          # full flag reference
```

</div>

### Option 3: Homebrew

<div class="os-tabs-src">

#### sh

```sh
brew install zeroclaw
zeroclaw quickstart
```

</div>

Gets you `brew services` integration. Binary lives at `$HOMEBREW_PREFIX/bin/zeroclaw`.

**Workspace location gotcha:** with Homebrew, the service user and the CLI user may be different, so the workspace lives at `$HOMEBREW_PREFIX/var/zeroclaw/` rather than `~/.zeroclaw/`. Point CLI invocations at the same workspace:

<div class="os-tabs-src">

#### sh

```sh
export ZEROCLAW_WORKSPACE="$HOMEBREW_PREFIX/var/zeroclaw"
```

</div>

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

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service install   # writes ~/Library/LaunchAgents/com.zeroclaw.daemon.plist
zeroclaw service start
zeroclaw service status
```

</div>

Logs go to `~/Library/Logs/ZeroClaw/`:

<div class="os-tabs-src">

#### sh

```sh
tail -f ~/Library/Logs/ZeroClaw/zeroclaw.log
```

</div>

For Homebrew installs, prefer:

<div class="os-tabs-src">

#### sh

```sh
brew services start zeroclaw
brew services info zeroclaw
```

</div>

Both methods produce the same end state, a loaded LaunchAgent that starts on login. Pick one and stick with it.

Full details: [Service management](./service.md).

## Update

Re-run the installer, it detects the existing install and upgrades in place:

<div class="os-tabs-src">

#### sh

```sh
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash -s -- --skip-quickstart
zeroclaw service restart
```

</div>

Or from a clone:

<div class="os-tabs-src">

#### sh

```sh
cd /path/to/zeroclaw
git pull
./install.sh --skip-quickstart
zeroclaw service restart
```

</div>

If installed via Homebrew instead:

<div class="os-tabs-src">

#### sh

```sh
brew update && brew upgrade zeroclaw
brew services restart zeroclaw
```

</div>

## Uninstall

<div class="os-tabs-src">

#### sh

```sh
# stop and unregister the service
zeroclaw service stop
zeroclaw service uninstall

# Homebrew
brew uninstall zeroclaw

# bootstrap / cargo
rm ~/.cargo/bin/zeroclaw
```

</div>

Remove config and workspace (optional: this deletes conversation history):

<div class="os-tabs-src">

#### sh

```sh
# Homebrew workspace
rm -rf "$HOMEBREW_PREFIX/var/zeroclaw"

# Default workspace
rm -rf ~/.zeroclaw ~/.config/zeroclaw

# Logs
rm -rf ~/Library/Logs/ZeroClaw
```

</div>

## Gotchas

- **Homebrew config path mismatch.** The `brew services` daemon reads `$HOMEBREW_PREFIX/var/zeroclaw/config.toml`, not `~/.zeroclaw/config.toml`. If your service is reading stale config, check which one the daemon sees and set `ZEROCLAW_WORKSPACE` accordingly.
- **First launch of the browser tool** downloads Chromium (~150 MB) via Playwright.
- **Apple Silicon** and **Intel** builds are both released. The bootstrap script auto-detects. Homebrew auto-selects.

## Next

- [Service management](./service.md)
- [Quickstart](../getting-started/quickstart.md)
- [Operations → Overview](../ops/overview.md)
