# Troubleshooting

Common failure modes, in the order you're likely to encounter them.

First stop for any issue:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw doctor
```

</div>

Runs a series of checks and prints a summary. Most of what follows is the detailed version of what `doctor` flags.

---

## Install-time

### `cargo` not found

<div class="os-tabs-src">

#### sh

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

</div>

Or pass `--prebuilt` to `install.sh` / `setup.bat` to skip Rust entirely.

### Missing build dependencies (Linux)

Install the baseline toolchain for your distro, then re-run `./install.sh`:

<div class="os-tabs-src">

#### Debian/Ubuntu

```sh
sudo apt install build-essential pkg-config
```

#### Fedora/RHEL

```sh
sudo dnf group install development-tools && sudo dnf install pkg-config
```

#### Arch

```sh
sudo pacman -S base-devel
```

</div>

Full per-distro list: [Setup → Linux](../setup/linux.md).

### Build OOMs on low-RAM hosts

Compiling ZeroClaw from source needs ~2 GB RAM at peak. On a 512 MB Raspberry Pi, you will OOM.

Options:

1. **Use a prebuilt**: `./install.sh --prebuilt` skips the toolchain and downloads from GitHub Releases
2. **Cross-compile on a bigger machine and copy the binary**
3. **Serialise the build**: `CARGO_BUILD_JOBS=1 cargo build --release --locked`
4. **Add swap** (works for RAM, costs disk, check you have both)

### Build is very slow

The Matrix E2EE stack (`matrix-sdk`, `ruma`, `vodozemac`) and TLS/crypto native deps (`aws-lc-sys`, `ring`) are the main cost. Opt out if you don't need them:

<div class="os-tabs-src">

#### sh

```sh
cargo build --release --locked --no-default-features --features "default-lean"
```

</div>

Or check what's happening:

<div class="os-tabs-src">

#### sh

```sh
cargo check --timings
# report at target/cargo-timings/cargo-timing.html
```

</div>

### `zeroclaw: command not found` after install

`cargo install` puts binaries in `~/.cargo/bin/`. Add to PATH:

<div class="os-tabs-src">

#### sh

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

</div>

Persist in your shell profile.

---

## Quickstart

### Quickstart won't overwrite an existing config

`zeroclaw quickstart` does not have a `--force` flag, it intentionally leaves an existing install alone. To run a fresh quickstart on a stale install, delete the directory and start over:

<div class="os-tabs-src">

#### sh

```sh
rm -rf ~/.zeroclaw
zeroclaw quickstart
```

</div>

Or, to edit a single stale field instead of wiping everything, use `zeroclaw config set <key>=<value>` directly.

### Homebrew install: config path mismatch

Homebrew installs prefer `$HOMEBREW_PREFIX/var/zeroclaw/` (so `brew services` works) while the default config dir is `~/.zeroclaw/`. Set `ZEROCLAW_WORKSPACE` to the Homebrew path before running quickstart so the two paths line up:

<div class="os-tabs-src">

#### sh

```sh
export ZEROCLAW_WORKSPACE="$HOMEBREW_PREFIX/var/zeroclaw"
zeroclaw quickstart
```

</div>

Or manually symlink once:

<div class="os-tabs-src">

#### sh

```sh
ln -s "$HOMEBREW_PREFIX/var/zeroclaw" ~/.zeroclaw
```

</div>

---

## Runtime

### OpenAI Codex subscription auth warns about config or streaming

Symptoms:

- The agent's `model_provider = "openai.<alias>"` points at a Codex entry, but runs still feel misconfigured
- Config loading warns about unknown top-level fields like `api_key` / `api_url` (those belong on the provider entry, not at the file root)
- Agent logs `provider streaming failed, falling back to non-streaming chat`

Checks (substitute `<alias>` with the configured agent alias from `[agents.<alias>]`):

For an OpenAI Codex subscription, set `requires_openai_auth = true` on the provider alias and leave `api_key` unset; the runtime uses the stored Codex login. Get the subscription credential from the vendor's own login flow. See [Provider Configuration → OAuth and subscription auth](../providers/configuration.md#oauth-and-subscription-auth) for the full credential model. Then test:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw agent -a <alias> -m "hello"
```

</div>

Notes:

