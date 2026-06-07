# Logging architecture

ZeroClaw has exactly one logging surface: the `zeroclaw_log::record!` macro. Every emission in the workspace — agent loop activity, channel I/O, cron runs, tool calls, memory ops, session lifecycle, errors — flows through it. The macro feeds a single `LogCaptureLayer` that materializes structured `LogEvent` records and routes them to three sinks at once:

1. The terminal (via the `tracing-subscriber` fmt layer that `zeroclaw-log` installs internally) so operators see colored, alias-prefixed lines on stderr.
2. The persisted JSONL log at `<workspace>/state/runtime-trace.jsonl` (when `[observability] log_persistence` is `"rolling"` or `"full"`).
3. The process-wide broadcast channel so the dashboard's SSE stream sees every event live.

The `tracing` crate is `zeroclaw-log`'s implementation detail. No other workspace crate references `tracing`, `tracing-subscriber`, or `tracing-attributes`. Their Cargo.toml files do not depend on those crates, and no `.rs` file outside `crates/zeroclaw-log/` names a tracing type.

## The `record!` macro

The macro is locked-shape: it takes a level, a single `Event` expression, and a message literal.

```rust
use zeroclaw_log::{record, Event, Action, EventCategory, EventOutcome};

record!(INFO, Event::new(module_path!(), Action::Start), "starting step");
record!(WARN, Event::new(module_path!(), Action::Fail).with_outcome(EventOutcome::Failure).with_attrs(serde_json::json!({"exit_code": 137})), "tool failed");
```

`module_path!()` is the canonical source of the event name — it's the Rust module path of the call site (e.g. `zeroclaw_channels::telegram`), so events are searchable, jump-to-source-able, and impossible to typo. The same convention is used at every `record!` site in the workspace.

The macro injects `file!()` and `line!()` automatically. The `LogCaptureLayer` attaches them to the event's `attributes` map as `_file` and `_line` so operators jump to source from a log viewer.

### Call-site contract

Every `record!` call is a single line of code that says **what happened**, not **who did it or under what context**.

