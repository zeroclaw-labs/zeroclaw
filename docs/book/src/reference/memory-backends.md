# Memory storage and enrichment

ZeroClaw configures memory on two independent axes:

- `memory.backend` selects the authoritative durable store;
- `memory.enricher` optionally selects a best-effort connector layered over
  SQLite.

The architectural rationale, alternatives, migration, and rollback plan are
recorded in the
[unified memory enrichment RFC](../architecture/memory-enrichment-rfc.md).

The durable backend remains locked for an agent after it owns data. Changing or
disabling an enricher does not change the durable store and is not backend
locked.

## Authoritative stores

| Backend | Durable data | Notes |
|---|---|---|
| `sqlite` | SQLite | Default; hybrid keyword and vector recall. |
| `markdown` | Markdown files | Human-readable, per-agent files. |
| `postgres` | PostgreSQL | Shared server-backed storage with optional pgvector. |
| `qdrant` | Qdrant | Vector-store payloads carry agent attribution. |
| `none` | none | Disables persistent memory. |

Memory enrichment currently requires SQLite. It does not make a remote service
authoritative and is not available with Markdown, Postgres, Qdrant, or `none`.

## One enrichment seam

Lucid is a thin protocol connector behind a SQLite-authoritative wrapper. The
wrapper owns local persistence, agent and session scoping, result merging,
fallback, cooldown, cleanup dispatch, and canonical-row rehydration. A
connector owns only its transport, request and response translation, and
declared capabilities.

Lucid can use its built-in defaults with `enricher = "lucid"`, or a typed
alias can specify the binary and deadlines:

```toml
[memory]
backend = "sqlite.default"
enricher = "lucid.local"

[memory_enrichment.lucid.local]
binary_path = "/opt/lucid/bin/lucid"
recall_timeout_ms = 2500
store_timeout_ms = 3000
```

An agent inherits `memory.enricher` when its own field is omitted. It can
select another connector alias or explicitly disable inheritance:

```toml
[agents.research.memory]
backend = "sqlite"
enricher = "lucid.local"

[agents.private.memory]
backend = "sqlite"
enricher = "none"
```

## Connector capabilities

| Connector | Protocol | Recall result | Agent scope | Remote cleanup |
|---|---|---|---|---|
| Lucid | local CLI | derived context | unscoped only | none |

Lucid-derived context can supplement a session-scoped recall because it is not
presented as an exact durable row. Lucid external recall is skipped when an
agent-scoped allowlist is required because the CLI protocol cannot express
that boundary. Every Lucid-derived entry is marked as untrusted external
enrichment before it enters model-visible memory context. The current Lucid
command protocol has no importance parameter, so importance remains on the
authoritative SQLite row and is not forwarded to Lucid.

## Failure and backup behavior

A successful write commits to SQLite before the external connector is
called. Recall searches SQLite first and calls the connector only below its
local-hit threshold. Connector timeout or failure preserves local results and
enters a short cooldown to avoid repeated slow failures.

Back up `data/memory/` as usual. External Lucid state is derived and may be
backed up separately only when preserving its index is operationally useful.
