# Writing a Memory Plugin

A memory plugin is a storage backend: it persists what the agent remembers
and answers recall queries. It is the most data-model-heavy plugin kind. Where
a tool has one function and a channel has a message shape, a memory backend
has multi-agent attribution, namespaces, sessions, categories, and importance
weighting, and the runtime's memory semantics (scoped recall, GDPR export,
supersession) lean on your implementation getting the row model right.

This guide assumes the [tool plugin](./writing-a-tool-plugin.md) basics and
the warm-store lifecycle from the
[channel guide](./writing-a-channel-plugin.md). It is checked against
`wit/v0/memory.wit` and the host adapter in
`crates/zeroclaw-plugins/src/wasm_memory.rs`.

> **Wiring status.** `WasmMemory` implements the runtime's full `Memory` trait
> against the `memory-plugin` world, capability-gated and unit-covered. The
> runtime does not yet construct it as a configurable backend; the host lacks
> a memory counterpart to `channel_plugin_details()`. As with channels, build
> against the contract: the WIT world and the adapter semantics are the part
> that freezes.

## The data model

One record type crosses the boundary in both directions, `memory-entry`
(`memory.wit`). Internalize its fields before designing storage, because the
optional methods are all views over them:

| Field | Meaning |
|-------|---------|
| `id` | Row identity. |
| `key` | Lookup key. **Not unique**: multiple rows may share a key, one per agent. Your storage must key on `(key, agent-id)`, not `key`. |
| `content` | The remembered text. |
| `category` | `core` (long-term facts), `daily` (session logs), `conversation` (context), or `custom(string)`. |
| `timestamp` | RFC 3339 creation time. Time-range recall bounds are inclusive. |
| `session-id` | Optional conversation scope. |
| `namespace` | Isolation boundary between agents or contexts. |
| `score` | Retrieval relevance 0.0-1.0; `none` for non-vector recall. |
| `importance` | Optional prioritization weight 0.0-1.0. |
| `superseded-by` | ID of the entry that replaced this one, if any. |
| `agent-alias` / `agent-id` | Display name versus raw storage identifier. Use `agent-id` for scope equality checks, `agent-alias` for display. |

The `(key, agent-id)` composite is the single most common mistake. The base
`get` contract explicitly says: when multiple rows share a key, an arbitrary
matching row is returned, and agent-scoped lookup goes through
`get-for-agent`. Likewise `forget` deletes all rows for a key regardless of
attribution, while `forget-for-agent` deletes exactly the `(key, agent-id)`
row and leaves siblings alone.

## Required exports

Twelve functions have no default and must work
(`memory.wit`, required-methods section):

| Export | Contract notes |
|--------|----------------|
| `name` | Backend name. |
| `get-memory-capabilities` | Bitmask of optional methods; read once at load. |
| `store-entry` | Store `(key, content, category, session-id)`. Named `store-entry` because `store` is reserved in wit-bindgen. |
| `recall` | Query + limit + optional session and RFC 3339 time bounds (inclusive). An empty or bare-`*` query means time-only recall: return the most recent entries. |
| `get` | By key; arbitrary row on multi-agent key collision. |
| `list-entries` | Optional category and session filters. Named for the wit-reserved `list`. |
| `forget` | Delete all rows for key; `true` if anything was deleted. |
| `forget-for-agent` | Delete the `(key, agent-id)` row only. |
| `count` | Total entries. |
| `health-check` | Reachability. |
| `store-with-agent` / `recall-for-agents` | The attribution-aware pair; see below. |

`recall-for-agents` takes an `agent-filter` variant: `all` (no agent filter)
or `some(list<string>)` (restrict to the listed agent IDs). The runtime maps
its Rust `&[&str]` slice as empty-slice-means-`all`, so treat `some([])` as
matching nothing, not everything.

## Capability flags: the {{#include ../_snippets/plugin-memory-flag-count.md}} optional methods

Same mechanism as channels: the host reads `get-memory-capabilities` once,
and for each unset flag it uses the Rust trait default instead of calling
you. The defaults are documented inline in `memory.wit` next to the flags,
and the host-side fallbacks are visible in `wasm_memory.rs` (each gated
method checks the flag and takes the fallback path when absent):

