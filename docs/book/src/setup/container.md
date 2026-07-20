# Docker & Containers

Run ZeroClaw in Docker, Podman, Kubernetes, or any OCI runtime.

## Official images

Pushed to GitHub Container Registry (`ghcr.io`) on every stable release:

- `ghcr.io/zeroclaw-labs/zeroclaw:latest`: latest stable
- `ghcr.io/zeroclaw-labs/zeroclaw:v0.7.5`: pinned
- `ghcr.io/zeroclaw-labs/zeroclaw:debian`: Debian-based image (larger, broader glibc support)

Multi-arch: `linux/amd64`, `linux/arm64`.

> **Note on shell access:** The default `latest` image is intentionally distroless and does not include `sh`, `ash`, or `bash`. Use the `debian` tag if you need a shell inside the container (for example, to run `docker exec` for debugging).

## Minimum run

<div class="os-tabs-src">

#### sh

```sh
docker run -d \
  --name zeroclaw \
  -v zeroclaw-data:/zeroclaw-data \
  -p 42617:42617 \
  ghcr.io/zeroclaw-labs/zeroclaw:latest
```

</div>

The official image already binds `[::]` with `allow_public_bind = true` and
`require_pairing = false` baked into its default config, so this direct
`docker run` example is reachable out of the box. The Compose examples below
still pin both gateway bind settings so persisted or custom configs cannot
silently restore a loopback-only listener.

The image expects persistent state at `/zeroclaw-data`. On first run, it bootstraps a default config: you still need to run quickstart before it's useful:

<div class="os-tabs-src">

#### sh

```sh
docker exec -it zeroclaw zeroclaw quickstart
```

</div>

## Running zerocode (the TUI)

The image ships the [zerocode](../zerocode/overview.md) terminal interface alongside the `zeroclaw` binary. The default entrypoint is `zeroclaw`, so launch zerocode by overriding it with `--entrypoint zerocode` and an interactive TTY (`-it`). Both image variants carry it:

<div class="os-tabs-src">

#### distroless (`:latest`)

```sh
docker run -it --entrypoint zerocode ghcr.io/zeroclaw-labs/zeroclaw:latest
```

#### debian

```sh
docker run -it --entrypoint zerocode ghcr.io/zeroclaw-labs/zeroclaw:debian
```

</div>

zerocode connects to a running ZeroClaw daemon, so point it at one:

- **Same container's daemon:** run it against the container that already runs the daemon (`docker exec -it zeroclaw zerocode`), which reaches the daemon over the local IPC socket.
- **A remote daemon:** connect over WebSocket Secure with `zerocode --connect wss://<host>:<port>`; see [Remote setup (WSS)](../zerocode/remote.md). This is the portable way to drive a containerized or remote daemon from your own terminal.

