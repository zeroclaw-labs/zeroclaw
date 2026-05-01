# Gateway HTTP API

The gateway exposes a REST surface alongside the local CLI. Anything that can
be set with `zeroclaw config get/set/list/init/migrate` is also reachable via
HTTP, so the dashboard, third-party tooling, and the CLI all drive the same
underlying `Config` mutation core.

This page is a high-level overview. Field-level definitions, request and
response shapes, and "Try it out" forms are generated from the runtime types
and live at `/api/docs` on a running gateway. The generator is the same set of
schemas the daemon enforces, so the docs cannot drift from the implementation.

> Tracked under issue #6175.

## Authentication

Every `/api/*` route is gated by the existing pairing/bearer auth. A first-run
pairing code is printed when the daemon starts; subsequent calls send the
derived bearer token in the `Authorization` header. The Scalar explorer at
`/api/docs` exposes an "Authentication" panel where you paste the token before
issuing live calls.

Local-bound by default. Over-the-network access requires TLS termination at
the gateway or in front of it; the per-property and PATCH endpoints are not
safe to expose unauthenticated regardless of TLS posture.

## Discovering the surface

Two endpoints answer the question "what can I do here?":

- `OPTIONS /api/config` returns the JSON Schema for the whole-config type and
  an `Allow` header listing the methods supported on the resource. Static per
  build; clients should cache against the `ETag` header.
- `OPTIONS /api/config/prop?path=<dotted>` returns the schema fragment for a
  specific path with `Allow: GET, PUT, DELETE, OPTIONS`. Returns 404 if the
  path doesn't exist in the schema.

`OPTIONS` returns capabilities. `GET /api/config/prop` and `GET /api/config/list`
return the user's current values. Forms in the dashboard issue `OPTIONS` once
at load time to learn types and constraints, then `GET` to populate fields,
then `PUT`/`PATCH` to write. There is no whole-file `GET /api/config` —
deliberately. Walk the per-property surface; the schema is the source of truth
for what fields exist.

CORS preflight requests (those carrying `Access-Control-Request-Method`) get
the standard preflight response and short-circuit before the schema body is
returned.

## Per-property CRUD

| Method | Path | Purpose |
|---|---|---|
| `PATCH` | `/api/config` | Apply a JSON Patch (RFC 6902) document atomically. |
| `OPTIONS` | `/api/config` | Whole-config JSON Schema (capabilities, not values). |
| `GET` | `/api/config/prop?path=...` | Read one field. Secrets return `{path, populated}` only. |
| `PUT` | `/api/config/prop` | Write one field. Body: `{path, value, comment?}`. Secrets respond with `{path, populated: true}` only. |
| `DELETE` | `/api/config/prop?path=...` | Reset one field to its default. Secrets respond with `{path, populated: false}`. |
| `OPTIONS` | `/api/config/prop?path=...` | Per-field schema fragment. |
| `GET` | `/api/config/list?prefix=...` | Enumerate every reachable path with type and category. Secret entries carry `{path, populated, is_secret: true}` and no value. |
| `POST` | `/api/config/init?section=...` | Instantiate `None` nested sections with defaults. Mirrors `zeroclaw config init`. |
| `POST` | `/api/config/migrate` | Apply on-disk schema migration in place. Mirrors `zeroclaw config migrate`. |

## Atomic batch writes — JSON Patch

`PATCH /api/config` accepts a JSON Patch document (RFC 6902). The supported op
subset is `add`, `replace`, `remove`, `test`. Each op runs against an
in-memory copy of the config; once every op has applied, `Config::validate()`
runs once on the result. If validation passes, the new state is persisted and
swapped in. If any op or the final validation fails, on-disk and in-memory
state are unchanged.

`move` and `copy` return `400 op_not_supported` because safe reference-graph
rewriting is not part of this surface. `test` against a `#[secret]` path is
rejected with `secret_test_forbidden` — a differential outcome would be the
only signal a client could read, and that would leak the value.

Path syntax: JSON Pointer (`/providers/fallback`) or the dotted form
(`providers.fallback`). Both are accepted; the server normalises.

The CLI counterpart is `zeroclaw config patch <file-or-stdin>`, which applies
the same op set against the local Config and returns the same structured
response shape (`--json` for scripts).

## Secrets — write-only over HTTP

Secret fields (those marked `#[secret]` or `#[derived_from_secret]` in the
schema) are **never** readable over HTTP in any form. Responses for secrets
carry `{populated: bool}` only — no value, no length, no masked stand-in, no
hash. This is enforced at the response layer regardless of which endpoint is
called.

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
| `provider_fallback_dangling` | 400 | `providers.fallback` references a key that is not configured. |
| `value_type_mismatch` | 400 | The submitted JSON value cannot coerce into the target type. |
| `op_not_supported` | 400 | JSON Patch op is `move` / `copy` / unknown. |
| `secret_test_forbidden` | 400 | JSON Patch `test` op targeted a secret path. |
| `config_changed_externally` | 409 | The on-disk config drifted from the in-memory copy. (See drift detection.) |
| `reload_failed` | 500 | The save succeeded but daemon reload could not pick up the new state; on-disk reverted. |
| `internal_error` | 500 | Unclassified server-side failure. |

## Live exploration

Once a gateway is running, browse to `http://<gateway-host>:<port>/api/docs`
for the Scalar API explorer. Schema definitions and "Try it out" forms come
from the same `schemars` annotations the daemon uses, so the documentation
cannot lie about the runtime surface.

The explorer's authentication panel binds to the `bearerAuth` scheme declared
in the spec — paste your pairing-derived bearer token there before issuing
live calls. The CLI shortcut for the URL is `zeroclaw config docs`.

If the Scalar bundle can't load from the CDN (offline / air-gapped install),
the page degrades gracefully and points you at the raw spec at
`/api/openapi.json` so you can use any compatible viewer
(Insomnia, Postman, Swagger UI, etc.).