| Flag | Host fallback when unset |
|------|--------------------------|
| `get-for-agent` | Host composes `get` + agent-id equality filter |
| `purge-namespace`, `purge-session`, `purge-session-for-agent`, `purge-agent` | Host returns "not supported" |
| `reindex` | Host returns 0 |
| `store-procedural` | Host no-ops |
| `ensure-agent-uuid` | Host echoes the alias unchanged |
| `recall-namespaced` | Host calls `recall` and post-filters by namespace |
| `export-entries` | Host calls `list-entries` and post-filters |
| `store-with-metadata` | Host delegates to `store-entry`, dropping namespace and importance |

Read that last row twice: if you do not implement `store-with-metadata`, the
namespace and importance the runtime asked for are **silently dropped** by the
fallback. A backend that stores namespaced data should implement
`store-with-metadata`, `recall-namespaced`, and `purge-namespace` as a set,
or namespace isolation quietly degrades to post-filtering and lossy writes.

The purge family is your data-deletion surface. `purge-agent` takes an
`agent-alias` (not an ID); `export-entries` exists for GDPR Art. 20 data
portability and must return entries ordered by creation time ascending with
embeddings excluded. If your backend serves real user data, implement the
purge and export flags; "not supported" is an acceptable answer only for
throwaway backends.

## Sketch: the storage shape

The component pattern is the channel one (warm instance, `thread_local`
state), so only the data layer differs. A minimal honest backend is an
in-memory map, keyed correctly:

```rust
use std::collections::HashMap;

struct Row {
    id: String,
    content: String,
    category: Category,
    timestamp: String,
    session_id: Option<String>,
    namespace: String,
    importance: Option<f64>,
    superseded_by: Option<String>,
    agent_alias: Option<String>,
}

/// (key, agent_id) -> Row. agent_id None models unattributed rows.
type Table = HashMap<(String, Option<String>), Row>;
```

Every required method is then a straightforward walk:

- `store-entry` inserts at `(key, None)` with namespace `"default"`.
- `store-with-agent` inserts at `(key, agent-id)` with the caller's
  namespace and importance.
- `recall` filters by substring/rank against `content`, applies the session
  filter, applies inclusive RFC 3339 bounds against `timestamp`, sorts, and
  truncates to `limit`. Handle the empty/`*` query as most-recent-first.
- `recall-for-agents` adds the agent-filter walk over the key's second
  component.

A real backend swaps the map for its store (an embedded KV, a remote vector
DB over `wasi:http` with the `http_client` permission) without the contract
changing shape.

## What the host does around you

Knowing the adapter behavior in `wasm_memory.rs` explains several contract
edges:

- **Warm store, refueled per call.** Same as channels: one instance for the
  plugin's lifetime, fresh fuel each call, calls serialized behind a mutex.
  Your backend never sees concurrent calls.
- **Capabilities cached at load.** The host reads your flags once during
  `from_wasm` and never again. There is no dynamic capability discovery;
  restarting the daemon is what re-reads them.
- **Trap wrapping.** Every call site wraps traps with a named context
  (`memory.recall-namespaced trapped`, etc.). A trap in one call does not
  tear down the plugin, but repeated traps make the backend useless; return
  `err(string)` for expected failures instead of panicking.
- **Post-filter fallbacks are host-side.** When your `recall-namespaced`
  flag is unset, the namespace filter runs on the host after your `recall`
  returned. Your `limit` handling interacts with that: the host passes the
  caller's limit to `recall`, so post-filtering can under-fill the result.
  Another reason to implement the namespaced variants natively.

## Manifest, build, install

{{#include ../_snippets/plugin-manifest-fields.md}}

For a memory backend: `capabilities` containing `memory`; `config_read` if
the backend needs connection settings; `http_client` only if it talks to a
remote store.

{{#include ../_snippets/plugin-build-component.md}}

{{#include ../_snippets/plugin-install-layout.md}}

## Next

- [Skill bundles](../tools/skill-bundles.md): markdown-only skill
  distribution through the same install machinery.
- [Distributing plugins](./distributing-plugins.md).
