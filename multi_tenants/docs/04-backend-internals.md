# 04 — Backend Internals

## Module Dependency Graph

```
main
└── routes/
    ├── auth      → auth/{jwt, otp, rate_limit, middleware}
    ├── user      → rbac, audit
    ├── tenant    → tenant/, docker/, proxy/, rbac, audit, vault
    ├── channel   → tenant/, vault, rbac, audit
    ├── member    → rbac, audit, email
    └── monitoring→ db

auth/
├── jwt           → (none; pure crypto)
├── otp           → db, email, rate_limit
├── rate_limit    → db (otp_tokens ttl scan)
└── middleware    → jwt (validate Bearer)

tenant/
├── provisioner   → allocator, slug, filesystem, config_render, docker, proxy, db, audit
├── allocator     → db (port/UID range)
├── slug          → db (uniqueness check)
├── filesystem    → (std::fs)
├── config_render → db (tenant_configs, channels), vault
├── health        → docker (container inspect)
└── pairing       → vault (token encrypt)

docker/
├── mod           → (bollard or std::process)
├── network       → mod
├── image         → mod
└── egress        → mod (iptables rules)

vault/           → db (vault_keys), rand, chacha20poly1305
rbac/            → db (members, users)
audit/           → db (audit_log)
background/
├── health_checker  → tenant/health, db, docker
├── usage_collector → db (usage_metrics), HTTP (Prometheus scrape)
└── resource_collector → docker/mod, db (resource_snapshots), std::process (du)
email/           → lettre or SMTP config
proxy/           → caddy admin API (HTTP)
```

---

## Tenant State Machine

```
         bootstrap
             │
           draft
             │ POST /deploy
         deploying ──────────────────────┐
             │ (provisioner pipeline)    │
           running ◄──── start           │
             │                          ▼
           stopped ──── stop          error
             │                          │
             └──────────────────────────┤
                       DELETE           │
                         ▼             ▼
                       deleted ◄────────┘
```

Guard rules enforced in route handlers before calling provisioner:
- `deploy`: only from `draft` or `error`
- `stop`: only from `running`
- `start`: only from `stopped`
- `restart`: only from `running`
- `delete`: any state except `deleted`

State stored in `tenants.status TEXT` + `tenants.error_message TEXT`.

---

## Provisioning Pipeline

All steps run inside a single async task spawned by `POST /deploy`. DB status updated at each transition for UI polling.

```
1. validate
   - slug unique (SELECT COUNT FROM tenants WHERE slug=?)
   - provider config present in tenant_configs
   - plan limits valid

2. allocate
   - slug/pairing-token: slug module, pairing module
   - host port: allocator scans used ports in [20000, 30000), picks next free
   - UID/GID: allocator picks next in [10000, 65000) range

3. create filesystem
   - mkdir /var/zcplatform/tenants/{slug}/
   - mkdir .../data/ .../logs/
   - chown to allocated UID

4. render config.toml
   - config_render: SELECT from tenant_configs + channels
   - decrypt vault fields
   - template → config.toml written to tenant dir

5. create Docker container (no start yet)
   - docker create with full security flags (see below)

6. start container
   - docker start {container_id}

7. poll health
   - GET http://127.0.0.1:{port}/health every 2s, up to 60s
   - 200 OK → proceed; timeout → error state

8. register Caddy route
   - POST to Caddy admin API: add reverse_proxy route for tenant subdomain

9. update DB
   - SET status='running', started_at=NOW()
   - INSERT audit_log entry

Failure at any step: SET status='error', error_message={step + cause}
```

---

## Config Rendering

`config_render.rs` reads `tenant_configs` and `channels` rows, decrypts vault fields, and templates `config.toml`.

```toml
[gateway]
bind = "127.0.0.1:{port}"
pairing_token = "{decrypted_pairing_token}"
max_connections = {plan.max_connections}

[agent]
provider = "{provider}"
model = "{model}"

[memory]
backend = "sqlite"
path = "/data/memory.db"

[[channel]]
type = "telegram"
token = "{decrypted_token}"
# ... per-channel fields from channels rows

[env]
PROVIDER_API_KEY = "{decrypted_api_key}"
```

API keys and tokens are always written as env vars or dedicated fields; never interpolated into log-visible positions.

---

## Docker Container Creation Flags

```bash
docker create \
  --name zeroclaw-{slug} \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  --read-only \
  --tmpfs /tmp:size=64m \
  --memory={plan.memory_mb}m \
  --memory-swap={plan.memory_mb}m \
  --cpus={plan.cpus} \
  --pids-limit=128 \
  --user={uid}:{gid} \
  -v /var/zcplatform/tenants/{slug}/data:/data:rw \
  -v /var/zcplatform/tenants/{slug}/config.toml:/config.toml:ro \
  -p 127.0.0.1:{host_port}:8080 \
  --network zeroclaw-tenant \
  --restart=unless-stopped \
  --label zeroclaw.tenant={slug} \
  zeroclaw/agent:latest
```

Network: all containers on isolated `zeroclaw-tenant` bridge network. Egress module applies iptables rules limiting outbound to allowlisted hosts per plan.

---

## Database Schema

