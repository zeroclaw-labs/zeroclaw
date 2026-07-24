# Gateway HTTP API

The gateway exposes a REST surface alongside the local CLI. Anything that can
be set with `zeroclaw config get/set/list/init/migrate` is also reachable via
HTTP, so the dashboard, third-party tooling, and the CLI all drive the same
underlying `Config` mutation core.

This page is a high-level overview. Field-level definitions, request and response shapes, and "Try it out" forms for the currently documented OpenAPI subset live at `/api/docs` on a running gateway. Those schemas come from runtime types, but the route inventory is assembled separately and does not yet cover every route registered by the gateway. The router in `crates/zeroclaw-gateway/src/lib.rs` remains the authority for the full live surface.

> Tracked under issue #6175.

## Authentication

The configuration value reads and mutations described on this page are gated
by the existing pairing and bearer authentication. Shape discovery through `/api/docs`,
`/api/openapi.json`, and config `OPTIONS` is public. A first-run pairing code is
printed when the daemon starts; subsequent authenticated calls send the derived
bearer token in the `Authorization` header. The Scalar explorer at `/api/docs`
exposes an "Authentication" panel where you paste the token before issuing
authenticated calls.

Local-bound by default. Over-the-network access requires TLS termination at
the gateway or in front of it; the per-property and PATCH endpoints are not
safe to expose unauthenticated regardless of TLS posture.

## Plugin webhook ingress

`GET /plugin/<path>` and `POST /plugin/<path>` are the transport boundary for a
channel plugin that advertises webhook ingress. They are not
pairing-authenticated: the plugin verifies the platform signature against its
own canonical channel config while decoding the raw headers and body. The
gateway still applies the same per-client webhook rate limiter and
trusted-forwarded-header policy as `POST /webhook`, plus the global 64 KiB
request-body ceiling.

The host removes caller-supplied copies of `x-webhook-method` and
`x-webhook-query`, then supplies those reserved headers from the actual HTTP
request. A plugin can answer a provider verification handshake without
enqueueing an agent message by returning one `__webhook_reply__` message. The
message content becomes the HTTP 200 response body when it is at most 4 KiB
(4096 UTF-8 bytes). An oversized response is rejected with the fixed 502 body
`invalid plugin webhook response`.

Public failures are deliberately fixed: `401 unauthorized webhook`,
`400 invalid webhook`, `429` admission/queue rejection, `502 invalid plugin
webhook response`, `503` unavailable, and `504 webhook processing timed out`.
Plugin rejection text and runtime traps are logged internally and never
reflected to an unauthenticated caller. Request cancellation reaches the
component parser itself; timed-out parser stores are discarded so a later
webhook cannot be blocked by the prior request.

After successful guest authentication and parsing, non-empty provider message
IDs use the gateway's existing bounded idempotency store. The reservation is
kept only after the normalized message reaches the channel queue, so an invalid,
timed-out, or undeliverable request does not consume its retry identity. If two
enabled plugin instances declare the same webhook path, every claimant is
withheld; startup iteration order never chooses an alias implicitly.

## Discovering the surface

Two endpoints answer the question "what can I do here?":

- `OPTIONS /api/config` returns the JSON Schema for the whole-config type.
  Static per build; clients should cache against the `ETag` header. Its current
  `Allow` header still lists legacy `PUT`, which the router does not register.
- `OPTIONS /api/config/prop?path=<dotted>` returns the schema fragment for a
  specific path with `Allow: GET, PUT, DELETE, OPTIONS`. Returns 404 if the
  path doesn't exist in the schema.

`OPTIONS` returns capabilities. `GET /api/config/prop` and `GET /api/config/list` return the user's current values. Forms in the dashboard issue `OPTIONS` once at load time to learn types and constraints, then `GET` to populate fields, then `PUT`/`PATCH` to write. A compatibility `GET /api/config` also returns a whole-config snapshot with secrets masked so older bundled dashboard pages do not fail against newer gateways. New clients should prefer the per-property surface because it carries field metadata and explicit secret handling.

CORS preflight requests (those carrying `Access-Control-Request-Method`) get
the standard preflight response and short-circuit before the schema body is
returned.