Persist `/zeroclaw-data` (as in [Minimum run](#minimum-run)) so the config and identity zerocode reads are the same ones the daemon uses.

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
      # Both settings are required: host selects the container interface and
      # allow_public_bind is the explicit permission for that exposure.
      - ZEROCLAW_gateway__host=0.0.0.0
      - ZEROCLAW_gateway__allow_public_bind=true
```

After the container starts, run quickstart:

<div class="os-tabs-src">

#### sh

```sh
docker compose exec zeroclaw zeroclaw quickstart
```

</div>

Compose should set both {{#env-var-name gateway.host}} and
{{#env-var-name gateway.allow_public_bind}} explicitly. Publishing a port does
not make a gateway bound to `127.0.0.1` inside the container reachable, and
`allow_public_bind = true` permits a public bind without selecting one. Keeping
the two overrides together also makes an existing volume or custom config with
localhost defaults behave consistently.

This intentionally changes exposure. The gateway must retain the explicit
`allow_public_bind` opt-in when it listens on `0.0.0.0`. The Compose mapping
`"42617:42617"` publishes on the host interfaces configured by Docker. To make
the gateway reachable only from the container host, use
`"127.0.0.1:42617:42617"`; keep the gateway's in-container host at `0.0.0.0`
because Docker bridge traffic does not arrive through container loopback.

### Rootless Compose with the Debian image

For rootless Docker or Podman Compose deployments that need shell tools inside
the container, use the current Debian image and bind a host data directory:

```yaml
services:
  zeroclaw:
    image: ghcr.io/zeroclaw-labs/zeroclaw:debian
    container_name: zeroclaw
    restart: unless-stopped
    ports:
      - "42617:42617"
    volumes:
      - ./data:/zeroclaw-data
    environment:
      - ZEROCLAW_gateway__host=0.0.0.0
      - ZEROCLAW_gateway__allow_public_bind=true
    healthcheck:
      test: ["CMD", "zeroclaw", "status", "--format=exit-code"]
      interval: 60s
      timeout: 10s
      retries: 3
      start_period: 10s
```

The current Debian image carries the packaged dashboard outside
`/zeroclaw-data`, so the bind mount does not hide it and no
`gateway.web_dist_dir` override is needed. The gateway overrides use the
schema-mirror spellings shown by {{#env-var-name gateway.host}} and
{{#env-var-name gateway.allow_public_bind}}. They take precedence over a
persisted localhost-default config while retaining the separate permission
opt-in.

## macOS: OrbStack vs Colima

macOS has no native Linux kernel, so every option (Docker Desktop, Podman, OrbStack, Colima) runs the container inside a lightweight Linux VM. For a Mac dev box, the two mac-native VMs worth comparing are OrbStack and Colima, both run the container with the same `docker run`/Compose commands above.

| | OrbStack | Colima |
|---|---|---|
| Engine | custom, tuned Linux VM (Apple Silicon optimized) | Lima VM + containerd/Docker |
| License | commercial, freemium (free personal use) | MIT (Lima underneath is Apache 2.0) |
| Interface | GUI app + CLI | CLI-first (`colima start/stop`), scriptable |
| Best when | minimal fuss, polished UX | everything OSS, config in code |

<div class="os-tabs-src">

#### OrbStack

```sh
# Provides the docker CLI:
brew install --cask orbstack
```

#### Colima

```sh
# docker CLI talks to colima's VM:
brew install colima docker docker-compose   # docker-compose = the Compose v2 plugin; install if you need `docker compose`
colima start --cpu 4 --memory 8   # add --network-address to expose the VM IP to macOS
```

</div>

Performance is comparable for typical dev workloads; the real differentiators are licensing (commercial vs OSS) and UX preference, not raw speed; benchmark both on your own machine if idle RAM or build throughput matters. Either way you drive the engine inside the VM with `docker`; systemd quadlets (below) are a Linux-host feature and don't apply on macOS.

## Podman & systemd quadlets

On a Linux server, the cleanest way to run the container long-term is a Podman **quadlet**: a declarative unit file that systemd turns into a real service. You get `systemctl` lifecycle, journald logs, auto-restart, and boot ordering with no daemon and no `--restart` hack, and the unit file is config you commit to git. This is the recommended server pattern; `docker run`/Compose are fine for a laptop.

A quadlet is a `*.container` file (siblings: `.pod`, `.volume`, `.network`, `.kube`, `.build`, `.image`). Podman's systemd generator reads it on every `daemon-reload` and writes a transient `.service`; you never author the `.service` yourself.

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
# The official image already binds publicly. If you mount a localhost-default
# config, override both gateway.host and gateway.allow_public_bind together.
# Optional rolling-upgrade path — re-pull a newer image on (re)start and opt into `podman auto-update`:
Pull=newer
AutoUpdate=registry

[Service]
Restart=always

[Install]
WantedBy=multi-user.target default.target
```

Deploy (idempotent, safe to re-run; re-applying converges the running container, never duplicates it):

<div class="os-tabs-src">

#### sh

```sh
sudo cp zeroclaw.container /etc/containers/systemd/
sudo systemctl daemon-reload      # generator turns .container into zeroclaw.service
sudo systemctl restart zeroclaw
```

</div>

Then onboard once, and manage it like any service:

<div class="os-tabs-src">

#### sh

```sh
sudo podman exec -it zeroclaw zeroclaw quickstart
systemctl status zeroclaw
journalctl -u zeroclaw -f
```

</div>

There is no `systemctl enable` step for generated units: the `[Install] WantedBy=` line is what brings it up on boot.

- **Version pinning vs `:latest`.** Pin a tag or digest (`Image=ghcr.io/zeroclaw-labs/zeroclaw:v0.7.5` or `...@sha256:...`) for reproducible, auditable deploys; upgrading is then a reviewable tag bump in the committed `.container` file. `Pull=newer` + `AutoUpdate=registry` instead give rolling upgrades, driven by `podman-auto-update.timer` (`sudo systemctl enable --now podman-auto-update.timer`). Pick reproducibility or currency; the deploy loop is the same either way.
- **Rootless variant.** Drop the file in `~/.config/containers/systemd/`, use `systemctl --user daemon-reload && systemctl --user restart zeroclaw`, and run `loginctl enable-linger $USER` so it survives logout (same lingering note as [Service & daemon](../ops/service.md)).
- **WSL2.** Modern WSL2 runs systemd (`[boot] systemd=true` in `/etc/wsl.conf`, then `wsl --shutdown`), so this exact quadlet pattern works inside a WSL distro: no Windows-specific dialect.

## Config inside containers

The image expects config under `/zeroclaw-data/.zeroclaw/`. Mount your local config in:

<div class="os-tabs-src">

#### sh

```sh
docker run -d --name zeroclaw \
  -v $(pwd)/my-config.toml:/zeroclaw-data/.zeroclaw/config.toml:ro \
  -v zeroclaw-state:/zeroclaw-data/workspace \
  -p 42617:42617 \
  ghcr.io/zeroclaw-labs/zeroclaw:latest
```

</div>

For container workloads, set `uri` on each `providers.models.<type>.<alias>` to a container-reachable address (e.g. `http://host.docker.internal:11434` for an Ollama server on the Docker Desktop host). The generic env-override mechanism can set the same field at runtime without editing the config:

{{#env-var container}}

See [Providers → Container-friendly overrides](../providers/configuration.md#container-friendly-overrides) for the grammar.

## Channels that poll (Telegram, email): just work

Outbound-initiated channels don't need any special container configuration. Telegram polling, IMAP, MQTT, Nostr relays: all pull; the container only needs egress.

## Channels that receive webhooks: need ingress

Discord, Slack, GitHub, and most webhook channels need inbound HTTP. Two options:

1. **Expose the gateway**: `-p 42617:42617` + reverse proxy with TLS in front, point the webhook URL at the public address
2. **Use a tunnel**: ngrok, Cloudflare Tunnel, or Tailscale Funnel; set the tunnel URL as the webhook target

Configure a tunnel by setting the top-level `[tunnel]` `tunnel_provider` (override env var: {{#env-var-name tunnel.tunnel_provider}}) to one of the supported providers and filling the matching `tunnel.*` block; the full provider list and per-provider fields are in the [Config reference](../reference/config.md#tunnel). The resulting public URL is what you point your webhook senders at.

## Kubernetes

Sample Kubernetes manifests are provided in the [`deploy-k8s/`](https://github.com/zeroclaw-labs/zeroclaw/tree/master/deploy-k8s) directory. Typical manifest fragment:

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
          # The official image already binds publicly. If you mount a
          # localhost-default config, override both gateway.host and
          # gateway.allow_public_bind together.
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: zeroclaw-data
```

**Scaling:** ZeroClaw is single-writer per workspace. Don't scale horizontally; run one instance per agent.

## Re-authenticating after logout

If you log out of the web UI while running in a container, the existing paircode becomes invalid. Generate a new one to log back in:

<div class="os-tabs-src">

#### sh

```sh
docker exec -it zeroclaw zeroclaw gateway get-paircode --new
```

</div>

For Compose deployments, use `docker compose exec` instead:

<div class="os-tabs-src">

#### sh

```sh
docker compose exec zeroclaw zeroclaw gateway get-paircode --new
```

</div>

## Gotchas

- **macOS hostname quirks (Docker Desktop, colima, Rancher Desktop).** `host.docker.internal` works out of the box on **Docker Desktop** for macOS. On **colima**, it is only reachable if you installed with `colima start --network-address` (otherwise the container can't see the host at all; connect via the VM's gateway IP, usually `192.168.5.2`, or tunnel through a shared network). **Rancher Desktop** behaves like Docker Desktop for recent versions but has had `host.docker.internal` resolve-failures on older releases. If provider calls fail with `connection refused` to `host.docker.internal`, verify with `docker run --rm alpine getent hosts host.docker.internal`: empty output means the hostname isn't resolvable and you need an explicit IP.
- **Host-side services.** If a provider is Ollama on the host, `uri = "http://host.docker.internal:11434"` (under `[providers.models.ollama.<alias>]`) works on Docker Desktop. On Linux Docker you may need `--add-host=host.docker.internal:host-gateway`.
- **Memory persistence.** Agent memory (the SQLite `brain.db`) lives under the config directory at `/zeroclaw-data/.zeroclaw/agents/<alias>/workspace/memory/`, with shared instance databases under `/zeroclaw-data/data/`. Mounting `/zeroclaw-data` persists all of it; skip the volume and every restart loses conversation history.
- **Bind-mounting `/zeroclaw-data`.** A host bind mount on `/zeroclaw-data` replaces the entire image directory, including the default config and (previously) the dashboard bundle. The dashboard is now installed at `/usr/share/zeroclawlabs/web/dist`, outside the mount, so a bind mount no longer hides it. On first run, mount an empty host directory and the container bootstraps a fresh config; the gateway auto-detects the dashboard from its image path.
- **No hardware passthrough by default.** GPIO / USB need explicit `--device` flags (`--device /dev/ttyUSB0`), and the container user needs matching GID for `dialout`/`gpio` groups.

## Next

- [Service management](./service.md)
- [Operations → Network deployment](../ops/network-deployment.md): tunnels, reverse proxies
