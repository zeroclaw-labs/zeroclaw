---
id: ADR-010
title: Separate conversation history, curated memory, and enrichment authority
date: 2026-07-19
status: proposed
relates-to:
  - ADR-005
  - docs/book/src/foundations/fnd-002-documentation-standards.md
  - docs/book/src/architecture/memory-payload-lifecycle.md
  - https://github.com/zeroclaw-labs/zeroclaw/issues/9048
  - https://github.com/zeroclaw-labs/zeroclaw/issues/9103
  - https://github.com/zeroclaw-labs/zeroclaw/issues/6850
  - https://github.com/zeroclaw-labs/zeroclaw/pull/9072
  - crates/zeroclaw-api/src/memory_traits.rs
  - crates/zeroclaw-memory
  - crates/zeroclaw-infra/src/session_backend.rs
  - crates/zeroclaw-infra/src/session_store.rs
  - crates/zeroclaw-infra/src/acp_session_store.rs
---

# ADR-010: Separate Conversation History, Curated Memory, And Enrichment Authority

## Context

[ADR-005](./ADR-005-pluggable-memory-backends.md) records a backend-neutral durable-memory contract with SQLite as the default. It does not decide which remembered data belongs in durable memory or whether an external memory integration is another authoritative store.

Current runtime, gateway, and channel paths can persist conversation turns both in session stores and as `MemoryCategory::Conversation` entries. That gives one transcript two possible owners with different retention, recall, export, deletion, and scoping behavior. An old conversation row can also re-enter a later prompt as if it were curated cross-session knowledge.

External integrations create a second authority risk. A connector can improve search or supply derived context, but treating its index as another durable store allows connector state, outages, or weaker scoping to compete with ZeroClaw's canonical data.

