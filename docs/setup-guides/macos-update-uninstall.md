# macOS Update and Uninstall Guide

This page documents supported update and uninstall procedures for JhedaiClaw on macOS (OS X).

Last verified: **February 22, 2026**.

## 1) Check current install method

```bash
which jhedaiclaw
jhedaiclaw --version
```

Typical locations:

- Homebrew: `/opt/homebrew/bin/jhedaiclaw` (Apple Silicon) or `/usr/local/bin/jhedaiclaw` (Intel)
- Cargo/bootstrap/manual: `~/.cargo/bin/jhedaiclaw`

If both exist, your shell `PATH` order decides which one runs.

## 2) Update on macOS

### A) Homebrew install

```bash
brew update
brew upgrade jhedaiclaw
jhedaiclaw --version
```

### B) Clone + bootstrap install

From your local repository checkout:

```bash
git pull --ff-only
./install.sh --prefer-prebuilt
jhedaiclaw --version
```

If you want source-only update:

```bash
git pull --ff-only
cargo install --path . --force --locked
jhedaiclaw --version
```

### C) Manual prebuilt binary install

Re-run your download/install flow with the latest release asset, then verify:

```bash
jhedaiclaw --version
```

## 3) Uninstall on macOS

### A) Stop and remove background service first

This prevents the daemon from continuing to run after binary removal.

```bash
jhedaiclaw service stop || true
jhedaiclaw service uninstall || true
```

Service artifacts removed by `service uninstall`:

- `~/Library/LaunchAgents/com.jhedaiclaw.daemon.plist`

### B) Remove the binary by install method

Homebrew:

```bash
brew uninstall jhedaiclaw
```

Cargo/bootstrap/manual (`~/.cargo/bin/jhedaiclaw`):

```bash
cargo uninstall jhedaiclaw || true
rm -f ~/.cargo/bin/jhedaiclaw
```

### C) Optional: remove local runtime data

Only run this if you want a full cleanup of config, auth profiles, logs, and workspace state.

```bash
rm -rf ~/.jhedaiclaw
```

## 4) Verify uninstall completed

```bash
command -v jhedaiclaw || echo "jhedaiclaw binary not found"
pgrep -fl jhedaiclaw || echo "No running jhedaiclaw process"
```

If `pgrep` still finds a process, stop it manually and re-check:

```bash
pkill -f jhedaiclaw
```

## Related docs

- [One-Click Bootstrap](one-click-bootstrap.md)
- [Commands Reference](../reference/cli/commands-reference.md)
- [Troubleshooting](../ops/troubleshooting.md)
