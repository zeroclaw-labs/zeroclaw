# Logging architecture

ZeroClaw has exactly one logging surface: the `zeroclaw_log::record!` macro. Every emission in the workspace, agent loop activity, channel I/O, cron runs, tool calls, memory ops, session lifecycle, errors, flows through it. The macro fires a `tracing` event that the installed subscriber feeds to two sibling layers: the stderr fmt layer (terminal output) and the `LogCaptureLayer`. The fmt layer prints colored, alias-prefixed lines on stderr (muted unless `--verbose`). The `LogCaptureLayer` materializes a structured `LogEvent` and fans it out, via `writer::record_event`, to:

1. The optional Observer bridge (`observer_bridge::forward`) for the subset of actions that map to typed Prometheus / OTel events, but only when a caller has installed a binding with `set_observer_bridge`. The current production bootstrap does not install one.
2. The process-wide broadcast channel for live subscribers such as the dashboard's SSE stream.
3. The asynchronous JSONL writer for `<workspace>/state/runtime-trace.jsonl` (when `[observability] log_persistence` is `"rolling"`, `"full"`, or `"rotating"`).

When the Observer bridge is bound, its projection and the broadcast send happen before the persistence enqueue attempt. Those three destinations have different completeness and durability guarantees; sharing one `LogEvent` does not make them interchangeable.

## Read this first: attribution is not attrs

Every log event carries two completely separate channels of structured data. Confusing them is the single most common mistake at a call site, so internalize the split before anything else:

| | **Attribution** (`zeroclaw.*`) | **Attrs** (`attributes.*`) |
|---|---|---|
| **Answers** | *Who* did it and *under what context* | *What* specifically happened |
| **Examples** | `channel`, `agent_alias`, `model_provider`, `tool`, `session_key`, `cron_job_id` | `bytes_received`, `tokens_used`, `status_code`, error payloads |
| **Source** | **Spans.** Opened at entry points, walked by the layer. | **The call site.** `Event::with_attrs(json!({...}))`. |
| **Appears at the call site?** | **Never.** Not a `record!` argument. | Yes, that's the only place it can come from. |

The rule that falls out of this: **if a value identifies who or what scope an event belongs to, it comes from a span and must never appear at the call site.** Attribution flows in automatically from `attribution_span!` / `scope!` wrappers opened higher up the stack; the layer walks the span scope leaf→root when an event fires and merges every contribution into the event's `zeroclaw.*` block. The call site that fires `record!` names none of it.

Because attribution is the load-bearing half of this split and the half that trips people up, it comes first.

## Attribution: it all comes from spans

**Attribution is never a call-site argument.** Read that again. Channel composite, agent_alias, model_provider, tool, session_key, cron_job_id: none of these are ever typed into a `record!` call. They flow in through tracing spans opened at entry points and walked by the layer when an event fires. If you find yourself wanting to pass `agent_alias` or `tool` to `record!`, stop: the value is already in scope through a span, or it should be, and the fix is to open or fix the span, not to thread the value into the call site.

The mechanism, end to end:

1. A "thing" (channel, provider, agent, tool, cron job, memory backend, …) implements `Attributable` once, next to its struct.
2. Its entry point wraps work in `attribution_span!(self)`, which opens a tracing span carrying that thing's role and alias.
3. Every `record!` fired anywhere inside that span, directly or nested arbitrarily deep, inherits the attribution automatically.
4. When the event fires, the layer walks the span scope **leaf→root**, merges every `Attributable`'s contribution, and writes the merged `zeroclaw.*` block. The call site named none of it.

This is the whole point of the design: per-thing logging code is zero. You impl the trait once and wrap the entry point once; every emission underneath is attributed for free.

### The `Attributable` trait

Lives in `crates/zeroclaw-api/src/attribution.rs` so every crate can implement it without depending on `zeroclaw-log`:

```rust
pub trait Attributable {
    fn role(&self) -> Role;
    fn alias(&self) -> &str;
}
```

Each "thing" in the workspace (a `TelegramChannel`, an `AnthropicModelProvider`, an `Agent`, a cron job, a tool, a memory backend, a peer group, a skill bundle, an MCP bundle, a session) impls `Attributable` once next to its struct.