```sql
users (
  id          TEXT PRIMARY KEY,       -- UUIDv4
  email       TEXT NOT NULL UNIQUE,
  role        TEXT NOT NULL,          -- viewer|contributor|manager|owner|super_admin
  is_active   INTEGER NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL           -- ISO-8601
)

otp_tokens (
  id          TEXT PRIMARY KEY,
  user_id     TEXT NOT NULL REFERENCES users(id),
  code        TEXT NOT NULL,          -- 6-digit, bcrypt-hashed
  used        INTEGER NOT NULL DEFAULT 0,
  expires_at  TEXT NOT NULL,
  created_at  TEXT NOT NULL
)

tenants (
  id            TEXT PRIMARY KEY,
  slug          TEXT NOT NULL UNIQUE,
  name          TEXT NOT NULL,
  plan          TEXT NOT NULL,
  status        TEXT NOT NULL,        -- draft|deploying|running|stopped|error|deleted
  error_message TEXT,
  host_port     INTEGER,
  uid           INTEGER,
  container_id  TEXT,
  created_at    TEXT NOT NULL,
  started_at    TEXT,
  stopped_at    TEXT
)

tenant_configs (
  tenant_id   TEXT NOT NULL REFERENCES tenants(id),
  key         TEXT NOT NULL,
  value       TEXT NOT NULL,          -- vault-encrypted if sensitive
  PRIMARY KEY (tenant_id, key)
)

channels (
  id          TEXT PRIMARY KEY,
  tenant_id   TEXT NOT NULL REFERENCES tenants(id),
  type        TEXT NOT NULL,
  enabled     INTEGER NOT NULL DEFAULT 1,
  config      TEXT NOT NULL           -- JSON, vault-encrypted fields inline
)

members (
  tenant_id   TEXT NOT NULL REFERENCES tenants(id),
  user_id     TEXT NOT NULL REFERENCES users(id),
  role        TEXT NOT NULL,
  joined_at   TEXT NOT NULL,
  PRIMARY KEY (tenant_id, user_id)
)

audit_log (
  id          TEXT PRIMARY KEY,
  actor_id    TEXT REFERENCES users(id),
  action      TEXT NOT NULL,          -- e.g. tenant.deploy, member.invite
  resource    TEXT NOT NULL,          -- e.g. tenant:{id}
  details     TEXT,                   -- JSON blob
  ip          TEXT,
  created_at  TEXT NOT NULL
)

usage_metrics (
  id          TEXT PRIMARY KEY,
  tenant_id   TEXT NOT NULL REFERENCES tenants(id),
  ts          TEXT NOT NULL,
  requests    INTEGER,
  tokens_in   INTEGER,
  tokens_out  INTEGER,
  errors      INTEGER
)

resource_snapshots (
  id          TEXT PRIMARY KEY,
  tenant_id   TEXT NOT NULL REFERENCES tenants(id),
  ts          TEXT NOT NULL,
  cpu_pct     REAL,
  mem_bytes   INTEGER,
  disk_bytes  INTEGER
)

vault_keys (
  version     INTEGER PRIMARY KEY,
  key_hex     TEXT NOT NULL,          -- 32-byte key, hex-encoded, stored encrypted at rest
  created_at  TEXT NOT NULL,
  retired_at  TEXT
)
```

---

## Vault Encryption

Algorithm: XChaCha20-Poly1305 (24-byte nonce, 16-byte tag).

Ciphertext wire format stored in DB:

```
v{version}:{nonce_hex}:{ciphertext_hex}
```

Example: `v1:a3f0...24b:9c1d...ff`

Key versioning: `vault_keys.version` is monotonically increasing. Encrypt always uses max active version. Decrypt parses version prefix and loads corresponding key. Key rotation: insert new `vault_keys` row, re-encrypt existing values in a migration transaction.

`vault::encrypt(plaintext, db)` → ciphertext string
`vault::decrypt(ciphertext, db)` → plaintext string; returns error if key version missing or tag invalid.

---

## Background Tasks

Three independent tokio tasks spawned at server startup.

### health_checker (30s interval)

```
for each tenant WHERE status='running':
  GET http://127.0.0.1:{port}/health (timeout 5s)
  if 200 OK:
    reset consecutive_fail_count
  else:
    increment consecutive_fail_count
    if consecutive_fail_count >= 3:
      SET status='error', error_message='health check failed'
      INSERT audit_log (action=tenant.health_fail)
```

### usage_collector (5min interval)

```
for each tenant WHERE status='running':
  GET http://127.0.0.1:{port}/metrics (Prometheus text format, timeout 10s)
  parse: zeroclaw_requests_total, zeroclaw_tokens_in_total,
         zeroclaw_tokens_out_total, zeroclaw_errors_total
  INSERT usage_metrics row with delta since last scrape
```

### resource_collector (60s interval)

```
for each tenant WHERE status IN ('running','stopped'):
  docker stats --no-stream --format json {container_id}
  → cpu_pct, mem_bytes
  du -sb /var/zcplatform/tenants/{slug}/data
  → disk_bytes
  INSERT resource_snapshots row
```

---

## Audit Logging

`audit::log(db, actor_id, action, resource, details, ip)` inserts a row into `audit_log`.

Action string convention: `{noun}.{verb}` — e.g. `tenant.deploy`, `tenant.stop`, `member.invite`, `member.remove`, `config.update`, `channel.create`, `channel.delete`, `auth.otp_request`, `auth.otp_verify`.

Resource format: `{type}:{id}` — e.g. `tenant:abc123`, `user:xyz789`.

Details: JSON-serialized context. Never include plaintext secrets or full config blobs. Include only IDs, types, and non-sensitive field names.
