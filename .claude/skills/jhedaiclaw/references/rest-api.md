# JhedaiClaw REST API Reference

Complete endpoint reference for the JhedaiClaw gateway HTTP API.

## Table of Contents

1. [Authentication](#authentication)
2. [Public Endpoints](#public-endpoints)
3. [Webhook](#webhook)
4. [WebSocket Chat](#websocket-chat)
5. [Status & Health](#status--health)
6. [Memory](#memory)
7. [Cron](#cron)
8. [Tools](#tools)
9. [Configuration](#configuration)
10. [Integrations](#integrations)
11. [Cost](#cost)
12. [Events (SSE)](#events-sse)
13. [Channel Webhooks](#channel-webhooks)
14. [Rate Limiting](#rate-limiting)
15. [Error Responses](#error-responses)

---

## Authentication

Three authentication mechanisms:

### Bearer Token (Primary)

```
Authorization: Bearer <token>
```

Obtained via `POST /pair`. Required for all `/api/*` endpoints when `require_pairing = true` (default).

### Webhook Secret

```
X-Webhook-Secret: <raw_secret>
```

Optional additional auth for `/webhook`. Server SHA-256 hashes and compares using constant-time comparison.

### WebSocket Token

```
ws://host:port/ws/chat?token=<bearer_token>
```

WebSocket connections pass the token as a query parameter (browsers can't set custom headers on WS handshake).

---

## Public Endpoints

### GET /health

No authentication required.

**Response 200:**

```json
{
  "status": "ok",
  "paired": true,
  "require_pairing": true,
  "runtime": {}
}
```

### GET /metrics

Prometheus text exposition format.

**Response 200:**

```
Content-Type: text/plain; version=0.0.4; charset=utf-8
```

### POST /pair

Exchange a one-time pairing code for a bearer token.

**Rate Limit:** Configurable per-minute limit per IP (default: 10/min).

**Headers:**

- `X-Pairing-Code: <code>` (required)

**Response 200 (success):**

```json
{
  "paired": true,
  "persisted": true,
  "token": "<bearer_token>",
  "message": "Save this token — use it as Authorization: Bearer <token>"
}
```

**Response 200 (persistence failure):**

```json
{
  "paired": true,
  "persisted": false,
  "token": "<bearer_token>",
  "message": "Paired for this process, but failed to persist token to config.toml..."
}
```

**Response 403:**

```json
{ "error": "Invalid pairing code" }
```

**Response 429:**

```json
{ "error": "Too many pairing requests. Please retry later.", "retry_after": 60 }
```

**Response 429 (lockout):**

```json
{
  "error": "Too many failed attempts. Try again in {lockout_secs}s.",
  "retry_after": 120
}
```

---

## Webhook

### POST /webhook

Send a message to the agent and receive a response.

**Rate Limit:** Configurable per-minute limit per IP (default: 60/min).

**Headers:**

- `Authorization: Bearer <token>` (if pairing enabled)
- `Content-Type: application/json`
- `X-Webhook-Secret: <secret>` (optional)
- `X-Idempotency-Key: <uuid>` (optional)

**Request Body:**

```json
{ "message": "your prompt here" }
```

**Response 200:**

```json
{ "response": "<llm_response>", "model": "<model_name>" }
```

**Response 200 (duplicate — idempotency key match):**

```json
{
  "status": "duplicate",
  "idempotent": true,
  "message": "Request already processed for this idempotency key"
}
```

**Response 401:**

```json
{
  "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
}
```

**Response 429:**

```json
{ "error": "Too many webhook requests. Please retry later.", "retry_after": 60 }
```

**Response 500:**

```json
{ "error": "LLM request failed" }
```

### Idempotency

- Header: `X-Idempotency-Key: <uuid>`
- TTL: configurable, default 300 seconds
- Max tracked keys: configurable, default 10,000
- Duplicate requests within TTL return `"status": "duplicate"` instead of re-processing

---

## WebSocket Chat

### GET /ws/chat?token=<bearer_token>

Streaming agent chat over WebSocket.

**Client → Server:**

```json
{ "type": "message", "content": "Hello, what's the weather?" }
```

**Server → Client (complete response):**

```json
{ "type": "done", "full_response": "The weather in San Francisco is sunny..." }
```

**Server → Client (error):**

```json
{ "type": "error", "message": "Error message here" }
```

Ignore unknown message types. Invalid JSON triggers an error response.

---

## Status & Health

### GET /api/status

**Response 200:**

```json
{
  "provider": "openrouter",
  "model": "anthropic/claude-sonnet-4",
  "temperature": 0.7,
  "uptime_seconds": 3600,
  "gateway_port": 42617,
  "locale": "en",
  "memory_backend": "sqlite",
  "paired": true,
  "channels": {
    "telegram": false,
    "discord": true,
    "slack": false
  },
  "health": {}
}
```

### GET /api/health

Component health snapshot (requires auth).

```json
{ "health": {} }
```

### GET or POST /api/doctor

Run system diagnostics.

```json
{
  "results": [
    {
      "name": "provider_connectivity",
      "severity": "ok",
      "message": "OpenRouter API reachable"
    }
  ],
  "summary": { "ok": 5, "warnings": 1, "errors": 0 }
}
```

---

## Memory

### GET /api/memory

List or search memory entries.

**Query Parameters:**

- `query` (string, optional) — search text; triggers search mode
- `category` (string, optional) — filter by category

**Response 200:**

```json
{
  "entries": [
    {
      "key": "memory_key",
      "content": "memory content",
      "category": "core",
      "timestamp": "2025-01-10T12:00:00Z"
    }
  ]
}
```

### POST /api/memory

Store a memory entry.

**Request Body:**

```json
{
  "key": "unique_key",
  "content": "memory content",
  "category": "core"
}
```

Category defaults to `"core"` if omitted. Other values: `daily`, `conversation`, or any custom string.

**Response 200:**

```json
{ "status": "ok" }
```

### DELETE /api/memory/{key}

Delete a memory entry.

**Response 200:**

```json
{ "status": "ok", "deleted": true }
```

---

## Cron

### GET /api/cron

List all scheduled jobs.

**Response 200:**

```json
{
  "jobs": [
    {
      "id": "<uuid>",
      "name": "daily-backup",
      "command": "backup.sh",
      "next_run": "2025-01-10T15:00:00Z",
      "last_run": "2025-01-09T15:00:00Z",
      "last_status": "success",
      "enabled": true
    }
  ]
}
```

### POST /api/cron

Add a new job.

**Request Body:**

```json
{
  "name": "job-name",
  "schedule": "0 9 * * *",
  "command": "command to run"
}
```

**Response 200:**

```json
{
  "status": "ok",
  "job": {
    "id": "<uuid>",
    "name": "job-name",
    "command": "command to run",
    "enabled": true
  }
}
```

### DELETE /api/cron/{id}

Remove a job.

**Response 200:**

```json
{ "status": "ok" }
```

---

## Tools

### GET /api/tools

List all registered tools with descriptions and parameter schemas.

**Response 200:**

```json
{
  "tools": [
    {
      "name": "shell",
      "description": "Execute shell commands",
      "parameters": {}
    },
    {
      "name": "file_read",
      "description": "Read file contents",
      "parameters": {}
    }
  ]
}
```

---

## Configuration

### GET /api/config

Get current config. Secrets are masked as `***MASKED***`.

**Response 200:**

```json
{ "format": "toml", "content": "<toml_string>" }
```

### PUT /api/config

Update config from TOML body. Body limit: 1 MB.

**Request Body:** Raw TOML text.

**Response 200:**

```json
{ "status": "ok" }
```

**Response 400:**

```json
{ "error": "Invalid TOML: <details>" }
```

or

```json
{ "error": "Invalid config: <validation_error>" }
```

---

## Integrations

### GET /api/integrations

List all integrations and their status.

**Response 200:**

```json
{
  "integrations": [
    {
      "name": "openrouter",
      "description": "OpenRouter LLM provider",
      "category": "providers",
      "status": "ok"
    },
    {
      "name": "telegram",
      "description": "Telegram messaging channel",
      "category": "channels",
      "status": "configured"
    }
  ]
}
```

---

## Cost

### GET /api/cost

Cost tracking summary.

**Response 200:**

```json
{
  "cost": {
    "session_cost_usd": 1.5,
    "daily_cost_usd": 5.0,
    "monthly_cost_usd": 150.0,
    "total_tokens": 50000,
    "request_count": 25,
    "by_model": { "anthropic/claude-sonnet-4": 1.5 }
  }
}
```

---

## Events (SSE)

### GET /api/events

Server-Sent Events stream. Requires bearer token.

**Content-Type:** `text/event-stream`

**Event types:**

| Type              | Fields                                                         | Description              |
| ----------------- | -------------------------------------------------------------- | ------------------------ |
| `llm_request`     | provider, model, timestamp                                     | LLM call started         |
| `tool_call_start` | tool, timestamp                                                | Tool execution started   |
| `tool_call`       | tool, duration_ms, success, timestamp                          | Tool execution completed |
| `agent_start`     | provider, model, timestamp                                     | Agent loop started       |
| `agent_end`       | provider, model, duration_ms, tokens_used, cost_usd, timestamp | Agent loop completed     |
| `error`           | component, message, timestamp                                  | Error occurred           |

**Example:**

```bash
curl -N -H "Authorization: Bearer <token>" http://127.0.0.1:42617/api/events
```

---

## Channel Webhooks

These are incoming webhook endpoints for specific messaging channels. They're set up automatically when channels are configured.

### WhatsApp (Meta Cloud API)

- `GET /whatsapp` — verification (echoes `hub.challenge`)
- `POST /whatsapp` — incoming messages (signature verified via `X-Hub-Signature-256`)

### WATI (WhatsApp Business)

- `GET /wati` — verification (echoes `challenge`)
- `POST /wati` — incoming messages

### Linq (iMessage/RCS/SMS)

- `POST /linq` — incoming messages (signature verified via `X-Webhook-Signature` + `X-Webhook-Timestamp`)

### Nextcloud Talk

- `POST /nextcloud-talk` — bot API webhook (signature verified via `X-Nextcloud-Talk-Signature`)

---

## Rate Limiting

Sliding window (60-second window), per client IP.

| Endpoint        | Default Limit |
| --------------- | ------------- |
| `POST /pair`    | 10/min        |
| `POST /webhook` | 60/min        |

If `trust_forwarded_headers` is enabled, uses `X-Forwarded-For` for client IP.

Max tracked keys: configurable (default: 10,000).

---

## Error Responses

**Standard format:**

```json
{ "error": "Human-readable error message" }
```

**With retry info:**

```json
{ "error": "...", "retry_after": 60 }
```

**Status codes:**
| Code | Meaning |
|------|---------|
| 200 | Success |
| 400 | Invalid JSON, missing fields, invalid TOML |
| 401 | Invalid/missing bearer token or webhook secret |
| 403 | Pairing verification failed |
| 404 | Endpoint or channel not configured |
| 408 | Request timeout (30s) |
| 429 | Rate limited (check `retry_after`) |
| 500 | LLM error, database error, internal failure |