## Per-property CRUD

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/config` | Compatibility whole-config snapshot with secrets masked; new clients should prefer the per-property surface. |
| `PATCH` | `/api/config` | Apply a JSON Patch (RFC 6902) document atomically. |
| `OPTIONS` | `/api/config` | Whole-config JSON Schema (capabilities, not values). |
| `GET` | `/api/config/prop?path=...` | Read one field. Secrets return `{path, populated}` only. |
| `PUT` | `/api/config/prop` | Write one field. Body: `{path, value, comment?}`. Secrets respond with `{path, populated: true}` only. |
| `DELETE` | `/api/config/prop?path=...` | Reset one field to its default. Secrets respond with `{path, populated: false}`. |
| `OPTIONS` | `/api/config/prop?path=...` | Per-field schema fragment. |
| `GET` | `/api/config/list?prefix=...` | Enumerate every reachable path with type and category. Secret entries carry `{path, populated, is_secret: true}` and no value. |
| `POST` | `/api/config/init?section=...` | Instantiate `None` nested sections with defaults. Mirrors `zeroclaw config init`. |
| `POST` | `/api/config/migrate` | Apply on-disk schema migration in place. Mirrors `zeroclaw config migrate`. |

## Atomic batch writes: JSON Patch

`PATCH /api/config` accepts a JSON Patch document (RFC 6902). The supported
config operations are `add`, `replace`, `remove`, and `test`. ZeroClaw also
accepts a `comment` extension for config annotations. Config operations run
against an in-memory copy; once every operation has applied,
`Config::validate()` runs once on the result. If validation passes, the new
state is persisted and swapped in. If any operation or final validation fails,
on-disk and in-memory state are unchanged. Comment annotations are applied
after the save on a non-fatal, best-effort basis.

`move` and `copy` return `400 op_not_supported` because safe reference-graph
rewriting is not part of this surface. `test` against a `#[secret]` path is
rejected with `secret_test_forbidden`: a differential outcome would be the
only signal a client could read, and that would leak the value.

Path syntax: JSON Pointer (`/agents/researcher/model_provider`) or the
dotted form (`agents.researcher.model_provider`). Both are accepted; the
server normalises.

The CLI counterpart is `zeroclaw config patch <file-or-stdin>`, which applies
the same op set against the local Config and returns the same structured
response shape (`--json` for scripts).

## Secrets: write-only over HTTP

Per-property reads never expose secret fields (those marked `#[secret]` or
`#[derived_from_secret]` in the schema). Their responses carry
`{populated: bool}` only, with no value, length, masked stand-in, or hash. The
compatibility `GET /api/config` instead serializes the whole config after
applying `MaskSecrets`, so secret fields can appear there only as masked
placeholders. Neither config read surface returns the underlying secret value.

`PUT` and `PATCH` write the new secret value and respond with
`{populated: true}`; `DELETE` clears it and responds with
`{populated: false}`. There is no HTTP path to retrieve a secret by any means.

## Stable error codes

Errors return JSON with a stable `code` field plus a human-readable `message`.
Frontends and scripts match against the code; UI matches against the path.

| Code | Status | Meaning |
|---|---|---|
| `path_not_found` | 404 | The requested property does not exist in the schema. |
| `validation_failed` | 400 | The whole-config validator rejected the proposed state. |
| `dangling_reference` | 400 | A configured alias reference (e.g. `agents.<x>.model_provider`) names a missing target (e.g. `providers.models.<type>.<alias>`). |
| `value_type_mismatch` | 400 | The submitted JSON value cannot coerce into the target type. |
| `op_not_supported` | 400 | JSON Patch op is `move` / `copy` / unknown. |
| `secret_test_forbidden` | 400 | JSON Patch `test` op targeted a secret path. |
| `config_changed_externally` | 409 | The on-disk config drifted from the in-memory copy. (See drift detection.) |
| `reload_failed` | 500 | The save succeeded but daemon reload could not pick up the new state; on-disk reverted. |
| `internal_error` | 500 | Unclassified server-side failure. |

## Live exploration

Once a gateway is running, browse to `http://<gateway-host>:<port>/api/docs` for the Scalar API explorer. The raw specification is available at `/api/openapi.json` for other compatible viewers.

The explorer's authentication panel binds to the `bearerAuth` scheme declared
in the spec, paste your pairing-derived bearer token there before issuing
live calls. The CLI shortcut for the URL is `zeroclaw config docs`.

If the Scalar bundle can't load from the CDN (offline / air-gapped install),
the page degrades gracefully and points you at the raw spec at
`/api/openapi.json` so you can use any compatible viewer
(Insomnia, Postman, Swagger UI, etc.).

## Event stream contract

`GET /api/events` is a raw Server-Sent Events stream of observable runtime
events. It is not a deduplicated one-row-per-turn lifecycle timeline.

Gateway handlers, webhook handling, cron/heartbeat work, and agent-loop
observers can all publish lifecycle-shaped events into the same broadcast path.
Clients should treat the stream as an append-only observation log. If a
dashboard wants a compact turn timeline, it should group or deduplicate by the
identifiers present on the event payload rather than assuming each
`agent_start`, `llm_request`, or `agent_end` frame appears only once.

`GET /api/events/history` replays the retained recent events from the same
buffer, oldest first. It is a reconnect window for subscribers, not a separate
canonical lifecycle store.
