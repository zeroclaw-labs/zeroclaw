# AGENTS.md — gateway/

> REST API, WebSocket, SSE, and pairing server. **HIGH RISK** — public-facing interface.

## Overview

Axum-based HTTP gateway serving the web dashboard, channel webhooks, real-time WebSocket chat, SSE events, node discovery, and live canvas. All state lives in `AppState` (single struct, ~30 fields, passed via `State<AppState>`). Body limit: 64 KB default, 1 MB for `PUT /api/config`. Request timeout: 30s default, overridable via `ZEROCLAW_GATEWAY_TIMEOUT_SECS`. No CORS middleware — the gateway is designed for same-origin or tunneled access.

## Key Files

| File | Responsibility |
|---|---|
| `mod.rs` | Server setup, `AppState`, route wiring, rate limiter, idempotency store, webhook/channel handlers |
| `api.rs` | `/api/*` REST handlers (status, config, memory, cron, tools, sessions, cost, doctor) |
| `api_pairing.rs` | Device registry (SQLite-backed), enhanced pairing flow, token rotation, device revocation |
| `api_plugins.rs` | `GET /api/plugins` — feature-gated behind `plugins-wasm` |
| `ws.rs` | `/ws/chat` — WebSocket agent chat with session persistence |
| `sse.rs` | `/api/events` — SSE stream wrapping `BroadcastObserver` |
| `nodes.rs` | `/ws/nodes` — dynamic node discovery, capability registration, remote invocation |
| `canvas.rs` | `/api/canvas/*` + `/ws/canvas/:id` — Live Canvas (A2UI) REST + real-time WebSocket |
| `hardware_context.rs` | `/api/hardware/*` — GPIO pin registration, append-only hardware context files |
| `static_files.rs` | `rust-embed` serving `web/dist/`, SPA fallback with `__ZEROCLAW_BASE__` injection |

## Route Map

**Public (no auth):** `/health`, `/metrics`, `/pair`, channel webhooks (`/webhook`, `/whatsapp`, `/linq`, `/wati`, `/nextcloud-talk`, `/webhook/gmail`).
**Bearer-protected (`/api/*`):** status, config (GET/PUT), memory (GET/POST/DELETE), cron (CRUD), tools, sessions, cost, doctor, devices, canvas, plugins, events (SSE), hardware.
**WebSocket:** `/ws/chat`, `/ws/nodes`, `/ws/canvas/:id`.
**Admin (localhost):** `/admin/shutdown`, `/admin/paircode`, `/admin/paircode/new`.
**Static:** `/_app/*` assets, SPA fallback for all other GETs.

When `gateway.path_prefix` is set, all routes are nested under that prefix via `Router::nest()`. A trailing-slash redirect is wired separately.

## Authentication

- **PairingGuard** (`security::pairing`): generates one-time pairing codes, issues bearer tokens, stores SHA-256 hashed tokens. `require_pairing = false` disables all auth.
- **Bearer token extraction**: `Authorization: Bearer <token>` header. `require_auth()` in `api.rs` is the canonical pattern — returns `Result<(), (StatusCode, Json)>`.
- **WebSocket auth** has three paths (precedence order): `Authorization` header, `Sec-WebSocket-Protocol: bearer.<token>` subprotocol, `?token=` query param. Browsers cannot set custom WS headers, so subprotocol/query paths are required.
- **Webhook secret**: `X-Webhook-Secret` header, compared via `constant_time_eq` against SHA-256 hash stored in `AppState`. Never stored in plaintext.
- **Channel-specific verification**: WhatsApp uses `X-Hub-Signature-256` (HMAC-SHA256), Linq uses signing secret, Nextcloud Talk uses webhook secret.

## WebSocket Protocol

### `/ws/chat` (subprotocol: `zeroclaw.v1`)

```
Server->Client: {"type":"session_start","session_id":"...","resumed":true,"message_count":42}
Client->Server: {"type":"connect","session_id":"...","device_name":"...","capabilities":[...]}  (optional first frame)
Client->Server: {"type":"message","content":"Hello"}
Server->Client: {"type":"chunk","content":"Hi! "}
Server->Client: {"type":"tool_call","name":"shell","args":{...}}
Server->Client: {"type":"tool_result","name":"shell","output":"..."}
Server->Client: {"type":"done","full_response":"..."}
```
First frame can be `{"type":"connect",...}` for session/device negotiation or `{"type":"message",...}` (backward-compatible). Session ID from query param `?session_id=` or connect frame. Sessions optionally persisted to SQLite.