### The `Role` taxonomy

Closed nested enum:

```rust
pub enum Role {
    Swarm,
    Agent,
    Channel(ChannelKind),       // Telegram, Discord, Slack, Matrix, Lark, ...
    Tool(ToolKind),             // Shell, HttpRequest, FetchUrl, ...
    Cron(CronKind),             // Interval, At, Cron, Once
    Provider(ProviderKind),     // Model, Tts, Transcription, Tunnel
    Memory(MemoryKind),         // Sqlite, Json, InMemory, Markdown, Qdrant, ...
    PeerGroup,
    Skill,
    Mcp,
    Sop,
    Session,
    System,
}
```

`ChannelKind`, `ToolKind`, `CronKind`, `MemoryKind`, and the four `ProviderKind` sub-enums (`ModelProviderKind`, `TtsProviderKind`, `TranscriptionProviderKind`, `TunnelProviderKind`) are all closed. The variant's snake_case form via `strum::IntoStaticStr` is the canonical `<type>` portion of the `<type>.<alias>` composite. Adding a new implementation: extend the relevant `Kind` enum, that's it.

### Opening a span, do this at every entry point

Wrap an entry-point's work with `attribution_span!(thing)`. The macro returns a `Span` carrying the thing's role and alias as structured fields. `.instrument(span)` the future (or `let _g = span.entered()` in sync code). **A spawned task that does not re-establish the span loses attribution**: every `tokio::spawn` body that emits must carry the same `attribution_span!` / `scope!` the parent used, or its emissions land un-attributed.

```rust
use zeroclaw_log::Instrument;

let span = zeroclaw_log::attribution_span!(self);  // self impls Attributable
async move {
    // every record! inside automatically carries the alias-bound fields
    record!(INFO, Event::new(module_path!(), Action::Start), "channel online");
    self.poll_loop().await
}.instrument(span).await
```

The layer walks the span scope leaf→root when an event fires, merges every `Attributable`'s contribution into the event's `zeroclaw.*` attribution block, and emits the composite (`channel = "telegram.clamps"`, `channel_type = "telegram"`, `channel_alias = "clamps"`) without the call site naming any of those keys.

### The `scope!` macro, non-role context

`attribution_span!` is for role-bearing `Attributable` things. For per-scope identifiers that aren't tied to one (sender id, message id, turn id, request id), use `scope!`:

```rust
zeroclaw_log::scope!(
    sender: msg.sender.as_str(),
    message_id: msg.id.as_str(),
    => async move { process_message(msg).await }
).await
```

`scope!` straddles the attribution/attrs line deliberately: field keys that match the alias-bound `ATTRIBUTION_FIELDS` / `COMPOSITE_PREFIXES` (in `crates/zeroclaw-log/src/event.rs`) land in the typed `zeroclaw.*` attribution slot; everything else lands in the event `attributes` map for every descendant emission. Either way the value rides on every nested `record!` without being a call-site argument.

## The `record!` macro and its call-site contract

The `tracing` crate is `zeroclaw-log`'s implementation detail: the `record!` / `scope!` / `attribution_span!` macros expand to `zeroclaw_log::__private::tracing` so a call site never names a tracing type. The log-event macros themselves (`tracing::{trace,debug,info,warn,error}`, `log::*`, `std::dbg`, plus bare `anyhow::anyhow!`) are **hard-banned workspace-wide** as `disallowed-macros` in `clippy.toml`. With `-D warnings` in CI, any direct `tracing::info!` etc. **fails the build**, with a clippy message naming `::zeroclaw_log::record!` as the replacement. This is not a convention; it is enforced.

