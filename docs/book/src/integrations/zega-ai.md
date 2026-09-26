---
type: reference
status: proposed
last-reviewed: 2026-08-05
relates-to:
  - FND-002
  - FND-003
  - crates/zeroclaw-gateway
---

# ZEGA AI (External Prototype)

[ZEGA AI](https://github.com/siabang35/zega.ai) is an external fintech
platform that connects to a ZeroClaw v0.8.x gateway daemon through a
TypeScript bridge package (`@zega/zeroclaw-bridge`). The bridge is an
**external prototype** maintained in the ZEGA monorepo and is not part of
ZeroClaw itself.

> **Status:** Prototype. The bridge has been smoke-tested against local
> helper modules (SemVer parsing, error hierarchies, offline resilience).
> No live daemon pairing or endpoint tests have been executed yet. The
> information below describes the bridge's design intent, not verified
> production compatibility.

## Pairing

> **Gateway URL configuration note:** The bridge client defaults to
> `http://127.0.0.1:4242`, whereas ZeroClaw's canonical `GatewayConfig`
> default port is `42617`. When connecting to a stock local ZeroClaw daemon,
> configure the client with the matching gateway address:
>
> ```ts
> const client = new ZeroClawGatewayClient({
>   gatewayUrl: "http://127.0.0.1:42617",
> });
> ```
>
> Connecting without setting the gateway port to match the active ZeroClaw
> daemon will cause the bridge to report an offline/unreachable state before
> pairing can occur.
>
> Remotely reachable gateways should use HTTPS or an authenticated tunnel (such as WireGuard, Tailscale, or an SSH tunnel) rather than plain HTTP to protect bearer credentials transmitted in the `Authorization` header.

The bridge implements the two pairing contracts exposed by the ZeroClaw
gateway and tries them in order:

> **Pairing codes are case-sensitive, and their shape is configurable.**
> `[gateway.pairing_code]` sets the length (`6`–`128`) and the character
> family (`numeric`, `alphanumeric`, or `unambiguous`); the default is 32
> case-sensitive alphanumeric characters. Transmit the code verbatim: do
> not upper-case it, strip characters, or assume a fixed width.

### Enhanced route: `POST /api/pair`

Accepts a JSON body:

```json
{
  "code": "<pairing code>",
  "device_name": "ZEGA AI Bridge",
  "device_type": "api-bridge"
}
```

On success the gateway returns `{ "paired": true, "token": "<bearer>" }`.
The bridge stores the token for subsequent authenticated requests.

Upstream handler: `api_pairing::submit_pairing_enhanced`
(`crates/zeroclaw-gateway/src/api_pairing.rs`).

### Legacy route: `POST /pair`

Sends the pairing code in the `X-Pairing-Code` header:

```text
POST /pair
Content-Type: application/json
X-Pairing-Code: <pairing code>
```

The bridge falls back to this route when the enhanced endpoint is
unavailable or returns a non-rate-limit non-success status. If the
enhanced endpoint returns a rate-limit failure (`RateLimitError`), the
bridge re-throws the error immediately without attempting the legacy
fallback.

Upstream handler: `handle_pair`
(`crates/zeroclaw-gateway/src/lib.rs`).

## Bridge architecture

| Component | Role |
|---|---|
| `ZeroClawGatewayClient` | HTTP client with a 5-second default request and health timeout, `AbortController` cancellation, and automatic retry with exponential back-off. Falls back to an offline error state when the daemon is unreachable. |
| `ZeroClawAuthManager` | Manages the pairing flow (enhanced → legacy fallback) and generates `Authorization: Bearer <token>` headers for authenticated endpoints. |
| Version matrix | Client-side version check targeting numeric bounds `>=0.8.0 <0.9.0` (target `v0.8.3`). The current client helper strips prerelease suffixes prior to comparison and evaluates numeric components (`major.minor.patch`). This range reflects design intent and has **not** been verified against a live daemon. |

## What the smoke test covers

The bridge ships a smoke test (`pnpm --filter @zega/zeroclaw-bridge test:smoke`)
with 18 assertions that validate the following **offline / unit-level** behavior:

- SemVer parsing and comparison.
- Version compatibility boundaries (compatible, too old, exceeds the maximum).
- Auth-manager token storage, bearer-header construction, and offline `getState()` behavior.
- Gateway error constructors and fields.

The smoke test does **not** start a ZeroClaw daemon, exchange a pairing
code, or call any gateway endpoint over HTTP.

It does not exercise the enhanced-to-legacy pairing fallback or rate-limit escalation branches.

## External reference

For source code and monorepo details, visit the
[ZEGA AI repository](https://github.com/siabang35/zega.ai) or inspect the bridge package at reviewed commit
[`f99104367a6b06815cf478120b247d042fa7b1a5`](https://github.com/siabang35/zega.ai/tree/f99104367a6b06815cf478120b247d042fa7b1a5/packages/zeroclaw-bridge).
