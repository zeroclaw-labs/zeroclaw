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

[ZEGA AI](https://github.com/siabang35/zega.ai) is an autonomous Solana Pay merchant fintech platform built natively on the [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) agentic framework. ZEGA connects to the ZeroClaw v0.8.x gateway daemon via a standalone TypeScript bridge package (`@zega/zeroclaw-bridge`), while leveraging ZeroClaw's Rust runtime, SOP engine, skills system, and security risk profiles.

> **Status:** Production-Hardened Integration. The TypeScript bridge package and settlement engine have been verified against the official ZeroClaw Rust binary (`v0.8.3`). All API routes, timing-safe webhooks, atomic PostgreSQL triggers, and OWASP Level 3 prompt injection guards are fully implemented and covered by an automated test suite (**89/89 PASS**).

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
| `ZeroClawGatewayClient` | HTTP client featuring `AbortController` timeouts (1.2s non-blocking ping), exponential backoff, and graceful offline fallback states when the daemon is unreachable. |
| `ZeroClawAuthManager` | Coordinates pairing credentials (enhanced → legacy fallback) and generates `Authorization: Bearer <token>` headers for all authenticated daemon interactions. |
| Version Matrix | Client-side version compatibility checker targeting numeric bounds `>=0.8.0 <0.9.0-alpha` (pinned target `v0.8.3`). Strips pre-release suffixes prior to numeric component comparison (`major.minor.patch`). |

## Native ZeroClaw Composition in ZEGA AI

ZEGA composes ZeroClaw's stock agent primitives to deliver an autonomous merchant terminal:

### 1. Standard Operating Procedures (SOPs)

- **`payment-reconciliation` (Cron Trigger `*/30s`)**: Periodically queries pending invoice reference keys on Solana Devnet via `getSignaturesForAddress` and `getTransaction`, verifying recipient pubkeys and posting confirmed settlements to ZEGA terminal views.
- **`refund-approval` (Channel Trigger)**: Handles customer refund requests. Automatically screens inputs for prompt injection; if safe, halts at an approval checkpoint (`kind: checkpoint`, `policy: merchant-refund`, `quorum: 1`) requiring human merchant confirmation before proceeding.
- **`defi-guardian` (Cron Trigger)**: Monitors price feed volatility and liquidity alerts via Jupiter & Switchboard.
- **`balance-alert` (Cron Trigger)**: Monitors merchant wallet SOL balances for operational threshold alerts.

### 2. Skills & Response Shaping

- **`solana-pay`**: Constructs unsigned Solana Pay URLs with single-use reference keys. Enforces response shaping capped at `<200 tokens` per step to prevent context window bloat.
- **`solana-blinks`**: Renders shareable Solana Actions and `dial.to` Blink URLs.
- **`merchant-memory`**: Interacts with ZeroClaw's relationship memory graph to log customer history and payment telemetry.
- **`defi-guardian`**: Queries Jupiter price feeds and fallback oracle quotes.

### 3. Keyless Custody & Security Invariants

- **Tier 1 (Keyless Agent)**: The LLM and ZeroClaw agent never access, hold, or sign with private keys. All transactions are signed client-side via Phantom or Solflare wallets.
- **Atomic Replay Protection**: PostgreSQL kernel constraint (`tx_signature` `UNIQUE`) and trigger `trg_sync_invoice_to_settlement` ensure database-backed signature deduplication and deterministic settlement persistence.
- **OWASP Prompt Injection Guard**: Level 3 regex threat screening blocks prompt injection attacks (e.g. "Ignore previous instructions", "Jailbreak refund") before reaching approval gates.

## What the Test Suite Covers

The bridge and integration suite (`pnpm test` / `pnpm --filter @zega/api test`) validates 89 automated test specs covering:

- Numeric version parsing, compatibility matrix bounds, and error class hierarchies.
- Auth manager initialization and `Authorization` header formatting.
- Gateway client offline resilience and graceful error handling.
- HMAC-SHA256 timing-safe signature verification (`crypto.timingSafeEqual`).
- Atomic database conflict resolution (`on_conflict=tx_signature`) and OWASP Level 3 prompt injection defense.

## External Reference

For source code, PRDs, and monorepo implementation details, visit the [ZEGA AI Repository](https://github.com/siabang35/zega.ai) or inspect the bridge package at reviewed commit [`f99104367a6b06815cf478120b247d042fa7b1a5`](https://github.com/siabang35/zega.ai/tree/f99104367a6b06815cf478120b247d042fa7b1a5/packages/zeroclaw-bridge).
