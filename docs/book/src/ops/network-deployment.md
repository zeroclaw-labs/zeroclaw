# Network Deployment

Deploying ZeroClaw so it can receive inbound traffic: gateway exposure, webhook channels, tunnels, and LAN-only vs. public-facing configurations. Raspberry Pis and other home-network hosts are first-class targets here.

## When inbound ports matter

| Mode | Needs inbound port | Notes |
|---|:---:|---|
| Telegram (long-poll) | No | ZeroClaw polls `api.telegram.org` — works behind NAT |
| Matrix / Mattermost / Nextcloud Talk | No | Sync/WebSocket — outbound only |
| Discord / Slack (Socket Mode) | No | Outbound WebSocket |
| Signal (`signal-cli-rest-api`) | No | Localhost container |
| Nostr / IMAP / MQTT | No | All outbound |
| Webhooks (GitHub, Slack Events API, WhatsApp, Nextcloud Talk bot, custom) | **Yes** | Public POST endpoint required |
| Gateway pairing from LAN | Yes (LAN-scope) | Bind to `0.0.0.0` or use a tunnel |
| Discord / Slack (HTTP Events) | Yes | If you don't use Socket Mode |

**Upshot:** a Telegram-only bot runs on a Pi behind a consumer router with zero port forwarding. Anything webhook-based needs a reachable URL — which is where tunnels come in.

## Binding the gateway

By default the gateway binds to `127.0.0.1` — unreachable from other devices. Three options to expose it:

### Option 1 — Public bind (LAN)

```toml
[gateway]
host = "0.0.0.0"
port = 42617
allow_public_bind = true     # required safety flag
```

Then any device on the LAN can reach `http://<pi-ip>:42617`. Doesn't help for internet-reachable webhooks — your router's public IP isn't forwarded to the Pi.

**Safety:** `allow_public_bind = true` is required because binding to `0.0.0.0` is a significant posture change. Without it, the daemon refuses. This is deliberate.

### Option 2 — Tunnel (internet-reachable)

```toml
[tunnel]
provider = "tailscale"       # or "cloudflare", "ngrok"
```

Then restart the daemon — the tunnel is managed declaratively from config, starting alongside the gateway.

The tunnel forwards from a public URL to the gateway on `127.0.0.1`. No router config, no opened ports. All three supported tunnels work similarly:

| Provider | Setup friction | Cost | Good for |
|---|---|---|---|
| Tailscale Funnel | Create account, install client | Free tier | Long-term, stable URLs |
| Cloudflare Tunnel | Create Cloudflare account, install `cloudflared` | Free | Custom domains |
| ngrok | Sign up, install CLI | Free with limits | Testing, short-lived |

### Option 3 — Reverse proxy

Run nginx / Caddy / Traefik in front of the gateway. Terminate TLS there, proxy to `localhost:42617`. Suitable for:

- Servers with a real public IP
- Existing reverse-proxy setups with Let's Encrypt
- Serving multiple services on the same host

A minimal Caddy config:

```caddy
agent.example.com {
    reverse_proxy localhost:42617
}
```

The gateway stays bound to `127.0.0.1` — the proxy does the listening.

## Raspberry Pi deployment

### Prerequisites

- Raspberry Pi 3/4/5 (or similar SBC) with Raspberry Pi OS or Alpine
- Network connectivity (WiFi or Ethernet)
- Optional: USB peripherals for hardware integration

### Install

For a Pi running Raspberry Pi OS:

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash -s -- --prebuilt
```

Prefer `--prebuilt` on a Pi — compiling from source can take 30+ minutes.

For a Pi running Alpine:

```bash
apk add curl rust cargo openssl-dev pkgconf
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

### Hardware features

```bash
cargo install --locked --path . --features "hardware peripheral-rpi"
```

Grants access to GPIO, I2C, SPI via `rppal`. The stock service unit already adds the user to the `gpio`, `spi`, `i2c` groups.

### Checklist

- [ ] Install the binary (prefer prebuilt on a Pi)
- [ ] Run `zeroclaw onboard`
- [ ] Configure your channels — Telegram needs no port; webhooks need a tunnel
- [ ] Install the service: `zeroclaw service install && zeroclaw service start`
- [ ] For LAN access: set `[gateway] host = "0.0.0.0"` + `allow_public_bind = true`
- [ ] For webhooks: configure `[tunnel]` with a provider

## Alpine Linux (OpenRC)

OpenRC services run system-wide. Install as root:

```bash
sudo zeroclaw service install
```

Creates:

- `/etc/init.d/zeroclaw` — init script
- `/etc/zeroclaw/` — config directory
- `/var/log/zeroclaw/` — log files

Enable and start:

```bash
sudo rc-update add zeroclaw default
sudo rc-service zeroclaw start
sudo rc-service zeroclaw status
```

Logs:

```bash
sudo tail -f /var/log/zeroclaw/error.log
```

### OpenRC notes

- Service runs as `zeroclaw:zeroclaw` (least privilege)
- Config path is fixed: `/etc/zeroclaw/config.toml`
- System-wide only — no user-level OpenRC services
- All service operations need `sudo`

## Telegram polling caveat

Telegram Bot API's `getUpdates` is single-poller per bot token. You cannot run two instances with the same token — the second gets `Conflict: terminated by other getUpdates request`.

If you see this:

1. `ps aux | grep zeroclaw` and confirm only one daemon is running
2. Check you don't have `cargo run --bin zeroclaw -- channel start telegram` from a dev session hanging around
3. If stale, reset Telegram's poll session:
   ```bash
   curl -X POST "https://api.telegram.org/bot$TOKEN/close"
   ```

## Exposing webhooks safely

A publicly-reachable webhook URL is attack surface. At minimum:

- **HMAC signature verification** — `secret` configured on each webhook channel
- **Source IP allowlist** where the service has fixed egress IPs (GitHub, AWS SNS)
- **Rate limiting** — `rate_limit_per_sec` in the webhook channel config

See [Channels → Webhooks](../channels/webhook.md) for the full set of knobs.

## See also

- [Setup → Container](../setup/container.md) — Docker-specific network config
- [Setup → Service management](../setup/service.md) — platform service integration
- [Operations → Overview](./overview.md)
- [Security → Overview](../security/overview.md)
