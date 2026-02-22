# Architecture Overview

## Component Diagram

```
Internet
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│  Caddy (reverse proxy + TLS termination)                    │
│  *.agents.example.com → tenant containers                   │
│  api.agents.example.com → zcplatform API                    │
└──────────────────┬───────────────────────────┬──────────────┘
                   │                           │
           Admin/SPA traffic            Tenant agent traffic
                   │                           │
                   ▼                           ▼
        ┌──────────────────┐      ┌─────────────────────────┐
        │  zcplatform      │      │  ZeroClaw containers    │
        │  (Rust/Axum)     │      │  (one per tenant)       │
        │  :8080           │      │  :10001 ... :10999      │
        │                  │      │                         │
        │  ┌────────────┐  │      │  acme    → :10001       │
        │  │ SQLite DB  │  │      │  globex  → :10042       │
        │  │ (WAL mode) │  │      │  initech → :10107       │
        │  └────────────┘  │      └─────────────────────────┘
        │                  │
        │  ┌────────────┐  │
        │  │ Vault keys │  │      ┌─────────────────────────┐
        │  │ (on disk,  │  │─────▶│  Docker daemon          │
        │  │  encrypted)│  │      │  (container lifecycle)  │
        │  └────────────┘  │      └─────────────────────────┘
        └──────────────────┘
                   ▲
                   │
        ┌──────────────────┐
        │  React SPA       │
        │  (served as      │
        │   static files   │
        │   from /static)  │
        └──────────────────┘
```

Key points:
- zcplatform and tenant containers are on the same host; containers bind to
  `127.0.0.1:{port}` only — never exposed directly to the internet
- Caddy holds the only public listener (80/443)
- Docker daemon is accessed via Unix socket (`/var/run/docker.sock`); zcplatform
  must run as a user in the `docker` group or as root
- SQLite WAL allows concurrent reads while a single writer serializes mutations

## Module Map

```
src/
├── main.rs              CLI entrypoint (bootstrap, serve subcommands)
├── lib.rs               Module exports
├── config.rs            platform.toml loading and plan tier structs
├── state.rs             AppState (DB pool, vault, Docker client, config)
├── error.rs             Unified error type → HTTP response mapping
├── email.rs             SMTP/OTP email delivery
│
├── db/
│   ├── mod.rs           DB initialization, WAL pragma, pool construction
│   ├── pool.rs          Writer + reader pool (1 writer, N readers)
│   └── migrations/      Embedded SQL migration files (applied at startup)
│
├── auth/
│   ├── mod.rs
│   ├── jwt.rs           Token minting and validation (HS256)
│   ├── otp.rs           One-time password generation and verification
│   ├── middleware.rs     Axum extractors: RequireAuth, RequireRole, RequireSuperAdmin
│   └── rate_limit.rs    Sliding-window rate limiter (per IP, per endpoint)
│
├── vault/
│   ├── mod.rs           Vault API (store, retrieve, rotate)
│   └── key.rs           XChaCha20-Poly1305 key management and versioning
│
├── rbac/                Role definitions (Viewer/Contributor/Manager/Owner)
│
├── audit/               Audit log helpers — write-only append to audit_log table
│
├── tenant/
│   ├── mod.rs
│   ├── provisioner.rs   Orchestrates the full provision/deprovision lifecycle
│   ├── config_render.rs Renders config.toml from DB values (provider, channels, tools)
│   ├── filesystem.rs    Creates/destroys tenant workspace directories
│   ├── health.rs        HTTP health check against the tenant's /health endpoint
│   ├── pairing.rs       Generates and stores the agent pairing code
│   ├── allocator.rs     Assigns free port and UID from configured ranges
│   └── slug.rs          Generates and validates tenant URL slugs
│
├── docker/
│   ├── mod.rs           Docker client wrapper (bollard)
│   ├── image.rs         Image pull and existence checks
│   ├── network.rs       Docker network creation and management
│   └── egress.rs        Optional tinyproxy container for outbound traffic control
│
├── proxy/
│   ├── mod.rs
│   └── caddy.rs         Caddy Admin API calls (add/remove routes via JSON config)
│
├── background/
│   ├── mod.rs           Spawns background tasks, broadcasts shutdown signal
│   ├── health_checker.rs    30s interval; restarts unhealthy containers
│   ├── usage_collector.rs   5min interval; scrapes /metrics, writes usage_metrics
│   └── resource_collector.rs 60s interval; docker stats → resource_snapshots
│
├── services/            Higher-level service functions used across routes
│
└── routes/
    ├── mod.rs           Router construction, middleware stack
    ├── auth_routes.rs   POST /api/auth/otp/request, POST /api/auth/otp/verify
    ├── tenant_routes.rs GET/POST/DELETE /api/tenants, POST /api/tenants/{id}/deploy
    ├── channel_routes.rs    /api/tenants/{id}/channels
    ├── member_routes.rs     /api/tenants/{id}/members
    ├── monitoring_routes.rs /api/tenants/{id}/metrics, /api/tenants/{id}/logs
    └── user_routes.rs       /api/users/me, /api/admin/users
```

