# Configuration Reference — zcplatform

All configuration lives in a single TOML file (default: `platform.toml`).
Pass a custom path with `--config /path/to/platform.toml` on any subcommand.

---

## [server]

| Key          | Type   | Default         | Description                                                        |
|--------------|--------|-----------------|--------------------------------------------------------------------|
| `host`       | string | `"127.0.0.1"`   | Bind address. Keep loopback; Caddy is the external-facing ingress. |
| `port`       | u16    | `8080`          | Listen port.                                                       |
| `public_url` | string | _(required)_    | Base URL reachable by users. Used in OTP email links and Caddy route injection. Example: `"https://platform.example.com"` |

---

## [database]

| Key    | Type   | Default       | Description                            |
|--------|--------|---------------|----------------------------------------|
| `path` | string | _(required)_  | Absolute path to SQLite database file. Created by `bootstrap` if absent. |

---

## [docker]

| Key                   | Type   | Default                  | Description                                                               |
|-----------------------|--------|--------------------------|---------------------------------------------------------------------------|
| `image`               | string | `"zeroclaw-agent:latest"`| Docker image used for tenant containers. Must be pre-pulled or built via `zcplatform build-image`. |
| `network_internal`    | string | `"zeroclaw-internal"`    | Bridge network for zcplatform ↔ container communication. Created by `bootstrap`. |
| `network_external`    | string | `"zeroclaw-external"`    | Bridge network connecting Caddy to tenant containers. Created by `bootstrap`. |
| `port_range_start`    | u16    | `32000`                  | Start of host port range allocated to tenant containers (inclusive).      |
| `port_range_end`      | u16    | `33000`                  | End of host port range (inclusive). Range must be ≥ max expected tenants. |
| `uid_range_start`     | u32    | `200000`                 | Start of UID range for non-root container users.                          |
| `uid_range_end`       | u32    | `299999`                 | End of UID range. Each tenant gets a unique allocated UID.                |
| `data_dir`            | string | `"/var/lib/zcplatform/tenants"` | Host directory for tenant volume mounts. Each tenant gets a subdirectory. |
| `vault_key_path`      | string | `"/etc/zcplatform/vault.key"` | Path to vault key file. Must exist with `0o600` permissions at startup. |
| `pids_limit`          | i64    | `128`                    | Default PID limit for containers (overridden per plan).                   |

---

## [proxy]

| Key          | Type   | Default  | Description                                                              |
|--------------|--------|----------|--------------------------------------------------------------------------|
| `enabled`    | bool   | `false`  | Launch a `tinyproxy` egress proxy container on the internal network.     |
| `image`      | string | `"tinyproxy:latest"` | Docker image for the proxy container (only used if `enabled = true`). |
| `http_proxy` | string | `""`     | Proxy URL injected as `HTTP_PROXY` / `HTTPS_PROXY` into tenant containers. Example: `"http://proxy:8888"` |

---

## [jwt]

| Key              | Type   | Default     | Description                                                              |
|------------------|--------|-------------|--------------------------------------------------------------------------|
| `secret`         | string | _(required)_| HS256 signing secret. Minimum 32 bytes. Generate: `openssl rand -hex 32`. Changing this invalidates all existing tokens. |
| `expiry_seconds` | u64    | `86400`     | Token lifetime in seconds (default: 24 hours).                           |

---

## [smtp]

| Key        | Type   | Default     | Description                                              |
|------------|--------|-------------|----------------------------------------------------------|
| `host`     | string | _(required)_| SMTP server hostname.                                    |
| `port`     | u16    | `587`       | SMTP port. Use 587 for STARTTLS, 465 for implicit TLS.   |
| `username` | string | _(required)_| SMTP authentication username.                            |
| `password` | string | _(required)_| SMTP authentication password.                            |
| `from`     | string | _(required)_| Sender address. Example: `"ZeroClaw <noreply@example.com>"` |
| `starttls` | bool   | `true`      | Use STARTTLS upgrade. Set `false` if using implicit TLS on port 465. |

---

## [plans.*]

Plans define resource quotas per tenant tier. Multiple plans are supported; name them anything.

