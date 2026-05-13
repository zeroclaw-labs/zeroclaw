# macOS Update and Uninstall Guide

This page documents supported update and uninstall procedures for DaemonClaw on macOS (OS X).

Last verified: **February 22, 2026**.

## 1) Check current install method

```bash
which daemonclaw
daemonclaw --version
```

Typical locations:

- Homebrew: `/opt/homebrew/bin/daemonclaw` (Apple Silicon) or `/usr/local/bin/daemonclaw` (Intel)
- Cargo/bootstrap/manual: `~/.cargo/bin/daemonclaw`

If both exist, your shell `PATH` order decides which one runs.

## 2) Update on macOS

### A) Homebrew install

```bash
brew update
brew upgrade daemonclaw
daemonclaw --version
```

### B) Clone + bootstrap install

From your local repository checkout:

```bash
git pull --ff-only
./install.sh --skip-onboard
daemonclaw --version
```

### C) Manual prebuilt binary install

Re-run your download/install flow with the latest release asset, then verify:

```bash
daemonclaw --version
```

## 3) Uninstall on macOS

### A) Stop and remove background service first

This prevents the daemon from continuing to run after binary removal.

```bash
daemonclaw service stop || true
daemonclaw service uninstall || true
```

Service artifacts removed by `service uninstall`:

- `~/Library/LaunchAgents/com.daemonclaw.daemon.plist`

### B) Remove the binary by install method

Homebrew:

```bash
brew uninstall daemonclaw
```

Cargo/bootstrap/manual (`~/.cargo/bin/daemonclaw`):

```bash
cargo uninstall daemonclaw || true
rm -f ~/.cargo/bin/daemonclaw
```

### C) Optional: remove local runtime data

Only run this if you want a full cleanup of config, auth profiles, logs, and workspace state.

```bash
rm -rf ~/.daemonclaw
```

## 4) Verify uninstall completed

```bash
command -v daemonclaw || echo "daemonclaw binary not found"
pgrep -fl daemonclaw || echo "No running daemonclaw process"
```

If `pgrep` still finds a process, stop it manually and re-check:

```bash
pkill -f daemonclaw
```

## Related docs

- [One-Click Bootstrap](one-click-bootstrap.md)
- [Commands Reference](../reference/cli/commands-reference.md)
- [Troubleshooting](../ops/troubleshooting.md)
