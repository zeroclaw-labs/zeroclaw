# Agent Teams Orchestration Evaluation Pack (2026-03-01)

Status: Deep optimization complete, validation evidence captured.
Linear parent: [RMN-284](https://linear.app/zeroclawlabs/issue/RMN-284/improvement-agent-teams-orchestration-research)
Execution slices: RMN-285, RMN-286, RMN-287, RMN-288, RMN-289

## 1) Objective

Define a practical and testable multi-agent orchestration contract that:

- decomposes complex work into parallelizable units,
- constrains communication overhead,
- preserves quality through explicit verification,
- and enforces token-aware execution policies.

## 2) A2A-Lite Protocol Contract

All inter-agent messages MUST follow a small fixed payload shape.

### Required fields

- `run_id`: stable run identifier
- `task_id`: task node identifier in DAG
- `sender`: agent id
- `recipient`: agent id or coordinator
- `status`: `queued|running|blocked|done|failed`
- `confidence`: `0-100`
- `risk_level`: `low|medium|high|critical`
- `summary`: short natural-language summary (token-capped)
- `artifacts`: list of evidence pointers (paths/URIs)
- `needs`: dependency requests or unblocks
- `next_action`: next deterministic action

### Message discipline

- Never forward raw transcripts by default.
- Always send evidence pointers, not full payload dumps.
- Keep summaries bounded by budget profile.
- Escalate to coordinator when risk is `high|critical`.

### Example message

```json
{
  "run_id": "run-2026-03-01-001",
  "task_id": "task-17",
  "sender": "worker-protocol",
  "recipient": "lead",
  "status": "done",
  "confidence": 0.91,
  "risk_level": "medium",
  "summary": "Protocol schema validated against three handoff paths; escalation path requires owner signoff.",
  "artifacts": [
    "docs/project/agent-teams-orchestration-eval-2026-03-01.md#2-a2a-lite-protocol-contract",
    "scripts/ci/agent_team_orchestration_eval.py"
  ],
  "needs": [
    "scheduler-policy-review"
  ],
  "next_action": "handoff-to-scheduler-owner"
}
```

## 3) DAG Scheduling + Budget Policy

### Decomposition rules

- Build a DAG first; avoid flat task lists.
- Parallelize only nodes without write-conflict overlap.
- Each node has one owner and explicit acceptance checks.

### Topology policy

- Default: `star` (lead + bounded workers).
- Escalation: temporary peer channels for conflict resolution only.
- Avoid sustained mesh communication unless explicitly justified.

### Budget hierarchy

- Run budget
- Team budget
- Task budget
- Message budget

### Auto-degradation policy (in order)

1. Reduce peer-to-peer communication.
2. Tighten summary caps.
3. Reduce active workers.
4. Switch lower-priority workers to lower-cost model tier.
5. Increase compaction cadence.

## 4) KPI Schema

Required metrics per run:

- `throughput` (tasks/day equivalent)
- `pass_rate`
- `defect_escape`
- `total_tokens`
- `coordination_tokens`
- `coordination_ratio`
- `p95_latency_s`

Derived governance checks:

- Coordination overhead target: `coordination_ratio <= 0.20`
- Quality floor: `pass_rate >= 0.80`

## 5) Experiment Matrix

Run all topology modes under `low|medium|high` budget buckets:

- `single`
- `lead_subagent`
- `star_team`
- `mesh_team`

Control variables:

- same workload set
- same task count
- same average task token baseline

Decision output:

- cost-optimal topology
- quality-optimal topology
- production default recommendation

## 5.1) Deep Optimization Dimensions

The evaluation engine now supports deeper policy dimensions:

- Workload profiles: `implementation`, `debugging`, `research`, `mixed`
- Protocol modes: `a2a_lite`, `transcript`
- Degradation policies: `none`, `auto`, `aggressive`
- Recommendation modes: `balanced`, `cost`, `quality`
- Gate checks: coordination ratio, pass rate, latency, budget compliance

Observed implications:

- `a2a_lite` keeps summary payload and coordination tokens bounded.
- `transcript` mode can substantially increase coordination overhead and budget risk.
- `auto` degradation can reduce participants and summary size when budget pressure is detected.

## 6) Validation Flow

1. Run simulation script and export JSON report.
2. Run protocol comparison (`a2a_lite` vs `transcript`).
3. Run budget sweep with degradation policy enabled.
4. Validate gating thresholds.
5. Attach output artifacts to the corresponding Linear issue.
6. Promote to rollout only when all acceptance checks pass.

## 7) Local Commands

```bash
python3 scripts/ci/agent_team_orchestration_eval.py --budget medium --json-output -
python3 scripts/ci/agent_team_orchestration_eval.py --budget medium --topologies star_team --enforce-gates
python3 scripts/ci/agent_team_orchestration_eval.py --budget medium --protocol-mode transcript --json-output -
python3 scripts/ci/agent_team_orchestration_eval.py --all-budgets --degradation-policy auto --json-output docs/project/agent-teams-orchestration-eval-sample-2026-03-01.json
python3 -m unittest scripts.ci.tests.test_agent_team_orchestration_eval -v
cargo test team_orchestration --lib
```

## 7.1) Key Validation Findings (2026-03-01)

- Medium budget + `a2a_lite`: recommendation = `star_team`
- Medium budget + `transcript`: recommendation = `lead_subagent` (coordination overhead spikes in larger teams)
- Budget sweep + `auto` degradation: mesh topology can be de-risked via participant reduction + tighter summaries, while `star_team` remains the balanced default

Sample evidence artifact:

- `docs/project/agent-teams-orchestration-eval-sample-2026-03-01.json`

## 7.2) Repository Core Implementation (Rust)

In addition to script-level simulation, the orchestration engine is implemented
as a reusable Rust module:

- `src/agent/team_orchestration.rs`
- `src/agent/mod.rs` (`pub mod team_orchestration;`)

Core capabilities implemented in Rust:

- `A2ALiteMessage` + `HandoffPolicy` validation and compaction
- `TeamTopology` evaluation under budget/workload/protocol dimensions
- `DegradationPolicy` (`none|auto|aggressive`) for pressure handling
- Multi-gate evaluation (`coordination_ratio`, `pass_rate`, `latency`, `budget`)
- Recommendation scoring (`balanced|cost|quality`)
- Budget sweep helper across `low|medium|high`
- DAG planner with conflict-aware batching (`build_conflict_aware_execution_plan`)
- Task budget allocator (`allocate_task_budgets`) for run-budget pressure
- Plan validator (`validate_execution_plan`) with topology/order/budget/lock checks
- Plan diagnostics (`analyze_execution_plan`) for critical path and parallel efficiency
- Batch handoff synthesis (`build_batch_handoff_messages`) for planner->worker A2A-Lite
- End-to-end orchestration API (`orchestrate_task_graph`) linking eval + plan + validation + diagnostics + handoff generation
- Handoff token estimators (`estimate_handoff_tokens`, `estimate_batch_handoff_tokens`) for communication-budget governance

Rust unit-test status:

- `cargo test team_orchestration --lib`
- result: `17 passed; 0 failed`

## 7.3) Concurrency Decomposition Contract (Rust planner)

The Rust planner now provides a deterministic decomposition pipeline:

1. validate task graph (`TaskNodeSpec`, dependency integrity)
2. topological sort with cycle detection
3. budget allocation per task under run budget pressure
4. ownership-lock-aware batch construction for bounded parallelism

Planner outputs:

- `ExecutionPlan.topological_order`
- `ExecutionPlan.budgets`
- `ExecutionPlan.batches`
- `ExecutionPlan.total_estimated_tokens`

This is the repository-native basis for converting complex work into safe
parallel slices while reducing merge/file ownership conflicts and token waste.

Additional hardening added:

- `validate_execution_plan(plan, tasks)` for dependency/topological-order/conflict/budget integrity checks
- `analyze_execution_plan(plan, tasks)` for critical-path and parallel-efficiency diagnostics
- `build_batch_handoff_messages(run_id, plan, tasks, policy)` for planner-to-worker A2A-Lite handoffs

## 7.4) End-to-End Orchestration Bundle

`orchestrate_task_graph(...)` now exposes one deterministic orchestration entrypoint:

1. evaluate topology candidates under budget/workload/protocol/degradation gates
2. choose recommended topology
3. derive planner config from selected topology and budget envelope
4. build conflict-aware execution plan
5. validate the plan
6. compute plan diagnostics
7. generate compact A2A-Lite batch handoff messages
8. estimate communication token cost for handoffs

Output contract (`OrchestrationBundle`) includes:

- recommendation report and selected topology evidence
- planner config used for execution
- validated execution plan
- diagnostics (`critical_path_len`, parallelism metrics, lock counts)
- batch handoff messages
- estimated handoff token footprint

## 8) Definition of Done

- Protocol contract documented and example messages included.
- Scheduling and budget degradation policy documented.
- KPI schema and experiment matrix documented.
- Evaluation script and tests passing in local validation.
- Protocol comparison and budget sweep evidence generated.
- Linear evidence links updated for execution traceability.
