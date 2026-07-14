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

Lucid and Shodh are separate thin protocol connectors behind the same
SQLite-authoritative wrapper. The wrapper owns local persistence, agent and
session scoping, result merging, fallback, cooldown, cleanup dispatch, and
canonical-row rehydration. A connector owns only authentication, transport,
request and response translation, and its declared capabilities.

Configure an alias in the single enrichment catalog and reference it from
memory:

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
```

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
enricher = "shodh.semantic"

[agents.private.memory]
backend = "sqlite"
enricher = "none"
```

## Connector capabilities

| Connector | Protocol | Recall result | Agent scope | Remote cleanup |
|---|---|---|---|---|
| Lucid | local CLI | derived context | unscoped only | none |
| Shodh | authenticated REST over HTTP | canonical SQLite references | configured agent allowlist | agent-scoped |

Lucid-derived context can supplement a session-scoped recall because it is not
presented as an exact durable row. Lucid external recall is skipped when an
agent-scoped allowlist is required because the CLI protocol cannot express
that boundary. Every Lucid-derived entry is marked as untrusted external
enrichment before it enters model-visible memory context. The current Lucid
command protocol has no importance parameter, so importance remains on the
authoritative SQLite row and is not forwarded to Lucid.

Each stable ZeroClaw agent UUID becomes a separate Shodh `user_id`.
Cross-agent recall makes one bounded request for each UUID already allowed by
`read_memory_from`; the connector cannot widen that allowlist. Session,
namespace, category, and ZeroClaw key metadata are mirrored as tags.

Shodh's downloadable local binary and a remote Shodh deployment use the same
connector transport. The binary is a server, not a Lucid-style command invoked
once per store or recall: start it with `shodh server`, keep the default
loopback `base_url`, and configure the API key created by `shodh init`. For a
remote deployment, change `base_url` and use that server's key. ZeroClaw does
not spawn or supervise the Shodh server; a service manager keeps one shared
process independent from individual agent-memory handles. See Shodh's
[direct server documentation](https://github.com/varun29ankuS/shodh-memory/blob/main/docs/direct-server-systemd.md)
for a local service example.

Shodh candidates never become authoritative. ZeroClaw maps each remote result
back to its SQLite key and agent UUID and returns the canonical local row. A
stale remote candidate is discarded after its local row is deleted. Scoped
forget, session purge, and agent purge are propagated only on a best-effort
basis.

Recent-only recall remains local for Shodh. Existing SQLite rows are not
bulk-uploaded when an enricher is enabled; subsequent writes and updates feed
the connector.

## Failure and backup behavior

A successful write commits to SQLite before either external connector is
called. Recall searches SQLite first and calls the connector only below its
local-hit threshold. Connector timeout or failure preserves local results and
enters a short cooldown to avoid repeated slow failures. For cross-agent Shodh
recall, `recall_timeout_ms` bounds the aggregate operation rather than applying
once per authorized agent; successful partial results are retained and partial
failures are logged.

Back up `data/memory/` as usual. External Lucid or Shodh state is derived and
may be backed up separately only when preserving its index is operationally
useful.
