# SOP Connectivity & Event Fan-In

This document describes how external events trigger SOP runs.

## Quick Paths

- [MQTT Integration](#2-mqtt-integration)
- [Webhook Integration](#3-webhook-integration)
- [Cron Integration](#4-cron-integration)
- [Security Defaults](#5-security-defaults)
- [Troubleshooting](#6-troubleshooting)

## 1. Overview

ZeroClaw routes MQTT/webhook/cron/peripheral events through a unified SOP dispatcher (`dispatch_sop_event`).

Key behaviors:

- **Consistent trigger matching:** one matcher path for all event sources.
- **Run-start audit:** started runs are persisted via `SopAuditLogger`.
- **Headless safety:** in non-agent-loop contexts, `ExecuteStep` actions are logged as pending (not silently executed).

## 2. MQTT Integration

### 2.1 Configuration

Configure broker access in `config.toml`:

```toml
[channels_config.mqtt]
broker_url = "mqtts://broker.example.com:8883"  # use mqtt:// for plaintext
client_id = "zeroclaw-agent-1"
topics = ["sensors/alert", "ops/deploy/#"]
qos = 1
username = "mqtt-user"      # optional
password = "mqtt-password"  # optional
use_tls = true              # must match scheme (mqtts:// => true)
```

### 2.2 Trigger Definition

In `SOP.toml`:

```toml
[[triggers]]
type = "mqtt"
topic = "sensors/alert"
condition = "$.severity >= 2"
```

MQTT payload is forwarded into SOP event payload (`event.payload`), then shown in step context.

## 3. Webhook Integration

### 3.1 Endpoints

- **`POST /sop/{*rest}`**: SOP-only endpoint. Returns `404` if no SOP matches. No LLM fallback.
- **`POST /webhook`**: chat endpoint. It attempts SOP dispatch first; if no match, falls back to normal LLM flow.

Path matching is exact against configured webhook trigger path.

Example:

- Trigger path in SOP: `path = "/sop/deploy"`
- Matching request: `POST /sop/deploy`

### 3.2 Authorization

When pairing is enabled (default), provide:

1. `Authorization: Bearer <token>` (from `POST /pair`)
2. Optional second layer: `X-Webhook-Secret: <secret>` when webhook secret is configured

### 3.3 Idempotency

Use:

`X-Idempotency-Key: <unique-key>`

Defaults:

- TTL: 300s
- Duplicate response: `200 OK` with `"status": "duplicate"`

Idempotency keys are namespaced per endpoint (`/webhook` vs `/sop/*`).

### 3.4 Example Request

```bash
curl -X POST http://127.0.0.1:3000/sop/deploy \
  -H "Authorization: Bearer <token>" \
  -H "X-Idempotency-Key: $(uuidgen)" \
  -H "Content-Type: application/json" \
  -d '{"message":"deploy-service-a"}'
```

Typical response:

```json
{
  "status": "accepted",
  "matched_sops": ["deploy-pipeline"],
  "source": "sop_webhook",
  "path": "/sop/deploy"
}
```

## 4. Cron Integration

The scheduler evaluates cached cron triggers using a window-based check.

- **Window-based:** events within `(last_check, now]` are not missed.
- **At-most-once per expression per tick:** if multiple fire points are in one poll window, dispatch happens once.

Trigger example:

```toml
[[triggers]]
type = "cron"
expression = "0 0 8 * * *"
```

Cron expressions support 5, 6, or 7 fields.

## 5. Security Defaults

| Feature | Mechanism |
|---|---|
| **MQTT transport** | `mqtts://` + `use_tls = true` for TLS transport |
| **Webhook auth** | Pairing bearer token (default required), optional shared secret header |
| **Rate limiting** | Per-client limits on webhook routes (`webhook_rate_limit_per_minute`, default `60`) |
| **Idempotency** | Header-based dedup (`X-Idempotency-Key`, default TTL `300s`) |
| **Cron validation** | Invalid cron expressions fail closed during parsing/cache build |

## 6. Troubleshooting

| Symptom | Likely Cause | Fix |
|---|---|---|
| **MQTT** connection errors | broker URL/TLS mismatch | Verify scheme + TLS flag pairing (`mqtt://`/`false`, `mqtts://`/`true`) |
| **Webhook** `401 Unauthorized` | missing bearer or invalid secret | re-pair token (`POST /pair`) and verify `X-Webhook-Secret` if configured |
| **`/sop/*` returns 404** | trigger path mismatch | ensure `SOP.toml` uses exact path (for example `/sop/deploy`) |
| **SOP started but step not executed** | headless trigger without active agent loop | run an agent loop for `ExecuteStep`, or design run to pause on approvals |
| **Cron not firing** | daemon not running or invalid expression | run `zeroclaw daemon`; check logs for cron parse warnings |
