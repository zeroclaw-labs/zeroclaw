# Security Model — zcplatform

## 1. Authentication

### 1.1 OTP Flow

```
Client             zcplatform          SMTP           DB / Memory
  |                    |                 |                |
  |-- POST /auth/otp ->|                 |                |
  |   { email }        |                 |                |
  |                    |-- generate 6-digit OTP           |
  |                    |   code = rand 000000..999999     |
  |                    |   hash = SHA-256(code + salt)    |
  |                    |-- store (email, hash, salt, exp) ->|
  |                    |   exp = now + 600s               |
  |                    |-- send email ------------------>|
  |<-- 200 OK ---------|                                  |
  |                    |                                  |
  |-- POST /auth/verify ->                                |
  |   { email, code }  |                                  |
  |                    |-- check attempt count ----------->|
  |                    |   if > 5 in 300s: 429            |
  |                    |-- lookup (email) <---------------|
  |                    |-- SHA-256(code + salt) == hash?  |
  |                    |-- not expired?                   |
  |                    |-- delete OTP record ------------>|
  |                    |-- issue JWT                      |
  |<-- { token } ------|                                  |
```

Rate limit: sliding window, 5 attempts per email per 300 seconds. Counter stored in memory (resets on restart — acceptable for low-risk OTP).

Codes are single-use. Verifying consumes the record regardless of outcome after 5 failures.

### 1.2 JWT Structure

Algorithm: HS256. Secret: 256-bit key from `[jwt] secret` in platform.toml.

```json
{
  "sub": "user-uuid",
  "email": "user@example.com",
  "tenant_roles": [
    { "tenant_id": "tenant-uuid", "role": 2 }
  ],
  "super_admin": false,
  "iat": 1740000000,
  "exp": 1740086400
}
```

Lifetime: 24 hours (`[jwt] expiry_seconds = 86400`).

`tenant_roles` is embedded for fast path checks. However, **role enforcement always re-reads from DB** (see §2.2), so stale claims do not grant access.

### 1.3 Token Revocation

An in-memory `HashSet<String>` holds revoked JTIs. On logout or admin revocation:

1. JWT JTI is added to the revoke set.
2. All subsequent requests carrying that token receive 401.
3. The set is pruned of expired JTIs on a background timer (every 15 min).

Caveat: revoke set is lost on restart. For the 24 h window this is an acceptable risk for OTP-based auth (attacker would need to re-obtain a code to get a new token).

---

## 2. Authorization

### 2.1 RBAC Role Hierarchy

| Role        | Level | Capabilities                                                  |
|-------------|-------|---------------------------------------------------------------|
| Viewer      | 0     | Read agent logs, read tenant config (non-secret fields)       |
| Contributor | 1     | Viewer + send messages to agent, restart agent                |
| Manager     | 2     | Contributor + manage members, edit tenant config              |
| Owner       | 3     | Manager + delete tenant, rotate agent secrets, transfer ownership |
| super_admin | flag  | Bypasses all tenant-role checks; platform-wide administration |

`super_admin` is a boolean column in the `users` table, not a tenant role. It grants access to `/admin/**` routes and overrides `require_tenant_role` for all tenants.

### 2.2 Per-Request Role Check

`require_tenant_role(min_role)` middleware:

1. Extract `tenant_id` from path param.
2. Query DB: `SELECT role FROM tenant_members WHERE user_id = ? AND tenant_id = ?`.
3. If no row: 403.
4. If `role < min_role`: 403.
5. If user is `super_admin`: skip step 2-4, allow.

DB read on every request ensures immediate effect of role revocation. No caching.

### 2.3 Endpoint-Role Matrix

| Endpoint                              | Min Role    |
|---------------------------------------|-------------|
| `GET /tenants/:id/logs`               | Viewer (0)  |
| `GET /tenants/:id/status`             | Viewer (0)  |
| `POST /tenants/:id/messages`          | Contributor (1) |
| `POST /tenants/:id/restart`           | Contributor (1) |
| `PUT /tenants/:id/config`             | Manager (2) |
| `POST /tenants/:id/members`           | Manager (2) |
| `DELETE /tenants/:id/members/:uid`    | Manager (2) |
| `DELETE /tenants/:id`                 | Owner (3)   |
| `POST /tenants/:id/rotate-secret`     | Owner (3)   |
| `GET /admin/users`                    | super_admin |
| `POST /admin/plans`                   | super_admin |
| `GET /admin/tenants`                  | super_admin |

---

## 3. Secret Storage (Vault)

### 3.1 Encryption Scheme

Algorithm: XChaCha20-Poly1305 AEAD (192-bit nonce, 256-bit key, 128-bit authentication tag).

Key file: `vault.key` at path configured in `[docker] data_dir`. Permissions enforced to `0o600` on write; startup aborts if permissions are wider.

### 3.2 Encryption Format

```
v1:<base64(nonce)>:<base64(ciphertext+tag)>
```

`v1` is the key version prefix. On rotation, new entries use `v2:...`. Old `v1:` ciphertexts remain decryptable using the stored v1 key.

Plaintext is never written to disk. All secrets (agent API keys, channel tokens, SMTP credentials per tenant) are stored in the `secrets` table as versioned ciphertext.

### 3.3 Key Rotation Procedure

```bash
# 1. Run rotate-key — generates new key version, appends to vault.key
zcplatform rotate-key --config platform.toml

# 2. Old key versions are retained in vault.key for decryption
# 3. New secrets written after rotation use the new key version
# 4. Existing secrets are re-encrypted lazily on next write, or eagerly:
zcplatform rotate-key --config platform.toml --reencrypt-all
```

