---
id: FORK-RFC-009
title: Unified memory enrichment configuration and connectors
date: 2026-07-14
type: rfc
status: implemented
last-reviewed: 2026-07-14
discussion: https://github.com/yanchenko/zeroclaw/issues/9
implementation: https://github.com/yanchenko/zeroclaw/tree/refactor/unified-memory-enrichment
relates-to:
  - ADR-002
  - ADR-004
  - crates/zeroclaw-config
  - crates/zeroclaw-memory
---

# RFC: Unified Memory Enrichment Configuration and Connectors

This fork RFC records the design implemented by the
`refactor/unified-memory-enrichment` branch. The discussion source is
[fork issue 9](https://github.com/yanchenko/zeroclaw/issues/9). It establishes
the shared seam and migrates Lucid; additional connectors are separate work.

Human sponsor: `@yanchenko`. Drafting assistance: Codex.

## Status

Implemented in the fork. This document is the versioned design record; it does
not claim ratification by the upstream ZeroClaw maintainers.

## Problem

Lucid supplements ZeroClaw's memory recall, but it should not replace
the durable store selected by `memory.backend`. Modeling each connector as a
full backend makes the architecture say that it owns canonical memory and
forces every connector to repeat integration work across:

- backend enums, profiles, storage aliases, and locking;
- factory construction and per-agent routing;
- Quickstart, gateway, and ZeroCode storage pickers;
- validation, status labels, localization, and documentation;
- local-first fallback, merge, scope, cooldown, and cleanup behavior.

That duplication becomes more dangerous than the extra code suggests. A new
connector can drift on agent isolation, revive a stale remote row, report the
wrong durable backend, or fail differently from an existing connector.

## Proposal

Separate memory configuration into two independent axes:

1. `memory.backend` selects the authoritative durable store.
2. `memory.enricher` optionally selects one best-effort enrichment connector.

Connector-specific settings live in one typed `memory_enrichment` catalog.
One shared SQLite-authoritative wrapper owns every connector-independent rule.
Lucid implements only its protocol translation and capability declarations.

```text
memory.backend ───────────────► authoritative store ───────► SQLite
                                      │
memory.enricher ─► connector factory ─┴─► EnrichedMemory<E>
                                                │
                                                ▼
                                        Lucid CLI adapter
```

The initial seam supports SQLite as the authoritative store. Extending it over
another durable backend requires a follow-up design based on demonstrated
cross-store requirements.

## Goals

- Keep one source of truth for durable storage and one for connector choice.
- Preserve SQLite as the canonical row store when enrichment is enabled.
- Keep the Lucid adapter as thin as its protocol allows.
- Centralize agent and session scope, fallback, merge, cooldown, and cleanup.
- Make a future connector additive rather than another product-wide backend.

## Non-goals

- Replacing SQLite with Lucid.
- Synchronizing all existing SQLite rows into a newly enabled connector.
- Generalizing enrichment over PostgreSQL, Qdrant, or Markdown in this RFC.
- Hiding protocol and capability differences behind an inaccurate common API.

## Sources of truth

The typed configuration is authoritative:

- `memory.backend` is the durable-store selector.
- `memory.enricher` is the optional global connector selector.
- `agents.<alias>.memory.enricher` overrides, inherits, or explicitly disables
  the global selector.
- `memory_enrichment.<kind>.<alias>` is the only canonical catalog for new
  connector settings.
- the factory resolves these selectors once for each handle construction.

Connector handles retain only the immutable operational projection they need,
such as a command path, URL, credential header, and deadline. They do not own a
second mutable selector or connector catalog. A configuration change takes
effect when the owning memory handle is reconstructed, following the existing
factory lifecycle used by other runtime integrations.

Status strings such as `sqlite+lucid` are derived views. They are never stored
as another selector.

## Configuration

The canonical configuration examples — the typed
`[memory_enrichment.lucid.<alias>]` catalog and per-agent `enricher`
selection, inheritance, and explicit disable (`enricher = "none"`) — live in
the operator reference:
[Memory storage and enrichment](../reference/memory-backends.md).

Enrichment with a non-SQLite durable backend fails validation. Dotted aliases
must resolve. Bare `lucid` remains a zero-configuration convenience.

## Shared runtime contract

`EnrichedMemory<E>` is the only implementation of enrichment policy. It owns:

- write ordering with SQLite committed first;
- best-effort connector writes;
- local-hit thresholds and connector failure cooldown;
- deterministic local-first merge and deduplication;
- agent allowlist and session-window enforcement;
- canonical-row rehydration and stale-reference rejection;
- capability-gated recall and cleanup dispatch;
- full delegation of the canonical SQLite memory surface.

`MemoryEnricher` is an edge contract with only three operations: store, recall,
and cleanup. Each adapter also declares stable capabilities:

- result kind: derived context or canonical-row reference;
- recall scope: unscoped only or agent allowlist;
- recall support: semantic, recent, or both;
- cleanup support: none or agent-scoped.

The wrapper makes security and lifecycle decisions from those capabilities
before invoking a connector. An adapter cannot widen caller scope by returning
more data than its declaration permits.

## Connector designs

### Lucid

Lucid is a local command-line adapter. It invokes the configured binary for
store and context operations, enforces per-command deadlines, and translates
Lucid context output into derived entries.

Lucid results are derived context rather than canonical SQLite references. The
CLI cannot express an agent allowlist, so the wrapper skips it for agent-scoped
cross-agent recall. Lucid declares no cleanup capability.

## Failure and consistency semantics

SQLite remains usable when a connector is missing, slow, malformed, or
unavailable. The operational behavior — local-first write ordering, the
local-hit recall threshold, and the connector failure cooldown — is
documented canonically in
[Memory storage and enrichment § Failure and backup behavior](../reference/memory-backends.md#failure-and-backup-behavior).
Cleanup is best-effort and dispatched only when the adapter declares it.

There is no distributed transaction between SQLite and a connector. Connector
state is a rebuildable derivative, not another authority.

## Security and privacy boundaries

- Durable backend locking compares the authoritative storage kind, not the
  connector label.
- Lucid is not queried where its unscoped protocol cannot preserve isolation.
- Lucid-derived context is marked as untrusted external enrichment before it
  becomes model-visible.

## Product surface

Storage selectors list only real durable stores: SQLite, PostgreSQL, Qdrant,
Markdown, and `none`. Lucid appears under a separate Memory Enrichment entry
with connector-specific configuration beneath it.

Quickstart and ZeroCode no longer present either connector as a backend. A
connector is configured only through `memory.enricher` and the
`memory_enrichment` catalog, so product boundaries cannot recreate the
incorrect storage model.

## Alignment with ZeroClaw patterns

This design follows existing repository patterns rather than introducing a
parallel integration system:

- **Trait-driven composition:** the public kernel contract remains `Memory`.
  `MemoryEnricher` is a narrow internal composition seam inside the memory
  crate, not a competing public backend or plugin ABI.
- **Decorator ownership:** `EnrichedMemory<E>` wraps canonical memory in the
  same style as audited and agent-scoped memory wrappers. Cross-connector
  behavior lives in the decorator, not the adapters.
- **Typed alias maps:** `memory_enrichment.<kind>.<alias>` follows the V3
  provider and storage catalog shape and resolves through typed configuration.
- **Factory wiring:** configuration resolution and concrete construction stay
  in the memory factory. Agent loops and product surfaces do not instantiate
  connector implementations directly.
- **Single source of truth:** storage and connector selections remain in
  `Config`; effective labels and runtime handles are derived projections.
- **Schema-derived UI:** the gateway walks the typed catalog, while storage
  pickers continue to use the canonical backend inventory.
- **Fail-closed capability checks:** unsupported scope, recall, or cleanup
  operations remain local instead of guessing connector behavior.

If enrichment becomes a third-party extension surface, its stable contract
belongs in `zeroclaw-api` or the versioned WIT memory world. This RFC keeps it
internal because Lucid is a first-party composition of the existing
`Memory` contract.

## Configuration boundary

Lucid is an enrichment connector only. It is not accepted by the
backend enum, storage catalog, memory factory, migration factory, or Quickstart
wire shape. Validation rejects an explicitly configured enricher over a
non-SQLite backend; an agent that merely inherits the install-wide
`memory.enricher` over a backend that cannot enrich resolves to no enricher
(enrichment is silently off for that agent).

### Legacy compatibility

The pre-enrichment `backend = "lucid"` selector — global `memory.backend` and
per-agent `agents.<alias>.memory.backend` — remains accepted as deprecated
compatibility input. It always meant SQLite-authoritative storage with Lucid
syncing alongside, so config load normalizes it to the canonical shape before
typed deserialization and logs a deprecation warning naming the migration
(`backend = "sqlite"` + `enricher = "lucid"`):

- with no explicit `enricher` at the same scope, `enricher = "lucid"` is
  implied (a `[memory_enrichment.lucid.default]` entry supplies typed
  settings when present; built-in defaults otherwise);
- an explicit `enricher = "none"` at that scope wins and enrichment stays
  off;
- the legacy backend combined with an explicit canonical Lucid enricher at
  the same scope is rejected as ambiguous — remove the legacy value.

The legacy `[storage.lucid.<alias>]` table is not part of the schema and is
ignored; its `binary_path` moves to `[memory_enrichment.lucid.<alias>]`.

## Rollout and rollback

The fork branch introduces the typed catalog, shared wrapper, adapters, product
surface, and documentation together. There is no partial mode in which a
connector appears as both storage and enrichment.

Rollback is configuration-safe:

1. Set `memory.enricher = "none"` or remove the field.
2. Continue using the unchanged SQLite database.
3. Stop the external connector process if it is no longer needed.
4. Remove its catalog alias after no agent references it.

No durable-memory migration is necessary because SQLite remains authoritative
throughout rollout and rollback.

## Alternatives considered

### Keep Lucid as an independent backend

Rejected. It misstates data ownership, couples connector changes to backend
locking, and repeats policy and product wiring.

### Generalize the wrapper over every durable backend

Deferred. The current implementation relies on SQLite-specific canonical-row
and agent-identity behavior. Generalization should follow a concrete second
store rather than speculative abstraction.

## Consequences

Positive:

- shared policy exists once and is tested once with capability variations;
- adapters are limited to protocol, authentication, translation, and declared
  capability differences;
- product surfaces grow by connector configuration rather than backend fanout;
- connector state cannot overrule SQLite authority.

Negative:

- enrichment is initially SQLite-only;
- connector state can lag SQLite and may need rebuilding;
- Lucid cannot participate in agent-allowlisted recall with its current CLI.

Known lifecycle constraint:

- connector configuration changes require reconstruction of the owning memory
  handle, matching the current factory lifecycle rather than adding a mutable
  connector-config cache.

## Verification

The implementation includes tests for:

- global and per-agent selection, inheritance, and explicit disable;
- SQLite-only validation and authoritative backend locking;
- factory resolution and missing-alias diagnostics;
- local-first writes, deterministic merge, deduplication, fallback, cooldown,
  and cleanup dispatch;
- derived-context and canonical-reference session behavior;
- canonical-row rehydration and agent allowlist enforcement in the shared seam;
- Lucid process deadlines and command translation;
- storage and enrichment pickers, Quickstart wiring, generated config
  references, and documentation consistency.

The branch was checked with formatting, memory and configuration test suites,
gateway and Quickstart product tests, workspace-wide compilation, strict
targeted linting, and the documentation consistency checker.

## Implementation map

- Configuration and validation: `zeroclaw-config` memory schema.
- Shared seam and factory: `zeroclaw-memory` enrichment and factory modules.
- Protocol adapter: `zeroclaw-memory` Lucid module.
- Product configuration: gateway sections, Quickstart, and ZeroCode.
- Operator reference: [Memory storage and enrichment](../reference/memory-backends.md).
