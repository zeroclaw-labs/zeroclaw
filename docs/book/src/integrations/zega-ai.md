# ZEGA AI Integration (Prototype Specification)

## Overview

[ZEGA AI](https://zegaai.site) provides an enterprise orchestration console that interfaces with the **ZeroClaw** Rust AI agent runtime.

This document describes the downstream proxy and gateway endpoints hosted by ZEGA AI (`siabang35/zega.ai`) for managing ZeroClaw agent telemetries, prototype Solana Devnet RPC queries, and human-in-the-loop SOP refund review checkpoints.

> [!NOTE]
> **Integration Status: Prototype / Demo Specification**
> The `/v1/zeroclaw/*` endpoints described below are downstream HTTP routes hosted by the ZEGA AI Fastify backend (`apps/api/src/routes/v1/zeroclaw.routes.ts`), not native ZeroClaw Rust gateway endpoints. The current implementation maintains in-memory state for demonstration purposes and does not yet perform production cryptographic verification, persistent database recording, or authenticated authorization checks.

---

## Architecture & Boundary

- **ZeroClaw Core Runtime:** Runs the high-performance Rust agent framework with standard `/api/*` endpoints.
- **ZEGA AI Proxy Layer:** Fastify REST API (`/v1/zeroclaw/*`) acting as an operational bridge between ZEGA's frontend dashboard and ZeroClaw agent instances.
- **Custody Model:** Keyless Tier 1 custody model where zero private keys are stored on server infrastructure. Transactions are constructed client-side and signed directly via user Solana wallets (e.g. Phantom, Solflare, Backpack) or Solana Pay QR request URIs (`solana:<recipient>?amount=<val>&reference=<refKey>&label=<label>&message=<msg>`).

---

## Downstream Proxy Routes (ZEGA Fastify API)

### 1. `GET /v1/zeroclaw/status`

Retrieves the current in-memory status of the ZeroClaw agent proxy, including connected communication channels, prototype total USDC reconciliation metrics, and active SOP checkpoints.

- **Status:** Prototype in-memory state stream.

### 2. `GET /v1/zeroclaw/solana-rpc`

Queries Solana Devnet RPC (`api.devnet.solana.com`) for account information (`getAccountInfo`) and recent transaction signatures (`getSignaturesForAddress`) for a target mint or account address (defaults to Devnet USDC mint `4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU`).

- **Status:** Prototype RPC data query. Returns raw signature lists; does not parse instruction data, token mints, amounts, reference keys, or on-chain settlement finality.

### 3. `POST /v1/zeroclaw/events`

Receives agent lifecycle events, prototype payment reconciliation entries, and refund requests, updating local in-memory telemetry.

- **Status:** In-memory prototype handler.

### 4. `POST /v1/zeroclaw/approve-checkpoint`

Updates the decision status (`approved` or `rejected`) for pending human-in-the-loop SOP refund checkpoints.

- **Status:** Prototype queue mutation without auth. Production deployment requires strict JWT/RBAC authorization.

---

## Operator Guide & Reproducible Steps

### Prerequisites

- Node.js v18+ and pnpm/npm
- ZEGA AI backend running locally (`http://localhost:3001`) or access to Devnet API (`https://api.zegaai.site`)
- Solana Devnet wallet address or SPL token mint address for RPC lookups

### Environment Setup & Base URL

Set the target ZEGA API base URL:

```bash
export ZEGA_API_URL="http://localhost:3001/v1/zeroclaw"
```

### Request & Response Examples

#### Check Proxy Status

```bash
curl -X GET "${ZEGA_API_URL}/status"
```

**Example Response:**

```json
{
  "success": true,
  "data": {
    "state": {
      "agentStatus": "active",
      "custodyTier": "T1 (Keyless / Unsigned)",
      "network": "solana-devnet",
      "rpcUrl": "https://api.devnet.solana.com",
      "connectedChannels": ["WhatsApp (+628123456789)", "Telegram Bot", "ZEGA Monorepo MCP"],
      "totalReconciledUsdc": 485.5,
      "reconciledTxCount": 24
    },
    "pendingCheckpoints": [],
    "recentReconciledEvents": []
  }
}
```

#### Query Solana Devnet RPC Signatures

```bash
curl -X GET "${ZEGA_API_URL}/solana-rpc?address=4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"
```

**Example Response:**

```json
{
  "success": true,
  "network": "solana-devnet",
  "rpcUrl": "https://api.devnet.solana.com",
  "address": "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU",
  "accountInfo": {
    "context": { "slot": 480013691 },
    "value": { "executable": false, "lamports": 1461600, "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA" }
  },
  "signatures": [
    { "signature": "5K2bM7xP9q8Z1a3N8xY2wLzR4w9M3k...", "slot": 480013650, "err": null }
  ]
}
```

#### Log a Prototype Reconciled Payment Event

```bash
curl -X POST "${ZEGA_API_URL}/events" \
  -H "Content-Type: application/json" \
  -d '{
    "eventType": "payment_reconciled",
    "amount": 15.00,
    "currency": "USDC",
    "signature": "5K2bM7xP9q8Z1a3N8xY2wLzR4w9M3k",
    "customerChannel": "WhatsApp (+628123456789)"
  }'
```

#### Decision on SOP Refund Checkpoint

```bash
curl -X POST "${ZEGA_API_URL}/approve-checkpoint" \
  -H "Content-Type: application/json" \
  -d '{
    "checkpointId": "chk_ref_9901",
    "decision": "approve"
  }'
```

---

## Production Security Roadmap

To elevate this prototype integration to enterprise-grade production readiness, the following enhancements must be implemented in ZEGA AI:

1. **Cryptographic Payment Verification:** Parse transaction instructions on-chain to verify recipient address, SPL token mint (`USDC`), exact amount, reference key, and commitment finality (`confirmed`/`finalized`).
2. **Authenticated Endpoint Authorization:** Enforce JWT Bearer tokens and Role-Based Access Control (RBAC) on `/approve-checkpoint` and `/events`.
3. **Persistent Settlement Storage:** Migrate in-memory state tracking to Supabase/PostgreSQL with strict idempotency key enforcement.
4. **Automated Webhooks:** Dispatch signed HMAC SHA-256 webhooks upon validated Solana transaction confirmation.

---

## Repositories & References

- **ZEGA AI Monorepo:** [siabang35/zega.ai](https://github.com/siabang35/zega.ai)
- **Live Demo Site:** [https://zegaai.site](https://zegaai.site)
- **PRD Integration Spec:** [docs/PRD/19-ZEROCLAW-SOLANA-INTEGRATION-SPEC.md](https://github.com/siabang35/zega.ai/blob/master/docs/PRD/19-ZEROCLAW-SOLANA-INTEGRATION-SPEC.md)
