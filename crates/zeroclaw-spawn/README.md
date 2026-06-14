# zeroclaw-spawn

Attribution-propagating, lifecycle-logged wrapper around `tokio::spawn` for
the ZeroClaw workspace.

Every call site that needs to fan out a background task must go through
[`spawn!`] instead of reaching for `tokio::spawn` directly. Direct
`tokio::spawn` is banned workspace-wide via `clippy.toml` `disallowed-methods`.

## What `spawn!` adds

1. **Attribution propagation.** The spawned future is wrapped with
   `.in_current_span()` so any `record!` it emits inherits the caller's
   `attribution_span` (channel, agent, session, cron job, ...). Without this,
   tokio detaches the task from the caller's span stack and records appear
   orphaned.

2. **Lifecycle telemetry.** Two structured log events are emitted through the
   standard `zeroclaw_log::record!` pipeline:
   - `runtime.task.spawn` (`Action::Spawn`) at spawn time.
   - `runtime.task.spawn` (`Action::Complete`) when the future resolves, with
     elapsed `duration_ms`.

## Usage

```rust
use zeroclaw_spawn::spawn;

let handle = spawn!(async move {
    do_work().await
});
let output = handle.await?;
```

The returned `JoinHandle<T>` is the unmodified handle from `tokio::spawn` —
panics propagate as `JoinError::Panic`, cancel semantics are unchanged, and
the future's output type is preserved. `zeroclaw-spawn` adds telemetry, not
control flow.

## Layering

`zeroclaw-spawn` depends on `zeroclaw-log`. Crates that already depend on
`zeroclaw-log` (i.e. almost everything in the workspace) can add
`zeroclaw-spawn` as a peer dependency without inverting the graph. The
lowest-level crate (`zeroclaw-api`) intentionally does NOT depend on
`zeroclaw-spawn`.

## Stability

Beta — breaking changes permitted in MINOR with changelog notes.
