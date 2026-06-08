# Docker & Containers

Run ZeroClaw in Docker, Podman, Kubernetes, or any OCI runtime.

## Official images

Pushed to GitHub Container Registry (`ghcr.io`) on every stable release:

- `ghcr.io/zeroclaw-labs/zeroclaw:latest` — latest stable
- `ghcr.io/zeroclaw-labs/zeroclaw:v0.7.5` — pinned
- `ghcr.io/zeroclaw-labs/zeroclaw:debian` — Debian-based image (larger, broader glibc support)

Multi-arch: `linux/amd64`, `linux/arm64`.

> **Note on shell access:** The default `latest` image is intentionally distroless and does not include `sh`, `ash`, or `bash`. Use the `debian` tag if you need a shell inside the container (for example, to run `docker exec` for debugging).

## Minimum run

```bash
docker run -d \
  --name zeroclaw \
  -v zeroclaw-data:/zeroclaw-data \
  -p 42617:42617 \
  ghcr.io/zeroclaw-labs/zeroclaw:latest
```

The image expects persistent state at `/zeroclaw-data`. On first run, it bootstraps a default config — you still need to run quickstart before it's useful:

```bash
docker exec -it zeroclaw zeroclaw quickstart
```

## Compose

A minimal `docker-compose.yml`:

```yaml
services:
  zeroclaw:
    image: ghcr.io/zeroclaw-labs/zeroclaw:latest
    restart: unless-stopped
    ports:
      - "42617:42617"      # gateway
    volumes:
      - ./data:/zeroclaw-data
    environment:
      ZEROCLAW_ALLOW_PUBLIC_BIND: "1"   # only if the gateway must be reachable on the LAN
```

After the container starts, run quickstart:

```bash
docker compose exec zeroclaw zeroclaw quickstart
```

Drop `ZEROCLAW_ALLOW_PUBLIC_BIND` if you only need local access.

## macOS — OrbStack vs Colima

macOS has no native Linux kernel, so every option (Docker Desktop, Podman, OrbStack, Colima) runs the container inside a lightweight Linux VM. For a Mac dev box, the two mac-native VMs worth comparing are OrbStack and Colima — both run the container with the same `docker run`/Compose commands above.

| | OrbStack | Colima |
|---|---|---|
| Engine | custom, tuned Linux VM (Apple Silicon optimized) | Lima VM + containerd/Docker |
| License | commercial, freemium (free personal use) | MIT (Lima underneath is Apache 2.0) |
| Interface | GUI app + CLI | CLI-first (`colima start/stop`), scriptable |
| Best when | minimal fuss, polished UX | everything OSS, config in code |

```bash
# OrbStack — provides the docker CLI:
brew install --cask orbstack

# Colima — docker CLI talks to colima's VM:
brew install colima docker docker-compose   # docker-compose = the Compose v2 plugin; install if you need `docker compose`
colima start --cpu 4 --memory 8   # add --network-address to expose the VM IP to macOS
```

Performance is comparable for typical dev workloads; the real differentiators are licensing (commercial vs OSS) and UX preference, not raw speed — benchmark both on your own machine if idle RAM or build throughput matters. Either way you drive the engine inside the VM with `docker`; systemd quadlets (below) are a Linux-host feature and don't apply on macOS.

## Podman & systemd quadlets

On a Linux server, the cleanest way to run the container long-term is a Podman **quadlet** — a declarative unit file that systemd turns into a real service. You get `systemctl` lifecycle, journald logs, auto-restart, and boot ordering with no daemon and no `--restart` hack, and the unit file is config you commit to git. This is the recommended server pattern; `docker run`/Compose are fine for a laptop.

A quadlet is a `*.container` file (siblings: `.pod`, `.volume`, `.network`, `.kube`, `.build`, `.image`). Podman's systemd generator reads it on every `daemon-reload` and writes a transient `.service` — you never author the `.service` yourself.

Rootful units live in `/etc/containers/systemd/`; rootless in `~/.config/containers/systemd/`.

`/etc/containers/systemd/zeroclaw.container`:

```ini
[Unit]
Description=ZeroClaw agent runtime
After=network-online.target
Wants=network-online.target

[Container]
# Pin a release in production; :latest is distroless (no shell — use :debian to exec a shell).
Image=ghcr.io/zeroclaw-labs/zeroclaw:latest
ContainerName=zeroclaw
PublishPort=42617:42617
Volume=zeroclaw-data:/zeroclaw-data
# Only if the gateway must be reachable off-localhost (LAN):
Environment=ZEROCLAW_ALLOW_PUBLIC_BIND=1
# Optional rolling-upgrade path — re-pull a newer image on (re)start and opt into `podman auto-update`:
Pull=newer
AutoUpdate=registry

[Service]
Restart=always

[Install]
WantedBy=multi-user.target default.target
```

Deploy (idempotent — safe to re-run; re-applying converges the running container, never duplicates it):

```bash
sudo cp zeroclaw.container /etc/containers/systemd/
sudo systemctl daemon-reload      # generator turns .container into zeroclaw.service
sudo systemctl restart zeroclaw
```

Then onboard once, and manage it like any service:

```bash
sudo podman exec -it zeroclaw zeroclaw onboard
systemctl status zeroclaw
journalctl -u zeroclaw -f
```

There is no `systemctl enable` step for generated units — the `[Install] WantedBy=` line is what brings it up on boot.

