---
type: architecture
status: accepted
last-reviewed: 2026-07-19
relates-to:
  - docs/book/src/foundations/fnd-002-documentation-standards.md
---

# Architecture Decision Records

Architecture Decision Records (ADRs) are the durable record of architecture choices that constrain future ZeroClaw work. They are shorter than RFCs: each one records the context, decision, consequences, and the sources that justify the decision.

Accepted ADRs are immutable. If the architecture changes, write a new ADR and mark the old one as superseded rather than rewriting history.

## Current ADRs

| ADR | Status | Decision |
| --- | --- | --- |
| [ADR-001](./ADR-001-rust-first.md) | accepted | ZeroClaw's runtime and first-party crates are implemented in Rust. |
| [ADR-002](./ADR-002-trait-driven-extensibility.md) | accepted | First-party extension surfaces use explicit trait contracts. |
| [ADR-003](./ADR-003-wasm-plugin-model.md) | superseded by [ADR-009](./ADR-009-wit-wasmtime-plugin-execution.md) | WASM plugins initially used Extism as the execution bridge. |
| [ADR-004](./ADR-004-tool-shared-state-ownership.md) | accepted | Tool-held shared state follows daemon-owned identity, handle ownership, isolation, and reload rules. |
| [ADR-005](./ADR-005-pluggable-memory-backends.md) | accepted | Memory storage uses a backend-neutral contract with SQLite as the default. |
| [ADR-006](./ADR-006-runtime-channel-plugins.md) | proposed | Runtime plugins are the target for optional channels, with explicit capability-based native exceptions during migration. |
| [ADR-007](./ADR-007-gateway-extraction.md) | proposed | The gateway becomes a separate optional process over a supported local IPC contract. |
| [ADR-008](./ADR-008-goal-mode-control-plane-and-usage-accounting.md) | accepted | Goal mode uses the durable task control plane and canonical usage ledger. |
| [ADR-009](./ADR-009-wit-wasmtime-plugin-execution.md) | accepted | WIT components and direct `wasmtime` replace the Extism plugin bridge. |
| [ADR-010](./ADR-010-memory-authority-boundaries.md) | proposed | Session history, curated memory, and enrichment have separate authority boundaries. |
| [ADR-011](./ADR-011-multi-agent-runtime-boundaries.md) | accepted | Configured agents have explicit runtime boundaries under one daemon. |
| [ADR-012](./ADR-012-generation-scoped-live-config-apply.md) | proposed | Live config application uses canonical generations and target-specific results. |

ADR-006 and ADR-007 are implementation-gated roadmap decisions from [FND-002](../../foundations/fnd-002-documentation-standards.md). Their target directions are recorded, but they remain proposed until the acceptance boundaries in each record ship.

ADR-010 remains proposed until the acceptance gates in the record are met.

ADR-012 remains proposed until canonical config publication, generation-scoped results, and the bounded security and channel live-apply consumers meet the acceptance gates in the record.
