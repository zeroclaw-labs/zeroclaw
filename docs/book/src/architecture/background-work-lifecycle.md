# Background work lifecycle

ZeroClaw has several ways to continue work after the inbound request that started it. Cron jobs, SOP runs, delegated tasks, and runtime-spawned subagents share some execution machinery, but they do not share one lifecycle or one durable store. Goal mode defines a related target contract that is not yet wired end to end.

Use this page when a change adds scheduled or autonomous work, introduces a wait or approval state, changes cancellation or restart behavior, or connects child work to an owning task. The first design question is not "how does it run in the background?" but "which subsystem owns its lifecycle?"

## Ownership map

| Work type | Current owner or status surface | Durable records |
| --- | --- | --- |
| Cron job | Cron scheduler and store | `data/cron/jobs.db` |
| SOP run | `SopEngine` and `SopRunStore` | Process memory by default; `data/sop/runs.db` when durable SQLite initialization succeeds |
| Background delegation | Durable task control plane | A required task row in `data/control_plane.db`; output content in `<workspace>/delegate_results/<task-id>.json` |
| Runtime-spawned subagent | Spawn site, with control-plane supervision when available | A best-effort task row in `data/control_plane.db` under a booted daemon |

Durable metadata is not the same as durable execution. A result file or task row can preserve what was known and let recovery mark work lost, timed out, or terminal without preserving the process-local future that was doing the work.

## Cron jobs

Cron combines declarative membership with a SQLite execution store. Runtime-created jobs and reconciled config jobs both carry an owning `agent_alias`; execution resolves that agent's security policy instead of running under an ambient daemon identity.

The scheduler polls for due, enabled, unclaimed rows. Claiming a row prevents duplicate selection while it is in flight. Completion records bounded output, then reschedules a recurring job, deletes a successful auto-delete one-shot, or disables another one-shot. If the process exits before releasing a claim, the next scheduler startup clears the stale lock.

Startup behavior is explicit. With catch-up enabled, overdue jobs are considered for execution. Otherwise an overdue one-shot is disabled with a skipped result, while a recurring job advances to its next future occurrence without recording a run result. The scheduler checks its cancellation token between polling iterations, so shutdown waits for the current due-job batch to finish before the loop exits. Cancelling the scheduler is not a promise that an already-dispatched external side effect can be rolled back.

## SOP runs

SOP definitions live under the configured `sops` directory. `SopEngine` owns run progression, approval waits, checkpoints, terminal transitions, and the in-process status surface. `SopRunStore` is the concurrency source of truth when it admits and claims a run.

Run persistence is opt-in. With the default `sop.persist_runs = false`, the engine uses an in-memory store. When persistence is enabled, the default SQLite backend writes `runs.db` under `<data_dir>/sop` unless `run_state_dir` overrides it. Successful store initialization lets active snapshots, terminal records, events, revisions, and concurrency claims support restart restoration. If store initialization fails, the daemon logs a warning and falls back to the in-memory store.

SOP audit records in the Memory backend are a separate observability surface. They do not replace the run store and must not be used as the authority for whether a run is active, paused, approved, or terminal.

Approval and checkpoint states are durable control states only when the run store is durable. Timeout policy remains fail-closed by default: a timed-out approval escalates and keeps waiting unless config explicitly selects cancellation or the legacy auto-approve behavior.

## Delegation and subagents

Subagents inherit their parent's effective security boundary. Policy and memory overrides may narrow the parent envelope but cannot widen it, and child action accounting uses the parent's tracker so spawning children cannot bypass the parent's action budget.

The `spawn_subagent` path is synchronous: the parent waits for the child run to finish, and this path has no local timeout or background cancellation handle.

The delegate tool can run synchronously or start a background task and return a UUID. A background task starts only after it can open the durable control plane, resolve its owning agent alias, and register its task row. The row is the single lifecycle authority for running, terminal, error, and recovery state. Completed output content is written atomically under the workspace passed to the tool; the task row stores its artifact reference instead of duplicating the content.

Current readers remain compatible with legacy status-bearing artifacts and legacy inline task output. The reverse is not guaranteed: artifacts written in the current output-only format are not readable by older binaries or scripts that require embedded lifecycle fields. Roll back the data directory and workspace artifacts together, or finish outstanding background work before downgrading.

A live cancellation registry maps task IDs to process-local tokens. Cancellation signals the running task when that token is still available and uses a compare-and-set terminal transition in the task store. Completion, failure, cancellation, and recovery therefore cannot overwrite one another after one terminal outcome wins.

Current delegate and subagent rows populate agent, status, owner PID and process identity, depth, and timestamps. Delegate rows also record the originating agent alias so result, wait, list, and cancellation actions cannot consume another agent's task; subagent rows still leave route absent. Both leave heartbeat, parent task, and principal absent. Startup recovery marks a running row `lost` only when the recorded owner process is provably gone or its PID has been reused; uncertain ownership fails closed. `timed_out` applies only to producers that emit stale heartbeats, which these producers do not currently do. The task row makes an interrupted child visible but does not recreate its execution. Legacy delegate result files that contain status remain readable only when no corresponding task row exists.

## Goal-mode target contract

[ADR-008](./decisions/ADR-008-goal-mode-control-plane-and-usage-accounting.md) accepts the task control plane as the future authority for goal lifecycle, ownership, route, principal, parent relation, and recovery eligibility. The repository contains goal storage and control-plane APIs, but production goal admission and execution are not yet wired end to end.

A background path may participate in goal mode only after it preserves the owning goal relationship and reports terminal state and model usage back to it. Until then, that path is ordinary background work rather than goal-mode execution.

## Change checklist

For background-work changes, answer these before reviewer sign-off:

- Which subsystem owns the lifecycle and which store is authoritative?
- Is the work process-local, durably supervised, or actually restart-resumable?
- Which token or control-plane action cancels it, and what can remain in flight?
- Which parent task, agent, route, principal, recursion depth, and usage fields does this path actually populate?
- Are waiting, approval, checkpoint, lost, timed-out, and terminal states distinguishable?
- Can startup recovery duplicate a side effect or silently strand a claim?
- Does result delivery remain idempotent if completion is observed after restart?

## Source pointers

- Cron scheduler and persistence: `crates/zeroclaw-runtime/src/cron/scheduler.rs`, `crates/zeroclaw-runtime/src/cron/store.rs`
- SOP engine and run stores: `crates/zeroclaw-runtime/src/sop/engine.rs`, `crates/zeroclaw-runtime/src/sop/store/`
- Delegation and subagent behavior: [Delegation & SubAgents](../agents/delegation.md), `crates/zeroclaw-runtime/src/tools/delegate.rs`, `crates/zeroclaw-runtime/src/tools/spawn_subagent.rs`, `crates/zeroclaw-runtime/src/subagent/mod.rs`
- Durable task control plane and recovery: `crates/zeroclaw-runtime/src/control_plane/`
- Goal-mode decision: [ADR-008](./decisions/ADR-008-goal-mode-control-plane-and-usage-accounting.md)
- SOP operator guide: [How SOPs run](../sop/how-it-works.md)