### `/ws/nodes` (subprotocol: `zeroclaw.nodes.v1`)

```
Node->Gateway:    {"type":"register","node_id":"phone-1","capabilities":[{"name":"camera.snap",...}]}
Gateway->Node:    {"type":"registered","node_id":"phone-1","capabilities_count":1}
Gateway->Node:    {"type":"invoke","call_id":"uuid","capability":"camera.snap","args":{...}}
Node->Gateway:    {"type":"result","call_id":"uuid","success":true,"output":"..."}
```
Nodes register capabilities that become dynamically available tools for the agent. `NodeRegistry` tracks connected nodes; invocations use `mpsc` + `oneshot` channels.

## Rate Limiting & Idempotency

- **Sliding-window rate limiter**: separate limits for `/pair` and `/webhook` (per IP, configurable per-minute). Periodic stale sweep every 5 min. LRU eviction when key cardinality exceeds `rate_limit_max_keys` (default 10,000).
- **Idempotency store**: `X-Idempotency-Key` header on webhooks. TTL-based, same max-key eviction. Returns `200 {"status":"duplicate"}` for repeated keys.
- **Client IP resolution**: `X-Forwarded-For` > `X-Real-IP` > peer addr. Only trusted when `trust_forwarded_headers = true`.

## Security Considerations

- **Public bind refused** unless tunnel configured or `allow_public_bind = true`. Enforced at startup.
- **Body limit**: 64 KB global via `RequestBodyLimitLayer`, 1 MB for config PUT only.
- **Timeout**: 30s default via `TimeoutLayer` (prevents slow-loris). Override with `ZEROCLAW_GATEWAY_TIMEOUT_SECS`.
- **Path traversal**: hardware endpoint validates device aliases (alphanumeric + hyphens/underscores only).
- **Append-only writes**: hardware context endpoint uses `OpenOptions::append(true)`, capped at 32 KB per payload.
- **Secret masking**: `GET /api/config` masks API keys with `***MASKED***`; PUT hydrates masked fields from current config.
- No CORS headers are set — rely on same-origin or tunnel. Adding CORS requires careful scoping (do not allow `*` with credentials).

## Testing Patterns

- Tests live at the bottom of `mod.rs` (module-level `#[cfg(test)]`).
- `AppState` is constructed with mock/stub providers, in-memory memory backends, and disabled pairing for most tests.
- Webhook tests exercise idempotency key dedup, secret validation, rate limiting.
- Use `axum::test::TestClient` or direct handler calls via `oneshot`.
- Always test both auth-required and auth-bypassed paths.

## Common Gotchas

1. **`require_auth` pattern is duplicated** — `api.rs`, `hardware_context.rs`, `sse.rs`, and `canvas.rs` each have their own copy. If you change auth logic, check all four.
2. **Config PUT hydration** — incoming TOML with `***MASKED***` values must be hydrated from the current config before saving. Missing this leaks masked strings to disk.
3. **WebSocket auth is different from REST auth** — three extraction paths with different precedence. Do not assume `Authorization` header works for browser WS clients.
4. **`plugins-wasm` feature gate** — `api_plugins.rs` is conditionally compiled. Adding non-gated references to it will break default builds.
5. **Path prefix** — all routes are nested under `path_prefix` when set. Hardcoding absolute paths in handlers will break reverse-proxy deployments.
6. **Body limit layering** — the 1 MB config PUT limit works because it is merged as a separate router before the 64 KB global layer applies.

## Cross-Subsystem Coupling

- `security::pairing` — `PairingGuard`, `constant_time_eq`, `is_public_bind`
- `channels/` — `WhatsAppChannel`, `LinqChannel`, `WatiChannel`, `NextcloudTalkChannel`, `GmailPushChannel`, `SessionBackend`
- `tools/` — `ToolSpec`, `CanvasStore`, MCP registry wiring
- `memory/` — `Memory` trait for chat/webhook memory storage
- `observability/` — `Observer` trait, `BroadcastObserver` wrapping for SSE
- `agent/` — agent loop invoked from webhook/WS handlers
- `config/` — `Config` struct, `gateway.*` section, `channels_config.*`
- `cron/` — cron CRUD API handlers read/write cron state
- `hooks/` — `HookRunner` fires `gateway_start` hook on boot