| Key                    | Type   | Default | Description                                                             |
|------------------------|--------|---------|-------------------------------------------------------------------------|
| `max_channels`         | u32    | —       | Maximum number of channels (Telegram bots, Discord bots, etc.) the tenant agent may configure. |
| `max_members`          | u32    | —       | Maximum users in the tenant workspace.                                  |
| `max_messages_per_day` | u64    | —       | Daily message throughput cap. Enforced by the agent runtime.            |
| `cpu_limit`            | string | —       | Docker `--cpus` value. Example: `"0.5"` = half a CPU core.             |
| `memory_limit_mb`      | u64    | —       | Docker `--memory` in megabytes. `--memory-swap` is set equal (no swap). |
| `pids_limit`           | i64    | `128`   | Docker `--pids-limit`. Prevents fork bombs.                             |

Example — two-tier setup:

```toml
[plans.free]
max_channels          = 2
max_members           = 3
max_messages_per_day  = 500
cpu_limit             = "0.25"
memory_limit_mb       = 256
pids_limit            = 64

[plans.pro]
max_channels          = 10
max_members           = 20
max_messages_per_day  = 10000
cpu_limit             = "1.0"
memory_limit_mb       = 1024
pids_limit            = 128

[plans.enterprise]
max_channels          = 50
max_members           = 100
max_messages_per_day  = 100000
cpu_limit             = "4.0"
memory_limit_mb       = 4096
pids_limit            = 256
```

Plan assignment: when creating a tenant via API, pass `"plan": "pro"`. The platform looks up `[plans.pro]` and applies those limits to the container launch.

---

## Environment Variable Overrides

A limited set of sensitive values can be overridden via environment variables (useful in CI / secrets managers):

| Env var                       | Overrides                  |
|-------------------------------|----------------------------|
| `ZCPLATFORM_JWT_SECRET`       | `[jwt] secret`             |
| `ZCPLATFORM_SMTP_PASSWORD`    | `[smtp] password`          |
| `ZCPLATFORM_DB_PATH`          | `[database] path`          |
| `ZCPLATFORM_VAULT_KEY_PATH`   | `[docker] vault_key_path`  |

Environment variables take precedence over `platform.toml` values. All other keys must be set in the TOML file.

---

## Minimal Config

Smallest valid configuration (all required keys; suitable for local testing):

```toml
[server]
public_url = "http://localhost:8080"

[database]
path = "/tmp/zcplatform-test.db"

[docker]
vault_key_path = "/tmp/vault.key"

[jwt]
secret = "0000000000000000000000000000000000000000000000000000000000000000"

[smtp]
host     = "localhost"
port     = 1025
username = "test"
password = "test"
from     = "test@localhost"

[plans.default]
max_channels          = 5
max_members           = 10
max_messages_per_day  = 1000
cpu_limit             = "0.5"
memory_limit_mb       = 512
pids_limit            = 64
```

---

## Full Config (All Options)

```toml
[server]
host       = "127.0.0.1"
port       = 8080
public_url = "https://platform.example.com"

[database]
path = "/var/lib/zcplatform/platform.db"

[docker]
image               = "zeroclaw-agent:latest"
network_internal    = "zeroclaw-internal"
network_external    = "zeroclaw-external"
port_range_start    = 32000
port_range_end      = 33000
uid_range_start     = 200000
uid_range_end       = 299999
data_dir            = "/var/lib/zcplatform/tenants"
vault_key_path      = "/etc/zcplatform/vault.key"
pids_limit          = 128

[proxy]
enabled    = true
image      = "tinyproxy:latest"
http_proxy = "http://proxy:8888"

[jwt]
secret         = "replace-with-openssl-rand-hex-32-output"
expiry_seconds = 86400

[smtp]
host     = "smtp.example.com"
port     = 587
username = "noreply@example.com"
password = "smtp-app-password"
from     = "ZeroClaw Platform <noreply@example.com>"
starttls = true

[plans.free]
max_channels          = 2
max_members           = 3
max_messages_per_day  = 500
cpu_limit             = "0.25"
memory_limit_mb       = 256
pids_limit            = 64

[plans.pro]
max_channels          = 10
max_members           = 20
max_messages_per_day  = 10000
cpu_limit             = "1.0"
memory_limit_mb       = 1024
pids_limit            = 128
```
