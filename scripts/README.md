# scripts/ — Raspberry Pi Deployment Guide

This directory contains everything needed to cross-compile ZeroClaw and deploy it to a Raspberry Pi over SSH.

## Contents

| File | Purpose |
|------|---------|
| `deploy-rpi.sh` | One-shot cross-compile and deploy script |
| `rpi-config.toml` | Production config template deployed to `~/.zeroclaw/config.toml` |
| `zeroclaw.service` | systemd unit file installed on the Pi |
| `99-act-led.rules` | udev rule for ACT LED sysfs access without sudo |

---

## Prerequisites

### Cross-compilation toolchain (pick one)

#### Option A — cargo-zigbuild (recommended for Apple Silicon)

```bash
brew install zig
cargo install cargo-zigbuild
rustup target add aarch64-unknown-linux-gnu
```

#### Option B — cross (Docker-based)

```bash
cargo install cross
rustup target add aarch64-unknown-linux-gnu
# Docker must be running
```

The deploy script auto-detects which tool is available, preferring `cargo-zigbuild`.
Force a specific tool with `CROSS_TOOL=zigbuild` or `CROSS_TOOL=cross`.

### Optional: passwordless SSH

If you can't use SSH key authentication, install `sshpass` and set the `RPI_PASS` environment variable:

```bash
brew install sshpass       # macOS
sudo apt install sshpass   # Linux
```

---

## Quick Start

```bash
RPI_HOST=raspberrypi.local RPI_USER=pi ./scripts/deploy-rpi.sh
```

After the first deploy, you must set your API key on the Pi (see [First-Time Setup](#first-time-setup)).

---

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `RPI_HOST` | `raspberrypi.local` | Pi hostname or IP address |
| `RPI_USER` | `pi` | SSH username |
| `RPI_PORT` | `22` | SSH port |
| `RPI_DIR` | `~/zeroclaw` | Remote directory for the binary and `.env` |
| `RPI_PASS` | _(unset)_ | SSH password — uses `sshpass` if set; key auth used otherwise |
| `CROSS_TOOL` | _(auto-detect)_ | Force `zigbuild` or `cross` |

---

## What the Deploy Script Does

1. **Cross-compile** — builds a release binary for `aarch64-unknown-linux-gnu` with `--features hardware,peripheral-rpi`.
2. **Stop service** — runs `sudo systemctl stop zeroclaw` on the Pi (continues if not yet installed).
3. **Create remote directory** — ensures `$RPI_DIR` exists on the Pi.
4. **Copy binary** — SCPs the compiled binary to `$RPI_DIR/zeroclaw`.
5. **Create `.env`** — writes an `.env` skeleton with an `ANTHROPIC_API_KEY=` placeholder to `$RPI_DIR/.env` with mode `600`. Skipped if the file already exists so an existing key is not overwritten.
6. **Deploy config** — copies `rpi-config.toml` to `~/.zeroclaw/config.toml`, preserving any `api_key` already present in the file.
7. **Install systemd service** — copies `zeroclaw.service` to `/etc/systemd/system/`, then enables and restarts it.
8. **Hardware permissions** — adds the deploy user to the `gpio` group, copies `99-act-led.rules` to `/etc/udev/rules.d/`, and resets the ACT LED trigger.

---

## First-Time Setup

After the first successful deploy, SSH into the Pi and fill in your API key:

```bash
ssh pi@raspberrypi.local
nano ~/zeroclaw/.env
# Set: ANTHROPIC_API_KEY=sk-ant-...
sudo systemctl restart zeroclaw
```

The `.env` is loaded by the systemd service as an `EnvironmentFile`.

---

## Interacting with ZeroClaw on the Pi

Once the service is running the gateway listens on port **8080**.

### Health check

```bash
curl http://raspberrypi.local:8080/health
```

### Send a message

```bash
curl -s -X POST http://raspberrypi.local:8080/api/chat \
  -H 'Content-Type: application/json' \
  -d '{"message": "What is the CPU temperature?"}' | jq .
```

### Stream a conversation

```bash
curl -N -s -X POST http://raspberrypi.local:8080/api/chat \
  -H 'Content-Type: application/json' \
  -H 'Accept: text/event-stream' \
  -d '{"message": "List connected hardware devices", "stream": true}'
```

### Follow service logs

```bash
ssh pi@raspberrypi.local 'journalctl -u zeroclaw -f'
```

---

## Hardware Features

### GPIO tools

ZeroClaw is deployed with the `peripheral-rpi` feature, which enables two LLM-callable tools:

- **`gpio_read`** — reads a GPIO pin value via sysfs (`/sys/class/gpio/...`).
- **`gpio_write`** — writes a GPIO pin value.

These tools let the agent directly control hardware in response to natural-language instructions.

### ACT LED

The udev rule `99-act-led.rules` grants the `gpio` group write access to:

```
/sys/class/leds/ACT/trigger
/sys/class/leds/ACT/brightness
```

This allows toggling the Pi's green ACT LED without `sudo`.

### Aardvark I2C/SPI adapter

If a Total Phase Aardvark adapter is connected, the `hardware` feature enables I2C/SPI communication with external devices. No extra setup is needed — the device is auto-detected via USB.

---

## Files Deployed to the Pi

| Remote path | Source | Description |
|------------|--------|-------------|
| `~/zeroclaw/zeroclaw` | compiled binary | Main agent binary |
| `~/zeroclaw/.env` | created on first deploy | API key and environment variables |
| `~/.zeroclaw/config.toml` | `rpi-config.toml` | Agent configuration |
| `/etc/systemd/system/zeroclaw.service` | `zeroclaw.service` | systemd service unit |
| `/etc/udev/rules.d/99-act-led.rules` | `99-act-led.rules` | ACT LED permissions |

---

## Configuration

`rpi-config.toml` is the production config template. Key defaults:

- **Provider**: `anthropic-custom:https://api.z.ai/api/anthropic`
- **Model**: `claude-3-5-sonnet-20241022`
- **Autonomy**: `full`
- **Allowed shell commands**: `git`, `cargo`, `npm`, `mkdir`, `touch`, `cp`, `mv`, `ls`, `cat`, `grep`, `find`, `echo`, `pwd`, `wc`, `head`, `tail`, `date`

To customise, edit `~/.zeroclaw/config.toml` directly on the Pi and restart the service.

---

## Troubleshooting

### Service won't start

```bash
ssh pi@raspberrypi.local 'sudo systemctl status zeroclaw'
ssh pi@raspberrypi.local 'journalctl -u zeroclaw -n 50 --no-pager'
```

### GPIO permission denied

Make sure the deploy user is in the `gpio` group and that a fresh login session has been started:

```bash
ssh pi@raspberrypi.local 'groups'
# Should include: gpio
```

If the group was just added, log out and back in, or run `newgrp gpio`.

### Wrong architecture / binary won't run

Re-run the deploy script. Confirm the target:

```bash
ssh pi@raspberrypi.local 'file ~/zeroclaw/zeroclaw'
# Expected: ELF 64-bit LSB pie executable, ARM aarch64
```

### Force a specific cross-compilation tool

```bash
CROSS_TOOL=zigbuild RPI_HOST=raspberrypi.local ./scripts/deploy-rpi.sh
# or
CROSS_TOOL=cross    RPI_HOST=raspberrypi.local ./scripts/deploy-rpi.sh
```

### Rebuild locally without deploying

```bash
cargo zigbuild --release \
  --target aarch64-unknown-linux-gnu \
  --features hardware,peripheral-rpi
```
