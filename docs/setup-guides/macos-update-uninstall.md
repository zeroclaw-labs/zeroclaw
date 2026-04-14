# macOS Update and Uninstall Guide

This page documents supported update and uninstall procedures for QuantClaw on macOS (OS X).

Last verified: **February 22, 2026**.

## 1) Check current install method

```bash
which quantclaw
quantclaw --version
```

Typical locations:

- Homebrew: `/opt/homebrew/bin/quantclaw` (Apple Silicon) or `/usr/local/bin/quantclaw` (Intel)
- Cargo/bootstrap/manual: `~/.cargo/bin/quantclaw`

If both exist, your shell `PATH` order decides which one runs.

## 2) Update on macOS

### A) Homebrew install

```bash
brew update
brew upgrade quantclaw
quantclaw --version
```

### B) Clone + bootstrap install

From your local repository checkout:

```bash
git pull --ff-only
./install.sh --skip-onboard
quantclaw --version
```

### C) Manual prebuilt binary install

Re-run your download/install flow with the latest release asset, then verify:

```bash
quantclaw --version
```

## 3) Uninstall on macOS

### A) Stop and remove background service first

This prevents the daemon from continuing to run after binary removal.

```bash
quantclaw service stop || true
quantclaw service uninstall || true
```

Service artifacts removed by `service uninstall`:

- `~/Library/LaunchAgents/com.quantclaw.daemon.plist`

### B) Remove the binary by install method

Homebrew:

```bash
brew uninstall quantclaw
```

Cargo/bootstrap/manual (`~/.cargo/bin/quantclaw`):

```bash
cargo uninstall quantclaw || true
rm -f ~/.cargo/bin/quantclaw
```

### C) Optional: remove local runtime data

Only run this if you want a full cleanup of config, auth profiles, logs, and workspace state.

```bash
rm -rf ~/.quantclaw
```

## 4) Verify uninstall completed

```bash
command -v quantclaw || echo "quantclaw binary not found"
pgrep -fl quantclaw || echo "No running quantclaw process"
```

If `pgrep` still finds a process, stop it manually and re-check:

```bash
pkill -f quantclaw
```

## Related docs

- [One-Click Bootstrap](one-click-bootstrap.md)
- [Commands Reference](../reference/cli/commands-reference.md)
- [Troubleshooting](../ops/troubleshooting.md)