- **Version pinning vs `:latest`.** Pin a tag or digest (`Image=ghcr.io/zeroclaw-labs/zeroclaw:v0.7.5` or `...@sha256:...`) for reproducible, auditable deploys — upgrading is then a reviewable tag bump in the committed `.container` file. `Pull=newer` + `AutoUpdate=registry` instead give rolling upgrades, driven by `podman-auto-update.timer` (`sudo systemctl enable --now podman-auto-update.timer`). Pick reproducibility or currency; the deploy loop is the same either way.
- **Rootless variant.** Drop the file in `~/.config/containers/systemd/`, use `systemctl --user daemon-reload && systemctl --user restart zeroclaw`, and run `loginctl enable-linger $USER` so it survives logout (same lingering note as [Service & daemon](../ops/service.md)).
- **WSL2.** Modern WSL2 runs systemd (`[boot] systemd=true` in `/etc/wsl.conf`, then `wsl --shutdown`), so this exact quadlet pattern works inside a WSL distro — no Windows-specific dialect.

## Config inside containers

The image expects config at `/zeroclaw-data/.zeroclaw/config.toml`. Mount your local config in:

```bash
docker run -d --name zeroclaw \
  -v $(pwd)/my-config.toml:/zeroclaw-data/.zeroclaw/config.toml:ro \
  -v zeroclaw-state:/zeroclaw-data/workspace \
  -p 42617:42617 \
  ghcr.io/zeroclaw-labs/zeroclaw:latest
```

For container workloads, set `uri` on each `[providers.models.<type>.<alias>]` to a container-reachable address (e.g. `http://host.docker.internal:11434` for an Ollama server on the Docker Desktop host). The `ZEROCLAW_providers__models__<type>__<alias>__uri=...` env override can do the same at runtime without editing `config.toml`.

## Channels that poll (Telegram, email) — just work

Outbound-initiated channels don't need any special container configuration. Telegram polling, IMAP, MQTT, Nostr relays — all pull; the container only needs egress.

## Channels that receive webhooks — need ingress

Discord, Slack, GitHub, and most webhook channels need inbound HTTP. Two options:

1. **Expose the gateway** — `-p 42617:42617` + reverse proxy with TLS in front, point the webhook URL at the public address
2. **Use a tunnel** — ngrok, Cloudflare Tunnel, or Tailscale Funnel; set the tunnel URL as the webhook target

Configure a tunnel via `zeroclaw config set gateway.tunnel.provider=<ngrok|cloudflare>` and the related `gateway.tunnel.*` fields (see the [Config reference](../reference/config.md)); the resulting public URL is what you point your webhook senders at.

## Kubernetes

Helm chart templates are published to the [zeroclaw-templates](https://github.com/zeroclaw-labs/zeroclaw-templates) repo. Typical manifest fragment:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: zeroclaw
spec:
  replicas: 1
  strategy:
    type: Recreate         # ZeroClaw is single-instance per workspace
  template:
    spec:
      containers:
        - name: zeroclaw
          image: ghcr.io/zeroclaw-labs/zeroclaw:v0.7.5
          ports:
            - containerPort: 42617
          volumeMounts:
            - name: data
              mountPath: /zeroclaw-data
          env:
            - name: ZEROCLAW_ALLOW_PUBLIC_BIND
              value: "1"
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: zeroclaw-data
```

**Scaling:** ZeroClaw is single-writer per workspace. Don't scale horizontally — run one instance per agent.

## Re-authenticating after logout

If you log out of the web UI while running in a container, the existing paircode becomes invalid. Generate a new one to log back in:

```bash
docker exec -it zeroclaw zeroclaw gateway get-paircode --new
```

For Compose deployments, use `docker compose exec` instead:

```bash
docker compose exec zeroclaw zeroclaw gateway get-paircode --new
```

## Gotchas

- **macOS hostname quirks (Docker Desktop, colima, Rancher Desktop).** `host.docker.internal` works out of the box on **Docker Desktop** for macOS. On **colima**, it is only reachable if you installed with `colima start --network-address` (otherwise the container can't see the host at all — connect via the VM's gateway IP, usually `192.168.5.2`, or tunnel through a shared network). **Rancher Desktop** behaves like Docker Desktop for recent versions but has had `host.docker.internal` resolve-failures on older releases. If provider calls fail with `connection refused` to `host.docker.internal`, verify with `docker run --rm alpine getent hosts host.docker.internal` — empty output means the hostname isn't resolvable and you need an explicit IP.
- **Host-side services.** If a provider is Ollama on the host, `uri = "http://host.docker.internal:11434"` (under `[providers.models.ollama.<alias>]`) works on Docker Desktop. On Linux Docker you may need `--add-host=host.docker.internal:host-gateway`.
- **Memory persistence.** The SQLite memory file sits inside `/zeroclaw-data/workspace/`. If you don't mount that volume, every restart loses conversation history.
- **Bind-mounting `/zeroclaw-data`.** A host bind mount on `/zeroclaw-data` replaces the entire image directory, including the default `config.toml` and (previously) the dashboard bundle. The dashboard is now installed at `/usr/share/zeroclawlabs/web/dist` — outside the mount — so a bind mount no longer hides it. On first run, mount an empty host directory and the container bootstraps a fresh config; the gateway auto-detects the dashboard from its image path.
- **No hardware passthrough by default.** GPIO / USB need explicit `--device` flags (`--device /dev/ttyUSB0`), and the container user needs matching GID for `dialout`/`gpio` groups.

## Next

- [Service management](./service.md)
- [Operations → Network deployment](../ops/network-deployment.md) — tunnels, reverse proxies
