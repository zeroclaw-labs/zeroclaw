# 11 — API Reference

## Overview

Base URL: `https://{platform-domain}/api`

All requests and responses use JSON. Include `Content-Type: application/json` on requests with a body.

### Authentication

Obtain a Bearer token via the OTP flow, then pass it on every authenticated request:

```
Authorization: Bearer <token>
```

### Role Hierarchy

From lowest to highest privilege within a tenant:

`Viewer < Contributor < Manager < Owner`

`super-admin` is a platform-wide flag, independent of tenant roles.

### Error Format

```json
{ "error": "human-readable message" }
```

### HTTP Status Codes

| Code | Meaning |
|------|---------|
| 200 | OK |
| 201 | Created |
| 400 | Bad Request — malformed input |
| 401 | Unauthorized — missing or invalid token |
| 403 | Forbidden — authenticated but insufficient role |
| 404 | Not Found |
| 409 | Conflict — duplicate resource |
| 429 | Rate Limited — retry after `Retry-After` header (seconds) |
| 500 | Internal Server Error |

### Rate Limiting

When the limit is exceeded the response is `429` with a `Retry-After: <seconds>` header. Back off for the indicated duration before retrying.

---

## Auth

### Request OTP

```
POST /api/auth/otp/request
Role: public
```

Request:
```json
{ "email": "user@example.com" }
```

Response `200`:
```json
{}
```

A one-time code is sent to the email address. Codes expire in 10 minutes.

```bash
curl -X POST https://platform.example.com/api/auth/otp/request \
  -H "Content-Type: application/json" \
  -d '{"email": "user@example.com"}'
```

### Verify OTP

```
POST /api/auth/otp/verify
Role: public
```

Request:
```json
{ "email": "user@example.com", "code": "123456" }
```

Response `200`:
```json
{
  "token": "eyJ...",
  "user": {
    "id": "usr_01abc",
    "email": "user@example.com",
    "is_super_admin": false,
    "tenant_roles": [
      { "tenant_id": "ten_01xyz", "role": "Owner" }
    ]
  }
}
```

```bash
curl -X POST https://platform.example.com/api/auth/otp/verify \
  -H "Content-Type: application/json" \
  -d '{"email": "user@example.com", "code": "123456"}'
```

### Get Current User

```
GET /api/auth/me
Role: authenticated
```

Response `200`: same shape as `user` object in verify response.

### Logout

```
POST /api/auth/logout
Role: authenticated
```

Response `200`: `{}`

---

## Users (super-admin only)

### List Users

```
GET /api/users
Role: super-admin
```

Response `200`: `User[]`

```json
[
  { "id": "usr_01abc", "email": "ops@example.com", "is_super_admin": true },
  { "id": "usr_02def", "email": "customer@example.com", "is_super_admin": false }
]
```

### Create User

```
POST /api/users
Role: super-admin
```

Request:
```json
{ "email": "newuser@example.com", "is_super_admin": false }
```

Response `201`: `User`

### Update User

```
PATCH /api/users/{id}
Role: super-admin
```

Request (all fields optional):
```json
{ "email": "updated@example.com", "is_super_admin": true }
```

Response `200`: `User`

### Delete User

```
DELETE /api/users/{id}
Role: super-admin
```

Response `200`: `{}`

---

## Tenants

### List Tenants

```
GET /api/tenants
Role: authenticated
```

Super-admins receive all tenants. Other users receive only tenants they are members of.

Response `200`: `Tenant[]`

```json
[
  {
    "id": "ten_01xyz",
    "name": "Acme Corp",
    "plan": "pro",
    "status": "running",
    "port": 8421,
    "created_at": "2026-02-01T00:00:00Z"
  }
]
```

### Create Tenant

```
POST /api/tenants
Role: authenticated
```

Request:
```json
{
  "name": "Acme Corp",
  "plan": "starter",
  "provider": "openai",
  "model": "gpt-4o-mini"
}
```

`provider`, `model`, and other config fields are optional at creation (draft mode). Deploy separately.

