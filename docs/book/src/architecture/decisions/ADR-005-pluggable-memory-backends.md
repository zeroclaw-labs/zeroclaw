---
id: ADR-005
title: Memory storage is backend-neutral with SQLite as the default
date: 2026-07-14
status: accepted
relates-to:
  - ADR-002
  - docs/book/src/foundations/fnd-001-intentional-architecture.md
  - docs/book/src/foundations/fnd-002-documentation-standards.md
  - https://github.com/zeroclaw-labs/zeroclaw/issues/6850
  - crates/zeroclaw-api/src/memory_traits.rs
  - crates/zeroclaw-memory
  - crates/zeroclaw-config/src/schema.rs
---

# ADR-005: Memory Storage Is Backend-Neutral With SQLite As The Default

This is a retroactive record of an architecture that evolved before the formal ADR process. The exact original decision date is not available in this record; the date above is the date this ADR was added to the architecture docs.

FND-002 originally described this decision as choosing SQLite and Markdown as the two memory backends. That description no longer captures the broader contract. This record describes the durable architecture instead of freezing a backend count.

## Context

ZeroClaw needs persistent memory across installations with different operational constraints. A local single-process agent benefits from an embedded store with no service dependency. Operators may also need a human-readable filesystem store, a shared database, a vector database, or an integration with another memory system.

These stores do not have identical schemas or operational properties. They still need to present one runtime interface for memory operations and scoping. Individual operations may have backend-specific capability semantics; for example, Markdown memory is append-only and does not delete entries. Turn processing must not depend on a concrete database type, and adding a backend must not require copying prompt assembly, consolidation, hygiene, or agent-authorization policy into that backend.

The current repository recognizes SQLite, Lucid, PostgreSQL, Qdrant, and Markdown storage, plus `none` to disable persistent memory. SQLite is the default. FND-001 separately identifies SQLite and Markdown as the desired baseline stores for the eventual minimal runtime; that packaging target does not limit the storage contract to two implementations.

## Decision

Memory persistence is selected through a backend-neutral contract, with SQLite as the default backend.

### Storage contract

Concrete stores implement the `Memory` trait from `zeroclaw-api`. The trait owns backend-neutral persistence operations and entry semantics. Callers use `Memory` handles rather than branching on SQLite, Markdown, PostgreSQL, Qdrant, or Lucid in turn-processing code.

Backend construction currently consults two overlapping configuration levels. `agents.<alias>.memory.backend` directly routes only the Markdown and `none` construction paths and supplies the kind used for same-backend sharing validation. All other per-agent values proceed through the install-wide factory, where `memory.backend` selects the concrete typed `storage.<kind>.<alias>` entry; legacy bare names resolve to the `default` alias. This ADR records that interaction without treating the overlap as an ideal end state. Runtime components must follow the current factory and validation ownership rather than infer unsupported per-agent selection or create another stored selector.

SQLite remains the default because it provides durable local storage, hybrid retrieval, and no external service requirement. Other backends may be selected when their different storage, deployment, readability, or integration properties are required. `none` is an explicit request to disable persistent memory, not an implicit fallback.

Changing either selector does not migrate existing data. Moving data between backend kinds requires an explicit migration path rather than silently reinterpreting one store as another.

### Lifecycle policy

Storage backends do not own prompt construction or turn policy. The turn engine owns memory-context selection and rendering. `MemoryStrategy` is the designated boundary for consolidation and governance above a `Memory` handle, but migration to that boundary is not complete: some paths still call lower-level lifecycle functions directly. Issue #6850 tracks the remaining alignment. Backend implementations provide storage and retrieval behavior without becoming the long-term owner of those lifecycle rules.

### Agent scope

Memory is scoped to agent identity independently of the concrete store. SQL-backed stores may use internal UUIDs while non-SQL stores may use an agent alias directly. Agent-scoping adapters bind a backend to an agent, and cross-agent recall is permitted only through the configured allowlist and only when the agents use the same backend. A caller must not bypass those adapters or infer that backend-specific identifiers have the same representation.

This ADR does not decide which backend implementations ship in a particular binary or whether a future backend is native, feature-gated, or supplied by a plugin. Those are packaging and plugin-lifecycle decisions. The stable constraint is that every supported backend preserves the common storage and agent-scoping contract.

## Consequences

Positive consequences:

- Agent, channel, gateway, and tool code can depend on one memory interface.
- SQLite provides a useful local default without making embedded storage the only deployment model.
- Operators can choose embedded, file-backed, shared database, or vector storage without changing turn-processing callers.
- Prompt construction, consolidation, and hygiene can evolve without adding lifecycle policy methods to every storage implementation.
- Agent isolation has one caller-visible contract across backend-specific identifier and storage models.

Negative consequences:

- Backend implementations must preserve shared entry and scoping contracts even when their storage and query models differ.
- Callers must account for capability differences such as append-only stores and trait operations that report unsupported or no-op behavior.
- Migration between backend kinds requires explicit data movement; changing a configured backend does not make existing data appear in the new store.
- Optional services and features increase the validation matrix even though most callers see only the shared trait.

Follow-up decisions:

- Issue #6850 tracks the boundary between storage and higher-level memory lifecycle policy.
- The WASM memory adapter is not yet a configurable daemon backend; its runtime construction and packaging remain separate plugin work.
- Changes to cross-backend migration, shared-agent recall, or storage identity require explicit compatibility review.

## References

- [ADR-002: Trait-driven extensibility](./ADR-002-trait-driven-extensibility.md)
- [FND-001: Intentional architecture](../../foundations/fnd-001-intentional-architecture.md)
- [FND-002: Documentation standards](../../foundations/fnd-002-documentation-standards.md)
- [Runtime state and persistence](../runtime-state-and-persistence.md)
- [Issue #6850](https://github.com/zeroclaw-labs/zeroclaw/issues/6850)
- `crates/zeroclaw-api/src/memory_traits.rs`
- `crates/zeroclaw-config/src/schema.rs`
- `crates/zeroclaw-memory/src/backend.rs`
- `crates/zeroclaw-memory/src/lib.rs`
- `crates/zeroclaw-runtime/src/agent/memory_strategy.rs`
