# SOP Observability & Audit

This page covers where SOP execution evidence is stored and how to inspect it.

## 1. Audit Persistence

SOP audit entries are persisted via `SopAuditLogger` into the configured Memory backend, category `sop`.

Common key patterns:

- `sop_run_{run_id}`: run snapshot (start + completion updates)
- `sop_step_{run_id}_{step_number}`: per-step result
- `sop_approval_{run_id}_{step_number}`: operator approval record
- `sop_timeout_approve_{run_id}_{step_number}`: timeout auto-approval record
- `sop_gate_decision_{gate_id}_{timestamp_ms}`: gate evaluator decision record (when `ampersona-gates` is enabled)
- `sop_phase_state`: persisted trust-phase state snapshot (when `ampersona-gates` is enabled)

## 2. Inspection Paths

### 2.1 Definition-level CLI

```bash
zeroclaw sop list
zeroclaw sop validate [name]
zeroclaw sop show <name>
```

### 2.2 Runtime run-state tools

SOP run state is queried from in-agent tools:

- `sop_status` — active/finished runs and optional metrics
- `sop_status` with `include_gate_status: true` — trust phase and gate evaluator state (when available)
- `sop_approve` — approve waiting run step
- `sop_advance` — submit step result and move run forward

## 3. Metrics

- `/metrics` exposes observer metrics when `[observability] backend = "prometheus"`.
- Current exported names are `zeroclaw_*` families (general runtime metrics).
- SOP-specific aggregates are available through `sop_status` with `include_metrics: true`.
