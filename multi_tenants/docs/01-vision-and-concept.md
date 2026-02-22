# Vision and Concept

## Problem

ZeroClaw is a single-tenant runtime. One binary, one config file, one agent.
Running ten agents for ten separate users means ten separate deployments: ten
config files, ten systemd units, ten reverse-proxy rules, ten sets of secrets to
rotate, and ten places to look when something breaks.

This does not scale. Manual deployment of N agents has O(N) operational burden
with no shared tooling for provisioning, monitoring, access control, or secret
management. Every new agent is a fresh toil cycle.

## Solution

zcplatform is a lightweight orchestration layer that sits above ZeroClaw
instances and provides:

- **Automated provisioning** — create a tenant record → platform renders config,
  provisions a Docker container, and registers a Caddy proxy route
- **Centralized operations** — single admin panel for all tenants: start, stop,
  restart, logs, config changes, usage metrics
- **Shared security primitives** — one encrypted vault, one RBAC system, one
  audit log covering all tenants

The key design constraint is **lightness**. zcplatform deliberately does not
require Kubernetes, etcd, Helm, Postgres, Redis, or a message broker. The full
platform stack runs on a $20/month VPS. The state store is SQLite with WAL mode.
The proxy is Caddy. The container runtime is plain Docker.

```
Heavy alternative:  Kubernetes + Helm + etcd + Postgres + Prometheus + Grafana
Medium alternative: docker-compose per tenant + manual nginx config
zcplatform:         SQLite + Docker API + Caddy Admin API (single binary)
```

## Target Audiences

**SaaS builders**
Offering "AI agent as a service" — zcplatform handles the multi-tenant
infrastructure so the product team can focus on the agent behavior and UX.
Each customer gets an isolated agent on a subdomain (e.g.,
`acme.agents.example.com`) without any manual provisioning work.

**Enterprise IT teams**
Internal platform serving multiple departments or teams. Each team gets its own
ZeroClaw agent with its own provider API key, channel integrations, and member
access list. Central IT manages the platform; teams self-manage their agents
within their role permissions.

**Agencies and consultants**
Managing AI agents on behalf of clients. zcplatform provides the operational
layer (provisioning, monitoring, restarts) while each client's config and secrets
remain isolated. Hand a client a pairing code and subdomain URL; zcplatform
handles everything underneath.

**Education**
Per-student or per-course AI assistants. Instructors provision agents for
students; students interact without touching infrastructure. Usage limits on the
free plan tier keep resource consumption bounded.

**Community hosting**
Shared ZeroClaw instance with isolation. Community admins provision agents for
members; container isolation and capability drops prevent cross-tenant
interference even on shared hardware.

## Value Propositions

### 1. Density (~200 tenants on 4 GB, $0.10/tenant)

ZeroClaw binaries are under 5 MB. Cold-start is ~10 ms. Each tenant container
uses ~7 MB resident memory at idle plus whatever the model provider calls consume
(those are outbound HTTP, not in-process). On a 4 GB VPS with 2 GB reserved for
the host and platform overhead, you can run ~200 tenants comfortably.

At $20/month for a 4 GB VPS:
- 100 tenants → $0.20/tenant/month infrastructure cost
- 200 tenants → $0.10/tenant/month infrastructure cost

Compare to managed alternatives where per-agent overhead alone exceeds this.

### 2. Zero-touch provisioning (<30 seconds)

The provisioning path is:

1. Admin POSTs to `/api/tenants` with name and plan → tenant record created in
   state `draft`
2. Admin POSTs to `/api/tenants/{id}/deploy` → platform:
   - Allocates port and UID from configured ranges
   - Creates filesystem workspace in `data/tenants/{slug}/`
   - Renders `config.toml` from DB values
   - Pulls Docker image if not present
   - Starts container with security flags and volume mounts
   - Registers Caddy route for `{slug}.{domain}`
   - Runs health check; transitions to `running` on success

Total time from deploy request to agent responding: under 30 seconds on a warm
Docker image. Under 60 seconds if the image needs pulling.

### 3. Security by default

- Secrets (API keys, tokens) stored via XChaCha20-Poly1305 AEAD; never written
  to disk in plaintext
- Containers start with `--cap-drop=ALL`, `--security-opt=no-new-privileges`,
  `--read-only` rootfs, and explicit tmpfs mounts for writable paths
- Authentication is passwordless OTP (no passwords to breach); sessions are
  short-lived JWTs (24h default)
- RBAC enforced at the API layer; every mutation logged to `audit_log`

### 4. Provider-agnostic

Each tenant independently selects its AI provider and model:

```toml
# Rendered into the tenant's config.toml at deploy time
[provider]
name = "anthropic"
model = "claude-opus-4-6"
# API key injected from vault — not stored in plaintext
```

Switching a tenant from OpenAI to Ollama is a config update + container restart.
No platform-level changes required.

### 5. Channel-agnostic

Each tenant configures its own channel integrations:

```toml
[[channels]]
type = "telegram"
# Bot token injected from vault

[[channels]]
type = "slack"
# Webhook URL injected from vault
```

ZeroClaw supports 15+ channels. zcplatform stores channel configs per tenant in
the `channels` table, encrypts secrets in the vault, and renders them into
`config.toml` at deploy/restart time.

## How It Works (High Level)

```
Admin creates tenant
        │
        ▼
zcplatform allocates resources
(port, UID, filesystem slug)
        │
        ▼
Config rendered from DB
→ config.toml written to tenant workspace
        │
        ▼
Docker container started
(zeroclaw binary reads config.toml on startup)
        │
        ▼
Caddy route registered
({slug}.{domain} → 127.0.0.1:{port})
        │
        ▼
Health check passes → tenant status: running
        │
        ▼
Pairing code displayed to admin
(used to authenticate the agent's first connection)
```

The platform then continuously monitors the tenant:
- `health_checker` polls every 30 seconds; auto-restarts containers that fail
- `usage_collector` scrapes `/metrics` every 5 minutes for message and token counts
- `resource_collector` samples Docker stats every 60 seconds for CPU/memory/disk

## Comparison to Alternatives

| Approach | Setup complexity | Cost/tenant | Operational burden | Isolation |
|---|---|---|---|---|
| Manual per-agent deployment | High (per agent) | Low infra, high labor | High | Poor |
| docker-compose per tenant | Medium | Low | Medium | Good |
| Kubernetes + Helm | Very high | High | Low (at scale) | Excellent |
| **zcplatform** | **Low (one binary)** | **$0.10-0.20** | **Low** | **Good** |

zcplatform occupies the "good enough isolation, minimal operational overhead,
runs anywhere Docker runs" niche. It is not the right choice for thousands of
tenants with strict SLAs and dedicated ops teams — at that scale, Kubernetes
wins. It is the right choice for 10–500 tenants where simplicity, density, and
cost matter more than enterprise orchestration features.

For continued reading:
- Architecture details: [02-architecture-overview.md](02-architecture-overview.md)
- Security model details: [06-security-model.md](06-security-model.md)
- Deploy from scratch: [07-deployment-guide.md](07-deployment-guide.md)
- Run as a commercial service: [10-service-provider-guide.md](10-service-provider-guide.md)
