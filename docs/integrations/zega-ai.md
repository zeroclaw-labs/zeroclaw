# ZEGA AI × ZeroClaw Integration Specification

## Overview

[ZEGA AI](https://zegaai.site) integrates the **ZeroClaw** Rust AI agent runtime into its enterprise orchestration console, serving as an autonomous payment & workflow gateway built on top of Solana Pay and Devnet RPC slot verification.

---

## Technical Features

### 1. Keyless Tier 1 Custody Model
- Zero private keys stored on server infrastructure.
- User transactions are constructed client-side and signed directly via Solana wallets (Phantom, Solflare, Backpack) or standard Solana Pay QR transfer request URIs (`solana:<recipient>?amount=<val>&reference=<refKey>&label=<label>&message=<msg>`).

### 2. Fastify REST API Proxy Layer
- `GET /v1/zeroclaw/status`: ZeroClaw Rust node health & telemetry stream.
- `GET /v1/zeroclaw/solana-rpc`: Solana Devnet RPC live slot stream & transaction signature verification (`Slot 480013691+`).
- `POST /v1/zeroclaw/events`: Payment reconciliation, Solana Pay reference key generation, and webhook streaming.
- `POST /v1/zeroclaw/approve-checkpoint`: SOP prompt injection refund checkpoint clearance.

### 3. Human-in-the-Loop SOP Checkpoints
- Automated guardrail for prompt injection attempts trying to force unauthorized refunds.
- Flagged transactions are held in a `pending` state queue requiring explicit admin approval via Fastify API.

### 4. Role-Separated Reconciliation Stream Histories
- **UMKM / Individual Dashboard:** Retail product sales, coffee shop cashier QR settlements, and customer WhatsApp micro-payments.
- **Enterprise Dashboard:** High-value corporate treasury settlements (1,250 USDC), multi-agent swarm escrows (250 USDC), and cross-border supply chain clearing (500 USDC).

---

## Repositories & References
- **ZEGA AI Monorepo:** [siabang35/zega.ai](https://github.com/siabang35/zega.ai)
- **Live Demo Site:** [https://zegaai.site](https://zegaai.site)
- **PRD Integration Spec:** [docs/PRD/19-ZEROCLAW-SOLANA-INTEGRATION-SPEC.md](https://github.com/siabang35/zega.ai/blob/master/docs/PRD/19-ZEROCLAW-SOLANA-INTEGRATION-SPEC.md)