Accepted RFCs [#9048](https://github.com/zeroclaw-labs/zeroclaw/issues/9048) and [#9103](https://github.com/zeroclaw-labs/zeroclaw/issues/9103) resolve these questions together. This record extends ADR-005 without rewriting it: the backend-neutral `Memory` contract and SQLite default remain valid, while this ADR assigns authority to session history, curated memory, and enrichment separately.

## Decision

### Conversation history owns continuity

The applicable chat, channel, gateway, or ACP session store is authoritative for conversation continuity. Automatic transcript persistence belongs in that session store and preserves the message structure needed for safe resume, including tool-call and tool-result pairing where supported.

Session history preserves every applicable session, agent, tenant, and principal boundary. It is not automatically cross-session knowledge. Trimming history changes session-visible and provider-visible context; deleting curated memory does not delete session history.

When current input or session history conflicts with curated memory, the current input and session history govern the turn. Curated memory must not rewrite, replace, or override that continuity record.

### Curated memory owns selected cross-session knowledge

Agent-curated long-term memory contains selected facts, preferences, decisions, conventions, and learned procedures that are intentionally retained across sessions. Writes must be deliberate or produced by an explicit bounded consolidation policy, preserve useful provenance, and retain every applicable agent, session, tenant, and principal boundary.

Curated memory uses the backend-neutral `Memory` contract from ADR-005. A configured backend is the authoritative durable store for that memory. Prompt assembly, consolidation, governance, and feedback remain lifecycle-policy concerns above the storage implementation; issue [#6850](https://github.com/zeroclaw-labs/zeroclaw/issues/6850) tracks that boundary.

This ADR does not decide whether per-agent Markdown is the canonical operator-visible curated-memory representation or a projection of another configured backend. That remains a separate implementation decision; no second durable authority may be introduced implicitly.

ACP/Code sessions, including ZeroCode's ACP pane, remain isolated from automatic long-term-memory injection. Knowledge bundles and retrieval-augmented generation are separate deliberate context sources; they do not turn curated memory back on for those Code sessions.

### Enrichment is a rebuildable derivative

A `MemoryEnricher` is an optional, best-effort derivative of the authoritative curated-memory store, not another source of truth. The authoritative store commits first. Connector failure, timeout, malformed output, or stale state cannot invalidate a successful authoritative write or make authoritative-store recall unavailable.

The shared enrichment boundary owns connector-independent policy: declared capabilities, caller and agent scope, bounded failure behavior, deterministic merge ordering, and any canonical-row rehydration needed to reject stale, superseded, or wrong-agent results. Connector-derived context is fenced as untrusted before it becomes model-visible.

New automatic transcript writes must not be forwarded to an enrichment connector. Migration policy must explicitly define any connector-side handling of legacy `MemoryCategory::Conversation` rows, prevent those derivatives from being recalled as curated memory, and provide cleanup or rebuild guidance. A future session-history index requires its own explicit authority, privacy, retention, and scoping decision; it cannot arrive implicitly through the curated-memory seam.

The first implementation may remain SQLite-specific. Generalizing enrichment over other durable stores waits for a concrete second consumer rather than making every backend adopt one connector's assumptions.

### Legacy conversation rows require an explicit transition

Existing `MemoryCategory::Conversation` data is compatibility state, not the future authority for transcripts. Migration must preserve rollback and avoid silent deletion. Implementations may retain compatibility reads, migrate recoverable turns into the correct session store, deliberately promote selected durable facts into curated memory, or expire rows under documented retention rules.

No implementation may treat bulk transcript-to-curated-memory promotion as the default. Promotion is a bounded, provenance-aware decision, not a storage-format conversion.

### Acceptance gates

This ADR remains proposed until all of these conditions are met:

- new automatic transcript writes use canonical session stores on the covered runtime, gateway, channel, and ACP paths;
- curated memory cannot override conflicting current input or session history, and ACP/Code automatic-memory isolation remains enforced;
- the authoritative-store/enricher seam is live with authoritative-first writes, scoped and capability-gated recall, untrusted-context fencing, and failure fallback;
- new automatic transcript writes are excluded from enrichment connectors; and
- compatibility and migration behavior for legacy conversation rows, including category-safe connector recall and cleanup or rebuild behavior, is documented and covered by rollback-capable tests.

## Consequences

Positive consequences:

- Conversation resume, transcript retention, and transcript deletion have one canonical owner.
- Curated memory can evolve without becoming a second session-history system.
- External enrichment can improve recall without taking durable authority from the configured memory backend.
- Connector outages and weaker connector protocols fail back to scoped authoritative data.
- ACP/Code sessions keep their existing protection against memory drift, including ZeroCode's ACP pane.

Negative consequences:

- Existing transcript autosave paths and legacy conversation rows need phased compatibility and migration work.
- Session stores must provide enough fidelity and coverage before transcript writes can leave general memory.
- Enrichment adapters cannot receive every `Memory` write indiscriminately; the shared wrapper must enforce category and scope boundaries.
- Operators may temporarily see both legacy conversation rows and canonical session history while migration remains active.

## References

- [ADR-005: Memory storage is backend-neutral with SQLite as the default](./ADR-005-pluggable-memory-backends.md)
- [FND-002: Documentation standards](../../foundations/fnd-002-documentation-standards.md)
- [Memory and payload lifecycle](../memory-payload-lifecycle.md)
- [RFC #9048: Separate conversation history from agent-curated long-term memory](https://github.com/zeroclaw-labs/zeroclaw/issues/9048)
- [RFC #9103: Separate authoritative memory storage from optional enrichment connectors](https://github.com/zeroclaw-labs/zeroclaw/issues/9103)
- [Memory lifecycle policy #6850](https://github.com/zeroclaw-labs/zeroclaw/issues/6850)
- [Enrichment implementation PR #9072](https://github.com/zeroclaw-labs/zeroclaw/pull/9072)
- `crates/zeroclaw-api/src/memory_traits.rs`
- `crates/zeroclaw-memory`
- `crates/zeroclaw-infra/src/session_backend.rs`
- `crates/zeroclaw-infra/src/session_store.rs`
- `crates/zeroclaw-infra/src/acp_session_store.rs`
