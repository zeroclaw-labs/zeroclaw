# macOS Update and Uninstall Guide

This page documents supported update and uninstall procedures for ZeroClaw on macOS (OS X).

Last verified: **February 22, 2026**.

## 1) Check current install method

```bash
which zeroclaw
zeroclaw --version
```

Typical locations:

- Homebrew: `/opt/homebrew/bin/zeroclaw` (Apple Silicon) or `/usr/local/bin/zeroclaw` (Intel)
- Cargo/bootstrap/manual: `~/.cargo/bin/zeroclaw`

If both exist, your shell `PATH` order decides which one runs.

## 2) Update on macOS

### A) Homebrew install

```bash
brew update
brew upgrade zeroclaw
zeroclaw --version
```

### B) Clone + bootstrap install

From your local repository checkout:

```bash
git pull --ff-only
./bootstrap.sh --prefer-prebuilt
zeroclaw --version
```

If you want source-only update:

```bash
git pull --ff-only
cargo install --path . --force --locked
zeroclaw --version
```

### C) Manual prebuilt binary install

Re-run your download/install flow with the latest release asset, then verify:

```bash
zeroclaw --version
```

## 3) Uninstall on macOS

### A) Stop and remove background service first

This prevents the daemon from continuing to run after binary removal.

```bash
zeroclaw service stop || true
zeroclaw service uninstall || true
```

Service artifacts removed by `service uninstall`:

- `~/Library/LaunchAgents/com.zeroclaw.daemon.plist`

### B) Remove the binary by install method

Homebrew:

```bash
brew uninstall zeroclaw
```

Cargo/bootstrap/manual (`~/.cargo/bin/zeroclaw`):

```bash
cargo uninstall zeroclaw || true
rm -f ~/.cargo/bin/zeroclaw
```

### C) Optional: remove local runtime data

Only run this if you want a full cleanup of config, auth profiles, logs, and workspace state.

```bash
rm -rf ~/.zeroclaw
```

## 4) Verify uninstall completed

```bash
command -v zeroclaw || echo "zeroclaw binary not found"
pgrep -fl zeroclaw || echo "No running zeroclaw process"
```

If `pgrep` still finds a process, stop it manually and re-check:

```bash
pkill -f zeroclaw
```

## Related docs

- [One-Click Bootstrap](../one-click-bootstrap.md)
- [Commands Reference](../commands-reference.md)
- [Troubleshooting](../troubleshooting.md)