The only exemptions are the few files inside `crates/zeroclaw-log/` that bootstrap the pipeline and carry a local `#![allow(clippy::disallowed_macros)]`. A handful of crates (`zeroclaw-api`, `zeroclaw-spawn`, `zeroclaw-providers`, `zeroclaw-hardware`, `zeroclaw-log`) still list `tracing` / `tracing-subscriber` in `Cargo.toml`, but only for span and subscriber plumbing, not for emitting log macros. The dependency being present does not license calling the banned macros. (`tokio::spawn` is banned the same way via `disallowed-methods`; use `::zeroclaw_spawn::spawn!` so spawned tasks inherit the caller's attribution span.)

The macro is locked-shape: it takes a level, a single `Event` expression, and a message literal.

```rust
use zeroclaw_log::{record, Event, Action, EventCategory, EventOutcome};

record!(INFO, Event::new(module_path!(), Action::Start), "starting step");
record!(WARN, Event::new(module_path!(), Action::Fail).with_outcome(EventOutcome::Failure).with_attrs(serde_json::json!({"exit_code": 137})), "tool failed");
```

`module_path!()` is the canonical source of the event name: it's the Rust module path of the call site (e.g. `zeroclaw_channels::telegram`), so events are searchable, jump-to-source-able, and impossible to typo. The same convention is used at every `record!` site in the workspace.

The macro injects `file!()` and `line!()` automatically. The `LogCaptureLayer` attaches them to the event's `attributes` map as `_file` and `_line` so operators jump to source from a log viewer.

### Call-site contract

Every `record!` call is a single line of code that says **what happened**, not **who did it or under what context**.

- The single positional argument after the level is an `Event` expression.
- The next argument is a string literal for the human-readable message.
- That is everything. Channel, agent_alias, provider, tool, session_key, cron_job_id, model: none of those are call-site arguments. They flow in from spans (see [Attribution: it all comes from spans](#attribution-it-all-comes-from-spans)).

The shape is enforced by the `Event` struct: unknown fields are a compile error.

### When attrs are warranted

`Event::with_attrs(serde_json::json!({...}))` is for per-event measurements and ad-hoc data that exist nowhere in the surrounding scope. Concretely:

- Per-event measurements: `bytes_received`, `tokens_used`, `retry_count`, `status_code`, `queue_depth`.
- Error payloads when the error is the event itself: anyhow chain text, HTTP error body, parse-error details.
- External-system identifiers: a remote API's `request_id`, an upstream trace header.
- Derived state captured at this instant: in-flight count, retry-after seconds.

**Attrs are NOT for** anything that comes from the surrounding scope: channel composite, agent_alias, model_provider, tool, session_key, cron_job_id, sender, message_id, etc. Those belong in a wrapping `attribution_span!` or `scope!`.

The serde rule: pass the **raw value**, never `format!("{}", v)` or `format!("{:?}", v)`. `serde_json::json!` serializes strings as strings, numbers as numbers, `Vec<T>` as arrays, `Option<T>` as null-or-value. Wrap with `.to_string()` only when the type doesn't `impl Serialize` (e.g. `anyhow::Error`, `reqwest::Error`, `std::io::Error`, `Path::Display`, `StatusCode`).

### Placeholder rule

Rust string-literal placeholders like `"raw error body: {body}"` are forbidden inside `record!` messages. Rust 2021's implicit format-string capture does not flow through `record!`: every `{var}` becomes a literal substring with no substitution. The conversion rule:

```rust
// BAD — {body} is a literal, never interpolated
record!(WARN, Event::new(module_path!(), Action::Fail), "raw error body: {body}");

// GOOD — body in attrs, message is plain prose
record!(WARN, Event::new(module_path!(), Action::Fail).with_attrs(serde_json::json!({"body": body})), "raw error body");
```

## `Event`, `Action`, `EventOutcome`, `EventCategory`

All four are closed enums defined in `crates/zeroclaw-log/src/event.rs`. Adding a value is the only point of change: call sites do not invent strings.

- `Action`: closed verb set, snake-cased on disk via `strum::IntoStaticStr`: `Start`, `Complete`, `Fail`, `Cancel`, `Skip`, `Timeout`, `Retry`, `Inbound`, `Outbound`, `Send`, `Receive`, `Connect`, `Disconnect`, `Reconnect`, `Spawn`, `Kill`, `Tick`, `Trigger`, `Schedule`, `Approve`, `Reject`, `Defer`, `Read`, `Write`, `Delete`, `List`, `Query`, `Invoke`, `Dispatch`, `Resolve`, `Register`, `Unregister`, `Load`, `Save`, `Migrate`, `Validate`, `Note`.
- `EventOutcome`: `Success`, `Failure`, `Unknown`. `Unknown` is the default and is skipped on serialization (omitted from the on-disk `event.outcome`), so a row with no `outcome` key is implicitly `Unknown`.
- `EventCategory`: `Agent`, `Channel`, `Cron`, `Memory`, `Tool`, `Provider`, `Session`, `System`, `Internal`. Derived from the innermost role span unless overridden via `Event::with_category(...)`.

## Tool input/output propagation

The central tool executor (`crates/zeroclaw-runtime/src/agent/tool_execution.rs::execute_one_tool`) wraps every `Tool::execute(args)` call with invoke/complete/fail events. Each event's name is `module_path!()` (the executor's own module), not a hardcoded string; the `Action` and severity distinguish them:

1. Before running: `record!(DEBUG, Event::new(module_path!(), Action::Invoke).with_category(EventCategory::Tool).with_attrs(...))` with `tool`, `tool_call_id`, and the full `input` in attrs.
2. Runs `execute(args).await`.
3. On success (`r.success`): `record!(DEBUG, ... Action::Complete)` with `Outcome::Success`, the duration, and `tool` / `tool_call_id` / `input` / `output` in attrs.
4. On tool-reported failure (`!r.success`): `record!(WARN, ... Action::Fail)` with `Outcome::Failure`, the duration, and `tool` / `tool_call_id` / `input` / `error` / `output` in attrs.
5. On `Err` from `execute`: `record!(ERROR, ... Action::Fail)` with `Outcome::Failure`, the duration, and the debug-formatted error in attrs.

These events are emitted inside a `scope!`-style span (`target = "zeroclaw_log_internal_scope"`, field `tool = <name>`) opened around the call, so the `tool` field rides on every descendant emission too. Per-tool `Tool::execute` impls add zero logging code.

## `LogCaptureLayer` and the on-disk schema

The layer in `crates/zeroclaw-log/src/layer.rs` is a `tracing-subscriber` Layer that:

1. On span creation/record with target `"zeroclaw_log_internal_attribution"` (the target the `attribution_span!` macro opens with): parses the role + alias fields into a `ZeroclawAttribution` snapshot stored on the span's extensions.
2. On span creation/record with target `"zeroclaw_log_internal_scope"` (`scope!`-opened): parses ad-hoc kvps and stashes them similarly.
3. On event emission with target `"zeroclaw_log_event"` (the target the `record!` macro fires through): builds a `LogEvent` from the `zc_*` field set, walks the span scope leaf→root merging every attribution snapshot it finds, parses the `zc_attrs` JSON blob into the event `attributes`, attaches `_file`/`_line` from auto-captured source location, and hands the final event to `writer::record_event`, which fans out in this order:
   - Observer bridge (`observer_bridge.rs`) for mapped Prometheus / OTel typed events when an Observer is bound.
   - Broadcast hook (`broadcast.rs`) for current SSE/dashboard subscribers when a sender is installed.
   - JSONL persistence (`writer.rs`), offered last to the asynchronous writer queue only when `log_persistence` is enabled.

The on-disk JSON shape (`LogEvent` in `event.rs`):

```json
{
  "id": "<uuid>",
  "@timestamp": "2026-05-16T10:08:59.002Z",
  "severity_number": 9,
  "severity_text": "INFO",
  "event": { "category": "channel", "action": "inbound", "outcome": "success" },
  "service": { "name": "zeroclaw", "version": "0.8.5" },
  "trace_id": "<turn id>",
  "span_id": "<sub-span id>",
  "zeroclaw": {
    "channel": "telegram.clamps",
    "channel_type": "telegram",
    "channel_alias": "clamps",
    "agent_alias": "clamps",
    "model_provider": "anthropic.clamps",
    "model_provider_type": "anthropic",
    "model_provider_alias": "clamps",
    "model": "claude-sonnet-4-6"
  },
  "message": "inbound message",
  "attributes": { "sender": "...", "_file": "...", "_line": 42 },
  "schema_version": 2
}
```

`@timestamp` is `chrono::DateTime<Utc>` serialized as RFC 3339 with `Z`. The schema version is `2`; older `version: 1` rows are migrated in place at daemon startup by `migrate::migrate_legacy_jsonl_in_place`.

## Delivery surfaces have different guarantees

`writer::record_event` builds the persisted value once, then derives the other deliveries from the same `LogEvent`. Each destination has a separate contract:

| Destination | Owner | Contract and loss boundary |
|---|---|---|
| Optional typed Observer bridge | `observer_bridge.rs` | `forward` is a no-op until an Observer is explicitly bound, and the current production bootstrap does not bind one. When bound, it forwards synchronously but projects only actions recognized by `project`; the current mapping may omit actions or default fields. Treat it as a selective metrics/tracing projection, not a complete event ledger, and inspect `project` for the current field mapping. |
| Live broadcast | `broadcast.rs` and its consumer | Sends the structured event to current in-process subscribers. A subscriber only sees events emitted after it subscribes, bounded receivers can lag, and the gateway SSE adapter skips lagged frames. Broadcast-only ephemeral attributes may appear in an authenticated live frame but are excluded from persisted JSONL. This is a live notification path, not replayable evidence. |
| Persisted JSONL | `writer.rs` | Enqueues the serialized event without blocking the runtime. The bounded queue can drop an event when full, worker write failures are warnings, and periodic `sync_all` covers only the current active file. Daily rotation before a new UTC day's first append and size rotation after a threshold-crossing append can rename the active file without first syncing it, so the cadence does not bound durability for a just-rotated archive. Persistence mode then decides whether the active file is trimmed, retained indefinitely, or rotated. This is best-effort operational history, not a transactional audit log. |

Do not use Observer output or SSE delivery to prove that every canonical event was retained. Conversely, do not assume a row absent from JSONL was never emitted: it may have reached live broadcast, and the Observer bridge when bound, before the persistence queue dropped or failed it.

## Reader cursors belong to one active file

`GET /api/logs` resolves the writer's current active path and calls `reader::load_page`. The reader scans that one JSONL file, keeps the newest matching window, and returns events newest first. It does not merge rotated archives.

The primary pagination cursor is `next_cursor_line_offset`, the byte offset immediately after the oldest matching event on the current page. A caller passes it back as `until_line_offset`; the next scan stops before that line and returns older matches. Pure appends preserve the prefix addressed by an existing cursor, so later events do not disturb an in-progress walk.

The offset is not a durable event identity or cross-file checkpoint. It becomes stale whenever the active file's bytes are replaced or its path changes:

- `rolling` trim streams the retained tail to a temporary file and renames it over the active path.
- `rotating` renames the active file to an archive; the next append creates a new active file.
- schema migration rewrites the active file through a temporary file and atomic rename.
- a daemon config reload can install a new persistence path.

After one of those boundaries, restart pagination from the newest page. Reusing the old number can duplicate, skip, or return unrelated rows because the API does not attach file identity or generation metadata to the cursor. The legacy timestamp/ID cursor remains for compatibility but is deprecated under [#8012](https://github.com/zeroclaw-labs/zeroclaw/issues/8012) because lexicographic ID ordering can skip tied events.

## Persistence policy owns rewrites and retention

`StoragePolicy` in `config.rs` controls only the JSONL destination. Observer and broadcast delivery remain independent of it.

| Policy | Active-file behavior | Retention owner |
|---|---|---|
| `none` | No new JSONL writes. | None. |
| `rolling` | After an append exceeds `max_entries`, stream only the newest non-empty lines to a temporary file and rename it over the active file. | The writer keeps the configured active-window size. It creates no archives and leaves archives from an earlier `rotating` configuration unmanaged. |
| `full` | Append without writer-managed trim or rotation. | The operator owns file growth and any external rotation. |
| `rotating` | Before a new UTC day's first append, or after an append reaches the byte threshold, rename the active file to a timestamped archive. | After each successful rotation, the writer prunes matching archives by age and then count. Removal is best-effort and never fails the enclosing append. |

Age and count retention run only after rotation. They do not sweep continuously, do not apply to `full` or `rolling`, and do not delete arbitrary neighboring files: archive discovery accepts only names generated from the active path's timestamped archive shape. The live `/api/logs` reader still sees only the active file; archives are offline diagnostic artifacts.

## Schema migration is an active-file rewrite

When persistence is enabled and the active path exists, `writer::init_from_config` runs `migrate::migrate_legacy_jsonl_in_place` before starting the disk worker. The migrator streams non-empty rows through a temporary file, converts legacy rows with `timestamp` but no `@timestamp`, preserves already-current rows, skips malformed JSON with a warning, syncs the temporary file, and atomically renames it over the active path.

Migration is best-effort. Its cheap schema check stops at the first non-empty row. Migration runs when that row is malformed or has `timestamp` without `@timestamp`; any other parseable JSON is treated as current, even when it is an unknown or invalid schema, so later legacy rows can remain unmigrated. If migration returns an error, initialization warns and continues, so later v2 appends can coexist with old rows that the v2 reader cannot deserialize. Rotated archives are not migrated.

`LogEvent` is the schema source of truth. For each schema change, assess migration compatibility, active-file deserialization, HTTP and RPC serialization/consumers, and the architecture and operator documentation; update only the boundaries whose behavior or compatibility changes. The RPC log surfaces live in `crates/zeroclaw-runtime/src/rpc/types.rs` and `dispatch.rs`. A migration that replaces the active file invalidates byte-offset cursors, while an additive compatible change that does not rewrite existing bytes does not.

## `LogConfig` vs `ObservabilityConfig`

`zeroclaw-log` defines its own minimal `LogConfig` (in `crates/zeroclaw-log/src/config.rs`): `log_persistence`, `log_persistence_path`, `log_persistence_max_entries`, `log_persistence_max_bytes`, `log_persistence_rotate_daily`, `log_persistence_retention_max_files`, `log_persistence_retention_max_age_days`, `log_tool_io`, `log_tool_io_truncate_bytes`, `log_tool_io_denylist`. This breaks what would otherwise be a dep cycle: `zeroclaw-config::ObservabilityConfig` carries the full schema (with TOML deserialization and validation), and the runtime converts to `LogConfig` at startup and after daemon config reload via `crates/zeroclaw-runtime/src/observability/runtime_trace.rs::to_log_config`. The result: `zeroclaw-config` can `record!` without inverting the dep tree, while log persistence and rotation policy changes still take effect on the next daemon reload.

## Subscriber installation

The daemon installs the global subscriber via:

```rust
zeroclaw_log::install_global_subscriber(
    recording_filter.as_deref(),   // Option<&str> — the --log-level flag, if set
    &default_filter,               // &str — fallback filter when no flag and no RUST_LOG
    cli.verbose,                   // bool — gates the stderr fmt (terminal) layer
);
```

Two independent axes: the **recording floor** (what reaches `LogCaptureLayer`, resolved as flag → `RUST_LOG` → default) and **terminal display** (the stderr fmt layer, muted entirely unless `verbose` is true). That single call sets up the agent-alias-prefixed terminal formatter + the `LogCaptureLayer` over a `tracing-subscriber::Registry`. `src/main.rs` is the only place that calls it. Tests use `zeroclaw_log::try_install_capture_subscriber()` + `zeroclaw_log::subscribe_or_install()` to drain emitted events through the broadcast hook without any tracing types named in the test crate.

## When to extend the closed enums

- **New channel impl**: add a variant to `ChannelKind`. The snake_case form is the on-disk `channel_type` string. Add `#[strum(serialize = "...")]` only when the variant name doesn't snake-case to the desired value (e.g. `OpenAi` → `"openai"`).
- **New tool impl** (workspace built-in): add to `ToolKind`.
- **New cron schedule shape**: add to `CronKind`.
- **New model / TTS / transcription / tunnel provider**: add to the relevant `*ProviderKind` sub-enum under `ProviderKind`.
- **New memory backend**: add to `MemoryKind`.
- **New `Role` family altogether** (PeerGroup / Skill / Mcp gain sub-types): nest with its own `Kind` on the fly: the pattern is uniform.

Then add `impl Attributable for X` next to the new struct (`fn role() -> Role::Family(Kind::Variant)`, `fn alias() -> &str { &self.alias }`) and wrap its entry point with `attribution_span!(self)`. The layer picks up everything else automatically.

## Operator concerns

For configuration knobs (`log_persistence`, `log_tool_io`, OTel export) and query syntax, see [Logs & observability](../ops/observability.md).