Response `201`: `Tenant`

```bash
curl -X POST https://platform.example.com/api/tenants \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name": "Acme Corp", "plan": "starter"}'
```

### Delete Tenant

```
DELETE /api/tenants/{id}
Role: Owner or super-admin
```

Stops and removes the container, then deletes the tenant record.

Response `200`: `{}`

### Deploy Tenant

```
POST /api/tenants/{id}/deploy
Role: Manager+
```

Provisions and starts the Docker container. Idempotent if already deployed.

Response `200`: `{}`

### Restart Tenant

```
POST /api/tenants/{id}/restart
Role: Manager+
```

Response `200`: `{}`

### Stop Tenant

```
POST /api/tenants/{id}/stop
Role: Manager+
```

Response `200`: `{}`

### Get Tenant Status

```
GET /api/tenants/{id}/status
Role: authenticated (tenant member)
```

Response `200`:
```json
{ "status": "running", "uptime": 86400, "port": 8421 }
```

`status` values: `draft | deploying | running | stopped | error`

### Test Provider Connection

```
POST /api/tenants/{id}/provider/test
Role: authenticated (tenant member)
```

Request:
```json
{ "provider": "openai", "model": "gpt-4o-mini", "api_key": "sk-..." }
```

Response `200`:
```json
{ "success": true }
```

On failure:
```json
{ "success": false, "error": "Invalid API key" }
```

### Get Logs

```
GET /api/tenants/{id}/logs?tail=N
Role: authenticated (tenant member)
```

`tail` defaults to 50. Max 1000.

Response `200`:
```json
["2026-02-21T03:00:01Z [INFO] Agent started", "..."]
```

### Get Pairing Code

```
GET /api/tenants/{id}/pairing-code
Role: Manager+
```

Response `200`:
```json
{ "pairing_code": "ZERO-7F3A-9B2C" }
```

### Reset Pairing Code

```
POST /api/tenants/{id}/reset-pairing
Role: Manager+
```

Response `200`:
```json
{ "pairing_code": "ZERO-1D4E-6F8A" }
```

### Execute Command

```
POST /api/tenants/{id}/exec
Role: Manager+
```

Request:
```json
{ "command": "zeroclaw status" }
```

Response `200`:
```json
{ "output": "Agent running. Uptime: 2h 14m. Provider: openai/gpt-4o-mini." }
```

All exec calls are written to the audit log.

---

## Config

### Get Config

```
GET /api/tenants/{id}/config
Role: authenticated (tenant member)
```

Response `200`:
```json
{
  "provider": "openai",
  "model": "gpt-4o-mini",
  "temperature": 0.7,
  "system_prompt": "You are a helpful assistant.",
  "tool_settings": { "shell": false, "browser": false }
}
```

API key is not returned in the response (write-only field).

### Update Config

```
PATCH /api/tenants/{id}/config
Role: Manager+
```

Request (all fields optional):
```json
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-6",
  "api_key": "sk-ant-...",
  "temperature": 0.5,
  "system_prompt": "You are a concise assistant.",
  "tool_settings": { "shell": false }
}
```

Response `200`: `TenantConfig`

---

## Channels

### List Channels

```
GET /api/tenants/{tid}/channels
Role: authenticated (tenant member)
```

Response `200`: `Channel[]` (configs returned without sensitive fields)

### Create Channel

```
POST /api/tenants/{tid}/channels
Role: Manager+
```

Request:
```json
{
  "kind": "telegram",
  "config": { "bot_token": "123:ABC...", "allowed_users": [100001] },
  "enabled": true
}
```

Response `201`: `Channel`

### Get Channel

```
GET /api/tenants/{tid}/channels/{cid}
Role: Manager+
```

Response `200`: `Channel` with decrypted config (use only over TLS).

### Update Channel

```
PATCH /api/tenants/{tid}/channels/{cid}
Role: Manager+
```

Request (all fields optional):
```json
{ "config": { "allowed_users": [100001, 100002] }, "enabled": true }
```

Response `200`: `Channel`

### Delete Channel