Cross-cutting concerns:
- `auth/middleware.rs` injects into routes that require authentication
- `vault/` is accessed from `tenant/config_render.rs` (secret retrieval) and
  `routes/` (secret storage on config update)
- `audit/` is called from any route handler that mutates state

## Data Flow: Tenant Provisioning

```
POST /api/tenants/{id}/deploy
         │
         ▼
tenant/provisioner.rs
    │
    ├─ allocator.rs      → assign port + UID (read/write tenants table)
    ├─ filesystem.rs     → mkdir data/tenants/{slug}/{workspace,memory,logs}
    ├─ config_render.rs  → SELECT from tenant_configs + channels
    │                    → vault.retrieve() for each secret field
    │                    → render config.toml template
    │                    → write to data/tenants/{slug}/config.toml
    ├─ docker/image.rs   → ensure zeroclaw:latest is present
    ├─ docker/mod.rs     → docker run (see container spec below)
    ├─ proxy/caddy.rs    → POST http://localhost:2019/config/apps/http/servers/...
    │                    → add route: {slug}.{domain} → 127.0.0.1:{port}
    ├─ tenant/health.rs  → poll GET http://127.0.0.1:{port}/health (retry 10×, 3s)
    └─ UPDATE tenants SET status = 'running' (or 'error' on failure)
```

## Container Isolation Model

Each tenant container is started with the following Docker parameters:

```
docker run \
  --name zcplatform-{slug} \
  --network zcplatform-internal \
  --user {uid}:{uid} \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  --read-only \
  --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --memory={memory_mb}m \
  --memory-swap={memory_mb}m \         # no swap
  --cpus={cpu_limit} \
  --pids-limit=128 \
  -p 127.0.0.1:{port}:8080 \
  -v data/tenants/{slug}/workspace:/workspace:rw \
  -v data/tenants/{slug}/memory:/home/zeroclaw/.local/share/zeroclaw/memory:rw \
  -v data/tenants/{slug}/config.toml:/home/zeroclaw/.config/zeroclaw/config.toml:ro \
  zeroclaw:latest
```

Isolation properties:

| Property | Mechanism |
|---|---|
| Process isolation | Separate PID namespace (Docker default) |
| Network isolation | Docker bridge; binds only on 127.0.0.1 |
| Filesystem | Read-only rootfs + explicit rw volume mounts |
| Privilege | No capabilities, no setuid, no privilege escalation |
| Resource limits | CPU quota, memory hard limit (no swap), PID limit |
| User | Non-root UID allocated per tenant (uid_range in config) |

Volume mount purposes:
- `workspace` — agent's working directory (file tool writes here)
- `memory` — ZeroClaw's SQLite memory backend persists here
- `config.toml` (ro) — rendered at deploy time; re-rendered on config update +
  container restart

## Request Routing

Caddy configuration is managed dynamically via its Admin API (port 2019).

On tenant provision, zcplatform POSTs a route entry:

```json
{
  "match": [{ "host": ["{slug}.agents.example.com"] }],
  "handle": [{
    "handler": "reverse_proxy",
    "upstreams": [{ "dial": "127.0.0.1:{port}" }]
  }]
}
```

On tenant deletion, zcplatform DELETEs the route entry. No Caddy reload needed;
the Admin API applies changes live.

The zcplatform API itself is served on a fixed subdomain (e.g.,
`api.agents.example.com → 127.0.0.1:8080`) configured statically in the Caddy
config file at startup.

## Database Schema

Single SQLite file at `database_path` (default: `data/platform.db`), opened with
WAL mode and `PRAGMA foreign_keys = ON`.

Connection pool: 1 writer connection + 4 reader connections (configurable).
Writers are serialized via tokio Mutex. Readers use a semaphore for fair
acquisition. See `db/pool.rs`.

Key tables:

