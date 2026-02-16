# Network Deployment — ZeroClaw on Raspberry Pi and Local Network

This document covers deploying ZeroClaw on a Raspberry Pi or other host on your local network, with Telegram and optional webhook channels.

---

## 1. Overview

| Mode | Inbound port needed? | Use case |
|------|----------------------|----------|
| **Telegram polling** | No | ZeroClaw polls Telegram API; works from anywhere |
| **Discord/Slack** | No | Same — outbound only |
| **Gateway webhook** | Yes | POST /webhook, WhatsApp, etc. need a public URL |
| **Gateway pairing** | Yes | If you pair clients via the gateway |

**Key:** Telegram, Discord, and Slack use **long-polling** — ZeroClaw makes outbound requests. No port forwarding or public IP required.

---

## 2. ZeroClaw on Raspberry Pi

### 2.1 Prerequisites

- Raspberry Pi (3/4/5) with Raspberry Pi OS
- USB peripherals (Arduino, Nucleo) if using serial transport
- Optional: `rppal` for native GPIO (`peripheral-rpi` feature)

### 2.2 Install

```bash
# Build for RPi (or cross-compile from host)
cargo build --release --features hardware

# Or install via your preferred method
```

### 2.3 Config

Edit `~/.zeroclaw/config.toml`:

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"

# Or Arduino over USB
[[peripherals.boards]]
board = "arduino-uno"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[channels_config.telegram]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = ["*"]

[gateway]
host = "127.0.0.1"
port = 8080
allow_public_bind = false
```

### 2.4 Run Daemon (Local Only)

```bash
zeroclaw daemon --host 127.0.0.1 --port 8080
```

- Gateway binds to `127.0.0.1` — not reachable from other machines
- Telegram channel works: ZeroClaw polls Telegram API (outbound)
- No firewall or port forwarding needed

---

## 3. Binding to 0.0.0.0 (Local Network)

To allow other devices on your LAN to hit the gateway (e.g. for pairing or webhooks):

### 3.1 Option A: Explicit Opt-In

```toml
[gateway]
host = "0.0.0.0"
port = 8080
allow_public_bind = true
```

```bash
zeroclaw daemon --host 0.0.0.0 --port 8080
```

**Security:** `allow_public_bind = true` exposes the gateway to your local network. Only use on trusted LANs.

### 3.2 Option B: Tunnel (Recommended for Webhooks)

If you need a **public URL** (e.g. WhatsApp webhook, external clients):

1. Run gateway on localhost:
   ```bash
   zeroclaw daemon --host 127.0.0.1 --port 8080
   ```

2. Start a tunnel:
   ```toml
   [tunnel]
   provider = "tailscale"   # or "ngrok", "cloudflare"
   ```
   Or use `zeroclaw tunnel` (see tunnel docs).

3. ZeroClaw will refuse `0.0.0.0` unless `allow_public_bind = true` or a tunnel is active.

---

## 4. Telegram Polling (No Inbound Port)

Telegram uses **long-polling** by default:

- ZeroClaw calls `https://api.telegram.org/bot{token}/getUpdates`
- No inbound port or public IP needed
- Works behind NAT, on RPi, in a home lab

**Config:**

```toml
[channels_config.telegram]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = ["*"]   # or specific @usernames / user IDs
```

Run `zeroclaw daemon` — Telegram channel starts automatically.

---

## 5. Webhook Channels (WhatsApp, Custom)

Webhook-based channels need a **public URL** so Meta (WhatsApp) or your client can POST events.

### 5.1 Tailscale Funnel

```toml
[tunnel]
provider = "tailscale"
```

Tailscale Funnel exposes your gateway via a `*.ts.net` URL. No port forwarding.

### 5.2 ngrok

```toml
[tunnel]
provider = "ngrok"
```

Or run ngrok manually:
```bash
ngrok http 8080
# Use the HTTPS URL for your webhook
```

### 5.3 Cloudflare Tunnel

Configure Cloudflare Tunnel to forward to `127.0.0.1:8080`, then set your webhook URL to the tunnel's public hostname.

---

## 6. Checklist: RPi Deployment

- [ ] Build with `--features hardware` (and `peripheral-rpi` if using native GPIO)
- [ ] Configure `[peripherals]` and `[channels_config.telegram]`
- [ ] Run `zeroclaw daemon --host 127.0.0.1 --port 8080` (Telegram works without 0.0.0.0)
- [ ] For LAN access: `--host 0.0.0.0` + `allow_public_bind = true` in config
- [ ] For webhooks: use Tailscale, ngrok, or Cloudflare tunnel

---

## 7. References

- [hardware-peripherals-design.md](./hardware-peripherals-design.md) — Peripherals design
- [adding-boards-and-tools.md](./adding-boards-and-tools.md) — Hardware setup and adding boards
