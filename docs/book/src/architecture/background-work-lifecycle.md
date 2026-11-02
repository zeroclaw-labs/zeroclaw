# Background work lifecycle

ZeroClaw has several ways to continue work after the inbound request that started it. Cron jobs, SOP runs, delegated tasks, and runtime-spawned subagents share some execution machinery, but they do not share one lifecycle or one durable store. Goal foreground child work is a separate, synchronously owned path: it preserves the current Goal operation rather than creating detached background work.

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

Before a background delegate publishes a terminal output artifact, it serializes the exact bytes, computes their SHA-256 digest, and persists a terminal-settlement intent containing the task id, current owner PID and boot identity, desired terminal status, artifact path/reference, digest, and terminal error. The intent is an unapplied transition; `TaskRecord.status` remains the sole persisted lifecycle authority.

The delegate then atomically writes and syncs those exact bytes. A single SQLite transaction compares the still-nonterminal, owner-matched task row to the intent, records the desired terminal status and payload, and deletes the unchanged intent. Store failures before intent persistence retry without publishing the artifact; failures after publication retry promotion in-process. A competing cancellation or other terminal winner is preserved and removes the stale intent.

A separate supervisor awaits the exact background worker's Tokio task handle. If that worker panics or is aborted while the parent process survives, the supervisor recovers only its task and recorded owner. It validates any persisted settlement intent with the same artifact checks used by process recovery; missing or corrupt output resolves Failed, and an exit before intent persistence resolves an owner-bound Failed result. Store errors retry with capped backoff. A live worker is never reclaimed, and a competing terminal winner or transferred owner cannot be overwritten. This uses direct worker-exit evidence rather than a heartbeat timeout or new lease.

Startup and periodic recovery process settlement intents before ordinary Lost reconciliation. Recovery acts only after proving the recorded owner is absent or its PID has been reused, validates the artifact bytes against the recorded digest, and resolves a missing or corrupt artifact conservatively as Failed with a persistence/recovery error rather than producing Completed. The random process nonce is not observable through OS process APIs, so PID plus OS start time remains conservative for same-second PID reuse: recovery treats that case as the same owner.

Current delegate and subagent rows populate agent, status, owner PID and process identity, depth, and timestamps. Delegate rows also record the creating agent alias, the ordered chain of caller aliases above it, and the tool-loop session key as principal; result, wait, list, and cancellation actions admit an alias that created the row or appears anywhere in that chain, while the session key is forensic only and is never consulted by the owner check. Subagent rows still leave route, chain, and principal absent. Both leave the parent task absent. Background delegate rows carry a heartbeat while they run: each progress write refreshes it, at least once a minute, so the reaper's heartbeat-age timeout applies to them; subagent rows still leave heartbeat absent. Startup recovery marks a running row `lost` only when the recorded owner process is provably gone or its PID has been reused; uncertain ownership fails closed. Windows recovery distinguishes a missing PID from access-denied or otherwise ambiguous probes and retains the row when absence cannot be proved. `timed_out` applies only to producers that emit heartbeats: background delegates do, subagents do not. A background delegate's heartbeat comes from its progress flusher, which rewrites the row at least once a minute on its own task, so it shows that the owning process is alive and flushing, not that the delegate is advancing: a delegate stuck inside one tool call keeps a fresh heartbeat and is bounded by its agentic timeout instead. The row ages past the reaper's limit only if the flusher stops or every progress write fails for the whole window; a row whose owner process died is reclaimed by startup recovery, not by heartbeat age. The task row makes an interrupted child visible but does not recreate its execution.

Legacy delegate result files that contain status remain readable when no task row exists. Terminal delegate rows created before `originator_route` was recorded also remain readable by exact task ID, including inline output. They cannot be cancelled or otherwise mutated through the legacy exception, and a nonterminal NULL-origin row remains hidden. Rows written before the originator chain existed keep an empty chain and stay creator-only through `originator_route`. New rows admit the creating alias and every ancestor alias recorded in their chain, for reads and mutations alike.

## Goal-mode target contract

[ADR-008](./decisions/ADR-008-goal-mode-control-plane-and-usage-accounting.md) accepts the task control plane as the authority for goal lifecycle, ownership, route, principal, parent relation, and recovery eligibility. In the current Goal implementation candidate, Matrix and ZeroCode native IPC/WSS parent and verifier work now install the same operation-owned foreground root. The native overall timeout remains optional where no caller deadline exists, while cancellation and drain obligations remain; this wiring does not establish validation or deployment readiness.

Supervised Start/Resume retains exact process-local launch custody from the committed admission through projection, acknowledgement, and worker registration. Cancellation transfers an unlaunched epoch to the same supervisor before releasing admission guards. One tracked initial cleanup attempt, followed only by bounded explicit control or lifecycle retries, reconciles the canonical winner without executing or replaying the request. Storage failure retains the actual cause and cleanup duty, blocks retirement and restart reopening, and returns through the existing error boundary; it does not imply a successful pause. The restart coordinator retains these supervisors until retirement is proved. This custody is not durable recovery intent: process death still uses the existing conservative previous-boot recovery, with no automatic resume.

Goal children invoked by `delegate` or `spawn_subagent` remain in the parent's foreground operation. They share its accountant and ledger and obey the cross-tool depth-one child limit. A Goal-spawned subagent requires an owner-bound task row linked to the Goal and settles that row before returning; synchronous delegation does not create an additional task row. Cancellation or deadline signals the child and awaits settlement and required row cleanup before the parent publishes its terminal outcome; dropping the caller's future is not a substitute for that cleanup. Existing authorization, approval, target, and ordinary spend policies still apply.

The candidate's parent and verifier calls use the ordinary daily/monthly cost tracker as well as the Goal budget. The Goal accountant remains the only writer for their usage. An ordinary pre-call spend refusal can pause the Goal only after accounting and the actual tool phase are clean. Wait for the ordinary limit to reset or raise it, then explicitly resume; resumption checks the same ordinary gate again. Missing or invalid accounting, unresolved tool work, and genuine operation failures do not become resumable merely because a budget error is also present.

A failed Goal subagent terminal-row write also prevents a new resumable budget pause, even when its model call was refused before admission and the parent tool result is paired. The original refusal and cleanup failure remain visible. A rolled-back write may leave the child row marked Running after its body has stopped; this path has no same-process cleanup retry queue. The existing generic reaper can mark a prior-owner row Lost only after its normal process-absence or process-reuse check, without replaying the child or claiming successful cleanup.

When a Goal-owned live SOP driver stops on an owned refusal, accounting, child, or tool-phase error, it stops advancing the current and queued actions. The existing SOP settlement owner terminates the begun run and cancels unstarted queued work without executing retry, Goto, or compensation steps. Failed terminal writes remain owned by the existing maintenance queue and prevent a clean budget pause; maintenance settlement does not replay the SOP steps. Ordinary SOP retry and failure policy is unchanged.

Detached/background delegate tasks do not become Goal children merely because a Goal initiated them. A background path may participate in Goal mode only after it preserves the owning Goal relationship and reports terminal state and model usage back to it; otherwise it remains ordinary background work.

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
