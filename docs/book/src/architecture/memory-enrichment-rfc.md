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
[fork issue 9](https://github.com/yanchenko/zeroclaw/issues/9). It supersedes
the separate Lucid and Shodh proposals in fork issues 7 and 8.

Human sponsor: `@yanchenko`. Drafting assistance: Codex.

## Status

Implemented in the fork. This document is the versioned design record; it does
not claim ratification by the upstream ZeroClaw maintainers.

## Problem

Lucid and Shodh supplement ZeroClaw's memory recall, but neither should replace
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
Lucid and Shodh implement only their protocol translation and capability
declarations.

```text
memory.backend ───────────────► authoritative store ───────► SQLite
                                      │
memory.enricher ─► connector factory ─┴─► EnrichedMemory<E>
                                                │
                                  ┌─────────────┴─────────────┐
                                  ▼                           ▼
                          Lucid CLI adapter          Shodh REST adapter
                                                    local or remote server
```

The initial seam supports SQLite as the authoritative store. Extending it over
another durable backend requires a follow-up design based on demonstrated
cross-store requirements.

## Goals

- Keep one source of truth for durable storage and one for connector choice.
- Preserve SQLite as the canonical row store when enrichment is enabled.
- Make Lucid and Shodh adapters as thin as their protocols allow.
- Centralize agent and session scope, fallback, merge, cooldown, and cleanup.
- Make a future connector additive rather than another product-wide backend.

## Non-goals

- Replacing SQLite with Lucid or Shodh.
- Synchronizing all existing SQLite rows into a newly enabled connector.
- Generalizing enrichment over PostgreSQL, Qdrant, or Markdown in this RFC.
- Starting, installing, upgrading, or supervising a Shodh server process.
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

```toml
[memory]
backend = "sqlite.default"
enricher = "shodh.semantic"

[storage.sqlite.default]

[memory_enrichment.shodh.semantic]
base_url = "http://127.0.0.1:3030"
api_key = "replace-with-the-shodh-api-key"
recall_timeout_ms = 2000
store_timeout_ms = 5000
local_hit_threshold = 3
failure_cooldown_ms = 15000

[memory_enrichment.lucid.local]
binary_path = "/opt/lucid/bin/lucid"
recall_timeout_ms = 2500
store_timeout_ms = 3000
```

An agent inherits the global connector when its field is omitted. A dotted
reference selects another typed alias. The string `none` explicitly disables
enrichment for that agent.

```toml
[agents.research.memory]
backend = "sqlite"
enricher = "lucid.local"

[agents.private.memory]
backend = "sqlite"
enricher = "none"
```

Enrichment with a non-SQLite durable backend fails validation. Dotted aliases
must resolve. Bare `lucid` remains a zero-configuration convenience; Shodh
requires a typed alias because its API credential and deployment details are
operationally significant.

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

### Shodh

Shodh uses its authenticated REST API. The same adapter targets either:

- the official local binary started with `shodh server` on loopback; or
- a separately managed remote Shodh deployment.

The binary is a server deployment, not a Lucid-style executable invoked for
each memory operation. ZeroClaw therefore does not create a second binary
adapter or spawn one server per agent-memory handle. Operators supervise one
shared local process with their normal service manager and point `base_url` at
it. This preserves one request/response implementation for local and remote
deployments.

Each stable ZeroClaw agent UUID maps to a Shodh `user_id`. Cross-agent recall
makes bounded calls only for UUIDs already admitted by `read_memory_from`.
ZeroClaw keys, sessions, namespaces, and categories are encoded as unambiguous
tags.

Shodh results are candidate references. The wrapper looks up the corresponding
key and agent UUID in SQLite, rejects missing or superseded rows, and returns
the canonical local content with the remote score. This prevents stale Shodh
state from resurrecting deleted or replaced memory.

## Failure and consistency semantics

SQLite remains usable when a connector is missing, slow, malformed, or
unavailable:

1. A write succeeds locally before connector synchronization is attempted.
2. Recall queries SQLite first.
3. The connector runs only when local results do not meet the configured
   threshold and its capabilities match the request.
4. A connector error returns local results and starts a bounded cooldown.
5. Cleanup is best-effort and dispatched only when the adapter declares it.

There is no distributed transaction between SQLite and a connector. Connector
state is a rebuildable derivative, not another authority.

## Security and privacy boundaries

- Durable backend locking compares the authoritative storage kind, not the
  connector label.
- Agent allowlists are resolved by ZeroClaw and cannot be expanded by Shodh.
- Canonical Shodh candidates are rehydrated under the caller's agent scope.
- Lucid is not queried where its unscoped protocol cannot preserve isolation.
- Lucid-derived context is marked as untrusted external enrichment before it
  becomes model-visible.
- Shodh credentials use the existing secret-field machinery.
- Shodh URLs reject embedded credentials, query strings, fragments, and
  non-HTTP schemes.
- A local Shodh server should bind to loopback. Remote exposure requires the
  server's own TLS and access-control deployment policy.

## Product surface

Storage selectors list only real durable stores: SQLite, PostgreSQL, Qdrant,
Markdown, and `none`. Lucid and Shodh appear under one Memory Enrichment entry
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
internal because Lucid and Shodh are first-party compositions of the existing
`Memory` contract.

## Configuration boundary

Lucid and Shodh are enrichment connectors only. They are not accepted by the
backend enum, storage catalog, memory factory, migration factory, or Quickstart
wire shape. Validation rejects enrichment over a non-SQLite backend.

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

### Keep Lucid and Shodh as independent backends

Rejected. It misstates data ownership, couples connector changes to backend
locking, and repeats policy and product wiring.

### One adapter with protocol conditionals

Rejected. Lucid's per-operation CLI and Shodh's REST API have genuinely
different translation and capability behavior. Sharing the policy wrapper is
useful; merging unrelated transports into one adapter is not.

### Separate Shodh binary and HTTP adapters

Rejected. The official binary exposes the same HTTP API used remotely. A
second adapter would duplicate request translation and encourage one server
process per memory handle. Deployment lifecycle is distinct from connector
protocol.

### Embed the Shodh Rust crate

Deferred. It would add a large in-process dependency and a new lifecycle and
storage-ownership question without improving the established connector seam.

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
- stale remote state cannot overrule SQLite authority.

Negative:

- enrichment is initially SQLite-only;
- connector state can lag SQLite and may need rebuilding;
- Shodh local deployment requires an independently supervised server;
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
- aggregate cross-agent recall deadlines and partial Shodh failures;
- derived-context and canonical-reference session behavior;
- stale Shodh row rejection and agent allowlist enforcement;
- Lucid process deadlines and Shodh authenticated REST translation;
- loopback Shodh defaults for the official local binary server;
- storage and enrichment pickers, Quickstart wiring, generated config
  references, and documentation consistency.

The branch was checked with formatting, memory and configuration test suites,
gateway and Quickstart product tests, workspace-wide compilation, strict
targeted linting, and the documentation consistency checker.

## Implementation map

- Configuration and validation: `zeroclaw-config` memory schema.
- Shared seam and factory: `zeroclaw-memory` enrichment and factory modules.
- Protocol adapters: `zeroclaw-memory` Lucid and Shodh modules.
- Product configuration: gateway sections, Quickstart, and ZeroCode.
- Operator reference: [Memory storage and enrichment](../reference/memory-backends.md).