The key file is a newline-delimited list of `version:base64_key` entries. Decryption tries each version matching the ciphertext prefix.

---

## 4. Container Isolation

### 4.1 Docker Security Flags

Every tenant container is launched with:

```
--cap-drop=ALL
--security-opt=no-new-privileges
--read-only
--user <allocated_uid>:65534
--memory=<limit>
--memory-swap=<limit>        # equal to --memory: no swap
--cpus=<limit>
--pids-limit=<limit>
--network zeroclaw-internal
-p 127.0.0.1:<port>:8080
```

| Flag                          | Purpose                                                      |
|-------------------------------|--------------------------------------------------------------|
| `--cap-drop=ALL`              | Remove all Linux capabilities; prevents privilege escalation |
| `--security-opt=no-new-privileges` | Blocks `setuid`/`setgid` binaries from gaining caps     |
| `--read-only`                 | Rootfs immutable; agent cannot modify its own binary         |
| `--user <uid>`                | Non-root; allocated from UID range, no host user mapping     |
| `--memory == --memory-swap`   | Disables swap; prevents memory DoS spilling to disk          |
| `--cpus`                      | CPU quota; prevents CPU starvation of other tenants          |
| `--pids-limit`                | Fork bomb prevention                                         |
| `-p 127.0.0.1:...`            | Port bound to loopback only; not reachable from outside host |

Writable paths are mounted explicitly as named volumes (e.g., `/data`, `/tmp`). The rootfs itself remains read-only.

### 4.2 Resource Limits as DoS Prevention

Per-plan limits (defined in `[plans.*]`) translate directly to Docker flags. A tenant that exhausts its memory limit is OOM-killed by the kernel; it cannot affect adjacent containers. CPU limits use the CFS scheduler quota.

---

## 5. Network Security

- **Loopback binding**: zcplatform API server binds to `127.0.0.1:8080`. Not accessible externally.
- **Caddy TLS termination**: Caddy handles HTTPS (wildcard cert via ACME), forwards to zcplatform on localhost. Caddy's admin API should also be loopback-only (`localhost:2019`).
- **Tenant network isolation**: All tenant containers share `zeroclaw-internal` Docker bridge. Containers can reach zcplatform's internal API but cannot reach each other by design (no inter-container routes needed; disable ICC if stricter isolation is required: `--icc=false` on the bridge).
- **External network**: A separate `zeroclaw-external` network connects Caddy to tenant containers for ingress. Tenant containers are NOT on the host network.
- **Egress proxy (optional)**: A `tinyproxy` container can be placed on `zeroclaw-internal`. Set `HTTP_PROXY` / `HTTPS_PROXY` env vars in tenant containers to route outbound traffic through it for logging/blocking.

---

## 6. Audit Trail

All state-changing operations write to the `audit_log` table:

```sql
CREATE TABLE audit_log (
    id          INTEGER PRIMARY KEY,
    ts          INTEGER NOT NULL,   -- unix epoch ms
    actor_id    TEXT,               -- user UUID or NULL (system)
    actor_email TEXT,
    tenant_id   TEXT,
    action      TEXT NOT NULL,      -- e.g. "tenant.create", "member.role_change"
    target_id   TEXT,               -- resource UUID
    detail      TEXT                -- JSON blob, no secrets
);
```

Logged actions include: auth attempts (success/fail), tenant create/delete, member add/remove/role-change, container start/stop/restart, config update, key rotation, backup, secret access.

Secrets are never written to `detail`. Queryable by `actor_id`, `tenant_id`, `action`, and time range via admin API.

Retention: no automatic pruning; operators are responsible for archiving. `zcplatform backup` includes `audit_log`.

---

## 7. Threat Model

### What zcplatform protects against

| Threat                        | Mechanism                                                    |
|-------------------------------|--------------------------------------------------------------|
| Brute-force OTP               | Rate limit: 5 attempts / 300 s per email                    |
| Stolen JWT                    | 24 h expiry; revocation set; rotate vault key to invalidate |
| Privilege escalation (RBAC)   | DB-fresh role check on every request; no cached claims       |
| Secret leak from DB           | All secrets encrypted at rest; key not in DB                 |
| Tenant container escape       | `--cap-drop=ALL`, `--no-new-privileges`, read-only rootfs    |
| Resource abuse / noisy neighbor | Per-plan CPU/memory/pids limits enforced by kernel         |
| External port exposure        | All ports bound to 127.0.0.1; Caddy is sole ingress         |
| Tenant-to-tenant traffic      | Separate Docker networks; no ICC between tenant containers   |

### What zcplatform does NOT protect against

- **Host OS compromise**: If the host is rooted, containers can be escaped regardless.
- **Caddy vulnerability**: Caddy is the TLS boundary; a Caddy exploit bypasses network controls.
- **Docker daemon compromise**: A compromised Docker daemon can rewrite container configs.
- **Supply chain attack on tenant image**: The base ZeroClaw image must be trusted and reproducibly built.
- **Operator misuse**: A `super_admin` can read any tenant's data by design.
- **SMTP interception**: OTP delivery depends on SMTP transport security (use TLS SMTP).

Operators deploying in high-security environments should layer additional host hardening (seccomp profiles, AppArmor policies, rootless Docker, dedicated VM per tenant) beyond zcplatform's defaults.