```sql
-- Core tenant record
CREATE TABLE tenants (
    id          TEXT PRIMARY KEY,          -- UUID v4
    slug        TEXT UNIQUE NOT NULL,      -- URL-safe name, e.g. "acme-corp"
    name        TEXT NOT NULL,
    status      TEXT NOT NULL,             -- draft|deploying|running|stopped|error|deleted
    plan        TEXT NOT NULL,             -- free|pro|enterprise (matches platform.toml plans)
    port        INTEGER UNIQUE,            -- assigned from port_range; NULL until deployed
    uid         INTEGER UNIQUE,            -- assigned from uid_range; NULL until deployed
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- Provider + model selection; all secret fields are vault references
CREATE TABLE tenant_configs (
    tenant_id       TEXT PRIMARY KEY REFERENCES tenants(id),
    provider_name   TEXT,                  -- e.g. "openai", "anthropic", "ollama"
    provider_model  TEXT,                  -- e.g. "gpt-4o", "claude-opus-4-6"
    provider_key_ref TEXT,                 -- vault key ID for API key
    extra_config    TEXT                   -- JSON blob for provider-specific fields
);

-- One row per channel per tenant
CREATE TABLE channels (
    id          TEXT PRIMARY KEY,
    tenant_id   TEXT NOT NULL REFERENCES tenants(id),
    channel_type TEXT NOT NULL,            -- e.g. "telegram", "discord", "slack"
    config_json TEXT NOT NULL,             -- non-secret config fields as JSON
    secret_ref  TEXT,                      -- vault key ID for token/webhook secret
    created_at  TEXT NOT NULL
);

-- Platform users (admins + tenant members)
CREATE TABLE users (
    id          TEXT PRIMARY KEY,
    email       TEXT UNIQUE NOT NULL,
    is_super_admin INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    last_login  TEXT
);

-- Tenant membership with role
CREATE TABLE members (
    id          TEXT PRIMARY KEY,
    tenant_id   TEXT NOT NULL REFERENCES tenants(id),
    user_id     TEXT NOT NULL REFERENCES users(id),
    role        TEXT NOT NULL,             -- viewer|contributor|manager|owner
    invited_by  TEXT REFERENCES users(id),
    joined_at   TEXT NOT NULL,
    UNIQUE(tenant_id, user_id)
);

-- OTP tokens (short TTL, cleaned up after use or expiry)
CREATE TABLE otp_tokens (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users(id),
    token_hash  TEXT NOT NULL,             -- bcrypt of the 6-digit code
    expires_at  TEXT NOT NULL,
    used        INTEGER NOT NULL DEFAULT 0
);

-- Hourly usage aggregates scraped from /metrics
CREATE TABLE usage_metrics (
    id          TEXT PRIMARY KEY,
    tenant_id   TEXT NOT NULL REFERENCES tenants(id),
    period_start TEXT NOT NULL,            -- ISO 8601 hour boundary
    messages_in INTEGER NOT NULL DEFAULT 0,
    messages_out INTEGER NOT NULL DEFAULT 0,
    tokens_in   INTEGER NOT NULL DEFAULT 0,
    tokens_out  INTEGER NOT NULL DEFAULT 0
);

-- 60s resource snapshots from docker stats
CREATE TABLE resource_snapshots (
    id          TEXT PRIMARY KEY,
    tenant_id   TEXT NOT NULL REFERENCES tenants(id),
    sampled_at  TEXT NOT NULL,
    cpu_percent REAL,
    memory_mb   REAL,
    disk_mb     REAL,
    net_rx_mb   REAL,
    net_tx_mb   REAL,
    pids        INTEGER
);

-- Immutable audit trail
CREATE TABLE audit_log (
    id          TEXT PRIMARY KEY,
    actor_id    TEXT REFERENCES users(id),  -- NULL for system actions
    actor_email TEXT,
    action      TEXT NOT NULL,              -- e.g. "tenant.deploy", "channel.create"
    resource_id TEXT,
    resource_type TEXT,
    ip_address  TEXT,
    detail_json TEXT,                       -- before/after or action-specific context
    created_at  TEXT NOT NULL
);

-- Vault key metadata (encrypted key material stored on disk, not in DB)
CREATE TABLE vault_keys (
    id          TEXT PRIMARY KEY,
    version     INTEGER NOT NULL,
    algorithm   TEXT NOT NULL DEFAULT 'xchacha20poly1305',
    created_at  TEXT NOT NULL,
    retired_at  TEXT                        -- NULL if still active for new encryptions
);
```

## Background Tasks

All background tasks are spawned at server startup and shut down cleanly via a
broadcast channel when the process receives SIGTERM/SIGINT.

```
background/mod.rs
    │
    ├── health_checker    interval: 30s
    │   └── for each tenant WHERE status = 'running':
    │       GET http://127.0.0.1:{port}/health
    │       on failure (3 consecutive): docker restart zcplatform-{slug}
    │       if restart fails: UPDATE tenants SET status = 'error'
    │
    ├── usage_collector   interval: 5min
    │   └── for each tenant WHERE status = 'running':
    │       GET http://127.0.0.1:{port}/metrics  (Prometheus text format)
    │       parse zeroclaw_messages_total, zeroclaw_tokens_total
    │       INSERT INTO usage_metrics (upsert on period_start)
    │
    └── resource_collector interval: 60s
        └── docker stats --no-stream --format json (all zcplatform-* containers)
            INSERT INTO resource_snapshots
```

Each task runs in its own `tokio::spawn` green thread. Shutdown signal is a
`tokio::sync::broadcast::Sender<()>`; each task selects on it and exits cleanly.

For continued reading:
- Security details (vault, auth, RBAC, container hardening): [06-security-model.md](06-security-model.md)
- Full backend module docs and config rendering pipeline: [04-backend-internals.md](04-backend-internals.md)
- Deployment and Caddy setup: [07-deployment-guide.md](07-deployment-guide.md)
- Complete DB schema with indexes and constraints: [04-backend-internals.md](04-backend-internals.md)