- `requires_openai_auth = true` on the alias (with `api_key` unset) selects the subscription path; surround it with the canonical agent + risk profile from the [Minimal working example](../providers/configuration.md#minimal-working-example).
- `api_key` / `uri` on the alias entry are only needed for custom OpenAI-compatible gateways or other explicit endpoint overrides.
- The streaming-disabled warning by itself is not an auth failure; ZeroClaw retries the request in non-streaming mode.

### Daemon starts, then immediately exits

Check journald / the platform log (see [Logs & observability](./observability.md)) for the actual error. Common causes:

- **Invalid config**: `zeroclaw config list` to print resolved values, `zeroclaw config schema` to see the expected shape
- **Port conflict**: another process on `42617`; change `[gateway] port` or free the port
- **Missing secrets**: encrypted secrets store can't decrypt because the key file is gone; restore from backup or re-run onboarding

### Daemon keeps restarting

`systemctl --user status zeroclaw` shows the last exit. If it's a config error, it stopped restarting (exit 2) and you need to fix the config. If it's a panic, the unit retries every 10 s.

Enable debug logging and catch the next failure:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service stop
RUST_LOG=debug zeroclaw daemon
```

</div>

### Gateway unreachable

<div class="os-tabs-src">

#### sh

```sh
curl -sv http://localhost:42617/health
```

</div>

If connection refused: daemon isn't running, or it's bound to a different interface. Check `[gateway] host` / `port` in config.

If 403 / 401: pairing not completed or token expired. Run the pairing flow again.

---

## Channels

### Telegram: `terminated by other getUpdates request`

Two processes are polling the same bot token. Telegram only allows one poller at a time.

Fix: stop all but one `zeroclaw daemon` / `zeroclaw channel start` using that token.

### Discord / Slack auth failures

Discord tokens expire if you regenerate them in the Developer Portal. Slack bot tokens don't expire but can be revoked. Check the bot is still installed in the target workspace/guild.

For either:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw channel doctor discord
zeroclaw channel doctor slack
```

</div>

### Matrix: "unknown device"

If you re-onboarded without keeping device keys, the homeserver sees a new device that hasn't been verified. Re-verify from another logged-in client, or reset the key store:

<div class="os-tabs-src">

#### sh

```sh
rm -rf ~/.zeroclaw/workspace/matrix-crypto
# re-run pairing flow on next channel start
```

</div>

### IMAP polling stopped

Most often an auth failure, provider rotated the password or the app-password expired. Check:

<div class="os-tabs-src">

#### sh

```sh
journalctl --user -u zeroclaw -n 200 | grep -i imap
```

</div>

---

## Providers

### "Connection timed out" to Ollama

- Ollama daemon not running: `systemctl status ollama` (Linux), `brew services list` (macOS)
- Wrong URL in config, from inside a container, `localhost:11434` doesn't reach the host; use `host.docker.internal` or the host's LAN IP
- Firewall blocking port 11434, rare locally, common on shared LANs

### Anthropic / OpenAI 401

API key invalid or expired. Regenerate at the provider's dashboard, update in `[providers.models.<name>] api_key`, restart the service.

If using OAuth (`sk-ant-oat*`), the OAuth token may have expired. OAuth-issued tokens are longer-lived but not infinite. Re-authenticate.

---

## Tools

### Shell commands "blocked by policy"

Expected behaviour at `Supervised` autonomy for unknown commands. Either:

- Approve inline when prompted
- Add the command to `[autonomy] allowed_commands`
- Raise autonomy to `Full` if you trust the context

See [Security → Autonomy levels](../security/autonomy.md).

### Tool invocations fail inside Docker sandbox

- Container image isn't pulled, run `docker pull <image>` for whatever you have configured under `[security.sandbox].image` (default: `alpine:latest`)
- Docker daemon not reachable from the ZeroClaw user, check `docker info`
- Tool needs a device that's not passed through, extend `allow_devices`

### Browser tool hangs on first use

Playwright downloads Chromium (~150 MB) on first launch. Let it finish. If it keeps hanging, check disk space and proxy config.

---

## Service mode

### Service installed but shows inactive

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service start
zeroclaw service status
```

</div>

Use `zeroclaw service logs` to tail the installed service logs. Add `--follow` to stream new entries or `--lines <count>` to change how much history is shown. If the wrapper is unavailable or you need to inspect the platform directly, use:

- Linux: `journalctl --user -u zeroclaw.service -f`
- macOS: `log stream --predicate 'process == "zeroclaw"'`
- If you are running `zeroclaw daemon` directly in a terminal, use that foreground output instead of service log commands.

If that succeeds interactively but the service dies in the background, it's almost always config or permissions, read the journal:

<div class="os-tabs-src">

#### sh

```sh
journalctl --user -u zeroclaw --since "5 minutes ago"
```

</div>

### Service can't find config

The service and CLI may resolve config differently if they run as different users or with different env vars. Force-print the path the daemon sees:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw config list
```

</div>

If the paths differ between `zeroclaw config list` (as you) and the service (as its user), either:

- Set `ZEROCLAW_CONFIG_DIR` in the service unit's `Environment=`
- Run the service as you (lingering-enabled user service)
- Copy/symlink the config to the path the service expects

---

## Still stuck?

Gather diagnostics and file an issue:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw --version
zeroclaw doctor
zeroclaw channel doctor
journalctl --user -u zeroclaw --since "1 hour ago" > zeroclaw-log.txt
```

</div>

Sanitise `zeroclaw-log.txt` (redact channel tokens if any slipped through, they shouldn't) and attach it to the issue. See [Contributing → Communication](../contributing/communication.md) for where.

## See also

- [Logs & observability](./observability.md)
- [Service & daemon](./service.md)
- [Setup → Service management](../setup/service.md)
- [Reference → Config](../reference/config.md)
