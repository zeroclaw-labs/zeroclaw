# scripts/ — Raspberry Pi Deployment Guide

This directory contains the scripts and templates used to deploy QuantClaw on a Raspberry Pi.

The recommended production path is now:

1. Push the latest deployment changes to the remote repository
2. SSH into the Pi
3. Run `bootstrap-pi-git.sh`
4. Let the Pi `git clone` or `git pull` `master`
5. Build and install locally

This keeps deployment separate from the existing `/home/quant/quanclaw` networking program and installs QuantClaw under `/home/quant/quantclaw_rust_app`.

## Contents

| File | Purpose |
|------|---------|
| `bootstrap-pi-git.sh` | Primary Pi-side bootstrap script for cleanup, clone/pull, and install |
| `install-pi.sh` | Pi-side installer that builds from a checked-out repository or compatible archive |
| `deploy-rpi.sh` | Auxiliary local helper for cross-compiling and pushing over SSH |
| `rpi-config.toml` | Gateway config template rendered to `~/.quantclaw/config.toml` |
| `quantclaw.service` | systemd unit template rendered to `/etc/systemd/system/quantclaw.service` |
| `99-act-led.rules` | Optional udev rule for ACT LED access |

---

## Recommended Layout

| Path | Purpose |
|------|---------|
| `/home/quant/quantclaw_rust_app` | QuantClaw app directory |
| `/home/quant/quantclaw_rust_app/repo` | Git checkout cloned from Gitea |
| `/home/quant/quantclaw_rust_app/quantclaw` | Installed binary |
| `/home/quant/quantclaw_rust_app/.env` | Provider credentials |
| `/home/quant/.quantclaw/config.toml` | Rendered runtime config |
| `/etc/systemd/system/quantclaw.service` | Boot-time service |
| `/swapfile_quantclaw` | Bootstrap-managed swap file for low-memory installs |

The gateway binds to `0.0.0.0:42617` by default. The webhook example port is `42618` to stay clear of `8080`.

---

## Recommended Flow

### 1. Push local changes first

The Pi pulls from:

```text
https://gitea.tangledup-ai.com/Therianclouds/QuantClaw_Rust.git
```

Branch:

```text
master
```

Push your local deployment changes before cloning or pulling on the Pi.

### 2. Copy the bootstrap script to the Pi

```powershell
scp .\scripts\bootstrap-pi-git.sh quant@6.6.6.46:/tmp/
```

### 3. Run the bootstrap script on the Pi

```bash
ssh quant@6.6.6.46
chmod +x /tmp/bootstrap-pi-git.sh
/tmp/bootstrap-pi-git.sh
```

The bootstrap script will:

1. Remove failed archive-install leftovers from `/tmp`
2. Clear the old installer cache under `/home/quant/.cache/quantclaw-install`
3. Create and enable `/swapfile_quantclaw` if swap is missing
4. Install system packages required for Rust builds
5. Install Rust `1.87.0` with a minimal profile and low-memory unpack settings when needed
6. Clone the repository into `/home/quant/quantclaw_rust_app/repo` if it does not exist
7. Otherwise fetch and fast-forward `master`
8. Run `scripts/install-pi.sh` from the checked-out repository

### 4. What the installer does

`install-pi.sh` will:

1. Build `quantclaw` from the repository root with `cargo build --release --features hardware,peripheral-rpi`
2. Install the binary to `/home/quant/quantclaw_rust_app/quantclaw`
3. Create `/home/quant/quantclaw_rust_app/.env` if missing
4. Render `rpi-config.toml` to `/home/quant/.quantclaw/config.toml`
5. Render `quantclaw.service` to `/etc/systemd/system/quantclaw.service`
6. Run `systemctl daemon-reload`
7. Run `systemctl enable --now quantclaw`

---

## Manual Cleanup

If you want to clean up the failed archive deployment manually before bootstrapping:

```bash
rm -f /tmp/quantclaw-rpi*.tar.gz /tmp/install-pi.sh
rm -rf /home/quant/.cache/quantclaw-install
```

If an incomplete repo directory already exists and you want a clean re-clone:

```bash
rm -rf /home/quant/quantclaw_rust_app/repo
```

If you also want to reset the bootstrap-managed swap file:

```bash
sudo swapoff /swapfile_quantclaw || true
sudo rm -f /swapfile_quantclaw
```

---

## First-Time Setup

After the first successful install, SSH into the Pi and fill in your API key:

```bash
ssh quant@6.6.6.46
nano /home/quant/quantclaw_rust_app/.env
# Set: ANTHROPIC_API_KEY=sk-ant-...
sudo systemctl restart quantclaw
```

The `.env` file is loaded by systemd via `EnvironmentFile=`.

---

## Runtime Checks

### Health check

```bash
curl http://6.6.6.46:42617/health
```

### Chat request

```bash
curl -s -X POST http://6.6.6.46:42617/api/chat \
  -H 'Content-Type: application/json' \
  -d '{"message": "ping"}'
```

### Service status

```bash
sudo systemctl status quantclaw --no-pager
```

### Follow logs

```bash
sudo journalctl -u quantclaw -f
```

### Confirm boot auto-start

```bash
sudo systemctl is-enabled quantclaw
```

Expected output: `enabled`

---

## Upgrades

To update an existing deployment on the Pi:

```bash
cd /home/quant/quantclaw_rust_app/repo
git fetch --all --prune
git checkout master
git pull --ff-only origin master
./scripts/install-pi.sh /home/quant/quantclaw_rust_app/repo
```

Or simply rerun:

```bash
/tmp/bootstrap-pi-git.sh
```

---

## Troubleshooting

### Service won't start

```bash
sudo systemctl status quantclaw --no-pager
sudo journalctl -u quantclaw -n 100 --no-pager
```

### Port check

```bash
ss -ltnp | grep 42617
```

### Wrong file location

Make sure QuantClaw is under:

```bash
ls -lah /home/quant/quantclaw_rust_app
ls -lah /home/quant/quantclaw_rust_app/repo
ls -lah /home/quant/.quantclaw
```

### Git sync failed

Check whether the Pi can access the Gitea repository and whether credentials are configured if the repo requires authentication.

```bash
git ls-remote https://gitea.tangledup-ai.com/Therianclouds/QuantClaw_Rust.git
```

### Rust install failed on low memory

The bootstrap script automatically provisions swap and installs Rust `1.87.0` with a minimal profile. If you need to retry manually:

```bash
export RUSTUP_IO_THREADS=1
export RUSTUP_UNPACK_RAM=67108864
/tmp/bootstrap-pi-git.sh
```

### GPIO permissions

If hardware access matters, confirm the deploy user is in the `gpio` group:

```bash
groups quant
```

---

## Configuration Defaults

`rpi-config.toml` currently renders these important values:

- Gateway host: `0.0.0.0`
- Gateway port: `42617`
- Example webhook port: `42618`
- Runtime config path: `/home/quant/.quantclaw/config.toml`
- App directory: `/home/quant/quantclaw_rust_app`
- Repo directory: `/home/quant/quantclaw_rust_app/repo`
- Managed swap file: `/swapfile_quantclaw`
