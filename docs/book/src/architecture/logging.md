# Logging architecture

ZeroClaw has one logging crate: `zeroclaw-log`. Every event the
workspace emits — `tracing::info!`, `tracing::error!`, the
crate-local `record!` macro, the runtime's legacy `runtime_trace`
shim — converges on a single pipeline owned by this crate.

This page describes the pipeline. For operator-facing config and
query syntax see [Logs & observability](../ops/observability.md).

## The emission surface

There are three ways a call site can emit:

```rust
// 1. Vanilla tracing — the most common case.
tracing::info!(channel = %composite, "connected and identified");

// 2. `record!` — structured event with explicit action / category /
//    outcome, mirroring `LogEvent` field names.
zeroclaw_log::record!(
    INFO,
    action: "channel_message_inbound",
    category: "channel",
    channel: composite,
    message: "inbound message",
    sender: msg.sender,
);

// 3. `scope!` — open a span for a future. Every `tracing::*` /
//    `record!` emission inside the future inherits the span fields.
zeroclaw_log::scope!(
    category: "channel",
    agent_alias: ctx.agent_alias.as_str(),
    channel: composite.as_str(),
    => async move { /* per-message work */ }
).await;

// 4. `spawn!` — `tokio::spawn` that propagates the caller's current
//    span into the spawned task.
zeroclaw_log::spawn!(async move { /* child task */ });
```

All four paths land at the same `LogCaptureLayer` (registered once in
the daemon's `tracing-subscriber` chain). The Layer is the SOLE write
path. Adding a new emission style is unnecessary — pick one of the
four and use the registered field names.

## The Layer

`crates/zeroclaw-log/src/layer.rs` is the entire surface. The Layer
hooks three `tracing-subscriber` callbacks:

- `on_new_span` / `on_record`: visit the span's fields. Known
  attribution names (the `ATTRIBUTION_FIELDS` + `COMPOSITE_PREFIXES`
  registry in `event.rs`) accumulate into a `ZeroclawAttribution`
  marker stashed in span extensions. When a `category` field is set,
  the parsed `EventCategory` lands in a separate `SpanCategory`
  marker. Everything else is ignored at the span level (it'll surface
  later when the event itself fires).
- `on_event`: build a `LogEvent` from the event's fields. Walk the
  span scope leaf→root and merge each ancestor's
  `ZeroclawAttribution` into the event (child-wins, parent-fills).
  When the event didn't carry `category`, pick the closest enclosing
  `SpanCategory`. Fall back to `EventCategory::Internal` if no
  ancestor stamped one. Call `record_event(event)`.

The pipeline fans `record_event` out three ways:

1. **JSONL writer** — when `[observability] log_persistence` is
   `"rolling"` or `"full"`. Append a line, fsync, optionally trim.
2. **Broadcast hook** — every event lands on a process-wide
   `tokio::sync::broadcast::Sender<Value>` the gateway installs. The
   dashboard's SSE stream subscribes there.
3. **Observer bridge** — for actions that map to a typed
   `ObserverEvent` (LLM request, tool call, channel message),
   `observer_bridge::forward` projects the `LogEvent` onto the typed
   variant. Prometheus / OTel collectors consume the typed surface.

The three fan-outs are independent. A `record!` call returns
synchronously after all three have been invoked (the broadcast and
observer paths are nonblocking sends; the JSONL append is sync but
serialized behind a per-writer mutex).

## Attribution

`ZeroclawAttribution` is a flat `BTreeMap<String, String>` (plus a
numeric `duration_ms`). Adding a new alias-bound key requires
extending one of two constants in `event.rs`:

```rust
pub const ATTRIBUTION_FIELDS: &[&str] = &[
    "agent_alias",
    "tool",
    "session_key",
    "cron_job_id",
    "risk_profile",
    "runtime_profile",
    "memory_namespace",
    "skill_bundle",
    "knowledge_bundle",
    "mcp_bundle",
    "peer_group",
    "model",
    "embedding_provider",
];

pub const COMPOSITE_PREFIXES: &[&str] = &[
    "channel",
    "model_provider",
    "tts_provider",
    "transcription_provider",
    "tunnel_provider",
];
```

Each composite prefix emits three on-disk keys: `<prefix>`,
`<prefix>_type`, `<prefix>_alias`. The Layer's `set_composite`
splits a `<type>.<alias>` value on the first `.`.

The Layer is "smart" about bare values on composite-prefix fields: a
caller passing `model_provider = "anthropic"` (no `.`) lands only in
`model_provider_type`. The parent span's full composite (set once at
agent loop entry) still wins on the leaf→root merge.

