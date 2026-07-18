---
type: architecture
status: accepted
last-reviewed: 2026-07-14
relates-to:
  - docs/book/src/foundations/fnd-002-documentation-standards.md
---

# Architecture Decision Records

Architecture Decision Records (ADRs) are the durable record of
architecture choices that constrain future ZeroClaw work. They are
shorter than RFCs: each one records the context, decision,
consequences, and the sources that justify the decision.

Accepted ADRs are immutable. If the architecture changes, write a new
ADR and mark the old one as superseded rather than rewriting history.

## Current ADRs

| ADR | Status | Decision |
| --- | --- | --- |
| [ADR-001](./ADR-001-rust-first.md) | accepted | ZeroClaw's runtime and first-party crates are implemented in Rust. |
| [ADR-002](./ADR-002-trait-driven-extensibility.md) | accepted | First-party extension surfaces use explicit trait contracts. |
| [ADR-003](./ADR-003-wasm-plugin-model.md) | superseded by [ADR-009](./ADR-009-wit-wasmtime-plugin-execution.md) | WASM plugins initially used Extism as the execution bridge. |
| [ADR-004](./ADR-004-tool-shared-state-ownership.md) | accepted | Tool-held shared state follows daemon-owned identity, handle ownership, isolation, and reload rules. |
| [ADR-005](./ADR-005-pluggable-memory-backends.md) | accepted | Memory storage uses a backend-neutral contract with SQLite as the default. |
| [ADR-008](./ADR-008-goal-mode-control-plane-and-usage-accounting.md) | accepted | Goal mode uses the durable task control plane and canonical usage ledger. |
| [ADR-009](./ADR-009-wit-wasmtime-plugin-execution.md) | accepted | WIT components and direct `wasmtime` replace the Extism plugin bridge. |

## Planned Architecture Decisions

[FND-002](../../foundations/fnd-002-documentation-standards.md) also reserves two implementation-gated roadmap decisions:

- ADR-006: migration from compiled optional channels to runtime plugins
- ADR-007: Gateway extraction as a separate optional binary

ADR-009 is intentionally present because it supersedes the historical ADR-003 plugin bridge. ADR numbers do not have to be contiguous. Planned entries have no placeholder files because an ADR must contain a reviewed decision record.
