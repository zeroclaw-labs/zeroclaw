# zeroclaw-log

Unified log emission surface for the ZeroClaw workspace.

Every crate that emits domain events (agent activity, channel I/O, cron runs,
tool calls, memory ops, session lifecycle, errors) goes through the [`record!`]
macro. That single emission point fans out to:

1. A `tracing::event!` at the matching severity so `RUST_LOG`-gated terminal
   output and any external `tracing-subscriber` consumer see the event with
   structured `key=value` fields.
2. The persisted JSONL log at `<workspace>/state/runtime-trace.jsonl` (when
   `[observability] log_persistence` is `"rolling"` or `"full"`).
3. The process-wide broadcast channel so the dashboard's SSE stream sees every
   event live.

## Usage

```rust
use zeroclaw_log::{record, Event, Action, EventOutcome};

// Simple informational message
record!(INFO, Event::new(module_path!(), Action::Note), "starting step");

// Warning with structured attributes
record!(
    WARN,
    Event::new(module_path!(), Action::Fail)
        .with_outcome(EventOutcome::Failure)
        .with_attrs(serde_json::json!({"err": "timeout"})),
    "tool failed"
);
```

## Architecture

- **`record!`** — the sole logging surface. Direct `tracing::info!` / `warn!` /
  etc. are banned workspace-wide via `clippy.toml` `disallowed-macros`.
- **`Event`** — structured event payload carrying name, action, outcome,
  category, duration, and arbitrary JSON attributes.
- **`LogEvent`** — the full wire schema (OTel/ECS hybrid with ZeroClaw-domain
  `zeroclaw.*` namespace for attribution fields).
- **Broadcast hook** — every `record!` emission reaches the dashboard's SSE
  stream via a process-wide `tokio::sync::broadcast` channel.
- **JSONL writer** — rolling/full persistence to `runtime-trace.jsonl`.

## Stability

Beta — breaking changes permitted in MINOR with changelog notes. Targeting
stable at v0.8.0.