Adding a new field is one line. Every consumer reads the registry at
runtime: the reader, the gateway's `/api/logs` validator, the dashboard
(via `attribution_keys` in every response).

## Span-carried category

`EventCategory` (`agent` / `channel` / `cron` / `memory` / `tool` /
`provider` / `session` / `system` / `internal`) is not derived from
module paths. It travels via spans:

- `agent::run` instruments with `category = "agent"`.
- `spawn_supervised_listener` constructs the `channel_listener` span
  with `category = "channel"`.
- `process_channel_message` opens a `scope!` with `category = "channel"`.
- Cron's `subagent` span carries `category = "cron"`.

The Layer reads the closest ancestor `SpanCategory` when the event
itself didn't set one. Adding a subsystem means stamping a category
on its entry span — not editing a string-match function.

## The on-disk file

JSONL: one event per line, `0o600` on Unix, `sync_data` after every
write. The writer (`writer.rs`):

- Serializes `LogEvent` to a single line with `serde_json::to_writer`.
- Appends, fsyncs, re-asserts `0o600`.
- Runs a streaming rolling trim when `log_persistence = "rolling"`
  exceeds `log_persistence_max_entries`. The trim never slurps; it
  streams the file line-by-line into a temp file (keeping the last
  N), fsyncs, and atomically renames.

The reader (`reader.rs`) is a one-pass, single-allocation-bounded
streamer: it reads forward through the file, applies the filter, and
keeps the last `limit` matches in a `VecDeque`. Reversed in place for
newest-first output. Cursor pagination is `(timestamp, id)` tuples;
RFC 3339 with milliseconds sorts lexicographically, so the cursor is
just a string compare. The caller pages older by passing the prior
response's `next_cursor` back as `until_ts` / `until_id`.

## Schema migration

`migrate::migrate_legacy_jsonl_in_place` runs once on writer init.
It detects schema-1 rows by shape, streams a temp file with v2 lines
in place of v1, and atomic-renames. v2 rows in the same file are
passed through. Pure streaming — RAM is bounded by one line.

Failures are logged but don't block the daemon. The old v1 rows
remain readable by external tools; they just won't pass the v2
reader's deserializer.

## Tool I/O capture

`tool_io.rs` defines `capture_tool_input` / `capture_tool_output`.
The runtime's `LeakDetector` scans the raw payload first (it owns the
regex tables); the post-scan text is passed here for policy
enforcement:

- `log_tool_io = "off"` → returns `None`. Only tool name + outcome +
  duration land in the event.
- `log_tool_io = "redacted"` → text is truncated at
  `log_tool_io_truncate_bytes` (char-boundary safe). The capture
  carries `original_bytes` and `truncated: bool` so the dashboard can
  render a "truncated" badge.
- `log_tool_io = "full"` → text is kept as-is.

`log_tool_io_denylist` short-circuits to `None` regardless of policy.
Use it for tools whose I/O is intrinsically sensitive.

## Terminal formatter

`src/main.rs` registers a custom `FormatEvent` that prefixes every
line with the closest enclosing alias-bound identity:

- `agent_alias` from the span scope → `[<agent_alias>]`
- otherwise `channel` (the composite) → `[<channel>]`
- otherwise → `[system]`

Followed by the timestamp, level, and the standard tracing-subscriber
event body (target, span chain, message + fields). The formatter does
not parse the JSONL — it reads the live `ZeroclawAttribution` marker
straight from span extensions.

## What lives where

```
crates/zeroclaw-log/src/
  event.rs           — LogEvent, EventCategory, EventOutcome,
                       ZeroclawAttribution, ATTRIBUTION_FIELDS,
                       COMPOSITE_PREFIXES, is_attribution_field
  layer.rs           — LogCaptureLayer (the sole write path)
  macro.rs           — record!, scope!, spawn!
  writer.rs          — JSONL append + rolling trim + init_from_config
  reader.rs          — paginated cursor reader behind /api/logs
  config.rs          — StoragePolicy, ToolIoPolicy, ResolvedPolicy
  broadcast.rs       — process-wide broadcast::Sender<Value>
  observer_bridge.rs — LogEvent → ObserverEvent projection
  tool_io.rs         — leak-scan + truncation + denylist
  migrate.rs         — schema-1 → schema-2 streaming migration
  chain.rs           — display_chain helper for anyhow chains
```

Touch the source before you trust the prose on this page.
