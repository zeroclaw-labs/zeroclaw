# Docker & Containers

Run ZeroClaw in Docker, Podman, Kubernetes, or any OCI runtime.

## Official images

Pushed to Docker Hub on every stable release:

- `zeroclawlabs/zeroclaw:latest` — latest stable
- `zeroclawlabs/zeroclaw:v0.7.5` — pinned
- `zeroclawlabs/zeroclaw:debian` — Debian-based image (larger, broader glibc support)

Multi-arch: `linux/amd64`, `linux/arm64`.

## Minimum run

```bash
docker run -d \
  --name zeroclaw \
  -v zeroclaw-data:/zeroclaw-data \
  -p 42617:42617 \
  zeroclawlabs/zeroclaw:latest
```

The image expects persistent state at `/zeroclaw-data`. On first run, it bootstraps a default config — you still need to onboard before it's useful:

```bash
docker exec -it zeroclaw zeroclaw onboard
```

## Compose

A minimal `docker-compose.yml`:

```yaml
services:
  zeroclaw:
    image: zeroclawlabs/zeroclaw:latest
    restart: unless-stopped
    ports:
      - "42617:42617"      # gateway
    volumes:
      - ./data:/zeroclaw-data
    environment:
      ZEROCLAW_ALLOW_PUBLIC_BIND: "1"   # only if the gateway must be reachable on the LAN
```

Drop `ZEROCLAW_ALLOW_PUBLIC_BIND` if you only need local access.

## Config inside containers

The image expects config at `/zeroclaw-data/.zeroclaw/config.toml`. Mount your local config in:

```bash
docker run -d --name zeroclaw \
  -v $(pwd)/my-config.toml:/zeroclaw-data/.zeroclaw/config.toml:ro \
  -v zeroclaw-state:/zeroclaw-data/workspace \
  -p 42617:42617 \
  zeroclawlabs/zeroclaw:latest
```

For container workloads, the onboarding wizard detects Docker/Podman/Kubernetes and rewrites `localhost` references in the config to `host.docker.internal` (Docker) or other container-appropriate aliases.

## Channels that poll (Telegram, email) — just work

Outbound-initiated channels don't need any special container configuration. Telegram polling, IMAP, MQTT, Nostr relays — all pull; the container only needs egress.

## Channels that receive webhooks — need ingress

Discord, Slack, GitHub, and most webhook channels need inbound HTTP. Two options:

1. **Expose the gateway** — `-p 42617:42617` + reverse proxy with TLS in front, point the webhook URL at the public address
2. **Use a tunnel** — ngrok, Cloudflare Tunnel, or Tailscale Funnel; set the tunnel URL as the webhook target

The onboarding wizard's tunnel step handles ngrok and Cloudflare directly.

## Kubernetes

Helm chart and marketplace templates are published to the [zeroclaw-templates](https://github.com/zeroclaw-labs/zeroclaw-templates) repo. Typical manifest fragment:

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
          image: zeroclawlabs/zeroclaw:v0.7.5
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

## Gotchas

- **macOS hostname quirks (Docker Desktop, colima, Rancher Desktop).** `host.docker.internal` works out of the box on **Docker Desktop** for macOS. On **colima**, it is only reachable if you installed with `colima start --network-address` (otherwise the container can't see the host at all — connect via the VM's gateway IP, usually `192.168.5.2`, or tunnel through a shared network). **Rancher Desktop** behaves like Docker Desktop for recent versions but has had `host.docker.internal` resolve-failures on older releases. If provider calls fail with `connection refused` to `host.docker.internal`, verify with `docker run --rm alpine getent hosts host.docker.internal` — empty output means the hostname isn't resolvable and you need an explicit IP.
- **Host-side services.** If a provider is Ollama on the host, `base_url = "http://host.docker.internal:11434"` works on Docker Desktop. On Linux Docker you may need `--add-host=host.docker.internal:host-gateway`.
- **Memory persistence.** The SQLite memory file sits inside `/zeroclaw-data/workspace/`. If you don't mount that volume, every restart loses conversation history.
- **No hardware passthrough by default.** GPIO / USB need explicit `--device` flags (`--device /dev/ttyUSB0`), and the container user needs matching GID for `dialout`/`gpio` groups.

## Next

- [Service management](./service.md)
- [Operations → Network deployment](../ops/network-deployment.md) — tunnels, reverse proxies
