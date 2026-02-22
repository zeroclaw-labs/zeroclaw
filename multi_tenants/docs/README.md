# zcplatform — ZeroClaw Multi-Tenant Platform

zcplatform turns ZeroClaw's single-tenant agent runtime into a managed AI agent
platform. It provisions isolated Docker containers (one per tenant), renders and
injects per-tenant configuration, routes HTTP traffic via Caddy, and exposes a
React SPA for lifecycle management — all backed by a single SQLite database and a
Rust/Axum API server.

## Key Capabilities

- **Zero-touch provisioning** — create a tenant → agent running in under 30 seconds
- **Container isolation** — each tenant gets its own network namespace, read-only
  rootfs, dropped capabilities, and enforced CPU/memory limits
- **Encrypted secret storage** — XChaCha20-Poly1305 vault with key versioning;
  API keys and channel tokens never stored in plaintext
- **RBAC** — four roles per tenant (Viewer / Contributor / Manager / Owner) plus
  a platform-level super-admin; every mutation is audit-logged
- **Provider and channel agnostic** — any ZeroClaw provider (OpenAI, Anthropic,
  Gemini, Ollama, 30+ total) or channel (Telegram, Discord, Slack, WhatsApp,
  Matrix, 15+ total) configurable per tenant
- **Lightweight** — ~200 tenants on a 4 GB VPS; SQLite, not Postgres; no
  Kubernetes required

## Tech Stack

| Layer    | Technology                                        |
|----------|---------------------------------------------------|
| Backend  | Rust + Axum 0.8, SQLite (WAL), XChaCha20-Poly1305 |
| Frontend | React 19 + TypeScript + Vite + React Query + Tailwind CSS v4 |
| Runtime  | Docker (one container per tenant)                 |
| Proxy    | Caddy (wildcard TLS, dynamic routes via Admin API) |

## Quick Start

```bash
# 1. Build and bootstrap (creates super-admin, generates master key)
cargo build -p zcplatform --release
./target/release/zcplatform bootstrap

# 2. Run the API server (default: http://127.0.0.1:8080)
./target/release/zcplatform serve

# 3. Open the admin SPA
open http://localhost:8080
```

See [07-deployment-guide.md](07-deployment-guide.md) for production setup
(Caddy, systemd, wildcard DNS, TLS).

## Documentation Index

| # | Document | What it covers |
|---|----------|----------------|
| 01 | [Vision and Concept](01-vision-and-concept.md) | Problem, solution, audiences, value props |
| 02 | [Architecture Overview](02-architecture-overview.md) | Components, data flow, isolation model |
| 03 | [User Flows](03-user-flows.md) | Admin, tenant user, and wizard journeys |
| 04 | [Backend Internals](04-backend-internals.md) | Rust modules, DB schema, state machine |
| 05 | [Frontend Internals](05-frontend-internals.md) | React SPA, pages, components, API client |
| 06 | [Security Model](06-security-model.md) | Auth, RBAC, vault, container hardening |
| 07 | [Deployment Guide](07-deployment-guide.md) | Self-host: Docker, Caddy, systemd, TLS |
| 08 | [Configuration Reference](08-configuration-reference.md) | platform.toml, plan tiers, all keys |
| 09 | [Scaling and Operations](09-scaling-and-operations.md) | Capacity, monitoring, backup, key rotation |
| 10 | [Service Provider Guide](10-service-provider-guide.md) | Running zcplatform as a commercial service |
| 11 | [API Reference](11-api-reference.md) | Complete REST API with request/response schemas |
