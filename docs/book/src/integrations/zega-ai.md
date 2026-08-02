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

The bridge implements the two pairing contracts exposed by the ZeroClaw
gateway and tries them in order:

### Enhanced route: `POST /api/pair`

Accepts a JSON body:

```json
{
  "code": "<6-digit pairing code>",
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
X-Pairing-Code: <6-digit code>
```

The bridge falls back to this route when the enhanced endpoint is
unavailable or returns a non-success status.

Upstream handler: `handle_pair`
(`crates/zeroclaw-gateway/src/lib.rs`).

## Bridge architecture

| Component | Role |
|---|---|
| `ZeroClawGatewayClient` | HTTP client with `AbortController` timeouts and automatic retry with exponential back-off. Falls back to an offline error state when the daemon is unreachable. |
| `ZeroClawAuthManager` | Manages the pairing flow (enhanced → legacy fallback) and generates `Authorization: Bearer <token>` headers for authenticated endpoints. |
| Version matrix | Client-side SemVer check enforcing `>=0.8.0 <0.9.0-alpha` (target `v0.8.3`). This range has **not** been verified against a live daemon; it reflects the bridge's design target. |

## What the smoke test covers

The bridge ships a smoke test (`pnpm --filter @zega/zeroclaw-bridge test:smoke`)
that validates the following **offline / unit-level** behavior:

- SemVer parsing and comparison.
- Version compatibility matrix (compatible, too-old, exceeds-max).
- Auth manager initialization and `Authorization` header formatting.
- Gateway client offline resilience (graceful error state, no crash).
- Error class hierarchy instantiation.

The smoke test does **not** start a ZeroClaw daemon, exchange a pairing
code, or call any gateway endpoint over HTTP.

## Verified Feature Coverage

The ZEGA AI integration bridges ZeroClaw runtime capabilities to provide a self-hosted AI agent merchant terminal with the following upstream feature support:

- **Keyless Tier 1 Custody:** Zero private keys stored server-side. Mobile and browser wallets (Phantom, Solflare) sign transactions client-side.
- **Directory-Based SOP Engine:** Executes multi-step procedures (`docs/zeroclaw/sops/*`) with cron scheduling, trigger filtering, and human approval gates (`kind: checkpoint`).
- **MCP Client Proxy:** Proxies Helius DAS RPC tools (SSE transport) and SendAI Solana execution tools (STDIO transport) with strict tool namespacing (`server__tool`).
- **Relationship Memory Graph:** Structured knowledge graph tracking CRM connections (`client`, `contact`, `pattern`, `decision`) persisted to Supabase PostgreSQL.
- **Webhook Inbound Security:** Validates inbound webhooks via `X-Webhook-Signature: sha256=<HMAC-SHA256>` calculated with `ZEROCLAW_WEBHOOK_SECRET`.
- **Solana Pay Reference Tracking:** Automatically attaches unique cryptographic reference keys (`&reference=RefXXXXXXX`) to generated Solana Pay URIs.
- **Real-Time Devnet RPC Reconciliation:** Polling engine querying `getSignaturesForAddress` on Solana Devnet RPC to reconcile confirmed on-chain transaction signatures into the UI.

## External reference

For source code and monorepo details, visit the
[ZEGA AI repository](https://github.com/siabang35/zega.ai).
