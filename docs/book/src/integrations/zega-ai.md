---
type: reference
status: proposed
last-reviewed: 2026-08-09
relates-to:
  - FND-002
  - FND-003
  - crates/zeroclaw-gateway
---

# ZEGA AI (External Integration & Bridge Specification)

[ZEGA AI](https://github.com/siabang35/zega.ai) is an autonomous Solana Pay merchant platform built to integrate with the [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) agentic framework. ZEGA connects to the ZeroClaw gateway daemon via a standalone TypeScript bridge package (`@zega/zeroclaw-bridge`), providing typed gateway pairing, status checks, and offline resilience for web applications.

> **Status:** Community Integration Reference. This specification documents the client-side bridge contract provided by `@zega/zeroclaw-bridge` to pair with a ZeroClaw daemon over HTTP.

## Gateway Connectivity & Pairing

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
> pairing can occur. Remotely reachable gateways should use HTTPS or an
> authenticated tunnel (such as WireGuard, Tailscale, or an SSH tunnel)
> rather than plain HTTP to protect bearer credentials transmitted in the
> `Authorization` header.

The bridge implements the two pairing contracts exposed by the ZeroClaw gateway in strict order:

### 1. Enhanced route: `POST /api/pair`

Accepts a JSON payload:

```json
{
  "code": "<6-digit pairing code>",
  "device_name": "ZEGA AI Bridge",
  "device_type": "api-bridge"
}
```

On success, the gateway returns `{ "paired": true, "token": "<bearer>" }`. The bridge stores the bearer token for subsequent authenticated requests.

Upstream handler: `api_pairing::submit_pairing_enhanced` (`crates/zeroclaw-gateway/src/api_pairing.rs`).

### 2. Legacy route: `POST /pair`

Sends the pairing code in the `X-Pairing-Code` header:

```text
POST /pair
Content-Type: application/json
X-Pairing-Code: <6-digit code>
```

The bridge falls back to this endpoint when the enhanced endpoint is unavailable or returns a non-rate-limit non-success status. If the enhanced endpoint returns a rate-limit failure (`RateLimitError`), the bridge re-throws the error immediately without attempting the legacy fallback.

Upstream handler: `handle_pair` (`crates/zeroclaw-gateway/src/lib.rs`).

## Bridge Architecture

| Component | Role |
|---|---|
| `ZeroClawGatewayClient` | HTTP client featuring `AbortController` timeouts (5 s default request/health timeout), exponential backoff, and graceful offline fallback states when the daemon is unreachable. |
| `ZeroClawAuthManager` | Coordinates pairing credentials (enhanced → legacy fallback) and generates `Authorization: Bearer <token>` headers for authenticated daemon interactions. |
| Version Matrix | Client-side version compatibility checker targeting numeric bounds `>=0.8.0 <0.9.0-alpha`. Strips pre-release suffixes prior to numeric component comparison (`major.minor.patch`). |

## Inspected Bridge Smoke Tests

The bridge package (`packages/zeroclaw-bridge/`) includes an offline smoke test suite (`pnpm --filter @zega/zeroclaw-bridge test:smoke`) with 18 assertions validating:

- SemVer parsing/comparison and version-compatibility boundary checking.
- Auth-manager token storage, bearer-header construction, and offline `getState()` behavior.
- Error constructor correctness for gateway error types.

> **Limitation:** The smoke suite does not start a ZeroClaw daemon, exchange a pairing code, or call a live gateway endpoint. Pairing fallback and rate-limit escalation branches (implemented in `src/auth.ts`) are not covered by the pinned smoke script.

## External Reference

For source code, package sources, and repository implementation details, visit the [ZEGA AI Repository](https://github.com/siabang35/zega.ai) or inspect the bridge package at reviewed commit [`f99104367a6b06815cf478120b247d042fa7b1a5`](https://github.com/siabang35/zega.ai/tree/f99104367a6b06815cf478120b247d042fa7b1a5/packages/zeroclaw-bridge).