```
DELETE /api/tenants/{tid}/channels/{cid}
Role: Manager+
```

Response `200`: `{}`

---

## Members

### List Members

```
GET /api/tenants/{tid}/members
Role: authenticated (tenant member)
```

Response `200`:
```json
[
  { "id": "mem_01abc", "user_id": "usr_01abc", "email": "ops@example.com", "role": "Owner" }
]
```

### Add Member

```
POST /api/tenants/{tid}/members
Role: Owner or super-admin
```

Request:
```json
{ "email": "colleague@example.com", "role": "Manager" }
```

Creates the user account if it does not exist.

Response `201`: `Member`

```bash
curl -X POST https://platform.example.com/api/tenants/ten_01xyz/members \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"email": "colleague@example.com", "role": "Contributor"}'
```

### Update Member Role

```
PATCH /api/tenants/{tid}/members/{mid}
Role: Owner or super-admin
```

Request:
```json
{ "role": "Viewer" }
```

Response `200`: `Member`

### Remove Member

```
DELETE /api/tenants/{tid}/members/{mid}
Role: Owner or super-admin
```

Response `200`: `{}`

---

## Monitoring

All monitoring endpoints require `super-admin` unless noted.

### Dashboard Summary

```
GET /api/monitoring/dashboard
Role: super-admin
```

Response `200`:
```json
{
  "total_tenants": 47,
  "running": 43,
  "stopped": 3,
  "error": 1,
  "total_users": 62,
  "total_channels": 89
}
```

### Fleet Health

```
GET /api/monitoring/health
Role: super-admin
```

Response `200`: `TenantHealth[]`

```json
[
  { "tenant_id": "ten_01xyz", "name": "Acme Corp", "status": "running", "uptime": 172800 },
  { "tenant_id": "ten_02abc", "name": "Beta LLC", "status": "error", "uptime": null }
]
```

### Usage Metrics

```
GET /api/monitoring/usage?tenant_id={id}&days=30
Role: authenticated (own tenant), super-admin (any tenant)
```

`days` defaults to 30, max 90.

Response `200`: `UsageEntry[]`

```json
[
  {
    "tenant_id": "ten_01xyz",
    "hour": "2026-02-21T03:00:00Z",
    "messages_total": 142,
    "tokens_input": 28400,
    "tokens_output": 14200
  }
]
```

### Audit Log

```
GET /api/monitoring/audit?page=1&per_page=50&action=deploy&since=2026-02-01T00:00:00Z
Role: super-admin
```

All parameters are optional.

Response `200`:
```json
{
  "entries": [
    {
      "id": "aud_01abc",
      "actor_id": "usr_01abc",
      "action": "deploy",
      "tenant_id": "ten_01xyz",
      "detail": "{}",
      "created_at": "2026-02-21T03:15:00Z"
    }
  ],
  "total": 1204,
  "page": 1,
  "per_page": 50
}
```

### Resource Summary (Fleet)

```
GET /api/monitoring/resources
Role: super-admin
```

Response `200`: `ResourceSummary[]`

```json
[
  {
    "tenant_id": "ten_01xyz",
    "cpu_pct": 0.3,
    "mem_bytes": 7340032,
    "mem_limit": 536870912,
    "disk_bytes": 52428800,
    "net_in": 1024,
    "net_out": 2048,
    "pids": 4,
    "recorded_at": "2026-02-21T03:14:00Z"
  }
]
```

### Tenant Resource History

```
GET /api/tenants/{id}/resources?range=1h|6h|24h|7d
Role: authenticated (tenant member)
```

`range` defaults to `24h`.

Response `200`:
```json
{
  "current": {
    "cpu_pct": 0.3,
    "mem_bytes": 7340032,
    "disk_bytes": 52428800
  },
  "history": [
    { "ts": "2026-02-21T02:00:00Z", "cpu_pct": 0.1, "mem_bytes": 7200000 },
    { "ts": "2026-02-21T03:00:00Z", "cpu_pct": 0.3, "mem_bytes": 7340032 }
  ]
}
```
