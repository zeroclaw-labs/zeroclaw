# macOS

Install, update, run as a LaunchAgent, and uninstall on macOS (Intel or Apple Silicon).

## Install

```sh
./install.sh
```

That is the whole install. Run it from a clone, or pipe it from `curl`:

<div class="os-tabs-src">

#### sh

```sh
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

</div>

When the platform maps to a supported prebuilt target, an interactive run offers prebuilt or source installation; other platforms build from source. Source installs also offer app and optional-feature choices. For an unconfigured install, the installer then offers CLI or browser-based setup. The piped `curl | bash` path is noninteractive: it selects a prebuilt binary when available, falls back to a source build otherwise, skips the setup prompt, and prints [`zeroclaw quickstart`](../getting-started/quickstart.md) as the next step. Both paths install to Cargo's bin directory, usually `~/.cargo/bin/zeroclaw`. Pass `--help` for the full flag reference, or `--skip-quickstart` to install only.

### Homebrew

<div class="os-tabs-src">

#### sh

```sh
brew install zeroclaw
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

Logs go to `~/.zeroclaw/logs/` (Homebrew installs: `$HOMEBREW_PREFIX/var/zeroclaw/logs/`):

<div class="os-tabs-src">

#### sh

```sh
tail -f ~/.zeroclaw/logs/daemon.stdout.log
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

# Default workspace (includes logs at ~/.zeroclaw/logs)
rm -rf ~/.zeroclaw ~/.config/zeroclaw
```

</div>

## Gotchas

- **Homebrew config path mismatch.** The `brew services` daemon reads config from `$HOMEBREW_PREFIX/var/zeroclaw/`, not `~/.zeroclaw/`. If your service is reading stale config, check which one the daemon sees and set `ZEROCLAW_WORKSPACE` accordingly.
- **First launch of the browser tool** downloads Chromium (~150 MB) via Playwright.
- **Apple Silicon and Intel:** the bootstrap script detects the architecture and uses a matching prebuilt release artifact when one is available. If the release has no matching artifact, it falls back to a source build. Homebrew selects the appropriate package for the host.

## Next

- [Service management](./service.md)
- [Quickstart](../getting-started/quickstart.md)
- [Operations → Overview](../ops/overview.md)
