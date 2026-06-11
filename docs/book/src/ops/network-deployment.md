# Network Deployment

Deploying ZeroClaw so it can receive inbound traffic: gateway exposure, webhook channels, tunnels, and LAN-only vs. public-facing configurations. Raspberry Pis and other home-network hosts are first-class targets here.

## When inbound ports matter

| Mode | Inbound port? | Notes |
|---|:---:|---|
| Telegram (long-poll) | No | ZeroClaw polls `api.telegram.org`, works behind NAT |
| Matrix / Mattermost / Nextcloud Talk | No | Sync/WebSocket, outbound only |
| Discord / Slack (Socket Mode) | No | Outbound WebSocket |
| Signal (`signal-cli-rest-api`) | No | Localhost container |
| Nostr / IMAP / MQTT | No | All outbound |
| Webhooks (GitHub, Slack Events API, WhatsApp, Nextcloud Talk bot, custom) | **Yes** | Public POST endpoint required |
| Gateway pairing from LAN | Yes (LAN-scope) | Bind to `0.0.0.0` or use a tunnel |
| Discord / Slack (HTTP Events) | Yes | If you don't use Socket Mode |

**Upshot:** a Telegram-only bot runs on a Pi behind a consumer router with zero port forwarding. Anything webhook-based needs a reachable URL, which is where tunnels come in.

## Binding the gateway

By default the gateway binds to `127.0.0.1`, unreachable from other devices. Three options to expose it:

### Option 1: Public bind (LAN)

Then any device on the LAN can reach `http://<pi-ip>:42617`. Doesn't help for internet-reachable webhooks, your router's public IP isn't forwarded to the Pi.

**Safety:** `allow_public_bind = true` is required because binding to `0.0.0.0` is a significant posture change. Without it, the daemon refuses. This is deliberate.

### Option 2: Tunnel (internet-reachable)

Then restart the daemon, the tunnel is managed declaratively from config, starting alongside the gateway.

The tunnel forwards from a public URL to the gateway on `127.0.0.1`. No router config, no opened ports. Set `tunnel.tunnel_provider` to one of the supported values; each works similarly:

| Provider | Setup friction | Cost | Good for |
|---|---|---|---|
| `tailscale` | Account + client | Free tier | Long-term, stable URLs |
| `cloudflare` | Account + `cloudflared` + token | Free | Custom domains |
| `ngrok` | Account + agent + token | Free with limits | Testing, short-lived |
| `pinggy` | SSH, no account | Free tier | Quick one-shot URLs |
| `openvpn` | Your own OpenVPN egress | Self-hosted | Existing VPN infra |
| `custom` | A command under `[tunnel.custom]` | Depends | Anything else |

`tunnel_provider = "none"` (the default) keeps the gateway local with no tunnel. See the [Config reference](../reference/config.md#tunnel) for each provider's `[tunnel.<provider>]` fields.

### Option 3: Reverse proxy

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

The gateway stays bound to `127.0.0.1`, the proxy does the listening.

## Remote daemon reload

`POST /admin/reload` re-reads `config.toml` and rebuilds every subsystem in place (same PID, sub-second downtime). It is the supported way to apply config changes without a full restart. By default it only accepts **loopback** callers, so a remote dashboard or `curl` from another machine gets `403 Forbidden`.

To allow authenticated remote reloads:

```toml
[gateway]
allow_remote_admin = true    # off by default
require_pairing = true        # required for remote reload (also the default)
```

With this enabled, a non-loopback caller may hit `/admin/reload` **only if it also passes pairing authentication** (`Authorization: Bearer <token>`). Loopback callers (the local CLI) are always allowed and need no token. `/admin/shutdown` and the pairing-code endpoints remain localhost-only regardless of this flag.

Because remote access is enforced through pairing, `allow_remote_admin` has no effect unless `require_pairing` is also on: if pairing is disabled, a remote caller cannot be authenticated, so the request is rejected with `403 Forbidden` rather than allowed anonymously. This makes it impossible to expose an unauthenticated remote reload by flipping a single flag.

**Safety:** leave `allow_remote_admin` off unless you specifically need to reload from another host. Keep `require_pairing = true` (the default) so reloads can't be triggered anonymously.

## Raspberry Pi deployment

### Prerequisites

- Raspberry Pi 3/4/5 (or similar SBC) with Raspberry Pi OS or Alpine
- Network connectivity (WiFi or Ethernet)
- Optional: USB peripherals for hardware integration

### Install

Clone and run the installer. With no flags it drops into an interactive picker
where you choose the build type and which features to compile in, including the
hardware features for GPIO/I2C/SPI. On the Pi it also uses the Pi-tuned cargo
profiles; see [Raspberry Pi setup](../hardware/raspberry-pi-setup.md) for swap
setup and the per-model build matrix.

<div class="os-tabs-src">

#### Raspberry Pi OS

```sh
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

#### Alpine

```sh
apk add curl rust cargo openssl-dev pkgconf git
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

</div>

Grants access to GPIO, I2C, SPI via `rppal` when you pick the hardware features.
The stock service unit already adds the user to the `gpio`, `spi`, `i2c` groups.

### Checklist

- [ ] Install the binary (`./install.sh`, pick your features in the picker)
- [ ] Run `zeroclaw quickstart`
- [ ] Configure your channels. Telegram needs no port; webhooks need a tunnel
- [ ] Install the service: `zeroclaw service install && zeroclaw service start`
- [ ] For LAN access: set `[gateway] host = "0.0.0.0"` + `allow_public_bind = true`
- [ ] For webhooks: configure `[tunnel]` with a provider

## Alpine Linux (OpenRC)

OpenRC services run system-wide. Install as root:

<div class="os-tabs-src">

#### sh

```sh
sudo zeroclaw service install
```

</div>

Creates:

- `/etc/init.d/zeroclaw`: init script
- `/etc/zeroclaw/`: config directory
- `/var/log/zeroclaw/`: log files

Enable and start:

<div class="os-tabs-src">

#### sh

```sh
sudo rc-update add zeroclaw default
sudo rc-service zeroclaw start
sudo rc-service zeroclaw status
```

</div>

Logs:

<div class="os-tabs-src">

#### sh

```sh
sudo tail -f /var/log/zeroclaw/error.log
```

</div>

### OpenRC notes

- Service runs as `zeroclaw:zeroclaw` (least privilege)
- System-wide only: no user-level OpenRC services
- All service operations need `sudo`

## Telegram polling caveat

Telegram Bot API's `getUpdates` is single-poller per bot token. You cannot run two instances with the same token; the second gets `Conflict: terminated by other getUpdates request`.

If you see this:

1. `ps aux | grep zeroclaw` and confirm only one daemon is running
2. Check you don't have `cargo run --bin zeroclaw -- channel start telegram` from a dev session hanging around
3. If stale, reset Telegram's poll session:

   <div class="os-tabs-src">

   #### sh

   ```sh
   curl -X POST "https://api.telegram.org/bot$TOKEN/close"
   ```

   </div>

## Exposing webhooks safely

A publicly-reachable webhook URL is attack surface. At minimum:

- **HMAC signature verification**: `secret` configured on each webhook channel
- **Source IP allowlist** where the service has fixed egress IPs (GitHub, AWS SNS)
- **Rate limiting**: `rate_limit_per_sec` in the webhook channel config

See [Channels → Webhooks](../channels/webhook.md) for the full set of knobs.

## See also

- [Setup → Container](../setup/container.md): Docker-specific network config
- [Setup → Service management](../setup/service.md): platform service integration
- [Operations → Overview](./overview.md)
- [Security → Overview](../security/overview.md)