- The single positional argument after the level is an `Event` expression.
- The next argument is a string literal for the human-readable message.
- That is everything. Channel, agent_alias, provider, tool, session_key, cron_job_id, model — none of those are call-site arguments. They flow in from spans (see [Attribution](#attribution)).

The shape is enforced by the `Event` struct: unknown fields are a compile error.

### When attrs are warranted

`Event::with_attrs(serde_json::json!({...}))` is for per-event measurements and ad-hoc data that exist nowhere in the surrounding scope. Concretely:

- Per-event measurements: `bytes_received`, `tokens_used`, `retry_count`, `status_code`, `queue_depth`.
- Error payloads when the error is the event itself: anyhow chain text, HTTP error body, parse-error details.
- External-system identifiers: a remote API's `request_id`, an upstream trace header.
- Derived state captured at this instant: in-flight count, retry-after seconds.

**Attrs are NOT for** anything that comes from the surrounding scope — channel composite, agent_alias, model_provider, tool, session_key, cron_job_id, sender, message_id, etc. Those belong in a wrapping `attribution_span!` or `scope!`.

The serde rule: pass the **raw value**, never `format!("{}", v)` or `format!("{:?}", v)`. `serde_json::json!` serializes strings as strings, numbers as numbers, `Vec<T>` as arrays, `Option<T>` as null-or-value. Wrap with `.to_string()` only when the type doesn't `impl Serialize` (e.g. `anyhow::Error`, `reqwest::Error`, `std::io::Error`, `Path::Display`, `StatusCode`).

### Placeholder rule

Rust string-literal placeholders like `"raw error body: {body}"` are forbidden inside `record!` messages. Rust 2021's implicit format-string capture does not flow through `record!` — every `{var}` becomes a literal substring with no substitution. The conversion rule:

```rust
// BAD — {body} is a literal, never interpolated
record!(WARN, Event::new(module_path!(), Action::Fail), "raw error body: {body}");

// GOOD — body in attrs, message is plain prose
record!(WARN, Event::new(module_path!(), Action::Fail).with_attrs(serde_json::json!({"body": body})), "raw error body");
```

## `Event`, `Action`, `EventOutcome`, `EventCategory`

All four are closed enums defined in `crates/zeroclaw-log/src/event.rs`. Adding a value is the only point of change — call sites do not invent strings.

- `Action` — closed verb set, snake-cased on disk via `strum::IntoStaticStr`: `Start`, `Complete`, `Fail`, `Cancel`, `Skip`, `Timeout`, `Retry`, `Inbound`, `Outbound`, `Send`, `Receive`, `Connect`, `Disconnect`, `Reconnect`, `Spawn`, `Kill`, `Tick`, `Trigger`, `Schedule`, `Approve`, `Reject`, `Defer`, `Read`, `Write`, `Delete`, `List`, `Query`, `Invoke`, `Dispatch`, `Resolve`, `Register`, `Unregister`, `Load`, `Save`, `Migrate`, `Validate`, `Note`.
- `EventOutcome` — `Success`, `Failure`, `Unknown` (the default — terminal outcome correlated to the matching Start via `trace_id`).
- `EventCategory` — `Agent`, `Channel`, `Cron`, `Memory`, `Tool`, `Provider`, `Session`, `System`, `Internal`. Derived from the innermost role span unless overridden via `Event::with_category(...)`.

## Attribution

Alias-bound attribution (channel composite, agent_alias, model_provider, tool, cron_job_id, …) is never a call-site argument. It flows through tracing spans opened at entry points and walked by the layer.

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
    Memory(MemoryKind),         // Sqlite, Json, InMemory
    PeerGroup,
    Skill,
    Mcp,
    Session,
    System,
}
```

`ChannelKind`, `ToolKind`, `CronKind`, `MemoryKind`, and the four `ProviderKind` sub-enums (`ModelProviderKind`, `TtsProviderKind`, `TranscriptionProviderKind`, `TunnelProviderKind`) are all closed. The variant's snake_case form via `strum::IntoStaticStr` is the canonical `<type>` portion of the `<type>.<alias>` composite. Adding a new implementation: extend the relevant `Kind` enum, that's it.

### Opening a span

Wrap an entry-point's work with `attribution_span!(thing)`. The macro returns a `Span` carrying the thing's role and alias as structured fields. `.instrument(span)` the future (or `let _g = span.entered()` in sync code).

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

### The `scope!` macro

For per-scope identifiers that aren't tied to a role-bearing `Attributable` thing — sender id, message id, turn id, request id — use `scope!`:

```rust
zeroclaw_log::scope!(
    sender: msg.sender.as_str(),
    message_id: msg.id.as_str(),
    => async move { process_message(msg).await }
).await
```

Field keys that match the alias-bound `ATTRIBUTION_FIELDS` / `COMPOSITE_PREFIXES` (in `crates/zeroclaw-log/src/event.rs`) land in the typed attribution slot; everything else lands in the event `attributes` map for every descendant emission.

## Tool input/output propagation

The central tool executor (`crates/zeroclaw-runtime/src/agent/tool_execution.rs::execute_one_tool`) wraps every `Tool::execute(args)` call with start/complete/fail events:

1. Emits `Event::new("tool.invoke.start", Action::Invoke)` with `args` in attrs.
2. Runs `execute(args).await`.
3. On success: `Event::new("tool.invoke.complete", Action::Complete)` with `Outcome::Success`, the duration, and the output in attrs.
4. On failure: `Event::new("tool.invoke.fail", Action::Fail)` with `Outcome::Failure`, the duration, and the error/output in attrs.
5. On panic / `Err`: same fail emission, error chain in attrs.

Per-tool `Tool::execute` impls add zero logging code. The matching pair (start ↔ complete/fail) shares a `trace_id` via the surrounding span scope, so a dashboard query can correlate them.

## `LogCaptureLayer` and the on-disk schema

The layer in `crates/zeroclaw-log/src/layer.rs` is a `tracing-subscriber` Layer that:

1. On span creation/record with target `"zeroclaw_log_internal_attribution"` (the target the `attribution_span!` macro opens with): parses the role + alias fields into a `ZeroclawAttribution` snapshot stored on the span's extensions.
2. On span creation/record with target `"zeroclaw_log_internal_scope"` (`scope!`-opened): parses ad-hoc kvps and stashes them similarly.
3. On event emission with target `"zeroclaw_log_event"` (the target the `record!` macro fires through): builds a `LogEvent` from the `zc_*` field set, walks the span scope leaf→root merging every attribution snapshot it finds, parses the `zc_attrs` JSON blob into the event `attributes`, attaches `_file`/`_line` from auto-captured source location, and writes the final event to:
   - JSONL persistence (`writer.rs`).
   - Broadcast hook (`broadcast.rs`) for SSE/dashboard subscribers.
   - Observer bridge (`observer_bridge.rs`) for Prometheus / OTel typed metrics.

The on-disk JSON shape (`LogEvent` in `event.rs`):

```json
{
  "id": "<uuid>",
  "@timestamp": "2026-05-16T10:08:59.002Z",
  "severity_number": 9,
  "severity_text": "INFO",
  "event": { "category": "channel", "action": "inbound", "outcome": "success" },
  "service": { "name": "zeroclaw", "version": "0.8.0-beta-2" },
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

## `LogConfig` vs `ObservabilityConfig`

`zeroclaw-log` defines its own minimal `LogConfig` (in `crates/zeroclaw-log/src/config.rs`) — `log_persistence`, `log_persistence_path`, `log_persistence_max_entries`, `log_tool_io`, `log_tool_io_truncate_bytes`, `log_tool_io_denylist`. This breaks what would otherwise be a dep cycle: `zeroclaw-config::ObservabilityConfig` carries the full schema (with TOML deserialization and validation), and the runtime converts to `LogConfig` at startup via `crates/zeroclaw-runtime/src/observability/runtime_trace.rs::to_log_config`. The result: `zeroclaw-config` can `record!` without inverting the dep tree.

## Subscriber installation

The daemon installs the global subscriber via:

```rust
zeroclaw_log::install_global_subscriber("info,matrix_sdk=warn,matrix_sdk_base=warn");
```

That single call sets up the agent-alias-prefixed terminal formatter + the `LogCaptureLayer` over a `tracing-subscriber::Registry`. `src/main.rs` is the only place that calls it. Tests use `zeroclaw_log::try_install_capture_subscriber()` + `zeroclaw_log::subscribe_or_install()` to drain emitted events through the broadcast hook without any tracing types named in the test crate.

## When to extend the closed enums

- **New channel impl**: add a variant to `ChannelKind`. The snake_case form is the on-disk `channel_type` string. Add `#[strum(serialize = "...")]` only when the variant name doesn't snake-case to the desired value (e.g. `OpenAi` → `"openai"`).
- **New tool impl** (workspace built-in): add to `ToolKind`.
- **New cron schedule shape**: add to `CronKind`.
- **New model / TTS / transcription / tunnel provider**: add to the relevant `*ProviderKind` sub-enum under `ProviderKind`.
- **New memory backend**: add to `MemoryKind`.
- **New `Role` family altogether** (PeerGroup / Skill / Mcp gain sub-types): nest with its own `Kind` on the fly — the pattern is uniform.

Then add `impl Attributable for X` next to the new struct (`fn role() -> Role::Family(Kind::Variant)`, `fn alias() -> &str { &self.alias }`) and wrap its entry point with `attribution_span!(self)`. The layer picks up everything else automatically.

## Operator concerns

For configuration knobs (`log_persistence`, `log_tool_io`, OTel export) and query syntax, see [Logs & observability](../ops/observability.md).
